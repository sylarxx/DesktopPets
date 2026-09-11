<script setup lang="ts">
import { computed } from 'vue'
import type { SysMessageNotification } from '../types/sys-message'
import {
  classifySysMessage,
  formatSysMessageDisplayTime,
  normalizeSysMessageDateTime
} from '../utils/sys-message-display'

const props = defineProps<{
  message: SysMessageNotification
  displayContent: string
  pendingCount?: number
  readPending?: boolean
  readAllPending?: boolean
  actionError?: string
}>()

const emit = defineEmits<{
  view: [message: SysMessageNotification]
  read: [message: SysMessageNotification]
  readAll: []
}>()

const title = computed(() => props.message.msgSubject || '站内消息')
const tipTone = computed(() => classifySysMessage(props.message.bizType, title.value))
const isMeeting = computed(() => tipTone.value === 'meeting')
const isTask = computed(() => tipTone.value === 'todo')
const isCompletedTodo = computed(() => isTask.value && /待办已完成|已完成|处理完成/.test(title.value))
const isNewTodo = computed(() => /新待办|新的待办|派发/.test(title.value))
const titleId = computed(() => `sys-message-title-${props.message.id.replace(/[^a-zA-Z0-9_-]/g, '-')}`)
const summaryId = computed(() => `sys-message-summary-${props.message.id.replace(/[^a-zA-Z0-9_-]/g, '-')}`)

const badgeLabel = computed(() => {
  if (isCompletedTodo.value) return '待办已完成'
  if (isMeeting.value) return '会议提醒'
  if (isTask.value && isNewTodo.value) return '新待办'
  if (isTask.value) return '任务提醒'
  return '消息提醒'
})

const displayTime = computed(() => formatSysMessageDisplayTime(props.message.createTime))
const dateTimeValue = computed(() => normalizeSysMessageDateTime(props.message.createTime))
const pendingLabel = computed(() => {
  const count = Math.max(0, props.pendingCount ?? 0)
  if (!count) return ''
  return `另有 ${count > 99 ? '99+' : count} 条`
})
const announcement = computed(() => {
  const content = props.displayContent.trim().replace(/[。！？!?]+$/, '')
  return `${badgeLabel.value}：${title.value}。${content}。${displayTime.value}`
})
</script>

<template>
  <article
    class="sys-message-tip"
    :class="[`sys-message-tip--${tipTone}`, { 'has-action-error': actionError }]"
    role="dialog"
    aria-modal="false"
    :aria-busy="readPending"
    :aria-labelledby="titleId"
    :aria-describedby="summaryId"
    @pointerdown.stop
    @pointermove.stop
    @pointerup.stop
    @click.stop
  >
    <span class="sr-only" role="status" aria-live="polite" aria-atomic="true">
      {{ announcement }}
    </span>
    <div class="sys-message-tip__card">
    <div class="sys-message-tip__reading" tabindex="0">
      <header class="sys-message-tip__header">
        <span class="sys-message-tip__badge">
          <svg class="sys-message-tip__badge-icon" viewBox="0 0 24 24" aria-hidden="true">
            <template v-if="isMeeting"><rect x="3" y="5" width="18" height="16" rx="3"/><path d="M8 2v5M16 2v5M3 10h18M7 14h2M13 14h2M7 17h2"/></template>
            <template v-else-if="isTask"><rect x="5" y="4" width="15" height="18" rx="2"/><path d="M9 2h7v4H9zM8 12l2 2 4-4M9 18h7"/></template>
            <template v-else><path d="M5 17h14l-2-3V9a5 5 0 0 0-10 0v5zM10 21h4"/></template>
          </svg>
          <span class="sys-message-tip__badge-label">{{ badgeLabel }}</span>
        </span>
        <span class="sys-message-tip__meta">
          <span v-if="pendingLabel" class="sys-message-tip__queue-count">{{ pendingLabel }}</span>
          <time class="sys-message-tip__time" :datetime="dateTimeValue">{{ displayTime }}</time>
        </span>
      </header>

      <div class="sys-message-tip__body" tabindex="0">
        <h2 :id="titleId">{{ title }}</h2>
        <div class="sys-message-tip__summary-shell">
          <p
            :id="summaryId"
            class="sys-message-tip__summary"
            tabindex="0"
          >
            {{ displayContent }}
          </p>
        </div>
      </div>

      <p v-if="actionError" class="sys-message-tip__error" role="alert">
        {{ actionError }}
      </p>

    </div>

      <div
        class="sys-message-tip__actions"
        :class="{ 'has-read-all': (pendingCount ?? 0) > 0 }"
      >
        <button
          class="sys-message-tip__button"
          type="button"
          :disabled="readPending"
          @click.stop="emit('read', message)"
        >
          {{ readPending ? '处理中…' : '知道了' }}
        </button>
        <button
          v-if="(pendingCount ?? 0) > 0"
          class="sys-message-tip__button sys-message-tip__button--read-all"
          type="button"
          :disabled="readPending"
          :aria-label="`将当前及其余 ${pendingCount ?? 0} 条提醒全部标为已读，共 ${(pendingCount ?? 0) + 1} 条`"
          :title="`全部标为已读（共 ${(pendingCount ?? 0) + 1} 条）`"
          @click.stop="emit('readAll')"
        >
          {{ readAllPending ? '处理中…' : '全部已读' }}
        </button>
        <button
          class="sys-message-tip__button sys-message-tip__button--primary"
          type="button"
          :disabled="readPending"
          @click.stop="emit('view', message)"
        >
          查看详情
        </button>
      </div>
    </div>

    <span class="sys-message-tip__tail" aria-hidden="true" />
  </article>
</template>
