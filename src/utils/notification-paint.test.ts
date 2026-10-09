import { describe, expect, it, vi } from 'vitest'
import { createNotificationPaintReceipt } from './notification-paint'

function setup() {
  const frames = new Map<number, FrameRequestCallback>()
  let sequence = 0
  const canConfirm = vi.fn(() => true)
  const confirm = vi.fn(async (_generation: number) => true)
  const receipt = createNotificationPaintReceipt({
    requestFrame(callback) { frames.set(++sequence, callback); return sequence },
    cancelFrame(frame) { frames.delete(frame) },
    canConfirm, confirm,
  })
  const frame = () => {
    const pending = [...frames.values()]
    frames.clear()
    pending.forEach(callback => callback(0))
  }
  return { receipt, frame, frames, canConfirm, confirm }
}

describe('post-show notification frame receipt', () => {
  it('confirms the current card only after two scheduled frames', () => {
    const h = setup()
    h.receipt.request(12)
    h.frame()
    expect(h.confirm).not.toHaveBeenCalled()
    h.frame()
    expect(h.confirm).toHaveBeenCalledWith(12)
    expect(h.frames.size).toBe(0)
  })

  it('cancels a suspended receipt on hide, replacement or disposal', () => {
    const h = setup()
    h.receipt.request(12)
    h.frame()
    h.receipt.request(13)
    h.frame()
    h.frame()
    expect(h.confirm).toHaveBeenCalledExactlyOnceWith(13)
    h.receipt.request(14)
    h.frame()
    h.receipt.cancel()
    h.frame()
    expect(h.confirm).toHaveBeenCalledOnce()
    expect(h.frames.size).toBe(0)
  })

  it('does not confirm a stale generation or card without layout', () => {
    const h = setup()
    h.receipt.request(12)
    h.canConfirm.mockReturnValue(false)
    h.frame()
    h.frame()
    expect(h.confirm).not.toHaveBeenCalled()
  })
})
