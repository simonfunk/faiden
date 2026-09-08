import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { directory, makeFakeApi } from './test/fakeApi'
import type { NewSession, Session, Thread } from './domain/types'

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
