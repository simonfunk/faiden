// Mirrors the serde shapes in src-tauri/src/{store,environment,terminal}.rs.
// Keep field names in sync with the Rust `rename_all = "camelCase"` attributes.

export interface Thread {
  id: string
  title: string
  notes: string
  workdir: string | null
  createdAt: number
  updatedAt: number
  seenAt: number | null
  reviewedAt: number | null
}

export interface Session {
  id: string
  threadId: string
  label: string
  kind: string
  predecessorId: string | null
  briefing: string
  createdAt: number
  endedAt: number | null
  outcome: string | null
}

export interface NewSession {
  label: string
  kind: string
  predecessorId: string | null
  briefing: string
}

export interface ReviewItem {
  id: string
  threadId: string
  sessionId: string | null
  kind: string
  summary: string
  createdAt: number
  seenAt: number | null
  reviewedAt: number | null
}

export interface HandoffDraft {
  threadId: string
  predecessorSessionId: string | null
  text: string
  disclosure: string
  generatedAt: number
}

export interface GitContext {
  repoRoot: string
  branch: string | null
  headShort: string | null
  detached: boolean
  unborn: boolean
  bare: boolean
  isLinkedWorktree: boolean
  dirty: boolean
  changedFiles: number
}

export interface DirectoryContext {
  path: string
  exists: boolean
  isDirectory: boolean
  provenance: string
  note: string
  observedAt: number
  git: GitContext | null
  /** Why Git could not be inspected; null means the probe completed. */
  gitUnavailable?: string | null
}

export interface ExitInfo {
  exitCode: number
  success: boolean
  description: string
  at: number
}

export interface TerminalInfo {
  terminalId: string
  sessionId: string
  epoch: number
  pid: number | null
  /** The directory the shell was *started* in, not an observed current cwd. */
  cwd: string | null
  commandLabel: string
  startedAt: number
  running: boolean
  exit: ExitInfo | null
}

export interface TerminalSnapshot {
  info: TerminalInfo
  data: string
  nextSeq: number
  truncated: boolean
}

export type TerminalEvent =
  | { type: 'output'; terminalId: string; epoch: number; seq: number; chunk: string }
  | {
      type: 'exit'
      terminalId: string
      epoch: number
      seq: number
      exitCode: number
      success: boolean
    }

export interface AppInfo {
  version: string
  platform: string
  dataDir: string
  lifecycleNote: string
  retainedOutputChars: number
}
