import { render, screen, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { ContextPanel } from './ContextPanel'
import { describeSupplied } from '../domain/handoff'
import type { AgentMessage, AgentRunInfo, AgentRunRecord } from '../domain/agentTypes'
import type { DirectoryContext, Session } from '../domain/types'

function run(over: Partial<AgentRunInfo> = {}): AgentRunInfo {
  return {
    runId: 'agr_1',
    sessionId: 'ses_1',
    epoch: 1,
    pid: 5150,
    program: '/Users/example/.local/bin/hermes',
    source: 'PATH entry /Users/example/.local/bin',
    cwd: '/Users/example/project',
    acpSessionId: 'acp_7',
    status: 'ready',
    detail: null,
    startedAt: 1_700_000_000_000,
    endedAt: null,
    promptInFlight: false,
    disclosure: 'This agent runs the Hermes you installed, with your user authority.',
    ...over,
  }
}

function record(over: Partial<AgentRunRecord> = {}): AgentRunRecord {
  return {
    runId: 'agr_old',
    sessionId: 'ses_0',
    program: '/usr/local/bin/hermes',
    cwd: '/Users/example/old-project',
    acpSessionId: null,
    startedAt: 1_600_000_000_000,
    endedAt: 1_600_000_100_000,
    outcome: 'ended by you',
    ...over,
  }
}

function directory(over: Partial<DirectoryContext> = {}): DirectoryContext {
  return {
    path: '/Users/example/project',
    exists: true,
    isDirectory: true,
    provenance: 'user-selected',
    note: 'Directory chosen by you in Faiden.',
    observedAt: 1_700_000_050_000,
    git: null,
    ...over,
  }
}

function session(over: Partial<Session> = {}): Session {
  return {
    id: 'ses_1',
    threadId: 'thr_1',
    label: 'Handoff continuation',
    kind: 'hermes-acp',
    predecessorId: 'ses_0',
    briefing: 'Goal: finish the rate limiter.',
    createdAt: 1,
    endedAt: null,
    outcome: null,
    ...over,
  }
}

function message(over: Partial<AgentMessage> = {}): AgentMessage {
  return {
    id: 'msg_1',
    runId: 'agr_1',
    key: 'user:1',
    role: 'user',
    turn: 1,
    body: 'Start with the failing test.',
    detail: null,
    status: 'sent',
    createdAt: 5,
    updatedAt: 5,
    ...over,
  }
}

describe('bound directory versus where the agent actually runs', () => {
  it('separates the thread binding from the agent’s real launch directory and executable', () => {
    render(
      <ContextPanel
        workdir="/Users/example/project"
        context={directory()}
        run={run()}
        archivedRun={null}
        supplied={null}
      />,
    )

    const binding = screen.getByTestId('context-binding')
    expect(binding).toHaveTextContent('/Users/example/project')

    const launch = screen.getByTestId('context-launch')
    expect(launch).toHaveTextContent('/Users/example/project')
    expect(launch).toHaveTextContent('/Users/example/.local/bin/hermes')
    expect(launch).toHaveTextContent('PATH entry /Users/example/.local/bin')
    expect(screen.queryByTestId('context-divergence')).not.toBeInTheDocument()
  })

  it('says plainly when a re-binding left the live agent in a different directory', () => {
    render(
      <ContextPanel
        workdir="/Users/example/other"
        context={directory({ path: '/Users/example/other' })}
        run={run({ cwd: '/Users/example/project' })}
        archivedRun={null}
        supplied={null}
      />,
    )

    const divergence = screen.getByTestId('context-divergence')
    expect(divergence).toHaveTextContent('/Users/example/project')
    expect(divergence).toHaveTextContent('/Users/example/other')
    expect(divergence.textContent?.toLowerCase()).toContain('was not moved')
  })

  it('labels an unbound thread as unbound rather than borrowing the agent’s directory', () => {
    render(
      <ContextPanel workdir={null} context={null} run={run()} archivedRun={null} supplied={null} />,
    )

    expect(screen.getByTestId('context-binding').textContent?.toLowerCase()).toContain(
      'no directory bound',
    )
    expect(screen.getByTestId('context-divergence')).toHaveTextContent('/Users/example/project')
  })

  it('labels an uninspected binding as uninspected instead of guessing it is current', () => {
    render(
      <ContextPanel
        workdir="/Users/example/project"
        context={null}
        run={null}
        archivedRun={null}
        supplied={null}
      />,
    )

    expect(screen.getByTestId('context-binding').textContent?.toLowerCase()).toContain(
      'not been read',
    )
  })

  it('marks a directory that has gone away instead of presenting it as usable', () => {
    render(
      <ContextPanel
        workdir="/Users/example/gone"
        context={directory({ path: '/Users/example/gone', exists: false, isDirectory: false })}
        run={null}
        archivedRun={null}
        supplied={null}
      />,
    )

    expect(screen.getByTestId('context-binding').textContent?.toLowerCase()).toContain(
      'no longer exists',
    )
  })

  it('makes no live claim when nothing is running, and marks a past run as historic', () => {
    const { rerender } = render(
      <ContextPanel
        workdir="/Users/example/project"
        context={directory()}
        run={null}
        archivedRun={null}
        supplied={null}
      />,
    )
    expect(screen.getByTestId('context-launch').textContent?.toLowerCase()).toContain(
      'no hermes session is running',
    )

    rerender(
      <ContextPanel
        workdir="/Users/example/project"
        context={directory()}
        run={null}
        archivedRun={record()}
        supplied={null}
      />,
    )
    const launch = screen.getByTestId('context-launch')
    expect(launch).toHaveTextContent('/Users/example/old-project')
    expect(launch.textContent?.toLowerCase()).toContain('historic')
    expect(launch.textContent?.toLowerCase()).toContain('no agent is running')
    // Present-tense live facts belong to a live run and must not be reused here.
    expect(launch.textContent?.toLowerCase()).not.toContain('agent session id')
  })

  it('does not invent an agent session id before the agent has reported one', () => {
    render(
      <ContextPanel
        workdir={null}
        context={null}
        run={run({ acpSessionId: null })}
        archivedRun={null}
        supplied={null}
      />,
    )

    expect(screen.getByTestId('context-launch').textContent?.toLowerCase()).toContain(
      'has not reported a session id',
    )
  })
})

describe('what Faiden actually supplied', () => {
  it('shows the saved briefing as prepared text and never as something that was sent', () => {
    render(
      <ContextPanel
        workdir={null}
        context={null}
        run={run()}
        archivedRun={null}
        supplied={describeSupplied(session(), [])}
      />,
    )

    const supplied = screen.getByTestId('context-supplied')
    expect(supplied).toHaveTextContent('Goal: finish the rate limiter.')
    expect(supplied.textContent?.toLowerCase()).toContain('not sent')
    expect(within(supplied).getByTestId('supplied-prompts').textContent?.toLowerCase()).toContain(
      'no prompt has been sent',
    )
  })

  it('distinguishes a prompt that was sent from one that failed and one still in flight', () => {
    render(
      <ContextPanel
        workdir={null}
        context={null}
        run={run()}
        archivedRun={null}
        supplied={describeSupplied(session({ briefing: '' }), [
          message({ id: 'p1', body: 'first prompt', status: 'sent' }),
          message({ id: 'p2', body: 'second prompt', status: 'sending', createdAt: 6 }),
          message({ id: 'p3', body: 'third prompt', status: 'not sent: pipe closed', createdAt: 7 }),
        ])}
      />,
    )

    const prompts = screen.getByTestId('supplied-prompts')
    expect(within(prompts).getByText(/first prompt/)).toBeInTheDocument()
    expect(prompts.textContent).toContain('sent to Hermes')
    expect(prompts.textContent).toContain('still being sent')
    expect(prompts.textContent).toContain('not sent')
  })

  it('never claims Faiden supplied notes, files, shell output or the whole transcript', () => {
    render(
      <ContextPanel
        workdir={null}
        context={null}
        run={run()}
        archivedRun={null}
        supplied={describeSupplied(session(), [message()])}
      />,
    )

    const boundary = screen.getByTestId('supplied-boundary').textContent ?? ''
    expect(boundary.toLowerCase()).toContain('cannot see')
    expect(boundary.toLowerCase()).toContain('token')
    expect(boundary.toLowerCase()).toContain('not sent by faiden')
  })
})

describe('partial transcript evidence', () => {
  it('marks a bounded prompt list as partial instead of implying it is complete', () => {
    render(
      <ContextPanel
        workdir={null}
        context={null}
        run={run()}
        archivedRun={null}
        supplied={describeSupplied(session({ briefing: '' }), [message()], true)}
      />,
    )

    expect(screen.getByTestId('supplied-partial').textContent?.toLowerCase()).toContain(
      'not a complete list',
    )
    expect(screen.getByTestId('supplied-boundary').textContent?.toLowerCase()).toContain(
      'bounded tail',
    )
  })

  it('claims no completeness marker when the whole transcript was read', () => {
    render(
      <ContextPanel
        workdir={null}
        context={null}
        run={run()}
        archivedRun={null}
        supplied={describeSupplied(session({ briefing: '' }), [message()], false)}
      />,
    )

    expect(screen.queryByTestId('supplied-partial')).not.toBeInTheDocument()
  })
})
