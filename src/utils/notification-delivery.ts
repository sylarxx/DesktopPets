export interface NotificationDelivery<T> {
  generation: number
  presentation: T | null
}

export type NotificationDeliveryFailurePhase = 'layout' | 'show' | 'paint'

interface NotificationDeliveryOptions<T> {
  nextGeneration: () => number
  key: (presentation: T) => string
  publish: (delivery: NotificationDelivery<T>) => Promise<void>
  show: (generation: number, presentation: T) => Promise<boolean>
  confirmVisible: (generation: number) => Promise<void>
  hide: (generation: number) => Promise<boolean>
  onVisible: (presentation: T | null) => void
  onStopped?: (presentation: T) => void
  onAttemptFailed?: (phase: NotificationDeliveryFailurePhase, generation: number) => void
}

// A message's recovery budget survives duplicate events, content updates and
// temporary hides. Only a distinct recovery event or session reset opens a new
// bounded round. A confirmed card's native visibility loss permits a fresh
// bounded show; duplicate ready/hide events cannot restart an unfinished round.
export function createNotificationDelivery<T>(options: NotificationDeliveryOptions<T>) {
  const budgets = new Map<string, { attempts: number; deadline: number; stopped: boolean; complete: boolean }>()
  let intent = 0
  let disposed = false
  let suspended = false
  const recoveryEvents = new Set<string>()
  let latest: T | null = null
  let activeKey = ''
  let fingerprint = ''
  let visible = false
  let shownGeneration = 0
  let revokedThroughGeneration = 0
  let retryTimer: ReturnType<typeof setTimeout> | undefined
  let deadlineTimer: ReturnType<typeof setTimeout> | undefined
  let pending: { generation: number; phase: 'layout' | 'paint'; finish: (ready: boolean) => void } | undefined
  let activeAttempt: { token: number; generation: number; phase: NotificationDeliveryFailurePhase; reported: boolean } | undefined

  function cancel() {
    intent += 1
    if (retryTimer !== undefined) clearTimeout(retryTimer)
    if (deadlineTimer !== undefined) clearTimeout(deadlineTimer)
    retryTimer = deadlineTimer = undefined
    pending?.finish(false)
    activeAttempt = undefined
  }
  function isCurrent(token: number) { return !disposed && token === intent }
  function acknowledge(generation: number) {
    if (pending?.generation === generation && pending.phase === 'layout') pending.finish(true)
  }
  function acknowledgeVisible(generation: number) {
    if (generation > revokedThroughGeneration && pending?.generation === generation && pending.phase === 'paint') pending.finish(true)
  }
  function revokeVisibility(generation: number) {
    if (disposed) return false
    revokedThroughGeneration = Math.max(revokedThroughGeneration, generation)
    if (generation < shownGeneration) return false
    const wasVisible = visible
    visible = false
    options.onVisible(null)
    if (pending?.phase === 'paint' && generation >= pending.generation) pending.finish(false)
    return wasVisible
  }
  function waitForReceipt(generation: number, phase: 'layout' | 'paint', publish: () => Promise<void>) {
    return new Promise<boolean>((resolve) => {
      const timeout = setTimeout(() => finish(false), 1500)
      const finish = (ready: boolean) => {
        if (pending?.generation !== generation || pending.phase !== phase) return
        clearTimeout(timeout)
        pending = undefined
        resolve(ready)
      }
      pending = { generation, phase, finish }
      void Promise.resolve().then(publish).catch(() => finish(false))
    })
  }
  function stop(token: number) {
    if (!isCurrent(token)) return
    reportAttemptFailed(token)
    const budget = budgets.get(activeKey)
    const alreadyStopped = budget?.stopped
    if (budget) budget.stopped = true
    cancel()
    visible = false
    options.onVisible(null)
    // Invalidate a native show still in flight; hiding never needs renderer ACK.
    void options.hide(options.nextGeneration()).catch(() => {})
    if (!alreadyStopped && latest !== null) options.onStopped?.(latest)
  }
  function reportAttemptFailed(token: number) {
    if (!isCurrent(token) || activeAttempt?.token !== token || activeAttempt.reported) return
    activeAttempt.reported = true
    try { options.onAttemptFailed?.(activeAttempt.phase, activeAttempt.generation) } catch {
      // Optional diagnostics must not interrupt bounded retries.
    }
  }
  async function attempt(token: number) {
    if (!isCurrent(token) || latest === null) return
    const budget = budgets.get(activeKey)!
    if (budget.stopped || budget.attempts >= 3 || Date.now() >= budget.deadline) {
      stop(token)
      return
    }
    budget.attempts += 1
    const presentation = latest
    const generation = options.nextGeneration()
    activeAttempt = { token, generation, phase: 'layout', reported: false }
    let success = false
    try {
      const ready = await waitForReceipt(generation, 'layout', () => options.publish({ generation, presentation }))
      if (!isCurrent(token)) return
      if (ready) {
        activeAttempt.phase = 'show'
        shownGeneration = generation
        success = await options.show(generation, presentation)
        if (!isCurrent(token)) return
        if (success) {
          activeAttempt.phase = 'paint'
          success = await waitForReceipt(generation, 'paint', () => options.confirmVisible(generation))
          success = success && generation > revokedThroughGeneration
        }
      }
    } catch { /* Retry only within this message's original budget. */ }
    if (!isCurrent(token)) return
    if (success) {
      activeAttempt = undefined
      if (deadlineTimer !== undefined) clearTimeout(deadlineTimer)
      deadlineTimer = undefined
      budget.complete = true
      visible = true
      options.onVisible(latest)
      // Content may have changed during layout/show. Keep the component and its
      // scroll position; no repeated native show or animation restart.
      if (latest !== presentation) publishUpdate(token)
      return
    }
    reportAttemptFailed(token)
    activeAttempt = undefined
    visible = false
    options.onVisible(null)
    // A shown HWND with no current renderer receipt must not remain as an
    // invisible input blocker while the next bounded attempt is prepared.
    if (budget.attempts < 3) void options.hide(options.nextGeneration()).catch(() => {})
    if (budget.attempts >= 3) stop(token)
    else retryTimer = setTimeout(() => { void attempt(token) }, 1000)
  }
  function publishUpdate(token: number) {
    const generation = options.nextGeneration()
    void options.publish({ generation, presentation: latest }).catch(() => stop(token))
  }
  const api = {
    acknowledge,
    acknowledgeVisible,
    revokeVisibility,
    sync(presentation: T | null) {
      if (disposed) return
      if (suspended) { latest = presentation; return }
      const nextKey = presentation === null ? '' : options.key(presentation)
      const nextFingerprint = JSON.stringify(presentation)
      const existingBudget = budgets.get(nextKey)
      const canRestoreRevoked = presentation !== null && !visible
        && existingBudget?.complete && !existingBudget.stopped
      if (nextKey === activeKey && nextFingerprint === fingerprint && !canRestoreRevoked) return
      fingerprint = nextFingerprint
      latest = presentation
      if (nextKey === activeKey && presentation !== null) {
        if (visible) { publishUpdate(intent); options.onVisible(presentation); return }
        if (!canRestoreRevoked) return
      }
      cancel()
      activeKey = nextKey
      visible = false
      options.onVisible(null)
      const token = intent
      if (presentation === null) {
        const generation = options.nextGeneration()
        void options.hide(generation).catch(() => {}).finally(() => {
          if (isCurrent(token)) void options.publish({ generation, presentation: null }).catch(() => {})
        })
        return
      }
      let budget = budgets.get(nextKey)
      if (!budget) {
        if (budgets.size >= 256) budgets.delete(budgets.keys().next().value!)
        budget = { attempts: 0, deadline: Date.now() + 10_000, stopped: false, complete: false }
        budgets.set(nextKey, budget)
      }
      if (budget.complete && !budget.stopped) {
        budget.attempts = 0
        budget.deadline = Date.now() + 10_000
        budget.complete = false
      }
      if (budget.stopped || Date.now() >= budget.deadline) { stop(token); return }
      deadlineTimer = setTimeout(() => stop(token), Math.max(0, budget.deadline - Date.now()))
      void attempt(token)
    },
    suspend() {
      if (disposed || suspended) return
      suspended = true
      cancel()
      activeKey = fingerprint = ''
      visible = false
      options.onVisible(null)
      void options.hide(options.nextGeneration()).catch(() => {})
    },
    recover(eventId: string, presentation: T | null) {
      if (disposed || !eventId || recoveryEvents.has(eventId)) return false
      recoveryEvents.add(eventId)
      if (recoveryEvents.size > 64) recoveryEvents.delete(recoveryEvents.values().next().value!)
      cancel()
      budgets.clear()
      suspended = false
      activeKey = fingerprint = ''
      visible = false
      options.onVisible(null)
      api.sync(presentation)
      return true
    },
    reset() {
      cancel()
      budgets.clear()
      activeKey = fingerprint = ''
      latest = null
      visible = false
      options.onVisible(null)
      void options.hide(options.nextGeneration()).catch(() => {})
    },
    dispose() { cancel(); disposed = true; budgets.clear(); recoveryEvents.clear() },
  }
  return api
}
