import { afterEach, describe, expect, it } from 'vitest'

import { NATIVE_UNAVAILABLE } from './domain/errors'
import { api, isNativeAvailable } from './bridge'

afterEach(() => {
  delete (globalThis as Record<string, unknown>).__TAURI_INTERNALS__
})

describe('native boundary', () => {
  it('reports the browser preview honestly instead of pretending', () => {
    expect(isNativeAvailable()).toBe(false)
  })

  it('sees the native host when Tauri injected its internals', () => {
    ;(globalThis as Record<string, unknown>).__TAURI_INTERNALS__ = {}
    expect(isNativeAvailable()).toBe(true)
  })

  it('refuses every native call in a browser rather than faking a result', async () => {
    const calls: Array<[string, Promise<unknown>]> = [
      ['threadList', api.threadList()],
      ['threadCreate', api.threadCreate('x')],
      ['terminalStart', api.terminalStart('s', null, 80, 24)],
      ['terminalWrite', api.terminalWrite('t', 'ls')],
      ['environmentInspect', api.environmentInspect('/tmp')],
      ['environmentPickDirectory', api.environmentPickDirectory()],
    ]

    for (const [name, promise] of calls) {
      await expect(promise, name).rejects.toMatchObject({ code: NATIVE_UNAVAILABLE })
    }
  })

  it('returns a no-op unsubscribe when there is no native event channel', async () => {
    const unlisten = await api.onTerminalEvent(() => undefined)
    expect(() => unlisten()).not.toThrow()
  })
})
