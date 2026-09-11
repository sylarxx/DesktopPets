<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { storage } from '../utils/storage'
import {
  clampTodoTextareaHeight,
  getTodoPanelHeight,
  TODO_TEXTAREA_MIN_HEIGHT
} from '../utils/todo-input-layout'

const props = defineProps<{
  loading: boolean
  error?: string
  prepareHeight?: (height: number) => Promise<void>
}>()

const emit = defineEmits<{
  submit: [text: string]
  draftChange: [text: string]
  focusChange: [focused: boolean]
  heightChange: [height: number]
  dismiss: []
  compositionChange: [composing: boolean]
}>()

const text = ref(storage.getTodoInputDraft())
const inputRef = ref<HTMLTextAreaElement | null>(null)
const textareaHeight = ref(TODO_TEXTAREA_MIN_HEIGHT)
const canSubmit = computed(() => text.value.trim().length > 0 && !props.loading)
const isMultiline = computed(() => textareaHeight.value > TODO_TEXTAREA_MIN_HEIGHT)
let lastPanelHeight = 0
let layoutGeneration = 0
let composing = false

function submit() {
  const value = text.value.trim()
  if (!value || props.loading) return
  emit('submit', value)
}

function focus() {
  inputRef.value?.focus()
}

async function syncHeight() {
  const generation = ++layoutGeneration
  await nextTick()
  const input = inputRef.value
  if (!input) return
  const previous = input.style.height
  input.style.height = '0px'
  const height = clampTodoTextareaHeight(input.scrollHeight)
  input.style.height = previous
  const panelHeight = getTodoPanelHeight(height, Boolean(props.error))
  // Grow the native viewport before applying the larger textarea. On shrink,
  // commit the smaller layout first, then reduce the outer window.
  if (panelHeight > lastPanelHeight) await props.prepareHeight?.(panelHeight)
  if (generation !== layoutGeneration) return
  textareaHeight.value = height
  input.style.height = `${height}px`
  input.style.overflowY = input.scrollHeight > height ? 'auto' : 'hidden'
  if (panelHeight < lastPanelHeight) await props.prepareHeight?.(panelHeight)
  if (generation !== layoutGeneration) return
  lastPanelHeight = panelHeight
  emit('heightChange', panelHeight)
}

function handleComposition(value: boolean) {
  composing = value
  emit('compositionChange', value)
}

function handleInput() {
  storage.setTodoInputDraft(text.value)
  emit('draftChange', text.value)
  syncHeight()
}

function handleFocus() {
  emit('focusChange', true)
}

function handleBlur() {
  emit('focusChange', false)
}

function handleKeydown(event: KeyboardEvent) {
  if (event.isComposing || composing || event.keyCode === 229) return
  if (event.key === 'Escape') {
    event.preventDefault()
    emit('dismiss')
    return
  }
  if (event.key !== 'Enter' || event.shiftKey) return
  event.preventDefault()
  submit()
}

function clear() {
  text.value = ''
  storage.setTodoInputDraft('')
  emit('draftChange', '')
  syncHeight()
}

function getDraft() {
  return text.value
}

onMounted(() => {
  emit('draftChange', text.value)
  syncHeight()
})

watch(() => props.error, syncHeight)
onUnmounted(() => { layoutGeneration += 1 })

defineExpose({ clear, focus, getDraft, syncHeight })
</script>

<template>
  <form
    class="todo-input"
    :class="{ 'is-multiline': isMultiline, 'has-error': error }"
    :aria-busy="loading"
    @submit.prevent="submit"
  >
    <label class="sr-only" for="desktop-todo-input">输入要创建的待办、提醒或会议安排</label>
    <textarea
      id="desktop-todo-input"
      ref="inputRef"
      v-model="text"
      rows="1"
      placeholder="一句话创建待办、提醒或会议安排"
      :disabled="loading"
      :aria-describedby="error ? 'desktop-todo-input-error' : undefined"
      @input="handleInput"
      @compositionstart="handleComposition(true)"
      @compositionend="handleComposition(false)"
      @focus="handleFocus"
      @blur="handleBlur"
      @keydown="handleKeydown"
    />
    <button class="todo-input__send" type="submit" :disabled="!canSubmit">
      <span class="sr-only">{{ loading ? '提交中' : '发送到工作台' }}</span>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M21 3 10 14" />
        <path d="m21 3-7 18-4-7-7-4 18-7Z" />
      </svg>
    </button>
    <p
      v-if="error"
      id="desktop-todo-input-error"
      class="todo-input__error"
      role="alert"
      tabindex="0"
    >
      {{ error }}
    </p>
  </form>
</template>
