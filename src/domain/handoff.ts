// Composing a handoff draft, and describing what Faiden actually supplied to a
// run. Both are pure and deterministic on purpose.
//
// Nothing here summarises, infers or rewrites. A handoff draft is the native
// draft text plus verbatim excerpts that each name their own source, so a
// reader can always tell what a human wrote from what a machine copied. The
// private half of a transcript — the agent's own reasoning, tool calls, tool
// output and permission payloads — is never quoted, because a draft is text a
// human is about to hand to another process.

import type { AgentMessage } from './agentTypes'
import type { HandoffDraft, Session } from './types'

/** Per-excerpt character bound. Longer bodies are cut and the cut is disclosed. */
export const HANDOFF_EXCERPT_MAX_CHARS = 600
/** How many excerpts a draft may carry. The newest are kept. */
export const HANDOFF_MAX_EXCERPTS = 8
/**
 * The whole draft's bound, in Unicode code points. It mirrors
 * `LIMIT_BRIEFING_CHARS` in `src-tauri/src/store.rs`, which validates with
 * `value.chars().count()` — code points, not UTF-16 units. A draft composed
 * from long notes and a long previous briefing can exceed it on its own, so the
 * bound is applied to the finished text rather than to the excerpts alone.
 */
export const HANDOFF_MAX_CHARS = 40_000

const CUT_NOTICE =
  '\n\n[Cut to fit: this draft exceeded the 40000-character limit Faiden’s database ' +
  'accepts for a briefing, and the end was removed. Nothing was summarised to make it ' +
  'fit — text was dropped.]'

/** Counts the way the native store counts: Unicode code points. */
function countChars(text: string): number {
  return Array.from(text).length
}

/** Cuts on a code-point boundary, so a surrogate pair is never split. */
function cutChars(text: string, max: number): string {
  return Array.from(text).slice(0, max).join('')
}

/** The only roles that may be quoted: what a human typed and what the agent said back. */
const QUOTABLE_ROLES = new Set(['user', 'agent'])

const ROLE_TITLES: Record<string, string> = {
  user: 'You wrote',
  agent: 'Hermes replied',
}

export interface HandoffSource {
  sessionId: string
  sessionLabel: string
  runId: string
  messages: AgentMessage[]
  /**
   * The transcript read failed. Its content is *unknown*, which is not the same
   * fact as "this run said nothing", and must never be shown as the latter.
   */
  failed?: boolean
  /** The store returned a bounded tail rather than the whole run. */
  truncated?: boolean
}

export interface HandoffEvidenceLimits {
  /** Agent runs this thread has that the read cap left out entirely. */
  omittedRuns?: number
}

export interface HandoffExcerpt {
  messageId: string
  role: string
  /** Identifies the session and run the text was copied out of. */
  sourceLabel: string
  text: string
  truncated: boolean
  /** The full body length, so a truncated excerpt can say what it left out. */
  fullLength: number
  at: number
}

export interface ComposedHandoff {
  text: string
  excerpts: HandoffExcerpt[]
  /** How many entries each excluded role contributed, so the omission is visible. */
  excludedByRole: Record<string, number>
  /** Quotable entries dropped because they fell outside the newest window. */
  omittedOlder: number
  /** Run ids whose transcript could not be read at all. */
  failedSources: string[]
  /** Run ids that returned a bounded tail rather than the whole transcript. */
  truncatedSources: string[]
  /** Agent runs the read cap left out entirely. */
  omittedRuns: number
  /** True when the finished text had to be cut to the native briefing limit. */
  overallTruncated: boolean
  /** The composed length in code points before any overall cut. */
  fullLength: number
}

/**
 * Appends cited excerpts to a native draft. The native text is copied through
 * untouched: it is the part a human wrote and the part the store validated.
 *
 * What the evidence *does not* cover is stated as plainly as what it does. A
 * failed transcript read, a bounded tail and runs beyond the read cap are three
 * different facts, and none of them may be shown as "there was nothing here".
 */
export function composeHandoff(
  base: HandoffDraft,
  sources: HandoffSource[],
  limits: HandoffEvidenceLimits = {},
): ComposedHandoff {
  const excludedByRole: Record<string, number> = {}
  const eligible: HandoffExcerpt[] = []
  const failedSources: string[] = []
  const truncatedSources: string[] = []
  const omittedRuns = limits.omittedRuns ?? 0

  for (const source of sources) {
    if (source.failed === true) failedSources.push(source.runId)
    else if (source.truncated === true) truncatedSources.push(source.runId)
    for (const message of source.messages) {
      if (!QUOTABLE_ROLES.has(message.role)) {
        excludedByRole[message.role] = (excludedByRole[message.role] ?? 0) + 1
        continue
      }
      const body = message.body
      const truncated = body.length > HANDOFF_EXCERPT_MAX_CHARS
      eligible.push({
        messageId: message.id,
        role: message.role,
        sourceLabel: `${source.sessionLabel} (session ${source.sessionId}, run ${source.runId})`,
        text: truncated ? body.slice(0, HANDOFF_EXCERPT_MAX_CHARS) : body,
        truncated,
        fullLength: body.length,
        at: message.createdAt,
      })
    }
  }

  // Stable across equal timestamps: message id breaks the tie, so the same
  // inputs always compose the same text.
  eligible.sort((a, b) => a.at - b.at || a.messageId.localeCompare(b.messageId))
  const omittedOlder = Math.max(0, eligible.length - HANDOFF_MAX_EXCERPTS)
  const excerpts = eligible.slice(omittedOlder)

  const composed = `${base.text}${renderExcerptSection(
    excerpts,
    excludedByRole,
    omittedOlder,
    failedSources,
    truncatedSources,
    omittedRuns,
  )}`
  // Applied last, to the finished text: the native limit covers the notes and
  // any previous briefing carried in `base.text`, not just the excerpts.
  const fullLength = countChars(composed)
  const overallTruncated = fullLength > HANDOFF_MAX_CHARS
  const text = overallTruncated
    ? `${cutChars(composed, HANDOFF_MAX_CHARS - countChars(CUT_NOTICE))}${CUT_NOTICE}`
    : composed

  return {
    text,
    excerpts,
    excludedByRole,
    omittedOlder,
    failedSources,
    truncatedSources,
    omittedRuns,
    overallTruncated,
    fullLength,
  }
}

function renderExcerptSection(
  excerpts: HandoffExcerpt[],
  excludedByRole: Record<string, number>,
  omittedOlder: number,
  failedSources: string[],
  truncatedSources: string[],
  omittedRuns: number,
): string {
  const lines: string[] = ['', '', '## Cited excerpts from this thread’s sessions']
  lines.push(
    'Every line below is verbatim text copied out of a stored message and labelled with',
    'the session, run and message it came from. Faiden did not summarise, interpret or',
    'invent any of it. Read it before you hand it on.',
    '',
  )

  if (excerpts.length === 0) {
    lines.push(
      failedSources.length > 0
        ? '(no quotable entries could be gathered — see the unread transcripts below)'
        : '(no quotable transcript entries were found — nothing was cited)',
    )
  } else {
    if (omittedOlder > 0) {
      lines.push(
        `Showing the ${excerpts.length} most recent quotable entries; ${omittedOlder} older ` +
          'quotable entries exist and were not included.',
        '',
      )
    }
    for (const excerpt of excerpts) {
      lines.push(
        `### ${ROLE_TITLES[excerpt.role] ?? excerpt.role} — ${excerpt.sourceLabel}, message ${excerpt.messageId}`,
      )
      for (const line of excerpt.text.split('\n')) lines.push(`> ${line}`)
      if (excerpt.truncated) {
        lines.push(
          `(truncated: the first ${HANDOFF_EXCERPT_MAX_CHARS} of ${excerpt.fullLength} characters)`,
        )
      }
      lines.push('')
    }
  }

  if (failedSources.length > 0 || truncatedSources.length > 0 || omittedRuns > 0) {
    lines.push('', '## How complete this evidence is')
    lines.push(
      'This section is partial. Treat what is quoted above as a sample, not as the',
      'record of what happened.',
    )
    if (failedSources.length > 0) {
      lines.push(
        `Transcripts that could not be read: ${failedSources.join(', ')}. What those runs`,
        'contain is unknown — this is not a statement that they were empty.',
      )
    }
    if (truncatedSources.length > 0) {
      lines.push(
        `Transcripts returned as a bounded tail, not in full: ${truncatedSources.join(', ')}.`,
        'Earlier entries in those runs exist and were never seen by this draft.',
      )
    }
    if (omittedRuns > 0) {
      lines.push(
        `${omittedRuns} older agent runs in this thread were outside the read cap and were`,
        'not consulted at all.',
      )
    }
  }

  lines.push('', '## What this draft deliberately leaves out')
  const excluded = Object.entries(excludedByRole).sort(([a], [b]) => a.localeCompare(b))
  const counted =
    excluded.length === 0
      ? 'None were present in the entries read.'
      : `${excluded.map(([role, n]) => `${n} ${role}`).join(', ')} — present and not quoted.`
  lines.push(
    'Private agent reasoning, tool calls, tool output, standard error and permission',
    `payloads are never quoted into a draft. ${counted}`,
  )
  return lines.join('\n')
}

export type SuppliedPromptState = 'sent' | 'sending' | 'failed' | 'unknown'

export interface SuppliedPrompt {
  id: string
  body: string
  at: number
  state: SuppliedPromptState
  /** The durable status string exactly as the native side recorded it. */
  statusText: string | null
}

export interface SuppliedContext {
  briefing: { sessionId: string; sessionLabel: string; text: string } | null
  prompts: SuppliedPrompt[]
  /** True when the messages read were a bounded tail of the run. */
  transcriptTruncated: boolean
  boundary: string
}

const BOUNDARY_TAIL =
  'Faiden cannot see Hermes’ own system prompt, its internal or tool context, or its model ' +
  'token usage, and does not claim to. Saved notes, repository files and shell output are ' +
  'not sent by Faiden; anything the agent read, it read by acting on your behalf.'

/** Only claimable when the whole transcript was in hand. */
export const SUPPLIED_BOUNDARY = `This is everything Faiden itself put into this run. ${BOUNDARY_TAIL}`

/** What to say instead when the transcript was a bounded tail. */
export const SUPPLIED_BOUNDARY_PARTIAL =
  'This list is drawn from a bounded tail of the transcript, not the whole run: earlier ' +
  `prompts may exist and are not shown, so this is not a complete list. ${BOUNDARY_TAIL}`

/**
 * What Faiden actually supplied to one run: the briefing saved on the owning
 * session, and the prompts a human pressed Send on. A briefing is prepared
 * text, not something Faiden transmits — that distinction is the whole point.
 */
export function describeSupplied(
  session: Session | null,
  messages: AgentMessage[],
  transcriptTruncated = false,
): SuppliedContext {
  const briefing =
    session !== null && session.briefing.trim().length > 0
      ? { sessionId: session.id, sessionLabel: session.label, text: session.briefing }
      : null

  const prompts = messages
    .filter((m) => m.role === 'user')
    .map((m) => ({
      id: m.id,
      body: m.body,
      at: m.createdAt,
      state: promptState(m.status),
      statusText: m.status,
    }))

  return {
    briefing,
    prompts,
    transcriptTruncated,
    boundary: transcriptTruncated ? SUPPLIED_BOUNDARY_PARTIAL : SUPPLIED_BOUNDARY,
  }
}

/** Mirrors the status strings written in `agent/runtime.rs::prompt`. */
function promptState(status: string | null): SuppliedPromptState {
  if (status === null) return 'unknown'
  if (status === 'sent') return 'sent'
  if (status === 'sending') return 'sending'
  if (status.startsWith('not sent')) return 'failed'
  return 'unknown'
}

export function promptStateLabel(state: SuppliedPromptState): string {
  switch (state) {
    case 'sent':
      return 'sent to Hermes'
    case 'sending':
      return 'still being sent — not confirmed'
    case 'failed':
      return 'not sent'
    case 'unknown':
      return 'delivery not recorded'
  }
}
