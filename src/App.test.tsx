import { act, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { directory, makeFakeApi } from './test/fakeApi'
import type { NewSession, Session, Thread } from './domain/types'
import type { AgentEvent, AgentOverview, AgentRunInfo } from './domain/agentTypes'

const holder: { fake: ReturnType<typeof makeFakeApi>; native: boolean } = {
  fake: makeFakeApi(),
  native: true,
}

vi.mock('./bridge', () => ({
  get api() {
    return holder.fake.api
  },
  isNativeAvailable: () => holder.native,
}))

// xterm needs a real canvas; the surface is exercised separately.
vi.mock('./components/XtermSurface', () => ({
  XtermSurface: ({ terminalId }: { terminalId: string }) => (
    <div data-testid="xterm-surface" data-terminal-id={terminalId} />
  ),
}))

const { App } = await import('./App')

function thread(over: Partial<Thread> = {}): Thread {
  return {
    id: 'thr_1',
    title: 'Rate limiter design',
    notes: '',
    workdir: null,
    createdAt: 1,
    updatedAt: 1,
    seenAt: null,
    reviewedAt: null,
    ...over,
  }
}

function session(over: Partial<Session> = {}): Session {
  return {
    id: 'ses_1',
    threadId: 'thr_1',
    label: 'Shell session',
    kind: 'shell',
    predecessorId: null,
    briefing: 'Goal: reproduce the flaky test.',
    createdAt: 1,
    endedAt: null,
    outcome: null,
    ...over,
  }
}

beforeEach(() => {
  holder.fake = makeFakeApi()
  holder.native = true
})

async function openApp() {
  const user = userEvent.setup()
  render(<App />)
  await screen.findByRole('navigation', { name: /threads/i })
  return user
}

describe('empty state', () => {
  it('shows nothing but an invitation — no simulated activity', async () => {
    await openApp()

    expect(await screen.findByText(/no threads yet/i)).toBeInTheDocument()
    expect(screen.queryByTestId('xterm-surface')).not.toBeInTheDocument()
    expect(holder.fake.api.terminalStart).not.toHaveBeenCalled()
    expect(holder.fake.api.sessionCreate).not.toHaveBeenCalled()
  })

  it('states the lifecycle plainly: quitting ends terminals, nothing runs in the background', async () => {
    await openApp()
    const note = await screen.findByTestId('lifecycle-note')
    expect(note.textContent?.toLowerCase()).toContain('no background service')
  })
})

describe('threads', () => {
  it('creates a thread with no repository, ticket or worktree', async () => {
    const user = await openApp()

    await user.click(screen.getByRole('button', { name: /new thread/i }))
    const input = await screen.findByRole('textbox', { name: /new thread title/i })
    await user.type(input, 'Brainstorm: retry policy{Enter}')

    await waitFor(() => expect(holder.fake.api.threadCreate).toHaveBeenCalledWith('Brainstorm: retry policy'))
    expect(await screen.findByRole('heading', { name: /brainstorm: retry policy/i })).toBeInTheDocument()
    expect(holder.fake.api.threadSetWorkdir).not.toHaveBeenCalled()
  })

  it('refuses a blank title locally instead of round-tripping', async () => {
    const user = await openApp()
    await user.click(screen.getByRole('button', { name: /new thread/i }))
    const input = await screen.findByRole('textbox', { name: /new thread title/i })
    await user.type(input, '   {Enter}')

    expect(await screen.findByRole('alert')).toHaveTextContent(/blank/i)
    expect(holder.fake.api.threadCreate).not.toHaveBeenCalled()
  })

  it('renames a thread', async () => {
    holder.fake = makeFakeApi({ threads: [thread()] })
    const user = await openApp()

    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    const title = await screen.findByRole('textbox', { name: /thread title/i })
    await user.clear(title)
    await user.type(title, 'Token bucket{Enter}')

    await waitFor(() => expect(holder.fake.api.threadRename).toHaveBeenCalledWith('thr_1', 'Token bucket'))
  })

  it('saves free-form notes without needing any environment', async () => {
    holder.fake = makeFakeApi({ threads: [thread()] })
    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    const notes = await screen.findByRole('textbox', { name: /thread notes/i })
    await user.type(notes, 'Decided: token bucket.')
    await user.click(screen.getByRole('button', { name: /save notes/i }))

    await waitFor(() =>
      expect(holder.fake.api.threadSetNotes).toHaveBeenCalledWith('thr_1', 'Decided: token bucket.'),
    )
  })
})

describe('seen versus reviewed', () => {
  it('opening a thread marks it seen and never reviewed', async () => {
    holder.fake = makeFakeApi({ threads: [thread()] })
    const user = await openApp()

    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    await waitFor(() => expect(holder.fake.api.threadMarkSeen).toHaveBeenCalledWith('thr_1'))
    expect(holder.fake.api.threadMarkReviewed).not.toHaveBeenCalled()
    expect(await screen.findByTestId('review-state')).toHaveTextContent(/not reviewed/i)
  })

  it('marks reviewed only on an explicit action', async () => {
    holder.fake = makeFakeApi({ threads: [thread()] })
    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    await user.click(await screen.findByRole('button', { name: /mark reviewed/i }))

    await waitFor(() => expect(holder.fake.api.threadMarkReviewed).toHaveBeenCalledWith('thr_1'))
    expect(await screen.findByTestId('review-state')).toHaveTextContent(/reviewed/i)
  })
})

describe('environment binding', () => {
  it('binds a directory only when the user picks one', async () => {
    holder.fake = makeFakeApi({ threads: [thread()] })
    holder.fake.pickResult = '/tmp/project'
    holder.fake.directories['/tmp/project'] = directory('/tmp/project', {
      git: {
        repoRoot: '/tmp/project',
        branch: 'main',
        headShort: 'a1b2c3d',
        detached: false,
        unborn: false,
        bare: false,
        isLinkedWorktree: false,
        dirty: false,
        changedFiles: 0,
      },
    })
    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    expect(holder.fake.api.environmentPickDirectory).not.toHaveBeenCalled()
    await user.click(await screen.findByRole('button', { name: /choose directory/i }))

    await waitFor(() =>
      expect(holder.fake.api.threadSetWorkdir).toHaveBeenCalledWith('thr_1', '/tmp/project'),
    )
    expect(await screen.findByText('main')).toBeInTheDocument()
  })
})

describe('terminals', () => {
  it('does not spawn anything until the user explicitly starts a shell', async () => {
    holder.fake = makeFakeApi({ threads: [thread()], sessions: [session()] })
    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    await screen.findByRole('button', { name: /start shell/i })
    expect(holder.fake.api.terminalStart).not.toHaveBeenCalled()
    expect(screen.queryByTestId('xterm-surface')).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /start shell/i }))
    await waitFor(() => expect(holder.fake.api.terminalStart).toHaveBeenCalledTimes(1))
    expect(await screen.findByTestId('xterm-surface')).toBeInTheDocument()
  })

  it('keeps the terminal alive when the user switches threads and back', async () => {
    holder.fake = makeFakeApi({
      threads: [thread(), thread({ id: 'thr_2', title: 'Other topic' })],
      sessions: [session()],
    })
    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /start shell/i }))
    await screen.findByTestId('xterm-surface')

    await user.click(screen.getByRole('button', { name: /other topic/i }))
    await waitFor(() => expect(screen.queryByTestId('xterm-surface')).not.toBeInTheDocument())
    expect(holder.fake.api.terminalClose).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: /rate limiter design/i }))
    expect(await screen.findByTestId('xterm-surface')).toBeInTheDocument()
    expect(holder.fake.api.terminalStart).toHaveBeenCalledTimes(1)
  })
})

describe('handoff draft', () => {
  it('produces an editable draft that says it did not resume an AI context', async () => {
    holder.fake = makeFakeApi({
      threads: [thread({ notes: 'Decided: pin the clock.' })],
      sessions: [session()],
    })
    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const draft = await screen.findByRole('textbox', { name: /handoff draft/i })
    expect((draft as HTMLTextAreaElement).value).toContain('does not resume an AI context')
    expect((draft as HTMLTextAreaElement).value).toContain('Decided: pin the clock.')
    expect(screen.getByTestId('handoff-disclosure').textContent?.toLowerCase()).toContain(
      'does not resume',
    )

    await user.click(screen.getByRole('button', { name: /save as session record/i }))
    await waitFor(() => expect(holder.fake.api.sessionCreate).toHaveBeenCalled())
    const firstCall = holder.fake.api.sessionCreate.mock.calls[0] as [string, NewSession]
    expect(firstCall[0]).toBe('thr_1')
    expect(firstCall[1].predecessorId).toBe('ses_1')
    expect(firstCall[1].kind).toBe('handoff-draft')
  })
})

describe('asynchronous safety', () => {
  it('discards a slow response for a thread the user already left', async () => {
    holder.fake = makeFakeApi({
      threads: [thread(), thread({ id: 'thr_2', title: 'Other topic' })],
      sessions: [session(), session({ id: 'ses_2', threadId: 'thr_2', label: 'Second thread session' })],
    })

    const slow: { release: (() => void) | null } = { release: null }
    const realSessionList = holder.fake.api.sessionList.getMockImplementation() as (
      threadId: string,
    ) => Promise<Session[]>
    holder.fake.api.sessionList.mockImplementation(async (threadId: string) => {
      if (threadId === 'thr_1') {
        await new Promise<void>((resolve) => {
          slow.release = resolve
        })
      }
      return realSessionList(threadId)
    })

    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /other topic/i }))
    await screen.findByText(/second thread session/i)

    slow.release?.()

    await new Promise((r) => setTimeout(r, 20))
    const panel = screen.getByRole('region', { name: /sessions/i })
    expect(within(panel).queryByText(/^shell session$/i)).not.toBeInTheDocument()
    expect(within(panel).getByText(/second thread session/i)).toBeInTheDocument()
  })

  it('shows a failing native call instead of silently doing nothing', async () => {
    holder.fake = makeFakeApi({ threads: [thread()] })
    holder.fake.api.threadSetNotes.mockRejectedValue({
      code: 'VALIDATION',
      message: 'must be at most 20000 characters',
      field: 'notes',
    })
    const user = await openApp()
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.type(await screen.findByRole('textbox', { name: /thread notes/i }), 'x')
    await user.click(screen.getByRole('button', { name: /save notes/i }))

    expect(await screen.findByRole('alert')).toHaveTextContent(/at most 20000 characters/i)
  })
})

describe('browser preview', () => {
  it('disables native actions and says why, rather than mocking a terminal', async () => {
    holder.native = false
    holder.fake = makeFakeApi()
    render(<App />)

    const banner = await screen.findByRole('status')
    expect(banner.textContent?.toLowerCase()).toContain('native connection unavailable')
    expect(screen.getByRole('button', { name: /new thread/i })).toBeDisabled()
    expect(screen.queryByTestId('xterm-surface')).not.toBeInTheDocument()
    expect(holder.fake.api.threadList).not.toHaveBeenCalled()
  })
})

describe('Hermes agent integration', () => {
  it('starts an agent only on an explicit click, and binds it to a session it creates', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread({ workdir: '/tmp/project' })]
    holder.fake.directories['/tmp/project'] = directory('/tmp/project')
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await screen.findByTestId('agent-cwd')

    expect(holder.fake.api.agentStart).not.toHaveBeenCalled()
    await user.click(screen.getByRole('button', { name: /start hermes session/i }))

    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(1))
    const created = holder.fake.api.sessionCreate.mock.calls.at(-1)?.[1] as NewSession
    expect(created.kind).toBe('hermes-acp')
    expect(holder.fake.api.agentStart).toHaveBeenCalledWith(holder.fake.sessions.at(-1)?.id)
  })

  it('shows a thread that is waiting for permission as needing attention', async () => {
    holder.fake.threads = [thread(), thread({ id: 'thr_2', title: 'Migration plan' })]
    holder.fake.sessions = [session({ id: 'ses_a', threadId: 'thr_2', kind: 'hermes-acp' })]
    holder.fake.agentOverviews = [
      {
        threadId: 'thr_2',
        sessionId: 'ses_a',
        runId: 'agr_1',
        status: 'awaitingPermission',
        awaitingPermission: true,
        endedAt: null,
      },
    ]
    render(<App />)

    const item = await screen.findByRole('button', { name: /migration plan/i })
    await waitFor(() => expect(within(item).getByTestId('thread-agent')).toHaveTextContent(/needs you/i))
    // The other thread makes no such claim.
    const quiet = screen.getByRole('button', { name: /rate limiter design/i })
    expect(within(quiet).queryByTestId('thread-agent')).toBeNull()
  })

  it('a working agent is shown as working, never as needing an answer', async () => {
    holder.fake.threads = [thread()]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.agentOverviews = [
      {
        threadId: 'thr_1',
        sessionId: 'ses_a',
        runId: 'agr_1',
        status: 'responding',
        awaitingPermission: false,
        endedAt: null,
      },
    ]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() => expect(within(item).getByTestId('thread-agent')).toHaveTextContent(/working/i))
    const badge = within(item).getByTestId('thread-agent')
    expect(badge.textContent?.toLowerCase()).not.toContain('needs you')
  })

  it('opening a thread with a waiting agent marks it seen but never reviewed', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread()]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.agentOverviews = [
      {
        threadId: 'thr_1',
        sessionId: 'ses_a',
        runId: 'agr_1',
        status: 'awaitingPermission',
        awaitingPermission: true,
        endedAt: null,
      },
    ]
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    await waitFor(() => expect(holder.fake.api.threadMarkSeen).toHaveBeenCalledWith('thr_1'))
    expect(holder.fake.api.threadMarkReviewed).not.toHaveBeenCalled()
    expect(holder.fake.threads[0]?.reviewedAt).toBeNull()
  })

  it('switching threads never applies one thread’s agent to another', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread(), thread({ id: 'thr_2', title: 'Migration plan' })]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.agentRuns = [
      {
        runId: 'agr_1',
        sessionId: 'ses_a',
        epoch: 1,
        pid: 1,
        program: 'hermes',
        source: 'PATH',
        cwd: '/tmp/project',
        acpSessionId: 'sess-1',
        status: 'ready',
        detail: null,
        startedAt: 1,
        endedAt: null,
        promptInFlight: false,
        disclosure: 'It is not a sandbox.',
      },
    ]
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await screen.findByTestId('agent-disclosure')

    await user.click(screen.getByRole('button', { name: /migration plan/i }))
    await waitFor(() => expect(screen.queryByTestId('agent-disclosure')).toBeNull())
    // The other thread's agent is still running; switching views did not end it.
    expect(holder.fake.api.agentStop).not.toHaveBeenCalled()
  })
})

describe('unread results', () => {
  it('a thread already seen and reviewed still shows results it has not shown you', async () => {
    holder.fake.threads = [thread({ seenAt: 100, reviewedAt: 200 })]
    holder.fake.unseenCounts = [{ threadId: 'thr_1', unseen: 2 }]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() => expect(within(item).getByTestId('thread-unseen')).toHaveTextContent('2'))
    // The human acts already recorded are not rewritten by an agent.
    expect(within(item)).toBeDefined()
    expect(holder.fake.threads[0]?.reviewedAt).toBe(200)
  })

  it('shows no unread badge for a thread with nothing waiting', async () => {
    holder.fake.threads = [thread({ seenAt: 100 })]
    holder.fake.unseenCounts = []
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    expect(within(item).queryByTestId('thread-unseen')).toBeNull()
  })

  it('opening a thread clears its unread count without marking anything reviewed', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread({ seenAt: 100, reviewedAt: 200 })]
    holder.fake.unseenCounts = [{ threadId: 'thr_1', unseen: 1 }]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() => expect(within(item).getByTestId('thread-unseen')).toBeInTheDocument())

    holder.fake.unseenCounts = []
    await user.click(item)
    await waitFor(() => expect(screen.queryByTestId('thread-unseen')).toBeNull())
    expect(holder.fake.api.threadMarkReviewed).not.toHaveBeenCalled()
  })
})

describe('sidebar attention correctness', () => {
  function overview(over: Partial<AgentOverview> = {}): AgentOverview {
    return {
      threadId: 'thr_1',
      sessionId: 'ses_a',
      runId: 'agr_1',
      status: 'ready',
      awaitingPermission: false,
      endedAt: null,
      ...over,
    }
  }

  function deferred<T>() {
    let resolve: (value: T) => void = () => undefined
    const promise = new Promise<T>((next) => {
      resolve = next
    })
    return { promise, resolve }
  }

  it('reconciles an event that lands between the first overview and the listener', async () => {
    // Subscribe-before-snapshot, at the App level: an event arriving in the gap
    // has no second chance, so the sidebar must re-read once it is listening.
    const initial = deferred<AgentOverview[]>()
    const registered = deferred<void>()
    const realOnAgentEvent = holder.fake.api.onAgentEvent.getMockImplementation()
    if (!realOnAgentEvent) throw new Error('the fake API must provide onAgentEvent')

    holder.fake.threads = [thread()]
    holder.fake.agentOverviews = [overview()]
    holder.fake.api.agentOverview.mockImplementationOnce(async () => initial.promise)
    holder.fake.api.onAgentEvent.mockImplementation(async (cb: (e: AgentEvent) => void) => {
      await registered.promise
      return realOnAgentEvent(cb)
    })

    render(<App />)
    await waitFor(() => expect(holder.fake.api.agentOverview).toHaveBeenCalledTimes(1))

    // Native state moves while App is neither subscribed nor holding a later read.
    holder.fake.agentOverviews = [overview({ status: 'awaitingPermission', awaitingPermission: true })]
    holder.fake.emitAgent({
      type: 'status',
      runId: 'agr_1',
      epoch: 1,
      seq: 1,
      status: 'awaitingPermission',
      detail: null,
    })
    initial.resolve([overview()])
    registered.resolve()
    await waitFor(() => expect(holder.fake.agentListeners).toBe(1))

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() =>
      expect(within(item).getByTestId('thread-agent')).toHaveTextContent('Needs you'),
    )
  })

  it('a slower earlier overview never overwrites a newer one', async () => {
    holder.fake.threads = [thread()]
    holder.fake.agentOverviews = [overview()]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() => expect(within(item).getByTestId('thread-agent')).toBeInTheDocument())
    await waitFor(() => expect(holder.fake.agentListeners).toBe(1))

    const older = deferred<AgentOverview[]>()
    const newer = deferred<AgentOverview[]>()
    holder.fake.api.agentOverview
      .mockImplementationOnce(async () => older.promise)
      .mockImplementationOnce(async () => newer.promise)

    const tick = (seq: number) =>
      holder.fake.emitAgent({
        type: 'status',
        runId: 'agr_1',
        epoch: 1,
        seq,
        status: 'awaitingPermission',
        detail: null,
      })
    tick(1)
    tick(2)
    await waitFor(() => expect(holder.fake.api.agentOverview).toHaveBeenCalledTimes(4))

    newer.resolve([overview({ status: 'awaitingPermission', awaitingPermission: true })])
    await waitFor(() =>
      expect(within(item).getByTestId('thread-agent')).toHaveTextContent('Needs you'),
    )

    older.resolve([overview()])
    await new Promise((r) => setTimeout(r, 20))
    expect(within(item).getByTestId('thread-agent')).toHaveTextContent('Needs you')
  })

  it('an ended run that produced a result keeps saying so until the thread is reviewed', async () => {
    holder.fake.threads = [thread({ id: 'thr_1', title: 'Rate limiter design' })]
    holder.fake.agentOverviews = [overview({ status: 'error', endedAt: 123 })]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() =>
      expect(within(item).getByTestId('thread-agent')).toHaveTextContent('Has a result'),
    )
  })

  it('a turn that ended normally is still a result to read', async () => {
    holder.fake.threads = [thread()]
    holder.fake.agentOverviews = [overview({ status: 'turnComplete', endedAt: 99 })]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() =>
      expect(within(item).getByTestId('thread-agent')).toHaveTextContent('Has a result'),
    )
  })

  it('an explicitly reviewed thread stops advertising an old ended result', async () => {
    holder.fake.threads = [thread({ reviewedAt: 500 })]
    holder.fake.agentOverviews = [overview({ status: 'error', endedAt: 123 })]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() => expect(holder.fake.agentListeners).toBe(1))
    expect(within(item).queryByTestId('thread-agent')).toBeNull()
  })

  it('a session that merely disconnected makes no result claim', async () => {
    holder.fake.threads = [thread()]
    holder.fake.agentOverviews = [overview({ status: 'disconnected', endedAt: 123 })]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() => expect(holder.fake.agentListeners).toBe(1))
    expect(within(item).queryByTestId('thread-agent')).toBeNull()
  })

  it('a live run outranks review state: a reviewed thread still shows a pending approval', async () => {
    holder.fake.threads = [thread({ reviewedAt: 500 })]
    holder.fake.agentOverviews = [overview({ status: 'awaitingPermission', awaitingPermission: true })]
    render(<App />)

    const item = await screen.findByRole('button', { name: /rate limiter design/i })
    await waitFor(() =>
      expect(within(item).getByTestId('thread-agent')).toHaveTextContent('Needs you'),
    )
  })
})

describe('two threads, switching away and back', () => {
  const runFor = (over: Partial<AgentRunInfo>): AgentRunInfo => ({
    runId: 'agr_a',
    sessionId: 'ses_a',
    epoch: 1,
    pid: 10,
    program: 'hermes',
    source: 'PATH',
    cwd: '/tmp/a',
    acpSessionId: 'sess-a',
    status: 'ready',
    detail: null,
    startedAt: 1,
    endedAt: null,
    promptInFlight: false,
    disclosure: 'It is not a sandbox.',
    ...over,
  })

  it('keeps each thread’s own agent state while unselected and restores it on return', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [
      thread({ id: 'thr_a', title: 'Thread A' }),
      thread({ id: 'thr_b', title: 'Thread B' }),
    ]
    holder.fake.sessions = [
      session({ id: 'ses_a', threadId: 'thr_a', kind: 'hermes-acp' }),
      session({ id: 'ses_b', threadId: 'thr_b', kind: 'hermes-acp' }),
    ]
    holder.fake.agentRuns = [
      runFor({}),
      runFor({ runId: 'agr_b', sessionId: 'ses_b', epoch: 2, pid: 11, acpSessionId: 'sess-b' }),
    ]
    holder.fake.agentSnapshots = {
      agr_a: {
        info: runFor({}),
        messages: [
          {
            id: 'msg_a',
            runId: 'agr_a',
            key: 'agent:1',
            role: 'agent',
            turn: 1,
            body: 'ALPHA transcript',
            detail: null,
            status: 'complete',
            createdAt: 1,
            updatedAt: 1,
          },
        ],
        truncated: false,
        pendingPermission: null,
        nextSeq: 1,
      },
      agr_b: {
        info: runFor({ runId: 'agr_b', sessionId: 'ses_b', epoch: 2, acpSessionId: 'sess-b' }),
        messages: [
          {
            id: 'msg_b',
            runId: 'agr_b',
            key: 'agent:1',
            role: 'agent',
            turn: 1,
            body: 'BETA transcript',
            detail: null,
            status: 'complete',
            createdAt: 1,
            updatedAt: 1,
          },
        ],
        truncated: false,
        pendingPermission: null,
        nextSeq: 1,
      },
    }

    render(<App />)
    await user.click(await screen.findByRole('button', { name: /thread a/i }))
    expect(await screen.findByText('ALPHA transcript')).toBeInTheDocument()

    // Switch away. Thread A's agent keeps running and asks for permission.
    await user.click(screen.getByRole('button', { name: /thread b/i }))
    expect(await screen.findByText('BETA transcript')).toBeInTheDocument()
    expect(screen.queryByText('ALPHA transcript')).toBeNull()
    expect(holder.fake.api.agentStop).not.toHaveBeenCalled()

    holder.fake.agentOverviews = [
      {
        threadId: 'thr_a',
        sessionId: 'ses_a',
        runId: 'agr_a',
        status: 'awaitingPermission',
        awaitingPermission: true,
        endedAt: null,
      },
    ]
    const pending = {
      requestId: 'perm_a',
      runId: 'agr_a',
      toolCallId: 'call-1',
      title: 'Run a command: rm -rf build',
      detail: '$ rm -rf build',
      options: [{ optionId: 'allow_once', name: 'Allow once', kind: 'allow_once' }],
      createdAt: 5,
      expiresAt: 60_000,
    }
    const snapshotA = holder.fake.agentSnapshots.agr_a
    if (snapshotA) snapshotA.pendingPermission = pending
    holder.fake.emitAgent({
      type: 'permission',
      runId: 'agr_a',
      epoch: 1,
      seq: 1,
      request: pending,
    })

    // The unselected thread's badge changes; the selected one is untouched.
    const itemA = screen.getByRole('button', { name: /thread a/i })
    await waitFor(() =>
      expect(within(itemA).getByTestId('thread-agent')).toHaveTextContent('Needs you'),
    )
    expect(screen.queryByTestId('agent-permission')).toBeNull()

    // Switching back restores thread A's durable state, with no B content.
    await user.click(itemA)
    expect(await screen.findByText('ALPHA transcript')).toBeInTheDocument()
    expect(await screen.findByTestId('agent-permission')).toHaveTextContent('rm -rf build')
    expect(screen.queryByText('BETA transcript')).toBeNull()
  })
})

describe('ended Hermes history', () => {
  it('opens a durable ended result read-only, keeps it isolated by thread, and restores it after remount', async () => {
    const ended = {
      runId: 'agr_ended', sessionId: 'ses_a', program: 'hermes', cwd: '/tmp/a',
      acpSessionId: 'sess-a', startedAt: 1, endedAt: 2, outcome: 'agent reported an error',
    }
    holder.fake.threads = [
      thread({ id: 'thr_a', title: 'Ended result' }),
      thread({ id: 'thr_b', title: 'Other thread' }),
    ]
    holder.fake.sessions = [
      session({ id: 'ses_a', threadId: 'thr_a', kind: 'hermes-acp' }),
      session({ id: 'ses_b', threadId: 'thr_b', kind: 'hermes-acp' }),
    ]
    holder.fake.api.agentHistory.mockImplementation(async (sessionId: string) =>
      sessionId === 'ses_a' ? [ended] : [],
    )
    holder.fake.api.agentTranscript.mockImplementation(async (runId: string) => ({
      messages: runId === 'agr_ended' ? [{
        id: 'msg_ended', runId, key: 'agent:1', role: 'agent', turn: 1,
        body: 'DURABLE ENDED RESULT', detail: null, status: 'complete', createdAt: 1, updatedAt: 1,
      }] : [],
      truncated: false,
    }))
    const user = userEvent.setup()
    const first = render(<App />)

    await user.click(await screen.findByRole('button', { name: /ended result/i }))
    expect(await screen.findByText('DURABLE ENDED RESULT')).toBeInTheDocument()
    expect(screen.queryByLabelText(/message to hermes/i)).toBeNull()
    expect(screen.queryByTestId('agent-permission')).toBeNull()
    expect(screen.getByRole('button', { name: /start hermes session/i })).toBeEnabled()

    await user.click(screen.getByRole('button', { name: /other thread/i }))
    await waitFor(() => expect(screen.queryByText('DURABLE ENDED RESULT')).toBeNull())
    first.unmount()

    render(<App />)
    await user.click(await screen.findByRole('button', { name: /ended result/i }))
    expect(await screen.findByText('DURABLE ENDED RESULT')).toBeInTheDocument()
    expect(holder.fake.api.agentHistory).toHaveBeenCalledWith('ses_a')
    expect(holder.fake.api.agentTranscript).toHaveBeenCalledWith('agr_ended')
  })
})

/** A live run bound to `ses_a`, the hermes-acp session used by the tests below. */
function liveRun(over: Partial<AgentRunInfo> = {}): AgentRunInfo {
  return {
    runId: 'agr_1',
    sessionId: 'ses_a',
    epoch: 1,
    pid: 1,
    program: '/opt/hermes',
    source: 'PATH entry /opt',
    cwd: '/tmp/project',
    acpSessionId: 'sess-1',
    status: 'ready',
    detail: null,
    startedAt: 1,
    endedAt: null,
    promptInFlight: false,
    disclosure: 'It is not a sandbox.',
    ...over,
  }
}

describe('context visibility', () => {
  it('tells the thread binding apart from where a live agent was actually launched', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread({ workdir: '/tmp/other' })]
    holder.fake.directories['/tmp/other'] = directory('/tmp/other')
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.agentRuns = [liveRun({ cwd: '/tmp/project' })]
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    const launch = await screen.findByTestId('context-launch')
    expect(launch).toHaveTextContent('/tmp/project')
    expect(launch).toHaveTextContent('/opt/hermes')
    const divergence = screen.getByTestId('context-divergence')
    expect(divergence).toHaveTextContent('/tmp/other')
    expect(divergence.textContent?.toLowerCase()).toContain('was not moved')
  })

  it('shows the briefing as prepared and a failed prompt as not sent', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread()]
    holder.fake.sessions = [
      session({ id: 'ses_a', kind: 'hermes-acp', briefing: 'Goal: land the limiter.' }),
    ]
    holder.fake.agentRuns = [liveRun()]
    holder.fake.agentMessages = [
      {
        id: 'p1', runId: 'agr_1', key: 'user:1', role: 'user', turn: 1,
        body: 'first prompt', detail: null, status: 'sent', createdAt: 1, updatedAt: 1,
      },
      {
        id: 'p2', runId: 'agr_1', key: 'user:2', role: 'user', turn: 2,
        body: 'second prompt', detail: null, status: 'not sent: pipe closed', createdAt: 2, updatedAt: 2,
      },
      {
        id: 't1', runId: 'agr_1', key: 'thought:1', role: 'thought', turn: 2,
        body: 'PRIVATE-REASONING', detail: null, status: null, createdAt: 3, updatedAt: 3,
      },
    ]
    render(<App />)

    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))

    const supplied = await screen.findByTestId('context-supplied')
    expect(supplied).toHaveTextContent('Goal: land the limiter.')
    expect(supplied.textContent?.toLowerCase()).toContain('not sent')
    const prompts = screen.getByTestId('supplied-prompts')
    expect(prompts).toHaveTextContent('first prompt')
    expect(prompts.textContent).toContain('sent to Hermes')
    expect(prompts.textContent).toContain('pipe closed')
    expect(prompts.textContent).not.toContain('PRIVATE-REASONING')
  })
})

describe('reviewed handoff into a new session', () => {
  function seedThreadWithHistory() {
    holder.fake.threads = [thread({ notes: 'Decided: pin the clock.' })]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.agentHistory = [
      {
        runId: 'agr_old', sessionId: 'ses_a', program: '/opt/hermes', cwd: '/tmp/project',
        acpSessionId: 'sess-0', startedAt: 1, endedAt: 2, outcome: 'ended by you',
      },
    ]
    holder.fake.agentMessages = [
      {
        id: 'm_pub', runId: 'agr_old', key: 'agent:1', role: 'agent', turn: 1,
        body: 'The clock boundary is the cause.', detail: null, status: 'complete',
        createdAt: 5, updatedAt: 5,
      },
      {
        id: 'm_think', runId: 'agr_old', key: 'thought:1', role: 'thought', turn: 1,
        body: 'PRIVATE-REASONING', detail: null, status: null, createdAt: 6, updatedAt: 6,
      },
      {
        id: 'm_tool', runId: 'agr_old', key: 'tool:1', role: 'tool', turn: 1,
        body: 'SECRET-TOOL-OUTPUT', detail: null, status: 'complete', createdAt: 7, updatedAt: 7,
      },
    ]
  }

  it('cites labelled excerpts and quotes no private reasoning or tool output', async () => {
    const user = userEvent.setup()
    seedThreadWithHistory()
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const box = (await screen.findByRole('textbox', { name: /handoff draft/i })) as HTMLTextAreaElement
    expect(box.value).toContain('The clock boundary is the cause.')
    expect(box.value).toContain('m_pub')
    expect(box.value).toContain('agr_old')
    expect(box.value).not.toContain('PRIVATE-REASONING')
    expect(box.value).not.toContain('SECRET-TOOL-OUTPUT')
    expect(box.value).toContain('never quoted')
  })

  it('starts a new session with exactly the edited text and sends nothing', async () => {
    const user = userEvent.setup()
    seedThreadWithHistory()
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const box = await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.clear(box)
    await user.type(box, 'Carry this forward.')
    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))

    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(1))
    const created = holder.fake.api.sessionCreate.mock.calls.at(-1) as [string, NewSession]
    expect(created[0]).toBe('thr_1')
    expect(created[1].kind).toBe('hermes-acp')
    expect(created[1].briefing).toBe('Carry this forward.')
    // Durable lineage inside the same thread, validated natively on create.
    expect(created[1].predecessorId).toBe('ses_a')

    const composer = (await screen.findByLabelText(/message to hermes/i)) as HTMLTextAreaElement
    await waitFor(() => expect(composer.value).toBe('Carry this forward.'))
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
    expect(screen.getByTestId('agent-prefilled').textContent?.toLowerCase()).toContain(
      'new session',
    )
  })

  it('two clicks in one tick start exactly one session', async () => {
    const user = userEvent.setup()
    seedThreadWithHistory()
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))
    await screen.findByRole('textbox', { name: /handoff draft/i })

    const start = screen.getByRole('button', { name: /start new hermes session/i })
    await act(async () => {
      start.click()
      start.click()
    })

    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(1))
    expect(
      holder.fake.api.sessionCreate.mock.calls.filter(
        (c) => (c[1] as NewSession).kind === 'hermes-acp',
      ),
    ).toHaveLength(1)
  })

  it('refuses to start while this thread still owns a live agent', async () => {
    const user = userEvent.setup()
    seedThreadWithHistory()
    holder.fake.agentRuns = [liveRun()]
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))
    await screen.findByRole('textbox', { name: /handoff draft/i })

    const start = screen.getByRole('button', { name: /start new hermes session/i })
    expect(start).toBeDisabled()
    expect(screen.getByTestId('handoff-blocked').textContent?.toLowerCase()).toContain('end that')
    expect(holder.fake.api.agentStart).not.toHaveBeenCalled()
  })

  it('a rejected start keeps the draft and retries into the same session', async () => {
    const user = userEvent.setup()
    seedThreadWithHistory()
    holder.fake.api.agentStart.mockRejectedValueOnce({
      code: 'CONFLICT',
      message: 'hermes could not be started',
    })
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))
    await screen.findByRole('textbox', { name: /handoff draft/i })

    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/could not be started/i)
    // The reviewed text is still on screen, so nothing has to be rewritten.
    const box = (await screen.findByRole('textbox', { name: /handoff draft/i })) as HTMLTextAreaElement
    expect(box.value.length).toBeGreaterThan(0)
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(2))
    const acpSessions = holder.fake.api.sessionCreate.mock.calls.filter(
      (c) => (c[1] as NewSession).kind === 'hermes-acp',
    )
    expect(acpSessions).toHaveLength(1)
    const [firstStart, secondStart] = holder.fake.api.agentStart.mock.calls
    expect(firstStart).toEqual(secondStart)
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })

  it('a slow start that lands after a thread switch never touches the other thread', async () => {
    const user = userEvent.setup()
    seedThreadWithHistory()
    holder.fake.threads = [
      thread({ notes: 'Decided: pin the clock.' }),
      thread({ id: 'thr_2', title: 'Migration plan' }),
    ]
    const slow: { release: (() => void) | null } = { release: null }
    const realStart = holder.fake.api.agentStart.getMockImplementation() as (
      id: string,
    ) => Promise<AgentRunInfo>
    holder.fake.api.agentStart.mockImplementation(async (id: string) => {
      await new Promise<void>((resolve) => {
        slow.release = resolve
      })
      return realStart(id)
    })

    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))
    await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))

    await user.click(await screen.findByRole('button', { name: /migration plan/i }))
    await waitFor(() => expect(screen.queryByTestId('agent-disclosure')).toBeNull())

    await act(async () => {
      slow.release?.()
      await new Promise((r) => setTimeout(r, 20))
    })

    // The other thread gains no run, no composer text and no draft.
    expect(screen.queryByTestId('agent-prefilled')).toBeNull()
    expect(screen.queryByRole('textbox', { name: /handoff draft/i })).toBeNull()
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })

  it('reopens a saved draft with its exact text after a thread switch and a remount', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread(), thread({ id: 'thr_2', title: 'Migration plan' })]
    holder.fake.sessions = [
      session({ id: 'ses_a', kind: 'hermes-acp' }),
      session({
        id: 'ses_saved',
        kind: 'handoff-draft',
        label: 'Continuation 1 Jan',
        predecessorId: 'ses_a',
        briefing: 'Reviewed text kept verbatim.',
      }),
    ]
    const { unmount } = render(<App />)

    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /migration plan/i }))
    await user.click(screen.getByRole('button', { name: /rate limiter design/i }))

    await user.click(await screen.findByRole('button', { name: /open saved draft/i }))
    let box = (await screen.findByRole('textbox', { name: /handoff draft/i })) as HTMLTextAreaElement
    expect(box.value).toBe('Reviewed text kept verbatim.')
    expect(screen.getByTestId('handoff-origin')).toHaveTextContent('Continuation 1 Jan')

    unmount()
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /open saved draft/i }))
    box = (await screen.findByRole('textbox', { name: /handoff draft/i })) as HTMLTextAreaElement
    expect(box.value).toBe('Reviewed text kept verbatim.')

    // Reading a saved draft starts nothing and sends nothing.
    expect(holder.fake.api.agentStart).not.toHaveBeenCalled()
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })

  it('saving a draft neither starts a session nor sends a message', async () => {
    const user = userEvent.setup()
    seedThreadWithHistory()
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))
    await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.click(screen.getByRole('button', { name: /save as session record/i }))

    await waitFor(() => expect(holder.fake.api.reviewAdd).toHaveBeenCalled())
    expect(holder.fake.api.agentStart).not.toHaveBeenCalled()
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })

  it('leaves the ordinary empty Start Hermes session path alone', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread({ workdir: '/tmp/project' })]
    holder.fake.directories['/tmp/project'] = directory('/tmp/project')
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /^start hermes session$/i }))

    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(1))
    const created = holder.fake.api.sessionCreate.mock.calls.at(-1)?.[1] as NewSession
    expect(created.briefing).toBe('')
    expect(created.predecessorId).toBeNull()
    expect(screen.queryByTestId('agent-prefilled')).toBeNull()
  })
})

describe('deferred handoff work never lands on another thread', () => {
  it('a save that resolves after a thread switch leaves the other thread untouched', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [
      thread({ notes: 'Decided: pin the clock.' }),
      thread({ id: 'thr_2', title: 'Migration plan' }),
    ]
    holder.fake.sessions = [
      session({ id: 'ses_a', kind: 'hermes-acp' }),
      session({ id: 'ses_b', threadId: 'thr_2', label: 'Second thread session' }),
    ]
    const slow: { release: (() => void) | null } = { release: null }
    const realCreate = holder.fake.api.sessionCreate.getMockImplementation() as (
      threadId: string,
      s: NewSession,
    ) => Promise<Session>
    holder.fake.api.sessionCreate.mockImplementation(async (threadId: string, s: NewSession) => {
      await new Promise<void>((resolve) => {
        slow.release = resolve
      })
      return realCreate(threadId, s)
    })

    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))
    await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.click(screen.getByRole('button', { name: /save as session record/i }))

    await user.click(await screen.findByRole('button', { name: /migration plan/i }))
    await screen.findByText(/second thread session/i)

    await act(async () => {
      slow.release?.()
      await new Promise((r) => setTimeout(r, 30))
    })

    // Thread B keeps exactly its own session, and gains no review item and no
    // draft from the save that belonged to thread A.
    const panel = screen.getByRole('region', { name: /sessions/i })
    expect(within(panel).queryByText(/^continuation/i)).not.toBeInTheDocument()
    expect(within(panel).getByText(/second thread session/i)).toBeInTheDocument()
    expect(screen.queryByRole('textbox', { name: /handoff draft/i })).toBeNull()
    const inbox = screen.getByRole('region', { name: /review/i })
    expect(within(inbox).queryByText(/handoff draft saved/i)).not.toBeInTheDocument()
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })
})

describe('retry after a failed start', () => {
  it('an edited retry uses the edited text, not the briefing of the abandoned record', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread()]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.api.agentStart.mockRejectedValueOnce({
      code: 'CONFLICT',
      message: 'hermes could not be started',
    })
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const box = await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.clear(box)
    await user.type(box, 'FIRST TEXT')
    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/could not be started/i)

    // The user corrects the draft before trying again.
    const again = await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.clear(again)
    await user.type(again, 'CORRECTED TEXT')
    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))

    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(2))
    const acp = holder.fake.api.sessionCreate.mock.calls.filter(
      (c) => (c[1] as NewSession).kind === 'hermes-acp',
    )
    // A fresh record carries the corrected text; the stale one is not reused.
    expect(acp).toHaveLength(2)
    expect((acp[1]![1] as NewSession).briefing).toBe('CORRECTED TEXT')
    expect((acp[1]![1] as NewSession).predecessorId).toBe('ses_a')
    const startedWith = holder.fake.api.agentStart.mock.calls.at(-1)?.[0]
    const fresh = holder.fake.sessions.filter((s) => s.briefing === 'CORRECTED TEXT')
    expect(fresh).toHaveLength(1)
    expect(startedWith).toBe(fresh[0]!.id)

    // The abandoned record is closed rather than left looking startable.
    await waitFor(() => expect(holder.fake.api.sessionEnd).toHaveBeenCalled())
    const composer = (await screen.findByLabelText(/message to hermes/i)) as HTMLTextAreaElement
    await waitFor(() => expect(composer.value).toBe('CORRECTED TEXT'))
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })
})

describe('handoff evidence that could not be read', () => {
  it('labels an unreadable transcript as unknown rather than as an empty history', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread()]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.agentHistory = [
      {
        runId: 'agr_old', sessionId: 'ses_a', program: '/opt/hermes', cwd: '/tmp/project',
        acpSessionId: 'sess-0', startedAt: 1, endedAt: 2, outcome: 'ended by you',
      },
    ]
    holder.fake.api.agentTranscript.mockRejectedValue({
      code: 'INTERNAL',
      message: 'transcript unavailable',
    })
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const box = (await screen.findByRole('textbox', { name: /handoff draft/i })) as HTMLTextAreaElement
    expect(box.value).toContain('agr_old')
    expect(box.value.toLowerCase()).toContain('could not be read')
    expect(box.value.toLowerCase()).toContain('unknown')
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })

  it('carries a bounded transcript tail into the draft as partial evidence', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread()]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    holder.fake.agentHistory = [
      {
        runId: 'agr_old', sessionId: 'ses_a', program: '/opt/hermes', cwd: '/tmp/project',
        acpSessionId: 'sess-0', startedAt: 1, endedAt: 2, outcome: 'ended by you',
      },
    ]
    holder.fake.agentMessages = [
      {
        id: 'm_pub', runId: 'agr_old', key: 'agent:1', role: 'agent', turn: 1,
        body: 'Only the tail survives.', detail: null, status: 'complete',
        createdAt: 5, updatedAt: 5,
      },
    ]
    holder.fake.agentTruncated = true
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const box = (await screen.findByRole('textbox', { name: /handoff draft/i })) as HTMLTextAreaElement
    expect(box.value).toContain('Only the tail survives.')
    expect(box.value.toLowerCase()).toContain('bounded tail')
    expect(box.value.toLowerCase()).toContain('partial')
  })
})

describe('retry after a start that ended its own session', () => {
  /**
   * The real native contract, which `fakeApi` does not model on its own:
   * `AgentManager::start` writes the run row *before* spawning, and settles it
   * with `Store::end_agent_run` when the spawn fails — which ends the owning
   * session in the same transaction (`store.rs`). A later `agentStart` on that
   * session is then rejected with `SESSION_ENDED` for good.
   */
  function startFailsAndEndsSession() {
    holder.fake.api.agentStart.mockImplementationOnce(async (sessionId: string) => {
      const owner = holder.fake.sessions.find((s) => s.id === sessionId)
      if (owner) {
        owner.endedAt = 7
        owner.outcome = 'the agent process could not start: No such file or directory (os error 2)'
      }
      throw {
        code: 'INTERNAL',
        message: 'the agent process could not start: No such file or directory (os error 2)',
      }
    })
  }

  it('starts a fresh same-thread session when the failed start ended the old one', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread()]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    startFailsAndEndsSession()
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const box = await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.clear(box)
    await user.type(box, 'REVIEWED TEXT')
    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/could not start/i)

    // The reviewed draft survives the rejection, unedited.
    const kept = (await screen.findByRole('textbox', {
      name: /handoff draft/i,
    })) as HTMLTextAreaElement
    expect(kept.value).toBe('REVIEWED TEXT')

    const abandoned = holder.fake.sessions.find((s) => s.briefing === 'REVIEWED TEXT')
    expect(abandoned?.endedAt).not.toBeNull()

    // Retry with the text untouched.
    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(2))

    const acp = holder.fake.api.sessionCreate.mock.calls.filter(
      (c) => (c[1] as NewSession).kind === 'hermes-acp',
    )
    // A session ends once, for good: the retry needs a new owner, not the old one.
    expect(acp).toHaveLength(2)
    expect((acp[1]![1] as NewSession).briefing).toBe('REVIEWED TEXT')
    expect((acp[1]![1] as NewSession).predecessorId).toBe('ses_a')
    expect(acp[1]![0]).toBe('thr_1')

    const fresh = holder.fake.sessions.filter(
      (s) => s.briefing === 'REVIEWED TEXT' && s.endedAt === null,
    )
    expect(fresh).toHaveLength(1)
    expect(holder.fake.api.agentStart.mock.calls.at(-1)?.[0]).toBe(fresh[0]!.id)
    expect(holder.fake.api.agentStart.mock.calls[0]?.[0]).not.toBe(fresh[0]!.id)

    // Faiden does not try to re-end a session the native side already ended.
    expect(holder.fake.api.sessionEnd).not.toHaveBeenCalled()

    const composer = (await screen.findByLabelText(/message to hermes/i)) as HTMLTextAreaElement
    await waitFor(() => expect(composer.value).toBe('REVIEWED TEXT'))
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })

  it('still reuses the record when the failed start left the session open', async () => {
    const user = userEvent.setup()
    holder.fake.threads = [thread()]
    holder.fake.sessions = [session({ id: 'ses_a', kind: 'hermes-acp' })]
    // A rejection that never wrote a run row leaves the owning session usable.
    holder.fake.api.agentStart.mockRejectedValueOnce({
      code: 'CONFLICT',
      message: 'hermes could not be started',
    })
    render(<App />)
    await user.click(await screen.findByRole('button', { name: /rate limiter design/i }))
    await user.click(await screen.findByRole('button', { name: /draft handoff/i }))

    const box = await screen.findByRole('textbox', { name: /handoff draft/i })
    await user.clear(box)
    await user.type(box, 'REVIEWED TEXT')
    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/could not be started/i)

    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    await waitFor(() => expect(holder.fake.api.agentStart).toHaveBeenCalledTimes(2))

    const acp = holder.fake.api.sessionCreate.mock.calls.filter(
      (c) => (c[1] as NewSession).kind === 'hermes-acp',
    )
    expect(acp).toHaveLength(1)
    const [first, second] = holder.fake.api.agentStart.mock.calls
    expect(first).toEqual(second)
    expect(holder.fake.api.sessionEnd).not.toHaveBeenCalled()
    expect(holder.fake.api.agentPrompt).not.toHaveBeenCalled()
  })
})
