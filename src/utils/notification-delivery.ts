export interface NotificationDelivery<T> {
  generation: number
  presentation: T | null
}

interface NotificationDeliveryOptions<T> {
  nextGeneration: () => number
  key: (presentation: T) => string
  publish: (delivery: NotificationDelivery<T>) => Promise<void>
  show: (generation: number, presentation: T) => Promise<boolean>
  hide: (generation: number) => Promise<boolean>
  onVisible: (presentation: T | null) => void
  onStopped?: () => void
}

// A message's recovery budget survives duplicate events, content updates and
// temporary hides. Only an explicit session reset discards terminal failures.
export function createNotificationDelivery<T>(options: NotificationDeliveryOptions<T>) {
  const budgets = new Map<string, { attempts: number; deadline: number; stopped: boolean; complete: boolean }>()
  let intent = 0
  let disposed = false
  let latest: T | null = null
  let activeKey = ''
  let fingerprint = ''
  let visible = false
  let retryTimer: ReturnType<typeof setTimeout> | undefined
  let deadlineTimer: ReturnType<typeof setTimeout> | undefined
  let pending: { generation: number; finish: (ready: boolean) => void } | undefined

  function cancel() {
    intent += 1
    if (retryTimer !== undefined) clearTimeout(retryTimer)
    if (deadlineTimer !== undefined) clearTimeout(deadlineTimer)
    retryTimer = deadlineTimer = undefined
    pending?.finish(false)
  }
  function isCurrent(token: number) { return !disposed && token === intent }
  function acknowledge(generation: number) {
    if (pending?.generation === generation) pending.finish(true)
  }
  function waitForLayout(delivery: NotificationDelivery<T>) {
    return new Promise<boolean>((resolve) => {
      const timeout = setTimeout(() => finish(false), 1500)
      const finish = (ready: boolean) => {
        if (pending?.generation !== delivery.generation) return
        clearTimeout(timeout)
        pending = undefined
        resolve(ready)
      }
      pending = { generation: delivery.generation, finish }
      void Promise.resolve().then(() => options.publish(delivery)).catch(() => finish(false))
    })
  }
  function stop(token: number) {
    if (!isCurrent(token)) return
    const budget = budgets.get(activeKey)
    const alreadyStopped = budget?.stopped
    if (budget) budget.stopped = true
    cancel()
    visible = false
    options.onVisible(null)
    // Invalidate a native show still in flight; hiding never needs renderer ACK.
    void options.hide(options.nextGeneration()).catch(() => {})
    if (!alreadyStopped) options.onStopped?.()
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
    let success = false
    try {
      const ready = await waitForLayout({ generation, presentation })
      if (!isCurrent(token)) return
      if (ready) success = await options.show(generation, presentation)
    } catch { /* Retry only within this message's original budget. */ }
    if (!isCurrent(token)) return
    if (success) {
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
    visible = false
    options.onVisible(null)
    if (budget.attempts >= 3) stop(token)
    else retryTimer = setTimeout(() => { void attempt(token) }, 1000)
  }
  function publishUpdate(token: number) {
    const generation = options.nextGeneration()
    void options.publish({ generation, presentation: latest }).catch(() => stop(token))
  }
  return {
    acknowledge,
    sync(presentation: T | null) {
      if (disposed) return
      const nextKey = presentation === null ? '' : options.key(presentation)
      const nextFingerprint = JSON.stringify(presentation)
      if (nextKey === activeKey && nextFingerprint === fingerprint) return
      fingerprint = nextFingerprint
      latest = presentation
      if (nextKey === activeKey && presentation !== null) {
        if (visible) { publishUpdate(intent); options.onVisible(presentation) }
        return
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
    reset() {
      cancel()
      budgets.clear()
      activeKey = fingerprint = ''
      latest = null
      visible = false
      options.onVisible(null)
      void options.hide(options.nextGeneration()).catch(() => {})
    },
    dispose() { cancel(); disposed = true; budgets.clear() },
  }
}
