import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { RuntimeState } from './runtime-recovery.service'

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), unlisten: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke, isTauri: () => true }))
vi.mock('@tauri-apps/api/event', () => ({ listen: mocks.listen }))

describe('native renderer recovery bridge', () => {
  let target: EventTarget
  let nativeEvent: (event: { payload: RuntimeState }) => void
  let frames: Map<number, FrameRequestCallback>
  let frameNumber: number
  const active = { epoch: 1, interactive: true, recovered: false, visible: true, deliveryGeneration: 100 }
  const dispatch = (epoch = 1, paint = true) => target.dispatchEvent(
    new CustomEvent('desktop-runtime-probe', { detail: { ...active, epoch, sequence: 3, paint } }),
  )
  const paintFrame = () => {
    const pending = [...frames.values()]
    frames.clear()
    pending.forEach(callback => callback(0))
  }

  beforeEach(() => {
    vi.resetModules()
    mocks.invoke.mockReset().mockResolvedValue(active)
    mocks.listen.mockReset().mockImplementation(async (_event, handler) => {
      nativeEvent = handler
      return mocks.unlisten
    })
    mocks.unlisten.mockReset()
    target = new EventTarget()
    frames = new Map()
    frameNumber = 0
    vi.stubGlobal('window', target)
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
      frames.set(++frameNumber, callback)
      return frameNumber
    })
    vi.stubGlobal('cancelAnimationFrame', (id: number) => frames.delete(id))
  })
  afterEach(() => vi.unstubAllGlobals())

  it('acknowledges JS immediately and requires two frames for a paint receipt', async () => {
    const { startRuntimeRecovery } = await import('./runtime-recovery.service')
    const state = vi.fn()
    const dispose = await startRuntimeRecovery(state)
    dispatch()
    expect(mocks.invoke).toHaveBeenLastCalledWith('desktop_runtime_ack', expect.objectContaining({ painted: false }))
    paintFrame()
    expect(mocks.invoke.mock.calls.filter(([name, args]) => name === 'desktop_runtime_ack' && args.painted)).toHaveLength(0)
    paintFrame()
    expect(mocks.invoke).toHaveBeenLastCalledWith('desktop_runtime_ack', expect.objectContaining({ painted: true }))
    expect(state).toHaveBeenCalledOnce()
    dispose()
  })

  it('applies a lock/unlock once per epoch and discards delayed old probes', async () => {
    const { startRuntimeRecovery } = await import('./runtime-recovery.service')
    const state = vi.fn()
    const dispose = await startRuntimeRecovery(state)
    nativeEvent({ payload: { ...active, epoch: 2, interactive: false } })
    nativeEvent({ payload: { ...active, epoch: 3 } })
    const calls = mocks.invoke.mock.calls.length
    dispatch(2)
    nativeEvent({ payload: { ...active, epoch: 2, interactive: false } })
    nativeEvent({ payload: { ...active, epoch: 3 } })
    expect(state.mock.calls.map(([s]) => [s.epoch, s.interactive])).toEqual([[1, true], [2, false], [3, true]])
    expect(mocks.invoke).toHaveBeenCalledTimes(calls)
    dispose()
  })

  it('cancels queued frame receipts and all events on disposal', async () => {
    const { startRuntimeRecovery } = await import('./runtime-recovery.service')
    const dispose = await startRuntimeRecovery(vi.fn())
    target.dispatchEvent(new Event('online'))
    expect(mocks.invoke).toHaveBeenLastCalledWith('request_runtime_resync')
    dispatch()
    paintFrame()
    dispose()
    const calls = mocks.invoke.mock.calls.length
    paintFrame()
    target.dispatchEvent(new Event('online'))
    dispatch()
    expect(mocks.invoke).toHaveBeenCalledTimes(calls)
    expect(mocks.unlisten).toHaveBeenCalledOnce()
  })

  it('retries a lost attachment and restores the already completed mount', async () => {
    const { startRuntimeRecovery, markRuntimeMounted } = await import('./runtime-recovery.service')
    mocks.invoke.mockRejectedValueOnce(new Error('startup IPC lost'))
    const dispose = await startRuntimeRecovery(vi.fn())
    markRuntimeMounted()
    dispatch()
    await Promise.resolve()
    await Promise.resolve()
    expect(mocks.invoke.mock.calls.filter(([name]) => name === 'desktop_runtime_ready')).toHaveLength(2)
    expect(mocks.invoke).toHaveBeenLastCalledWith('desktop_runtime_mounted', expect.objectContaining({ instance: expect.any(String) }))
    dispose()
  })
})
