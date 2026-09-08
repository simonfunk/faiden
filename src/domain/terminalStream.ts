// Reconciles the "subscribe first, snapshot second" handshake with the native
// terminal.
//
// The listener is registered before the snapshot is requested, so events can
// arrive for output the snapshot already contains. Each event carries a
// sequence number and the snapshot reports the first sequence it does *not*
// contain, which makes the overlap exactly resolvable: buffer until the
// snapshot lands, then replay only what is genuinely new.
//
// After the snapshot, only *contiguous* sequences are consumed. A sequence
// beyond the cursor means an event was genuinely missed, and advancing over it
// would discard real output — or, when the missing event is the Exit, leave the
// view claiming "Running" for a process that is dead. Such an event is held
// instead. If the gap does not close within `gapTimeoutMs` the stream reports
// it and asks to be resynchronised from a fresh snapshot; without a resync it
// skips forward rather than freezing, but never silently.

import type { TerminalEvent, TerminalSnapshot } from './types'

const DEFAULT_MAX_PENDING = 512
const DEFAULT_GAP_TIMEOUT_MS = 500

/** A range of sequence numbers that never arrived. */
export interface SequenceGap {
  from: number
  to: number
}

export interface TerminalStreamOptions {
  terminalId: string
  epoch: number
  write: (data: string) => void
  onExit?: (exit: { exitCode: number; success: boolean }) => void
  onTruncated?: () => void
  /** Reports output that was missed. Never called for a gap that closes. */
  onGap?: (gap: SequenceGap) => void
  /**
   * Asked for a fresh snapshot when a gap does not close. The stream returns to
   * its attach state, so the handler need only call `onSnapshot` again.
   */
  onResync?: () => void
  /** Cap on events held while the snapshot is in flight, or across a gap. */
  maxPending?: number
  /** How long an unfilled gap is tolerated before resyncing. */
  gapTimeoutMs?: number
}

export interface TerminalStream {
  onEvent(event: TerminalEvent): void
  onSnapshot(snapshot: TerminalSnapshot): void
  hasExited(): boolean
  pendingCount(): number
  /** Events received out of order and waiting for the gap before them. */
  heldCount(): number
  dispose(): void
}

export function createTerminalStream(options: TerminalStreamOptions): TerminalStream {
  const maxPending = options.maxPending ?? DEFAULT_MAX_PENDING
  const gapTimeoutMs = options.gapTimeoutMs ?? DEFAULT_GAP_TIMEOUT_MS

  let pending: TerminalEvent[] = []
  let held = new Map<number, TerminalEvent>()
  let gapTimer: ReturnType<typeof setTimeout> | null = null
  let nextSeq: number | null = null
  let exited = false
  let disposed = false

  /** Rejects anything from another terminal or an earlier/later run. */
  const mine = (event: TerminalEvent) =>
    event.terminalId === options.terminalId && event.epoch === options.epoch

  function cancelGapTimer() {
    if (gapTimer !== null) {
      clearTimeout(gapTimer)
      gapTimer = null
    }
  }

  /** Times from the *first* unfilled gap, so it cannot be postponed forever. */
  function armGapTimer() {
    if (gapTimer === null) gapTimer = setTimeout(onGapTimeout, gapTimeoutMs)
  }

  /** Consumes an event that is exactly at the cursor. */
  function consume(event: TerminalEvent) {
    nextSeq = event.seq + 1
    if (event.type === 'output') {
      options.write(event.chunk)
      return
    }
    if (!exited) {
      exited = true
      options.onExit?.({ exitCode: event.exitCode, success: event.success })
    }
  }

  function drainHeld() {
    while (nextSeq !== null) {
      const event = held.get(nextSeq)
      if (!event) break
      held.delete(nextSeq)
      consume(event)
    }
  }

  function apply(event: TerminalEvent) {
    if (nextSeq === null || event.seq < nextSeq) return
    if (event.seq > nextSeq) {
      held.set(event.seq, event)
      // Keep the sequences closest to the cursor: those are the ones that can
      // still become contiguous.
      while (held.size > maxPending) {
        const highest = Math.max(...held.keys())
        held.delete(highest)
      }
      armGapTimer()
      return
    }
    consume(event)
    drainHeld()
    if (held.size === 0) cancelGapTimer()
    else armGapTimer()
  }

  function onGapTimeout() {
    gapTimer = null
    if (disposed || nextSeq === null || held.size === 0) return

    const resumeAt = Math.min(...held.keys())
    options.onGap?.({ from: nextSeq, to: resumeAt - 1 })

    if (options.onResync) {
      // Back to the attach state: buffer what we hold and wait for a fresh cut,
      // which is authoritative for both the scrollback and the cursor.
      pending = [...held.values()]
      held = new Map()
      nextSeq = null
      options.onResync()
      return
    }

    // No resync available. Skipping is a loss, but it was already reported and
    // it is better than a view that never updates again.
    nextSeq = resumeAt
    drainHeld()
    if (held.size > 0) armGapTimer()
  }

  return {
    onEvent(event) {
      if (disposed || !mine(event)) return
      if (nextSeq === null) {
        pending.push(event)
        if (pending.length > maxPending) {
          // The snapshot is taken after these events, so it already covers the
          // oldest ones; dropping them loses nothing.
          pending = pending.slice(pending.length - maxPending)
        }
        return
      }
      apply(event)
    },

    onSnapshot(snapshot) {
      if (disposed || nextSeq !== null) return
      if (snapshot.data.length > 0) options.write(snapshot.data)
      if (snapshot.truncated) options.onTruncated?.()
      nextSeq = snapshot.nextSeq

      const queued = pending.slice().sort((a, b) => a.seq - b.seq)
      pending = []
      for (const event of queued) apply(event)

      if (!exited && snapshot.info.exit && !snapshot.info.running) {
        // The process finished before we attached; the exit event is already
        // superseded, so report it from the snapshot instead.
        exited = true
        options.onExit?.({
          exitCode: snapshot.info.exit.exitCode,
          success: snapshot.info.exit.success,
        })
      }
    },

    hasExited: () => exited,
    pendingCount: () => pending.length,
    heldCount: () => held.size,

    dispose() {
      disposed = true
      cancelGapTimer()
      pending = []
      held = new Map()
    },
  }
}
