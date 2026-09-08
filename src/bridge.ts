// The only place the frontend touches the native host.
//
// In a browser (`pnpm dev` without Tauri, or a static preview) there is no
// native host. Every call fails with an explicit NATIVE_UNAVAILABLE error and
// the UI says so. Nothing is mocked: a browser preview cannot run a process and
// must not look as though it can.

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

import { nativeUnavailableError, toAppError } from './domain/errors'
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
} from './domain/types'

/** Matches `src-tauri/src/lib.rs::TERMINAL_EVENT`. */
const TERMINAL_EVENT = 'faiden://terminal'

export function isNativeAvailable(): boolean {
  return typeof globalThis !== 'undefined' && '__TAURI_INTERNALS__' in globalThis
}

async function call<T>(command: string, action: string, args?: Record<string, unknown>): Promise<T> {
  if (!isNativeAvailable()) {
    throw nativeUnavailableError(action)
  }
  try {
    return (await invoke<T>(command, args)) as T
  } catch (raw) {
    throw toAppError(raw)
  }
}

export interface FaidenApi {
  appInfo(): Promise<AppInfo>
  threadList(): Promise<Thread[]>
  threadCreate(title: string): Promise<Thread>
  threadRename(id: string, title: string): Promise<Thread>
  threadSetNotes(id: string, notes: string): Promise<Thread>
  threadSetWorkdir(id: string, path: string | null): Promise<Thread>
  threadMarkSeen(id: string): Promise<Thread>
  threadMarkReviewed(id: string): Promise<Thread>
  sessionList(threadId: string): Promise<Session[]>
  sessionCreate(threadId: string, session: NewSession): Promise<Session>
  sessionEnd(id: string, outcome: string): Promise<Session>
  handoffDraft(threadId: string, predecessorId: string | null): Promise<HandoffDraft>
  reviewList(threadId: string): Promise<ReviewItem[]>
  reviewAdd(
    threadId: string,
    sessionId: string | null,
    kind: string,
    summary: string,
  ): Promise<ReviewItem>
  reviewMarkReviewed(id: string): Promise<ReviewItem>
  environmentInspect(path: string): Promise<DirectoryContext>
  environmentPickDirectory(): Promise<string | null>
  terminalStart(
    sessionId: string,
    cwd: string | null,
    cols: number,
    rows: number,
  ): Promise<TerminalInfo>
  terminalWrite(terminalId: string, data: string): Promise<void>
  terminalResize(terminalId: string, cols: number, rows: number): Promise<void>
  terminalSnapshot(terminalId: string): Promise<TerminalSnapshot>
  terminalList(): Promise<TerminalInfo[]>
  terminalClose(terminalId: string): Promise<ExitInfo>
  onTerminalEvent(handler: (event: TerminalEvent) => void): Promise<() => void>
}

export const api: FaidenApi = {
  appInfo: () => call('app_info', 'read application information'),

  threadList: () => call('thread_list', 'load threads'),
  threadCreate: (title) => call('thread_create', 'create a thread', { title }),
  threadRename: (id, title) => call('thread_rename', 'rename a thread', { id, title }),
  threadSetNotes: (id, notes) => call('thread_set_notes', 'save notes', { id, notes }),
  threadSetWorkdir: (id, path) => call('thread_set_workdir', 'bind a directory', { id, path }),
  threadMarkSeen: (id) => call('thread_mark_seen', 'record that a thread was seen', { id }),
  threadMarkReviewed: (id) => call('thread_mark_reviewed', 'mark a thread reviewed', { id }),

  sessionList: (threadId) => call('session_list', 'load sessions', { threadId }),
  sessionCreate: (threadId, session) =>
    call('session_create', 'create a session record', { threadId, session }),
  sessionEnd: (id, outcome) => call('session_end', 'close a session record', { id, outcome }),
  handoffDraft: (threadId, predecessorId) =>
    call('handoff_draft', 'build a handoff draft', { threadId, predecessorId }),

  reviewList: (threadId) => call('review_list', 'load the review inbox', { threadId }),
  reviewAdd: (threadId, sessionId, kind, summary) =>
    call('review_add', 'add a review item', { threadId, sessionId, kind, summary }),
  reviewMarkReviewed: (id) => call('review_mark_reviewed', 'mark an item reviewed', { id }),

  environmentInspect: (path) => call('environment_inspect', 'inspect a directory', { path }),
  environmentPickDirectory: () => call('environment_pick_directory', 'choose a directory'),

  terminalStart: (sessionId, cwd, cols, rows) =>
    call('terminal_start', 'start a terminal', { sessionId, cwd, cols, rows }),
  terminalWrite: (terminalId, data) =>
    call('terminal_write', 'write to a terminal', { terminalId, data }),
  terminalResize: (terminalId, cols, rows) =>
    call('terminal_resize', 'resize a terminal', { terminalId, cols, rows }),
  terminalSnapshot: (terminalId) =>
    call('terminal_snapshot', 'read terminal output', { terminalId }),
  terminalList: () => call('terminal_list', 'list terminals'),
  terminalClose: (terminalId) => call('terminal_close', 'close a terminal', { terminalId }),

  async onTerminalEvent(handler) {
    if (!isNativeAvailable()) {
      // Nothing will ever arrive; hand back an unsubscribe that is safe to call.
      return () => undefined
    }
    const unlisten = await listen<TerminalEvent>(TERMINAL_EVENT, (event) => handler(event.payload))
    return () => unlisten()
  },
}
