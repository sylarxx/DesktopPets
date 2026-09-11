import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createNotificationDelivery, type NotificationDelivery } from './notification-delivery'

function setup() {
  let generation = 0
  const publish = vi.fn(async (_delivery: NotificationDelivery<string>) => {})
  const show = vi.fn(async () => true)
  const hide = vi.fn(async () => true)
  const onVisible = vi.fn()
  const onStopped = vi.fn()
  const delivery = createNotificationDelivery<string>({
    nextGeneration: () => ++generation, key: (value) => value.split(':')[0],
    publish, show, hide, onVisible, onStopped,
  })
  return { delivery, publish, show, hide, onVisible, onStopped }
}

describe('bounded notification delivery', () => {
  beforeEach(() => vi.useFakeTimers())
  afterEach(() => vi.useRealTimers())

  it('requires the matching layout ACK and leaves a successful HWND alone', async () => {
    const h = setup()
    h.delivery.sync('meeting')
    h.delivery.acknowledge(0)
    await vi.advanceTimersByTimeAsync(0)
    expect(h.show).not.toHaveBeenCalled()
    h.delivery.acknowledge(1)
    await vi.advanceTimersByTimeAsync(60_000)
    expect(h.show).toHaveBeenCalledWith(1, 'meeting')
    expect(h.onVisible).toHaveBeenLastCalledWith('meeting')
    expect(vi.getTimerCount()).toBe(0)
    h.delivery.dispose()
  })

  it('stops after two remedies; late ACKs, duplicate updates and other messages cannot revive it', async () => {
    const h = setup()
    h.delivery.sync('meeting')
    await vi.advanceTimersByTimeAsync(10_000)
    expect(h.publish).toHaveBeenCalledTimes(3)
    expect(h.onStopped).toHaveBeenCalledTimes(1)
    h.delivery.acknowledge(3)
    h.delivery.sync('meeting:enriched')
    h.delivery.sync(null)
    h.delivery.sync('new')
    h.delivery.sync('meeting:new-count')
    await vi.advanceTimersByTimeAsync(60_000)
    expect(h.show).not.toHaveBeenCalled()
    expect(h.publish.mock.calls.filter(([d]) => d.presentation?.startsWith('meeting'))).toHaveLength(3)
    expect(vi.getTimerCount()).toBe(0)
    h.delivery.dispose()
  })

  it('bounds a hung native show by the whole-round deadline', async () => {
    const h = setup()
    let finish!: (value: boolean) => void
    h.show.mockImplementationOnce(() => new Promise(resolve => { finish = resolve }))
    h.delivery.sync('meeting')
    h.delivery.acknowledge(1)
    await vi.advanceTimersByTimeAsync(10_000)
    expect(h.onStopped).toHaveBeenCalledTimes(1)
    expect(h.hide).toHaveBeenCalledWith(2)
    finish(true)
    await vi.advanceTimersByTimeAsync(0)
    expect(h.onVisible).not.toHaveBeenCalledWith('meeting')
    expect(vi.getTimerCount()).toBe(0)
    h.delivery.dispose()
  })

  it('honors hide and session reset while an old ACK or retry is outstanding', async () => {
    const h = setup()
    h.delivery.sync('meeting')
    h.delivery.sync(null)
    h.delivery.acknowledge(1)
    await vi.advanceTimersByTimeAsync(10_000)
    expect(h.show).not.toHaveBeenCalled()
    h.delivery.reset()
    h.delivery.sync('meeting')
    h.delivery.acknowledge(4)
    await vi.advanceTimersByTimeAsync(0)
    expect(h.onVisible).toHaveBeenLastCalledWith('meeting')
    h.delivery.dispose()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('updates visible content without a native show or resetting its component', async () => {
    const h = setup()
    h.delivery.sync('meeting')
    h.delivery.acknowledge(1)
    await vi.advanceTimersByTimeAsync(0)
    h.delivery.sync('meeting:enriched')
    h.delivery.sync('meeting:enriched')
    await vi.advanceTimersByTimeAsync(0)
    expect(h.publish).toHaveBeenLastCalledWith({ generation: 2, presentation: 'meeting:enriched' })
    expect(h.show).toHaveBeenCalledTimes(1)
    h.delivery.dispose()
  })

  it('restores a healthy card after a menu closes without periodic recovery', async () => {
    const h = setup()
    h.delivery.sync('meeting')
    h.delivery.acknowledge(1)
    await vi.advanceTimersByTimeAsync(20_000)
    h.delivery.sync(null)
    await vi.advanceTimersByTimeAsync(0)
    h.delivery.sync('meeting')
    h.delivery.acknowledge(3)
    await vi.advanceTimersByTimeAsync(0)
    expect(h.onVisible).toHaveBeenLastCalledWith('meeting')
    expect(h.onStopped).not.toHaveBeenCalled()
    h.delivery.dispose()
  })
})
