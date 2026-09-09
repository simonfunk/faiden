import { describe, expect, it } from 'vitest'

import {
  agentAttention,
  applyAgentEvent,
  emptyAgentView,
  isActionable,
  statusLabel,
  viewFromSnapshot,
} from './agentStream'
import type {
  AgentEvent,
  AgentMessage,
  AgentRunInfo,
  AgentSnapshot,
  PermissionPrompt,
} from './agentTypes'

const info = (over: Partial<AgentRunInfo> = {}): AgentRunInfo => ({
  runId: 'agr_1',
  sessionId: 'ses_1',
  epoch: 3,
  pid: 4242,
  program: '/Users/example/.local/bin/hermes',
  source: 'PATH entry /Users/example/.local/bin',
  cwd: '/Users/example/project',
  acpSessionId: 'sess-1',
  status: 'ready',
  detail: null,
  startedAt: 1,
  endedAt: null,
  promptInFlight: false,
  disclosure: 'It is not a sandbox.',
  ...over,
})

const message = (over: Partial<AgentMessage> = {}): AgentMessage => ({
  id: 'msg_1',
  runId: 'agr_1',
  key: 'agent:1',
  role: 'agent',
  turn: 1,
  body: 'Hello',
  detail: null,
  status: 'streaming',
  createdAt: 1,
  updatedAt: 1,
  ...over,
})

const prompt = (over: Partial<PermissionPrompt> = {}): PermissionPrompt => ({
  requestId: 'perm_1',
  runId: 'agr_1',
  toolCallId: 'perm-check-1',
  title: 'Run a command: rm -rf build',
  detail: '$ rm -rf build',
  options: [
    { optionId: 'allow_once', name: 'Allow once', kind: 'allow_once' },
    { optionId: 'deny', name: 'Deny', kind: 'reject_once' },
  ],
  createdAt: 5,
  expiresAt: 50_000,
  ...over,
})

const snapshot = (over: Partial<AgentSnapshot> = {}): AgentSnapshot => ({
  info: info(),
  messages: [],
  truncated: false,
  pendingPermission: null,
  nextSeq: 5,
  ...over,
})

const status = (over: Partial<Extract<AgentEvent, { type: 'status' }>> = {}): AgentEvent => ({
  type: 'status',
  runId: 'agr_1',
  epoch: 3,
  seq: 5,
  status: 'responding',
  detail: null,
  ...over,
})

describe('agent view', () => {
  it('starts empty rather than inventing a session', () => {
    expect(emptyAgentView.info).toBeNull()
    expect(emptyAgentView.messages).toEqual([])
    expect(emptyAgentView.pending).toBeNull()
  })

  it('an event that arrived before the snapshot is already inside it', () => {
    const view = viewFromSnapshot(snapshot())
    // The subscriber attached first, so it may hold events the snapshot covers.
    const after = applyAgentEvent(view, status({ seq: 4, status: 'error' }))
    expect(after).toBe(view)
    expect(after.info?.status).toBe('ready')
  })

  it('applies an event at the snapshot cursor and advances it', () => {
    const view = applyAgentEvent(viewFromSnapshot(snapshot()), status({ seq: 5 }))
    expect(view.info?.status).toBe('responding')
    expect(view.nextSeq).toBe(6)
  })

  it('ignores events belonging to another run or another epoch', () => {
    const view = viewFromSnapshot(snapshot())
    expect(applyAgentEvent(view, status({ runId: 'agr_other' }))).toBe(view)
    // A restarted session reuses nothing: a stale epoch must never repaint the
    // run the user is now looking at.
    expect(applyAgentEvent(view, status({ epoch: 2 }))).toBe(view)
  })

  it('ignores every event while no run is selected', () => {
    expect(applyAgentEvent(emptyAgentView, status())).toBe(emptyAgentView)
  })

  it('merges a streamed message into one entry instead of appending duplicates', () => {
    let view = viewFromSnapshot(snapshot())
    view = applyAgentEvent(view, {
      type: 'message',
      runId: 'agr_1',
      epoch: 3,
      seq: 5,
      message: message({ body: 'Hel' }),
    })
    view = applyAgentEvent(view, {
      type: 'message',
      runId: 'agr_1',
      epoch: 3,
      seq: 6,
      message: message({ body: 'Hello there', status: 'complete' }),
    })

    expect(view.messages).toHaveLength(1)
    expect(view.messages[0]?.body).toBe('Hello there')
    expect(view.messages[0]?.status).toBe('complete')
    expect(view.nextSeq).toBe(7)
  })

  it('keeps a permission request until it is resolved, and only its own', () => {
    let view = viewFromSnapshot(snapshot())
    view = applyAgentEvent(view, {
      type: 'permission',
      runId: 'agr_1',
      epoch: 3,
      seq: 5,
      request: prompt(),
    })
    expect(view.pending?.requestId).toBe('perm_1')

    // A resolution for a different request must not clear this one.
    view = applyAgentEvent(view, {
      type: 'permissionResolved',
      runId: 'agr_1',
      epoch: 3,
      seq: 6,
      requestId: 'perm_other',
      resolution: 'cancelled (timed out)',
    })
    expect(view.pending?.requestId).toBe('perm_1')

    view = applyAgentEvent(view, {
      type: 'permissionResolved',
      runId: 'agr_1',
      epoch: 3,
      seq: 7,
      requestId: 'perm_1',
      resolution: 'selected:allow_once (answered by you)',
    })
    expect(view.pending).toBeNull()
    expect(view.lastResolution).toBe('selected:allow_once (answered by you)')
  })

  it('an ended run records its outcome and stops being actionable', () => {
    let view = viewFromSnapshot(snapshot())
    view = applyAgentEvent(view, {
      type: 'ended',
      runId: 'agr_1',
      epoch: 3,
      seq: 5,
      outcome: 'the agent session ended — the agent process exited (exit status: 0)',
    })
    expect(view.info?.endedAt).not.toBeNull()
    expect(view.info?.detail).toContain('exited')
    expect(isActionable(view)).toBe(false)
  })

  it('a snapshot that dropped older entries says so', () => {
    const view = viewFromSnapshot(snapshot({ truncated: true, messages: [message()] }))
    expect(view.truncated).toBe(true)
  })
})

describe('status labels', () => {
  it('never turns a finished turn into a success claim', () => {
    const label = statusLabel('turnComplete')
    expect(label.toLowerCase()).not.toContain('success')
    expect(label.toLowerCase()).not.toContain('done')
  })

  it('describes a live but unstarted session as connecting, not ready', () => {
    expect(statusLabel('connecting').toLowerCase()).toContain('starting')
    expect(statusLabel('connecting').toLowerCase()).not.toContain('ready')
  })

  it('has a label for every status the native side can send', () => {
    for (const s of [
      'connecting',
      'ready',
      'responding',
      'awaitingPermission',
      'cancelling',
      'turnComplete',
      'disconnected',
      'error',
    ] as const) {
      expect(statusLabel(s).length).toBeGreaterThan(0)
    }
  })
})

describe('attention', () => {
  it('a pending permission is the strongest call for attention', () => {
    const view = { ...viewFromSnapshot(snapshot()), pending: prompt() }
    expect(agentAttention(view)).toBe('needs-you')
  })

  it('a working agent is not asking for anything', () => {
    const view = viewFromSnapshot(snapshot({ info: info({ status: 'responding' }) }))
    expect(agentAttention(view)).toBe('working')
  })

  it('a finished turn is a result to look at, not an approval', () => {
    const view = viewFromSnapshot(snapshot({ info: info({ status: 'turnComplete' }) }))
    expect(agentAttention(view)).toBe('has-result')
  })

  it('an error asks for attention too, but is not confused with a permission', () => {
    const view = viewFromSnapshot(snapshot({ info: info({ status: 'error' }) }))
    expect(agentAttention(view)).toBe('has-result')
  })

  it('an idle or absent agent asks for nothing', () => {
    expect(agentAttention(emptyAgentView)).toBe('none')
    expect(agentAttention(viewFromSnapshot(snapshot()))).toBe('none')
  })
})

describe('actionability', () => {
  it('a ready or finished session accepts the next message', () => {
    expect(isActionable(viewFromSnapshot(snapshot()))).toBe(true)
    expect(isActionable(viewFromSnapshot(snapshot({ info: info({ status: 'turnComplete' }) })))).toBe(
      true,
    )
  })

  it('a session that is still starting or still working does not', () => {
    expect(isActionable(viewFromSnapshot(snapshot({ info: info({ status: 'connecting' }) })))).toBe(
      false,
    )
    expect(
      isActionable(
        viewFromSnapshot(snapshot({ info: info({ status: 'responding', promptInFlight: true }) })),
      ),
    ).toBe(false)
  })
})
