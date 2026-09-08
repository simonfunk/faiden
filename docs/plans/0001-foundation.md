# Faiden foundation implementation plan

**Goal:** Produce and exercise the first native Mac foundation described in CONCEPT.md, without pretending the full agent integration already exists.

**Architecture:** React frontend talks to a narrow Tauri/Rust backend. SQLite stores threads, notes, environment bindings, session lineage and review items. App-owned PTYs stream bounded output into xterm.js; the service boundary remains separable for later daemon extraction.

**Stack:** Tauri 2, Rust, portable-pty, rusqlite, React, TypeScript, Vite, xterm.js, Vitest/Testing Library. One coupled implementation writer; independent review follows.

## 1. Bootstrap and thread persistence
Files: package.json, tsconfig*, vite.config.ts, src/main.tsx, src/domain/*, src-tauri/Cargo.toml, src-tauri/tauri.conf.json, src-tauri/src/store.rs.
- Configure only the tools needed for the first slice.
- Test creating and loading a thread without a directory in a temporary SQLite database. Run RED, implement, run GREEN.
- Test renamed title and notes persistence; input validation and bounded text.
- Test human seen/reviewed states separately; selecting a thread must not set reviewed.

## 2. Session lineage and continuation draft
Files: src/domain/*, src-tauri/src/store.rs, associated tests.
- Test predecessor -> successor within same thread; source briefing preserved.
- Fresh continuation drafts require no active terminal for source; stale/invalid IDs fail closed.
- UI explicitly labels this as a briefing/session record until an actual harness session is connected. No fake ACP state.

## 3. Native execution
Files: src-tauri/src/terminal.rs, src-tauri/src/lib.rs, src/bridge.ts, src/components/TerminalView.tsx.
- Test real disposable PTY command output/exit, resize bounds, closed-ID write failure, duplicate start and cleanup.
- Spawn shell only on explicit click; switching views must not spawn duplicates or terminate process.
- Bounded retained output, UTF-8-safe streaming, serialized ownership, authoritative process exit. Separate terminal lifecycle from agent semantic state.
- Browser preview must disable native actions and say native connection unavailable, not mock a terminal.

## 4. Git context
Files: src-tauri/src/environment.rs, src/components/ContextBar.tsx and tests.
- Native folder picker or validated path binding initiated by user.
- Test temporary non-repo directory and disposable Git repository/worktree, dirty state, detached HEAD and removed path.
- Show selected/initial directory provenance honestly; do not claim current agent tool cwd tracking exists.
- Never permit implicit directory mutation for running terminal.

## 5. Usable app shell
Files: src/App.tsx, src/styles.css, src/components/*, frontend tests.
- Thread sidebar, new/rename/select, editable briefing, terminal/session list, context bar, notes/continuation, review inbox.
- Empty state, no fabricated activity. Dark restrained indigo/neutral styling; accessible named controls, semantic HTML, keyboard focus.
- Async errors visible; initialization event races guarded; strict TypeScript.

## 6. Release gates
- pnpm test, pnpm typecheck, pnpm build.
- cargo fmt --check, cargo test, cargo check; real tauri bundle.
- No configured signing identity is invented. Ad-hoc development build labeled as such; distribution signing is later.
- Independent full-code security/lifecycle review; fix and rerun.
- Launch actual .app, execute harmless PTY command through UI, test persistence and worktree metadata. Save local verification evidence outside tracked content.
- README distinguishes implemented vs planned; cross-platform claims only if tested.
- No remote publication/push. Record license and GitHub owner as pre-publication decisions.

## Delivery profile
Discovery began 2026-09-08T11:05:09+02:00. Record real phase timestamps and test/build durations, do not estimate retroactively.
