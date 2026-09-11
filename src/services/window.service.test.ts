import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  openUrl: vi.fn(),
  windowOpen: vi.fn(),
  emitTo: vi.fn(),
}))

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('@tauri-apps/api/event', () => ({ emitTo: mocks.emitTo }))
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: mocks.openUrl }))
vi.mock('../utils/env', () => ({
  env: {
    webBaseUrl: 'https://workbench.example.com',
    desktopUserId: '',
    enableMock: false,
    mockUserId: '',
  },
}))
vi.mock('../utils/storage', () => ({
  storage: { getUserInfo: vi.fn(() => null) },
}))

import {
  PANEL_REVEAL_EVENT,
  finishMascotNotificationCollapse,
  hideMascotSystemNotificationWindow,
  isMascotSystemNotificationReady,
  openDesktopLogin,
  openExternal,
  setMascotSystemNotificationReady,
  showMascotSystemNotificationWindow,
  showNotificationWindow,
  showPanelWindow,
  togglePanelWindow,
} from './window.service'

describe('external window opening', () => {
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals() })
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('window', {
      open: mocks.windowOpen,
      dispatchEvent: vi.fn(),
    })
  })

  it('returns true when native code reuses an existing browser tab', async () => {
    mocks.invoke.mockResolvedValue(true)

    await expect(openExternal('https://workbench.example.com/calendar')).resolves.toBe(true)
    expect(mocks.openUrl).not.toHaveBeenCalled()
  })

  it('returns true when the opener launches a new browser tab', async () => {
    mocks.invoke.mockResolvedValue(false)
    mocks.openUrl.mockResolvedValue(undefined)

    await expect(openExternal('https://workbench.example.com/calendar')).resolves.toBe(true)
  })

  it('opens the exact desktop authorization URL without best-effort tab reuse', async () => {
    mocks.openUrl.mockResolvedValue(undefined)

    await expect(openDesktopLogin('desktop-state')).resolves.toBe(true)

    expect(mocks.invoke).not.toHaveBeenCalledWith('open_or_focus_web_url', expect.anything())
    expect(mocks.openUrl).toHaveBeenCalledOnce()
    const openedUrl = new URL(mocks.openUrl.mock.calls[0]?.[0] as string)
    expect(openedUrl.pathname).toBe('/login')
    expect(openedUrl.searchParams.get('from')).toBe('desktop')
    expect(openedUrl.searchParams.get('desktopCallback')).toBe('huali-ai-mascot://auth-callback')
    expect(openedUrl.searchParams.get('state')).toBe('desktop-state')
  })

  it('returns false when native reuse, opener, and browser fallback all fail', async () => {
    mocks.invoke.mockRejectedValue(new Error('not in Tauri'))
    mocks.openUrl.mockRejectedValue(new Error('no opener'))
    mocks.windowOpen.mockReturnValue(null)

    await expect(openExternal('https://workbench.example.com/calendar')).resolves.toBe(false)
  })

  it('returns true and removes opener access when the browser fallback opens', async () => {
    const openedWindow = { opener: {} }
    mocks.invoke.mockRejectedValue(new Error('not in Tauri'))
    mocks.openUrl.mockRejectedValue(new Error('no opener'))
    mocks.windowOpen.mockReturnValue(openedWindow)

    await expect(openExternal('https://workbench.example.com/calendar')).resolves.toBe(true)
    expect(openedWindow.opener).toBeNull()
  })
  it('releases a hung browser handoff without opening a second tab after timeout', async () => {
    vi.useFakeTimers()
    let finish!: (value: boolean) => void
    mocks.invoke.mockImplementationOnce(() => new Promise(resolve => { finish = resolve }))
    const outcome = expect(openExternal('https://workbench.example.com/workbench')).resolves.toBe(false)
    await vi.advanceTimersByTimeAsync(12_000)
    await outcome
    finish(false)
    await vi.advanceTimersByTimeAsync(0)
    expect(mocks.openUrl).not.toHaveBeenCalled()
    expect(mocks.windowOpen).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
  })

})

describe('background task panel reveal', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.stubGlobal('window', { dispatchEvent: vi.fn() })
  })
  afterEach(() => vi.unstubAllGlobals())

  it('returns false and does not emit a reveal when native positioning fails', async () => {
    mocks.invoke.mockResolvedValue(false)
    await expect(showPanelWindow({ focus: false })).resolves.toBe(false)
    expect(mocks.emitTo).not.toHaveBeenCalled()
  })

  it('requests non-activating display for pushed tasks', async () => {
    mocks.invoke.mockResolvedValue(true)
    await expect(showPanelWindow({ focus: false })).resolves.toBe(true)
    expect(mocks.invoke).toHaveBeenCalledWith('show_panel_window', { focus: false })
    expect(mocks.emitTo).toHaveBeenCalledWith('panel', PANEL_REVEAL_EVENT, {
      focus: false,
    })
  })

  it('marks a deliberate mascot toggle as focus-eligible', async () => {
    mocks.invoke.mockResolvedValue(true)
    await expect(togglePanelWindow()).resolves.toBe(true)
    expect(mocks.emitTo).toHaveBeenCalledWith('panel', PANEL_REVEAL_EVENT, { focus: true })
  })

  it('returns the native non-activating reminder show result', async () => {
    mocks.invoke.mockResolvedValue(false)
    await expect(showNotificationWindow()).resolves.toBe(false)
    expect(mocks.invoke).toHaveBeenCalledWith('show_notification_window')
  })

  it('finishes an off-screen notification collapse before returning to the desktop', async () => {
    mocks.invoke.mockResolvedValue(true)
    await expect(finishMascotNotificationCollapse()).resolves.toBe(true)
    expect(mocks.invoke).toHaveBeenCalledWith('finish_mascot_notification_collapse')
  })

  it('routes system-message presentation through its independent native window', async () => {
    mocks.invoke.mockResolvedValue(true)

    await expect(isMascotSystemNotificationReady()).resolves.toBe(true)
    await expect(setMascotSystemNotificationReady()).resolves.toBe(true)
    await expect(showMascotSystemNotificationWindow(true, 7)).resolves.toBe(true)
    await expect(hideMascotSystemNotificationWindow(8)).resolves.toBe(true)

    expect(mocks.invoke).toHaveBeenCalledWith('is_mascot_system_notification_ready')
    expect(mocks.invoke).toHaveBeenCalledWith('set_mascot_system_notification_ready')
    expect(mocks.invoke).toHaveBeenCalledWith('show_mascot_system_notification_window', {
      compact: true,
      clientGeneration: 7,
    })
    expect(mocks.invoke).toHaveBeenCalledWith('hide_mascot_system_notification_window', {
      clientGeneration: 8,
    })
  })
})
