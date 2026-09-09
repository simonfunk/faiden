//! The ACP wire, and nothing else.
//!
//! This module is pure: it builds JSON-RPC frames and reads them back. It never
//! spawns a process, holds a lock or blocks. Everything it encodes was checked
//! against the adapter Faiden actually talks to — `~/.hermes/hermes-agent/
//! acp_adapter` and the `acp` Python SDK it imports (schema tag v0.11.2,
//! `PROTOCOL_VERSION = 1`) — rather than against prose documentation.
//!
//! Two spellings are easy to get wrong and are pinned by tests:
//!
//! * `session/cancel` is a *notification*. It has no id and no reply; the
//!   in-flight `session/prompt` is what eventually returns.
//! * ACP spells "the user did not allow this" as `outcome: "cancelled"`
//!   (`schema.py::DeniedOutcome`), not `"denied"`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// The protocol version this client speaks. Matches `acp.PROTOCOL_VERSION`.
pub const PROTOCOL_VERSION: u32 = 1;
/// Longest single line accepted from the agent's stdout. A frame beyond this is
/// a protocol fault, not something to buffer without limit.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// A message read from the agent's stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A reply to a request we sent.
    Response {
        id: Value,
        result: Value,
    },
    /// A failed reply to a request we sent.
    ErrorResponse {
        id: Value,
        code: i64,
        message: String,
    },
    /// The agent asking *us* something. It expects a reply with the same id.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Not JSON at all — most often a log line that escaped onto stdout.
    Malformed(String),
    /// Valid JSON that is not a JSON-RPC message we can act on.
    Unrecognised(String),
    /// The line exceeded [`MAX_LINE_BYTES`].
    Oversized { bytes: usize },
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Malformed(detail) => write!(f, "malformed protocol line: {detail}"),
            ProtocolError::Unrecognised(detail) => {
                write!(f, "line is JSON but not a JSON-RPC message: {detail}")
            }
            ProtocolError::Oversized { bytes } => write!(
                f,
                "protocol line of {bytes} bytes exceeds the {MAX_LINE_BYTES} byte limit"
            ),
        }
    }
}

/// What the user decided about a permission request, in ACP's own vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Selected {
        option_id: String,
    },
    /// Everything that is not an explicit allow: an explicit deny, a timeout,
    /// a cancel, a disconnect. All of them fail closed to the same answer.
    Cancelled,
}

/// One option the agent offered. Rendered as text; never executed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    /// `allow_once` | `allow_always` | `reject_once` | `reject_always`.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub title: String,
    pub detail: String,
    pub options: Vec<PermissionOption>,
}

/// One increment of transcript, as the agent described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptDelta {
    AgentMessage {
        text: String,
    },
    Thought {
        text: String,
    },
    /// Covers both `tool_call` and `tool_call_update`: the same tool call id
    /// identifies one entry, and later updates fill in what they know.
    ToolCall {
        tool_call_id: String,
        title: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        content: String,
    },
    /// A variant we do not model. Named rather than silently discarded.
    Other {
        kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUpdate {
    pub session_id: String,
    pub delta: TranscriptDelta,
}

/// Why a turn stopped. Reported verbatim; none of these mean "it worked".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    Other(String),
}

impl StopReason {
    pub fn parse(raw: &str) -> StopReason {
        match raw {
            "end_turn" => StopReason::EndTurn,
            "max_tokens" => StopReason::MaxTokens,
            "max_turn_requests" => StopReason::MaxTurnRequests,
            "refusal" => StopReason::Refusal,
            "cancelled" => StopReason::Cancelled,
            other => StopReason::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::MaxTurnRequests => "max_turn_requests",
            StopReason::Refusal => "refusal",
            StopReason::Cancelled => "cancelled",
            StopReason::Other(raw) => raw,
        }
    }

    /// Human-readable, and deliberately free of any success claim. ACP has no
    /// notion of "the task is done": `end_turn` only means the agent stopped
    /// producing output for this turn.
    pub fn describe(&self) -> String {
        let detail = match self {
            StopReason::EndTurn => "the agent stopped producing output for this turn",
            StopReason::MaxTokens => "the turn hit the model's token limit",
            StopReason::MaxTurnRequests => "the turn hit the agent's request limit",
            StopReason::Refusal => "the agent refused to continue",
            StopReason::Cancelled => "the turn was cancelled",
            StopReason::Other(_) => "the agent reported a stop reason this build does not model",
        };
        format!(
            "Turn ended ({}) — {}. This is not a claim that the task succeeded.",
            self.as_str(),
            detail
        )
    }
}

// --- outgoing frames --------------------------------------------------------

fn frame(value: Value) -> String {
    // `to_string` never emits a raw newline: control characters inside strings
    // are escaped, so one frame is always exactly one line.
    value.to_string()
}

pub fn initialize_request(id: u64) -> String {
    frame(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "clientInfo": {"name": "faiden", "version": env!("CARGO_PKG_VERSION")},
            // Faiden implements `session/update` and
            // `session/request_permission` and nothing else. Advertising a
            // filesystem or terminal host we do not have would invite requests
            // we could only fail.
            "clientCapabilities": {
                "fs": {"readTextFile": false, "writeTextFile": false},
                "terminal": false
            }
        }
    }))
}

/// `cwd` must be absolute — `NewSessionRequest` requires it. `mcpServers` is
/// empty because Faiden contributes no servers of its own; the agent keeps
/// whatever the user configured in Hermes.
pub fn session_new_request(id: u64, cwd: &str) -> String {
    frame(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/new",
        "params": {"cwd": cwd, "mcpServers": []}
    }))
}

pub fn session_prompt_request(id: u64, session_id: &str, text: &str) -> String {
    frame(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": text}]
        }
    }))
}

/// A notification: no id, and no reply will ever arrive. Sending it does not
/// mean the turn is over — the in-flight `session/prompt` still has to return.
pub fn session_cancel_notification(session_id: &str) -> String {
    frame(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session_id}
    }))
}

pub fn permission_response(id: &Value, decision: PermissionDecision) -> String {
    let outcome = match decision {
        PermissionDecision::Selected { option_id } => {
            json!({"outcome": "selected", "optionId": option_id})
        }
        PermissionDecision::Cancelled => json!({"outcome": "cancelled"}),
    };
    frame(json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}}))
}

/// The honest answer to a request for a capability we never advertised.
pub fn method_not_found_response(id: &Value, method: &str) -> String {
    frame(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!(
                "Faiden does not implement {method}: it advertises no filesystem or \
                 terminal capabilities to the agent"
            )
        }
    }))
}

// --- incoming frames --------------------------------------------------------

pub fn parse_line(line: &str) -> Result<Incoming, ProtocolError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(ProtocolError::Oversized { bytes: line.len() });
    }
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|e| ProtocolError::Malformed(truncate_for_log(&e.to_string())))?;
    let Some(object) = value.as_object() else {
        return Err(ProtocolError::Unrecognised(truncate_for_log(line)));
    };

    let has_id = object.contains_key("id") && !object["id"].is_null();
    match (has_id, object.get("method")) {
        (true, Some(Value::String(method))) => Ok(Incoming::Request {
            id: object["id"].clone(),
            method: method.clone(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (false, Some(Value::String(method))) => Ok(Incoming::Notification {
            method: method.clone(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (true, None) => parse_reply(object),
        _ => Err(ProtocolError::Unrecognised(truncate_for_log(line))),
    }
}

fn parse_reply(object: &Map<String, Value>) -> Result<Incoming, ProtocolError> {
    let id = object["id"].clone();
    if let Some(error) = object.get("error") {
        return Ok(Incoming::ErrorResponse {
            id,
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the agent reported an error with no message")
                .to_string(),
        });
    }
    match object.get("result") {
        Some(result) => Ok(Incoming::Response {
            id,
            result: result.clone(),
        }),
        None => Err(ProtocolError::Unrecognised(
            "reply has neither result nor error".to_string(),
        )),
    }
}

/// Reads a `session/update` notification. `None` means the payload was not a
/// session update we can attribute to a session, which the caller counts rather
/// than turning into an empty transcript entry.
pub fn parse_session_update(params: &Value) -> Option<SessionUpdate> {
    let session_id = params.get("sessionId")?.as_str()?.to_string();
    let update = params.get("update")?;
    let kind = update.get("sessionUpdate")?.as_str()?;

    let delta = match kind {
        "agent_message_chunk" => TranscriptDelta::AgentMessage {
            text: content_text(update.get("content")),
        },
        "agent_thought_chunk" => TranscriptDelta::Thought {
            text: content_text(update.get("content")),
        },
        "user_message_chunk" => TranscriptDelta::Other {
            // The agent echoing our own prompt back; Faiden already recorded it.
            kind: kind.to_string(),
        },
        "tool_call" | "tool_call_update" => TranscriptDelta::ToolCall {
            tool_call_id: update.get("toolCallId")?.as_str()?.to_string(),
            title: string_field(update, "title"),
            kind: string_field(update, "kind"),
            status: string_field(update, "status"),
            content: content_text(update.get("content")),
        },
        other => TranscriptDelta::Other {
            kind: other.to_string(),
        },
    };
    Some(SessionUpdate { session_id, delta })
}

/// Reads a `session/request_permission` payload. `None` means the request was
/// unanswerable as sent — no session, or no option to choose — and the caller
/// must refuse it rather than invent an answer.
pub fn parse_permission_request(params: &Value) -> Option<PermissionRequest> {
    let session_id = params.get("sessionId")?.as_str()?.to_string();
    let tool_call = params.get("toolCall")?;
    let tool_call_id = tool_call
        .get("toolCallId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut options = Vec::new();
    for raw in params.get("options")?.as_array()? {
        let option_id = raw.get("optionId")?.as_str()?.to_string();
        options.push(PermissionOption {
            name: raw
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&option_id)
                .to_string(),
            kind: raw
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            option_id,
        });
    }
    if options.is_empty() {
        return None;
    }

    let title = tool_call
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("The agent requested permission")
        .to_string();
    let mut detail = content_text(tool_call.get("content"));
    if detail.trim().is_empty() {
        // Fall back to the raw input the agent attached, rendered as text. It
        // is displayed, never executed.
        if let Some(raw_input) = tool_call.get("rawInput") {
            detail = raw_input.to_string();
        }
    }

    Some(PermissionRequest {
        session_id,
        tool_call_id,
        title,
        detail,
        options,
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Flattens ACP content into plain text. Content arrives either as one block or
/// as an array of blocks, and tool content wraps a block under `content`.
/// Anything that is not text contributes a labelled placeholder rather than
/// being dropped or rendered as if it were text.
fn content_text(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| content_text(Some(item)))
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(object) => {
            if let Some(Value::String(text)) = object.get("text") {
                return text.clone();
            }
            // `{"type":"content","content":{...}}` — a tool call content block.
            if let Some(inner) = object.get("content") {
                return content_text(Some(inner));
            }
            match object.get("type").and_then(Value::as_str) {
                Some(other) => format!("[{other} content]"),
                None => String::new(),
            }
        }
        Value::String(text) => text.clone(),
        _ => String::new(),
    }
}

fn truncate_for_log(text: &str) -> String {
    const LIMIT: usize = 200;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}… (truncated)")
}
