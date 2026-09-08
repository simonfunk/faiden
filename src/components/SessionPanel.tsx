import { useState } from 'react'

import type { HandoffDraft, Session } from '../domain/types'

export interface SessionPanelProps {
  sessions: Session[]
  draft: HandoffDraft | null
  busy: boolean
  disabled: boolean
  onDraft: () => void
  onDiscardDraft: () => void
  onSaveDraft: (text: string) => void
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString()
}

function kindLabel(kind: string): string {
  if (kind === 'shell') return 'shell'
  if (kind === 'handoff-draft') return 'handoff draft'
  return kind
}

export function SessionPanel({
  sessions,
  draft,
  busy,
  disabled,
  onDraft,
  onDiscardDraft,
  onSaveDraft,
}: SessionPanelProps) {
  const [text, setText] = useState('')
  const [shownDraftAt, setShownDraftAt] = useState<number | null>(null)

  // Adopt a newly generated draft exactly once, so edits are not clobbered.
  if (draft !== null && draft.generatedAt !== shownDraftAt) {
    setShownDraftAt(draft.generatedAt)
    setText(draft.text)
  }
  if (draft === null && shownDraftAt !== null) {
    setShownDraftAt(null)
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
              </li>
            )
          })}
        </ul>
      )}

      {draft !== null ? (
        <div className="handoff">
          <h3>Handoff draft</h3>
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
          <div className="panel__foot">
            <button type="button" onClick={onDiscardDraft} disabled={busy}>
              Discard
            </button>
            <button type="button" onClick={() => onSaveDraft(text)} disabled={busy}>
              Save as session record
            </button>
          </div>
        </div>
      ) : null}
    </section>
  )
}
