// An in-memory stand-in for the native bridge, used by component tests.
// It mirrors the native contract closely enough to catch UI mistakes: opening a
// thread marks it seen but never reviewed, ids can go stale, and terminals only
// exist once something explicitly starts them.

import { vi } from 'vitest'
import type { Mock } from 'vitest'

import type { FaidenApi } from '../bridge'

import type {
  AppInfo,
  DirectoryContext,
  ExitInfo,
  HandoffDraft,
  NewSession,
  ReviewItem,
  Session,
  TerminalEvent,
  TerminalInfo,
  TerminalSnapshot,
  Thread,
} from '../domain/types'

/** Every native call, replaced by a spy. */
export type FakeApiMocks = { [K in keyof FaidenApi]: Mock }

export interface FakeApi {
  threads: Thread[]
  sessions: Session[]
  reviews: ReviewItem[]
  terminals: TerminalInfo[]
  directories: Record<string, DirectoryContext>
  pickResult: string | null
  emit(event: TerminalEvent): void
  api: FakeApiMocks
}

let counter = 0
const nextId = (p: string) => `${p}_${(counter += 1)}`

export function makeFakeApi(seed: Partial<Pick<FakeApi, 'threads' | 'sessions' | 'reviews' | 'directories'>> = {}): FakeApi {
  const state: FakeApi = {
    threads: seed.threads ?? [],
    sessions: seed.sessions ?? [],
    reviews: seed.reviews ?? [],
    terminals: [],
    directories: seed.directories ?? {},
    pickResult: null,
    emit: () => undefined,
    api: {} as FakeApiMocks,
  }

  const listeners = new Set<(e: TerminalEvent) => void>()
  state.emit = (event) => listeners.forEach((l) => l(event))

  const thread = (id: string): Thread => {
    const t = state.threads.find((x) => x.id === id)
    if (!t) throw { code: 'NOT_FOUND', message: `no thread with id ${id}`, entity: 'thread', id }
    return t
  }

  state.api = {
    appInfo: vi.fn(
      async (): Promise<AppInfo> => ({
        version: '0.1.0',
        platform: 'macos',
        dataDir: '/tmp/faiden-test',
        lifecycleNote: 'Quitting Faiden ends every terminal. There is no background service in this build.',
        retainedOutputChars: 200000,
      }),
    ),
    threadList: vi.fn(async () => [...state.threads]),
    threadCreate: vi.fn(async (title: string) => {
      const t: Thread = {
        id: nextId('thr'),
        title: title.trim(),
        notes: '',
        workdir: null,
        createdAt: 1,
        updatedAt: 1,
        seenAt: null,
        reviewedAt: null,
      }
      state.threads = [t, ...state.threads]
      return t
    }),
    threadRename: vi.fn(async (id: string, title: string) => {
      const t = thread(id)
      t.title = title.trim()
      return { ...t }
    }),
    threadSetNotes: vi.fn(async (id: string, notes: string) => {
      const t = thread(id)
      t.notes = notes
      return { ...t }
    }),
    threadSetWorkdir: vi.fn(async (id: string, path: string | null) => {
      const t = thread(id)
      t.workdir = path
      return { ...t }
    }),
    threadMarkSeen: vi.fn(async (id: string) => {
      const t = thread(id)
      t.seenAt = t.seenAt ?? 100
      state.reviews.filter((r) => r.threadId === id).forEach((r) => (r.seenAt = r.seenAt ?? 100))
      return { ...t }
    }),
    threadMarkReviewed: vi.fn(async (id: string) => {
      const t = thread(id)
      t.reviewedAt = 200
      t.seenAt = t.seenAt ?? 200
      return { ...t }
    }),
    sessionList: vi.fn(async (threadId: string) => state.sessions.filter((s) => s.threadId === threadId)),
    sessionCreate: vi.fn(async (threadId: string, s: NewSession) => {
      const created: Session = {
        id: nextId('ses'),
        threadId,
        label: s.label,
        kind: s.kind,
        predecessorId: s.predecessorId,
        briefing: s.briefing,
        createdAt: 1,
        endedAt: null,
        outcome: null,
      }
      state.sessions = [...state.sessions, created]
      return created
    }),
    sessionEnd: vi.fn(async (id: string, outcome: string) => {
      const s = state.sessions.find((x) => x.id === id)
      if (!s) throw { code: 'NOT_FOUND', message: 'no session', entity: 'session', id }
      s.endedAt = 5
      s.outcome = outcome
      return { ...s }
    }),
    handoffDraft: vi.fn(
      async (threadId: string, predecessorId: string | null): Promise<HandoffDraft> => ({
        threadId,
        predecessorSessionId: predecessorId,
        text: `# Handoff draft — ${thread(threadId).title}\nThis is a human-authored handoff draft. It does not resume an AI context.\n\n${thread(threadId).notes}`,
        disclosure: 'This is a human-authored handoff draft. It does not resume an AI context.',
        generatedAt: 9,
      }),
    ),
    reviewList: vi.fn(async (threadId: string) => state.reviews.filter((r) => r.threadId === threadId)),
    reviewAdd: vi.fn(async (threadId: string, sessionId: string | null, kind: string, summary: string) => {
      const item: ReviewItem = {
        id: nextId('rev'),
        threadId,
        sessionId,
        kind,
        summary,
        createdAt: 1,
        seenAt: null,
        reviewedAt: null,
      }
      state.reviews = [...state.reviews, item]
      return item
    }),
    reviewMarkReviewed: vi.fn(async (id: string) => {
      const r = state.reviews.find((x) => x.id === id)
      if (!r) throw { code: 'NOT_FOUND', message: 'no review item', entity: 'review_item', id }
      r.reviewedAt = 300
      r.seenAt = r.seenAt ?? 300
      return { ...r }
    }),
    environmentInspect: vi.fn(async (path: string): Promise<DirectoryContext> => {
      const known = state.directories[path]
      if (known) return known
      return {
        path,
        exists: false,
        isDirectory: false,
        provenance: 'user-selected',
        note: 'Directory chosen by you in Faiden. This is not an observed process working directory.',
        observedAt: 10,
        git: null,
      }
    }),
    environmentPickDirectory: vi.fn(async () => state.pickResult),
    terminalStart: vi.fn(async (sessionId: string, cwd: string | null, cols: number, rows: number) => {
      void cols
      void rows
      const info: TerminalInfo = {
        terminalId: nextId('term'),
        sessionId,
        epoch: state.terminals.length + 1,
        pid: 4242,
        cwd,
        commandLabel: '/bin/zsh -l',
        startedAt: 1,
        running: true,
        exit: null,
      }
      state.terminals = [...state.terminals, info]
      return info
    }),
    terminalWrite: vi.fn(async () => undefined),
    terminalResize: vi.fn(async () => undefined),
    terminalSnapshot: vi.fn(async (terminalId: string): Promise<TerminalSnapshot> => {
      const info = state.terminals.find((t) => t.terminalId === terminalId)
      if (!info) throw { code: 'NOT_FOUND', message: 'no terminal', entity: 'terminal', id: terminalId }
      return { info, data: '', nextSeq: 1, truncated: false }
    }),
    terminalList: vi.fn(async () => [...state.terminals]),
    terminalClose: vi.fn(async (terminalId: string): Promise<ExitInfo> => {
      state.terminals = state.terminals.filter((t) => t.terminalId !== terminalId)
      return { exitCode: 0, success: true, description: 'Success', at: 11 }
    }),
    onTerminalEvent: vi.fn(async (cb: (e: TerminalEvent) => void) => {
      listeners.add(cb)
      return () => listeners.delete(cb)
    }),
  }

  return state
}

export function directory(path: string, over: Partial<DirectoryContext> = {}): DirectoryContext {
  return {
    path,
    exists: true,
    isDirectory: true,
    provenance: 'user-selected',
    note: 'Directory chosen by you in Faiden and read just now. This is not an observed process working directory.',
    observedAt: 10,
    git: null,
    ...over,
  }
}
