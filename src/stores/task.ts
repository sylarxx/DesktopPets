import { defineStore } from 'pinia'
import { handleTaskAction as requestTaskAction } from '../services/task.service'
import { openCalendar } from '../services/window.service'
import type { TaskAction, TaskCreatedEvent } from '../types/task'

export interface TaskItem extends TaskCreatedEvent {
  handling?: boolean
  error?: string
}

interface TaskState {
  taskQueue: TaskItem[]
  currentTask: TaskItem | null
  latestMessage: string
  receivedEventIds: string[]
}

export const useTaskStore = defineStore('task', {
  state: (): TaskState => ({
    taskQueue: [],
    currentTask: null,
    latestMessage: '',
    receivedEventIds: []
  }),
  actions: {
    pushTask(event: TaskCreatedEvent) {
      if (this.receivedEventIds.includes(event.eventId)) return
      this.receivedEventIds.push(event.eventId)
      if (this.receivedEventIds.length > 256) this.receivedEventIds.shift()
      const nextTask = { ...event }
      this.taskQueue.unshift(nextTask)
      this.currentTask = nextTask
      this.latestMessage = '收到一个新任务'
    },
    removeTask(taskId: string) {
      this.taskQueue = this.taskQueue.filter((item) => item.payload.taskId !== taskId)
      this.currentTask = this.taskQueue[0] ?? null
    },
    clearTasks() {
      this.taskQueue = []
      this.currentTask = null
      this.latestMessage = ''
      this.receivedEventIds = []
    },
    async handleAction(eventId: string, taskId: string, action: TaskAction) {
      const task = this.taskQueue.find((item) => item.eventId === eventId && item.payload.taskId === taskId)
      if (!task || task.handling) return false
      const isCurrent = () => this.taskQueue.includes(task)

      task.handling = true
      task.error = ''
      try {
        if (action === 'openDetail') {
          const opened = await openCalendar(taskId)
          if (!isCurrent()) return false
          if (!opened) throw new Error('未能打开任务详情，请检查浏览器后重试')
          this.latestMessage = '已打开任务详情'
          return true
        }

        const response = await requestTaskAction({ eventId, taskId, action })
        if (!isCurrent()) return false
        if (!response.success) {
          throw new Error(response.message || '操作失败，请重试')
        }
        this.latestMessage = response.message || '操作成功'
        this.removeTask(taskId)
        return true
      } catch (error) {
        if (!isCurrent()) return false
        task.error = error instanceof Error ? error.message : '操作失败，请重试'
        this.latestMessage = task.error
        return false
      } finally {
        task.handling = false
      }
    }
  }
})
