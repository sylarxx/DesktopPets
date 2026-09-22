import type { SysMessageNotification, SysMessagePushPayload, SysMessageStatus } from '../types/sys-message'
import { env } from '../utils/env'
import { isSysMessageExpired, resolveSysMessageExpiresAt } from '../utils/sys-message-expiry'
import { request } from './request'
import { maskDiagnosticIdentifier, recordDesktopDiagnostic } from './diagnostic.service'

type MessageListener = (message: SysMessageNotification) => void

let socket: WebSocket | null = null
let reconnectTimer: number | undefined
let reconnectAttempts = 0
let shouldReconnect = true
let activeUserId = ''
let pollTimer: number | undefined
let polling = false
let pollInitialized = false
let pollGeneration = 0
const messageListeners = new Set<MessageListener>()
const knownMessageIds = new Set<string>()
const SYS_MESSAGE_POLL_INTERVAL = 10_000
const MAX_KNOWN_MESSAGE_IDS = 500

interface LocationLike {
  protocol: string
  host: string
}

interface SysMessageBackendItem {
  id?: string | number
  msgSubject?: string | null
  msgContent?: string | null
  msgStatus?: number | null
  msgType?: number | null
  bizType?: number | null
  bizId?: string | number | null
  createTime?: string | null
}

interface SysMessagePagePayload {
  rows?: SysMessageBackendItem[] | null
  list?: SysMessageBackendItem[] | null
}

function notifyMessage(message: SysMessageNotification, source: string) {
  recordDesktopDiagnostic('reminder.message_dispatched', {
    source,
    messageIdMasked: maskDiagnosticIdentifier(message.id),
    bizType: message.bizType ?? null,
    listenerCount: messageListeners.size,
  })
  messageListeners.forEach((listener) => listener(message))
}

function rememberMessage(message: SysMessageNotification) {
  if (knownMessageIds.has(message.id)) return false

  knownMessageIds.add(message.id)
  if (knownMessageIds.size > MAX_KNOWN_MESSAGE_IDS) {
    const oldestId = knownMessageIds.values().next().value
    if (oldestId) knownMessageIds.delete(oldestId)
  }
  return true
}

function deliverMessage(message: SysMessageNotification, source = 'websocket') {
  if (rememberMessage(message)) {
    notifyMessage(message, source)
  } else {
    recordDesktopDiagnostic('reminder.message_duplicate', {
      source,
      messageIdMasked: maskDiagnosticIdentifier(message.id),
    })
  }
}

function toId(value?: string | number | null) {
  return value === null || value === undefined ? '' : String(value).trim()
}

function toNumber(value: unknown, fallback = 0) {
  const numeric = Number(value)
  return Number.isFinite(numeric) ? numeric : fallback
}

function toStatus(value: unknown): SysMessageStatus {
  return Number(value) === 1 ? 1 : 0
}

function normalizeRequestId(id: string | number) {
  if (typeof id === 'number') return id
  const numeric = Number(id)
  return Number.isFinite(numeric) && String(numeric) === id.trim() ? numeric : id
}

function normalizeSysMessageItem(payload: SysMessageBackendItem): SysMessageNotification | null {
  const id = toId(payload.id)
  if (!id) return null

  const createTime = payload.createTime?.trim() || undefined
  const msgSubject = payload.msgSubject?.trim() || '站内消息'
  const msgContent = payload.msgContent?.trim() || ''

  return {
    id,
    rawId: payload.id ?? id,
    dedupeKey: [id, createTime || '', msgSubject, msgContent].join('|'),
    msgSubject,
    msgContent,
    msgStatus: toStatus(payload.msgStatus),
    msgType: toNumber(payload.msgType, 1),
    bizType:
      payload.bizType === null || payload.bizType === undefined
        ? undefined
        : toNumber(payload.bizType),
    bizId: toId(payload.bizId) || undefined,
    createTime
  }
}

function normalizeSysMessage(payload: SysMessagePushPayload): SysMessageNotification | null {
  if (payload.type !== 'sys_message') return null
  return normalizeSysMessageItem(payload)
}

function toWsProtocol(protocol: string) {
  return protocol === 'https:' ? 'wss:' : 'ws:'
}

function getDefaultLocation(): LocationLike | undefined {
  if (typeof window === 'undefined') return undefined

  return {
    protocol: window.location.protocol,
    host: window.location.host
  }
}

function normalizeWebSocketBaseUrl(baseUrl: string, location = getDefaultLocation()) {
  const trimmed = baseUrl.trim().replace(/\/+$/, '')

  if (!trimmed) {
    if (!location?.host) return ''
    return `${toWsProtocol(location.protocol)}//${location.host}/websocket`
  }

  if (trimmed.startsWith('ws://') || trimmed.startsWith('wss://')) {
    return trimmed.endsWith('/websocket') ? trimmed : `${trimmed}/websocket`
  }

  if (trimmed.startsWith('http://') || trimmed.startsWith('https://')) {
    const url = new URL(trimmed)
    url.protocol = toWsProtocol(url.protocol)
    return url.toString().replace(/\/+$/, '').endsWith('/websocket')
      ? url.toString().replace(/\/+$/, '')
      : `${url.toString().replace(/\/+$/, '')}/websocket`
  }

  const resolved = `ws://${trimmed}`
  return resolved.endsWith('/websocket') ? resolved : `${resolved}/websocket`
}

function buildSysMessageWebSocketUrl(userId: string) {
  const baseUrl = normalizeWebSocketBaseUrl(env.sysMessageWsBaseUrl)
  if (!baseUrl || !userId.trim()) return ''

  return `${baseUrl}/${encodeURIComponent(userId.trim())}`
}

async function pollUnreadMessages() {
  if (polling || !activeUserId.trim()) return

  const generation = pollGeneration
  const userId = activeUserId
  polling = true
  try {
    const initial = !pollInitialized
    const startedAt = Date.now()
    const messages: SysMessageNotification[] = []
    const batchIds = new Set<string>()
    // Initial catch-up is at most 5 pages / 100 rows / 10 seconds. Ordinary
    // reconnects keep the existing poll cursor and deduplication history.
    for (let pageNum = 1; pageNum <= (initial ? 5 : 1); pageNum += 1) {
      const remaining = 10_000 - (Date.now() - startedAt)
      if (remaining <= 0) break
      const payload = await request.get<unknown, SysMessagePagePayload>('/sys-message/page', {
        params: { pageNum, pageSize: 20, msgStatus: 0 },
        timeoutMs: remaining,
      })
      if (generation !== pollGeneration || userId !== activeUserId) return
      const page = (payload?.rows ?? payload?.list ?? [])
        .map(normalizeSysMessageItem)
        .filter((item): item is SysMessageNotification => Boolean(item))
      let added = 0
      for (const message of page) {
        if (batchIds.has(message.id)) continue
        batchIds.add(message.id)
        messages.push(message)
        added += 1
      }
      if (page.length < 20 || added === 0) break
    }
    if (generation !== pollGeneration || userId !== activeUserId) return
    pollInitialized = true
    const attentionKey = `catch-up:${generation}:${startedAt}`
    messages.sort((a, b) => {
      const left = new Date(a.createTime?.replace(' ', 'T') || '').getTime()
      const right = new Date(b.createTime?.replace(' ', 'T') || '').getTime()
      return (Number.isFinite(left) ? left : startedAt) - (Number.isFinite(right) ? right : startedAt)
    }).forEach((message) => {
      if (message.msgStatus !== 0 || isSysMessageExpired(resolveSysMessageExpiresAt(message.createTime))) {
        rememberMessage(message)
        return
      }
      deliverMessage(initial ? { ...message, attentionKey } : message, initial ? 'poll-initial' : 'poll')
    })

  } catch (error) {
    if (generation === pollGeneration) {
      console.warn('Sys message polling failed', error)
      recordDesktopDiagnostic('reminder.poll.failed', {
        userIdMasked: maskDiagnosticIdentifier(userId),
        errorName: error instanceof Error ? error.name : 'unknown',
        generation,
      })
    }
  } finally {
    if (generation === pollGeneration) {
      polling = false
    }
  }
}

function startPolling(reset: boolean) {
  window.clearInterval(pollTimer)
  pollGeneration += 1
  polling = false
  if (reset) {
    pollInitialized = false
    knownMessageIds.clear()
  }

  recordDesktopDiagnostic('reminder.poll.started', {
    userIdMasked: maskDiagnosticIdentifier(activeUserId),
    reset,
    generation: pollGeneration,
  })

  void pollUnreadMessages()
  pollTimer = window.setInterval(() => {
    void pollUnreadMessages()
  }, SYS_MESSAGE_POLL_INTERVAL)
}

function stopPolling() {
  window.clearInterval(pollTimer)
  pollTimer = undefined
  pollGeneration += 1
  polling = false
  pollInitialized = false
  knownMessageIds.clear()
  recordDesktopDiagnostic('reminder.poll.stopped', {
    generation: pollGeneration,
  })
}

function teardownSocket() {
  window.clearTimeout(reconnectTimer)
  if (!socket) return

  const existing = socket
  socket = null
  existing.close()
}

function scheduleReconnect() {
  if (!shouldReconnect || !activeUserId.trim()) return

  window.clearTimeout(reconnectTimer)
  const delay = Math.min(30000, 3000 + reconnectAttempts * 2000)
  recordDesktopDiagnostic('reminder.websocket.reconnect_scheduled', {
    userIdMasked: maskDiagnosticIdentifier(activeUserId),
    attempt: reconnectAttempts + 1,
    delayMs: delay,
  })
  reconnectTimer = window.setTimeout(() => {
    reconnectAttempts += 1
    connectSocket()
  }, delay)
}

function connectSocket() {
  if (!activeUserId.trim()) {
    recordDesktopDiagnostic('reminder.websocket.skipped', { reason: 'missing-user-id' })
    return
  }

  const url = buildSysMessageWebSocketUrl(activeUserId)
  if (!url) {
    recordDesktopDiagnostic('reminder.websocket.skipped', {
      reason: 'missing-endpoint',
      userIdMasked: maskDiagnosticIdentifier(activeUserId),
    })
    return
  }

  teardownSocket()

  let nextSocket: WebSocket
  try {
    nextSocket = new WebSocket(url)
    recordDesktopDiagnostic('reminder.websocket.connecting', {
      userIdMasked: maskDiagnosticIdentifier(activeUserId),
      secure: url.startsWith('wss:'),
      attempt: reconnectAttempts + 1,
    })
  } catch (error) {
    console.warn('Sys message websocket connection failed', error)
    recordDesktopDiagnostic('reminder.websocket.failed', {
      phase: 'construct',
      errorName: error instanceof Error ? error.name : 'unknown',
    })
    scheduleReconnect()
    return
  }
  socket = nextSocket

  nextSocket.addEventListener('open', () => {
    if (socket !== nextSocket) return
    reconnectAttempts = 0
    recordDesktopDiagnostic('reminder.websocket.authenticated', {
      userIdMasked: maskDiagnosticIdentifier(activeUserId),
    })
  })
  nextSocket.addEventListener('message', (event) => {
    if (socket !== nextSocket || typeof event.data !== 'string') return

    try {
      const message = normalizeSysMessage(JSON.parse(event.data) as SysMessagePushPayload)
      if (message) {
        recordDesktopDiagnostic('reminder.websocket.payload_received', {
          messageIdMasked: maskDiagnosticIdentifier(message.id),
          bizType: message.bizType ?? null,
        })
        deliverMessage(message)
      } else {
        recordDesktopDiagnostic('reminder.websocket.payload_ignored', {
          reason: 'unsupported-type-or-invalid-id',
        })
      }
    } catch (error) {
      console.warn('Invalid sys_message websocket payload', error)
      recordDesktopDiagnostic('reminder.websocket.payload_invalid', {
        errorName: error instanceof Error ? error.name : 'unknown',
      })
    }
  })
  nextSocket.addEventListener('close', () => {
    if (socket !== nextSocket) return
    socket = null
    recordDesktopDiagnostic('reminder.websocket.closed', {
      reconnect: shouldReconnect,
      userIdMasked: maskDiagnosticIdentifier(activeUserId),
    })
    if (shouldReconnect) scheduleReconnect()
  })
  nextSocket.addEventListener('error', () => {
    recordDesktopDiagnostic('reminder.websocket.failed', {
      phase: 'runtime',
      userIdMasked: maskDiagnosticIdentifier(activeUserId),
    })
    if (socket === nextSocket) nextSocket.close()
  })
}

export const sysMessageService = {
  onMessage(listener: MessageListener) {
    messageListeners.add(listener)
    return () => messageListeners.delete(listener)
  },
  connect(userId: string, options: { force?: boolean; catchUp?: boolean } = {}) {
    if (env.enableMock) {
      recordDesktopDiagnostic('reminder.subscription.skipped', { reason: 'mock-mode' })
      return
    }

    const nextUserId = userId.trim()
    if (!nextUserId) {
      recordDesktopDiagnostic('reminder.subscription.skipped', { reason: 'missing-user-id' })
      return
    }
    const userChanged = activeUserId !== nextUserId

    activeUserId = nextUserId
    shouldReconnect = true
    recordDesktopDiagnostic('reminder.subscription.started', {
      userIdMasked: maskDiagnosticIdentifier(nextUserId),
      userChanged,
      force: Boolean(options.force),
      websocketEndpointConfigured: Boolean(env.sysMessageWsBaseUrl.trim()),
    })
    if (options.catchUp) pollInitialized = false
    startPolling(userChanged)

    if (
      !options.force &&
      !userChanged &&
      socket &&
      (socket.readyState === WebSocket.CONNECTING || socket.readyState === WebSocket.OPEN)
    ) {
      return
    }

    reconnectAttempts = 0
    connectSocket()
  },
  async markRead(message: SysMessageNotification) {
    if (message.msgStatus === 1) return true

    const markedRead = await request.put<unknown, boolean>('/sys-message/read', {
      ids: [normalizeRequestId(message.rawId)],
    })
    if (markedRead !== true) throw new Error('服务端未确认消息已读')
    message.msgStatus = 1
    return true
  },
  async markAllRead(messages: SysMessageNotification[]) {
    const unreadMessages = messages.filter((message) => message.msgStatus !== 1)
    const ids = Array.from(new Set(
      unreadMessages.map((message) => normalizeRequestId(message.rawId))
    ))
    if (!ids.length) return true

    const markedRead = await request.put<unknown, boolean>('/sys-message/read', { ids })
    if (markedRead !== true) throw new Error('服务端未确认全部消息已读')
    unreadMessages.forEach((message) => {
      message.msgStatus = 1
    })
    return true
  },
  disconnect() {
    const userIdMasked = maskDiagnosticIdentifier(activeUserId)
    shouldReconnect = false
    activeUserId = ''
    stopPolling()
    teardownSocket()
    recordDesktopDiagnostic('reminder.subscription.stopped', { userIdMasked })
  }
}
