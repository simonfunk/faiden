import { useEffect, useState } from 'react'

import { checkTitle } from '../domain/validation'
import type { Thread } from '../domain/types'

export interface ThreadHeaderProps {
  thread: Thread
  busy: boolean
  onRename: (title: string) => void
  onMarkReviewed: () => void
  onLocalError: (message: string | null) => void
}

function formatTime(ms: number | null): string {
  if (ms === null) return ''
  return new Date(ms).toLocaleString()
}

export function ThreadHeader({
  thread,
  busy,
  onRename,
  onMarkReviewed,
  onLocalError,
}: ThreadHeaderProps) {
  const [title, setTitle] = useState(thread.title)

  useEffect(() => {
    setTitle(thread.title)
  }, [thread.id, thread.title])

  function commit() {
    if (title.trim() === thread.title) return
    const problem = checkTitle(title)
    if (problem !== null) {
      onLocalError(`Thread title ${problem}`)
      setTitle(thread.title)
      return
    }
    onLocalError(null)
    onRename(title.trim())
  }

  const reviewed = thread.reviewedAt !== null

  return (
    <header className="thread-header">
      {/* The editable field is the visible title; the heading keeps the document
          structure intact for assistive technology. */}
      <h1 className="visually-hidden">{thread.title}</h1>
      <input
        className="thread-header__title"
        aria-label="Thread title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault()
            commit()
          }
          if (e.key === 'Escape') setTitle(thread.title)
        }}
      />

      <div className="thread-header__review">
        <span
          className={`review-state review-state--${reviewed ? 'reviewed' : 'pending'}`}
          data-testid="review-state"
          title={reviewed ? `Reviewed ${formatTime(thread.reviewedAt)}` : undefined}
        >
          {reviewed ? 'Reviewed' : 'Not reviewed'}
        </span>
        <button type="button" onClick={onMarkReviewed} disabled={busy || reviewed}>
          Mark reviewed
        </button>
      </div>
    </header>
  )
}
