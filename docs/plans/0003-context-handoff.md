# 0003 — Context visibility and reviewed handoff

Status: implemented (frontend only; no native change).

## Problem

Two things were invisible or, worse, easy to misread.

1. **Which context?** `ContextBar` shows the directory bound to the *thread* and
   its Git facts. A running agent was started in a directory that was correct at
   launch and can drift the moment the binding changes — a process is not moved
   by re-pointing a thread. One line for both facts invites a false reading.
2. **What did Faiden supply?** A saved briefing existed but was unreachable, and
   nothing distinguished *prepared* text from text a human actually sent, or a
   prompt that went out from one that failed on a closed pipe.

A handoff draft also existed but dead-ended: it could be saved and never
reopened, and there was no path from reviewed text to a new session.

## Non-goals

Genuine ACP load / resume / import stays out of scope. `session/load`,
`session/resume` and `session/fork` are still not used. A handoff starts a new
session; it never continues a model context.

## Design

Everything added is derived from data the existing native contracts already
return. No Tauri command was added or changed, and `src-tauri/**` is untouched
by this phase.

### `src/domain/handoff.ts` — pure, deterministic

- `composeHandoff(base, sources)` appends cited excerpts to the native draft
  text, which is copied through unmodified.
  - Quotable roles: `user`, `agent`. Everything else — `thought`, `tool`,
    `system` — is counted and named in the draft, never quoted. Permission
    payloads are not messages and never enter this path at all.
  - `HANDOFF_EXCERPT_MAX_CHARS = 600` per excerpt; truncation prints the cut and
    the full body length.
  - `HANDOFF_MAX_EXCERPTS = 8`, newest kept, `omittedOlder` printed.
  - `HANDOFF_MAX_CHARS = 40 000` applied to the **finished** text, mirroring
    `LIMIT_BRIEFING_CHARS` in `store.rs`, which validates with
    `value.chars().count()` — code points, not UTF-16 units. Long thread notes
    or a long previous briefing carried in `base.text` can exceed the limit on
    their own, so the bound is applied last and the cut is disclosed in the text
    itself. Cutting is code-point safe: a surrogate pair is never split.
  - Evidence completeness is a first-class output. A transcript that could not
    be read is reported as **unknown**, never as an empty run; a bounded tail is
    reported as partial; runs beyond the read cap are counted and named. Those
    three facts are printed only when they apply, so a complete draft makes no
    apologetic noise.
  - Sorted by `createdAt` with `messageId` as tie-break, so the same inputs
    always produce the same bytes.
- `describeSupplied(session, messages, transcriptTruncated)` returns the saved
  briefing (labelled *prepared, not sent*) and the `user` messages with a state
  mapped from the durable status strings written by `agent/runtime.rs::prompt` —
  `sent`, `sending`, `not sent: …`. The boundary disclosure has two forms: it
  may only claim to be "everything Faiden itself put into this run" when the
  whole transcript was in hand. Once `AgentSnapshot.truncated` is set, it says
  the list is drawn from a bounded tail and is not complete, and the panel
  repeats that above the list.

### `src/components/ContextPanel.tsx`

Presentational. Renders binding vs. launch as two separate blocks, a divergence
notice when `run.cwd !== thread.workdir`, and the supplied-context block. It is
rendered by `AgentPane`, which already holds the run info and the transcript, so
no state had to be lifted into `App`.

### `SessionPanel` / `App`

- `draftKey` replaces `generatedAt` for adoption: two drafts opened in the same
  millisecond are still two drafts.
- Saved `handoff-draft` records with non-empty briefing get *Open saved draft*.
- *Start new Hermes session with this text* creates a `hermes-acp` session with
  `briefing = reviewed text` and `predecessorId` inside the same thread
  (`Store::create_session` already validates that natively via
  `require_session_in_thread`), starts the agent, and hands the text to the
  composer as a prefill addressed to that `runId`.

### Async and duplicate rules

- `draftHandoff` captures the generation and drops a draft whose transcripts
  landed after the user left the thread.
- `startHandoffSession` guards synchronously with `handoffInFlight` (React
  cannot disable a button before a second click in the same tick).
- A rejected start records `handoffRetry = {threadId, sessionId, text,
  predecessorId}`. A retry reuses that session **only** while all four still
  match; the reviewed text and the lineage are part of its identity, because a
  reused record would otherwise start an agent whose durable briefing is not
  what the user just reviewed. On a mismatch the stale record is closed via
  `sessionEnd` (best effort) and a fresh one is created.
- **Reuse also requires the record to still be open.** `AgentManager::start`
  writes its run row before spawning, and settles it with
  `Store::end_agent_run` when the spawn fails — which ends the *owning session*
  in the same transaction. `Store::start_agent_run` then rejects that session
  with `SESSION_ENDED`, permanently. So an unchanged retry re-reads
  `sessionList` and reuses the record only while `endedAt === null`; otherwise
  it creates a fresh same-thread session carrying the same reviewed text and
  predecessor. No `sessionEnd` is sent for a session the native side already
  ended. The generation is captured across that extra read. An unchanged retry
  against a still-open record still creates no duplicate, and no path sends
  anything.
- `saveDraft` captures the generation and owns its writes: the session record
  and the review item are created because the user asked for them, but nothing
  is painted — no appended session, no cleared draft, no appended review item —
  if the user has switched threads before the writes land.
- The composer prefill is addressed to a `runId`, is applied once, and never
  overwrites text the user has already typed.
- A live agent in the thread blocks a handoff start, in the panel (disabled
  button plus a stated reason) and again in `App`.

## Known limits

- The bounds are enforced in the frontend, against the native limit rather than
  in place of it: `Store::create_session` still validates the briefing, and a
  user who types past 40 000 characters after composition gets that native
  validation error surfaced in the UI. Faiden does not silently trim what the
  user typed themselves.
- A retry that abandons its record leaves an ended `hermes-acp` session that
  never ran — either ended by Faiden as *superseded by an edited handoff draft*,
  or already ended by the native side with the spawn failure as its outcome.
  That is the honest trace of an attempt, not a leak.
- If the user switches threads while a handoff start is in flight, the session
  and run are still created — that is the honest outcome of an accepted native
  call — but no prefill is painted. The reviewed text is recoverable from the
  session's saved briefing, which the context panel displays.
- The `App`-level live-agent guard is defence in depth; the user-facing
  enforcement covered by tests is the disabled button and its stated reason.
- Evidence limits are reported for what the *draft* read. Faiden still cannot
  see Hermes' own context, and says so rather than inferring it.
- No test in this phase calls a model or a real Hermes. Frontend tests drive the
  in-memory `fakeApi`.
