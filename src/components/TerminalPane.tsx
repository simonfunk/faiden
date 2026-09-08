import type { TerminalInfo } from '../domain/types'
import { XtermSurface } from './XtermSurface'

export interface TerminalPaneProps {
  native: boolean
  /** The directory the next shell would start in. */
  workdir: string | null
  terminal: TerminalInfo | null
  busy: boolean
  onStart: () => void
  onClose: () => void
  onExited: () => void
}

export function TerminalPane({
  native,
  workdir,
  terminal,
  busy,
  onStart,
  onClose,
  onExited,
}: TerminalPaneProps) {
  const running = terminal?.running === true
  const exit = terminal?.exit ?? null

  return (
    <section className="panel terminal" aria-label="Terminal">
      <div className="panel__head">
        <h2>Terminal</h2>
        <div className="panel__actions">
          {running ? (
            <button type="button" onClick={onClose} disabled={busy}>
              Close terminal
            </button>
          ) : (
            <button type="button" onClick={onStart} disabled={busy || !native}>
              Start shell
            </button>
          )}
        </div>
      </div>

      {!native ? (
        <p className="terminal__unavailable">
          Native connection unavailable. This is a browser preview of the Faiden interface — it
          cannot start a real process, and nothing here is simulated.
        </p>
      ) : null}

      {terminal === null ? (
        native ? (
          <p className="panel__empty">
            No terminal for this thread yet. Starting one launches your login shell
            {workdir === null ? '' : ` in ${workdir}`}.
          </p>
        ) : null
      ) : (
        <>
          <div className="terminal__meta">
            <span className="terminal__status" data-testid="terminal-status">
              {running ? (
                <>
                  <span className="dot dot--live" aria-hidden="true" /> Running
                </>
              ) : (
                <>
                  <span className="dot dot--done" aria-hidden="true" />{' '}
                  {exit ? exit.description : 'Exited'}
                </>
              )}
            </span>
            <span className="terminal__command ctx-mono">{terminal.commandLabel}</span>
            {terminal.pid !== null ? (
              <span className="terminal__pid">pid {terminal.pid}</span>
            ) : null}
            {terminal.cwd !== null ? (
              <span className="terminal__cwd" data-testid="terminal-cwd">
                started in <span className="ctx-mono">{terminal.cwd}</span>
              </span>
            ) : null}
          </div>

          <p className="terminal__caveat" data-testid="terminal-caveat">
            A live process, terminal output and CPU use are not evidence of agent progress. This
            build reports process lifecycle only.
          </p>

          <XtermSurface
            key={terminal.terminalId}
            terminalId={terminal.terminalId}
            epoch={terminal.epoch}
            onExit={onExited}
          />
        </>
      )}
    </section>
  )
}
