<script setup lang="ts">
import { emitTo, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { nextTick, onMounted, onUnmounted, ref } from 'vue'
import AuthLoginTip from '../components/AuthLoginTip.vue'
import SysMessageTip from '../components/SysMessageTip.vue'
import {
  MASCOT_SYSTEM_NOTIFICATION_ACTION_EVENT,
  MASCOT_SYSTEM_NOTIFICATION_PRESENT_EVENT,
  MASCOT_SYSTEM_NOTIFICATION_LAYOUT_EVENT,
  setMascotSystemNotificationReady,
  type MascotSystemNotificationAction,
  type MascotSystemNotificationPresentation,
  type MascotSystemNotificationDelivery,
} from '../services/window.service'

const presentation = ref<MascotSystemNotificationPresentation | null>(null)
const notificationWindow = ref<HTMLElement | null>(null)
let deliveryGeneration = 0
let disposed = false
let readyRetryTimer: number | undefined
let readyAttempts = 0
const placement = ref('above')
let removePlacementListener: UnlistenFn | undefined
let removePresentationListener: UnlistenFn | undefined
const preview = import.meta.env.DEV
  ? new URLSearchParams(window.location.search).get('preview')
  : null

if (preview === 'auth' || preview === 'auth-pending') {
  presentation.value = {
    kind: 'auth',
    generation: 1,
    pending: preview === 'auth-pending',
    message: '',
  }
} else if (preview === 'sys-message') {
  presentation.value = {
    kind: 'message',
    generation: 1,
    message: {
      id: 'notification-window-preview',
      rawId: 'notification-window-preview',
      dedupeKey: 'notification-window-preview',
      msgSubject: '会议即将开始',
      msgContent: '您的项目评审会议将在 15 分钟后开始，请提前准备相关材料。',
      msgStatus: 0,
      msgType: 1,
      bizType: 2,
      bizId: 'notification-window-preview',
      createTime: '2026-08-21 16:55',
    },
    displayContent: '您的项目评审会议将在 15 分钟后开始，请提前准备相关材料。',
    pendingCount: 2,
    readPending: false,
    readAllPending: false,
    actionError: '',
  }
}

function publishAction(action: MascotSystemNotificationAction) {
  void emitTo('mascot', MASCOT_SYSTEM_NOTIFICATION_ACTION_EVENT, action)
}

function handleRead() {
  if (presentation.value?.kind !== 'message') return
  publishAction({ action: 'read', message: presentation.value.message })
}

function handleReadAll() {
  publishAction({ action: 'readAll' })
}

function handleView() {
  if (presentation.value?.kind !== 'message') return
  publishAction({ action: 'view', message: presentation.value.message })
}

function handleLogin() {
  publishAction({ action: 'login' })
}

async function applyPresentation(delivery: MascotSystemNotificationDelivery) {
  if (disposed || delivery.generation <= deliveryGeneration) return
  deliveryGeneration = delivery.generation
  presentation.value = delivery.presentation
  await nextTick()
  if (disposed || delivery.generation !== deliveryGeneration || !delivery.presentation) return
  // Hidden WebViews may suspend animation frames. A synchronous layout read
  // after Vue's flush confirms the card without waiting for a visible HWND.
  const card = notificationWindow.value?.firstElementChild
  const bounds = card?.getBoundingClientRect()
  if (!bounds || bounds.width <= 0 || bounds.height <= 0) return
  await emitTo('mascot', MASCOT_SYSTEM_NOTIFICATION_LAYOUT_EVENT, {
    generation: delivery.generation,
  })
}

async function announceReady() {
  if (disposed || readyAttempts >= 3) return
  readyAttempts += 1
  if (!await setMascotSystemNotificationReady() && !disposed && readyAttempts < 3) {
    readyRetryTimer = window.setTimeout(() => { void announceReady() }, 1500)
  }
}

onMounted(async () => {
  removePresentationListener = await listen<MascotSystemNotificationDelivery>(
    MASCOT_SYSTEM_NOTIFICATION_PRESENT_EVENT,
    (event) => {
      void applyPresentation(event.payload).catch(() => {})
    },
  )
  removePlacementListener = await listen<string>('mascot-system-notification-placement', (event) => {
    placement.value = event.payload
  })
  await announceReady()
})

onUnmounted(() => {
  disposed = true
  window.clearTimeout(readyRetryTimer)
  removePresentationListener?.()
  removePlacementListener?.()
})
</script>

<template>
  <section ref="notificationWindow" class="mascot-notification-window" :class="`is-${placement}`" aria-label="机器人提醒窗口">
    <!-- Replace the card in one Vue flush. An out-in leave transition can
         postpone its replacement indefinitely inside a hidden WebView. -->
    <AuthLoginTip
      v-if="presentation?.kind === 'auth'"
      :key="presentation.generation"
      :pending="presentation.pending"
      :message="presentation.message"
      @login="handleLogin"
    />
    <SysMessageTip
      v-else-if="presentation?.kind === 'message'"
      :key="presentation.generation"
      :message="presentation.message"
      :display-content="presentation.displayContent"
      :pending-count="presentation.pendingCount"
      :read-pending="presentation.readPending"
      :read-all-pending="presentation.readAllPending"
      :action-error="presentation.actionError"
      @read="handleRead"
      @read-all="handleReadAll"
      @view="handleView"
    />
  </section>
</template>
