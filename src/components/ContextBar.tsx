import type { DirectoryContext } from '../domain/types'

export interface ContextBarProps {
  workdir: string | null
  context: DirectoryContext | null
  busy: boolean
  onPick: () => void
  onClear: () => void
  onRefresh: () => void
}

function GitSummary({ context }: { context: DirectoryContext }) {
  const git = context.git
  if (!git) {
    if (context.gitUnavailable !== null && context.gitUnavailable !== undefined) {
      return (
        <span className="ctx-chip ctx-chip--warn" title={context.gitUnavailable}>
          Git status unavailable
        </span>
      )
    }
    return <span className="ctx-chip ctx-chip--muted">Not a git repository</span>
  }
  return (
    <>
      {git.detached ? (
        <span className="ctx-chip ctx-chip--warn">Detached HEAD</span>
      ) : (
        <span className="ctx-chip">
          <span className="ctx-chip__label">branch</span>
          <span className="ctx-chip__value">{git.branch ?? 'unknown'}</span>
        </span>
      )}
      {git.unborn ? (
        <span className="ctx-chip ctx-chip--muted">No commits yet</span>
      ) : git.headShort ? (
        <span className="ctx-chip">
          <span className="ctx-chip__label">head</span>
          <span className="ctx-chip__value ctx-mono">{git.headShort}</span>
        </span>
      ) : null}
      {git.bare ? <span className="ctx-chip ctx-chip--muted">Bare repository</span> : null}
      {git.isLinkedWorktree ? <span className="ctx-chip">Linked worktree</span> : null}
      {git.bare ? null : git.dirty ? (
        <span className="ctx-chip ctx-chip--warn">
          {git.changedFiles} changed {git.changedFiles === 1 ? 'file' : 'files'}
        </span>
      ) : (
        <span className="ctx-chip ctx-chip--ok">Clean</span>
      )}
    </>
  )
}

export function ContextBar({ workdir, context, busy, onPick, onClear, onRefresh }: ContextBarProps) {
  const missing = workdir !== null && context !== null && !context.isDirectory

  return (
    <section className="context-bar" aria-label="Environment">
      <div className="context-bar__row">
        {workdir === null ? (
          <p className="context-bar__empty">
            No directory bound. Threads do not need one — bind a directory when you want a shell or
            Git context.
          </p>
        ) : (
          <p className="context-bar__path" title={workdir}>
            <span className="ctx-mono">{workdir}</span>
          </p>
        )}

        <div className="context-bar__actions">
          <button type="button" onClick={onPick} disabled={busy}>
            {workdir === null ? 'Choose directory…' : 'Change directory…'}
          </button>
          {workdir !== null ? (
            <>
              <button type="button" onClick={onRefresh} disabled={busy}>
                Re-read
              </button>
              <button type="button" onClick={onClear} disabled={busy}>
                Clear directory
              </button>
            </>
          ) : null}
        </div>
      </div>

      {missing ? (
        <p className="context-bar__alert" role="alert">
          This directory no longer exists. The binding is kept so you can repoint it; nothing was
          deleted from the thread.
        </p>
      ) : null}

      {workdir !== null && context !== null && context.isDirectory ? (
        <div className="context-bar__chips">
          <GitSummary context={context} />
        </div>
      ) : null}

      {workdir !== null && context !== null ? (
        <p className="context-bar__note" data-testid="provenance-note">
          {context.note}
        </p>
      ) : null}
    </section>
  )
}
