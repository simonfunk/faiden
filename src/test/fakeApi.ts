// An in-memory stand-in for the native bridge, used by component tests.
// It mirrors the native contract closely enough to catch UI mistakes: opening a
// thread marks it seen but never reviewed, ids can go stale, and terminals only
// exist once something explicitly starts them.

import { vi } from 'vitest'
import type { Mock } from 'vitest'

import type { FaidenApi } from '../bridge'
import type {
  AgentAvailability,
  AgentEvent,
  AgentMessage,
  AgentOverview,
  AgentRunInfo,
  AgentRunRecord,
  AgentSnapshot,
  PermissionPrompt,
  UnseenReviewCount,
} from '../domain/agentTypes'

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

  // --- agents ---
  agentAvailability: AgentAvailability
  agentRuns: AgentRunInfo[]
  agentMessages: AgentMessage[]
  agentTruncated: boolean
  agentPending: PermissionPrompt | null
  agentNextSeq: number
  agentOverviews: AgentOverview[]
  agentHistory: AgentRunRecord[]
  /** Per-run snapshots, so two sessions can hold distinct durable state. */
  agentSnapshots: Record<string, AgentSnapshot>
  unseenCounts: UnseenReviewCount[]
  /** How many live agent-event listeners exist, so disposal can be asserted. */
  agentListeners: number
  emitAgent(event: AgentEvent): void

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
    agentAvailability: {
      installed: true,
      program: '/Users/example/.local/bin/hermes',
      source: 'PATH entry /Users/example/.local/bin',
      message: 'Hermes found at /Users/example/.local/bin/hermes.',
      disclosure:
        'This agent runs the Hermes you installed, with your user authority. It is not a sandbox.',
    },
    agentRuns: [],
    agentMessages: [],
    agentTruncated: false,
    agentPending: null,
    agentNextSeq: 1,
    agentOverviews: [],
    agentHistory: [],
    agentSnapshots: {},
    unseenCounts: [],
    agentListeners: 0,
    emitAgent: () => undefined,
    api: {} as FakeApiMocks,
  }

  const listeners = new Set<(e: TerminalEvent) => void>()
  state.emit = (event) => listeners.forEach((l) => l(event))

  const agentListeners = new Set<(e: AgentEvent) => void>()
  state.emitAgent = (event) => agentListeners.forEach((l) => l(event))

  const agentRun = (runId: string): AgentRunInfo => {
    const run = state.agentRuns.find((r) => r.runId === runId)
    if (!run) throw { code: 'NOT_FOUND', message: `no agent run ${runId}`, entity: 'agent_run', id: runId }
    return run
  }

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

    reviewUnseenCounts: vi.fn(async () => [...state.unseenCounts]),

    agentAvailability: vi.fn(async () => state.agentAvailability),
    agentPlannedCwd: vi.fn(async (threadId: string) => ({
      path: '/Users/example/project',
      label: `/Users/example/project — the directory bound to this thread (${threadId})`,
    })),
    agentStart: vi.fn(async (sessionId: string) => {
      const run: AgentRunInfo = {
        runId: nextId('agr'),
        sessionId,
        epoch: state.agentRuns.length + 1,
        pid: 5150,
        program: state.agentAvailability.program ?? 'hermes',
        source: state.agentAvailability.source ?? 'PATH',
        cwd: '/Users/example/project',
        acpSessionId: null,
        status: 'connecting',
        detail: null,
        startedAt: 1,
        endedAt: null,
        promptInFlight: false,
        disclosure: state.agentAvailability.disclosure,
      }
      state.agentRuns = [...state.agentRuns, run]
      return run
    }),
    agentList: vi.fn(async () => [...state.agentRuns]),
    agentOverview: vi.fn(async () => [...state.agentOverviews]),
    agentSnapshot: vi.fn(async (runId: string): Promise<AgentSnapshot> => {
      const own = state.agentSnapshots[runId]
      if (own) return { ...own, messages: [...own.messages] }
      return {
        info: agentRun(runId),
        messages: [...state.agentMessages],
        truncated: state.agentTruncated,
        pendingPermission: state.agentPending,
        nextSeq: state.agentNextSeq,
      }
    }),
    agentPrompt: vi.fn(async (runId: string, text: string): Promise<AgentMessage> => {
      const sent: AgentMessage = {
        id: nextId('msg'),
        runId,
        key: 'user:1',
        role: 'user',
        turn: 1,
        body: text,
        detail: null,
        status: 'sent',
        createdAt: 1,
        updatedAt: 1,
      }
      state.agentMessages = [...state.agentMessages, sent]
      return sent
    }),
    agentAnswerPermission: vi.fn(async () => undefined),
    agentCancel: vi.fn(async (runId: string) => ({ ...agentRun(runId), status: 'cancelling' as const })),
    agentStop: vi.fn(async (runId: string) => {
      const stopped = { ...agentRun(runId), status: 'disconnected' as const, endedAt: 9 }
      state.agentRuns = state.agentRuns.map((r) => (r.runId === runId ? stopped : r))
      return stopped
    }),
    agentDiagnostics: vi.fn(async (runId: string) => ({
      runId,
      unmatchedReplies: 0,
      protocolFaults: 0,
      stderrTail: '',
    })),
    agentHistory: vi.fn(async () => [...state.agentHistory]),
    agentTranscript: vi.fn(async () => ({
      messages: [...state.agentMessages],
      truncated: state.agentTruncated,
    })),
    onAgentEvent: vi.fn(async (cb: (e: AgentEvent) => void) => {
      agentListeners.add(cb)
      state.agentListeners = agentListeners.size
      return () => {
        agentListeners.delete(cb)
        state.agentListeners = agentListeners.size
      }
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
