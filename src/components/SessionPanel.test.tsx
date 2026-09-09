import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { SessionPanel } from './SessionPanel'
import type { HandoffDraft, Session } from '../domain/types'

function session(over: Partial<Session> = {}): Session {
  return {
    id: 'ses_1',
    threadId: 'thr_1',
    label: 'Hermes ACP session',
    kind: 'hermes-acp',
    predecessorId: null,
    briefing: '',
    createdAt: 1,
    endedAt: null,
    outcome: null,
    ...over,
  }
}

function draft(over: Partial<HandoffDraft> = {}): HandoffDraft {
  return {
    threadId: 'thr_1',
    predecessorSessionId: 'ses_1',
    text: 'Reviewed handoff text.',
    disclosure: 'This is a human-authored handoff draft. It does not resume an AI context.',
    generatedAt: 9,
    ...over,
  }
}

const handlers = () => ({
  onDraft: vi.fn(),
  onDiscardDraft: vi.fn(),
  onSaveDraft: vi.fn(),
  onOpenSaved: vi.fn(),
  onStartHandoff: vi.fn(),
})

function renderPanel(over: Record<string, unknown> = {}) {
  const h = handlers()
  const result = render(
    <SessionPanel
      sessions={[]}
      draft={null}
      draftKey={0}
      savedDraftLabel={null}
      liveAgent={false}
      busy={false}
      disabled={false}
      {...h}
      {...over}
    />,
  )
  return { ...result, ...h }
}

describe('reopening a saved draft', () => {
  it('offers every saved handoff draft for reopening, with its own briefing', async () => {
    const saved = session({
      id: 'ses_2',
      kind: 'handoff-draft',
      label: 'Continuation 1 Jan',
      briefing: 'Reviewed handoff text.',
      predecessorId: 'ses_1',
    })
    const { onOpenSaved } = renderPanel({ sessions: [session(), saved] })

    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: /open saved draft/i }))
    expect(onOpenSaved).toHaveBeenCalledWith(saved)
  })

  it('does not offer to reopen a session that holds no saved draft text', () => {
    renderPanel({ sessions: [session(), session({ id: 'ses_3', kind: 'handoff-draft', briefing: '' })] })
    expect(screen.queryByRole('button', { name: /open saved draft/i })).not.toBeInTheDocument()
  })

  it('says which saved draft is being edited, so an edit is never anonymous', () => {
    renderPanel({ draft: draft(), draftKey: 1, savedDraftLabel: 'Continuation 1 Jan' })
    expect(screen.getByTestId('handoff-origin')).toHaveTextContent('Continuation 1 Jan')
  })
})

describe('starting a new session from reviewed text', () => {
  it('hands the exact edited text to the caller, and only on an explicit press', async () => {
    const { onStartHandoff, onSaveDraft } = renderPanel({ draft: draft(), draftKey: 1 })

    const user = userEvent.setup()
    const box = screen.getByRole('textbox', { name: /handoff draft/i })
    await user.clear(box)
    await user.type(box, 'Edited by hand.')
    expect(onStartHandoff).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: /start new hermes session/i }))
    expect(onStartHandoff).toHaveBeenCalledWith('Edited by hand.')
    expect(onSaveDraft).not.toHaveBeenCalled()
  })

  it('saving sends nothing and starts nothing', async () => {
    const { onStartHandoff, onSaveDraft } = renderPanel({ draft: draft(), draftKey: 1 })

    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: /save as session record/i }))
    expect(onSaveDraft).toHaveBeenCalledWith('Reviewed handoff text.')
    expect(onStartHandoff).not.toHaveBeenCalled()
  })

  it('calls it a new session rather than a resume', () => {
    renderPanel({ draft: draft(), draftKey: 1 })
    const button = screen.getByRole('button', { name: /start new hermes session/i })
    expect(button.textContent?.toLowerCase()).not.toContain('resume')
    expect(screen.getByTestId('handoff-start-note').textContent?.toLowerCase()).toContain(
      'new session',
    )
  })

  it('blocks a handoff start while an agent is still live in this thread', async () => {
    const { onStartHandoff } = renderPanel({ draft: draft(), draftKey: 1, liveAgent: true })

    const button = screen.getByRole('button', { name: /start new hermes session/i })
    expect(button).toBeDisabled()
    expect(screen.getByTestId('handoff-blocked').textContent?.toLowerCase()).toContain('end')

    const user = userEvent.setup()
    await user.click(button)
    expect(onStartHandoff).not.toHaveBeenCalled()
  })

  it('adopts a newly opened draft even when two drafts share a generation time', () => {
    const { rerender } = renderPanel({ draft: draft({ text: 'first' }), draftKey: 1 })
    expect((screen.getByRole('textbox', { name: /handoff draft/i }) as HTMLTextAreaElement).value).toBe(
      'first',
    )

    rerender(
      <SessionPanel
        sessions={[]}
        draft={draft({ text: 'second' })}
        draftKey={2}
        savedDraftLabel={null}
        liveAgent={false}
        busy={false}
        disabled={false}
        onDraft={vi.fn()}
        onDiscardDraft={vi.fn()}
        onSaveDraft={vi.fn()}
        onOpenSaved={vi.fn()}
        onStartHandoff={vi.fn()}
      />,
    )
    expect((screen.getByRole('textbox', { name: /handoff draft/i }) as HTMLTextAreaElement).value).toBe(
      'second',
    )
  })
})
