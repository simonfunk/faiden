import { useState } from 'react'

import { checkTitle } from '../domain/validation'
import type { AgentOverview, UnseenReviewCount } from '../domain/agentTypes'
import type { Thread } from '../domain/types'

export interface ThreadSidebarProps {
  threads: Thread[]
  /** Live agent runs, so the sidebar reflects real state rather than guessing. */
  agents: AgentOverview[]
  /** Results a human has not looked at yet, per thread. */
  unseen: UnseenReviewCount[]
  selectedId: string | null
  disabled: boolean
  onSelect: (id: string) => void
  onCreate: (title: string) => void
  onLocalError: (message: string | null) => void
}

function statusLabel(thread: Thread): string {
  if (thread.reviewedAt !== null) return 'Reviewed'
  if (thread.seenAt !== null) return 'Seen, not reviewed'
  return 'New'
}

/**
 * What an agent on this thread is asking of the reader. Derived from protocol
 * state: a pending permission genuinely blocks the agent, whereas a running
 * turn asks for nothing.
 *
 * A run that has *ended* is not thereby uninteresting. A turn that finished, or
 * one that failed, left something to read, and an ended run is exactly the case
 * where no further event will ever arrive to re-announce it. Such a run keeps
 * saying so until the human explicitly reviews the thread — the one act that
 * means "I have looked at this". A run that merely disconnected makes no result
 * claim, so it says nothing.
 *
 * Live state always outranks review state: a reviewed thread whose agent is now
 * blocked on an approval still says so.
 */
function agentBadge(
  agents: AgentOverview[],
  threadId: string,
  reviewed: boolean,
): string | null {
  const mine = agents.filter((a) => a.threadId === threadId)
  const live = mine.filter((a) => a.endedAt === null)

  if (live.some((a) => a.awaitingPermission)) return 'Needs you'
  if (live.some((a) => a.status === 'responding' || a.status === 'cancelling')) return 'Working'
  if (live.some((a) => a.status === 'connecting')) return 'Starting'
  if (mine.some((a) => a.status === 'turnComplete' || a.status === 'error')) {
    return reviewed ? null : 'Has a result'
  }
  if (live.length > 0) return 'Agent ready'
  return null
}

function AgentBadge({ label }: { label: string | null }) {
  if (label === null) return null
  return (
    <span
      className={`thread-item__agent thread-item__agent--${
        label === 'Needs you' ? 'attention' : 'live'
      }`}
      data-testid="thread-agent"
    >
      {label}
    </span>
  )
}

export function ThreadSidebar({
  threads,
  agents,
  unseen,
  selectedId,
  disabled,
  onSelect,
  onCreate,
  onLocalError,
}: ThreadSidebarProps) {
  const [composing, setComposing] = useState(false)
  const [title, setTitle] = useState('')

  function submit() {
    const problem = checkTitle(title)
    if (problem !== null) {
      onLocalError(`Thread title ${problem}`)
      return
    }
    onLocalError(null)
    onCreate(title.trim())
    setTitle('')
    setComposing(false)
  }

  return (
    <nav className="sidebar" aria-label="Threads">
      {/* The window has a transparent title bar, so this strip doubles as the
          drag region. It is inert in a browser preview. */}
      <div className="sidebar__head" data-tauri-drag-region>
        <span className="sidebar__brand" data-tauri-drag-region>
          Faiden
        </span>
        <button
          type="button"
          className="sidebar__new"
          disabled={disabled}
          onClick={() => {
            setComposing(true)
            onLocalError(null)
          }}
        >
          New thread
        </button>
      </div>

      {composing ? (
        <div className="sidebar__compose">
          <input
            aria-label="New thread title"
            placeholder="What are you thinking about?"
            autoFocus
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault()
                submit()
              }
              if (e.key === 'Escape') {
                setComposing(false)
                setTitle('')
                onLocalError(null)
              }
            }}
          />
        </div>
      ) : null}

      {threads.length === 0 ? (
        <p className="sidebar__empty">No threads yet.</p>
      ) : (
        <ul className="sidebar__list">
          {threads.map((t) => (
            <li key={t.id}>
              <button
                type="button"
                className="thread-item"
                aria-current={t.id === selectedId ? 'true' : undefined}
                onClick={() => onSelect(t.id)}
              >
                <span className="thread-item__title">{t.title}</span>
                {(unseen.find((u) => u.threadId === t.id)?.unseen ?? 0) > 0 ? (
                  <span className="thread-item__unseen" data-testid="thread-unseen">
                    {unseen.find((u) => u.threadId === t.id)?.unseen} new
                  </span>
                ) : null}
                <AgentBadge
                  label={agentBadge(agents, t.id, t.reviewedAt !== null)}
                />
                <span
                  className={`thread-item__status thread-item__status--${
                    t.reviewedAt !== null ? 'reviewed' : t.seenAt !== null ? 'seen' : 'new'
                  }`}
                >
                  {statusLabel(t)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </nav>
  )
}
