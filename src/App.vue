<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { emitTo, listen } from '@tauri-apps/api/event'
import MascotWindow from './views/MascotWindow.vue'
import MascotMenuWindow from './views/MascotMenuWindow.vue'
import MascotNotificationWindow from './views/MascotNotificationWindow.vue'
import PanelWindow from './views/PanelWindow.vue'
import {
  getOrCreateDesktopAuthState,
  listenDesktopAuthCallbacks,
  prepareDesktopReleaseSmokeState,
  recordDesktopReleaseSmokeRestart,
  recordDesktopReleaseSmokeSession,
  type DesktopAuthCallbackError,
} from './services/desktop-auth.service'
import { maskDiagnosticIdentifier, recordDesktopDiagnostic } from './services/diagnostic.service'
import { onDesktopUnauthorized, type DesktopUnauthorizedContext } from './services/request'
import {
  DESKTOP_UNAUTHORIZED_CONFIRMATION_WINDOW_MS,
  recordConfirmedDesktopUnauthorized,
  type DesktopUnauthorizedEvidence,
} from './services/session-recovery.service'
import { validateDesktopSession } from './services/session.service'
import { getSysMessageFallback, resolveSysMessageContent } from './services/sys-message-content.service'
import { sysMessageService } from './services/sys-message.service'
import { websocketService } from './services/websocket.service'
import { startRuntimeRecovery, markRuntimeMounted, requestNotificationRecovery, type RuntimeState } from './services/runtime-recovery.service'
import { readRecoverySnapshot, writeRecoverySnapshot, clearRecoverySnapshots } from './utils/recovery-storage'
import {
  PANEL_TASK_DELIVERED_EVENT,
  PANEL_TASK_READY_EVENT,
  PANEL_TASK_STATE_EVENT,
  PANEL_TASK_STATE_REQUEST_EVENT,
  MASCOT_CONTEXT_MENU_VISIBILITY_EVENT,
  MASCOT_SYSTEM_NOTIFICATION_ACTION_EVENT,
  MASCOT_SYSTEM_NOTIFICATION_PRESENT_EVENT,
  MASCOT_SYSTEM_NOTIFICATION_LAYOUT_EVENT,
  MASCOT_SYSTEM_NOTIFICATION_READY_EVENT,
  hideMascotSystemNotificationWindow,
  hidePanelWindow,
  isMascotSystemNotificationReady,
  openDesktopLogin,
  openSysMessageDetail,
  openWorkbench,
  setMascotNotificationVisible,
  showAssistant,
  showMascotSystemNotificationWindow,
  showNotificationWindow,
  showPanelWindow,
  type MascotMenuVisibilityPayload,
  type MascotSystemNotificationAction,
  type MascotSystemNotificationPresentation,
  type PanelTaskDeliveredPayload,
  type PanelTaskDeliveryPayload,
  type PanelTaskStatePayload,
  type PanelTaskStateRequestPayload,
} from './services/window.service'
import { useMascotStore } from './stores/mascot'
import { useTaskStore } from './stores/task'
import { useUserStore } from './stores/user'
import type { MascotStatus } from './types/mascot'
import type { SysMessageNotification } from './types/sys-message'
import type { TaskCreatedEvent } from './types/task'
import { env } from './utils/env'
import { storage } from './utils/storage'
import { createNotificationDelivery } from './utils/notification-delivery'
import {
  SYS_MESSAGE_EXPIRY_MS,
  isSysMessageExpired,
  resolveSysMessageExpiresAt,
} from './utils/sys-message-expiry'

type ResolvedSysMessage = SysMessageNotification & {
  displayContent: string
  expiresAt: number
}

interface MascotRecoverySnapshot {
  current: ResolvedSysMessage | null
  queue: ResolvedSysMessage[]
  deferred: ResolvedSysMessage[]
  tasks: TaskCreatedEvent[]
  sessionEpoch: number
  hiddenKey: string
}
interface PanelRecoverySnapshot {
  tasks: typeof taskStore.taskQueue
  receivedEventIds: string[]
  sessionEpoch: number | null
}

const taskStore = useTaskStore()
const mascotStore = useMascotStore()
const userStore = useUserStore()
const searchParams = new URLSearchParams(window.location.search)
const windowMode = searchParams.get('window') || 'mascot'
const savedMascot = windowMode === 'mascot'
  ? readRecoverySnapshot<MascotRecoverySnapshot>('mascot', userStore.userInfo?.userId || '') : null
const savedPanel = windowMode === 'panel'
  ? readRecoverySnapshot<PanelRecoverySnapshot>('panel', userStore.userInfo?.userId || '') : null
const isSysMessagePreview = import.meta.env.DEV && searchParams.get('preview') === 'sys-message'
const isTaskPreview = import.meta.env.DEV && windowMode === 'panel' && searchParams.get('preview') === 'task'
const isMascotAnimationPreview = import.meta.env.DEV && searchParams.has('previewAnimation')
const hasSysMessagePreviewQueue = isSysMessagePreview && searchParams.get('previewQueue') === '1'
const hasSysMessagePreviewError = isSysMessagePreview && searchParams.get('previewError') === '1'
const hasSysMessagePreviewLongContent = isSysMessagePreview && searchParams.get('previewLong') === '1'
const hasTaskPreviewQueue = isTaskPreview && searchParams.get('previewQueue') === '1'
const hasTaskPreviewError = isTaskPreview && searchParams.get('previewError') === '1'
const hasTaskPreviewLongContent = isTaskPreview && searchParams.get('previewLong') === '1'
const socketStatus = ref(env.enableMock ? 'mock' : 'closed')
const currentSysMessage = ref<ResolvedSysMessage | null>(
  savedMascot?.current && !isSysMessageExpired(savedMascot.current.expiresAt) ? savedMascot.current : null,
)
const sysMessageQueue = ref<ResolvedSysMessage[]>(Array.isArray(savedMascot?.queue) ? savedMascot.queue.filter(m => !isSysMessageExpired(m.expiresAt)) : [])
const deferredSysMessages = ref<ResolvedSysMessage[]>(Array.isArray(savedMascot?.deferred) ? savedMascot.deferred.filter(m => !isSysMessageExpired(m.expiresAt)) : [])
const runtimeInteractive = ref(true)
let removeRuntimeRecovery: (() => void) | undefined
let runtimeRecovered = false
let runtimeCoordinatorReady = false
let pendingRuntimeState: RuntimeState | undefined
const sysMessageReadPendingKey = ref('')
const sysMessageReadAllPending = ref(false)
const sysMessageActionError = ref('')
const recentSysMessageKeys = new Set<string>()
const authPending = ref(false)
const authErrorMessage = ref('')
const SESSION_VALIDATION_INTERVAL = 5 * 60 * 1000
const AUTH_CALLBACK_REMINDER_DELAY = 2 * 60 * 1000
const TASK_DELIVERY_ACK_TIMEOUT = 1000
const MASCOT_INTERACTION_READY_TIMEOUT = 4000
let removeTaskListener: (() => void) | undefined
let removeStatusListener: (() => void) | undefined
let removeTrayListener: (() => void) | undefined
let removeTrayLogoutListener: (() => void) | undefined
let removePanelTaskListener: (() => void) | undefined
let removePanelTaskDeliveredListener: (() => void) | undefined
let removePanelTaskReadyListener: (() => void) | undefined
let removePanelTaskStateListener: (() => void) | undefined
let removePanelTaskStateRequestListener: (() => void) | undefined
let removePanelSessionClearedListener: (() => void) | undefined
let removeMascotMessageListener: (() => void) | undefined
let removeSysMessageListener: (() => void) | undefined
let removeSystemNotificationActionListener: UnlistenFn | undefined
let removeSystemNotificationReadyListener: UnlistenFn | undefined
let removeSystemNotificationLayoutListener: UnlistenFn | undefined
let removeContextMenuVisibilityListener: UnlistenFn | undefined
let removeDeepLinkListener: UnlistenFn | undefined
let removeUnauthorizedListener: (() => void) | undefined
let sessionValidationTimer: number | undefined
let sessionRecoveryPromise: Promise<void> | undefined
let sessionUnauthorizedEvidence: DesktopUnauthorizedEvidence | undefined
let authCallbackTimer: number | undefined
let releaseSmokePrepared = false
let sysMessageExpiryTimer: number | undefined
let sysMessageEnrichmentGeneration = 0
let systemNotificationPresentationGeneration = 0
// Leave enough sequence space between renderer lifetimes so a late IPC from a
// suspended/reloaded WebView cannot outrank the new coordinator's intent.
let systemNotificationSyncGeneration = Date.now() * 1000
let systemNotificationMessageKey = ''
// Right-click “隐藏” suppresses only the presentation the user has already
// seen. A later auth cycle or a newly delivered reminder receives a new key and
// may wake the assistant, preserving the product promise on the menu action.
let userHiddenSystemNotificationKey = savedMascot?.hiddenKey || ''
const systemNotificationWindowReady = ref(false)
const contextMenuWindowVisible = ref(false)
const visibleSystemNotification = ref<MascotSystemNotificationPresentation | null>(null)
const systemMessageWindowVisible = computed(() =>
  visibleSystemNotification.value?.kind === 'message'
  && visibleSystemNotification.value.message.dedupeKey === currentSysMessage.value?.dedupeKey
  && !contextMenuWindowVisible.value
  && !needsAuth.value
)
const notificationDelivery = createNotificationDelivery<MascotSystemNotificationPresentation>({
  nextGeneration: () => ++systemNotificationSyncGeneration,
  publish: (delivery) => emitTo(
    'mascot-notification', MASCOT_SYSTEM_NOTIFICATION_PRESENT_EVENT, delivery,
  ),
  async show(syncGeneration, presentation) {
    const mascotShown = await showNotificationWindow()
    if (!mascotShown || syncGeneration !== systemNotificationSyncGeneration) return false
    const notificationShown = await showMascotSystemNotificationWindow(
      presentation.kind === 'auth',
      syncGeneration,
    )
    recordDesktopDiagnostic('notification.sync.completed', {
      kind: presentation.kind, mascotShown, notificationShown, generation: syncGeneration,
    })
    return notificationShown
  },
  hide: (generation) => hideMascotSystemNotificationWindow(generation),
  key: (presentation) => presentation.kind === 'auth' ? 'auth' : presentation.message.dedupeKey,
  onStopped: handleNotificationStopped,
  onVisible: (presentation) => { visibleSystemNotification.value = presentation },
})
let isDeliveringDeferredTasks = false
const panelTaskStateReady = ref(false)
const deferredTaskEvents: TaskCreatedEvent[] = Array.isArray(savedMascot?.tasks) ? savedMascot.tasks : []
const panelHasTask = ref(false)
const panelSessionEpoch = ref<number | null>(savedPanel?.sessionEpoch ?? null)
let taskSessionEpoch = Number.isSafeInteger(savedMascot?.sessionEpoch) ? savedMascot!.sessionEpoch : Date.now()
let mascotInteractionReadyResolved = false
let resolveMascotInteractionReady: (() => void) | undefined
const mascotInteractionReady = new Promise<void>((resolve) => {
  resolveMascotInteractionReady = resolve
})

function handleMascotInteractionReady() {
  if (mascotInteractionReadyResolved) return
  mascotInteractionReadyResolved = true
  resolveMascotInteractionReady?.()
  resolveMascotInteractionReady = undefined
}

async function waitForMascotInteractionReady() {
  if (mascotInteractionReadyResolved) return

  let timeout: number | undefined
  await Promise.race([
    mascotInteractionReady,
    new Promise<void>((resolve) => {
      timeout = window.setTimeout(resolve, MASCOT_INTERACTION_READY_TIMEOUT)
    }),
  ])
  window.clearTimeout(timeout)
}
let awaitingTaskDelivery: PanelTaskDeliveredPayload | null = null
let taskDeliveryRetryTimer: number | undefined
let taskDeliveryFailureEventId = ''

const panelWindowRef = ref<InstanceType<typeof PanelWindow> | null>(null)
const currentTask = computed(() => taskStore.currentTask)
const sysMessageUserId = computed(() => userStore.userInfo?.userId || env.desktopUserId || env.mockUserId)
const needsAuth = computed(
  () => !isSysMessagePreview
    && !isMascotAnimationPreview
    && !env.enableMock
    && !userStore.isAuthenticated
)
const currentSysMessageContent = computed(() => currentSysMessage.value?.displayContent || '')
const isCurrentSysMessageReadPending = computed(
  () => sysMessageReadAllPending.value
    || currentSysMessage.value?.dedupeKey === sysMessageReadPendingKey.value
)
const pendingSysMessageCount = computed(() => sysMessageQueue.value.length)

if (savedPanel && Array.isArray(savedPanel.tasks)) {
  taskStore.taskQueue = savedPanel.tasks.map(task => ({
    ...task, handling: false,
    error: task.handling ? '上次操作结果尚未确认，请先查看工作台' : task.error,
  }))
  taskStore.currentTask = taskStore.taskQueue[0] ?? null
  taskStore.receivedEventIds = Array.isArray(savedPanel.receivedEventIds) ? savedPanel.receivedEventIds : []
}

function persistRecoveryState() {
  const userId = storage.getUserInfo()?.userId || ''
  if (windowMode === 'mascot') {
    writeRecoverySnapshot('mascot', userId, {
      current: currentSysMessage.value, queue: sysMessageQueue.value,
      deferred: deferredSysMessages.value, tasks: deferredTaskEvents,
      sessionEpoch: taskSessionEpoch, hiddenKey: userHiddenSystemNotificationKey,
    } satisfies MascotRecoverySnapshot)
  } else if (windowMode === 'panel') {
    writeRecoverySnapshot('panel', userId, {
      tasks: taskStore.taskQueue, receivedEventIds: taskStore.receivedEventIds,
      sessionEpoch: panelSessionEpoch.value,
    } satisfies PanelRecoverySnapshot)
  }
}

watch([currentSysMessage, sysMessageQueue, deferredSysMessages, () => taskStore.taskQueue, panelSessionEpoch],
  persistRecoveryState, { deep: true, flush: 'sync' })

function handleNotificationStopped(presentation: MascotSystemNotificationPresentation) {
  if (presentation.kind === 'message' && currentSysMessage.value?.dedupeKey === presentation.message.dedupeKey) {
    const message = currentSysMessage.value
    if (!deferredSysMessages.value.some(item => item.dedupeKey === message.dedupeKey)) {
      deferredSysMessages.value.push(message)
    }
    // Keep unread data, but release the invisible head so clicks and later
    // messages can proceed. Only a new runtime recovery round retries it.
    showNextSysMessage()
  }
  if (runtimeInteractive.value) {
    mascotStore.showMessage('提醒暂未显示，消息已保留，可在工作台查看', 'idle', true)
    requestNotificationRecovery()
  }
}

function recoverDesktopRuntime(state: RuntimeState) {
  runtimeRecovered ||= state.recovered
  runtimeInteractive.value = state.interactive
  if (windowMode !== 'mascot') return
  if (!state.interactive) {
    notificationDelivery.suspend()
    return
  }
  expireStaleSysMessages()
  const messages = [currentSysMessage.value, ...sysMessageQueue.value, ...deferredSysMessages.value]
    .filter((message): message is ResolvedSysMessage => Boolean(message && !isSysMessageExpired(message.expiresAt)))
  const unique = [...new Map(messages.map(message => [message.dedupeKey, message])).values()]
  currentSysMessage.value = unique.shift() ?? null
  sysMessageQueue.value = unique
  deferredSysMessages.value = []
  contextMenuWindowVisible.value = false
  const presentation = buildSystemNotificationPresentation()
  notificationDelivery.recover(`native:${state.epoch}`, systemNotificationMessageKey === userHiddenSystemNotificationKey ? null : presentation)
  connectDesktopSockets({ force: true, catchUp: true })
  requestPanelTaskState()
  scheduleSysMessageExpiry()
  persistRecoveryState()
}

function publishPanelTaskState(requestReveal = false) {
  if (windowMode !== 'panel' || panelSessionEpoch.value === null) return

  const payload: PanelTaskStatePayload = {
    hasTask: Boolean(currentTask.value),
    requestReveal,
    sessionEpoch: panelSessionEpoch.value,
  }
  void emitTo('mascot', PANEL_TASK_STATE_EVENT, payload)
}

function adoptPanelSessionEpoch(sessionEpoch: number) {
  if (panelSessionEpoch.value !== null && sessionEpoch < panelSessionEpoch.value) return false
  if (panelSessionEpoch.value !== null && sessionEpoch !== panelSessionEpoch.value) {
    taskStore.clearTasks()
  }
  panelSessionEpoch.value = sessionEpoch
  return true
}

async function showTaskPanelWithFallback() {
  if (needsAuth.value || currentSysMessage.value || !panelHasTask.value) return

  // The native panel command restores a peeked mascot, positions both HWNDs,
  // then reveals them without activation. A separate mascot show before that
  // sequence would expose the off-screen peek position for one frame.
  const shown = await showPanelWindow({ focus: false })
  // A system message can arrive while native positioning is in flight. It
  // remains the higher-priority surface even if the panel just became visible.
  if (needsAuth.value || currentSysMessage.value) {
    if (shown) await hidePanelWindow()
    return
  }

  if (shown) return
  mascotStore.showMessage('收到新任务，任务卡暂时无法显示；请单击机器人重试', 'remind', true)
}

function queueTaskForPanel(event: TaskCreatedEvent) {
  if (deferredTaskEvents.some((item) => item.eventId === event.eventId)) return
  deferredTaskEvents.push(event)
  persistRecoveryState()
}

async function deliverTasksWhenSystemMessagesFinish() {
  if (
    needsAuth.value
    || currentSysMessage.value
    || !panelTaskStateReady.value
    || awaitingTaskDelivery
    || isDeliveringDeferredTasks
  ) return

  const event = deferredTaskEvents[0]
  if (!event) {
    if (panelHasTask.value) await showTaskPanelWithFallback()
    return
  }

  isDeliveringDeferredTasks = true
  const delivery: PanelTaskDeliveryPayload = {
    event,
    sessionEpoch: taskSessionEpoch,
  }
  awaitingTaskDelivery = {
    eventId: event.eventId,
    sessionEpoch: taskSessionEpoch,
  }
  try {
    await emitTo('panel', 'task-created', delivery)
  } catch {
    // Keep the event queued. Native emit can resolve even when a renderer is
    // reloading, so the ACK timeout below is the only dequeue authority.
    if (taskDeliveryFailureEventId !== event.eventId) {
      taskDeliveryFailureEventId = event.eventId
      mascotStore.showMessage('收到新任务，暂时无法打开任务卡；稍后将继续重试', 'remind', true)
    }
  } finally {
    isDeliveringDeferredTasks = false
  }

  if (
    awaitingTaskDelivery?.eventId !== event.eventId
    || awaitingTaskDelivery.sessionEpoch !== delivery.sessionEpoch
  ) {
    void deliverTasksWhenSystemMessagesFinish()
    return
  }

  window.clearTimeout(taskDeliveryRetryTimer)
  taskDeliveryRetryTimer = window.setTimeout(() => {
    taskDeliveryRetryTimer = undefined
    if (
      awaitingTaskDelivery?.eventId !== event.eventId
      || awaitingTaskDelivery.sessionEpoch !== delivery.sessionEpoch
      || taskSessionEpoch !== delivery.sessionEpoch
    ) return

    awaitingTaskDelivery = null
    void deliverTasksWhenSystemMessagesFinish()
  }, TASK_DELIVERY_ACK_TIMEOUT)
}

function handleTaskDelivered(payload: PanelTaskDeliveredPayload) {
  if (
    payload.sessionEpoch !== taskSessionEpoch
    || awaitingTaskDelivery?.sessionEpoch !== payload.sessionEpoch
    || awaitingTaskDelivery.eventId !== payload.eventId
    || deferredTaskEvents[0]?.eventId !== payload.eventId
  ) return

  window.clearTimeout(taskDeliveryRetryTimer)
  taskDeliveryRetryTimer = undefined
  awaitingTaskDelivery = null
  deferredTaskEvents.shift()
  persistRecoveryState()
  taskDeliveryFailureEventId = ''
  void deliverTasksWhenSystemMessagesFinish()
}

function requestPanelTaskState() {
  const payload: PanelTaskStateRequestPayload = { sessionEpoch: taskSessionEpoch }
  void emitTo('panel', PANEL_TASK_STATE_REQUEST_EVENT, payload)
}

function handlePanelTaskReady() {
  panelTaskStateReady.value = false
  requestPanelTaskState()
}

function handlePanelTaskState(payload: PanelTaskStatePayload) {
  if (payload.sessionEpoch !== taskSessionEpoch) return

  panelTaskStateReady.value = true
  panelHasTask.value = payload.hasTask
  // The delivery coordinator is the only task-panel reveal authority. Calling
  // show here as well races with ACK-driven delivery and can replay the native
  // reveal animation two or three times for one incoming task.
  void deliverTasksWhenSystemMessagesFinish()
}

watch(currentTask, () => publishPanelTaskState(), { flush: 'sync' })

if (isSysMessagePreview) {
  const previewSubject = hasSysMessagePreviewLongContent
    ? '研发项目月度评审任务即将开始请所有相关负责人提前完成材料准备与风险确认'
    : '会议即将开始'
  const previewContent = hasSysMessagePreviewLongContent
    ? '请携带项目进度、风险清单、本月交付结果、资源缺口和下阶段计划参加评审，并提前确认会议室、参会人员、演示设备以及需要管理层决策的事项。这是一段用于验证极端长消息边界的预览内容。'
    : '您的项目评审会议将在 15 分钟后开始，请提前准备相关材料。'
  currentSysMessage.value = {
    id: 'design-preview-meeting',
    rawId: 'design-preview-meeting',
    dedupeKey: 'design-preview-meeting',
    msgSubject: previewSubject,
    msgContent: previewContent,
    displayContent: previewContent,
    msgStatus: 0,
    msgType: 1,
    bizType: 2,
    bizId: 'design-preview-meeting',
    createTime: '2026-07-16 18:15',
    expiresAt: Date.now() + SYS_MESSAGE_EXPIRY_MS,
  }
  if (hasSysMessagePreviewQueue) {
    sysMessageQueue.value.push({
      id: 'design-preview-todo',
      rawId: 'design-preview-todo',
      dedupeKey: 'design-preview-todo',
      msgSubject: '研发项目月度评审任务即将开始',
      msgContent: '请携带项目进度、风险清单和本月交付结果参加评审，并提前确认会议室与参会人员。',
      displayContent: '请携带项目进度、风险清单和本月交付结果参加评审，并提前确认会议室与参会人员。',
      msgStatus: 0,
      msgType: 1,
      bizType: 1,
      bizId: 'design-preview-todo',
      createTime: '2026-07-16 18:55',
      expiresAt: Date.now() + SYS_MESSAGE_EXPIRY_MS,
    })
  }
  if (hasSysMessagePreviewError) {
    sysMessageActionError.value = '未能标记已读，请检查网络后重试'
  }
}

if (isTaskPreview) {
  if (hasTaskPreviewQueue) {
    taskStore.pushTask({
      eventId: 'design-preview-task-queued',
      eventType: 'task.created',
      timestamp: '2026-08-12 15:08:00',
      payload: {
        taskId: 'design-preview-task-queued',
        title: '确认下一条任务切换状态',
        content: '完成当前任务后，应立即显示这条任务，按钮保持可操作。',
        deadline: '2026-08-13 10:00:00',
        actions: [
          { key: 'confirm', label: '完成' },
          { key: 'openDetail', label: '查看详情' },
        ],
      },
    })
  }

  taskStore.pushTask({
    eventId: 'design-preview-task-current',
    eventType: 'task.created',
    timestamp: '2026-08-12 15:10:00',
    payload: {
      taskId: 'design-preview-task-current',
      title: hasTaskPreviewLongContent
        ? '研发项目月度评审任务即将到期请相关负责人逐项核对交付内容并确认所有风险处理结果'
        : '确认研发项目月度评审材料',
      content: hasTaskPreviewLongContent
        ? '请逐项核对本月交付结果、风险清单、资源缺口、下阶段计划、会议材料、参会人员、演示设备以及需要管理层决策的事项。这是一段用于验证笔记本小高度和高 DPI 下正文滚动、错误提示与完整操作区的预览内容。'
        : '请核对交付结果、风险清单和下阶段计划。',
      deadline: '2026-08-12 18:30:00',
      actions: [
        { key: 'confirm', label: '管理员强制完成' },
        { key: 'later', label: '稍后提醒' },
        { key: 'openDetail', label: '任意查看文案' },
      ],
    },
  })

  if (hasTaskPreviewError && taskStore.currentTask) {
    taskStore.currentTask.error = '操作失败：服务暂时不可用，请稍后重试。当前任务不会被移除。'
  }
}

document.documentElement.dataset.window = windowMode
document.body.dataset.window = windowMode

function rememberSysMessageKey(key: string) {
  recentSysMessageKeys.add(key)
  window.setTimeout(() => recentSysMessageKeys.delete(key), 5000)
}

function showNextSysMessage(now = Date.now()) {
  sysMessageActionError.value = ''
  sysMessageQueue.value = sysMessageQueue.value.filter(
    (message) => !isSysMessageExpired(message.expiresAt, now),
  )
  currentSysMessage.value = sysMessageQueue.value.shift() ?? null
  if (currentSysMessage.value) {
    void emitTo('mascot', 'mascot-close-overlays', {})
    void hidePanelWindow()
  } else {
    void deliverTasksWhenSystemMessagesFinish()
  }
}

function showIncomingSysMessage(message: ResolvedSysMessage) {
  if (isSysMessageExpired(message.expiresAt)) return false

  // A genuinely new reminder is allowed to wake an explicitly hidden
  // assistant. Do this before queue mutation so the reactive sync observes the
  // cleared suppression even when another message is currently at the head.
  if (!message.attentionKey || !currentSysMessage.value) {
    userHiddenSystemNotificationKey = ''
    void emitTo('mascot', 'mascot-close-overlays', {})
  }

  if (currentSysMessage.value) {
    sysMessageQueue.value.push(message)
    if (message.attentionKey) {
      sysMessageQueue.value.sort((a, b) => a.expiresAt - b.expiresAt)
      return true
    }
  } else {
    sysMessageActionError.value = ''
    currentSysMessage.value = message
  }
  // MascotWindow owns the native notification pipeline. It waits for Vue to
  // paint and for the bounds command to succeed before showing the hidden HWND.
  void hidePanelWindow()
  return true
}

function expireStaleSysMessages(now = Date.now()) {
  deferredSysMessages.value = deferredSysMessages.value.filter(message => !isSysMessageExpired(message.expiresAt, now))
  const activeQueue = sysMessageQueue.value.filter(
    (message) => !isSysMessageExpired(message.expiresAt, now),
  )
  if (activeQueue.length !== sysMessageQueue.value.length) {
    sysMessageQueue.value = activeQueue
  }

  if (currentSysMessage.value && isSysMessageExpired(currentSysMessage.value.expiresAt, now)) {
    showNextSysMessage(now)
  }
}

function scheduleSysMessageExpiry() {
  window.clearTimeout(sysMessageExpiryTimer)
  sysMessageExpiryTimer = undefined

  const expiries = [
    ...(currentSysMessage.value ? [currentSysMessage.value.expiresAt] : []),
    ...sysMessageQueue.value.map((message) => message.expiresAt),
    ...deferredSysMessages.value.map((message) => message.expiresAt),
  ]
  if (!expiries.length) return

  const nextExpiry = Math.min(...expiries)
  const delay = Math.max(0, nextExpiry - Date.now())
  sysMessageExpiryTimer = window.setTimeout(() => {
    sysMessageExpiryTimer = undefined
    expireStaleSysMessages()
    // Browsers may deliver a timeout a fraction early. Rescheduling also picks
    // up the next queued reminder after the current one has been removed.
    scheduleSysMessageExpiry()
  }, delay)
}

function applyEnrichedSysMessage(key: string, displayContent: string) {
  if (currentSysMessage.value?.dedupeKey === key) {
    currentSysMessage.value = { ...currentSysMessage.value, displayContent }
    return
  }

  const index = sysMessageQueue.value.findIndex((item) => item.dedupeKey === key)
  if (index >= 0) {
    sysMessageQueue.value[index] = { ...sysMessageQueue.value[index], displayContent }
  }
}

async function enrichSysMessage(message: SysMessageNotification, generation: number) {
  try {
    const displayContent = await resolveSysMessageContent(message)
    if (generation !== sysMessageEnrichmentGeneration) return
    applyEnrichedSysMessage(message.dedupeKey, displayContent)
  } catch (error) {
    // The readable fallback is already visible. Enrichment failure must not
    // delay or remove either the current notification or queued messages.
    console.warn('Failed to enrich sys_message content', error)
  }
}

function pushSysMessage(message: SysMessageNotification) {
  if ([currentSysMessage.value, ...sysMessageQueue.value, ...deferredSysMessages.value]
    .some(item => item?.dedupeKey === message.dedupeKey)) return
  if (recentSysMessageKeys.has(message.dedupeKey)) {
    recordDesktopDiagnostic('notification.renderer_duplicate', {
      messageIdMasked: maskDiagnosticIdentifier(message.id),
      bizType: message.bizType ?? null,
    })
    return
  }
  rememberSysMessageKey(message.dedupeKey)

  const generation = sysMessageEnrichmentGeneration
  const resolvedMessage: ResolvedSysMessage = {
    ...message,
    displayContent: getSysMessageFallback(message),
    expiresAt: resolveSysMessageExpiresAt(message.createTime),
  }
  if (!showIncomingSysMessage(resolvedMessage)) {
    recordDesktopDiagnostic('notification.renderer_rejected', {
      reason: 'expired',
      messageIdMasked: maskDiagnosticIdentifier(message.id),
      bizType: message.bizType ?? null,
    })
    return
  }
  recordDesktopDiagnostic('notification.renderer_queued', {
    messageIdMasked: maskDiagnosticIdentifier(message.id),
    bizType: message.bizType ?? null,
    queueLength: sysMessageQueue.value.length,
  })
  // Each message enriches independently, so one 12-second detail request can
  // never block the first card or later cards behind it.
  void enrichSysMessage(message, generation)
}

function hideCurrentSysMessage(message: SysMessageNotification) {
  if (currentSysMessage.value?.dedupeKey === message.dedupeKey) {
    showNextSysMessage()
    return
  }

  sysMessageQueue.value = sysMessageQueue.value.filter((item) => item.dedupeKey !== message.dedupeKey)
}

async function handleSysMessageRead(message: SysMessageNotification) {
  if (isCurrentSysMessageReadPending.value) return

  if (isSysMessagePreview) {
    hideCurrentSysMessage(message)
    return
  }

  const actionSession = sysMessageEnrichmentGeneration
  sysMessageActionError.value = ''
  sysMessageReadPendingKey.value = message.dedupeKey
  try {
    await sysMessageService.markRead(message)
    if (actionSession !== sysMessageEnrichmentGeneration) return
    hideCurrentSysMessage(message)
  } catch (error) {
    if (actionSession !== sysMessageEnrichmentGeneration) return
    console.warn('Failed to mark sys_message as read', error)
    if (currentSysMessage.value?.dedupeKey === message.dedupeKey) {
      sysMessageActionError.value = '未能标记已读，请检查网络后重试'
    }
  } finally {
    if (actionSession !== sysMessageEnrichmentGeneration) return
    if (sysMessageReadPendingKey.value === message.dedupeKey) {
      sysMessageReadPendingKey.value = ''
    }
  }
}

async function handleAllSysMessagesRead() {
  if (isCurrentSysMessageReadPending.value) return
  expireStaleSysMessages()
  const snapshot = [
    ...(currentSysMessage.value ? [currentSysMessage.value] : []),
    ...sysMessageQueue.value,
  ]
  if (snapshot.length < 2) return

  const snapshotKeys = new Set(snapshot.map((message) => message.dedupeKey))
  const actionSession = sysMessageEnrichmentGeneration
  sysMessageActionError.value = ''
  sysMessageReadAllPending.value = true
  try {
    if (!isSysMessagePreview) {
      await sysMessageService.markAllRead(snapshot)
    }

    if (actionSession !== sysMessageEnrichmentGeneration) return
    const remainingMessages = [
      ...(currentSysMessage.value && !snapshotKeys.has(currentSysMessage.value.dedupeKey)
        ? [currentSysMessage.value]
        : []),
      ...sysMessageQueue.value.filter((message) => !snapshotKeys.has(message.dedupeKey)),
    ]
    currentSysMessage.value = remainingMessages.shift() ?? null
    sysMessageQueue.value = remainingMessages
    if (currentSysMessage.value) {
      void hidePanelWindow()
    } else {
      void deliverTasksWhenSystemMessagesFinish()
    }
  } catch (error) {
    if (actionSession !== sysMessageEnrichmentGeneration) return
    console.warn('Failed to mark all sys_messages as read', error)
    if (currentSysMessage.value && snapshotKeys.has(currentSysMessage.value.dedupeKey)) {
      sysMessageActionError.value = '未能全部标为已读，消息已保留，请稍后重试'
    }
  } finally {
    if (actionSession !== sysMessageEnrichmentGeneration) return
    sysMessageReadAllPending.value = false
  }
}

async function handleSysMessageView(message: SysMessageNotification) {
  if (isCurrentSysMessageReadPending.value) return

  if (isSysMessagePreview) {
    hideCurrentSysMessage(message)
    return
  }

  const actionSession = sysMessageEnrichmentGeneration
  sysMessageActionError.value = ''
  sysMessageReadPendingKey.value = message.dedupeKey
  try {
    const opened = await openSysMessageDetail(message)
    if (actionSession !== sysMessageEnrichmentGeneration) return
    if (!opened) {
      if (currentSysMessage.value?.dedupeKey === message.dedupeKey) {
        sysMessageActionError.value = '未能打开详情，请检查默认浏览器后重试'
      }
      return
    }

    const detailId = message.bizId || message.id
    storage.setLastSysMessageDetail({
      messageId: message.id,
      detailId,
      bizType: message.bizType
    })

    try {
      await sysMessageService.markRead(message)
    } catch (error) {
      if (actionSession !== sysMessageEnrichmentGeneration) return
      console.warn('Failed to mark viewed sys_message as read', error)
      if (currentSysMessage.value?.dedupeKey === message.dedupeKey) {
        sysMessageActionError.value = '详情已打开，但未能标记已读；可点击“知道了”重试'
      }
      return
    }

    if (actionSession !== sysMessageEnrichmentGeneration) return
    hideCurrentSysMessage(message)
  } finally {
    if (actionSession !== sysMessageEnrichmentGeneration) return
    if (sysMessageReadPendingKey.value === message.dedupeKey) {
      sysMessageReadPendingKey.value = ''
    }
  }
}

function buildSystemNotificationPresentation(): MascotSystemNotificationPresentation | null {
  if (needsAuth.value) {
    if (systemNotificationMessageKey !== 'auth') {
      systemNotificationMessageKey = 'auth'
      systemNotificationPresentationGeneration += 1
    }

    return {
      kind: 'auth',
      generation: systemNotificationPresentationGeneration,
      pending: authPending.value,
      message: authErrorMessage.value,
    }
  }

  const message = currentSysMessage.value
  if (!message) return null

  const presentationKey = `message:${message.dedupeKey}`
  if (presentationKey !== systemNotificationMessageKey) {
    systemNotificationMessageKey = presentationKey
    systemNotificationPresentationGeneration += 1
  }

  return {
    kind: 'message',
    generation: systemNotificationPresentationGeneration,
    message,
    displayContent: currentSysMessageContent.value,
    pendingCount: pendingSysMessageCount.value,
    readPending: isCurrentSysMessageReadPending.value,
    readAllPending: sysMessageReadAllPending.value,
    actionError: sysMessageActionError.value,
  }
}

async function syncSystemNotificationWindow() {
  if (windowMode !== 'mascot') return
  if (!runtimeInteractive.value) { notificationDelivery.suspend(); return }
  const presentation = buildSystemNotificationPresentation()
  if (!presentation) systemNotificationMessageKey = ''
  if (!presentation) userHiddenSystemNotificationKey = ''
  const suppressedByUserHide = Boolean(
    presentation
    && systemNotificationMessageKey
    && systemNotificationMessageKey === userHiddenSystemNotificationKey
  )
  // Preserve this intent even when ready has not arrived yet. Delivery waits
  // for the card's layout ACK and retries native failures without a new message.
  notificationDelivery.sync(
    suppressedByUserHide || contextMenuWindowVisible.value ? null : presentation,
  )
  if (!presentation || suppressedByUserHide) {
    recordDesktopDiagnostic('notification.sync.hidden', {
      generation: systemNotificationSyncGeneration,
      reason: suppressedByUserHide ? 'user-hide' : 'no-presentation',
    })
  }
}

function handleSystemNotificationAction(payload: MascotSystemNotificationAction) {
  if (payload.action === 'login') {
    void startDesktopLogin()
    return
  }
  if (payload.action === 'readAll') {
    void handleAllSysMessagesRead()
    return
  }
  if (!payload.message) return
  if (payload.message.dedupeKey !== currentSysMessage.value?.dedupeKey) return
  if (payload.action === 'read') {
    void handleSysMessageRead(payload.message)
  } else if (payload.action === 'view') {
    void handleSysMessageView(payload.message)
  }
}

watch(
  () => [
    needsAuth.value,
    authPending.value,
    authErrorMessage.value,
    currentSysMessage.value?.dedupeKey,
    currentSysMessageContent.value,
    pendingSysMessageCount.value,
    isCurrentSysMessageReadPending.value,
    sysMessageReadAllPending.value,
    sysMessageActionError.value,
  ],
  () => { void syncSystemNotificationWindow() },
  { flush: 'post' },
)

watch(
  () => [
    currentSysMessage.value?.expiresAt ?? 0,
    ...sysMessageQueue.value.map((message) => message.expiresAt),
    ...deferredSysMessages.value.map((message) => message.expiresAt),
  ],
  scheduleSysMessageExpiry,
  { immediate: true, flush: 'sync' },
)

function connectDesktopSockets(options: { force?: boolean; catchUp?: boolean } = {}) {
  if (needsAuth.value) {
    recordDesktopDiagnostic('subscription.start_blocked', {
      reason: 'desktop-session-missing',
      tokenPresent: Boolean(userStore.token),
      userIdPresent: Boolean(userStore.userInfo?.userId),
    })
    return
  }

  recordDesktopDiagnostic('subscription.start', {
    force: Boolean(options.force),
    tokenPresent: Boolean(userStore.token),
    tokenLength: userStore.token.length,
    userIdPresent: Boolean(sysMessageUserId.value),
    userIdMasked: maskDiagnosticIdentifier(sysMessageUserId.value),
  })
  // These are independent channels. A construction/configuration failure in
  // the legacy task socket must never suppress the sys_message polling/socket
  // channel that carries todo, meeting and message reminders.
  try {
    websocketService.connect(options)
  } catch (error) {
    recordDesktopDiagnostic('task.websocket.connect_unhandled', {
      errorName: error instanceof Error ? error.name : 'unknown',
    })
  }
  try {
    sysMessageService.connect(sysMessageUserId.value, options)
  } catch (error) {
    recordDesktopDiagnostic('reminder.subscription.connect_unhandled', {
      errorName: error instanceof Error ? error.name : 'unknown',
    })
  }
}

function stopSessionValidation() {
  window.clearInterval(sessionValidationTimer)
  sessionValidationTimer = undefined
}

function stopAuthCallbackTimer() {
  window.clearTimeout(authCallbackTimer)
  authCallbackTimer = undefined
}

function resetSessionUnauthorizedEvidence() {
  sessionUnauthorizedEvidence = undefined
}

function clearDesktopSession(message: string, status: MascotStatus = 'remind') {
  notificationDelivery.reset()
  recordDesktopDiagnostic('session.clear_requested', {
    status,
    tokenPresent: Boolean(userStore.token),
    userIdMasked: maskDiagnosticIdentifier(userStore.userInfo?.userId),
  })
  websocketService.disconnect()
  sysMessageService.disconnect()
  userStore.clearSession()
  authPending.value = false
  stopAuthCallbackTimer()
  resetSessionUnauthorizedEvidence()
  sysMessageEnrichmentGeneration += 1
  window.clearTimeout(sysMessageExpiryTimer)
  sysMessageExpiryTimer = undefined
  currentSysMessage.value = null
  sysMessageQueue.value = []
  deferredSysMessages.value = []
  sysMessageReadPendingKey.value = ''
  sysMessageReadAllPending.value = false
  sysMessageActionError.value = ''
  recentSysMessageKeys.clear()
  taskSessionEpoch += 1
  window.clearTimeout(taskDeliveryRetryTimer)
  taskDeliveryRetryTimer = undefined
  awaitingTaskDelivery = null
  taskDeliveryFailureEventId = ''
  deferredTaskEvents.length = 0
  panelTaskStateReady.value = false
  panelHasTask.value = false
  storage.setTodoInputDraft('')
  void hidePanelWindow()
  void emitTo('panel', 'desktop-session-cleared', { sessionEpoch: taskSessionEpoch })
  requestPanelTaskState()
  socketStatus.value = env.enableMock ? 'mock' : 'closed'
  stopSessionValidation()
  clearRecoverySnapshots()
  mascotStore.showMessage(message, status, true)
}

function handleSessionExpired(context?: DesktopUnauthorizedContext) {
  if (!userStore.isAuthenticated || env.enableMock) return
  if (context && context.token !== userStore.token) return
  clearDesktopSession('登录状态已过期，请重新登录', 'remind')
}

async function confirmSessionAfterUnauthorized(context: DesktopUnauthorizedContext) {
  if (!userStore.isAuthenticated || env.enableMock || context.token !== userStore.token) return
  if (sessionRecoveryPromise) return sessionRecoveryPromise

  const validatedToken = userStore.token
  const currentUserId = userStore.userInfo?.userId || ''
  sessionRecoveryPromise = (async () => {
    const result = await validateDesktopSession(currentUserId)
    recordDesktopDiagnostic('session.validation.after_401', {
      result: result.status,
      userIdMasked: maskDiagnosticIdentifier(currentUserId),
    })
    if (
      userStore.token !== validatedToken
      || (userStore.userInfo?.userId || '') !== currentUserId
    ) return

    if (result.status === 'unauthorized') {
      const confirmation = recordConfirmedDesktopUnauthorized(
        sessionUnauthorizedEvidence,
        validatedToken,
      )
      sessionUnauthorizedEvidence = confirmation.evidence
      recordDesktopDiagnostic('session.unauthorized_confirmed', {
        confirmations: confirmation.evidence.confirmations,
        confirmationWindowMs: DESKTOP_UNAUTHORIZED_CONFIRMATION_WINDOW_MS,
        shouldExpire: confirmation.shouldExpire,
      })
      if (confirmation.shouldExpire) {
        handleSessionExpired({ token: validatedToken })
      }
      return
    }

    // A 401 from an optional business endpoint must not destroy a session that
    // the dedicated identity endpoint still accepts. Temporary validation
    // failures also keep the local session and will be retried periodically.
    if (result.status === 'valid') {
      resetSessionUnauthorizedEvidence()
      userStore.setUserInfo(result.userInfo)
      connectDesktopSockets({ force: true })
    }
  })().finally(() => {
    sessionRecoveryPromise = undefined
  })

  return sessionRecoveryPromise
}

async function validateAndRestoreSession(options: { forceReconnect?: boolean } = {}) {
  if (env.enableMock || !userStore.isAuthenticated) {
    recordDesktopDiagnostic('session.validation.skipped', {
      mockMode: env.enableMock,
      tokenPresent: Boolean(userStore.token),
      userIdPresent: Boolean(userStore.userInfo?.userId),
    })
    return
  }

  const validatedToken = userStore.token
  const currentUserId = userStore.userInfo?.userId || ''
  const result = await validateDesktopSession(currentUserId)
  recordDesktopDiagnostic('session.validation.completed', {
    result: result.status,
    userIdMasked: maskDiagnosticIdentifier(currentUserId),
    forceReconnect: Boolean(options.forceReconnect),
  })
  // A validation started for an older session must never clear or overwrite a
  // login callback that completed while the request was in flight.
  if (
    userStore.token !== validatedToken
    || (userStore.userInfo?.userId || '') !== currentUserId
  ) return

  if (result.status === 'unauthorized') {
    // A passive five-minute health probe is not user-visible proof that the
    // desktop login expired. Keep the persisted session; a real protected API
    // 401 must be confirmed twice before logout.
    return
  }

  if (result.status === 'valid') {
    resetSessionUnauthorizedEvidence()
    userStore.setUserInfo(result.userInfo)
  }

  connectDesktopSockets({ force: options.forceReconnect })
}

function startSessionValidation() {
  stopSessionValidation()
  if (env.enableMock || !userStore.isAuthenticated) return

  sessionValidationTimer = window.setInterval(() => {
    void validateAndRestoreSession()
  }, SESSION_VALIDATION_INTERVAL)
}

async function startDesktopLogin() {
  stopAuthCallbackTimer()
  // Reopening the confirmation page must keep the same state. Otherwise an
  // already-open browser tab becomes stale and its later confirmation can
  // invalidate the user's new attempt.
  const state = getOrCreateDesktopAuthState()
  const authAttempt = storage.getDesktopAuthAttempt()
  recordDesktopDiagnostic('auth.login.open_requested', {
    state,
    statePresent: Boolean(state),
    stateLength: state.length,
    authAttemptCreatedAt: authAttempt?.createdAt ?? 0,
    authAttemptAgeMs: authAttempt ? Math.max(0, Date.now() - authAttempt.createdAt) : -1,
    currentToken: userStore.token,
    currentUserId: userStore.userInfo?.userId || '',
  })
  // Clicking the mascot or login action is an explicit request to see the auth
  // flow again, even if the same card was previously hidden from its menu.
  const authPresentationWasHidden = userHiddenSystemNotificationKey === 'auth'
  userHiddenSystemNotificationKey = ''
  authPending.value = true
  authErrorMessage.value = ''
  if (authPresentationWasHidden) void syncSystemNotificationWindow()
  const opened = await openDesktopLogin(state)
  recordDesktopDiagnostic('auth.login.open_completed', { opened })
  if (!opened) {
    authPending.value = false
    authErrorMessage.value = '未能打开登录页面，请检查默认浏览器后重试。'
    return
  }

  authCallbackTimer = window.setTimeout(() => {
    authCallbackTimer = undefined
    if (!authPending.value) return
    // Elapsed time alone does not prove that login failed: browser protocol
    // prompts, page switching and enterprise endpoint protection can all delay
    // the handoff. Keep accepting the same state and offer a safe retry.
    authErrorMessage.value = '网页已登录不等于桌面授权完成；请回到刚打开的授权页点击确认。'
  }, AUTH_CALLBACK_REMINDER_DELAY)

  void hidePanelWindow()
}

function handleLogout() {
  if (!userStore.isAuthenticated) {
    mascotStore.showMessage('当前未登录', 'thinking', true)
    return
  }

  if (env.enableMock) {
    clearDesktopSession('已清除本地登录态', 'success')
    return
  }

  clearDesktopSession('已退出登录', 'success')
}

function handleDesktopAuthCallbackError(error: DesktopAuthCallbackError) {
  recordDesktopDiagnostic('auth.callback.rejected', {
    error,
    authPending: authPending.value,
  })
  if (!authPending.value) return

  stopAuthCallbackTimer()
  const message = error === 'expired'
    ? '已忽略旧确认页，请在最近打开的页面再次确认。'
    : error === 'missing-identity'
      ? '网页返回信息不完整，请在当前页面再次确认。'
      : 'Windows 未传入确认结果，请重新打开确认页。'
  authErrorMessage.value = message
}

function queueDesktopReleaseSmokeReminders() {
  const reminders: SysMessageNotification[] = [
    { bizType: 1, subject: '待办提醒' },
    { bizType: 2, subject: '会议提醒' },
    { bizType: 3, subject: '消息提醒' },
  ].map(({ bizType, subject }) => ({
    id: `release-smoke-${bizType}`,
    rawId: `release-smoke-${bizType}`,
    dedupeKey: `release-smoke-${bizType}-${Date.now()}`,
    msgSubject: subject,
    msgContent: '发布门禁消息，不包含真实用户数据。',
    msgStatus: 0,
    msgType: 1,
    bizType,
    bizId: `release-smoke-${bizType}`,
  }))

  reminders.forEach((message) => pushSysMessage(message))
  recordDesktopReleaseSmokeSession(
    userStore.isAuthenticated,
    !needsAuth.value,
    reminders.length,
  )
}

onMounted(async () => {
  removeRuntimeRecovery = await startRuntimeRecovery((state) => {
    runtimeRecovered ||= state.recovered
    runtimeInteractive.value = state.interactive
    pendingRuntimeState = state
    if (runtimeCoordinatorReady) recoverDesktopRuntime(state)
    else if (!state.interactive) notificationDelivery.suspend()
  })
  if (windowMode === 'mascot') {
    releaseSmokePrepared = await prepareDesktopReleaseSmokeState()
    recordDesktopDiagnostic('renderer.mascot_mounted', {
      token: userStore.token,
      tokenPresent: Boolean(userStore.token),
      tokenLength: userStore.token.length,
      userId: userStore.userInfo?.userId || '',
      userIdPresent: Boolean(userStore.userInfo?.userId),
      userIdMasked: maskDiagnosticIdentifier(userStore.userInfo?.userId),
      authenticated: userStore.isAuthenticated,
      releaseSmokePrepared,
    })
    if (releaseSmokePrepared && userStore.isAuthenticated) {
      recordDesktopReleaseSmokeRestart(true)
    }
    removeSystemNotificationActionListener = await listen<MascotSystemNotificationAction>(
      MASCOT_SYSTEM_NOTIFICATION_ACTION_EVENT,
      (event) => handleSystemNotificationAction(event.payload),
    )
    removeSystemNotificationLayoutListener = await listen<{ generation: number }>(
      MASCOT_SYSTEM_NOTIFICATION_LAYOUT_EVENT,
      (event) => notificationDelivery.acknowledge(event.payload.generation),
    )
    removeSystemNotificationReadyListener = await listen(
      MASCOT_SYSTEM_NOTIFICATION_READY_EVENT,
      () => {
        systemNotificationWindowReady.value = true
        recordDesktopDiagnostic('notification.renderer_ready', { ready: true })
        void syncSystemNotificationWindow()
      },
    )
    removeContextMenuVisibilityListener = await listen<MascotMenuVisibilityPayload>(
      MASCOT_CONTEXT_MENU_VISIBILITY_EVENT,
      (event) => {
        const { visible, restoreNotification } = event.payload
        const wasVisible = contextMenuWindowVisible.value
        contextMenuWindowVisible.value = visible
        recordDesktopDiagnostic('interaction.context_menu.visibility_received', {
          visible,
          restoreNotification,
          wasVisible,
        })
        if (visible) {
          notificationDelivery.sync(null)
        }
        if (!visible && !restoreNotification) {
          userHiddenSystemNotificationKey = systemNotificationMessageKey
          persistRecoveryState()
          notificationDelivery.sync(null)
          recordDesktopDiagnostic('interaction.context_menu.hide_intent', {
            presentationKind: systemNotificationMessageKey === 'auth'
              ? 'auth'
              : systemNotificationMessageKey.startsWith('message:')
                ? 'message'
                : 'none',
          })
        }
        if (wasVisible && !visible && restoreNotification) void syncSystemNotificationWindow()
      },
    )
    const nativeNotificationReady = await isMascotSystemNotificationReady()
    systemNotificationWindowReady.value ||= nativeNotificationReady
    recordDesktopDiagnostic('notification.native_ready_checked', {
      ready: systemNotificationWindowReady.value,
    })
    void syncSystemNotificationWindow()

    removePanelTaskDeliveredListener = await listen<PanelTaskDeliveredPayload>(
      PANEL_TASK_DELIVERED_EVENT,
      (event) => handleTaskDelivered(event.payload),
    )
    removePanelTaskReadyListener = await listen(
      PANEL_TASK_READY_EVENT,
      handlePanelTaskReady,
    )
    removePanelTaskStateListener = await listen<PanelTaskStatePayload>(
      PANEL_TASK_STATE_EVENT,
      (event) => handlePanelTaskState(event.payload),
    )
    // The panel also publishes immediately after installing this listener; the
    // request closes the opposite startup ordering without assuming which
    // WebView mounted first.
    requestPanelTaskState()

    removeMascotMessageListener = await listen<{
      message: string
      status?: MascotStatus
      autoReset?: boolean
    }>('mascot-message', (event) => {
      mascotStore.showMessage(event.payload.message, event.payload.status, event.payload.autoReset)
    })

    removeTaskListener = websocketService.onTask((event) => {
      if (needsAuth.value) return
      void emitTo('mascot', 'mascot-close-overlays', {})
      queueTaskForPanel(event)
      // Only the mascot WebView owns system-message state. It therefore keeps
      // the original event until every message is dismissed, then delivers it
      // to the panel WebView whose Pinia store is intentionally independent.
      void deliverTasksWhenSystemMessagesFinish()
    })
    removeStatusListener = websocketService.onStatus((status) => {
      socketStatus.value = status
      void emitTo('panel', 'socket-status', status)
    })
    removeSysMessageListener = sysMessageService.onMessage((message) => {
      recordDesktopDiagnostic('reminder.listener.received', {
        messageIdMasked: maskDiagnosticIdentifier(message.id),
        bizType: message.bizType ?? null,
      })
      pushSysMessage(message)
    })
    removeUnauthorizedListener = onDesktopUnauthorized((context) => {
      void confirmSessionAfterUnauthorized(context)
    })
    removeDeepLinkListener = await listenDesktopAuthCallbacks(
      (payload) => {
        recordDesktopDiagnostic('auth.callback.accepted', {
          token: payload.token,
          tokenPresent: Boolean(payload.token),
          tokenLength: payload.token.length,
          userId: payload.userInfo.userId,
          userName: payload.userInfo.userName,
          department: payload.userInfo.department || '',
          userIdPresent: Boolean(payload.userInfo.userId),
          userIdMasked: maskDiagnosticIdentifier(payload.userInfo.userId),
        })
        // 登录卡消失时直接恢复普通窗口，避免 Windows 在“大卡片 -> 小气泡”
        // 连续缩放中出现窗口尺寸与 WebView 渲染尺寸不同步。
        const previousUserId = userStore.userInfo?.userId
        if (previousUserId && previousUserId !== payload.userInfo.userId) {
          clearDesktopSession('', 'idle')
        }
        mascotStore.resetStatus()
        userStore.setSession(payload)
        recordDesktopDiagnostic('auth.callback.session_committed', {
          authenticated: userStore.isAuthenticated,
          token: userStore.token,
          tokenPresent: Boolean(userStore.token),
          tokenLength: userStore.token.length,
          userId: userStore.userInfo?.userId || '',
          userIdMasked: maskDiagnosticIdentifier(userStore.userInfo?.userId),
        })
        resetSessionUnauthorizedEvidence()
        authPending.value = false
        stopAuthCallbackTimer()
        authErrorMessage.value = ''
        // Hide the native login surface immediately. The regular reactive sync
        // will publish the next system message if one is queued, but a delayed
        // WebView2 paint can no longer leave the old login card on screen.
        notificationDelivery.sync(null)
        connectDesktopSockets({ force: true })
        startSessionValidation()
        if (releaseSmokePrepared) queueDesktopReleaseSmokeReminders()
        // The state-checked callback is the login completion signal. Socket and
        // message channels connect above; periodic validation continues later.
        mascotStore.showMessage('登录成功，消息提醒已开启', 'success', true)
      },
      handleDesktopAuthCallbackError,
    )
    if (!needsAuth.value) {
      void validateAndRestoreSession({ forceReconnect: true })
      startSessionValidation()
    }
    // Authentication renders in the independent notification HWND. Keep the
    // mascot's own topmost window at its 120x104 collapsed bounds on startup so
    // transparent pixels cannot become a desktop-wide click interceptor.
    await setMascotNotificationVisible(false, false)
    await waitForMascotInteractionReady()
    // A native repair may reload a deliberately hidden mascot. Restore the
    // recorded visibility rather than turning that reload into a user show.
    if (runtimeInteractive.value && !runtimeRecovered) await showAssistant()
  }

  if (windowMode === 'panel') {
    try {
      removePanelTaskListener = await listen<PanelTaskDeliveryPayload>('task-created', async (event) => {
        const delivery = event.payload
        if (panelSessionEpoch.value !== delivery.sessionEpoch) return
        // Store synchronously, then ask the mascot-side coordinator to reveal.
        // The coordinator alone knows whether a system-message card is active.
        taskStore.pushTask(delivery.event)
        await nextTick()
        await panelWindowRef.value?.syncVisiblePanelHeight()
        if (panelSessionEpoch.value !== delivery.sessionEpoch) return
        try {
          const ack: PanelTaskDeliveredPayload = {
            eventId: delivery.event.eventId,
            sessionEpoch: delivery.sessionEpoch,
          }
          await emitTo('mascot', PANEL_TASK_DELIVERED_EVENT, ack)
        } finally {
          publishPanelTaskState(true)
        }
      })
      removePanelTaskStateRequestListener = await listen<PanelTaskStateRequestPayload>(
        PANEL_TASK_STATE_REQUEST_EVENT,
        (event) => {
          if (!adoptPanelSessionEpoch(event.payload.sessionEpoch)) return
          publishPanelTaskState()
        },
      )
      removePanelSessionClearedListener = await listen<PanelTaskStateRequestPayload>(
        'desktop-session-cleared',
        (event) => {
          if (!adoptPanelSessionEpoch(event.payload.sessionEpoch)) return
          taskStore.clearTasks()
          publishPanelTaskState()
        },
      )
      void emitTo('mascot', PANEL_TASK_READY_EVENT, {})
      removeStatusListener = await listen<string>('socket-status', (event) => {
        socketStatus.value = event.payload
      })
    } catch {
      removePanelTaskListener = undefined
      removePanelTaskStateRequestListener = undefined
      removePanelSessionClearedListener = undefined
      removeStatusListener = undefined
    }
  }

  if (windowMode === 'mascot') {
    try {
      removeTrayListener = await listen('tray-open-workbench', () => openWorkbench())
      removeTrayLogoutListener = await listen('tray-logout', () => handleLogout())
    } catch {
      removeTrayListener = undefined
      removeTrayLogoutListener = undefined
    }
  }
  runtimeCoordinatorReady = true
  if (pendingRuntimeState) recoverDesktopRuntime(pendingRuntimeState)
  markRuntimeMounted()
})

onUnmounted(() => {
  removeRuntimeRecovery?.()
  removeTaskListener?.()
  removeStatusListener?.()
  removeTrayListener?.()
  removeTrayLogoutListener?.()
  removePanelTaskListener?.()
  removePanelTaskDeliveredListener?.()
  removePanelTaskReadyListener?.()
  removePanelTaskStateListener?.()
  removePanelTaskStateRequestListener?.()
  removePanelSessionClearedListener?.()
  removeMascotMessageListener?.()
  removeSysMessageListener?.()
  removeSystemNotificationActionListener?.()
  removeSystemNotificationReadyListener?.()
  removeSystemNotificationLayoutListener?.()
  notificationDelivery.dispose()
  removeContextMenuVisibilityListener?.()
  removeDeepLinkListener?.()
  removeUnauthorizedListener?.()
  stopSessionValidation()
  stopAuthCallbackTimer()
  window.clearTimeout(sysMessageExpiryTimer)
  window.clearTimeout(taskDeliveryRetryTimer)
  if (windowMode === 'mascot') {
    websocketService.disconnect()
    sysMessageService.disconnect()
  }
})
</script>

<template>
  <main class="app-shell" :class="[`is-${windowMode}`]">
    <MascotWindow
      v-if="windowMode === 'mascot'"
      :needs-auth="needsAuth"
      :sys-message="currentSysMessage"
      :system-message-visible="systemMessageWindowVisible"
      @login="startDesktopLogin"
      @ready="handleMascotInteractionReady"
    />
    <MascotMenuWindow v-else-if="windowMode === 'mascot-menu'" />
    <MascotNotificationWindow v-else-if="windowMode === 'mascot-notification'" />
    <PanelWindow v-else ref="panelWindowRef" :socket-status="socketStatus" :mock-enabled="env.enableMock" :task="currentTask" />
  </main>
</template>
