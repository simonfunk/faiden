import { useCallback, useEffect, useRef, useState } from 'react'

import { api, isNativeAvailable } from './bridge'
import { describeError, toAppError } from './domain/errors'
import type {
  AppInfo,
  DirectoryContext,
  HandoffDraft,
  ReviewItem,
  Session,
  TerminalInfo,
  Thread,
} from './domain/types'
import { ContextBar } from './components/ContextBar'
import { NotesPanel } from './components/NotesPanel'
import { ReviewInbox } from './components/ReviewInbox'
import { SessionPanel } from './components/SessionPanel'
import { TerminalPane } from './components/TerminalPane'
import { ThreadHeader } from './components/ThreadHeader'
import { ThreadSidebar } from './components/ThreadSidebar'

const FALLBACK_LIFECYCLE_NOTE =
  'Terminals are owned by this application. Switching threads keeps them running; quitting Faiden ends every terminal. There is no background service in this build.'

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
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  /** Bumped on every thread switch; late responses for an older generation are
      dropped rather than painted over the thread the user is now looking at. */
  const generation = useRef(0)

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
      } catch (e) {
        if (!cancelled) fail(e)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [native, fail])

  // Everything that belongs to the selected thread.
  useEffect(() => {
    if (!native || selectedId === null) {
      setSessions([])
      setReviews([])
      setContext(null)
      setTerminal(null)
      setDraft(null)
      return
    }

    const gen = (generation.current += 1)
    const id = selectedId
    const current = () => generation.current === gen

    void (async () => {
      try {
        setDraft(null)
        // Opening a thread records that a human saw it. It never marks the
        // thread, or anything in it, reviewed.
        const seen = await api.threadMarkSeen(id)
        if (!current()) return
        setThreads((prev) => prev.map((t) => (t.id === id ? seen : t)))

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
  }, [native, selectedId, fail])

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

  const draftHandoff = () =>
    run(async () => {
      if (!selected) return
      const predecessor = sessions.length > 0 ? sessions[sessions.length - 1] : undefined
      setDraft(await api.handoffDraft(selected.id, predecessor?.id ?? null))
    })

  const saveDraft = (text: string) =>
    run(async () => {
      if (!selected || !draft) return
      const created = await api.sessionCreate(selected.id, {
        label: `Continuation ${new Date().toLocaleDateString()}`,
        kind: 'handoff-draft',
        predecessorId: draft.predecessorSessionId,
        briefing: text,
      })
      setSessions((prev) => [...prev, created])
      setDraft(null)
      const item = await api.reviewAdd(
        selected.id,
        created.id,
        'handoff',
        'Handoff draft saved — read it before starting the next session.',
      )
      setReviews((prev) => [...prev, item])
    })

  const markItemReviewed = (id: string) =>
    run(async () => {
      const updated = await api.reviewMarkReviewed(id)
      setReviews((prev) => prev.map((r) => (r.id === id ? updated : r)))
    })

  return (
    <div className="app">
      <ThreadSidebar
        threads={threads}
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
                  busy={busy}
                  disabled={!native}
                  onDraft={() => void draftHandoff()}
                  onDiscardDraft={() => setDraft(null)}
                  onSaveDraft={(text) => void saveDraft(text)}
                />
                <ReviewInbox
                  items={reviews}
                  busy={busy}
                  onMarkReviewed={(id) => void markItemReviewed(id)}
                />
              </div>

              <div className="workspace__column workspace__column--terminal">
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
