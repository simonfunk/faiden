# Faiden

The agent-first terminal. Never lose the thread.

## Product contract
Faiden is a local-first desktop terminal organized around enduring **threads**, not disposable model contexts or Git worktrees. Start brainstorming without a repository. Later attach tickets and execution environments without losing the original discussion. Technical harness sessions and runs live underneath the thread. Multiple sessions may serve one thread; contexts can be continued, replaced through a handoff, or branched.

Primary platform: macOS. Architecture supports future Windows/Linux builds, but support is claimed only after actual platform tests. Intended release: open source on GitHub; repository owner, license and publication remain explicit pre-publication decisions. No code copied from Warp, cmux or Orca.

## User-visible model
- Threads: durable topics, searchable/renameable, usable without Git.
- Sessions: particular harness contexts, with predecessor links and preserved history.
- Runs: an individual execution, with process and event identities.
- Environments: optional host, directory, repository, worktree, branch.
- Inbox: unseen results, seen-but-unreviewed results, questions and approvals.
- Artifacts: references to outputs/evidence, not claims that work succeeded.

## Correctness rules
- Process alive, terminal output and CPU use are NOT proof of agent semantic activity.
- Connection health, execution state and human review state are independent.
- No fabricated progress, cost, context usage, completion or verification.
- Associate late events with their exact session/run/process epoch. Never apply old completion to new work.
- An assigned folder is NOT proven tool execution cwd. Label provenance and freshness. Never claim arbitrary subprocess/SSH/container cwd is observed without a source.
- Opening a thread does not approve a result. Seen and reviewed are distinct.
- Context continuation is not lossless. Handoff carries goals, decisions, rejected approaches, open tasks, verified evidence, environment and active work. Full source history remains available for retrieval.
- Fresh-context continuation is distinct from resuming the same harness session. Never duplicate live writers or abandon running children silently.
- No repository, ticket or worktree required for a new thread.
- Worktree removal must not delete the thread/history. Multiple implementation branches may derive from one discussion.

## Architecture
Tauri 2 desktop host; React/TypeScript/Vite frontend; xterm.js terminal; Rust async runtime/process boundary; portable-pty; SQLite. Native commands form an explicit authority boundary. No remote web content receives native execution privileges. Terminal bytes never become application commands. No telemetry, cloud upload, credential copying or API spend by default.

Target architecture separates a durable local session service from the window. First implementation can use an app-owned runtime but MUST disclose that full app exit ends terminals; closing/reopening a view may not kill a terminal. No hidden daemon autostart or persistence claims before it is implemented and exercised.

ACP is the preferred structured adapter for Hermes and other supporting harnesses, with capabilities negotiated rather than assumed. OpenClaw ACP has different capabilities; use a gateway adapter only when needed. Generic CLI sessions remain fully usable with explicitly limited status. Harness credentials remain with the installed harness.

## Delivery sequence
### First working foundation
Real native Mac app, no seeded demo activity. Thread create/rename/select, free-form notes/briefing, explicit folder binding, real PTY shell in xterm, switching without process loss, bounded terminal output, SQLite persistence, honest lifecycle, read/review distinction, Git inspection for selected directory, continuation draft linked to predecessor and source notes. Continuation in this slice must not pretend it resumed an AI context. Generic installed Hermes can run inside the actual terminal; status stays limited until instrumentation exists.

### Agent integrations and continuity
Hermes ACP real streaming/tool/approval/cancel canary; retained CLI path for full TUI capabilities. Context budget from advertised metrics; explicit unknown otherwise. Structured handoff with source references and safe active-work check. OpenClaw adapter, lineage and capability matrix. Discover/import existing session histories with explicit scope; never falsely attach running Warp processes.

### Workflow and durability
Reviewed ticket creation (first adapter Linear), verified worktree creation/linking, execution environment per worker, remote session service via SSH, disconnect/reconnect reconciliation, replay cursors, process epoch recovery. A server host is required to continue computing while the laptop sleeps.

### Coordination and distribution
Cross-harness delegation, dependencies, concurrency conflicts, review artifacts, macOS notifications; supported Windows/Linux builds; signed/notarized releases, license and contributor/security documentation; GitHub publication after explicit review.

## Competitor evidence (research, not live feature certification)
- Warp supports Hermes CLI recognition but its checked CLI-agent support page excludes Hermes notifications: https://docs.warp.dev/agents/cli-agents/overview/
- cmux has notifications/unread state, workspace metadata and programmable status: https://cmux.com/docs/notifications and https://cmux.com/docs/api
- Orca installed reference includes Runs/Tasks/Dispatches, DAGs, ask/reply, completion, remote workers, workspace status and folder contexts. Therefore task-centric multi-agent orchestration alone is not a unique feature.
- Faiden's hypothesis is a simpler thread-first workflow, free brainstorming, safe fresh-context continuation and human review inbox, independent of Git ownership.
- Hermes ACP: https://hermes-agent.nousresearch.com/docs/user-guide/features/acp
- ACP: https://agentclientprotocol.com/protocol/overview

## Definition of done for the first foundation
Unit/component/native tests; strict typecheck; frontend production build; Rust fmt/test/check; real .app bundle; native launch and real PTY command roundtrip; data survives application restart; git metadata from actual disposable fixture; independent security/lifecycle review. Report any unexercised or blocked gate honestly. Browser-only previews cannot claim native terminal capabilities.
