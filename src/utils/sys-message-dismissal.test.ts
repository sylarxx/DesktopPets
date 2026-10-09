import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  clearSysMessageDismissals,
  dismissSysMessage,
  isSysMessageDismissed,
} from './sys-message-dismissal'
import { SYS_MESSAGE_EXPIRY_MS } from './sys-message-expiry'

describe('local system message dismissal', () => {
  const start = new Date('2026-10-09T17:00:00').getTime()
  const message = { id: '101', createTime: '2026-10-09 16:55:00' }
  let values: Map<string, string>

  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(start)
    values = new Map()
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value) },
      removeItem: (key: string) => { values.delete(key) },
    })
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.useRealTimers()
  })

  it('persists a close by server ID across transports while isolating accounts and other IDs', () => {
    expect(dismissSysMessage('one', message, start + 60_000)).toBe(true)
    expect(isSysMessageDismissed('one', { ...message })).toBe(true)
    expect(isSysMessageDismissed('one', { ...message, createTime: '2026-10-09T16:55:00' })).toBe(true)
    expect(isSysMessageDismissed('one', { ...message, createTime: '2026-10-09 17:00:00' })).toBe(true)
    expect(isSysMessageDismissed('one', { id: message.id })).toBe(true)
    expect(isSysMessageDismissed('two', message)).toBe(false)
    expect(isSysMessageDismissed('one', { ...message, id: '102' })).toBe(false)
    expect(dismissSysMessage('two', message, start + 60_000)).toBe(true)
    expect(isSysMessageDismissed('one', message)).toBe(true)
    expect(isSysMessageDismissed('two', message)).toBe(true)
  })

  it('uses metadata only even when the supplied message also contains content or credentials', () => {
    const notification = { ...message, msgContent: 'private content', token: 'secret', dedupeKey: 'private content' }
    expect(dismissSysMessage('one', notification, start + 60_000)).toBe(true)
    const stored = JSON.parse([...values.values()][0])
    expect(stored).toEqual([{ userId: 'one', id: '101', createTime: message.createTime, expiresAt: start + 60_000 }])
  })

  it('expires at the original server lifetime and removes expired records', () => {
    expect(dismissSysMessage('one', message, start + SYS_MESSAGE_EXPIRY_MS * 2)).toBe(true)
    const expiry = start + 25 * 60_000
    expect(isSysMessageDismissed('one', message, expiry - 1)).toBe(true)
    expect(isSysMessageDismissed('one', message, expiry)).toBe(false)
    expect(values.size).toBe(0)
  })

  it('caps a missing or future timestamp at 30 minutes and never extends repeated dismissal', () => {
    const withoutTime = { id: '102' }
    const future = { id: '103', createTime: '2026-10-10 17:00:00' }
    expect(dismissSysMessage('one', withoutTime, start + SYS_MESSAGE_EXPIRY_MS * 2)).toBe(true)
    expect(dismissSysMessage('one', future, start + SYS_MESSAGE_EXPIRY_MS * 2)).toBe(true)
    vi.setSystemTime(start + 60_000)
    expect(dismissSysMessage('one', withoutTime, start + SYS_MESSAGE_EXPIRY_MS * 2)).toBe(true)
    expect(isSysMessageDismissed('one', withoutTime, start + SYS_MESSAGE_EXPIRY_MS)).toBe(false)
    expect(isSysMessageDismissed('one', future, start + SYS_MESSAGE_EXPIRY_MS)).toBe(false)
  })

  it('keeps the first expiry when the same server ID replays with a missing timestamp', () => {
    expect(dismissSysMessage('one', message, start + SYS_MESSAGE_EXPIRY_MS)).toBe(true)
    vi.setSystemTime(start + 10 * 60_000)
    expect(dismissSysMessage('one', { id: message.id }, start + SYS_MESSAGE_EXPIRY_MS * 2)).toBe(true)
    const expiry = start + 25 * 60_000
    expect(isSysMessageDismissed('one', { id: message.id }, expiry - 1)).toBe(true)
    expect(isSysMessageDismissed('one', { id: message.id }, expiry)).toBe(false)
  })

  it('keeps only the latest 256 dismissals across accounts', () => {
    for (let i = 0; i < 257; i++) {
      expect(dismissSysMessage(i % 2 ? 'one' : 'two', { id: String(i) }, start + 60_000)).toBe(true)
    }
    expect(JSON.parse([...values.values()][0])).toHaveLength(256)
    expect(isSysMessageDismissed('two', { id: '0' })).toBe(false)
    expect(isSysMessageDismissed('two', { id: '256' })).toBe(true)
  })

  it('rejects missing identities and expired or invalid expiry values', () => {
    expect(dismissSysMessage('', message, start + 60_000)).toBe(false)
    expect(dismissSysMessage('one', { id: ' ' }, start + 60_000)).toBe(false)
    expect(dismissSysMessage('one', message, Number.NaN)).toBe(false)
    expect(dismissSysMessage('one', message, start)).toBe(false)
    expect(dismissSysMessage('one', { id: '104', createTime: '2026-10-09 16:29:59' }, start + 60_000)).toBe(false)
    expect(isSysMessageDismissed('one', message, Number.NaN)).toBe(false)
    expect(values.size).toBe(0)
  })

  it('fails open on malformed runtime user IDs and restored message identities', () => {
    const invalidUsers: unknown[] = [null, undefined, 1102080, {}, []]
    for (const userId of invalidUsers) {
      const runtimeUser = userId as string
      expect(dismissSysMessage(runtimeUser, message, start + 60_000)).toBe(false)
      expect(isSysMessageDismissed(runtimeUser, message)).toBe(false)
    }
    const invalidMessages: unknown[] = [
      null, undefined, '101', 101, [], {}, { id: null }, { id: 101 },
      { id: '101', createTime: 123 }, { id: '101', createTime: null },
    ]
    for (const candidate of invalidMessages) {
      const restored = candidate as Parameters<typeof dismissSysMessage>[1]
      expect(dismissSysMessage('one', restored, start + 60_000)).toBe(false)
      expect(isSysMessageDismissed('one', restored)).toBe(false)
    }
    expect(values.size).toBe(0)
    expect(dismissSysMessage('one', message, start + 60_000)).toBe(true)
    expect(isSysMessageDismissed('one', message)).toBe(true)
  })

  it('fails open on unavailable, corrupt or full storage', () => {
    vi.stubGlobal('localStorage', { getItem() { throw new Error('denied') } })
    expect(dismissSysMessage('one', message, start + 60_000)).toBe(false)
    expect(isSysMessageDismissed('one', message)).toBe(false)
    expect(() => clearSysMessageDismissals()).not.toThrow()
    vi.stubGlobal('localStorage', { getItem: () => '{broken' })
    expect(dismissSysMessage('one', message, start + 60_000)).toBe(false)
    expect(isSysMessageDismissed('one', message)).toBe(false)
    vi.stubGlobal('localStorage', { getItem: () => null, setItem() { throw new Error('quota') } })
    expect(dismissSysMessage('one', message, start + 60_000)).toBe(false)
    expect(isSysMessageDismissed('one', message)).toBe(false)
  })

  it('clears one account without touching another account or other storage', () => {
    values.set('huali_ai_todo_input_draft', 'draft')
    dismissSysMessage('one', message, start + 60_000)
    dismissSysMessage('two', message, start + 60_000)
    clearSysMessageDismissals('one')
    expect(isSysMessageDismissed('one', message)).toBe(false)
    expect(isSysMessageDismissed('two', message)).toBe(true)
    clearSysMessageDismissals()
    expect(isSysMessageDismissed('two', message)).toBe(false)
    expect(values.get('huali_ai_todo_input_draft')).toBe('draft')
  })
})
