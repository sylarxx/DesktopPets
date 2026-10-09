interface PaintReceiptOptions {
  requestFrame: (callback: FrameRequestCallback) => number
  cancelFrame: (frame: number) => void
  canConfirm: (generation: number) => boolean
  confirm: (generation: number) => Promise<unknown>
}

// Layout while hidden prepares the card. Only frames scheduled after native
// show may acknowledge its presentation; a replacement cancels old receipts.
export function createNotificationPaintReceipt(options: PaintReceiptOptions) {
  let frame: number | undefined
  let intent = 0
  function cancel() {
    intent += 1
    if (frame !== undefined) options.cancelFrame(frame)
    frame = undefined
  }
  return {
    cancel,
    request(generation: number) {
      cancel()
      const token = intent
      frame = options.requestFrame(() => {
        frame = options.requestFrame(() => {
          frame = undefined
          if (token !== intent || !options.canConfirm(generation)) return
          void options.confirm(generation).catch(() => {})
        })
      })
    },
  }
}
