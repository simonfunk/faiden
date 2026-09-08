import type { ReviewItem } from '../domain/types'

export interface ReviewInboxProps {
  items: ReviewItem[]
  busy: boolean
  onMarkReviewed: (id: string) => void
}

function state(item: ReviewItem): { label: string; modifier: string } {
  if (item.reviewedAt !== null) return { label: 'Reviewed', modifier: 'reviewed' }
  if (item.seenAt !== null) return { label: 'Seen, not reviewed', modifier: 'seen' }
  return { label: 'Unseen', modifier: 'new' }
}

export function ReviewInbox({ items, busy, onMarkReviewed }: ReviewInboxProps) {
  return (
    <section className="panel inbox" aria-label="Review inbox">
      <div className="panel__head">
        <h2>Review</h2>
        <span className="panel__hint">Opening a thread marks items seen. Review stays explicit.</span>
      </div>

      {items.length === 0 ? (
        <p className="panel__empty">Nothing waiting for review.</p>
      ) : (
        <ul className="inbox__list">
          {items.map((item) => {
            const s = state(item)
            return (
              <li key={item.id} className="inbox__item">
                <span className={`inbox__state inbox__state--${s.modifier}`}>{s.label}</span>
                <span className="inbox__summary">{item.summary}</span>
                <button
                  type="button"
                  disabled={busy || item.reviewedAt !== null}
                  onClick={() => onMarkReviewed(item.id)}
                >
                  Mark reviewed
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </section>
  )
}
