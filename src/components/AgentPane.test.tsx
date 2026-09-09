import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { makeFakeApi, type FakeApi } from '../test/fakeApi'
import type { AgentEvent, AgentMessage, AgentRunInfo, PermissionPrompt } from '../domain/agentTypes'

let fake: FakeApi

vi.mock('../bridge', () => ({
  api: new Proxy({} as Record<string, unknown>, {
    get: (_t, key: string) => fake.api[key as keyof typeof fake.api],
  }),
  isNativeAvailable: () => true,
}))

const { AgentPane } = await import('./AgentPane')

const RUN: AgentRunInfo = {
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
  disclosure:
    'This agent runs the Hermes you installed, with your user’s authority. It is not a sandbox.',
}

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

const PROMPT: PermissionPrompt = {
  requestId: 'perm_1',
  runId: 'agr_1',
  toolCallId: 'perm-check-1',
  title: 'Run a command: rm -rf build',
  detail: '$ rm -rf build',
  options: [
    { optionId: 'allow_once', name: 'Allow once', kind: 'allow_once' },
    { optionId: 'allow_always', name: 'Allow always', kind: 'allow_always' },
    { optionId: 'deny', name: 'Deny', kind: 'reject_once' },
  ],
  createdAt: 5,
  expiresAt: 60_000,
}

const handlers = () => ({
  onStart: vi.fn(),
  onStop: vi.fn(),
  onError: vi.fn(),
})

function renderPane(runId: string | null, over: Record<string, unknown> = {}) {
  const h = handlers()
  const result = render(
    <AgentPane
      native
      runId={runId}
      archivedRun={null}
      availability={fake.agentAvailability}
      plannedCwd={{ path: '/Users/example/project', label: '/Users/example/project — the directory bound to this thread.' }}
      busy={false}
      {...h}
      {...over}
    />,
  )
  return { ...result, ...h }
}

beforeEach(() => {
  fake = makeFakeApi()
})

describe('AgentPane', () => {
  it('offers to start nothing when Hermes is not installed, and says what it looked for', () => {
    fake.agentAvailability = {
      installed: false,
      program: null,
      source: null,
      message: 'no executable named `hermes` in any of: /usr/bin, ~/.local/bin',
      disclosure: 'It is not a sandbox.',
    }
    renderPane(null)

    expect(screen.getByTestId('agent-unavailable')).toHaveTextContent('.local/bin')
    expect(screen.queryByRole('button', { name: /start hermes/i })).toBeNull()
  })

  it('names the directory and its provenance before anything is started', async () => {
    const user = userEvent.setup()
    const { onStart } = renderPane(null)

    expect(screen.getByTestId('agent-cwd')).toHaveTextContent(
      '/Users/example/project — the directory bound to this thread.',
    )
    await user.click(screen.getByRole('button', { name: /start hermes/i }))
    expect(onStart).toHaveBeenCalledTimes(1)
  })

  it('discloses the authority the agent actually runs with', async () => {
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await screen.findByTestId('agent-disclosure')
    expect(screen.getByTestId('agent-disclosure')).toHaveTextContent('not a sandbox')
  })

  it('subscribes before it snapshots, so a chunk sent during startup is not lost', async () => {
    fake.agentRuns = [RUN]
    let release = () => undefined as void
    fake.api.agentSnapshot.mockImplementationOnce(async (runId: string) => {
      await new Promise<void>((resolve) => {
        release = () => resolve()
      })
      return {
        info: RUN,
        messages: [],
        truncated: false,
        pendingPermission: null,
        nextSeq: 1,
        runId,
      }
    })

    renderPane('agr_1')
    // The listener is attached first; this event arrives while the snapshot
    // request is still outstanding.
    await waitFor(() => expect(fake.agentListeners).toBe(1))
    emit(event({ type: 'message', seq: 1, message: message({ body: 'early chunk' }) }))
    release()

    expect(await screen.findByText('early chunk')).toBeInTheDocument()
  })

  it('streams chunks into one entry through the real event callback', async () => {
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))

    emit(event({ type: 'message', seq: 1, message: message({ body: 'Hel' }) }))
    emit(
      event({ type: 'message', seq: 2, message: message({ body: 'Hello there', status: 'complete' }) }),
    )

    expect(await screen.findByText('Hello there')).toBeInTheDocument()
    expect(screen.queryByText('Hel')).toBeNull()
  })

  it('drops an event from a superseded run rather than painting it over this one', async () => {
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))

    emit(event({ type: 'message', seq: 1, epoch: 2, message: message({ body: 'stale' }) }))
    emit(event({ type: 'message', seq: 1, message: message({ body: 'current' }) }))

    expect(await screen.findByText('current')).toBeInTheDocument()
    expect(screen.queryByText('stale')).toBeNull()
  })

  it('shows a permission request in full and sends only the option that was clicked', async () => {
    const user = userEvent.setup()
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))

    emit(event({ type: 'permission', seq: 1, request: PROMPT }))

    const card = await screen.findByTestId('agent-permission')
    expect(card).toHaveTextContent('Run a command: rm -rf build')
    expect(card).toHaveTextContent('$ rm -rf build')

    await user.click(screen.getByRole('button', { name: 'Allow once' }))
    expect(fake.api.agentAnswerPermission).toHaveBeenCalledWith('agr_1', 'perm_1', 'allow_once')
  })

  it('denying sends no option at all, which is what fails closed', async () => {
    const user = userEvent.setup()
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))
    emit(event({ type: 'permission', seq: 1, request: PROMPT }))
    await screen.findByTestId('agent-permission')

    await user.click(screen.getByRole('button', { name: /^deny$/i }))
    expect(fake.api.agentAnswerPermission).toHaveBeenCalledWith('agr_1', 'perm_1', null)
  })

  it('a resolved request disappears and says how it ended', async () => {
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))
    emit(event({ type: 'permission', seq: 1, request: PROMPT }))
    await screen.findByTestId('agent-permission')

    emit(
      event({
        type: 'permissionResolved',
        seq: 2,
        requestId: 'perm_1',
        resolution: 'cancelled (no answer within Faiden’s permission deadline)',
      }),
    )

    await waitFor(() => expect(screen.queryByTestId('agent-permission')).toBeNull())
    expect(screen.getByTestId('agent-resolution')).toHaveTextContent('cancelled')
  })

  it('sends a message only on an explicit submit, and never twice', async () => {
    const user = userEvent.setup()
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))

    const box = screen.getByLabelText(/message/i)
    await user.type(box, 'hello agent')
    expect(fake.api.agentPrompt).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: /send/i }))
    expect(fake.api.agentPrompt).toHaveBeenCalledWith('agr_1', 'hello agent')
    expect(fake.api.agentPrompt).toHaveBeenCalledTimes(1)
  })

  it('cannot send while a turn is in flight', async () => {
    fake.agentRuns = [{ ...RUN, status: 'responding', promptInFlight: true }]
    renderPane('agr_1')

    await waitFor(() =>
      expect(screen.getByRole('button', { name: /send/i })).toBeDisabled(),
    )
    expect(screen.getByTestId('agent-status')).toHaveTextContent('Working on your message')
  })

  it('a finished turn is labelled without claiming the task succeeded', async () => {
    fake.agentRuns = [RUN]
    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))

    emit(
      event({
        type: 'status',
        seq: 1,
        status: 'turnComplete',
        detail: 'Turn ended (end_turn) — the agent stopped producing output for this turn.',
      }),
    )

    const status = await screen.findByTestId('agent-status')
    expect(status.textContent?.toLowerCase()).not.toContain('success')
    expect(screen.getByTestId('agent-detail')).toHaveTextContent('end_turn')
  })

  it('a still-starting session is shown as starting, not as ready', async () => {
    fake.agentRuns = [{ ...RUN, status: 'connecting', acpSessionId: null }]
    renderPane('agr_1')
    const status = await screen.findByTestId('agent-status')
    expect(status.textContent?.toLowerCase()).toContain('starting')
    expect(status.textContent?.toLowerCase()).not.toContain('ready')
  })

  it('says when the transcript it is showing is only a tail', async () => {
    fake.agentRuns = [RUN]
    fake.agentTruncated = true
    renderPane('agr_1')
    expect(await screen.findByTestId('agent-truncated')).toHaveTextContent(/older/i)
  })

  it('stops listening when the run it was watching goes away', async () => {
    fake.agentRuns = [RUN]
    const { unmount } = renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))
    unmount()
    await waitFor(() => expect(fake.agentListeners).toBe(0))
  })
})

/** Emits through the real listener, inside act so React can settle. */
function emit(e: AgentEvent) {
  act(() => fake.emitAgent(e))
}

/** Builds a well-formed event for run `agr_1` at epoch 3. */
function event(over: Record<string, unknown>): AgentEvent {
  return { runId: 'agr_1', epoch: 3, ...over } as AgentEvent
}

describe('AgentPane submit guard', () => {
  it('two submits in the same tick still send exactly one message', async () => {
    fake.agentRuns = [RUN]
    let release = () => undefined as void
    fake.api.agentPrompt.mockImplementationOnce(async () => {
      await new Promise<void>((resolve) => {
        release = () => resolve()
      })
      return {
        id: 'msg_x',
        runId: 'agr_1',
        key: 'user:1',
        role: 'user',
        turn: 1,
        body: 'hello',
        detail: null,
        status: 'sent',
        createdAt: 1,
        updatedAt: 1,
      }
    })

    renderPane('agr_1')
    await waitFor(() => expect(fake.agentListeners).toBe(1))
    const user = userEvent.setup()
    await user.type(screen.getByLabelText(/message/i), 'hello')

    // React state cannot disable the control before both handlers have run, so
    // the guard has to be synchronous.
    const form = screen.getByRole('button', { name: /send/i }).closest('form')
    if (!form) throw new Error('the composer is a form')
    await act(async () => {
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    })

    expect(fake.api.agentPrompt).toHaveBeenCalledTimes(1)
    await act(async () => release())
  })
})

describe('AgentPane context and reviewed handoff', () => {
  it('shows what Faiden supplied: the saved briefing as prepared, and only real prompts', async () => {
    fake.agentRuns = [RUN]
    fake.agentMessages = [
      message({ id: 'u1', role: 'user', body: 'Reproduce the failure.', status: 'sent' }),
      message({ id: 't1', role: 'thought', body: 'PRIVATE-REASONING' }),
    ]
    renderPane('agr_1', {
      workdir: '/Users/example/project',
      directoryContext: null,
      runSession: {
        id: 'ses_1',
        threadId: 'thr_1',
        label: 'Handoff continuation',
        kind: 'hermes-acp',
        predecessorId: 'ses_0',
        briefing: 'Goal: finish the limiter.',
        createdAt: 1,
        endedAt: null,
        outcome: null,
      },
    })

    const supplied = await screen.findByTestId('context-supplied')
    expect(supplied).toHaveTextContent('Goal: finish the limiter.')
    expect(supplied.textContent?.toLowerCase()).toContain('not sent')
    const prompts = screen.getByTestId('supplied-prompts')
    expect(prompts).toHaveTextContent('Reproduce the failure.')
    expect(prompts.textContent).not.toContain('PRIVATE-REASONING')
  })

  it('prefills reviewed handoff text into the composer and sends nothing on its own', async () => {
    fake.agentRuns = [RUN]
    const onPrefillConsumed = vi.fn()
    renderPane('agr_1', {
      prefill: { runId: 'agr_1', text: 'Reviewed handoff text.' },
      onPrefillConsumed,
    })

    const box = (await screen.findByLabelText(/message/i)) as HTMLTextAreaElement
    await waitFor(() => expect(box.value).toBe('Reviewed handoff text.'))
    expect(fake.api.agentPrompt).not.toHaveBeenCalled()
    expect(onPrefillConsumed).toHaveBeenCalledTimes(1)
    expect(screen.getByTestId('agent-prefilled').textContent?.toLowerCase()).toContain(
      'nothing has been sent',
    )
  })

  it('sends exactly the reviewed text when the user presses Send, and only then', async () => {
    fake.agentRuns = [RUN]
    renderPane('agr_1', {
      prefill: { runId: 'agr_1', text: 'Reviewed handoff text.' },
      onPrefillConsumed: vi.fn(),
    })

    const box = (await screen.findByLabelText(/message/i)) as HTMLTextAreaElement
    await waitFor(() => expect(box.value).toBe('Reviewed handoff text.'))
    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: /^send$/i }))

    await waitFor(() => expect(fake.api.agentPrompt).toHaveBeenCalledTimes(1))
    expect(fake.api.agentPrompt.mock.calls[0]).toEqual(['agr_1', 'Reviewed handoff text.'])
  })

  it('ignores a prefill addressed to a different run', async () => {
    fake.agentRuns = [RUN]
    const onPrefillConsumed = vi.fn()
    renderPane('agr_1', {
      prefill: { runId: 'agr_other', text: 'Belongs elsewhere.' },
      onPrefillConsumed,
    })

    const box = (await screen.findByLabelText(/message/i)) as HTMLTextAreaElement
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20))
    })
    expect(box.value).toBe('')
    expect(onPrefillConsumed).not.toHaveBeenCalled()
  })

  it('does not overwrite what the user has already typed with a prefill', async () => {
    fake.agentRuns = [RUN]
    const { rerender } = renderPane('agr_1')
    const box = (await screen.findByLabelText(/message/i)) as HTMLTextAreaElement
    const user = userEvent.setup()
    await user.type(box, 'my own words')

    rerender(
      <AgentPane
        native
        runId="agr_1"
        archivedRun={null}
        availability={fake.agentAvailability}
        plannedCwd={null}
        busy={false}
        prefill={{ runId: 'agr_1', text: 'Reviewed handoff text.' }}
        onPrefillConsumed={vi.fn()}
        onStart={vi.fn()}
        onStop={vi.fn()}
        onError={vi.fn()}
      />,
    )

    await act(async () => {
      await new Promise((r) => setTimeout(r, 20))
    })
    expect(box.value).toBe('my own words')
  })
})

describe('AgentPane partial transcript', () => {
  it('tells the context panel the prompt list is partial when the tail is bounded', async () => {
    fake.agentRuns = [RUN]
    fake.agentTruncated = true
    fake.agentMessages = [message({ id: 'u1', role: 'user', body: 'only the tail', status: 'sent' })]
    renderPane('agr_1')

    expect((await screen.findByTestId('supplied-partial')).textContent?.toLowerCase()).toContain(
      'not a complete list',
    )
  })
})
