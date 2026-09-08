// Client-side mirror of the native bounds in `src-tauri/src/store.rs`. This is
// a courtesy check only: the native side re-validates and remains authoritative.

export const LIMIT_TITLE_CHARS = 200
export const LIMIT_NOTES_CHARS = 20000

/** Characters, not UTF-16 code units, so emoji count as one. */
function charCount(value: string): number {
  return [...value].length
}

export function checkTitle(value: string): string | null {
  const trimmed = value.trim()
  if (trimmed.length === 0) return 'must not be blank'
  const n = charCount(trimmed)
  if (n > LIMIT_TITLE_CHARS) {
    return `must be at most ${LIMIT_TITLE_CHARS} characters (got ${n})`
  }
  return null
}

export function checkNotes(value: string): string | null {
  const n = charCount(value)
  if (n > LIMIT_NOTES_CHARS) {
    return `must be at most ${LIMIT_NOTES_CHARS} characters (got ${n})`
  }
  return null
}
