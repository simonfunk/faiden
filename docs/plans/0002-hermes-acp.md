# Faiden: first real Hermes ACP integration

**Goal:** a working vertical slice where a Faiden thread can own a *new* Hermes
ACP session: real streaming assistant/tool output, real permission requests the
user answers, real cancel, real durable transcript, and sidebar status derived
from protocol facts rather than from "a process exists".

**Out of scope, deliberately:** importing or resuming existing Warp/Hermes
sessions, `session/load`, `session/resume`, model/mode pickers, a background
daemon, remote hosts, worktree automation, context rollover. The generic PTY
shell path is untouched and stays fully usable.

## 1. What the protocol actually is (verified, not assumed)

Read from the installed adapter at `~/.hermes/hermes-agent/acp_adapter` and the
`acp` Python SDK it imports (`.../venv/lib/python3.11/site-packages/acp`,
schema tag `v0.11.2`, `PROTOCOL_VERSION = 1`). Nothing below is inferred from
prose docs alone.

* **Transport:** stdio, newline-delimited JSON-RPC 2.0. `acp/stdio.py` feeds the
  agent's stdin with `sys.stdin.buffer.readline()`; `acp/connection.py` writes
  `{"jsonrpc":"2.0", ...}` objects terminated by `\n`. stdout is protocol-only —
  `entry.py::_setup_logging` routes *all* logging to stderr for exactly this
  reason. So: **stdout is parsed, stderr is drained and bounded as diagnostics.**
* **Launch:** `hermes acp` (`acp_adapter/entry.py`). We spawn the resolved
  executable with argv `["acp"]` and piped stdio — never a shell string.
* **Agent methods** (`acp/meta.py::AGENT_METHODS`): `initialize`,
  `authenticate`, `session/new`, `session/prompt`, `session/cancel`,
  `session/load`, `session/resume`, `session/fork`, `session/list`,
  `session/close`, `session/set_mode`, `session/set_model`,
  `session/set_config_option`. We use `initialize`, `session/new`,
  `session/prompt` and `session/cancel`. Nothing else.
* **Client methods** (`CLIENT_METHODS`): `session/update` (notification),
  `session/request_permission`, `fs/read_text_file`, `fs/write_text_file`,
  `terminal/create|output|release|wait_for_exit|kill`.
  We implement `session/update` and `session/request_permission` and advertise
  **nothing else**: `clientCapabilities = {fs:{readTextFile:false,
  writeTextFile:false}, terminal:false}`. Any other client request is answered
  with JSON-RPC `-32601 method not found`, which is the honest answer for a
  capability we did not advertise.
* **`session/new` params:** `{cwd: <absolute>, mcpServers: []}` — `cwd` is
  required and must be absolute (`schema.py::NewSessionRequest`). Response:
  `{sessionId, models?, modes?, configOptions?}`.
* **`session/prompt` params:** `{sessionId, prompt:[{type:"text",text}]}`.
  Response: `{stopReason}` where `stopReason ∈ {end_turn, max_tokens,
  max_turn_requests, refusal, cancelled}`.
* **`session/cancel`** is a *notification* (no id, no response). The in-flight
  `session/prompt` still returns, normally with `stopReason: "cancelled"`.
* **`session/update` variants** (`SessionNotification.update`, discriminated by
  `sessionUpdate`): `user_message_chunk`, `agent_message_chunk`,
  `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`,
  `available_commands_update`, `current_mode_update`, `config_option_update`,
  `session_info_update`, `usage_update`. Tool calls carry `toolCallId`, `title`,
  `kind ∈ {read,edit,delete,move,search,execute,think,fetch,switch_mode,other}`,
  `status ∈ {pending,in_progress,completed,failed}`.
* **`session/request_permission` params:** `{sessionId, toolCall:
  ToolCallUpdate, options:[{optionId, name, kind}]}` with
  `kind ∈ {allow_once, allow_always, reject_once, reject_always}`. Hermes's
  concrete option ids are `allow_once`, `allow_session`, `allow_always`,
  `deny`, `deny_always` (`acp_adapter/permissions.py`).
  **Response:** `{"outcome":{"outcome":"selected","optionId":…}}` or
  `{"outcome":{"outcome":"cancelled"}}` — note that ACP spells "denied" as
  `cancelled` (`schema.py::DeniedOutcome`).
  Hermes auto-denies after **60 s** without a response
  (`permissions.py::make_approval_callback(timeout=60.0)`), so Faiden's own
  deadline is deliberately shorter and fails closed first.

There is **no clarification/question protocol** in ACP. An agent asking the user
something arrives as an ordinary `agent_message_chunk` and the turn ends with
`stopReason: "end_turn"`. Faiden therefore does **not** claim to detect
questions, and labels a finished turn as *"turn ended (end_turn) — this is not a
claim that the task succeeded"*. Answering is just the next prompt.

## 2. Implementation decisions

**One subprocess per owned session.** Each Faiden agent run owns its own
`hermes acp` child, spawned with the thread's directory as cwd. Hermes binds
per-session state (cwd, session key, approval callback) inside one process, and
several of its caches are process-global; a private child is the only way two
Faiden threads cannot contaminate each other's safety state or cwd. The cost is
one Python process per live agent session, which we accept and disclose.

**Executable resolution.** `agent::locate` searches `PATH` and then the
GUI-invisible locations a Tauri app does not inherit: `~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.hermes/bin`. No hardcoded personal
path ships. If nothing is found the error names every directory searched and
tells the user to install Hermes — Faiden never installs, authenticates, or
runs a setup flow on the user's behalf.

**Environment.** The child inherits the user's environment because that is where
its credentials and configuration live (auth stays with the installed harness).
Before spawning we *remove* the inherited approval-bypass variables that are
real, verified switches in the installed Hermes — `HERMES_YOLO_MODE`,
`HERMES_ACP_AUTO_APPROVE`, `HERMES_NONINTERACTIVE`, `HERMES_EXEC_ASK`,
`HERMES_CRON_SESSION` — so a variable exported in the parent shell cannot
silently turn Faiden's approval UI into a rubber stamp. We do **not** invent env
flags, we do **not** pass `--yolo`/`--accept-hooks` (we pass argv `["acp"]` and
nothing else), and we never write to the user's `~/.hermes` config. A bypass
configured *inside* the user's own `~/.hermes/.env` or `config.yaml` is loaded by
the adapter itself and is outside Faiden's control; the UI says so.

**MCP.** We send `mcpServers: []`, meaning *Faiden contributes no extra MCP
servers*. We deliberately do **not** set `HERMES_ACP_SKIP_CONFIGURED_MCP=1`: the
user's globally configured tools stay available, because silently amputating
them would be a worse surprise than the startup cost. `entry.py` already starts
that discovery on a background daemon thread, so it cannot block the handshake;
our own `initialize` deadline bounds startup regardless. This is disclosed in
the UI: the agent runs with the user's Hermes tools and authority — **not a
sandbox**.

**State, derived only from protocol events.**
`connecting → ready → responding → (awaiting_permission) → turn_complete`,
plus `cancelling`, `disconnected`, `error`. `connecting` means the child is
alive but `initialize`+`session/new` have not both returned. A live process is
never reported as `ready`. `turn_complete` records the literal `stopReason`.

**Late events.** Every run has a `runId` and an `epoch`; every emitted event
carries `epoch` and a per-run `seq`. The UI subscribes *before* it snapshots and
discards anything with `seq < snapshot.nextSeq`, the same discipline
`terminal.rs` already uses. Responses whose JSON-RPC id is unknown or already
settled are dropped with a counter, never applied.

**Exclusive prompt.** One in-flight `session/prompt` per run. A second Submit is
refused with a `CONFLICT`, so a slow turn cannot produce duplicate submissions.
A user message row is written *before* the request goes out and is marked
`failed` if the write to the child fails — a message never appears as sent when
it was not.

**Permissions fail closed.** A pending request is answerable exactly once, only
by its own `requestId`, and only with an `optionId` the agent actually offered.
Cancel, disconnect and our own timeout all resolve it as
`{"outcome":{"outcome":"cancelled"}}` — never as an allow, and never with a
"permanent" option the user did not choose.

**Concurrency.** Reader thread (stdout), stderr drain thread, and a serialized
newline writer behind its own mutex. No global lock is held across IO: the
manager's registry lock is taken to clone an `Arc<AgentRun>` and released before
any write or wait. Deadlines bound the handshake and every RPC.

**Persistence.** Additive schema at `user_version = 2`: `agent_runs`,
`agent_messages`, `agent_permission_requests`. Existing tables and rows are
untouched. On restart, open runs are reconciled to
`disconnected — Faiden exited while this agent session was running`; nothing is
reattached and no prompt is ever replayed.

## 3. Test contract

Deterministic tests never talk to Hermes and never call a provider. They spawn a
**fixture peer**: a small `python3` script, written into a `TempDir` by the test
itself, that speaks the same newline JSON-RPC over stdio. It is labelled a
fixture everywhere it appears and is not reachable from production code or the
UI — the Tauri command layer resolves the program itself and never accepts one
from the webview.

Covered: two simultaneous isolated sessions; fragmented, malformed and oversized
lines; `initialize` failure and `session/new` failure; JSON-RPC id correlation
including a stale/unknown id; streamed message and tool-call updates; permission
allow, deny, timeout and stale answer; cancel; every `stopReason`; agent-side
error responses; EOF/child death; restart reconciliation; DB migration
preserving pre-existing rows; and on the frontend: no lost update on view
switch, listener disposal, stale-selection responses, seen ≠ reviewed.

Gates: `pnpm typecheck`, `pnpm test`, `pnpm build`, `cargo fmt --check`,
`cargo test`, `cargo clippy --all-targets -- -D warnings`. No release bundle is
built here; the parent owns that. No live LLM call is made by this work.

## 4. Corrections after independent review

Two REDs were independently reproduced by the parent against this library, and
two provisional review reports raised further fail-closed concerns. Each was
reproduced with a deterministic fixture, given a repository regression test, then
fixed minimally.

**Single owner per session (reproduced by `race_probe`).** `AgentManager::start`
checked an in-memory registry, released the lock, and only then inserted and
spawned, so two barrier-synchronised starts for one session both won. The check
now lives in the database: migration 3 adds
`agent_runs_one_open_per_session UNIQUE(session_id) WHERE ended_at IS NULL`, and
`start_agent_run` also refuses a session that has already ended. Two independent
`Store` connections therefore cannot both win either. The loser gets a
`CONFLICT`, and its start never reaches `Command::spawn`.

**Permissions that were never the user's to answer (reproduced by
`permission_probe`).** A `session/request_permission` naming a different ACP
session reached the approval UI while a real prompt was active. `handle_request`
now refuses, before anything is persisted or shown, when the request names a
session this run does not own, when no turn is in flight, or when the run is
ending or ended — always with ACP's `cancelled` outcome, counted in
`refused_requests`.

`ended_at` alone could not express "ending": it is `settle`'s first-writer guard
and is set only once the child is gone, while frames already buffered in the pipe
keep arriving during teardown. A separate `closing` flag is raised before stdin
is closed, so nothing that arrives during shutdown becomes an approval prompt.

**Durable before allowed.** `resolve_permission` wrote the JSON-RPC answer first
and ignored a failed audit write, so a transient database failure could let the
agent proceed on an approval nothing durable remembered. The audit row is now
committed first; if that fails, an *allow* is downgraded to `cancelled` and the
caller is told. A denial is still sent either way — refusing to deny is the
unsafe direction.

**Turn attribution.** `session/update` was accepted purely by session id, so a
chunk arriving after a turn's reply repainted the finished turn as streaming, or
was attributed to the next one. Updates now require a turn in flight and are
otherwise counted in `unattributed_updates`.

**Startup honesty.** `initialize` responses are checked against the protocol
version this build implements, and a `session/new` id that cannot be recorded
fails the startup instead of publishing `ready` over a row that could not
identify the run after a restart.

**Worker-thread failures.** A reader or handshake thread that cannot be created
now kills and reaps the child, settles the durable row with a truthful outcome
and drops the run from the registry, rather than propagating an error over an
orphaned process.

**Store bounds.** `list_agent_messages` clamps its own limit
(`LIMIT_AGENT_LIST_MAX`); a `usize` above `i64::MAX` used to cast negative, which
SQLite reads as no limit at all.

**Unread results.** An agent never rewrites a thread's `seen_at`/`reviewed_at` —
those record what a human did. Results are surfaced as unseen review items
instead, counted per thread by `unseen_review_counts` and shown in the sidebar,
so a thread that was already reviewed becomes actionable again without the
record of that review being falsified.
