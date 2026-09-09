import { useState } from 'react'

import type { HandoffDraft, Session } from '../domain/types'

export interface SessionPanelProps {
  sessions: Session[]
  draft: HandoffDraft | null
  /**
   * Changes whenever a different draft is put in the editor. A timestamp is not
   * enough: two drafts opened in the same millisecond are still two drafts, and
   * the second one must replace the first rather than be silently dropped.
   */
  draftKey: number
  /** The label of the saved record being edited, or null for a fresh draft. */
  savedDraftLabel: string | null
  /** True while this thread still owns a running agent. */
  liveAgent: boolean
  busy: boolean
  disabled: boolean
  onDraft: () => void
  onDiscardDraft: () => void
  onSaveDraft: (text: string) => void
  onOpenSaved: (session: Session) => void
  onStartHandoff: (text: string) => void
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString()
}

function kindLabel(kind: string): string {
  if (kind === 'shell') return 'shell'
  if (kind === 'handoff-draft') return 'handoff draft'
  return kind
}

/** A saved draft is a `handoff-draft` record that actually holds text. */
function isReopenable(session: Session): boolean {
  return session.kind === 'handoff-draft' && session.briefing.trim().length > 0
}

export function SessionPanel({
  sessions,
  draft,
  draftKey,
  savedDraftLabel,
  liveAgent,
  busy,
  disabled,
  onDraft,
  onDiscardDraft,
  onSaveDraft,
  onOpenSaved,
  onStartHandoff,
}: SessionPanelProps) {
  const [text, setText] = useState('')
  const [shownKey, setShownKey] = useState<number | null>(null)

  // Adopt a newly opened draft exactly once, so edits are not clobbered.
  if (draft !== null && draftKey !== shownKey) {
    setShownKey(draftKey)
    setText(draft.text)
  }
  if (draft === null && shownKey !== null) {
    setShownKey(null)
    setText('')
  }

  const byId = new Map(sessions.map((s) => [s.id, s]))

  return (
    <section className="panel sessions" aria-label="Sessions">
      <div className="panel__head">
        <h2>Sessions</h2>
        <button type="button" onClick={onDraft} disabled={busy || disabled}>
          Draft handoff
        </button>
      </div>

      {sessions.length === 0 ? (
        <p className="panel__empty">
          No session records yet. A session is created when you start a shell or save a handoff
          draft.
        </p>
      ) : (
        <ul className="session-list">
          {sessions.map((s) => {
            const predecessor = s.predecessorId ? byId.get(s.predecessorId) : undefined
            return (
              <li key={s.id} className="session">
                <div className="session__row">
                  <span className="session__label">{s.label}</span>
                  <span className="session__kind">{kindLabel(s.kind)}</span>
                  <span className="session__time">{formatTime(s.createdAt)}</span>
                </div>
                {predecessor ? (
                  <p className="session__lineage">
                    Continues from <span className="session__ref">{predecessor.label}</span>
                  </p>
                ) : null}
                {s.endedAt !== null ? (
                  <p className="session__outcome">Ended: {s.outcome ?? 'no outcome recorded'}</p>
                ) : null}
                {isReopenable(s) ? (
                  <div className="session__foot">
                    <button
                      type="button"
                      onClick={() => onOpenSaved(s)}
                      disabled={busy || disabled}
                    >
                      Open saved draft
                    </button>
                  </div>
                ) : null}
              </li>
            )
          })}
        </ul>
      )}

      {draft !== null ? (
        <div className="handoff">
          <h3>Handoff draft</h3>
          <p className="handoff__origin" data-testid="handoff-origin">
            {savedDraftLabel === null
              ? 'Composed just now from what Faiden has stored. It has not been saved yet.'
              : `Editing the saved draft “${savedDraftLabel}”. Saving records a new draft; the original stays as it was.`}
          </p>
          <p className="handoff__disclosure" data-testid="handoff-disclosure">
            {draft.disclosure}
          </p>
          <textarea
            aria-label="Handoff draft"
            className="handoff__text"
            rows={12}
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
          <p className="handoff__note" data-testid="handoff-start-note">
            Starting from this text opens a new session and puts the text in the composer. It is a
            new session, not a resumed one, and nothing is sent until you press Send there.
          </p>
          {liveAgent ? (
            <p className="handoff__blocked" data-testid="handoff-blocked" role="status">
              This thread already owns a running agent. End that session before starting a handoff,
              so nothing is interrupted on your behalf.
            </p>
          ) : null}
          <div className="panel__foot">
            <button type="button" onClick={onDiscardDraft} disabled={busy}>
              Discard
            </button>
            <button type="button" onClick={() => onSaveDraft(text)} disabled={busy}>
              Save as session record
            </button>
            <button
              type="button"
              onClick={() => onStartHandoff(text)}
              disabled={busy || disabled || liveAgent || text.trim().length === 0}
            >
              Start new Hermes session with this text
            </button>
          </div>
        </div>
      ) : null}
    </section>
  )
}
