import { promptStateLabel, type SuppliedContext } from '../domain/handoff'
import type { AgentRunInfo, AgentRunRecord } from '../domain/agentTypes'
import type { DirectoryContext } from '../domain/types'

export interface ContextPanelProps {
  /** The directory bound to the thread right now, or null when unbound. */
  workdir: string | null
  /** The last inspection of that binding, or null when it has not been read. */
  context: DirectoryContext | null
  /** The live run, if one exists. */
  run: AgentRunInfo | null
  /** A durable ended run, shown only as history when nothing is live. */
  archivedRun: AgentRunInfo | AgentRunRecord | null
  /** What Faiden itself put into the live run, or null when there is none. */
  supplied: SuppliedContext | null
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString()
}

/**
 * The two contexts a reader keeps confusing, told apart on purpose:
 *
 * * what this *thread* is bound to now, and when that was last read;
 * * where the *agent process* was actually started, with which executable.
 *
 * They begin equal and drift the moment the binding changes, because a running
 * process is not moved by re-pointing a thread. Everything shown is a stored
 * fact with its own provenance; nothing here is inferred.
 */
export function ContextPanel({ workdir, context, run, archivedRun, supplied }: ContextPanelProps) {
  const historic = run === null ? archivedRun : null
  const diverged = run !== null && run.cwd !== workdir

  return (
    <section className="panel context-panel" aria-label="Session context">
      <div className="panel__head">
        <h2>Session context</h2>
      </div>

      <div className="context-panel__block" data-testid="context-binding">
        <h3>Directory bound to this thread</h3>
        {workdir === null ? (
          <p className="context-panel__muted">
            No directory bound to this thread. Nothing below describes a repository.
          </p>
        ) : (
          <>
            <p className="ctx-mono context-panel__path">{workdir}</p>
            {context === null ? (
              <p className="context-panel__muted">
                This binding has not been read in this view. What is on disk now is unknown.
              </p>
            ) : !context.isDirectory ? (
              <p className="context-panel__warn">
                This path no longer exists as a directory. The binding is kept so you can repoint
                it; it is not usable as it stands.
              </p>
            ) : (
              <p className="context-panel__muted">
                Read at {formatTime(context.observedAt)}. Branch and working-tree facts are shown in
                the bar above and describe this directory now — not what any agent saw when it
                started.
              </p>
            )}
          </>
        )}
      </div>

      <div className="context-panel__block" data-testid="context-launch">
        <h3>Where the agent actually runs</h3>
        {run !== null ? (
          <dl className="context-panel__facts">
            <dt>Launched in</dt>
            <dd className="ctx-mono">{run.cwd}</dd>
            <dt>Executable</dt>
            <dd className="ctx-mono">{run.program}</dd>
            <dt>Found by</dt>
            <dd>{run.source}</dd>
            <dt>Started</dt>
            <dd>{formatTime(run.startedAt)}</dd>
            <dt>Agent session id</dt>
            <dd className="ctx-mono">
              {run.acpSessionId ?? 'the agent has not reported a session id yet'}
            </dd>
          </dl>
        ) : historic !== null ? (
          <>
            <p className="context-panel__muted">
              No agent is running in this thread. The facts below are historic: they come from
              Faiden’s durable record of a session that has ended.
            </p>
            <dl className="context-panel__facts">
              <dt>Was launched in</dt>
              <dd className="ctx-mono">{historic.cwd ?? '(directory no longer recorded)'}</dd>
              <dt>Executable</dt>
              <dd className="ctx-mono">{historic.program}</dd>
              <dt>Ended</dt>
              <dd>{historic.endedAt === null ? 'end time not recorded' : formatTime(historic.endedAt)}</dd>
            </dl>
          </>
        ) : (
          <p className="context-panel__muted">
            No Hermes session is running or recorded for this thread. Nothing here describes a live
            process.
          </p>
        )}
      </div>

      {diverged ? (
        <p className="context-panel__warn" data-testid="context-divergence" role="status">
          The running agent was started in <span className="ctx-mono">{run.cwd}</span>. This thread{' '}
          {workdir === null ? (
            'has no bound directory'
          ) : (
            <>
              is now bound to <span className="ctx-mono">{workdir}</span>
            </>
          )}
          . These are two different contexts: the running agent was not moved, and re-binding the
          thread does not reach into a process that is already alive.
        </p>
      ) : null}

      {supplied !== null ? (
        <div className="context-panel__block" data-testid="context-supplied">
          <h3>What Faiden supplied to this session</h3>
          {supplied.briefing !== null ? (
            <div className="context-panel__briefing">
              <p className="context-panel__muted">
                Saved briefing on <span className="ctx-mono">{supplied.briefing.sessionLabel}</span>{' '}
                — prepared text, not sent. Faiden stored it; it reaches Hermes only through a
                message you send.
              </p>
              <pre className="context-panel__text">{supplied.briefing.text}</pre>
            </div>
          ) : (
            <p className="context-panel__muted">No briefing is saved on this session.</p>
          )}

          <div data-testid="supplied-prompts">
            <h4>Prompts you sent to this run</h4>
            {supplied.transcriptTruncated ? (
              <p className="context-panel__warn" data-testid="supplied-partial">
                Only a bounded tail of this run’s transcript was read, so this is not a complete
                list. Earlier prompts may exist and are not shown here.
              </p>
            ) : null}
            {supplied.prompts.length === 0 ? (
              <p className="context-panel__muted">No prompt has been sent to this run.</p>
            ) : (
              <ol className="context-panel__prompts">
                {supplied.prompts.map((prompt) => (
                  <li key={prompt.id}>
                    <pre className="context-panel__text">{prompt.body}</pre>
                    <span className={`context-panel__state context-panel__state--${prompt.state}`}>
                      {promptStateLabel(prompt.state)}
                      {prompt.state === 'failed' && prompt.statusText !== null
                        ? ` (${prompt.statusText})`
                        : ''}
                    </span>
                  </li>
                ))}
              </ol>
            )}
          </div>

          <p className="context-panel__muted" data-testid="supplied-boundary">
            {supplied.boundary}
          </p>
        </div>
      ) : null}
    </section>
  )
}
