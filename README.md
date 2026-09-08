# Faiden

**The agent-first terminal. Never lose the thread.**

Faiden is an early, local-first desktop terminal built around enduring topics rather than mandatory Git worktrees or disposable model contexts. Start with a free-form thread; attach a directory and terminal when needed.

> **Foundation, not a finished agent harness UI.** This build runs real shells. It does not yet integrate Hermes ACP, OpenClaw, automatic context rollover, remote hosts, tickets, semantic agent status, or agent-generated result ingestion. A handoff draft is not an AI session resume.

## Implemented foundation

- Threads, editable notes, session lineage and explicit seen/reviewed state stored locally in SQLite.
- Actual PTY-backed login shells rendered with xterm.js; switching threads retains running terminals.
- Optional directory bindings, repository/branch/HEAD/dirty metadata and linked-worktree detection.
- Git context is a snapshot of the **selected directory**, not proof of an agent's current tool cwd or an SSH/container cwd.
- Editable handoff drafts with predecessor references; no automated AI summarization or hidden prompt submission.
- Bounded retained output, streaming UTF-8 decoding, epoch/sequence correlation, lifecycle reconciliation and tested macOS terminal cleanup.
- Browser preview explicitly disables native capabilities instead of simulating them.

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
- Corrupt/unavailable storage fails rather than silently recreating an empty database. Back up the data directory before manual recovery; there is no recovery UI yet.

## Roadmap

1. Stabilize native lifecycle, terminal usability and keyboard workflows.
2. Add a permission-aware Hermes ACP adapter with honest new/load/handoff semantics and exclusive session ownership.
3. Add actionable results/decisions and attention views, independent of execution state.
4. Carry brainstorming into linked tickets and worktrees without changing the thread's identity.
5. Add OpenClaw/other adapters, remote execution and verified Windows/Linux support.

See [CONCEPT.md](CONCEPT.md) and the [foundation plan](docs/plans/0001-foundation.md).

## Publication status

Local development repository; public GitHub publication is planned, but has not happened. The open-source license, distribution signing and release workflow are still to be selected. No open-source license grant is implied until a LICENSE is added.
