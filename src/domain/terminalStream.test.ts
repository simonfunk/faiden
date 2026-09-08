import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { TerminalEvent, TerminalSnapshot } from './types'
import { createTerminalStream } from './terminalStream'

const ID = 'term_a'
const EPOCH = 3

function output(seq: number, chunk: string, over: Partial<{ terminalId: string; epoch: number }> = {}): TerminalEvent {
  return { type: 'output', terminalId: over.terminalId ?? ID, epoch: over.epoch ?? EPOCH, seq, chunk }
}

function snapshot(data: string, nextSeq: number, over: Partial<TerminalSnapshot> = {}): TerminalSnapshot {
  return {
    info: {
      terminalId: ID,
      sessionId: 's',
      epoch: EPOCH,
      pid: 42,
      cwd: null,
      commandLabel: '/bin/zsh -l',
      startedAt: 0,
      running: true,
      exit: null,
    },
    data,
    nextSeq,
    truncated: false,
    ...over,
  }
}

describe('createTerminalStream', () => {
  it('holds events that arrive before the snapshot, then replays only the new ones', () => {
    const write = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write })

    // Listener registered first: these arrive while the snapshot is in flight.
    s.onEvent(output(1, 'a'))
    s.onEvent(output(2, 'b'))
    s.onEvent(output(3, 'c'))
    expect(write).not.toHaveBeenCalled()

    // The snapshot already contains seq 1 and 2.
    s.onSnapshot(snapshot('ab', 3))

    expect(write.mock.calls.map((c) => c[0])).toEqual(['ab', 'c'])
  })

  it('never writes a chunk twice when the snapshot lands after the event', () => {
    const write = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write })
    s.onEvent(output(1, 'hello'))
    s.onSnapshot(snapshot('hello', 2))
    s.onEvent(output(2, ' world'))

    expect(write.mock.calls.map((c) => c[0]).join('')).toBe('hello world')
  })

  it('replays buffered events in sequence order regardless of delivery order', () => {
    const write = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write })
    s.onEvent(output(3, 'c'))
    s.onEvent(output(2, 'b'))
    s.onSnapshot(snapshot('', 2))

    expect(write.mock.calls.map((c) => c[0])).toEqual(['b', 'c'])
  })

  it('ignores events belonging to a different terminal or a previous run', () => {
    const write = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write })
    s.onSnapshot(snapshot('', 1))

    s.onEvent(output(1, 'other', { terminalId: 'term_b' }))
    s.onEvent(output(1, 'stale', { epoch: EPOCH - 1 }))
    s.onEvent(output(1, 'future', { epoch: EPOCH + 1 }))
    expect(write).not.toHaveBeenCalled()

    s.onEvent(output(1, 'mine'))
    expect(write).toHaveBeenCalledWith('mine')
  })

  it('drops duplicated or already-superseded sequences', () => {
    const write = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write })
    s.onSnapshot(snapshot('abc', 4))

    s.onEvent(output(2, 'b'))
    s.onEvent(output(3, 'c'))
    expect(write).toHaveBeenCalledTimes(1) // the snapshot only

    s.onEvent(output(4, 'd'))
    s.onEvent(output(4, 'd again'))
    expect(write.mock.calls.map((c) => c[0])).toEqual(['abc', 'd'])
  })

  it('reports the authoritative exit exactly once and stops writing afterwards', () => {
    const write = vi.fn()
    const onExit = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write, onExit })
    s.onSnapshot(snapshot('', 1))

    s.onEvent({ type: 'exit', terminalId: ID, epoch: EPOCH, seq: 1, exitCode: 7, success: false })
    s.onEvent({ type: 'exit', terminalId: ID, epoch: EPOCH, seq: 1, exitCode: 7, success: false })

    expect(onExit).toHaveBeenCalledTimes(1)
    expect(onExit).toHaveBeenCalledWith({ exitCode: 7, success: false })
    expect(s.hasExited()).toBe(true)
  })

  it('surfaces snapshot truncation so dropped scrollback is never silent', () => {
    const write = vi.fn()
    const onTruncated = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write, onTruncated })
    s.onSnapshot(snapshot('tail only', 9, { truncated: true }))
    expect(onTruncated).toHaveBeenCalledTimes(1)
  })

  it('does nothing at all once disposed, so a late listener cannot write to a dead view', () => {
    const write = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write })
    s.onSnapshot(snapshot('a', 2))
    s.dispose()
    s.onEvent(output(2, 'b'))
    s.onSnapshot(snapshot('zzz', 5))
    expect(write.mock.calls.map((c) => c[0])).toEqual(['a'])
  })

  it('bounds how many pre-snapshot events it buffers', () => {
    const write = vi.fn()
    const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write, maxPending: 3 })
    for (let i = 1; i <= 10; i += 1) s.onEvent(output(i, String(i)))
    expect(s.pendingCount()).toBe(3)

    // The snapshot is taken after those events, so it already contains 1..7 —
    // dropping the oldest pending events loses nothing.
    s.onSnapshot(snapshot('1234567', 8))
    expect(write.mock.calls.map((c) => c[0])).toEqual(['1234567', '8', '9', '10'])
  })

  // --- Sequence gaps --------------------------------------------------------
  //
  // Events arrive on one channel, but a gap means output was genuinely missed.
  // Advancing over it would silently discard the missing range and — when the
  // missing event is the Exit — leave the view falsely "Running".

  describe('post-snapshot sequence gaps', () => {
    beforeEach(() => vi.useFakeTimers())
    afterEach(() => vi.useRealTimers())

    it('holds an out-of-order event instead of advancing past the gap', () => {
      const write = vi.fn()
      const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write })
      s.onSnapshot(snapshot('', 1))

      s.onEvent(output(3, 'c'))
      expect(write).not.toHaveBeenCalled()
      expect(s.heldCount()).toBe(1)

      s.onEvent(output(2, 'b'))
      expect(write).not.toHaveBeenCalled()

      s.onEvent(output(1, 'a'))
      expect(write.mock.calls.map((c) => c[0])).toEqual(['a', 'b', 'c'])
      expect(s.heldCount()).toBe(0)
    })

    it('does not report an exit that arrives before the output it follows', () => {
      const write = vi.fn()
      const onExit = vi.fn()
      const s = createTerminalStream({ terminalId: ID, epoch: EPOCH, write, onExit })
      s.onSnapshot(snapshot('', 1))

      s.onEvent({ type: 'exit', terminalId: ID, epoch: EPOCH, seq: 2, exitCode: 0, success: true })
      expect(onExit).not.toHaveBeenCalled()
      expect(s.hasExited()).toBe(false)

      s.onEvent(output(1, 'bye'))
      expect(write).toHaveBeenCalledWith('bye')
      expect(onExit).toHaveBeenCalledWith({ exitCode: 0, success: true })
      expect(s.hasExited()).toBe(true)
    })

    it('asks for a resync rather than freezing when a gap never closes', () => {
      const write = vi.fn()
      const onResync = vi.fn()
      const onGap = vi.fn()
      const s = createTerminalStream({
        terminalId: ID,
        epoch: EPOCH,
        write,
        onResync,
        onGap,
        gapTimeoutMs: 200,
      })
      s.onSnapshot(snapshot('', 1))
      s.onEvent(output(3, 'c'))

      vi.advanceTimersByTime(199)
      expect(onResync).not.toHaveBeenCalled()

      vi.advanceTimersByTime(1)
      expect(onGap).toHaveBeenCalledWith({ from: 1, to: 2 })
      expect(onResync).toHaveBeenCalledTimes(1)

      // The stream is back in its attach state: a fresh cut resolves everything.
      s.onEvent(output(4, 'd'))
      s.onSnapshot(snapshot('abc', 4))
      expect(write.mock.calls.map((c) => c[0])).toEqual(['abc', 'd'])
    })

    it('never freezes when no resync is available: it skips forward and says so', () => {
      const write = vi.fn()
      const onGap = vi.fn()
      const s = createTerminalStream({
        terminalId: ID,
        epoch: EPOCH,
        write,
        onGap,
        gapTimeoutMs: 100,
      })
      s.onSnapshot(snapshot('', 1))
      s.onEvent(output(4, 'd'))

      vi.advanceTimersByTime(100)
      expect(onGap).toHaveBeenCalledWith({ from: 1, to: 3 })
      expect(write.mock.calls.map((c) => c[0])).toEqual(['d'])

      // And the stream keeps working from there.
      s.onEvent(output(5, 'e'))
      expect(write.mock.calls.map((c) => c[0])).toEqual(['d', 'e'])
    })

    it('asks for nothing while the stream stays contiguous', () => {
      const onResync = vi.fn()
      const onGap = vi.fn()
      const s = createTerminalStream({
        terminalId: ID,
        epoch: EPOCH,
        write: vi.fn(),
        onResync,
        onGap,
        gapTimeoutMs: 50,
      })
      s.onSnapshot(snapshot('', 1))
      s.onEvent(output(1, 'a'))
      s.onEvent(output(2, 'b'))

      vi.advanceTimersByTime(5_000)
      expect(onResync).not.toHaveBeenCalled()
      expect(onGap).not.toHaveBeenCalled()
    })

    it('cancels a pending resync once the gap closes on its own', () => {
      const onResync = vi.fn()
      const s = createTerminalStream({
        terminalId: ID,
        epoch: EPOCH,
        write: vi.fn(),
        onResync,
        gapTimeoutMs: 100,
      })
      s.onSnapshot(snapshot('', 1))
      s.onEvent(output(2, 'b'))
      vi.advanceTimersByTime(50)
      s.onEvent(output(1, 'a'))

      vi.advanceTimersByTime(5_000)
      expect(onResync).not.toHaveBeenCalled()
    })

    it('does not resync after being disposed', () => {
      const onResync = vi.fn()
      const s = createTerminalStream({
        terminalId: ID,
        epoch: EPOCH,
        write: vi.fn(),
        onResync,
        gapTimeoutMs: 50,
      })
      s.onSnapshot(snapshot('', 1))
      s.onEvent(output(9, 'far ahead'))
      s.dispose()

      vi.advanceTimersByTime(5_000)
      expect(onResync).not.toHaveBeenCalled()
    })

    it('bounds how many out-of-order events it holds', () => {
      const s = createTerminalStream({
        terminalId: ID,
        epoch: EPOCH,
        write: vi.fn(),
        maxPending: 3,
        gapTimeoutMs: 1_000,
      })
      s.onSnapshot(snapshot('', 1))
      for (let i = 2; i <= 20; i += 1) s.onEvent(output(i, String(i)))
      expect(s.heldCount()).toBe(3)
    })
  })
})
