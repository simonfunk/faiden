// Error handling for the native boundary. Codes mirror `src-tauri/src/error.rs`
// so the UI can branch on them instead of matching prose.

export const NATIVE_UNAVAILABLE = 'NATIVE_UNAVAILABLE'

const NATIVE_CODES = ['VALIDATION', 'NOT_FOUND', 'CONFLICT', 'INTERNAL'] as const

export type AppErrorCode = (typeof NATIVE_CODES)[number] | typeof NATIVE_UNAVAILABLE | 'UNKNOWN'

export interface AppErrorShape {
  code: AppErrorCode
  message: string
  field?: string
  entity?: string
  id?: string
  conflict?: string
}

function isKnownCode(value: unknown): value is (typeof NATIVE_CODES)[number] | typeof NATIVE_UNAVAILABLE {
  return (
    typeof value === 'string' &&
    (value === NATIVE_UNAVAILABLE || (NATIVE_CODES as readonly string[]).includes(value))
  )
}

function optionalString(source: Record<string, unknown>, key: string): string | undefined {
  const v = source[key]
  return typeof v === 'string' ? v : undefined
}

/**
 * Normalises anything a rejected promise can carry. Nothing is invented: a
 * payload we do not recognise becomes `UNKNOWN` rather than a plausible-looking
 * native error.
 */
export function toAppError(value: unknown): AppErrorShape {
  if (typeof value === 'string') {
    return { code: 'UNKNOWN', message: value }
  }
  if (value instanceof Error) {
    return { code: 'UNKNOWN', message: value.message || value.name }
  }
  if (value && typeof value === 'object') {
    const o = value as Record<string, unknown>
    const message = optionalString(o, 'message') ?? 'Unknown error'
    const code: AppErrorCode = isKnownCode(o['code']) ? o['code'] : 'UNKNOWN'
    const out: AppErrorShape = { code, message }
    const field = optionalString(o, 'field')
    const entity = optionalString(o, 'entity')
    const id = optionalString(o, 'id')
    const conflict = optionalString(o, 'conflict')
    if (field !== undefined) out.field = field
    if (entity !== undefined) out.entity = entity
    if (id !== undefined) out.id = id
    if (conflict !== undefined) out.conflict = conflict
    return out
  }
  return { code: 'UNKNOWN', message: 'Unknown error' }
}

export function nativeUnavailableError(action: string): AppErrorShape {
  return {
    code: NATIVE_UNAVAILABLE,
    message: `Cannot ${action}: the native connection is unavailable. This is a browser preview of the Faiden interface, so no real process can be started.`,
  }
}

export function describeError(error: AppErrorShape): string {
  return error.field ? `${error.field}: ${error.message}` : error.message
}
