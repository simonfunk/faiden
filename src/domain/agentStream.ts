// Turning the native agent event stream into what a view renders.
//
// Pure, so the ordering rules that matter can be tested without React:
//
// * A view subscribes *before* it snapshots. The snapshot carries `nextSeq`,
//   and anything below it is already inside the snapshot — applying it again
//   would replay old output over new.
// * `runId` and `epoch` are checked on every event. A restarted session shares
//   nothing with the one before it, so a late event from the old run can never
//   repaint the run the user is now looking at.

import type { AgentEvent, AgentSnapshot, AgentStatus, PermissionPrompt } from './agentTypes'
import type { AgentMessage, AgentRunInfo } from './agentTypes'

export interface AgentView {
  info: AgentRunInfo | null
  messages: AgentMessage[]
  /** True when older transcript entries exist that this view does not hold. */
  truncated: boolean
  pending: PermissionPrompt | null
  /** How the last permission request ended, for display after it is gone. */
  lastResolution: string | null
  nextSeq: number
}

export const emptyAgentView: AgentView = {
  info: null,
  messages: [],
  truncated: false,
  pending: null,
  lastResolution: null,
  nextSeq: 0,
}

export function viewFromSnapshot(snapshot: AgentSnapshot): AgentView {
  return {
    info: snapshot.info,
    messages: snapshot.messages,
    truncated: snapshot.truncated,
    pending: snapshot.pendingPermission,
    lastResolution: null,
    nextSeq: snapshot.nextSeq,
  }
}

export function applyAgentEvent(view: AgentView, event: AgentEvent): AgentView {
  const info = view.info
  if (info === null) return view
  if (event.runId !== info.runId || event.epoch !== info.epoch) return view
  if (event.seq < view.nextSeq) return view

  const next: AgentView = { ...view, nextSeq: event.seq + 1 }

  switch (event.type) {
    case 'status':
      next.info = {
        ...info,
        status: event.status,
        detail: event.detail ?? info.detail,
      }
      return next

    case 'message':
      next.messages = mergeMessage(view.messages, event.message)
      return next

    case 'permission':
      next.pending = event.request
      return next

    case 'permissionResolved':
      if (view.pending?.requestId === event.requestId) next.pending = null
      next.lastResolution = event.resolution
      return next

    case 'ended':
      next.info = {
        ...info,
        endedAt: info.endedAt ?? event.seq,
        detail: event.outcome,
      }
      next.pending = null
      return next
  }
}

/** Streamed chunks share an id: they update one entry rather than piling up. */
function mergeMessage(messages: AgentMessage[], incoming: AgentMessage): AgentMessage[] {
  const index = messages.findIndex((m) => m.id === incoming.id)
  if (index === -1) return [...messages, incoming]
  const merged = [...messages]
  merged[index] = incoming
  return merged
}

const STATUS_LABELS: Record<AgentStatus, string> = {
  connecting: 'Starting — the process is running but the session does not exist yet',
  ready: 'Ready for a message',
  responding: 'Working on your message',
  awaitingPermission: 'Waiting for your permission',
  cancelling: 'Cancel sent — the agent has not reported the turn as over yet',
  turnComplete: 'Turn ended — this is not a claim that the task succeeded',
  disconnected: 'Ended — this session is over and cannot be resumed',
  error: 'The last operation failed',
}

export function statusLabel(status: AgentStatus): string {
  return STATUS_LABELS[status]
}

export type AgentAttention = 'needs-you' | 'has-result' | 'working' | 'none'

/**
 * What the sidebar should say. A pending permission outranks everything: it is
 * a real request that blocks the agent until a human answers.
 */
export function agentAttention(view: AgentView): AgentAttention {
  if (view.pending !== null) return 'needs-you'
  const info = view.info
  if (info === null) return 'none'
  switch (info.status) {
    case 'awaitingPermission':
      return 'needs-you'
    case 'turnComplete':
    case 'error':
      return 'has-result'
    case 'responding':
    case 'cancelling':
    case 'connecting':
      return 'working'
    default:
      return 'none'
  }
}

/** Whether the composer may send. Never true while a turn is in flight. */
export function isActionable(view: AgentView): boolean {
  const info = view.info
  if (info === null || info.endedAt !== null) return false
  if (info.promptInFlight) return false
  return info.status === 'ready' || info.status === 'turnComplete' || info.status === 'error'
}
