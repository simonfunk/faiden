import { describe, expect, it } from 'vitest'

import { NATIVE_UNAVAILABLE, describeError, nativeUnavailableError, toAppError } from './errors'

describe('toAppError', () => {
  it('keeps the discriminated shape the native side sends', () => {
    const e = toAppError({ code: 'VALIDATION', message: 'must not be blank', field: 'title' })
    expect(e.code).toBe('VALIDATION')
    expect(e.message).toBe('must not be blank')
    expect(e.field).toBe('title')
  })

  it('preserves NOT_FOUND details so stale ids can be handled specifically', () => {
    const e = toAppError({ code: 'NOT_FOUND', message: 'no thread with id x', entity: 'thread', id: 'x' })
    expect(e.code).toBe('NOT_FOUND')
    expect(e.entity).toBe('thread')
    expect(e.id).toBe('x')
  })

  it('does not invent a code for unknown payloads', () => {
    expect(toAppError('boom').code).toBe('UNKNOWN')
    expect(toAppError('boom').message).toBe('boom')
    expect(toAppError(new Error('kaboom')).message).toBe('kaboom')
    expect(toAppError(undefined).code).toBe('UNKNOWN')
    expect(toAppError(undefined).message).toBeTruthy()
    expect(toAppError({ nope: 1 }).code).toBe('UNKNOWN')
  })

  it('rejects a payload whose code is not one Faiden defines', () => {
    expect(toAppError({ code: 'MADE_UP', message: 'x' }).code).toBe('UNKNOWN')
  })

  it('is idempotent', () => {
    const once = toAppError({ code: 'CONFLICT', message: 'busy', conflict: 'TERMINAL_EXITED' })
    expect(toAppError(once)).toEqual(once)
    expect(once.conflict).toBe('TERMINAL_EXITED')
  })
})

describe('nativeUnavailableError', () => {
  it('is explicit that this is a browser preview, not a broken terminal', () => {
    const e = nativeUnavailableError('start a terminal')
    expect(e.code).toBe(NATIVE_UNAVAILABLE)
    expect(e.message.toLowerCase()).toContain('native')
    expect(e.message).toContain('start a terminal')
  })
})

describe('describeError', () => {
  it('renders a field-scoped validation error usefully', () => {
    expect(describeError({ code: 'VALIDATION', message: 'must not be blank', field: 'title' })).toBe(
      'title: must not be blank',
    )
  })

  it('renders other errors as their message', () => {
    expect(describeError({ code: 'INTERNAL', message: 'sqlite: disk full' })).toBe('sqlite: disk full')
  })
})
