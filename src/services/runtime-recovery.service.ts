import { invoke, isTauri } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export interface RuntimeState {
  epoch: number
  interactive: boolean
  recovered: boolean
  visible: boolean
}
export interface RuntimeProbe extends RuntimeState { sequence: number; paint: boolean }
export const RUNTIME_RECOVERY_EVENT = 'desktop-runtime-state'
const PROBE_EVENT = 'desktop-runtime-probe'
const INSTANCE = `${Date.now()}-${Math.random().toString(36).slice(2)}`

export async function startRuntimeRecovery(onState: (state: RuntimeState) => void) {
  if (!isTauri()) return () => {}
  let disposed = false
  let latestEpoch = -1
  let frame: number | undefined
  const apply = (state: RuntimeState) => {
    if (disposed || state.epoch <= latestEpoch) return
    latestEpoch = state.epoch
    onState(state)
  }
  const probe = (event: Event) => {
    const payload = (event as CustomEvent<RuntimeProbe>).detail
    if (!payload || disposed) return
    apply(payload)
    const ack = (painted: boolean) => {
      if (!disposed) void invoke('desktop_runtime_ack', {
        instance: INSTANCE, epoch: payload.epoch, sequence: payload.sequence, painted,
      }).catch(() => {})
    }
    ack(false)
    if (frame !== undefined) cancelAnimationFrame(frame)
    if (payload.paint) frame = requestAnimationFrame(() => {
      frame = requestAnimationFrame(() => { frame = undefined; ack(true) })
    })
  }
  window.addEventListener(PROBE_EVENT, probe)
  let unlisten: (() => void) | undefined
  try {
    unlisten = await listen<RuntimeState>(RUNTIME_RECOVERY_EVENT, ({ payload }) => apply(payload))
    const state = await invoke<RuntimeState>('desktop_runtime_ready', { instance: INSTANCE })
    apply(state)
  } catch {
    // A later native probe carries the current state and retries attachment.
  }
  // Attach again after a lost startup IPC. Identity does not reset recovery
  // budgets on repeated probes; it only identifies this renderer lifetime.
  const attachOnProbe = () => {
    void invoke<RuntimeState>('desktop_runtime_ready', { instance: INSTANCE })
      .then(apply).catch(() => {})
  }
  if (latestEpoch < 0) window.addEventListener(PROBE_EVENT, attachOnProbe, { once: true })
  return () => {
    disposed = true
    unlisten?.()
    if (frame !== undefined) cancelAnimationFrame(frame)
    window.removeEventListener(PROBE_EVENT, probe)
    window.removeEventListener(PROBE_EVENT, attachOnProbe)
  }
}

export function requestNotificationRecovery() {
  if (isTauri()) void invoke('request_notification_recovery').catch(() => {})
}

export function markRuntimeMounted() {
  if (isTauri()) void invoke('desktop_runtime_mounted', { instance: INSTANCE }).catch(() => {})
}
