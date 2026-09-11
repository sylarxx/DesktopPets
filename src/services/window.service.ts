import { openUrl } from '@tauri-apps/plugin-opener'
import { invoke } from '@tauri-apps/api/core'
import { emitTo } from '@tauri-apps/api/event'
import { DESKTOP_AUTH_SCHEME } from './desktop-auth.service'
import { recordDesktopDiagnostic } from './diagnostic.service'
import type { SysMessageNotification } from '../types/sys-message'
import { env } from '../utils/env'
import { storage } from '../utils/storage'
import type { NotificationDelivery } from '../utils/notification-delivery'

export const MASCOT_REVEAL_EVENT = 'huali:mascot-reveal'
export const MASCOT_NATIVE_DRAG_ENDED_EVENT = 'mascot-native-drag-ended'
export const MASCOT_NATIVE_REVEALED_EVENT = 'mascot-native-revealed'
export const MASCOT_NATIVE_HOVER_REVEALED_EVENT = 'mascot-native-hover-revealed'
export const PANEL_REVEAL_EVENT = 'huali:panel-reveal'
export const PANEL_ACTIVITY_EVENT = 'huali:panel-activity'
export const PANEL_VISIBILITY_EVENT = 'huali:panel-visibility'
export const PANEL_TASK_STATE_EVENT = 'huali:panel-task-state'
export const PANEL_TASK_STATE_REQUEST_EVENT = 'huali:panel-task-state-request'
export const PANEL_TASK_READY_EVENT = 'huali:panel-task-ready'
export const PANEL_TASK_DELIVERED_EVENT = 'huali:panel-task-delivered'
export const MASCOT_CONTEXT_MENU_VISIBILITY_EVENT = 'mascot-context-menu-visibility'
export const MASCOT_SYSTEM_NOTIFICATION_READY_EVENT = 'mascot-system-notification-ready'
export const MASCOT_SYSTEM_NOTIFICATION_PRESENT_EVENT = 'mascot-system-notification-present'
export const MASCOT_SYSTEM_NOTIFICATION_LAYOUT_EVENT = 'mascot-system-notification-layout'
export const MASCOT_SYSTEM_NOTIFICATION_ACTION_EVENT = 'mascot-system-notification-action'
export type MascotDockSide = 'left' | 'right'

export interface MascotContextMenuPlacement {
  generation: number
  placement: 'above' | 'below'
  /** Horizontal tail position inside the 192px visible menu card. */
  tailX: number
}

export interface MascotMenuVisibilityPayload {
  visible: boolean
  /** Whether closing the menu should restore the card hidden behind it. */
  restoreNotification: boolean
}

export interface MascotAuthPresentation {
  kind: 'auth'
  generation: number
  pending: boolean
  message: string
}

export interface MascotSystemMessagePresentation {
  kind: 'message'
  generation: number
  message: SysMessageNotification
  displayContent: string
  pendingCount: number
  readPending: boolean
  readAllPending: boolean
  actionError: string
}

export type MascotSystemNotificationPresentation =
  | MascotAuthPresentation
  | MascotSystemMessagePresentation

export type MascotSystemNotificationDelivery = NotificationDelivery<MascotSystemNotificationPresentation>

export type MascotSystemNotificationAction =
  | { action: 'login' }
  | { action: 'readAll' }
  | { action: 'read' | 'view'; message: SysMessageNotification }

export interface PanelActivityPayload {
  hasText: boolean
  focused: boolean
}

export interface PanelRevealPayload {
  focus: boolean
}

export interface PanelTaskStatePayload {
  hasTask: boolean
  requestReveal: boolean
  sessionEpoch: number
}

export interface PanelTaskStateRequestPayload {
  sessionEpoch: number
}

export interface PanelTaskDeliveryPayload {
  event: import('../types/task').TaskCreatedEvent
  sessionEpoch: number
}

export interface PanelTaskDeliveredPayload {
  eventId: string
  sessionEpoch: number
}

function announceMascotReveal() {
  window.dispatchEvent(new Event(MASCOT_REVEAL_EVENT))
}

function getWebBaseUrl() {
  return env.webBaseUrl.replace(/\/$/, '')
}

function buildUrl(path: string) {
  return `${getWebBaseUrl()}${path}`
}

function buildQuery(params: Record<string, string | undefined>) {
  const search = new URLSearchParams()
  Object.entries(params).forEach(([key, value]) => {
    if (value) search.set(key, value)
  })

  const query = search.toString()
  return query ? `?${query}` : ''
}

function getDesktopUserId() {
  return storage.getUserInfo()?.userId || env.desktopUserId || (env.enableMock ? env.mockUserId : '')
}

function getDesktopLinkParams() {
  const desktopUserId = getDesktopUserId()
  return {
    from: 'desktop',
    desktopClient: 'huali-ai-mascot',
    desktopUserId: desktopUserId || undefined
  }
}

export async function openExternal(
  url: string,
  options: { reuseExistingTab?: boolean } = {},
): Promise<boolean> {
  let expired = false
  let timer: ReturnType<typeof setTimeout> | undefined
  async function openOnce() {
    if (options.reuseExistingTab !== false) {
      try {
        const reused = await invoke<boolean>('open_or_focus_web_url', {
          url,
          matchUrl: getWebBaseUrl()
        })

        if (expired) return false
        if (reused) return true
      } catch {
        // Browser tab reuse is implemented by the native desktop shell when the
        // operating system exposes a supported browser window.
      }
    }

    if (expired) return false
    try {
      await openUrl(url)
      return true
    } catch {
      if (expired) return false
      try {
        const openedWindow = window.open(url, '_blank')
        if (!openedWindow) return false
        // Preserve noopener semantics while still retaining the only reliable
        // browser signal that a fallback window was actually created.
        try {
          openedWindow.opener = null
        } catch {
          // A created cross-origin WindowProxy is still a successful launch.
        }
        return true
      } catch {
        return false
      }
    }
  }
  try {
    return await Promise.race([
      openOnce(),
      new Promise<boolean>(resolve => {
        timer = setTimeout(() => { expired = true; resolve(false) }, 12_000)
      }),
    ])
  } finally {
    if (timer !== undefined) clearTimeout(timer)
  }
}

export function openWorkbench(options: { draftId?: string; todoText?: string } = {}): Promise<boolean> {
  const query = buildQuery({
    ...getDesktopLinkParams(),
    todoDraftId: options.draftId,
    desktopTodoText: options.todoText
  })
  return openExternal(buildUrl(`/workbench${query}`))
}

export async function openDesktopLogin(state: string): Promise<boolean> {
  const desktopCallback = `${DESKTOP_AUTH_SCHEME}://auth-callback`
  const query = buildQuery({
    from: 'desktop',
    desktopCallback,
    state
  })
  const loginUrl = buildUrl(`/login${query}`)
  recordDesktopDiagnostic('auth.login.url_built', {
    webBaseUrl: getWebBaseUrl(),
    loginUrl,
    desktopCallback,
    state,
  })

  // The login URL carries the one-time desktop state. UI Automation can report
  // that an existing tab was reused before the browser has actually navigated,
  // leaving the user visibly signed in but never entering the desktop callback
  // route. Hand the exact URL to the OS opener for every auth attempt instead.
  const opened = await openExternal(loginUrl, { reuseExistingTab: false })
  recordDesktopDiagnostic('auth.login.url_open_result', {
    loginUrl,
    desktopCallback,
    state,
    opened,
  })
  return opened
}

export function openCalendar(taskId?: string): Promise<boolean> {
  const query = buildQuery({
    ...getDesktopLinkParams(),
    taskId
  })
  return openExternal(buildUrl(`/calendar${query}`))
}

export function openSysMessageDetail(message: SysMessageNotification): Promise<boolean> {
  const detailId = message.bizId || message.id
  const query = buildQuery({
    ...getDesktopLinkParams(),
    desktopTodoId: detailId,
    desktopMessageId: message.id,
    desktopBizType: message.bizType === undefined ? undefined : String(message.bizType)
  })

  return openExternal(buildUrl(`/calendar${query}`))
}

export function openAgents(): Promise<boolean> {
  const query = buildQuery(getDesktopLinkParams())
  return openExternal(buildUrl(`/agents${query}`))
}

export async function hideAssistant() {
  try {
    await invoke('hide_main_window')
  } catch {
    document.body.classList.add('is-hidden-preview')
  }
}

export async function showMascotContextMenu() {
  try {
    return await invoke<boolean>('show_mascot_context_menu')
  } catch {
    return false
  }
}

export async function ackMascotContextMenuLayout(generation: number) {
  try {
    return await invoke<boolean>('ack_mascot_context_menu_layout', { generation })
  } catch {
    return false
  }
}

export async function hideMascotContextMenu() {
  try {
    await invoke('hide_mascot_context_menu')
  } catch {
    return
  }
}

export async function setMascotContextMenuReady() {
  try {
    await invoke('set_mascot_context_menu_ready')
  } catch {
    return
  }
}

export async function setMascotSystemNotificationReady() {
  try {
    return await invoke<boolean>('set_mascot_system_notification_ready')
  } catch {
    return false
  }
}

export async function isMascotSystemNotificationReady() {
  try {
    return await invoke<boolean>('is_mascot_system_notification_ready')
  } catch {
    return false
  }
}

export async function showMascotSystemNotificationWindow(
  compact = false,
  clientGeneration?: number,
) {
  try {
    return await invoke<boolean>('show_mascot_system_notification_window', {
      compact,
      clientGeneration: clientGeneration ?? null,
    })
  } catch {
    return false
  }
}

export async function hideMascotSystemNotificationWindow(clientGeneration?: number) {
  try {
    return await invoke<boolean>('hide_mascot_system_notification_window', {
      clientGeneration: clientGeneration ?? null,
    })
  } catch {
    return false
  }
}

export async function showAssistant(): Promise<boolean> {
  announceMascotReveal()
  for (const retryDelay of [0, 100, 300]) {
    if (retryDelay > 0) {
      await new Promise<void>((resolve) => window.setTimeout(resolve, retryDelay))
    }
    try {
      if (await invoke<boolean>('show_main_window')) return true
    } catch {
      // A first-show WebView2 transition can race native window creation on a
      // cold machine. Retry the same idempotent show before reporting failure.
    }
  }
  return false
}

export async function showNotificationWindow(): Promise<boolean> {
  try {
    return await invoke<boolean>('show_notification_window')
  } catch {
    return false
  }
}

export async function finishMascotNotificationCollapse(): Promise<boolean> {
  try {
    return await invoke<boolean>('finish_mascot_notification_collapse')
  } catch {
    return false
  }
}

export async function peekMascotWindow(reducedMotion = false) {
  try {
    return await invoke<MascotDockSide | null>('peek_mascot_window', { reducedMotion })
  } catch {
    return null
  }
}

export async function revealMascotWindow(reducedMotion = false) {
  announceMascotReveal()
  try {
    await invoke('reveal_mascot_window', { reducedMotion })
  } catch {
    return
  }
}

export async function startMascotWindowDrag() {
  await invoke('start_mascot_drag')
}

export async function togglePanelWindow() {
  announceMascotReveal()
  try {
    const visible = await invoke<boolean>('toggle_panel_window')
    if (visible) {
      await emitTo('panel', PANEL_REVEAL_EVENT, { focus: true } satisfies PanelRevealPayload)
    }
    return visible
  } catch {
    return undefined
  }
}

export async function showPanelWindow(options: { focus?: boolean } = {}): Promise<boolean> {
  announceMascotReveal()
  try {
    const visible = await invoke<boolean>('show_panel_window', {
      focus: options.focus ?? false
    })
    if (visible) {
      await emitTo('panel', PANEL_REVEAL_EVENT, {
        focus: options.focus ?? false
      } satisfies PanelRevealPayload)
    }
    return visible
  } catch {
    return false
  }
}

export async function hidePanelWindow(): Promise<boolean> {
  try {
    return await invoke<boolean>('hide_panel_window')
  } catch {
    return false
  }
}

export async function syncPanelWindow() {
  try {
    await invoke('sync_panel_window')
  } catch {
    return
  }
}

export async function setMascotNotificationVisible(
  visible: boolean,
  compact = false,
  options: { reveal?: boolean; reducedMotion?: boolean; hideDuringResize?: boolean } = {}
): Promise<boolean> {
  try {
    return await invoke<boolean>('set_mascot_notification_visible', {
      visible,
      compact,
      reveal: options.reveal ?? false,
      reducedMotion: options.reducedMotion ?? false,
      hideDuringResize: options.hideDuringResize ?? false
    })
  } catch {
    return false
  }
}

let panelHeightQueue = Promise.resolve()
let panelHeightRevision = 0
export async function setPanelHeight(height: number) {
  const revision = ++panelHeightRevision
  const operation = panelHeightQueue.then(async () => {
    if (revision !== panelHeightRevision) return
    try { await invoke('set_panel_height', { height }) } catch { /* Browser preview has no native window. */ }
  })
  panelHeightQueue = operation
  await operation
}

export async function setPanelActivity(activity: PanelActivityPayload) {
  try {
    await invoke('set_panel_activity', {
      hasText: activity.hasText,
      focused: activity.focused
    })
  } catch {
    return
  }
}

export async function exitAssistant() {
  try {
    await invoke('exit_app')
  } catch {
    window.close()
  }
}
