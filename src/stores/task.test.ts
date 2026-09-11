import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import type { TaskCreatedEvent } from '../types/task'

const mocks = vi.hoisted(() => ({
  requestTaskAction: vi.fn(),
  openCalendar: vi.fn(),
}))

vi.mock('../services/task.service', () => ({
  handleTaskAction: mocks.requestTaskAction,
}))

vi.mock('../services/window.service', () => ({
  openCalendar: mocks.openCalendar,
}))

import { useTaskStore } from './task'

function task(index: number): TaskCreatedEvent {
  return {
    eventId: `event-${index}`,
    eventType: 'task.created',
    timestamp: '2026-08-12 15:08:00',
    payload: {
      taskId: `task-${index}`,
      title: `任务 ${index}`,
      content: `任务内容 ${index}`,
    },
  }
}

describe('task store queue', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
    mocks.openCalendar.mockResolvedValue(true)
    mocks.requestTaskAction.mockResolvedValue({
      success: true,
      taskStatus: 'completed',
      message: '已完成',
    })
  })

  it('switches to the next queued task only after a successful completion', async () => {
    const store = useTaskStore()
    store.pushTask(task(1))
    store.pushTask(task(2))

    await expect(store.handleAction('event-2', 'task-2', 'confirm')).resolves.toBe(true)
    expect(store.currentTask?.payload.taskId).toBe('task-1')
    expect(store.taskQueue).toHaveLength(1)
  })

  it('retains the current task and exposes a retryable error on failure', async () => {
    mocks.requestTaskAction.mockRejectedValue(new Error('服务暂时不可用'))
    const store = useTaskStore()
    store.pushTask(task(1))

    await expect(store.handleAction('event-1', 'task-1', 'confirm')).resolves.toBe(false)
    expect(store.currentTask?.payload.taskId).toBe('task-1')
    expect(store.currentTask?.error).toBe('服务暂时不可用')
    expect(store.currentTask?.handling).toBe(false)
  })

  it('keeps the task when opening details fails and allows a retry', async () => {
    mocks.openCalendar.mockResolvedValue(false)
    const store = useTaskStore()
    store.pushTask(task(1))

    await expect(store.handleAction('event-1', 'task-1', 'openDetail')).resolves.toBe(false)
    expect(store.currentTask?.payload.taskId).toBe('task-1')
    expect(store.currentTask?.error).toContain('重试')
    expect(store.currentTask?.handling).toBe(false)
  })

  it('does not remove a task merely because its detail opened', async () => {
    const store = useTaskStore()
    store.pushTask(task(1))

    await expect(store.handleAction('event-1', 'task-1', 'openDetail')).resolves.toBe(true)
    expect(store.currentTask?.payload.taskId).toBe('task-1')
    expect(store.taskQueue).toHaveLength(1)
  })

  it('clears every queued task when the desktop session changes', () => {
    const store = useTaskStore()
    store.pushTask(task(1))
    store.pushTask(task(2))

    store.clearTasks()

    expect(store.currentTask).toBeNull()
    expect(store.taskQueue).toHaveLength(0)
    expect(store.latestMessage).toBe('')
  })

  it.each(['success', 'failure'])('ignores a late %s after a new session replaces the same task ID', async (outcome) => {
    let finish!: (result: { success: boolean; message: string }) => void
    mocks.requestTaskAction.mockReturnValue(new Promise(resolve => { finish = resolve }))
    const store = useTaskStore()
    store.pushTask(task(1))
    const pending = store.handleAction('event-1', 'task-1', 'confirm')
    store.clearTasks()
    store.pushTask(task(1))
    const nextTask = store.currentTask
    finish({ success: outcome === 'success', message: '旧账号操作结果' })

    await expect(pending).resolves.toBe(false)
    expect(store.currentTask).toBe(nextTask)
    expect(store.taskQueue).toHaveLength(1)
    expect(store.latestMessage).toBe('收到一个新任务')
    expect(store.currentTask?.error).toBeUndefined()
  })

  it('ignores detail-opening feedback after session clearing', async () => {
    let finish!: (opened: boolean) => void
    mocks.openCalendar.mockReturnValue(new Promise(resolve => { finish = resolve }))
    const store = useTaskStore()
    store.pushTask(task(1))
    const pending = store.handleAction('event-1', 'task-1', 'openDetail')
    store.clearTasks()
    finish(true)
    await expect(pending).resolves.toBe(false)
    expect(store.latestMessage).toBe('')
  })
})
