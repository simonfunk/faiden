import { describe, expect, it } from 'vitest'

import { LIMIT_NOTES_CHARS, LIMIT_TITLE_CHARS, checkNotes, checkTitle } from './validation'

describe('checkTitle', () => {
  it('mirrors the native bounds so the user is told before a round trip', () => {
    expect(checkTitle('Rate limiter design')).toBeNull()
    expect(checkTitle('  padded  ')).toBeNull()
    expect(checkTitle('')).toMatch(/blank/i)
    expect(checkTitle('   ')).toMatch(/blank/i)
    expect(checkTitle('x'.repeat(LIMIT_TITLE_CHARS))).toBeNull()
    expect(checkTitle('x'.repeat(LIMIT_TITLE_CHARS + 1))).toMatch(/200/)
  })

  it('counts characters, not UTF-16 code units', () => {
    expect(checkTitle('🧵'.repeat(LIMIT_TITLE_CHARS))).toBeNull()
    expect(checkTitle('🧵'.repeat(LIMIT_TITLE_CHARS + 1))).toMatch(/200/)
  })
})

describe('checkNotes', () => {
  it('allows empty notes and bounds long ones', () => {
    expect(checkNotes('')).toBeNull()
    expect(checkNotes('n'.repeat(LIMIT_NOTES_CHARS))).toBeNull()
    expect(checkNotes('n'.repeat(LIMIT_NOTES_CHARS + 1))).toMatch(/20000|20,000/)
  })
})
