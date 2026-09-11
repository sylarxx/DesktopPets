import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import appSource from '../App.vue?raw'
import deliverySource from './notification-delivery.ts?raw'
import authLoginTipSource from '../components/AuthLoginTip.vue?raw'
import taskCardSource from '../components/TaskPushCard.vue?raw'
import sysMessageTipSource from '../components/SysMessageTip.vue?raw'
import todoInputSource from '../components/TodoInputBox.vue?raw'
import sysMessageServiceSource from '../services/sys-message.service.ts?raw'
import windowServiceSource from '../services/window.service.ts?raw'
import taskStoreSource from '../stores/task.ts?raw'
import mascotNotificationWindowSource from '../views/MascotNotificationWindow.vue?raw'
import mascotWindowSource from '../views/MascotWindow.vue?raw'
import panelSource from '../views/PanelWindow.vue?raw'
import rustSource from '../../src-tauri/src/main.rs?raw'
import tauriConfigSource from '../../src-tauri/tauri.conf.json?raw'

const appStyles = readFileSync(new URL('../assets/styles/app.css', import.meta.url), 'utf8')

function section(source: string, startMarker: string, endMarker: string) {
  // Git can materialize raw source imports with CRLF on the Windows release
  // runner. Normalize only for contract extraction so the same assertions run
  // identically on macOS and Windows without weakening their source ordering.
  const normalizedSource = source.replace(/\r\n?/g, '\n')
  const start = normalizedSource.indexOf(startMarker)
  const end = normalizedSource.indexOf(endMarker, start + startMarker.length)
  expect(start, `missing section start: ${startMarker}`).toBeGreaterThanOrEqual(0)
  expect(end, `missing section end: ${endMarker}`).toBeGreaterThan(start)
  return normalizedSource.slice(start, end)
}

function normalize(source: string) {
  return source.replace(/\s+/g, ' ').trim()
}

function expectInOrder(source: string, ...fragments: string[]) {
  const normalizedSource = normalize(source)
  let cursor = 0
  for (const fragment of fragments) {
    const normalizedFragment = normalize(fragment)
    const index = normalizedSource.indexOf(normalizedFragment, cursor)
    expect(index, `missing or out-of-order contract fragment: ${normalizedFragment}`).toBeGreaterThanOrEqual(cursor)
    cursor = index + normalizedFragment.length
  }
}

describe('notification and task production contracts', () => {
  it('stores a panel task and returns the matching eventId ACK before requesting reveal', () => {
    const panelDeliveryListener = section(
      appSource,
      "removePanelTaskListener = await listen<PanelTaskDeliveryPayload>('task-created'",
      'removePanelTaskStateRequestListener = await listen<PanelTaskStateRequestPayload>',
    )

    expectInOrder(
      panelDeliveryListener,
      'if (panelSessionEpoch.value !== delivery.sessionEpoch) return',
      'taskStore.pushTask(delivery.event)',
      'eventId: delivery.event.eventId',
      'sessionEpoch: delivery.sessionEpoch',
      "await emitTo('mascot', PANEL_TASK_DELIVERED_EVENT, ack)",
      'publishPanelTaskState(true)',
    )
    expect(panelSource).toContain('<TaskPushCard')
    expect(panelSource).toContain('v-if="task"')
    expect(panelSource).toContain('<TodoInputBox')
    expect(panelSource).toContain('v-else')
  })

  it('keeps the queue head until a matching eventId and sessionEpoch ACK arrives', () => {
    const delivery = section(
      appSource,
      'async function deliverTasksWhenSystemMessagesFinish()',
      'function handleTaskDelivered',
    )
    const acknowledge = section(
      appSource,
      'function handleTaskDelivered',
      'function requestPanelTaskState',
    )

    expectInOrder(
      delivery,
      'const event = deferredTaskEvents[0]',
      'awaitingTaskDelivery = {',
      'eventId: event.eventId',
      'sessionEpoch: taskSessionEpoch',
      "await emitTo('panel', 'task-created', delivery)",
      'window.setTimeout',
      'awaitingTaskDelivery = null',
      'void deliverTasksWhenSystemMessagesFinish()',
    )
    expect(delivery).not.toContain('deferredTaskEvents.shift()')
    expectInOrder(
      acknowledge,
      'payload.sessionEpoch !== taskSessionEpoch',
      'awaitingTaskDelivery?.sessionEpoch !== payload.sessionEpoch',
      'awaitingTaskDelivery.eventId !== payload.eventId',
      'deferredTaskEvents[0]?.eventId !== payload.eventId',
      'awaitingTaskDelivery = null',
      'deferredTaskEvents.shift()',
      'void deliverTasksWhenSystemMessagesFinish()',
    )
  })

  it('rejects stale sessions and does not deliver or reveal tasks while authentication is required', () => {
    const delivery = section(
      appSource,
      'async function deliverTasksWhenSystemMessagesFinish()',
      'function handleTaskDelivered',
    )
    const reveal = section(appSource, 'async function showTaskPanelWithFallback()', 'function queueTaskForPanel')
    const panelState = section(appSource, 'function handlePanelTaskState', 'watch(currentTask')
    const panelEpoch = section(appSource, 'function adoptPanelSessionEpoch', 'async function showTaskPanelWithFallback')
    const taskPushListener = section(
      appSource,
      'removeTaskListener = websocketService.onTask',
      'removeStatusListener = websocketService.onStatus',
    )

    expect(delivery).toContain('needsAuth.value')
    expect(delivery).toContain('!panelTaskStateReady.value')
    expect(reveal).toContain('needsAuth.value')
    expect(reveal).toContain('currentSysMessage.value')
    expect(panelState).toContain('payload.sessionEpoch !== taskSessionEpoch')
    expect(panelState).toContain('void deliverTasksWhenSystemMessagesFinish()')
    expect(panelEpoch).toContain('sessionEpoch < panelSessionEpoch.value')
    expect(panelEpoch).toContain('taskStore.clearTasks()')
    expect(taskPushListener).toContain('if (needsAuth.value) return')
  })

  it('confirms a reported 401 before clearing the session and makes authentication clicks actionable', () => {
    const confirmation = section(
      appSource,
      'async function confirmSessionAfterUnauthorized',
      'async function validateAndRestoreSession',
    )
    const unauthorizedListener = section(
      appSource,
      'removeUnauthorizedListener = onDesktopUnauthorized',
      'removeDeepLinkListener = await listenDesktopAuthCallbacks',
    )
    const authCallback = section(
      appSource,
      'removeDeepLinkListener = await listenDesktopAuthCallbacks',
      'if (!needsAuth.value)',
    )
    const mascotToggle = section(
      mascotWindowSource,
      'function togglePanel()',
      'function playTransientAnimation',
    )
    const mascotClick = section(
      mascotWindowSource,
      'function scheduleAvatarClick',
      'function dismissTransientOverlays',
    )

    expectInOrder(
      confirmation,
      'context.token !== userStore.token',
      'if (sessionRecoveryPromise) return sessionRecoveryPromise',
      'const result = await validateDesktopSession(currentUserId)',
      "if (result.status === 'unauthorized')",
      'recordConfirmedDesktopUnauthorized',
      'sessionUnauthorizedEvidence = confirmation.evidence',
      'if (confirmation.shouldExpire)',
      'handleSessionExpired({ token: validatedToken })',
      "if (result.status === 'valid')",
      'resetSessionUnauthorizedEvidence()',
      'userStore.setUserInfo(result.userInfo)',
    )
    expect(unauthorizedListener).toContain('void confirmSessionAfterUnauthorized(context)')
    expect(appSource).toContain('A passive five-minute health probe is not user-visible proof')
    expectInOrder(
      authCallback,
      'userStore.setSession(payload)',
      'stopAuthCallbackTimer()',
      'notificationDelivery.sync(null)',
      'connectDesktopSockets({ force: true })',
    )
    expectInOrder(mascotToggle, 'if (props.needsAuth)', "emit('login')", 'canOpenMascotTodoPanel(false')
    expectInOrder(
      mascotClick,
      'classifyMascotClickContinuation(pendingAvatarClick, click)',
      "continuation === 'duplicate'",
      "continuation === 'double'",
      'clearAvatarClickSequence()',
      'void openWorkbench()',
    )
    expect(appSource).toContain('@login="startDesktopLogin"')
    expect(appSource).toContain('const AUTH_CALLBACK_REMINDER_DELAY = 2 * 60 * 1000')
    expect(appSource).toContain('const state = getOrCreateDesktopAuthState()')
    expect(appSource).toContain('网页已登录不等于桌面授权完成')
    expect(appSource).not.toContain("authPending.value = false\n    authErrorMessage.value = '未收到网页确认")
    expect(authLoginTipSource).toContain("'has-error': message && !pending")
  })

  it('uses one coordinator as the sole task-panel reveal authority', () => {
    const delivery = section(
      appSource,
      'async function deliverTasksWhenSystemMessagesFinish()',
      'function handleTaskDelivered',
    )
    const panelState = section(appSource, 'function handlePanelTaskState', 'watch(currentTask')

    expect(delivery).toContain('await showTaskPanelWithFallback()')
    expect(panelState).toContain('void deliverTasksWhenSystemMessagesFinish()')
    expect(panelState).not.toContain('showTaskPanelWithFallback()')
    // One definition plus one coordinator-owned call. A second call site can
    // race the ACK/state callback and reveal/animate the same panel twice.
    expect(appSource.match(/showTaskPanelWithFallback\(\)/g)).toHaveLength(2)
  })

  it('keeps panel failures inline and prevents a competing mascot bubble', () => {
    const taskAction = section(taskStoreSource, 'async handleAction', '\n    }\n  }')
    const panelTaskAction = section(panelSource, 'async function handleTaskAction', '</script>')
    const todoSubmit = section(panelSource, 'async function submitTodo', 'async function handleTaskAction')
    const mascotTemplate = section(mascotWindowSource, '<template>', '</template>')

    expectInOrder(taskAction, 'catch (error)', 'task.error =', 'return false', 'finally')
    expect(taskCardSource).toContain('v-if="task.error"')
    expect(taskCardSource).toContain('class="task-card__error" role="alert"')
    expect(panelTaskAction).not.toContain('showMascotMessage(')
    expectInOrder(todoSubmit, 'if (!opened)', 'submitError.value =', 'return')
    expect(todoInputSource).toContain('class="todo-input__error"')
    expect(todoInputSource).toContain('role="alert"')

    // The long-lived auth and system cards render in the isolated notification
    // HWND. The mascot HWND expands only for short compact bubbles.
    expect(mascotTemplate).toContain('mode="out-in"')
    expect(mascotTemplate).toContain('v-if="!isContextMenuVisible && mascotStore.message"')
    expect(mascotWindowSource).not.toContain('AuthLoginTip')
    expect(mascotWindowSource).not.toContain('SysMessageTip')
    expect(mascotNotificationWindowSource).toContain('<AuthLoginTip')
    expect(mascotNotificationWindowSource).toContain('<SysMessageTip')
    expect(mascotWindowSource).not.toMatch(/watch\(\s*hasBubbleMessage\s*,[\s\S]{0,260}?hidePanelWindow\(\)/)
  })

  it('wakes a hidden assistant without activation for bubbles, system cards and pushed tasks', () => {
    const notificationReveal = section(
      mascotWindowSource,
      'async function revealNotificationAfterLayout',
      'function scheduleIdleHide',
    )
    const taskReveal = section(appSource, 'async function showTaskPanelWithFallback()', 'function queueTaskForPanel')
    const nativeNotification = section(rustSource, 'fn show_notification_window', '#[tauri::command]\nfn peek_mascot_window')
    const nativePanel = section(rustSource, 'fn show_panel_window', '#[tauri::command]\nfn hide_panel_window')

    expect(notificationReveal).toContain('force: visible || attempt > 0')
    expect(notificationReveal).toContain('shown = await showNotificationWindow()')
    expect(taskReveal).not.toContain('showNotificationWindow()')
    expect(taskReveal).toContain('await showPanelWindow({ focus: false })')
    expect(nativeNotification).toContain('show_interactive_window(&window, false)')
    expectInOrder(nativePanel, 'restore_mascot_if_peeked', 'place_panel_near_mascot', 'show_interactive_window(&mascot, false)', 'show_interactive_window(&panel, should_focus)')
  })

  it('shows a notification only after bounds succeed and retries the full pipeline once', () => {
    const syncLayout = section(
      mascotWindowSource,
      'async function syncNativeNotificationLayout',
      'function notificationLayoutStillDesired',
    )
    const reveal = section(
      mascotWindowSource,
      'async function revealNotificationAfterLayout',
      'function scheduleIdleHide',
    )

    expectInOrder(syncLayout, 'const synced = await setMascotNotificationVisible', 'if (synced)', 'nativeNotificationLayout = { visible, compact }')
    expectInOrder(reveal, 'const synced = await syncNativeNotificationLayout', 'if (synced && visible && notificationCoordinatorReady)', 'shown = await showNotificationWindow()', 'if (synced && shown) return', 'if (attempt >= 1) return', 'attempt + 1')
    expect(reveal.match(/showNotificationWindow\(\)/g)).toHaveLength(1)
    expect(mascotWindowSource).toContain("{ immediate: true, flush: 'post' }")
    expect(rustSource).toMatch(/fn set_window_bounds[\s\S]{0,220}?Result<\(\), String>/)
    expect(rustSource).toMatch(/fn resize_mascot_for_notification[\s\S]{0,320}?Result<\(\), String>/)
    expect(rustSource).toMatch(/fn set_mascot_notification_visible[\s\S]{0,620}?-> bool/)
  })

  it('keeps system-message rendering in a fixed independent HWND', () => {
    const tauriConfig = JSON.parse(tauriConfigSource) as {
      app: { windows: Array<Record<string, unknown>> }
    }
    const notificationWindow = tauriConfig.app.windows.find(
      (window) => window.label === 'mascot-notification',
    )
    const sync = section(
      appSource,
      'async function syncSystemNotificationWindow',
      'function handleSystemNotificationAction',
    )
    const nativeShow = section(
      rustSource,
      'fn show_mascot_system_notification_window',
      '#[tauri::command]\nfn hide_mascot_system_notification_window',
    )
    const mascotResize = section(
      rustSource,
      'fn set_mascot_notification_visible',
      '#[tauri::command]\nfn set_panel_height',
    )
    const startup = section(
      appSource,
      'if (!needsAuth.value)',
      'await showAssistant()',
    )

    expect(notificationWindow).toMatchObject({
      width: 320,
      height: 320,
      transparent: true,
      alwaysOnTop: true,
      visible: false,
      resizable: false,
    })
    const delivery = section(appSource, 'const notificationDelivery =', 'let isDeliveringDeferredTasks')
    expectInOrder(delivery, "emitTo( 'mascot-notification'", 'await showNotificationWindow()', "await showMascotSystemNotificationWindow( presentation.kind === 'auth', syncGeneration, )")
    expect(sync).toContain('notificationDelivery.sync(')
    expectInOrder(deliverySource, 'const ready = await waitForLayout', 'if (!isCurrent(token)) return', 'if (ready) success = await options.show')
    expect(mascotWindowSource).toContain('() => props.needsAuth')
    expect(appSource).toContain("kind: 'auth'")
    expect(appSource).toContain("kind: 'message'")
    expect(rustSource).toContain('const MASCOT_AUTH_NOTIFICATION_HEIGHT: f64 = 192.0;')
    expect(mascotResize).toContain('let compact = visible')
    expect(startup).toContain('await setMascotNotificationVisible(false, false)')
    expect(startup).toContain('await waitForMascotInteractionReady()')
    expect(appSource).toContain('@ready="handleMascotInteractionReady"')
    expect(mascotWindowSource).toContain("emit('ready')")
    expect(startup).not.toContain("请先登录后接收消息")
    expect(nativeShow).toContain('get_webview_window("mascot-notification")')
    expect(nativeShow).toContain('position_mascot_system_notification_window')
    expect(nativeShow).toContain('show_interactive_window(&notification, false)')
    expect(delivery).toContain('hide: (generation) => hideMascotSystemNotificationWindow(generation)')
    expect(mascotNotificationWindowSource).not.toContain('hideMascotSystemNotificationWindow')
  })

  it('ACKs committed card layout without a hidden-window transition and gates waving on display', () => {
    const apply = section(mascotNotificationWindowSource, 'async function applyPresentation', 'async function announceReady')
    expectInOrder(apply, 'delivery.generation <= deliveryGeneration', 'presentation.value = delivery.presentation', 'await nextTick()', 'delivery.generation !== deliveryGeneration', 'getBoundingClientRect()', 'bounds.width <= 0', "await emitTo('mascot', MASCOT_SYSTEM_NOTIFICATION_LAYOUT_EVENT")
    expect(mascotNotificationWindowSource).not.toContain('<Transition')
    expect(mascotNotificationWindowSource).not.toContain('requestAnimationFrame(')
    expect(appSource).toContain('notificationDelivery.acknowledge(event.payload.generation)')
    expect(appSource).toContain(':system-message-visible="systemMessageWindowVisible"')
    expect(mascotWindowSource).toContain("props.sysMessage && props.systemMessageVisible && waveCycles.value > 0 ? 'waving'")
    expect(appSource).toContain('visibleSystemNotification.value.message.dedupeKey === currentSysMessage.value?.dedupeKey')
  })

  it('keeps the detached system card attached to the mascot throughout native dragging', () => {
    const dragMonitor = section(
      rustSource,
      'fn monitor_native_drag',
      '#[derive(Clone, serde::Serialize)]',
    )
    const nativeSync = section(
      rustSource,
      'fn sync_visible_mascot_system_notification_to_mascot',
      '#[tauri::command]\nfn set_mascot_system_notification_ready',
    )

    expectInOrder(
      dragMonitor,
      'let mut last_mascot_position = None',
      'if mascot_position != last_mascot_position',
      'sync_visible_mascot_system_notification_to_mascot(&app)',
      'thread::sleep(Duration::from_millis(MASCOT_DOCK_ANIMATION_FRAME_MS))',
    )
    expect(nativeSync).toContain('is_visible()')
    expect(nativeSync).toContain('position_mascot_system_notification_window(&mascot, &notification, compact)')
    expect(rustSource).toMatch(/WindowEvent::Moved\(_\)[\s\S]{0,260}?cfg\(not\(windows\)\)[\s\S]{0,120}?sync_visible_mascot_system_notification_to_mascot/)
    expect(rustSource).toContain('return set_window_physical_position_if_changed(window, position)')
    expect(rustSource).toContain('SWP_NOSIZE')
  })

  it('synchronizes both half-head transitions with the native side slide', () => {
    const runtimeAnimation = section(
      mascotWindowSource,
      'const avatarAnimationState = computed',
      'const isNotifying = computed',
    )
    const idleHide = section(
      mascotWindowSource,
      'function scheduleIdleHide()',
      'function shouldPauseIdleHide()',
    )

    expectInOrder(
      idleHide,
      'peekMascotWindow(reducedMotion)',
      'peekSide.value = side',
      "peekTransition.value = 'peeking'",
    )
    expect(runtimeAnimation).toContain("peekTransition.value === 'revealing'")
    expect(runtimeAnimation).toContain("peekSide.value === 'left' ? 'revealing-left' : 'revealing'")
    expect(runtimeAnimation).toContain("peekTransition.value === 'peeking' || isPeeked.value")
    expect(runtimeAnimation).toContain("peekSide.value === 'left' ? 'peeking-left' : 'peeking'")
    expect(mascotWindowSource).toContain('const peekHideDurationMs = mascotAnimationTiming.peeking.durationMs')
    expect(mascotWindowSource).toContain('const peekRevealDurationMs = mascotAnimationTiming.revealing.durationMs')
    expect(rustSource).toContain('const MASCOT_PEEK_ANIMATION_DURATION_MS: u64 = 560;')
    expect(rustSource).toContain('const MASCOT_REVEAL_ANIMATION_DURATION_MS: u64 = 480;')
    expect(rustSource).toContain('fn monitor_mascot_peek_hover(')
    expect(rustSource).toContain('GetCursorPos(&mut cursor)')
    expect(rustSource).toContain('physical_rect_intersection(')
    expect(rustSource).toContain('MASCOT_NATIVE_HOVER_REVEALED_EVENT')
    expect(mascotWindowSource).toContain('listen(MASCOT_NATIVE_HOVER_REVEALED_EVENT')
    expect(mascotWindowSource).toContain('startPeekReveal(false)')
  })

  it('reveals the first mascot HWND only after renderer readiness and reasserts hit testing', () => {
    const nativeShow = section(
      rustSource,
      'fn show_main_window(',
      '#[tauri::command]\nfn show_notification_window',
    )
    const rendererReady = section(
      mascotWindowSource,
      'onMounted(async () =>',
      'onUnmounted(() =>',
    )

    expectInOrder(
      rendererReady,
      'await syncNativeNotificationLayout(',
      'notificationCoordinatorReady = true',
      'await nextTick()',
      'requestAnimationFrame(() => requestAnimationFrame(() => resolve()))',
      "emit('ready')",
    )
    expectInOrder(
      nativeShow,
      'show_interactive_window(&window, true)',
      'MASCOT_NATIVE_REVEALED_EVENT',
      'schedule_mascot_interactivity_recovery(',
      'return true',
    )
    expect(windowServiceSource).toContain("invoke<boolean>('show_main_window')")
  })

  it('drops stale reminders and schedules the nearest 30-minute expiry', () => {
    const pushHandler = section(appSource, 'function pushSysMessage', 'function hideCurrentSysMessage')
    const expiryScheduler = section(
      appSource,
      'function expireStaleSysMessages',
      'function applyEnrichedSysMessage',
    )

    expectInOrder(
      pushHandler,
      'resolveSysMessageExpiresAt(message.createTime)',
      'if (!showIncomingSysMessage(resolvedMessage)) {',
      "reason: 'expired'",
      'return',
      'void enrichSysMessage(message, generation)',
    )
    expectInOrder(
      expiryScheduler,
      'isSysMessageExpired(message.expiresAt, now)',
      'isSysMessageExpired(currentSysMessage.value.expiresAt, now)',
      'showNextSysMessage(now)',
      'const nextExpiry = Math.min(...expiries)',
      'window.setTimeout',
      'expireStaleSysMessages()',
      'scheduleSysMessageExpiry()',
    )
  })

  it('makes every hidden detached overlay click-through and hides stale presentations natively', () => {
    const safeHide = section(
      rustSource,
      'fn hide_transparent_window_safely',
      'fn show_interactive_window',
    )
    const safeShow = section(
      rustSource,
      'fn show_interactive_window',
      'fn mascot_logical_size',
    )
    const nativeHide = section(
      rustSource,
      'fn hide_mascot_system_notification_native_window',
      'fn position_mascot_system_notification_window',
    )
    const nativeShow = section(
      rustSource,
      'fn show_mascot_system_notification_window',
      '#[tauri::command]\nfn hide_mascot_system_notification_window',
    )
    const sync = section(
      appSource,
      'async function syncSystemNotificationWindow',
      'function handleSystemNotificationAction',
    )

    expectInOrder(safeHide, 'set_ignore_cursor_events(true)', 'window.hide()')
    expectInOrder(safeShow, 'set_ignore_cursor_events(false)', 'show_window_without_activation')
    expectInOrder(nativeHide, 'state.request_hide(client_generation)', 'state.transition.lock()', 'state.can_hide(generation)', 'hide_transparent_window_safely(&window)', 'state.mark_physical_hidden()')
    expectInOrder(nativeShow, 'state.request_show(compact, client_generation)', 'state.transition.lock()', 'state.can_show(generation, compact)', 'position_mascot_system_notification_window', 'show_interactive_window(&notification, false)', 'state.mark_visible(generation, compact)')
    expectInOrder(sync, 'if (!presentation)', 'notificationDelivery.sync(', 'suppressedByUserHide || contextMenuWindowVisible.value ? null : presentation')
    expectInOrder(deliverySource, 'if (presentation === null)', 'const generation = options.nextGeneration()', 'options.hide(generation)', 'options.publish({ generation, presentation: null })')
    expect(appSource).toContain('nextGeneration: () => ++systemNotificationSyncGeneration')
    expect(appSource).toContain('let systemNotificationSyncGeneration = Date.now() * 1000')
  })

  it('recovers a staged mascot collapse natively if the renderer misses its finish IPC', () => {
    const recovery = section(
      rustSource,
      'fn schedule_mascot_collapse_recovery',
      '#[tauri::command]\nfn finish_mascot_notification_collapse',
    )
    const collapse = section(
      rustSource,
      'fn set_mascot_notification_visible',
      '#[tauri::command]\nfn set_panel_height',
    )

    expect(rustSource).toContain('const MASCOT_COLLAPSE_RECOVERY_TIMEOUT_MS: u64 = 1500;')
    expectInOrder(
      recovery,
      'thread::sleep',
      'let was_visible = matches!(window.is_visible(), Ok(true))',
      'restore_staged_position_for_generation(&window, generation)',
      'if was_visible',
      'show_interactive_window(&window, false)',
    )
    expectInOrder(
      collapse,
      'stage_collapsed_position(target_position)',
      'PhysicalPosition::new(-32_000, -32_000)',
      'show_window_without_activation(&window)',
      'schedule_mascot_collapse_recovery',
    )
  })

  it('shows notification actions and login feedback without ellipsis', () => {
    const actions = section(appStyles, '.sys-message-tip__actions {', '.sys-message-tip__button {')
    const button = section(appStyles, '.sys-message-tip__button {', '.sys-message-tip__button--primary {')
    const bubble = section(appStyles, '.mascot-bubble__text {', '.mascot-bubble__tail {')
    const loginButton = section(appStyles, '.auth-login-tip__button {', '.auth-login-tip__button:hover {')

    expect(actions).toContain('display: grid')
    expect(actions).toContain('grid-template-columns: repeat(2, minmax(0, 1fr))')
    expect(appStyles).toMatch(
      /\.sys-message-tip__actions\.has-read-all\s*\{[\s\S]*?grid-template-columns: repeat\(3, minmax\(0, 1fr\)\)/,
    )
    expect(sysMessageTipSource).toContain("'has-read-all': (pendingCount ?? 0) > 0")
    for (const completeTextRegion of [button, bubble, loginButton]) {
      expect(completeTextRegion).toContain('white-space: normal')
      expect(completeTextRegion).not.toContain('text-overflow: ellipsis')
    }
  })

  it('reserves two unclipped lines for every login guidance state', () => {
    const detachedLogin = section(
      appStyles,
      '.mascot-notification-window .auth-login-tip {',
      '.mascot-notification-window .sys-message-tip__card {',
    )
    const loginBody = section(appStyles, '.auth-login-tip__body {', '.auth-login-tip__body strong {')
    const loginCopy = section(appStyles, '.auth-login-tip__body p {', '.auth-login-tip__button {')

    expect(detachedLogin).toContain('max-height: none')
    expect(detachedLogin).toContain('flex: 0 0 auto')
    expect(loginBody).toContain('overflow: visible')
    expect(loginCopy).toContain('min-height: 40px')
    expect(loginCopy).toContain('line-height: 19px')
    expect(loginCopy).toContain('overflow: visible')
    expect(mascotNotificationWindowSource).toContain("preview === 'auth-pending'")
  })

  it('offers one-click batch read only when more than one reminder is pending', () => {
    const batchService = section(
      sysMessageServiceSource,
      'async markAllRead',
      'disconnect()',
    )
    const batchHandler = section(
      appSource,
      'async function handleAllSysMessagesRead',
      'async function handleSysMessageView',
    )

    expect(sysMessageTipSource).toContain('v-if="(pendingCount ?? 0) > 0"')
    expect(sysMessageTipSource).toContain("全部已读")
    expect(sysMessageTipSource).toContain("emit('readAll')")
    expectInOrder(batchService, 'unreadMessages', 'new Set', "request.put<unknown, boolean>('/sys-message/read', { ids })", 'markedRead !== true', 'message.msgStatus = 1')
    expectInOrder(batchHandler, 'snapshot.length < 2', 'sysMessageReadAllPending.value = true', 'await sysMessageService.markAllRead(snapshot)', 'snapshotKeys', 'currentSysMessage.value = remainingMessages.shift() ?? null', 'catch (error)', '消息已保留')
  })

  it('clears queued delivery state and invalidates old panel events on session clear', () => {
    const clearSession = section(appSource, 'function clearDesktopSession', 'function handleSessionExpired')
    const panelClearListener = section(
      appSource,
      "removePanelSessionClearedListener = await listen<PanelTaskStateRequestPayload>",
      "void emitTo('mascot', PANEL_TASK_READY_EVENT",
    )

    expectInOrder(
      clearSession,
      'taskSessionEpoch += 1',
      'window.clearTimeout(taskDeliveryRetryTimer)',
      'awaitingTaskDelivery = null',
      'deferredTaskEvents.length = 0',
      'panelTaskStateReady.value = false',
      'panelHasTask.value = false',
      "emitTo('panel', 'desktop-session-cleared', { sessionEpoch: taskSessionEpoch })",
    )
    expectInOrder(panelClearListener, 'adoptPanelSessionEpoch(event.payload.sessionEpoch)', 'taskStore.clearTasks()', 'publishPanelTaskState()')
  })

  it('uses safe focus defaults and focuses the task card only after explicit user activation', () => {
    const showPanel = section(windowServiceSource, 'export async function showPanelWindow', 'export async function hidePanelWindow')
    const panelReveal = section(panelSource, 'function playPanelReveal', 'onMounted')
    const nativePanel = section(rustSource, 'fn show_panel_window', '#[tauri::command]\nfn hide_panel_window')
    const nativeToggle = section(rustSource, 'fn toggle_panel_window', '#[tauri::command]\nfn show_panel_window')

    expect(showPanel).toContain('focus: options.focus ?? false')
    expect(showPanel).toContain('if (visible)')
    expect(panelReveal).toContain('if (options.focus)')
    expect(panelReveal).toContain('window.setTimeout(focusVisibleControl, 80)')
    expect(nativePanel).toContain('let should_focus = focus.unwrap_or(false)')
    expect(nativePanel).toContain('show_interactive_window(&panel, should_focus)')
    expect(nativeToggle).toContain('show_interactive_window(&panel, true)')
    expect(taskCardSource).toContain('tabindex="-1"')
    expect(taskCardSource).not.toContain('autofocus')
  })

  it('treats markRead data false as failure and keeps the system card visible', () => {
    const markRead = section(sysMessageServiceSource, 'async markRead', 'disconnect()')
    const readHandler = section(appSource, 'async function handleSysMessageRead', 'async function handleSysMessageView')
    const viewHandler = section(appSource, 'async function handleSysMessageView', 'function connectDesktopSockets')

    expectInOrder(markRead, 'const markedRead = await request.put', "if (markedRead !== true) throw new Error('服务端未确认消息已读')", 'message.msgStatus = 1')
    expectInOrder(readHandler, 'await sysMessageService.markRead(message)', 'hideCurrentSysMessage(message)', 'catch (error)', "sysMessageActionError.value = '未能标记已读，请检查网络后重试'")
    expectInOrder(viewHandler, 'const opened = await openSysMessageDetail(message)', 'if (!opened)', 'await sysMessageService.markRead(message)', 'catch (error)', 'return', 'hideCurrentSysMessage(message)')
  })

  it('renders message fallback immediately and enriches each message independently', () => {
    const pushHandler = section(appSource, 'function pushSysMessage', 'function hideCurrentSysMessage')

    expectInOrder(pushHandler, 'getSysMessageFallback(message)', 'showIncomingSysMessage(resolvedMessage)', 'void enrichSysMessage(message, generation)')
    expect(appSource).not.toContain('sysMessageResolutionQueue')
    expect(appSource).not.toContain('isResolvingSysMessage')
  })

  it('keeps unsupported task actions out and failed workbench drafts retryable', () => {
    const submitHandler = section(panelSource, 'async function submitTodo', 'async function handleTaskAction')

    expect(taskCardSource).not.toContain("'later'")
    expect(taskCardSource).not.toContain('16:00')
    expect(taskCardSource).not.toContain('确认会议纪要')
    expect(taskCardSource).toContain('getVisibleTaskActions')
    expectInOrder(submitHandler, 'if (!opened)', 'return', 'inputBoxRef.value?.clear()', 'hidePanelWindow()')
    expect(todoInputSource).toContain('<label class="sr-only"')
    expect(appStyles).toMatch(/\.pet-prompt \.todo-input:focus-within\s*\{[\s\S]*?box-shadow:/)
  })

  it('keeps variable notification text scrollable with production-sized action targets', () => {
    expect(appStyles).toMatch(/\.sys-message-tip__body h2\s*\{[\s\S]*?overflow-y: auto;/)
    expect(appStyles).toMatch(/\.sys-message-tip__error\s*\{[\s\S]*?overflow-y: auto;/)
    expect(appStyles).toMatch(/\.pet-prompt\.has-task \.task-card__action\s*\{[\s\S]*?min-height: 40px;/)
    expect(appStyles).toMatch(/@media \(max-height: 440px\)[\s\S]*?\.sys-message-tip__button\s*\{[\s\S]*?min-height: 40px;/)
  })

  it('provides deterministic DEV previews for long, queued and error states', () => {
    expect(appSource).toContain("searchParams.get('preview') === 'task'")
    expect(appSource).toContain("searchParams.get('previewQueue') === '1'")
    expect(appSource).toContain("searchParams.get('previewError') === '1'")
    expect(appSource).toContain("searchParams.get('previewLong') === '1'")
  })
})
