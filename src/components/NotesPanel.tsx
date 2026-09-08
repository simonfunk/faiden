import { useEffect, useState } from 'react'

import { LIMIT_NOTES_CHARS, checkNotes } from '../domain/validation'
import type { Thread } from '../domain/types'

export interface NotesPanelProps {
  thread: Thread
  busy: boolean
  onSave: (notes: string) => void
  onLocalError: (message: string | null) => void
}

export function NotesPanel({ thread, busy, onSave, onLocalError }: NotesPanelProps) {
  const [notes, setNotes] = useState(thread.notes)

  useEffect(() => {
    setNotes(thread.notes)
  }, [thread.id, thread.notes])

  const dirty = notes !== thread.notes
  const used = [...notes].length

  return (
    <section className="panel notes" aria-label="Briefing notes">
      <div className="panel__head">
        <h2>Notes</h2>
        <span className="panel__hint">
          Free-form. No repository, ticket or worktree required.
        </span>
      </div>

      <textarea
        aria-label="Thread notes"
        className="notes__input"
        rows={7}
        placeholder="Goals, decisions, rejected approaches, open questions…"
        value={notes}
        onChange={(e) => setNotes(e.target.value)}
      />

      <div className="panel__foot">
        <span className={`notes__count ${used > LIMIT_NOTES_CHARS ? 'notes__count--over' : ''}`}>
          {used.toLocaleString()} / {LIMIT_NOTES_CHARS.toLocaleString()}
        </span>
        <button
          type="button"
          disabled={busy || !dirty}
          onClick={() => {
            const problem = checkNotes(notes)
            if (problem !== null) {
              onLocalError(`Notes ${problem}`)
              return
            }
            onLocalError(null)
            onSave(notes)
          }}
        >
          Save notes
        </button>
      </div>
    </section>
  )
}
