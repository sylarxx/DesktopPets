// Small local snapshots survive a WebView reload/recreation. They contain no
// credentials and are never diagnostics. Each window writes only its own key.
const PREFIX = 'huali_ai_recovery_v1_'
type Owner = 'mascot' | 'panel'

export function readRecoverySnapshot<T>(owner: Owner, userId: string): T | null {
  if (!userId) return null
  try {
    const record = JSON.parse(localStorage.getItem(PREFIX + owner) || 'null')
    return record?.userId === userId ? record.value as T : null
  } catch { return null }
}

export function writeRecoverySnapshot(owner: Owner, userId: string, value: unknown) {
  if (!userId) return
  try { localStorage.setItem(PREFIX + owner, JSON.stringify({ userId, value })) } catch {
    // Storage limits must never prevent a click, queue transition or recovery.
  }
}

export function clearRecoverySnapshots() {
  try {
    localStorage.removeItem(PREFIX + 'mascot')
    localStorage.removeItem(PREFIX + 'panel')
  } catch { /* Storage may be unavailable. */ }
}
