import { useEffect, useRef, useState } from 'react'
import { FitAddon } from '@xterm/addon-fit'
import { Terminal } from '@xterm/xterm'
import '@xterm/xterm/css/xterm.css'

import { api } from '../bridge'
import { createTerminalStream } from '../domain/terminalStream'
import { describeError, toAppError } from '../domain/errors'

export interface XtermSurfaceProps {
  terminalId: string
  epoch: number
  onExit?: (exit: { exitCode: number; success: boolean }) => void
}

/**
 * Owns one xterm instance bound to one native terminal run.
 *
 * The listener is registered *before* the snapshot is requested and the stream
 * reconciles the overlap by sequence number, so re-attaching after a thread
 * switch replays the scrollback exactly once.
 */
export function XtermSurface({ terminalId, epoch, onExit }: XtermSurfaceProps) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const host = hostRef.current
    if (!host) return

    let disposed = false
    const term = new Terminal({
      fontFamily:
        'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
      fontSize: 12.5,
      lineHeight: 1.25,
      cursorBlink: true,
      scrollback: 5000,
      theme: {
        background: '#0b0c11',
        foreground: '#d7dae3',
        cursor: '#a5b4fc',
        selectionBackground: '#312e81',
        black: '#12141c',
        brightBlack: '#3b3f52',
      },
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(host)

    let resyncing = false

    const stream = createTerminalStream({
      terminalId,
      epoch,
      write: (data) => term.write(data),
      onTruncated: () =>
        setNotice('Earlier output exceeded the retained buffer and was dropped.'),
      onGap: () => setError('Terminal output stream was interrupted; refreshing retained output.'),
      onResync: () => {
        if (disposed || resyncing) return
        resyncing = true
        void api
          .terminalSnapshot(terminalId)
          .then((snapshot) => {
            if (disposed) return
            term.reset()
            stream.onSnapshot(snapshot)
          })
          .catch((e) => {
            if (!disposed) {
              setError(
                `Terminal output may be incomplete; retained output could not be refreshed: ${describeError(toAppError(e))}`,
              )
            }
          })
          .finally(() => {
            resyncing = false
          })
      },
      ...(onExit ? { onExit } : {}),
    })

    const dataSub = term.onData((data) => {
      void api.terminalWrite(terminalId, data).catch((e) => setError(describeError(toAppError(e))))
    })

    const pushSize = () => {
      try {
        fit.fit()
      } catch {
        return
      }
      void api
        .terminalResize(terminalId, term.cols, term.rows)
        .catch(() => undefined /* a resize race after exit is not worth surfacing */)
    }

    let unlisten: (() => void) | null = null
    void (async () => {
      try {
        // Subscribe first, then snapshot: the stream discards whatever the
        // snapshot already covers.
        const stop = await api.onTerminalEvent((event) => stream.onEvent(event))
        if (disposed) {
          stop()
          return
        }
        unlisten = stop
        const snapshot = await api.terminalSnapshot(terminalId)
        if (disposed) return
        stream.onSnapshot(snapshot)
        pushSize()
        term.focus()
      } catch (e) {
        if (!disposed) setError(describeError(toAppError(e)))
      }
    })()

    const observer = new ResizeObserver(() => pushSize())
    observer.observe(host)

    return () => {
      disposed = true
      observer.disconnect()
      unlisten?.()
      stream.dispose()
      dataSub.dispose()
      term.dispose()
    }
  }, [terminalId, epoch, onExit])

  return (
    <div className="xterm-surface">
      <div className="xterm-surface__host" ref={hostRef} data-testid="xterm-host" />
      {notice ? <p className="xterm-surface__notice">{notice}</p> : null}
      {error ? (
        <p className="xterm-surface__error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  )
}
