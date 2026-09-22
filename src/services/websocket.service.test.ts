import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { TaskCreatedEvent } from '../types/task'

const mockEnv = vi.hoisted(() => ({
  enableMock: false,
  wsUrl: 'ws://desktop.test/tasks',
}))

vi.mock('../utils/env', () => ({ env: mockEnv }))

type FakeSocketEvent = Event & { data?: string }
type FakeSocketListener = (event: FakeSocketEvent) => void

class FakeWebSocket {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSING = 2
  static readonly CLOSED = 3
  static instances: FakeWebSocket[] = []
  static failNextConstruction = false

  readonly url: string
  readyState = FakeWebSocket.CONNECTING
  closeCalls = 0
  private listeners = new Map<string, FakeSocketListener[]>()

  constructor(url: string) {
    if (FakeWebSocket.failNextConstruction) {
      FakeWebSocket.failNextConstruction = false
      throw new TypeError('invalid websocket endpoint')
    }
    this.url = url
    FakeWebSocket.instances.push(this)
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject) {
    const callback = typeof listener === 'function'
      ? listener as FakeSocketListener
      : (event: FakeSocketEvent) => listener.handleEvent(event)
    const listeners = this.listeners.get(type) ?? []
    listeners.push(callback)
    this.listeners.set(type, listeners)
  }

  close() {
    this.closeCalls += 1
    this.readyState = FakeWebSocket.CLOSED
    this.dispatch('close')
  }

  open() {
    this.readyState = FakeWebSocket.OPEN
    this.dispatch('open')
  }

  message(payload: unknown) {
    this.dispatch('message', { data: JSON.stringify(payload) })
  }

  serverClose() {
    this.readyState = FakeWebSocket.CLOSED
    this.dispatch('close')
  }

  error() {
    this.dispatch('error')
  }

  private dispatch(type: string, extra: Partial<FakeSocketEvent> = {}) {
    const event = Object.assign(new Event(type), extra) as FakeSocketEvent
    this.listeners.get(type)?.forEach((listener) => listener(event))
  }
}

function taskEvent(eventId: string): TaskCreatedEvent {
  return {
    eventId,
    eventType: 'task.created',
    timestamp: '2026-08-13 09:00:00',
    payload: {
      taskId: `task-${eventId}`,
      title: `Task ${eventId}`,
    },
  }
}

type WebsocketService = typeof import('./websocket.service')['websocketService']
let service: WebsocketService | undefined

describe('websocketService connection generations', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.resetModules()
    FakeWebSocket.instances = []
    FakeWebSocket.failNextConstruction = false
    vi.stubGlobal('WebSocket', FakeWebSocket)
    vi.stubGlobal('window', {
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    })
  })

  afterEach(() => {
    service?.disconnect()
    service = undefined
    vi.unstubAllGlobals()
    vi.useRealTimers()
  })

  it('replaces an apparently open overnight connection without accepting its late events', async () => {
    service = (await import('./websocket.service')).websocketService
    const listener = vi.fn()
    const remove = service.onTask(listener)
    service.connect()
    const old = FakeWebSocket.instances[0]
    old.open()
    service.connect({ force: true })
    const fresh = FakeWebSocket.instances[1]
    fresh.open()
    old.message(taskEvent('stale'))
    old.serverClose()
    fresh.message(taskEvent('fresh'))
    await vi.advanceTimersByTimeAsync(4000)
    expect(old.closeCalls).toBe(1)
    expect(FakeWebSocket.instances).toHaveLength(2)
    expect(listener).toHaveBeenCalledExactlyOnceWith(taskEvent('fresh'))
    remove()
  })

  it('ignores message, close and error callbacks from a disconnected socket after a new connect', async () => {
    service = (await import('./websocket.service')).websocketService
    const taskListener = vi.fn()
    const statusListener = vi.fn()
    const removeTaskListener = service.onTask(taskListener)
    const removeStatusListener = service.onStatus(statusListener)

    service.connect()
    const oldSocket = FakeWebSocket.instances[0]
    oldSocket.open()

    service.disconnect()
    service.connect()
    const currentSocket = FakeWebSocket.instances[1]
    currentSocket.open()

    oldSocket.message(taskEvent('stale'))
    oldSocket.serverClose()
    oldSocket.error()
    await vi.advanceTimersByTimeAsync(3_000)

    expect(taskListener).not.toHaveBeenCalled()
    expect(FakeWebSocket.instances).toHaveLength(2)
    expect(statusListener.mock.calls.filter(([status]) => status === 'reconnecting')).toHaveLength(0)

    currentSocket.message(taskEvent('current'))
    expect(taskListener).toHaveBeenCalledOnce()
    expect(taskListener).toHaveBeenCalledWith(taskEvent('current'))
    expect(currentSocket.closeCalls).toBe(0)

    removeTaskListener()
    removeStatusListener()
  })

  it('schedules only one reconnect when error and duplicate close callbacks race', async () => {
    service = (await import('./websocket.service')).websocketService
    const statusListener = vi.fn()
    const removeStatusListener = service.onStatus(statusListener)

    service.connect()
    const failedSocket = FakeWebSocket.instances[0]
    failedSocket.open()

    failedSocket.error()
    failedSocket.serverClose()

    expect(failedSocket.closeCalls).toBe(1)
    expect(statusListener.mock.calls.filter(([status]) => status === 'reconnecting')).toHaveLength(1)

    await vi.advanceTimersByTimeAsync(2_999)
    expect(FakeWebSocket.instances).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(FakeWebSocket.instances).toHaveLength(2)

    await vi.advanceTimersByTimeAsync(3_000)
    expect(FakeWebSocket.instances).toHaveLength(2)

    removeStatusListener()
  })

  it('contains a synchronous socket construction failure and retries without escaping', async () => {
    service = (await import('./websocket.service')).websocketService
    const statusListener = vi.fn()
    const removeStatusListener = service.onStatus(statusListener)
    FakeWebSocket.failNextConstruction = true

    expect(() => service?.connect()).not.toThrow()
    expect(statusListener).toHaveBeenCalledWith('reconnecting')
    expect(FakeWebSocket.instances).toHaveLength(0)

    await vi.advanceTimersByTimeAsync(3_000)
    expect(FakeWebSocket.instances).toHaveLength(1)

    removeStatusListener()
  })
})
