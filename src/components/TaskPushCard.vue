<script setup lang="ts">
import { computed, ref } from 'vue'
import type { TaskAction, TaskCreatedEvent } from '../types/task'
import { getVisibleTaskActions } from '../utils/task-actions'
import { formatDateTime } from '../utils/time'

const props = withDefaults(defineProps<{
  task: TaskCreatedEvent & { handling?: boolean; error?: string }
  pendingCount?: number
}>(), {
  pendingCount: 0,
})

const emit = defineEmits<{
  action: [eventId: string, taskId: string, action: TaskAction]
}>()

const cardRef = ref<HTMLElement | null>(null)
const taskTitle = computed(() => props.task.payload.title?.trim() || '未命名任务')
const taskContent = computed(() => props.task.payload.content?.trim() || '')
const deadlineLabel = computed(() => props.task.payload.deadline
  ? `截止 ${formatDateTime(props.task.payload.deadline)}`
  : '未设置截止时间')
const visibleActions = computed(() => getVisibleTaskActions(props.task.payload.actions))
const remainingLabel = computed(() => {
  const count = Math.max(0, props.pendingCount)
  return count ? `另有 ${count > 99 ? '99+' : count} 条` : ''
})
const titleId = computed(
  () => `task-card-title-${props.task.eventId.replace(/[^a-zA-Z0-9_-]/g, '-')}`
)

function focusCard() {
  cardRef.value?.focus()
}

function getPreferredHeight() {
  const card = cardRef.value
  if (!card) return 240
  const style = getComputedStyle(card)
  let height = 16 + parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) + 2
  for (const child of Array.from(card.querySelectorAll(':scope > .task-card__reading > *, :scope > .task-card__actions'))) {
    const box = child as HTMLElement
    const css = getComputedStyle(box)
    height += (box.classList.contains('task-card__body') ? box.scrollHeight : box.offsetHeight)
      + parseFloat(css.marginTop || '0') + parseFloat(css.marginBottom || '0')
  }
  return Math.min(380, Math.max(180, Math.ceil(height)))
}

defineExpose({ focusCard, getPreferredHeight })
</script>

<template>
  <article
    ref="cardRef"
    class="task-card"
    role="dialog"
    tabindex="-1"
    aria-modal="false"
    :aria-busy="task.handling"
    :aria-labelledby="titleId"
  >
    <div class="task-card__reading" tabindex="0">
    <header class="task-card__header">
      <span class="task-card__kind">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <rect x="3" y="5" width="18" height="15" rx="3" />
          <path d="M8 2v4M16 2v4M3 10h18" />
        </svg>
        <span>任务提醒</span>
      </span>
      <span class="task-card__meta">
        <span v-if="remainingLabel" class="task-card__queue-count">{{ remainingLabel }}</span>
        <span class="task-card__deadline">{{ deadlineLabel }}</span>
      </span>
    </header>

    <div class="task-card__body" tabindex="0">
      <h2 :id="titleId">{{ taskTitle }}</h2>
      <p v-if="taskContent" class="task-card__description" tabindex="0">{{ taskContent }}</p>
      <p v-else class="task-card__description task-card__description--empty">暂无补充说明</p>
    </div>

    <p v-if="task.error" class="task-card__error" role="alert">{{ task.error }}</p>

    </div>

    <div class="task-card__actions" :class="{ 'is-single': visibleActions.length === 1 }">
      <button
        v-for="action in visibleActions"
        :key="action.key"
        class="task-card__action"
        :class="`task-card__action--${action.key}`"
        type="button"
        :disabled="task.handling"
        @click="emit('action', task.eventId, task.payload.taskId, action.key)"
      >
        <svg v-if="action.key === 'confirm'" viewBox="0 0 24 24" aria-hidden="true">
          <path d="m7 12 3 3 7-7" />
          <circle cx="12" cy="12" r="9" />
        </svg>
        <svg v-else viewBox="0 0 24 24" aria-hidden="true">
          <path d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z" />
          <circle cx="12" cy="12" r="3" />
        </svg>
        <span>{{ task.handling ? '处理中…' : action.label }}</span>
      </button>
    </div>
  </article>
</template>
