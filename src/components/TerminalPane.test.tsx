import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import type { TerminalInfo } from '../domain/types'

vi.mock('./XtermSurface', () => ({
  XtermSurface: ({ terminalId }: { terminalId: string }) => (
    <div data-testid="xterm-surface" data-terminal-id={terminalId} />
  ),
}))

const { TerminalPane } = await import('./TerminalPane')

function terminal(over: Partial<TerminalInfo> = {}): TerminalInfo {
  return {
    terminalId: 'term_1',
    sessionId: 'ses_1',
    epoch: 1,
    pid: 4242,
    cwd: '/tmp/project',
    commandLabel: '/bin/zsh -l',
    startedAt: 1,
    running: true,
    exit: null,
    ...over,
  }
}

const handlers = { onStart: vi.fn(), onClose: vi.fn(), onExited: vi.fn() }

describe('TerminalPane', () => {
  it('starts nothing on its own and spawns only on an explicit click', async () => {
    const onStart = vi.fn()
    const user = userEvent.setup()
    render(
      <TerminalPane
        native
        workdir="/tmp/project"
        terminal={null}
        busy={false}
        {...handlers}
        onStart={onStart}
      />,
    )

    expect(screen.queryByTestId('xterm-surface')).not.toBeInTheDocument()
    expect(onStart).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: /start shell/i }))
    expect(onStart).toHaveBeenCalledTimes(1)
  })

  it('refuses to start in a browser preview and says why instead of faking one', () => {
    render(
      <TerminalPane native={false} workdir="/tmp/project" terminal={null} busy={false} {...handlers} />,
    )

    expect(screen.getByRole('button', { name: /start shell/i })).toBeDisabled()
    expect(screen.getByText(/native connection unavailable/i)).toBeInTheDocument()
    expect(screen.queryByTestId('xterm-surface')).not.toBeInTheDocument()
  })

  it('labels the directory as the one the shell started in, not an observed cwd', () => {
    render(<TerminalPane native workdir="/tmp/project" terminal={terminal()} busy={false} {...handlers} />)

    const label = screen.getByTestId('terminal-cwd').textContent ?? ''
    expect(label).toContain('/tmp/project')
    expect(label.toLowerCase()).toContain('started in')
  })

  it('does not treat a live process as evidence that an agent is doing anything', () => {
    render(<TerminalPane native workdir={null} terminal={terminal()} busy={false} {...handlers} />)

    expect(screen.getByTestId('terminal-status')).toHaveTextContent(/running/i)
    const caveat = screen.getByTestId('terminal-caveat').textContent?.toLowerCase() ?? ''
    expect(caveat).toContain('not')
    expect(caveat).toMatch(/progress|activity/)
  })

  it('reports the real exit status and offers a fresh start', async () => {
    const onStart = vi.fn()
    const user = userEvent.setup()
    render(
      <TerminalPane
        native
        workdir={null}
        terminal={terminal({
          running: false,
          exit: { exitCode: 7, success: false, description: 'Exited with code 7', at: 5 },
        })}
        busy={false}
        {...handlers}
        onStart={onStart}
      />,
    )

    expect(screen.getByTestId('terminal-status')).toHaveTextContent(/exited with code 7/i)
    expect(screen.queryByTestId('xterm-surface')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: /start shell/i }))
    expect(onStart).toHaveBeenCalledTimes(1)
  })

  it('closes only when asked', async () => {
    const onClose = vi.fn()
    const user = userEvent.setup()
    render(
      <TerminalPane
        native
        workdir={null}
        terminal={terminal()}
        busy={false}
        {...handlers}
        onClose={onClose}
      />,
    )

    expect(onClose).not.toHaveBeenCalled()
    await user.click(screen.getByRole('button', { name: /close terminal/i }))
    expect(onClose).toHaveBeenCalledTimes(1)
  })
})
