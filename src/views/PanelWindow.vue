<script setup lang="ts">
import { emitTo, listen } from '@tauri-apps/api/event'
import type { UnlistenFn } from '@tauri-apps/api/event'
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import TaskPushCard from '../components/TaskPushCard.vue'
import TodoInputBox from '../components/TodoInputBox.vue'
import {
  PANEL_ACTIVITY_EVENT,
  PANEL_REVEAL_EVENT,
  hidePanelWindow,
  openWorkbench,
  setPanelActivity,
  setPanelHeight,
  type PanelRevealPayload,
} from '../services/window.service'
import { useMascotStore } from '../stores/mascot'
import { useTaskStore, type TaskItem } from '../stores/task'
import type { MascotStatus } from '../types/mascot'
import type { TaskAction } from '../types/task'

const props = defineProps<{
  socketStatus: string
  mockEnabled: boolean
  task: TaskItem | null
}>()

const mascotStore = useMascotStore()
const taskStore = useTaskStore()
const inputBoxRef = ref<InstanceType<typeof TodoInputBox> | null>(null)
const taskCardRef = ref<InstanceType<typeof TaskPushCard> | null>(null)
const loading = ref(false)
const submitError = ref('')
const isRevealing = ref(false)
const panelHasText = ref(false)
const panelFocused = ref(false)
const panelComposing = ref(false)
let operationGeneration = 0
let panelWidth = window.innerWidth
function handlePanelResize() {
  if (window.innerWidth === panelWidth) return
  panelWidth = window.innerWidth
  void syncVisiblePanelHeight()
}
const pendingTaskCount = computed(() => Math.max(0, taskStore.taskQueue.length - 1))
let revealTimer: number | undefined
let focusTimer: number | undefined
let revealFrame: number | undefined
let removeRevealListener: UnlistenFn | undefined
let removeSessionClearedListener: UnlistenFn | undefined
let panelActivityInitialized = false

function focusVisibleControl() {
  if (props.task) taskCardRef.value?.focusCard()
  else inputBoxRef.value?.focus()
}

async function syncVisiblePanelHeight() {
  await nextTick()
  if (props.task) await setPanelHeight(taskCardRef.value?.getPreferredHeight() ?? 240)
  else await inputBoxRef.value?.syncHeight()
}

defineExpose({ syncVisiblePanelHeight })

function playPanelReveal(options: PanelRevealPayload = { focus: false }) {
  window.clearTimeout(revealTimer)
  window.clearTimeout(focusTimer)
  window.cancelAnimationFrame(revealFrame ?? 0)
  isRevealing.value = false
  revealFrame = window.requestAnimationFrame(() => {
    isRevealing.value = true
    revealTimer = window.setTimeout(() => {
      isRevealing.value = false
    }, 220)
  })
  syncVisiblePanelHeight()
  publishPanelActivity()
  // Only a deliberate mascot click may move keyboard focus into the panel.
  // Server pushes and post-notification restores animate without activation or
  // preselecting the destructive completion action.
  if (options.focus) {
    focusTimer = window.setTimeout(focusVisibleControl, 80)
  }
}

onMounted(async () => {
  window.addEventListener('resize', handlePanelResize)
  syncVisiblePanelHeight()
  publishPanelActivity()
  removeRevealListener = await listen<PanelRevealPayload>(PANEL_REVEAL_EVENT, (event) => {
    playPanelReveal(event.payload)
  })
  removeSessionClearedListener = await listen('desktop-session-cleared', async () => {
    operationGeneration += 1
    submitError.value = ''
    loading.value = false
    await nextTick()
    inputBoxRef.value?.clear()
  })
})

onUnmounted(() => {
  operationGeneration += 1
  window.removeEventListener('resize', handlePanelResize)
  window.clearTimeout(revealTimer)
  window.clearTimeout(focusTimer)
  window.cancelAnimationFrame(revealFrame ?? 0)
  removeRevealListener?.()
  removeSessionClearedListener?.()
})

watch(() => [props.task?.eventId, props.task?.error, props.task?.payload.title, props.task?.payload.content], async () => {
  publishPanelActivity()
  await nextTick()
  syncVisiblePanelHeight()
})

function showMascotMessage(message: string, status?: MascotStatus, autoReset = false) {
  mascotStore.showMessage(message, status, autoReset)
  void emitTo('mascot', 'mascot-message', { message, status, autoReset })
}

function publishPanelActivity() {
  panelActivityInitialized = true
  const activity = {
    // A visible task is active panel content and must not be treated like an
    // empty draft by the native idle-hide policy.
    hasText: panelHasText.value || panelComposing.value || Boolean(props.task),
    focused: panelFocused.value,
  }
  void setPanelActivity(activity)
  void emitTo('mascot', PANEL_ACTIVITY_EVENT, activity)
}

function handleDraftChange(text: string) {
  const hasText = text.trim().length > 0
  if (panelActivityInitialized && panelHasText.value === hasText) return
  panelHasText.value = hasText
  publishPanelActivity()
}

function handleFocusChange(focused: boolean) {
  if (panelActivityInitialized && panelFocused.value === focused) return
  panelFocused.value = focused
  publishPanelActivity()
}

async function prepareInputHeight(height: number) {
  if (!props.task) await setPanelHeight(height)
}

function handleCompositionChange(composing: boolean) {
  panelComposing.value = composing
  publishPanelActivity()
}

async function submitTodo(text: string) {
  if (loading.value) return

  const operation = ++operationGeneration
  submitError.value = ''
  loading.value = true
  try {
    const opened = await openWorkbench({ todoText: text })
    if (operation !== operationGeneration) return
    if (!opened) {
      submitError.value = '未确认工作台是否打开，请先检查浏览器。当前内容已保留。'
      return
    }

    inputBoxRef.value?.clear()
    const hidden = await hidePanelWindow()
    if (hidden) showMascotMessage('已打开工作台', 'success', true)
  } catch {
    if (operation !== operationGeneration) return
    submitError.value = '未确认工作台是否打开，请先检查浏览器。当前内容已保留。'
  } finally {
    if (operation !== operationGeneration) return
    loading.value = false
    if (submitError.value) {
      await nextTick()
      inputBoxRef.value?.syncHeight()

    }
  }
}

async function handleTaskAction(eventId: string, taskId: string, action: TaskAction) {
  if (props.task?.handling) return
  const operation = operationGeneration
  const succeeded = await taskStore.handleAction(eventId, taskId, action)
  if (operation !== operationGeneration) return
  await nextTick()
  syncVisiblePanelHeight()
  if (props.task?.eventId === eventId && (!succeeded || action === 'confirm')) focusVisibleControl()
}
</script>

<template>
  <section
    class="pet-prompt"
    :class="{ 'is-revealing': isRevealing, 'has-task': task }"
    :aria-label="task ? '任务提醒' : '一句话创建'"
  >
    <TaskPushCard
      v-if="task"
      ref="taskCardRef"
      :task="task"
      :pending-count="pendingTaskCount"
      @action="handleTaskAction"
    />
    <TodoInputBox
      v-else
      ref="inputBoxRef"
      :loading="loading"
      :error="submitError"
      :prepare-height="prepareInputHeight"
      @submit="submitTodo"
      @draft-change="handleDraftChange"
      @focus-change="handleFocusChange"
      @composition-change="handleCompositionChange"
      @dismiss="hidePanelWindow"
    />
    <span class="pet-prompt__tail" aria-hidden="true" />
  </section>
</template>
