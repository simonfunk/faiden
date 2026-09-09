// Mirrors the serde shapes in src-tauri/src/agent/runtime.rs and the agent
// tables in src-tauri/src/store.rs. Keep field names in sync with the Rust
// `rename_all = "camelCase"` attributes.

/** Derived from protocol facts only. A live process is never `ready` by itself. */
export type AgentStatus =
  | 'connecting'
  | 'ready'
  | 'responding'
  | 'awaitingPermission'
  | 'cancelling'
  | 'turnComplete'
  | 'disconnected'
  | 'error'

export interface AgentRunInfo {
  runId: string
  sessionId: string
  epoch: number
  pid: number | null
  program: string
  /** Where the executable was found. Provenance, not a capability claim. */
  source: string
  /** The directory the agent was *started* in, not an observed tool cwd. */
  cwd: string
  /** The agent's own session id, known only once `session/new` answered. */
  acpSessionId: string | null
  status: AgentStatus
  /** The agent's own words for the last thing that happened. */
  detail: string | null
  startedAt: number
  endedAt: number | null
  promptInFlight: boolean
  disclosure: string
}

export interface AgentMessage {
  id: string
  runId: string
  key: string
  /** `user` | `agent` | `thought` | `tool` | `system`. */
  role: string
  turn: number
  body: string
  detail: string | null
  status: string | null
  createdAt: number
  updatedAt: number
}

export interface PermissionOption {
  optionId: string
  name: string
  /** `allow_once` | `allow_always` | `reject_once` | `reject_always`. */
  kind: string
}

export interface PermissionPrompt {
  requestId: string
  runId: string
  toolCallId: string
  title: string
  detail: string
  options: PermissionOption[]
  createdAt: number
  /** When Faiden answers "not allowed" on the user's behalf. */
  expiresAt: number
}

export interface AgentSnapshot {
  info: AgentRunInfo
  messages: AgentMessage[]
  truncated: boolean
  pendingPermission: PermissionPrompt | null
  nextSeq: number
}

export interface AgentTranscript {
  messages: AgentMessage[]
  truncated: boolean
}

export interface AgentDiagnostics {
  runId: string
  unmatchedReplies: number
  protocolFaults: number
  stderrTail: string
}

export interface AgentAvailability {
  installed: boolean
  program: string | null
  source: string | null
  message: string
  disclosure: string
}

export interface AgentCwd {
  path: string
  label: string
}

/** A durable record of a past run. Never a reattached session. */
export interface AgentRunRecord {
  runId: string
  sessionId: string
  program: string
  cwd: string | null
  acpSessionId: string | null
  startedAt: number
  endedAt: number | null
  outcome: string | null
}

/**
 * Results waiting to be looked at on one thread. Independent of the thread's
 * own seen/reviewed state, which records what a human did rather than what an
 * agent produced afterwards.
 */
export interface UnseenReviewCount {
  threadId: string
  unseen: number
}

/** One line per agent run, enough for the sidebar to show real state. */
export interface AgentOverview {
  threadId: string
  sessionId: string
  runId: string
  status: AgentStatus
  awaitingPermission: boolean
  endedAt: number | null
}

export type AgentEvent =
  | {
      type: 'status'
      runId: string
      epoch: number
      seq: number
      status: AgentStatus
      detail: string | null
    }
  | { type: 'message'; runId: string; epoch: number; seq: number; message: AgentMessage }
  | { type: 'permission'; runId: string; epoch: number; seq: number; request: PermissionPrompt }
  | {
      type: 'permissionResolved'
      runId: string
      epoch: number
      seq: number
      requestId: string
      resolution: string
    }
  | { type: 'ended'; runId: string; epoch: number; seq: number; outcome: string }
