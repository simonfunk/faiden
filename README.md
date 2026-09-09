# Faiden

**The agent-first terminal. Never lose the thread.**

Faiden is an early, local-first desktop terminal built around enduring topics rather than mandatory Git worktrees or disposable model contexts. Start with a free-form thread; attach a directory and terminal when needed.

> **Early build.** It runs real shells and can start a **new** Hermes ACP session. It does not import or resume existing Hermes/Warp sessions, and it has no OpenClaw adapter, context rollover, remote hosts or tickets. A handoff draft is not an AI session resume.

## Implemented foundation

- Threads, editable notes, session lineage and explicit seen/reviewed state stored locally in SQLite.
- Actual PTY-backed login shells rendered with xterm.js; switching threads retains running terminals.
- Optional directory bindings, repository/branch/HEAD/dirty metadata and linked-worktree detection.
- Git context is a snapshot of the **selected directory**, not proof of an agent's current tool cwd or an SSH/container cwd.
- Editable handoff drafts with predecessor references; no automated AI summarization or hidden prompt submission.
- Bounded retained output, streaming UTF-8 decoding, epoch/sequence correlation, lifecycle reconciliation and tested macOS terminal cleanup.
- Browser preview explicitly disables native capabilities instead of simulating them.

## Hermes agent sessions (ACP)

A thread can own one **new** Hermes session over the Agent Client Protocol. Faiden
spawns the Hermes you installed as `hermes acp` with piped stdio and speaks
newline-delimited JSON-RPC to it.

- **Real status, never inferred from liveness.** `connecting → ready → responding
  → awaiting permission → cancelling → turn ended`, plus `disconnected` and
  `error`, each derived from an actual request or event. A running process that
  has not completed `initialize` *and* `session/new` is reported as *starting*,
  not ready.
- **A finished turn is not a success claim.** Faiden shows the literal ACP
  `stopReason` (`end_turn`, `refusal`, `cancelled`, …) and says so in words.
- **Approvals are shown as the agent sent them** — title, tool detail and the
  exact options it offered — and only an offered option can be sent back.
  Anything else fails closed to ACP's `cancelled` outcome: your own deadline
  (shorter than Hermes's 60 s auto-deny), cancelling, stopping, quitting, a
  request naming a session this run does not own, a request arriving with no
  turn in flight, and an approval whose audit row could not be written.
- **Streaming transcript** of assistant text, thinking and tool calls, persisted
  in SQLite and bounded — a truncated tail says it is truncated.
- **Sidebar attention** from real state: *Needs you* only for a genuinely pending
  approval, plus a count of results you have not looked at yet.
- **Cancel and stop** are explicit. Nothing is ever sent without pressing Send.

### What it is not

- **Not a sandbox.** The agent runs the Hermes you installed, with your user's
  authority, your Hermes configuration and any MCP servers you configured
  globally. Faiden removes inherited approval-bypass variables
  (`HERMES_YOLO_MODE`, `HERMES_ACP_AUTO_APPROVE`, `HERMES_NONINTERACTIVE`,
  `HERMES_EXEC_ASK`, `HERMES_CRON_SESSION`) before starting the child, and never
  passes `--yolo` or any auto-approval flag. A bypass configured inside your own
  `~/.hermes` config or `.env` is loaded by Hermes itself and is outside Faiden's
  control.
- **No installation, setup or sign-in.** If `hermes` is not found on `PATH`,
  `~/.local/bin`, `~/.hermes/bin`, `/opt/homebrew/bin` or `/usr/local/bin`,
  Faiden names every directory it searched and stops. Credentials stay with the
  installed harness.
- **No context reporting.** Faiden does not display a context budget or token
  usage; the ACP `usage_update` variant is received but deliberately not shown,
  because this build has not verified what it means.
- **No question detection.** ACP has no clarification protocol. If Hermes asks
  you something it arrives as ordinary assistant text and the turn ends with
  `end_turn`; Faiden does not parse prose to guess that a question was asked.
- **No import, load, resume, fork or model/mode switching.** `session/load`,
  `session/resume`, `session/fork`, `session/set_model` and `session/set_mode`
  are not used, so existing Hermes sessions cannot be attached.
- **Agents do not survive quitting Faiden**, exactly like terminals. Interrupted
  runs are reconciled as *disconnected* on the next start; nothing is reattached
  and no prompt is ever replayed.
- **Not yet exercised against a live provider by this build's own tests.** Every
  automated test drives a labelled Python fixture peer over real pipes; no test
  calls a model.

## Session context and reviewed handoff

Two questions the interface now answers with stored facts rather than guesses:
*what is this session actually bound to*, and *what did Faiden actually supply*.

- **Binding and launch directory are separate facts.** The context panel shows
  the directory bound to the thread now, and, separately, the directory the
  running agent was *started* in, with the executable path and how it was found.
  Re-binding a thread does not move a live process, so when the two differ the
  panel says so instead of showing one number for both.
- **Freshness is labelled.** A binding that has not been inspected says so; one
  whose directory has gone away says so; an inspected one carries the time it
  was read. Branch and working-tree facts stay in the bar above and describe the
  directory *now*, never what the agent saw at launch.
- **Nothing is described in the present tense once it has ended.** With no live
  run the panel either says nothing is running, or presents a past run as
  explicitly historic, from Faiden's durable record.
- **"What Faiden supplied" is exactly two things**: the briefing saved on the
  owning session — *prepared text, not sent* — and the prompts you pressed Send
  on, each carrying the durable status the native side recorded (`sent`,
  `sending`, `not sent: …`). Saved notes, repository files, shell output and the
  historical transcript are **not** supplied by Faiden and are never claimed to
  be. Faiden also states that it cannot see Hermes' own system prompt, internal
  or tool context, or model token usage. When the transcript read was itself a
  bounded tail, the list is labelled as incomplete rather than presented as
  everything that was sent.

### Handoff: draft, review, start a **new** session

A handoff is a human-authored briefing, composed locally and deterministically.

- The draft is the native draft text (thread notes, predecessor session,
  environment, editable *Goal / Decisions / Open tasks* placeholders) plus
  verbatim excerpts, each labelled with the session, run and message id it was
  copied from. Faiden does not summarise, infer or invent any of it.
- **Bounds, stated in the draft itself**: at most the 3 most recent agent runs
  are read, at most 8 excerpts are quoted (the newest; the count of older ones
  dropped is printed), and each excerpt is cut at 600 characters with the cut
  and the full length disclosed. The finished draft — notes, any previous
  briefing and excerpts together — is bounded to the 40 000 Unicode code points
  the database accepts for a briefing, counted the way the store counts them,
  and a draft that had to be cut says so.
- **Missing evidence is named, never shown as absence.** A transcript that could
  not be read is reported as *unknown*, not as a run that said nothing; a
  bounded transcript tail is reported as partial; agent runs beyond the read cap
  are counted. A draft assembled from complete evidence carries none of these
  notices.
- **Excluded by rule and named in the text**: private agent reasoning
  (`thought`), tool calls, tool output, standard error and permission payloads.
  Only what you wrote and what the agent said back can be quoted.
- Drafts are editable, saved as a `handoff-draft` session record, and can be
  **reopened verbatim** after a thread switch or an app restart.
- Starting from reviewed text creates a **new** `hermes-acp` session — never a
  resume, load or import — stores the text as that session's briefing, records a
  durable predecessor link inside the same thread (validated natively), and
  places the text in the composer. **It is not sent.** Sending stays one
  explicit press of Send, and edits made in the composer are what get sent.
- **A retry never reuses the wrong text.** If a start is rejected and you then
  edit the draft, Faiden closes the record it had created and makes a fresh one
  carrying the corrected text and lineage; an unchanged retry reuses an open record, but creates a fresh same-thread
  record when native startup has already ended the previous one.
- A start is **blocked** while the thread still owns a live agent: Faiden never
  interrupts a running session for you. Viewing, saving or starting sends
  nothing; a rejected start keeps the draft on screen. Retry preserves the reviewed
  text and predecessor without attempting to restart an ended session.

## Development

Prerequisites: macOS with Xcode Command Line Tools, a current stable Rust toolchain, Node.js and pnpm. The initial verified host is Apple Silicon macOS. Windows/Linux are design targets, **not yet validated**; macOS-specific descendant cleanup is not a cross-platform guarantee.

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

If Cargo is not on your PATH, load your Rust environment first: `. "$HOME/.cargo/env"`.

Browser-only preview (no shell/database/filesystem functionality):

```sh
pnpm dev --host 127.0.0.1
```

Tests use temporary repositories/databases, not personal projects:

```sh
pnpm typecheck
pnpm test
pnpm build
cd src-tauri
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

## Build a Mac application

```sh
pnpm tauri build --bundles app
```

Output: `src-tauri/target/release/bundle/macos/Faiden.app`.

The local prototype uses the canonical install location `~/Applications/Faiden.app`. A raw bundle may need whole-bundle signing before installation. Local ad-hoc signing is only a development convenience, not notarization or a stable distribution/TCC identity:

```sh
codesign --force --deep --sign - src-tauri/target/release/bundle/macos/Faiden.app
codesign --verify --deep --strict src-tauri/target/release/bundle/macos/Faiden.app
```

Quit the existing app before replacing its installed bundle. Do not install another copy under `/Applications` with the same bundle identifier (`dev.faiden.app`).

## Lifecycle and privacy

- No background service: quitting Faiden ends owned terminals. Do not expect long-running work to survive application quit.
- No automatic reconnection to existing agent sessions, no credential import and no background provider calls.
- Starting a shell runs your local login shell with your user's authority and shell configuration. This is **not a sandbox**.
- Notes and session metadata persist in plaintext SQLite at `~/Library/Application Support/dev.faiden.app/faiden.sqlite3` on macOS. Runtime terminal scrollback is bounded and not a durable full transcript.
- Incomplete persisted terminal runs are reconciled as unknown after restart; they are not presented as live agents.
- Agent transcripts, approval requests and their answers are persisted in the same plaintext SQLite database. Prompts you type are stored; model output is stored as it streams.
- One Hermes child process per agent session, so two threads cannot contaminate each other's cwd or approval state. Quitting Faiden ends them.
- Corrupt/unavailable storage fails rather than silently recreating an empty database. Back up the data directory before manual recovery; there is no recovery UI yet.

## Roadmap

1. Stabilize native lifecycle, terminal usability and keyboard workflows.
2. Extend the Hermes ACP adapter beyond new sessions: honest load/resume semantics, context budget from advertised metrics, model and mode selection.
3. Deepen actionable results/decisions and attention views, independent of execution state.
4. Carry brainstorming into linked tickets and worktrees without changing the thread's identity.
5. Add OpenClaw/other adapters, remote execution and verified Windows/Linux support.

See [CONCEPT.md](CONCEPT.md), the [foundation plan](docs/plans/0001-foundation.md)
and the [Hermes ACP plan](docs/plans/0002-hermes-acp.md).

## Open source and contributing

Faiden is developed at [simonfunk/faiden](https://github.com/simonfunk/faiden) under the [MIT License](LICENSE). You may use, modify, and redistribute it, including commercially, subject to the license terms. Third-party dependencies retain their own licenses; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Contributions are welcome! Start with [CONTRIBUTING.md](CONTRIBUTING.md), open a focused bug report or feature proposal, and follow our [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

This is an independent early-stage project, not an official Hermes/Nous Research product. Signed/notarized releases are not yet available.
