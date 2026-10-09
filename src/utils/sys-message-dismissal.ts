import { resolveSysMessageExpiresAt } from './sys-message-expiry'

const STORAGE_KEY = 'huali_ai_sys_message_dismissals_v1'
const MAX_DISMISSALS = 256

interface MessageIdentity {
  id: string
  createTime?: string
}

interface DismissalRecord {
  userId: string
  id: string
  createTime: string
  expiresAt: number
}

function identityFor(userId: string, message: MessageIdentity) {
  // Persisted snapshots and IPC payloads are runtime data even when callers
  // carry TypeScript types. A malformed identity must fail open before trim.
  if (typeof userId !== 'string' || !message || typeof message !== 'object'
    || typeof message.id !== 'string'
    || (message.createTime !== undefined && typeof message.createTime !== 'string')) return null
  const owner = userId.trim()
  const id = message.id.trim()
  return owner && id
    ? { userId: owner, id, createTime: message.createTime?.trim() || '' }
    : null
}

function sameMessage(record: DismissalRecord, identity: ReturnType<typeof identityFor>) {
  // The server ID is stable across WebSocket and polling. Timestamp formatting
  // (or its omission in one transport) must not revive a closed notification.
  return identity !== null
    && record.userId === identity.userId
    && record.id === identity.id
}

function readRecords(now: number): { records: DismissalRecord[]; changed: boolean } {
  const raw = localStorage.getItem(STORAGE_KEY)
  if (!raw) return { records: [], changed: false }
  const stored: unknown = JSON.parse(raw)
  if (!Array.isArray(stored)) return { records: [], changed: true }
  const valid = stored.filter((value): value is DismissalRecord => {
    if (typeof value !== 'object' || value === null) return false
    const record = value as Partial<DismissalRecord>
    return typeof record.userId === 'string' && Boolean(record.userId)
      && typeof record.id === 'string' && Boolean(record.id)
      && typeof record.createTime === 'string'
      && typeof record.expiresAt === 'number' && Number.isFinite(record.expiresAt)
      && record.expiresAt > now
  })
  const records = valid.slice(-MAX_DISMISSALS)
  return { records, changed: records.length !== stored.length }
}

function writeRecords(records: DismissalRecord[]) {
  if (records.length) localStorage.setItem(STORAGE_KEY, JSON.stringify(records))
  else localStorage.removeItem(STORAGE_KEY)
}

/** A local close survives restart even when the server read receipt fails. */
export function dismissSysMessage(userId: string, message: MessageIdentity, expiresAt: number) {
  const identity = identityFor(userId, message)
  const now = Date.now()
  if (!identity || !Number.isFinite(expiresAt)) return false
  const expiry = Math.min(expiresAt, resolveSysMessageExpiresAt(identity.createTime, now))
  if (expiry <= now) return false
  try {
    const { records } = readRecords(now)
    const existing = records.find((record) => sameMessage(record, identity))
    const remaining = records.filter((record) => !sameMessage(record, identity))
    // Repeated close/replay must not extend the original reminder lifetime.
    remaining.push({
      ...identity,
      createTime: existing?.createTime ?? identity.createTime,
      expiresAt: Math.min(expiry, existing?.expiresAt ?? expiry),
    })
    writeRecords(remaining.slice(-MAX_DISMISSALS))
    return true
  } catch {
    // Unavailable/corrupt storage must never hide a message or block a close.
    return false
  }
}

export function isSysMessageDismissed(userId: string, message: MessageIdentity, now = Date.now()) {
  const identity = identityFor(userId, message)
  if (!identity || !Number.isFinite(now)) return false
  try {
    const { records, changed } = readRecords(now)
    if (changed) writeRecords(records)
    return records.some((record) => sameMessage(record, identity))
  } catch {
    return false
  }
}

export function clearSysMessageDismissals(userId?: string) {
  try {
    if (userId === undefined) localStorage.removeItem(STORAGE_KEY)
    else {
      const { records } = readRecords(Date.now())
      writeRecords(records.filter((record) => record.userId !== userId.trim()))
    }
  } catch { /* Storage may be unavailable. */ }
}
