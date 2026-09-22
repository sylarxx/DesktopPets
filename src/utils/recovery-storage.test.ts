import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest'
import { clearRecoverySnapshots, readRecoverySnapshot, writeRecoverySnapshot } from './recovery-storage'

describe('renderer recovery snapshots', () => {
  let values: Map<string, string>
  beforeEach(() => {
    values = new Map()
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value) },
      removeItem: (key: string) => { values.delete(key) },
    })
  })
  afterEach(() => vi.unstubAllGlobals())

  it('keeps each window independent and rejects another account snapshot', () => {
    writeRecoverySnapshot('mascot', 'one', { message: 'a', expiresAt: 1000 })
    writeRecoverySnapshot('panel', 'one', { task: 'b', sessionEpoch: 10 })
    expect(readRecoverySnapshot('mascot', 'one')).toEqual({ message: 'a', expiresAt: 1000 })
    expect(readRecoverySnapshot('panel', 'one')).toEqual({ task: 'b', sessionEpoch: 10 })
    expect(readRecoverySnapshot('mascot', 'two')).toBeNull()
    expect(readRecoverySnapshot('panel', '')).toBeNull()
  })

  it('clears both windows on logout without touching the input draft', () => {
    localStorage.setItem('huali_ai_todo_input_draft', 'draft')
    writeRecoverySnapshot('mascot', 'one', { id: 1 })
    writeRecoverySnapshot('panel', 'one', { id: 2 })
    clearRecoverySnapshots()
    expect(readRecoverySnapshot('mascot', 'one')).toBeNull()
    expect(readRecoverySnapshot('panel', 'one')).toBeNull()
    expect(localStorage.getItem('huali_ai_todo_input_draft')).toBe('draft')
  })

  it('does not break interaction when storage is full or corrupt', () => {
    localStorage.setItem('huali_ai_recovery_v1_mascot', '{broken')
    expect(readRecoverySnapshot('mascot', 'one')).toBeNull()
    vi.stubGlobal('localStorage', { setItem() { throw new Error('quota') } })
    expect(() => writeRecoverySnapshot('panel', 'one', { task: 1 })).not.toThrow()
  })
})
