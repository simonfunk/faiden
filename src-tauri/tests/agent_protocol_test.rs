//! ACP wire protocol: framing, correlation and payload shapes.
//!
//! Every expectation here is checked against the installed adapter and the
//! `acp` Python SDK schema (tag v0.11.2, PROTOCOL_VERSION 1) it imports — see
//! docs/plans/0002-hermes-acp.md. Nothing in this file performs IO.

use faiden_lib::agent::protocol::{
    self, Incoming, PermissionDecision, ProtocolError, StopReason, TranscriptDelta,
};
use serde_json::{json, Value};

fn parse(line: &str) -> Incoming {
    protocol::parse_line(line).expect("parse")
}

#[test]
fn initialize_advertises_only_capabilities_we_actually_implement() {
    let value: Value = serde_json::from_str(&protocol::initialize_request(1)).unwrap();
    assert_eq!(value["jsonrpc"], "2.0");
    assert_eq!(value["id"], 1);
    assert_eq!(value["method"], "initialize");
    assert_eq!(value["params"]["protocolVersion"], 1);

    let caps = &value["params"]["clientCapabilities"];
    assert_eq!(caps["fs"]["readTextFile"], false);
    assert_eq!(caps["fs"]["writeTextFile"], false);
    assert_eq!(
        caps["terminal"], false,
        "Faiden hosts no terminal for the agent; claiming otherwise would be a lie"
    );
    assert_eq!(value["params"]["clientInfo"]["name"], "faiden");
}

#[test]
fn a_new_session_names_an_absolute_directory_and_adds_no_mcp_servers() {
    let value: Value =
        serde_json::from_str(&protocol::session_new_request(7, "/Users/x/proj")).unwrap();
    assert_eq!(value["id"], 7);
    assert_eq!(value["method"], "session/new");
    assert_eq!(value["params"]["cwd"], "/Users/x/proj");
    assert_eq!(
        value["params"]["mcpServers"],
        json!([]),
        "Faiden contributes no MCP servers of its own"
    );
}

#[test]
fn a_prompt_is_one_text_block_addressed_to_a_session() {
    let value: Value =
        serde_json::from_str(&protocol::session_prompt_request(9, "sess-1", "hello")).unwrap();
    assert_eq!(value["id"], 9);
    assert_eq!(value["method"], "session/prompt");
    assert_eq!(value["params"]["sessionId"], "sess-1");
    assert_eq!(
        value["params"]["prompt"],
        json!([{"type":"text","text":"hello"}])
    );
}

#[test]
fn cancel_is_a_notification_and_therefore_carries_no_id() {
    let value: Value =
        serde_json::from_str(&protocol::session_cancel_notification("sess-1")).unwrap();
    assert_eq!(value["method"], "session/cancel");
    assert_eq!(value["params"]["sessionId"], "sess-1");
    assert!(
        value.get("id").is_none(),
        "session/cancel is a notification; an id would invent a response that never comes"
    );
}

#[test]
fn a_permission_answer_uses_acps_own_spelling_for_allow_and_deny() {
    let allow: Value = serde_json::from_str(&protocol::permission_response(
        &json!(4),
        PermissionDecision::Selected {
            option_id: "allow_once".to_string(),
        },
    ))
    .unwrap();
    assert_eq!(allow["id"], 4);
    assert_eq!(
        allow["result"]["outcome"],
        json!({"outcome": "selected", "optionId": "allow_once"})
    );

    // ACP spells "the user did not allow this" as `cancelled`.
    let deny: Value = serde_json::from_str(&protocol::permission_response(
        &json!("abc"),
        PermissionDecision::Cancelled,
    ))
    .unwrap();
    assert_eq!(deny["id"], "abc");
    assert_eq!(deny["result"]["outcome"], json!({"outcome": "cancelled"}));
}

#[test]
fn an_unadvertised_client_request_is_refused_as_method_not_found() {
    let value: Value = serde_json::from_str(&protocol::method_not_found_response(
        &json!(3),
        "fs/read_text_file",
    ))
    .unwrap();
    assert_eq!(value["id"], 3);
    assert_eq!(value["error"]["code"], -32601);
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("fs/read_text_file"), "got {message}");
    assert!(
        value.get("result").is_none(),
        "an error is not also a result"
    );
}

#[test]
fn every_frame_is_exactly_one_line_so_the_peer_can_read_it_with_readline() {
    for frame in [
        protocol::initialize_request(1),
        protocol::session_new_request(2, "/tmp"),
        protocol::session_prompt_request(3, "s", "multi\nline\nprompt"),
        protocol::session_cancel_notification("s"),
        protocol::permission_response(&json!(1), PermissionDecision::Cancelled),
        protocol::method_not_found_response(&json!(1), "terminal/create"),
    ] {
        assert!(
            !frame.contains('\n'),
            "frame must not embed a newline: {frame}"
        );
        serde_json::from_str::<Value>(&frame).expect("each frame is valid JSON");
    }
}

#[test]
fn responses_errors_requests_and_notifications_are_told_apart() {
    match parse(r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"s1"}}"#) {
        Incoming::Response { id, result } => {
            assert_eq!(id, json!(1));
            assert_eq!(result["sessionId"], "s1");
        }
        other => panic!("expected a response, got {other:?}"),
    }

    match parse(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"no provider"}}"#) {
        Incoming::ErrorResponse { id, code, message } => {
            assert_eq!(id, json!(2));
            assert_eq!(code, -32000);
            assert_eq!(message, "no provider");
        }
        other => panic!("expected an error response, got {other:?}"),
    }

    match parse(r#"{"jsonrpc":"2.0","id":3,"method":"fs/read_text_file","params":{}}"#) {
        Incoming::Request { id, method, .. } => {
            assert_eq!(id, json!(3));
            assert_eq!(method, "fs/read_text_file");
        }
        other => panic!("expected a request, got {other:?}"),
    }

    match parse(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s"}}"#) {
        Incoming::Notification { method, .. } => assert_eq!(method, "session/update"),
        other => panic!("expected a notification, got {other:?}"),
    }
}

#[test]
fn malformed_and_alien_lines_are_reported_not_guessed_at() {
    assert!(matches!(
        protocol::parse_line("this is not json"),
        Err(ProtocolError::Malformed(_))
    ));
    // A stray log line on stdout is malformed protocol, not an empty update.
    assert!(matches!(
        protocol::parse_line("2026-09-08 [INFO] hermes: starting"),
        Err(ProtocolError::Malformed(_))
    ));
    // Valid JSON that is not a JSON-RPC message at all.
    assert!(matches!(
        protocol::parse_line(r#"{"hello":"world"}"#),
        Err(ProtocolError::Unrecognised(_))
    ));
    assert!(matches!(
        protocol::parse_line("[1,2,3]"),
        Err(ProtocolError::Unrecognised(_))
    ));
}

#[test]
fn message_and_thought_chunks_are_kept_apart() {
    let update = protocol::parse_session_update(&json!({
        "sessionId": "s1",
        "update": {"sessionUpdate": "agent_message_chunk",
                   "content": {"type": "text", "text": "Hello "}}
    }))
    .expect("agent message chunk");
    assert_eq!(update.session_id, "s1");
    assert_eq!(
        update.delta,
        TranscriptDelta::AgentMessage {
            text: "Hello ".into()
        }
    );

    let thought = protocol::parse_session_update(&json!({
        "sessionId": "s1",
        "update": {"sessionUpdate": "agent_thought_chunk",
                   "content": {"type": "text", "text": "thinking"}}
    }))
    .expect("thought chunk");
    assert_eq!(
        thought.delta,
        TranscriptDelta::Thought {
            text: "thinking".into()
        }
    );
}

#[test]
fn tool_calls_carry_their_real_identity_title_kind_and_status() {
    let start = protocol::parse_session_update(&json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "tool_call",
            "toolCallId": "call-1",
            "title": "Read src/main.rs",
            "kind": "read",
            "status": "pending"
        }
    }))
    .expect("tool call");
    assert_eq!(
        start.delta,
        TranscriptDelta::ToolCall {
            tool_call_id: "call-1".into(),
            title: Some("Read src/main.rs".into()),
            kind: Some("read".into()),
            status: Some("pending".into()),
            content: String::new(),
        }
    );

    let progress = protocol::parse_session_update(&json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "42 lines"}}]
        }
    }))
    .expect("tool call update");
    match progress.delta {
        TranscriptDelta::ToolCall {
            tool_call_id,
            title,
            status,
            content,
            ..
        } => {
            assert_eq!(tool_call_id, "call-1");
            assert_eq!(title, None, "an update need not repeat the title");
            assert_eq!(status.as_deref(), Some("completed"));
            assert_eq!(content, "42 lines");
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
}

#[test]
fn an_update_variant_we_do_not_model_is_named_not_silently_dropped() {
    let update = protocol::parse_session_update(&json!({
        "sessionId": "s1",
        "update": {"sessionUpdate": "usage_update", "used": 10, "size": 100}
    }))
    .expect("usage update");
    assert_eq!(
        update.delta,
        TranscriptDelta::Other {
            kind: "usage_update".into()
        }
    );

    assert!(protocol::parse_session_update(&json!({"update": {}})).is_none());
}

#[test]
fn a_permission_request_is_read_with_its_offered_options_intact() {
    let request = protocol::parse_permission_request(&json!({
        "sessionId": "s1",
        "toolCall": {
            "toolCallId": "perm-check-1",
            "title": "Run a command: rm -rf build",
            "kind": "execute",
            "content": [{"type": "content", "content": {"type": "text", "text": "$ rm -rf build"}}]
        },
        "options": [
            {"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
            {"optionId": "allow_always", "name": "Allow always", "kind": "allow_always"},
            {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
        ]
    }))
    .expect("permission request");

    assert_eq!(request.session_id, "s1");
    assert_eq!(request.title, "Run a command: rm -rf build");
    assert!(request.detail.contains("rm -rf build"));
    assert_eq!(
        request
            .options
            .iter()
            .map(|o| o.option_id.as_str())
            .collect::<Vec<_>>(),
        vec!["allow_once", "allow_always", "deny"]
    );
    assert_eq!(request.options[1].kind, "allow_always");
}

#[test]
fn a_permission_request_without_options_is_unanswerable_and_refused() {
    assert!(protocol::parse_permission_request(&json!({
        "sessionId": "s1",
        "toolCall": {"toolCallId": "p1"},
        "options": []
    }))
    .is_none());
}

#[test]
fn stop_reasons_are_reported_literally_and_never_as_success() {
    for (raw, expected) in [
        ("end_turn", StopReason::EndTurn),
        ("max_tokens", StopReason::MaxTokens),
        ("max_turn_requests", StopReason::MaxTurnRequests),
        ("refusal", StopReason::Refusal),
        ("cancelled", StopReason::Cancelled),
    ] {
        assert_eq!(StopReason::parse(raw), expected);
        assert_eq!(expected.as_str(), raw);
    }
    assert_eq!(
        StopReason::parse("something_new"),
        StopReason::Other("something_new".to_string())
    );

    let label = StopReason::EndTurn.describe();
    assert!(label.contains("end_turn"), "got {label}");
    assert!(
        !label.to_lowercase().contains("success")
            && !label.to_lowercase().contains("completed the task"),
        "a finished turn is not a success claim: {label}"
    );
}
