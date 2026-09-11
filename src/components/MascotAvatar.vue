<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import spriteSheetUrl from '../assets/mascot/xiaoli-spritesheet.webp'
import runningSpriteSheetUrl from '../assets/mascot/xiaoli-running-spritesheet.webp'
import peekSpriteSheetUrl from '../assets/mascot/xiaoli-peek-spritesheet.webp'
import type { MascotAnimationState, MascotStatus } from '../types/mascot'
import { mascotAnimationTiming } from '../utils/mascot-animation-timing'
import type { RunningDirection } from '../utils/mascot-drag-motion'

const props = defineProps<{
  status: MascotStatus
  animationState?: MascotAnimationState
  waveKey?: string
}>()

const emit = defineEmits<{ waveCycle: []; activate: [] }>()

function handleKeydown(event: KeyboardEvent) {
  if ((event.key !== 'Enter' && event.key !== ' ') || event.repeat || event.isComposing) return
  event.preventDefault()
  emit('activate')
}

type SpriteSheetName = 'main' | 'running' | 'peek'

type SpriteState = {
  sheet: SpriteSheetName
  row: number
  frames: number
  duration: number
  animation: 'mascot-sprite-idle' | 'mascot-sprite-active'
  iterations?: 'infinite' | '1'
  direction?: 'normal' | 'reverse'
}

// These logical dimensions land on whole physical pixels at every supported
// Windows scale: 125%, 150%, 175% and 200%.
const SPRITE_DISPLAY_WIDTH = 92
const BASE_SPRITE_DISPLAY_HEIGHT = 76
const RUN_SPRITE_DISPLAY_HEIGHT = 84
const spriteSheets = {
  main: {
    url: spriteSheetUrl,
    columns: 12,
    rows: 10,
    displayHeight: BASE_SPRITE_DISPLAY_HEIGHT
  },
  running: {
    url: runningSpriteSheetUrl,
    columns: 24,
    rows: 2,
    displayHeight: RUN_SPRITE_DISPLAY_HEIGHT
  },
  peek: {
    url: peekSpriteSheetUrl,
    columns: 12,
    rows: 2,
    displayHeight: BASE_SPRITE_DISPLAY_HEIGHT
  }
} as const

const isReturningFromRun = ref(false)
const lastRunningDirection = ref<RunningDirection>('running-right')
let runReturnTimer: number | undefined

const spriteStates: Record<string, SpriteState> = {
  idle: {
    sheet: 'main', row: 0,
    frames: mascotAnimationTiming.idle.frames,
    duration: mascotAnimationTiming.idle.durationMs,
    animation: 'mascot-sprite-idle'
  },
  hover: {
    sheet: 'main', row: 1, frames: 6, duration: 500,
    animation: 'mascot-sprite-active'
  },
  thinking: {
    sheet: 'main', row: 2,
    frames: mascotAnimationTiming.thinking.frames,
    duration: mascotAnimationTiming.thinking.durationMs,
    animation: 'mascot-sprite-active'
  },
  waiting: {
    sheet: 'main', row: 3,
    frames: mascotAnimationTiming.waiting.frames,
    duration: mascotAnimationTiming.waiting.durationMs,
    animation: 'mascot-sprite-active'
  },
  remind: {
    sheet: 'main', row: 4,
    frames: mascotAnimationTiming.remind.frames,
    duration: mascotAnimationTiming.remind.durationMs,
    animation: 'mascot-sprite-active'
  },
  waving: {
    sheet: 'main', row: 4,
    frames: mascotAnimationTiming.waving.frames,
    duration: mascotAnimationTiming.waving.durationMs,
    animation: 'mascot-sprite-active'
  },
  success: {
    sheet: 'main', row: 5,
    frames: mascotAnimationTiming.success.frames,
    duration: mascotAnimationTiming.success.durationMs,
    animation: 'mascot-sprite-active'
  },
  error: {
    sheet: 'main', row: 6,
    frames: mascotAnimationTiming.error.frames,
    duration: mascotAnimationTiming.error.durationMs,
    animation: 'mascot-sprite-active'
  },
  peeking: {
    sheet: 'peek', row: 0,
    frames: mascotAnimationTiming.peeking.frames,
    duration: mascotAnimationTiming.peeking.durationMs,
    animation: 'mascot-sprite-active', iterations: '1'
  },
  'peeking-left': {
    sheet: 'peek', row: 1,
    frames: mascotAnimationTiming.peeking.frames,
    duration: mascotAnimationTiming.peeking.durationMs,
    animation: 'mascot-sprite-active', iterations: '1'
  },
  revealing: {
    sheet: 'peek', row: 0,
    frames: mascotAnimationTiming.revealing.frames,
    duration: mascotAnimationTiming.revealing.durationMs,
    animation: 'mascot-sprite-active', iterations: '1', direction: 'reverse'
  },
  'revealing-left': {
    sheet: 'peek', row: 1,
    frames: mascotAnimationTiming.revealing.frames,
    duration: mascotAnimationTiming.revealing.durationMs,
    animation: 'mascot-sprite-active', iterations: '1', direction: 'reverse'
  },
  'running-right': {
    sheet: 'running', row: 0,
    frames: mascotAnimationTiming.running.frames,
    duration: mascotAnimationTiming.running.durationMs,
    animation: 'mascot-sprite-active'
  },
  'running-left': {
    sheet: 'running', row: 1,
    frames: mascotAnimationTiming.running.frames,
    duration: mascotAnimationTiming.running.durationMs,
    animation: 'mascot-sprite-active'
  },
  'cooling-office': {
    sheet: 'main', row: 9,
    frames: mascotAnimationTiming.coolingOffice.frames,
    duration: mascotAnimationTiming.coolingOffice.durationMs,
    animation: 'mascot-sprite-active'
  }
}

const resolvedState = computed<string>(() => {
  if (props.animationState === 'jumping') return 'success'
  if (props.animationState === 'failed') return 'error'
  if (props.animationState) return props.animationState
  // Keep the idle atlas on hover. A row swap adds visual noise without helping
  // the pointer interaction, and the outer button already changes its cursor.
  if (props.status === 'hover') return 'idle'
  return props.status
})

const runningDirection = computed<RunningDirection | undefined>(() => {
  if (resolvedState.value === 'running-left' || resolvedState.value === 'running-right') {
    return resolvedState.value
  }
  return undefined
})

watch(runningDirection, (direction, previousDirection) => {
  window.clearTimeout(runReturnTimer)
  runReturnTimer = undefined

  if (direction) {
    lastRunningDirection.value = direction
    isReturningFromRun.value = false
    return
  }

  if (!previousDirection) return
  isReturningFromRun.value = true
  runReturnTimer = window.setTimeout(() => {
    isReturningFromRun.value = false
    runReturnTimer = undefined
  }, 280)
}, { immediate: true })

// Recreate a state sprite atomically so its timeline starts at frame zero. Both
// running directions deliberately share one key, preserving gait phase when a
// drag reverses. There is never an old and new sprite in the DOM together.
const spriteElementKey = computed(() => {
  if (runningDirection.value) return 'running'
  if (resolvedState.value === 'waving') return `waving:${props.waveKey ?? ''}`
  return resolvedState.value
})

const spriteStyle = computed(() => {
  const sprite = spriteStates[resolvedState.value] ?? spriteStates.idle
  const sheet = spriteSheets[sprite.sheet]
  const isIdle = sprite.animation === 'mascot-sprite-idle'
  const lastFrameOffset = Math.max(0, sprite.frames - 1) * SPRITE_DISPLAY_WIDTH
  const timingFunction = isIdle
    ? `steps(${Math.max(1, sprite.frames - 1)}, end)`
    : `steps(${sprite.frames}, jump-none)`

  return {
    backgroundImage: `url(${sheet.url})`,
    '--sprite-display-width': `${SPRITE_DISPLAY_WIDTH}px`,
    '--sprite-display-height': `${sheet.displayHeight}px`,
    '--sprite-sheet-width': `${sheet.columns * SPRITE_DISPLAY_WIDTH}px`,
    '--sprite-sheet-height': `${sheet.rows * sheet.displayHeight}px`,
    '--sprite-row-offset': `-${sprite.row * sheet.displayHeight}px`,
    '--sprite-end-offset': `-${lastFrameOffset}px`,
    '--sprite-animation': [
      sprite.animation,
      `${sprite.duration}ms`,
      timingFunction,
      sprite.iterations ?? 'infinite',
      sprite.direction ?? 'normal',
      sprite.iterations === '1' ? 'both' : 'none'
    ].join(' ')
  }
})

onBeforeUnmount(() => {
  window.clearTimeout(runReturnTimer)
})
</script>

<template>
  <button
    class="mascot-avatar"
    :class="[
      `is-${status}`,
      `is-visual-${resolvedState}`,
      {
        'is-returning-from-run': isReturningFromRun,
        'is-returning-right': isReturningFromRun && lastRunningDirection === 'running-right',
        'is-returning-left': isReturningFromRun && lastRunningDirection === 'running-left'
      }
    ]"
    type="button"
    aria-label="单击打开输入框，双击打开工作台"
    @keydown="handleKeydown"
  >
    <span class="status-orbit" />
    <span class="mascot-sprite-stage">
      <span
        :key="spriteElementKey"
        class="mascot-sprite mascot-sprite--single"
        :style="spriteStyle"
        @animationiteration="resolvedState === 'waving' && emit('waveCycle')"
        aria-hidden="true"
      />
    </span>
  </button>
</template>
