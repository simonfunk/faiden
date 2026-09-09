import { describe, expect, it } from 'vitest'

import {
  HANDOFF_EXCERPT_MAX_CHARS,
  HANDOFF_MAX_CHARS,
  HANDOFF_MAX_EXCERPTS,
  composeHandoff,
  describeSupplied,
  type HandoffSource,
} from './handoff'
import type { AgentMessage } from './agentTypes'
import type { HandoffDraft, Session } from './types'

function base(over: Partial<HandoffDraft> = {}): HandoffDraft {
  return {
    threadId: 'thr_1',
    predecessorSessionId: 'ses_1',
    text: '# Handoff draft — Rate limiter\n\n## Thread notes\nDecided: pin the clock.',
    disclosure: 'This is a human-authored handoff draft. It does not resume an AI context.',
    generatedAt: 900,
    ...over,
  }
}

function message(over: Partial<AgentMessage> = {}): AgentMessage {
  return {
    id: 'msg_1',
    runId: 'agr_1',
    key: 'user:1',
    role: 'user',
    turn: 1,
    body: 'Reproduce the flaky test.',
    detail: null,
    status: 'sent',
    createdAt: 10,
    updatedAt: 10,
    ...over,
  }
}

function source(messages: AgentMessage[], over: Partial<HandoffSource> = {}): HandoffSource {
  return {
    sessionId: 'ses_1',
    sessionLabel: 'Hermes ACP session',
    runId: 'agr_1',
    messages,
    ...over,
  }
}

function session(over: Partial<Session> = {}): Session {
  return {
    id: 'ses_9',
    threadId: 'thr_1',
    label: 'Handoff continuation',
    kind: 'hermes-acp',
    predecessorId: 'ses_1',
    briefing: 'Goal: finish the rate limiter.',
    createdAt: 1,
    endedAt: null,
    outcome: null,
    ...over,
  }
}

describe('composeHandoff', () => {
  it('keeps the native draft verbatim and adds only cited, labelled excerpts', () => {
    const composed = composeHandoff(base(), [
      source([
        message({ id: 'msg_1', role: 'user', body: 'Reproduce the flaky test.' }),
        message({ id: 'msg_2', role: 'agent', body: 'It fails on a clock boundary.', createdAt: 20 }),
      ]),
    ])

    // The deterministic native text survives untouched.
    expect(composed.text).toContain('Decided: pin the clock.')
    expect(composed.text).toContain('It fails on a clock boundary.')
    // Every excerpt names where it came from, so nothing reads as a summary.
    expect(composed.text).toContain('ses_1')
    expect(composed.text).toContain('msg_2')
    expect(composed.text).toContain('Hermes ACP session')
    expect(composed.excerpts.map((e) => e.messageId)).toEqual(['msg_1', 'msg_2'])
  })

  it('never quotes private reasoning, tool calls or permission payloads, and says so', () => {
    const composed = composeHandoff(base(), [
      source([
        message({ id: 'm_think', role: 'thought', body: 'SECRET-REASONING' }),
        message({ id: 'm_tool', role: 'tool', body: 'SECRET-TOOL-OUTPUT', status: 'complete' }),
        message({ id: 'm_sys', role: 'system', body: 'SECRET-PERMISSION-PAYLOAD' }),
        message({ id: 'm_ok', role: 'agent', body: 'Public answer.' }),
      ]),
    ])

    expect(composed.text).not.toContain('SECRET-REASONING')
    expect(composed.text).not.toContain('SECRET-TOOL-OUTPUT')
    expect(composed.text).not.toContain('SECRET-PERMISSION-PAYLOAD')
    expect(composed.text).toContain('Public answer.')
    expect(composed.excludedByRole).toEqual({ thought: 1, tool: 1, system: 1 })
    expect(composed.text.toLowerCase()).toContain('not quoted')
  })

  it('bounds each excerpt and discloses the truncation rather than hiding it', () => {
    const long = 'x'.repeat(HANDOFF_EXCERPT_MAX_CHARS + 500)
    const composed = composeHandoff(base(), [source([message({ role: 'agent', body: long })])])

    expect(composed.excerpts[0]!.truncated).toBe(true)
    expect(composed.excerpts[0]!.text.length).toBe(HANDOFF_EXCERPT_MAX_CHARS)
    expect(composed.text).not.toContain(long)
    expect(composed.text).toContain(String(long.length))
    expect(composed.text.toLowerCase()).toContain('truncated')
  })

  it('keeps the most recent excerpts and reports how many older ones it dropped', () => {
    const many = Array.from({ length: HANDOFF_MAX_EXCERPTS + 3 }, (_, i) =>
      message({ id: `msg_${i}`, role: 'agent', body: `entry ${i}`, createdAt: i }),
    )
    const composed = composeHandoff(base(), [source(many)])

    expect(composed.excerpts).toHaveLength(HANDOFF_MAX_EXCERPTS)
    expect(composed.excerpts[0]!.messageId).toBe('msg_3')
    expect(composed.omittedOlder).toBe(3)
    expect(composed.text).toContain('3 older')
  })

  it('invents no summary when there is nothing to cite', () => {
    const composed = composeHandoff(base(), [])

    expect(composed.excerpts).toEqual([])
    expect(composed.text).toContain('nothing was cited')
    // No fabricated goal/decision lines beyond the editable placeholders.
    expect(composed.text).not.toMatch(/in summary/i)
  })

  it('is deterministic: the same inputs produce the same text', () => {
    const sources = [source([message({ id: 'msg_1' }), message({ id: 'msg_2', createdAt: 20 })])]
    expect(composeHandoff(base(), sources).text).toBe(composeHandoff(base(), sources).text)
  })

  it('orders excerpts across sessions by time and labels each session separately', () => {
    const composed = composeHandoff(base(), [
      source([message({ id: 'later', role: 'agent', body: 'second', createdAt: 50 })], {
        sessionId: 'ses_2',
        sessionLabel: 'Second session',
        runId: 'agr_2',
      }),
      source([message({ id: 'earlier', role: 'user', body: 'first', createdAt: 5 })]),
    ])

    expect(composed.excerpts.map((e) => e.messageId)).toEqual(['earlier', 'later'])
    expect(composed.excerpts[1]!.sourceLabel).toContain('Second session')
    expect(composed.excerpts[1]!.sourceLabel).toContain('agr_2')
  })
})

describe('describeSupplied', () => {
  it('reports the saved briefing as prepared, not as something Faiden sent', () => {
    const supplied = describeSupplied(session(), [])

    expect(supplied.briefing?.text).toBe('Goal: finish the rate limiter.')
    expect(supplied.briefing?.sessionId).toBe('ses_9')
    expect(supplied.prompts).toEqual([])
    expect(supplied.boundary.toLowerCase()).toContain('not')
  })

  it('lists only the user prompts, with the durable status distinguishing sent from failed', () => {
    const supplied = describeSupplied(session(), [
      message({ id: 'p1', role: 'user', body: 'one', status: 'sent' }),
      message({ id: 'p2', role: 'user', body: 'two', status: 'sending', createdAt: 20 }),
      message({ id: 'p3', role: 'user', body: 'three', status: 'not sent: pipe closed', createdAt: 30 }),
      message({ id: 'p4', role: 'user', body: 'four', status: null, createdAt: 40 }),
      message({ id: 'a1', role: 'agent', body: 'reply', createdAt: 50 }),
      message({ id: 't1', role: 'thought', body: 'private', createdAt: 60 }),
    ])

    expect(supplied.prompts.map((p) => p.id)).toEqual(['p1', 'p2', 'p3', 'p4'])
    expect(supplied.prompts.map((p) => p.state)).toEqual(['sent', 'sending', 'failed', 'unknown'])
    expect(supplied.prompts[2]!.statusText).toBe('not sent: pipe closed')
  })

  it('discloses the boundary: Faiden cannot see the model’s own context or token use', () => {
    const supplied = describeSupplied(null, [])

    expect(supplied.briefing).toBeNull()
    const boundary = supplied.boundary.toLowerCase()
    expect(boundary).toContain('cannot')
    expect(boundary).toMatch(/token|internal/)
  })
})

describe('partial and failed evidence', () => {
  it('says a transcript could not be read, rather than showing it as empty history', () => {
    const composed = composeHandoff(base(), [
      source([], { sessionId: 'ses_broken', runId: 'agr_broken', failed: true }),
    ])

    expect(composed.failedSources).toEqual(['agr_broken'])
    expect(composed.text).toContain('agr_broken')
    expect(composed.text.toLowerCase()).toContain('could not be read')
    // "nothing was cited" alone would read as "there was nothing to cite".
    expect(composed.text.toLowerCase()).toContain('unknown')
  })

  it('carries a bounded transcript tail through as partial evidence', () => {
    const composed = composeHandoff(base(), [
      source([message({ id: 'm1', role: 'agent', body: 'tail entry' })], { truncated: true }),
    ])

    expect(composed.truncatedSources).toEqual(['agr_1'])
    expect(composed.text.toLowerCase()).toContain('bounded tail')
    expect(composed.text.toLowerCase()).toContain('partial')
  })

  it('reports agent runs that the read cap left out', () => {
    const composed = composeHandoff(base(), [source([message()])], { omittedRuns: 4 })

    expect(composed.omittedRuns).toBe(4)
    expect(composed.text).toContain('4 older agent runs')
  })

  it('claims completeness when every source was read in full', () => {
    const composed = composeHandoff(base(), [source([message()])])

    expect(composed.failedSources).toEqual([])
    expect(composed.truncatedSources).toEqual([])
    expect(composed.text.toLowerCase()).not.toContain('could not be read')
    expect(composed.text.toLowerCase()).not.toContain('bounded tail')
  })
})

describe('the native briefing limit', () => {
  it('bounds the whole draft to what the store will accept, counting code points', () => {
    const huge = 'a'.repeat(HANDOFF_MAX_CHARS + 5_000)
    const composed = composeHandoff(base({ text: huge }), [source([message()])])

    expect(composed.overallTruncated).toBe(true)
    expect([...composed.text].length).toBeLessThanOrEqual(HANDOFF_MAX_CHARS)
    expect(composed.text.toLowerCase()).toContain('cut to fit')
  })

  it('counts astral characters the way the native store counts them', () => {
    // '𝄞' is one code point but two UTF-16 units; the store counts chars().
    const huge = '𝄞'.repeat(HANDOFF_MAX_CHARS)
    const composed = composeHandoff(base({ text: huge }), [])

    expect([...composed.text].length).toBeLessThanOrEqual(HANDOFF_MAX_CHARS)
    // Cutting must not split a surrogate pair into a replacement character.
    expect(composed.text).not.toContain('�')
  })

  it('leaves a draft that already fits completely untouched', () => {
    const composed = composeHandoff(base(), [source([message()])])

    expect(composed.overallTruncated).toBe(false)
    expect(composed.text.toLowerCase()).not.toContain('cut to fit')
  })
})

describe('the supplied-context boundary', () => {
  it('never claims to list everything once the transcript is a bounded tail', () => {
    const full = describeSupplied(session(), [message()], false)
    const tail = describeSupplied(session(), [message()], true)

    expect(tail.transcriptTruncated).toBe(true)
    expect(tail.boundary.toLowerCase()).toContain('bounded tail')
    expect(full.boundary.toLowerCase()).not.toContain('bounded tail')
    for (const text of [full.boundary, tail.boundary]) {
      expect(text.toLowerCase()).toContain('cannot')
    }
  })
})
