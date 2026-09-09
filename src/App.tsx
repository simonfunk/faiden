import { useCallback, useEffect, useRef, useState } from 'react'

import { api, isNativeAvailable } from './bridge'
import { describeError, toAppError } from './domain/errors'
import type {
  AgentAvailability,
  AgentCwd,
  AgentOverview,
  AgentRunInfo,
  AgentRunRecord,
  UnseenReviewCount,
} from './domain/agentTypes'
import type {
  AppInfo,
  DirectoryContext,
  HandoffDraft,
  ReviewItem,
  Session,
  TerminalInfo,
  Thread,
} from './domain/types'
import { composeHandoff, type HandoffSource } from './domain/handoff'
import { AgentPane, type ComposerPrefill } from './components/AgentPane'
import { ContextBar } from './components/ContextBar'
import { NotesPanel } from './components/NotesPanel'
import { ReviewInbox } from './components/ReviewInbox'
import { SessionPanel } from './components/SessionPanel'
import { TerminalPane } from './components/TerminalPane'
import { ThreadHeader } from './components/ThreadHeader'
import { ThreadSidebar } from './components/ThreadSidebar'

const FALLBACK_LIFECYCLE_NOTE =
  'Terminals are owned by this application. Switching threads keeps them running; quitting Faiden ends every terminal. There is no background service in this build.'

/** How many past agent runs a draft may read. A draft is a briefing, not an archive. */
const HANDOFF_MAX_RUNS = 3

/**
 * The identity a failed handoff start may be retried under. Reusing the created
 * session is only safe while the reviewed text and the lineage are still the
 * ones it was written with; an edit makes it a different handoff.
 */
interface HandoffRetry {
  threadId: string
  sessionId: string
  text: string
  predecessorId: string | null
}

export function App() {
  const native = isNativeAvailable()

  const [appInfo, setAppInfo] = useState<AppInfo | null>(null)
  const [threads, setThreads] = useState<Thread[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [sessions, setSessions] = useState<Session[]>([])
  const [reviews, setReviews] = useState<ReviewItem[]>([])
  const [context, setContext] = useState<DirectoryContext | null>(null)
  const [terminal, setTerminal] = useState<TerminalInfo | null>(null)
  const [draft, setDraft] = useState<HandoffDraft | null>(null)
  /** Identifies which draft is in the editor; see `SessionPanel.draftKey`. */
  const [draftKey, setDraftKey] = useState(0)
  const [savedDraftLabel, setSavedDraftLabel] = useState<string | null>(null)
  const [prefill, setPrefill] = useState<ComposerPrefill | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const [availability, setAvailability] = useState<AgentAvailability | null>(null)
  const [agentOverviews, setAgentOverviews] = useState<AgentOverview[]>([])
  const [agentRun, setAgentRun] = useState<AgentRunInfo | null>(null)
  const [endedAgentRun, setEndedAgentRun] = useState<AgentRunInfo | AgentRunRecord | null>(null)
  const [plannedCwd, setPlannedCwd] = useState<AgentCwd | null>(null)
  const [unseen, setUnseen] = useState<UnseenReviewCount[]>([])

  /** Bumped on every thread switch; late responses for an older generation are
      dropped rather than painted over the thread the user is now looking at. */
  const generation = useRef(0)
  /** Issue/apply cursors for agent-state reads. See `refreshAgents`. */
  const agentReadIssued = useRef(0)
  const agentReadApplied = useRef(0)
  /** Synchronous guard: two clicks in one tick must start one session, not two. */
  const handoffInFlight = useRef(false)
  /** A session created for a handoff whose agent then failed to start. */
  const handoffRetry = useRef<HandoffRetry | null>(null)

  const selected = threads.find((t) => t.id === selectedId) ?? null

  const fail = useCallback((e: unknown) => setError(describeError(toAppError(e))), [])

  const run = useCallback(
    async (action: () => Promise<void>) => {
      setBusy(true)
      try {
        await action()
        setError(null)
      } catch (e) {
        fail(e)
      } finally {
        setBusy(false)
      }
    },
    [fail],
  )

  /**
   * Re-reads real agent state. Called on every agent event, never on a timer.
   *
   * Reads are ticketed and applied in issue order: several are legitimately in
   * flight at once (initial load, one per event, one per thread switch), and a
   * slower earlier answer must never overwrite a newer one — that would put a
   * stale badge back on a thread that has since asked for attention.
   */
  const refreshAgents = useCallback(async () => {
    const ticket = (agentReadIssued.current += 1)
    const [overview, runs, counts] = await Promise.all([
      api.agentOverview(),
      api.agentList(),
      api.reviewUnseenCounts(),
    ])
    if (ticket < agentReadApplied.current) return runs
    agentReadApplied.current = ticket
    setAgentOverviews(overview)
    setUnseen(counts)
    return runs
  }, [])

  // Initial load. In a browser preview nothing is called at all: there is no
  // native host to answer, and inventing data would be a lie.
  useEffect(() => {
    if (!native) return undefined
    let cancelled = false
    void (async () => {
      try {
        const info = await api.appInfo()
        if (!cancelled) setAppInfo(info)
        const list = await api.threadList()
        if (!cancelled) setThreads(list)
        const install = await api.agentAvailability()
        if (!cancelled) setAvailability(install)
        await refreshAgents()
      } catch (e) {
        if (!cancelled) fail(e)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [native, fail, refreshAgents])

  // Agent traffic drives the sidebar. The listener is registered once and torn
  // down on unmount, so a remount cannot leave a second one behind.
  useEffect(() => {
    if (!native) return undefined
    let disposed = false
    let unlisten: (() => void) | null = null
    void (async () => {
      const stop = await api.onAgentEvent(() => {
        if (disposed) return
        void refreshAgents().catch(() => undefined)
      })
      if (disposed) {
        stop()
        return
      }
      unlisten = stop
      // Subscribe first, then read — the same discipline the transcript uses.
      // An event that landed between the initial read and this registration
      // gets no repeat, so the state it changed is re-read once now.
      await refreshAgents().catch(() => undefined)
    })()
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [native, refreshAgents])

  // Everything that belongs to the selected thread.
  useEffect(() => {
    if (!native || selectedId === null) {
      setSessions([])
      setReviews([])
      setContext(null)
      setTerminal(null)
      setDraft(null)
      setSavedDraftLabel(null)
      setPrefill(null)
      setAgentRun(null)
      setEndedAgentRun(null)
      setPlannedCwd(null)
      return
    }

    const gen = (generation.current += 1)
    const id = selectedId
    const current = () => generation.current === gen

    void (async () => {
      try {
        setDraft(null)
        setSavedDraftLabel(null)
        setPrefill(null)
        setAgentRun(null)
        setEndedAgentRun(null)
        // Opening a thread records that a human saw it. It never marks the
        // thread, or anything in it, reviewed.
        const seen = await api.threadMarkSeen(id)
        if (!current()) return
        setThreads((prev) => prev.map((t) => (t.id === id ? seen : t)))
        // Opening the thread marked its waiting results seen, so agent state
        // has to be re-read rather than left showing a stale unread count.
        await refreshAgents()
        if (!current()) return

        const [loadedSessions, loadedReviews, liveTerminals] = await Promise.all([
          api.sessionList(id),
          api.reviewList(id),
          api.terminalList(),
        ])
        if (!current()) return

        setSessions(loadedSessions)
        setReviews(loadedReviews)
        const sessionIds = new Set(loadedSessions.map((s) => s.id))
        setTerminal(liveTerminals.find((t) => sessionIds.has(t.sessionId)) ?? null)

        const [liveAgents, planned, histories] = await Promise.all([
          api.agentList(),
          api.agentPlannedCwd(id).catch(() => null),
          Promise.all(
            loadedSessions
              .filter((session) => session.kind === 'hermes-acp')
              .map((session) => api.agentHistory(session.id)),
          ),
        ])
        if (!current()) return
        // A run belongs to this thread only through one of its sessions; a
        // stale response for a thread the user already left is discarded above.
        const threadRuns = liveAgents.filter((agent) => sessionIds.has(agent.sessionId))
        setAgentRun(threadRuns.find((agent) => agent.endedAt === null) ?? null)
        // The process list cannot survive an app restart. Fall back to durable
        // history and show its transcript read-only; nothing is reattached.
        const ended = [
          ...threadRuns.filter((agent) => agent.endedAt !== null),
          ...histories.flat().filter((agent) => agent.endedAt !== null),
        ].sort((a, b) => (b.endedAt ?? b.startedAt) - (a.endedAt ?? a.startedAt))[0]
        setEndedAgentRun(ended ?? null)
        setPlannedCwd(planned)

        if (seen.workdir === null) {
          setContext(null)
          return
        }
        const ctx = await api.environmentInspect(seen.workdir)
        if (!current()) return
        setContext(ctx)
      } catch (e) {
        if (current()) fail(e)
      }
    })()
    return undefined
  }, [native, selectedId, fail, refreshAgents])

  const replaceThread = (updated: Thread) =>
    setThreads((prev) => prev.map((t) => (t.id === updated.id ? updated : t)))

  const createThread = (title: string) =>
    run(async () => {
      const created = await api.threadCreate(title)
      setThreads((prev) => [created, ...prev])
      setSelectedId(created.id)
    })

  const renameThread = (title: string) =>
    run(async () => {
      if (!selected) return
      replaceThread(await api.threadRename(selected.id, title))
    })

  const saveNotes = (notes: string) =>
    run(async () => {
      if (!selected) return
      replaceThread(await api.threadSetNotes(selected.id, notes))
    })

  const markReviewed = () =>
    run(async () => {
      if (!selected) return
      replaceThread(await api.threadMarkReviewed(selected.id))
    })

  const pickDirectory = () =>
    run(async () => {
      if (!selected) return
      const path = await api.environmentPickDirectory()
      if (path === null) return
      replaceThread(await api.threadSetWorkdir(selected.id, path))
      setContext(await api.environmentInspect(path))
    })

  const clearDirectory = () =>
    run(async () => {
      if (!selected) return
      replaceThread(await api.threadSetWorkdir(selected.id, null))
      setContext(null)
    })

  const refreshContext = () =>
    run(async () => {
      if (!selected?.workdir) return
      setContext(await api.environmentInspect(selected.workdir))
    })

  const startShell = () =>
    run(async () => {
      if (!selected) return
      const existing = sessions.find((s) => s.kind === 'shell' && s.endedAt === null)
      let target = existing
      if (!target) {
        target = await api.sessionCreate(selected.id, {
          label: 'Shell session',
          kind: 'shell',
          predecessorId: null,
          briefing: '',
        })
        setSessions((prev) => [...prev, target as Session])
      }
      setTerminal(await api.terminalStart(target.id, selected.workdir, 80, 24))
    })

  const closeTerminal = () =>
    run(async () => {
      if (!terminal) return
      await api.terminalClose(terminal.terminalId)
      setTerminal(null)
    })

  const handleExited = useCallback(() => {
    const gen = generation.current
    void (async () => {
      try {
        const live = await api.terminalList()
        if (generation.current !== gen) return
        setTerminal((prev) =>
          prev ? (live.find((t) => t.terminalId === prev.terminalId) ?? prev) : prev,
        )
      } catch {
        // The exit banner already came from the authoritative event.
      }
    })()
  }, [])

  /**
   * Starting an agent is an explicit act. It creates (or reuses) a session of
   * kind `hermes-acp` so the run has a durable owner, then asks the native side
   * to start it. The UI never names an executable.
   */
  const startAgent = () =>
    run(async () => {
      if (!selected) return
      const existing = sessions.find((s) => s.kind === 'hermes-acp' && s.endedAt === null)
      let target = existing
      if (!target) {
        target = await api.sessionCreate(selected.id, {
          label: 'Hermes ACP session',
          kind: 'hermes-acp',
          predecessorId: null,
          briefing: '',
        })
        setSessions((prev) => [...prev, target as Session])
      }
      setAgentRun(await api.agentStart(target.id))
      await refreshAgents()
    })

  const stopAgent = () =>
    run(async () => {
      if (!agentRun) return
      setAgentRun(await api.agentStop(agentRun.runId))
      await refreshAgents()
    })

  /** Puts a draft in the editor. The key is what makes a second one replace a first. */
  const showDraft = (next: HandoffDraft, savedLabel: string | null) => {
    setDraft(next)
    setSavedDraftLabel(savedLabel)
    setDraftKey((k) => k + 1)
  }

  /**
   * Composes a draft locally: the native deterministic text, plus verbatim
   * excerpts cited from this thread's own agent runs.
   *
   * A transcript that could not be read is carried through as *unreadable*, not
   * as an empty run: those are different facts, and only one of them is true.
   * The read cap and any bounded tail travel with it for the same reason.
   */
  const draftHandoff = () =>
    run(async () => {
      if (!selected) return
      const gen = generation.current
      const threadId = selected.id
      const predecessor = sessions.length > 0 ? sessions[sessions.length - 1] : undefined
      const base = await api.handoffDraft(threadId, predecessor?.id ?? null)

      const histories = await Promise.all(
        sessions
          .filter((s) => s.kind === 'hermes-acp')
          .map(async (session) => ({
            session,
            records: await api.agentHistory(session.id).catch(() => [] as AgentRunRecord[]),
          })),
      )
      const all = histories
        .flatMap(({ session, records }) => records.map((record) => ({ session, record })))
        .sort((a, b) => a.record.startedAt - b.record.startedAt)
      const omittedRuns = Math.max(0, all.length - HANDOFF_MAX_RUNS)
      const sources: HandoffSource[] = await Promise.all(
        all.slice(omittedRuns).map(async ({ session, record }) => {
          const read = await api
            .agentTranscript(record.runId)
            .then((t) => ({ messages: t.messages, truncated: t.truncated, failed: false }))
            .catch(() => ({ messages: [], truncated: false, failed: true }))
          return {
            sessionId: session.id,
            sessionLabel: session.label,
            runId: record.runId,
            ...read,
          }
        }),
      )

      // The user may have moved on while the transcripts were being read; a
      // draft belongs to the thread it was asked for and to no other.
      if (generation.current !== gen) return
      showDraft({ ...base, text: composeHandoff(base, sources, { omittedRuns }).text }, null)
    })

  /** Reopens a saved draft for editing. Reading it sends and starts nothing. */
  const openSavedDraft = (session: Session) => {
    if (!selected) return
    setError(null)
    showDraft(
      {
        threadId: session.threadId,
        // The new session continues from the saved draft itself, so the
        // lineage a reader sees is the one they actually reviewed.
        predecessorSessionId: session.id,
        text: session.briefing,
        disclosure:
          'This is the handoff text you saved, read back from Faiden’s database. It does not resume an AI context: no model session was replayed, continued or contacted.',
        generatedAt: session.createdAt,
      },
      session.label,
    )
  }

  /**
   * Saves the reviewed text as a durable record. The record belongs to the
   * thread it was drafted for; if the user has moved on by the time the writes
   * land, nothing is painted, because the thread now on screen is not its owner
   * and must not inherit its session, its review item or its cleared draft.
   */
  const saveDraft = (text: string) =>
    run(async () => {
      if (!selected || !draft) return
      const gen = generation.current
      const threadId = selected.id
      const owned = () => generation.current === gen
      const created = await api.sessionCreate(threadId, {
        label: `Continuation ${new Date().toLocaleDateString()}`,
        kind: 'handoff-draft',
        predecessorId: draft.predecessorSessionId,
        briefing: text,
      })
      if (owned()) {
        setSessions((prev) => [...prev, created])
        setDraft(null)
        setSavedDraftLabel(null)
      }
      const item = await api.reviewAdd(
        threadId,
        created.id,
        'handoff',
        'Handoff draft saved — read it before starting the next session.',
      )
      if (owned()) setReviews((prev) => [...prev, item])
    })

  /**
   * Starts a *new* Hermes session carrying exactly the reviewed text. It is not
   * a resume and not a replay: the text becomes the new session's briefing and
   * is placed in the composer, and only Send transmits it.
   *
   * A live agent in this thread blocks the start outright, so nothing the user
   * is watching is interrupted on their behalf.
   */
  const startHandoffSession = (text: string) => {
    if (!selected || !draft) return
    if (agentRun !== null && agentRun.endedAt === null) {
      setError(
        'This thread already owns a running Hermes session. End it before starting a handoff — Faiden does not interrupt a live agent for you.',
      )
      return
    }
    // Synchronous: React cannot disable the button before a second click in
    // the same tick has already run this handler.
    if (handoffInFlight.current) return
    handoffInFlight.current = true

    const gen = generation.current
    const threadId = selected.id
    const predecessorId = draft.predecessorSessionId

    void run(async () => {
      try {
        // A record created for an earlier attempt may only be reused when it
        // still holds this exact text and lineage. Otherwise it would start an
        // agent whose durable briefing is not what the user just reviewed.
        const previous = handoffRetry.current
        const sameHandoff =
          previous !== null &&
          previous.threadId === threadId &&
          previous.text === text &&
          previous.predecessorId === predecessorId
        let targetId: string | null = null

        if (previous !== null && !sameHandoff) {
          handoffRetry.current = null
          // Best effort: a record that was never started and is now superseded
          // is closed, rather than left looking like a session you could use.
          await api
            .sessionEnd(
              previous.sessionId,
              'abandoned — never started, superseded by an edited handoff draft',
            )
            .catch(() => undefined)
        } else if (previous !== null) {
          // The record is only reusable if it is still *open*. A start that
          // fails after the native side has written its run row settles that
          // run, and `Store::end_agent_run` ends the owning session in the same
          // transaction; `start_agent_run` then rejects it with SESSION_ENDED
          // for good. Reusing it would make every retry fail forever, so the
          // store is asked what actually happened rather than assumed.
          const live = await api.sessionList(threadId).catch(() => null)
          const record = live?.find((s) => s.id === previous.sessionId) ?? null
          if (record !== null && record.endedAt === null) {
            targetId = previous.sessionId
          } else {
            // Already ended, or gone: it cannot own another run. No `sessionEnd`
            // is sent — a session ends once, and the native side ended this one.
            handoffRetry.current = null
          }
          if (live !== null && generation.current === gen) setSessions(live)
        }

        if (targetId === null) {
          const created = await api.sessionCreate(threadId, {
            label: `Handoff session ${new Date().toLocaleDateString()}`,
            kind: 'hermes-acp',
            predecessorId,
            briefing: text,
          })
          targetId = created.id
          if (generation.current === gen) setSessions((prev) => [...prev, created])
        }
        // Recorded before the start is attempted, so a rejected start leaves a
        // retry path rather than a second session on the next click.
        handoffRetry.current = { threadId, sessionId: targetId, text, predecessorId }
        const started = await api.agentStart(targetId)
        handoffRetry.current = null
        await refreshAgents()
        if (generation.current !== gen) return
        setAgentRun(started)
        setDraft(null)
        setSavedDraftLabel(null)
        setPrefill({ runId: started.runId, text })
      } finally {
        handoffInFlight.current = false
      }
    })
  }

  const clearPrefill = useCallback(() => setPrefill(null), [])

  const markItemReviewed = (id: string) =>
    run(async () => {
      const updated = await api.reviewMarkReviewed(id)
      setReviews((prev) => prev.map((r) => (r.id === id ? updated : r)))
    })

  return (
    <div className="app">
      <ThreadSidebar
        threads={threads}
        agents={agentOverviews}
        unseen={unseen}
        selectedId={selectedId}
        disabled={!native || busy}
        onSelect={(id) => {
          setError(null)
          setSelectedId(id)
        }}
        onCreate={(title) => void createThread(title)}
        onLocalError={setError}
      />

      <main className="workspace">
        {!native ? (
          <p className="banner banner--warn" role="status">
            Native connection unavailable. This is a browser preview of the Faiden interface. No
            terminal, filesystem or database access exists here, and nothing on this screen is
            simulated.
          </p>
        ) : null}

        {error !== null ? (
          <p className="banner banner--error" role="alert">
            {error}
          </p>
        ) : null}

        {selected === null ? (
          <section className="empty">
            <h1>Never lose the thread.</h1>
            <p>
              A thread is a durable topic. Start one to think out loud — no repository, ticket or
              worktree required. Attach a directory and a shell later, when you actually need them.
            </p>
            <p className="empty__note" data-testid="lifecycle-note">
              {appInfo?.lifecycleNote ?? FALLBACK_LIFECYCLE_NOTE}
            </p>
          </section>
        ) : (
          <>
            <ThreadHeader
              thread={selected}
              busy={busy}
              onRename={(title) => void renameThread(title)}
              onMarkReviewed={() => void markReviewed()}
              onLocalError={setError}
            />

            <ContextBar
              workdir={selected.workdir}
              context={context}
              busy={busy}
              onPick={() => void pickDirectory()}
              onClear={() => void clearDirectory()}
              onRefresh={() => void refreshContext()}
            />

            <div className="workspace__grid">
              <div className="workspace__column">
                <NotesPanel
                  thread={selected}
                  busy={busy}
                  onSave={(notes) => void saveNotes(notes)}
                  onLocalError={setError}
                />
                <SessionPanel
                  sessions={sessions}
                  draft={draft}
                  draftKey={draftKey}
                  savedDraftLabel={savedDraftLabel}
                  liveAgent={agentRun !== null && agentRun.endedAt === null}
                  busy={busy}
                  disabled={!native}
                  onDraft={() => void draftHandoff()}
                  onDiscardDraft={() => {
                    setDraft(null)
                    setSavedDraftLabel(null)
                  }}
                  onSaveDraft={(text) => void saveDraft(text)}
                  onOpenSaved={openSavedDraft}
                  onStartHandoff={startHandoffSession}
                />
                <ReviewInbox
                  items={reviews}
                  busy={busy}
                  onMarkReviewed={(id) => void markItemReviewed(id)}
                />
              </div>

              <div className="workspace__column workspace__column--terminal">
                <AgentPane
                  native={native}
                  runId={agentRun?.runId ?? null}
                  archivedRun={agentRun === null ? endedAgentRun : null}
                  availability={availability}
                  plannedCwd={plannedCwd}
                  workdir={selected.workdir}
                  directoryContext={context}
                  runSession={
                    agentRun === null
                      ? null
                      : (sessions.find((s) => s.id === agentRun.sessionId) ?? null)
                  }
                  prefill={prefill}
                  onPrefillConsumed={clearPrefill}
                  busy={busy}
                  onStart={() => void startAgent()}
                  onStop={() => void stopAgent()}
                  onError={setError}
                />
                <TerminalPane
                  native={native}
                  workdir={selected.workdir}
                  terminal={terminal}
                  busy={busy}
                  onStart={() => void startShell()}
                  onClose={() => void closeTerminal()}
                  onExited={handleExited}
                />
                <p className="workspace__lifecycle" data-testid="lifecycle-note">
                  {appInfo?.lifecycleNote ?? FALLBACK_LIFECYCLE_NOTE}
                </p>
              </div>
            </div>
          </>
        )}
      </main>
    </div>
  )
}
