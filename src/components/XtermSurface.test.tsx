import { act, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import type { TerminalEvent, TerminalSnapshot } from '../domain/types'

const mocks = vi.hoisted(() => {
  const terminals: Array<{
    writes: string[]
    reset: ReturnType<typeof vi.fn>
    dispose: ReturnType<typeof vi.fn>
  }> = []
  let eventHandler: ((event: TerminalEvent) => void) | null = null
  const api = {
    terminalWrite: vi.fn(async () => undefined),
    terminalResize: vi.fn(async () => undefined),
    terminalSnapshot: vi.fn(),
    onTerminalEvent: vi.fn(async (handler: (event: TerminalEvent) => void) => {
      eventHandler = handler
      return () => {
        eventHandler = null
      }
    }),
  }
  return { api, terminals, emit: (event: TerminalEvent) => eventHandler?.(event) }
})

vi.stubGlobal(
  'ResizeObserver',
  class {
    observe = vi.fn()
    disconnect = vi.fn()
  },
)

vi.mock('../bridge', () => ({ api: mocks.api }))
vi.mock('@xterm/addon-fit', () => ({ FitAddon: class { fit = vi.fn() } }))
vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 80
    rows = 24
    writes: string[] = []
    reset = vi.fn()
    dispose = vi.fn()
    constructor() {
      mocks.terminals.push(this)
    }
    loadAddon = vi.fn()
    open = vi.fn()
    focus = vi.fn()
    write = (data: string) => this.writes.push(data)
    onData = vi.fn(() => ({ dispose: vi.fn() }))
  },
}))

const { XtermSurface } = await import('./XtermSurface')

function snapshot(terminalId: string, data = '', nextSeq = 1): TerminalSnapshot {
  return {
    info: {
      terminalId,
      sessionId: 'session',
      epoch: 1,
      pid: 1,
      cwd: null,
      commandLabel: 'shell',
      startedAt: 0,
      running: true,
      exit: null,
    },
    data,
    nextSeq,
    truncated: false,
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

describe('XtermSurface sequence recovery', () => {
  afterEach(() => {
    vi.useRealTimers()
    mocks.api.terminalWrite.mockClear()
    mocks.api.terminalResize.mockClear()
    mocks.api.terminalSnapshot.mockReset()
    mocks.api.onTerminalEvent.mockClear()
    mocks.terminals.splice(0)
  })

  it('uses the production event callback to resync a real sequence gap and replay the fresh snapshot', async () => {
    mocks.api.terminalSnapshot.mockResolvedValueOnce(snapshot('term-a'))
    render(<XtermSurface terminalId="term-a" epoch={1} />)
    await act(async () => {})
    expect(mocks.api.terminalSnapshot).toHaveBeenCalledWith('term-a')

    vi.useFakeTimers()
    const fresh = deferred<TerminalSnapshot>()
    mocks.api.terminalSnapshot.mockReturnValueOnce(fresh.promise)
    act(() => {
      mocks.emit({ type: 'output', terminalId: 'term-a', epoch: 1, seq: 2, chunk: 'later' })
      vi.advanceTimersByTime(500)
    })

    expect(mocks.api.terminalSnapshot).toHaveBeenCalledTimes(2)
    expect(screen.getByRole('alert')).toHaveTextContent(/output stream was interrupted/i)
    await act(async () => fresh.resolve(snapshot('term-a', 'retained output', 3)))

    expect(mocks.terminals[0]?.reset).toHaveBeenCalledTimes(1)
    expect(mocks.terminals[0]?.writes).toEqual(['retained output'])
  })

  it('does not reset a replaced terminal when its deferred resync snapshot resolves late', async () => {
    mocks.api.terminalSnapshot.mockResolvedValueOnce(snapshot('term-a'))
    const view = render(<XtermSurface terminalId="term-a" epoch={1} />)
    await act(async () => {})
    expect(mocks.api.terminalSnapshot).toHaveBeenCalledWith('term-a')
    const oldTerminal = mocks.terminals[0]

    vi.useFakeTimers()
    const stale = deferred<TerminalSnapshot>()
    mocks.api.terminalSnapshot.mockReturnValueOnce(stale.promise)
    act(() => {
      mocks.emit({ type: 'output', terminalId: 'term-a', epoch: 1, seq: 2, chunk: 'later' })
      vi.advanceTimersByTime(500)
    })

    mocks.api.terminalSnapshot.mockResolvedValueOnce(snapshot('term-b'))
    view.rerender(<XtermSurface terminalId="term-b" epoch={1} />)
    await act(async () => {})
    expect(mocks.api.terminalSnapshot).toHaveBeenCalledWith('term-b')
    await act(async () => stale.resolve(snapshot('term-a', 'stale output', 3)))

    expect(oldTerminal?.reset).not.toHaveBeenCalled()
    expect(oldTerminal?.writes).toEqual([])
  })
})
