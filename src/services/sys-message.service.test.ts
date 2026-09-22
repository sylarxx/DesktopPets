import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  put: vi.fn(),
}))

vi.mock('./request', () => ({
  request: { get: mocks.get, put: mocks.put },
}))

vi.mock('../utils/env', () => ({
  env: {
    enableMock: false,
    sysMessageWsBaseUrl: 'http://hlai.hlmc.cn:5900',
  },
}))

class FakeWebSocket {
  static CONNECTING = 0
  static OPEN = 1
  static instances: FakeWebSocket[] = []

  readonly url: string
  readyState = FakeWebSocket.OPEN
  private listeners = new Map<string, Array<(event: Event) => void>>()

  constructor(url: string) {
    this.url = url
    FakeWebSocket.instances.push(this)
  }

  addEventListener(type: string, listener: (event: Event) => void) {
    const listeners = this.listeners.get(type) ?? []
    listeners.push(listener)
    this.listeners.set(type, listeners)
  }

  close() {
    this.readyState = 3
    this.listeners.get('close')?.forEach((listener) => listener(new Event('close')))
  }

  open() {
    this.readyState = FakeWebSocket.OPEN
    this.listeners.get('open')?.forEach((listener) => listener(new Event('open')))
  }

  message(payload: unknown) {
    const event = Object.assign(new Event('message'), { data: JSON.stringify(payload) })
    this.listeners.get('message')?.forEach((listener) => listener(event))
  }

  serverClose() {
    this.readyState = 3
    this.listeners.get('close')?.forEach((listener) => listener(new Event('close')))
  }
}

import { sysMessageService } from './sys-message.service'

describe('sysMessageService', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-07-14T10:05:00'))
    vi.stubGlobal('window', {
      location: { protocol: 'http:', host: 'tauri.localhost' },
      setInterval: globalThis.setInterval,
      clearInterval: globalThis.clearInterval,
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    })
    vi.stubGlobal('WebSocket', FakeWebSocket)
    FakeWebSocket.instances = []
    mocks.get.mockReset()
    mocks.put.mockReset()
  })

  afterEach(() => {
    sysMessageService.disconnect()
    vi.unstubAllGlobals()
    vi.useRealTimers()
  })

  it('使用未读消息轮询作为 WebSocket 的提醒兜底', async () => {
    mocks.get.mockResolvedValue({
      rows: [
        {
          id: 101,
          msgSubject: '新待办提醒',
          msgContent: '你有一条新待办',
          msgStatus: 0,
          msgType: 1,
          bizType: 1,
          bizId: 42,
          createTime: '2026-07-14 10:00:00',
        },
      ],
    })
    const listener = vi.fn()
    const removeListener = sysMessageService.onMessage(listener)

    sysMessageService.connect('10002')
    await Promise.resolve()
    await Promise.resolve()

    expect(mocks.get).toHaveBeenCalledWith('/sys-message/page', {
      timeoutMs: 10_000,
      params: {
        pageNum: 1,
        pageSize: 20,
        msgStatus: 0,
      },
    })
    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({
        id: '101',
        rawId: 101,
        msgSubject: '新待办提醒',
        bizId: '42',
      }),
    )

    removeListener()
  })

  it('marks the selected message as read in the backend', async () => {
    mocks.get.mockResolvedValue({ rows: [] })
    mocks.put.mockResolvedValue(true)
    const message = {
      id: '101',
      rawId: 101,
      dedupeKey: '101',
      msgSubject: '新待办提醒',
      msgContent: '你有一条新待办',
      msgStatus: 0 as const,
      msgType: 1,
    }

    await expect(sysMessageService.markRead(message)).resolves.toBe(true)

    expect(mocks.put).toHaveBeenCalledWith('/sys-message/read', { ids: [101] })
    expect(message.msgStatus).toBe(1)
  })

  it('keeps the message unread when the backend returns data false', async () => {
    mocks.get.mockResolvedValue({ rows: [] })
    mocks.put.mockResolvedValue(false)
    const message = {
      id: '101',
      rawId: 101,
      dedupeKey: '101',
      msgSubject: '新待办提醒',
      msgContent: '你有一条新待办',
      msgStatus: 0 as const,
      msgType: 1,
    }

    await expect(sysMessageService.markRead(message)).rejects.toThrow('服务端未确认消息已读')
    expect(message.msgStatus).toBe(0)
  })

  it('marks the current and queued messages as read in one request', async () => {
    mocks.get.mockResolvedValue({ rows: [] })
    mocks.put.mockResolvedValue(true)
    const messages = [
      {
        id: '101', rawId: 101, dedupeKey: '101', msgSubject: '提醒 1', msgContent: '',
        msgStatus: 0 as const, msgType: 1,
      },
      {
        id: '102', rawId: '102', dedupeKey: '102', msgSubject: '提醒 2', msgContent: '',
        msgStatus: 0 as const, msgType: 1,
      },
    ]

    await expect(sysMessageService.markAllRead(messages)).resolves.toBe(true)

    expect(mocks.put).toHaveBeenCalledWith('/sys-message/read', { ids: [101, 102] })
    expect(messages.map((message) => message.msgStatus)).toEqual([1, 1])
  })

  it('keeps every queued message when batch read is not confirmed', async () => {
    mocks.get.mockResolvedValue({ rows: [] })
    mocks.put.mockResolvedValue(false)
    const messages = [
      {
        id: '101', rawId: 101, dedupeKey: '101', msgSubject: '提醒 1', msgContent: '',
        msgStatus: 0 as const, msgType: 1,
      },
      {
        id: '102', rawId: 102, dedupeKey: '102', msgSubject: '提醒 2', msgContent: '',
        msgStatus: 0 as const, msgType: 1,
      },
    ]

    await expect(sysMessageService.markAllRead(messages)).rejects.toThrow(
      '服务端未确认全部消息已读'
    )
    expect(messages.map((message) => message.msgStatus)).toEqual([0, 0])
  })

  it('忽略退出或切换用户后才返回的旧轮询结果', async () => {
    let resolveOldRequest: ((value: unknown) => void) | undefined
    mocks.get
      .mockImplementationOnce(() => new Promise((resolve) => {
        resolveOldRequest = resolve
      }))
      .mockResolvedValueOnce({
        rows: [{
          id: 202,
          msgSubject: '新用户的会议提醒',
          msgContent: '会议将于 15 分钟后开始',
          msgStatus: 0,
          msgType: 1,
          bizType: 2,
        }],
      })
    const listener = vi.fn()
    const removeListener = sysMessageService.onMessage(listener)

    sysMessageService.connect('old-user')
    sysMessageService.disconnect()
    sysMessageService.connect('new-user')
    await Promise.resolve()
    await Promise.resolve()

    resolveOldRequest?.({
      rows: [{
        id: 101,
        msgSubject: '旧用户的待办提醒',
        msgContent: '这条消息不应出现',
        msgStatus: 0,
        msgType: 1,
        bizType: 1,
      }],
    })
    await Promise.resolve()
    await Promise.resolve()

    expect(listener).toHaveBeenCalledTimes(1)
    expect(listener).toHaveBeenCalledWith(expect.objectContaining({ id: '202' }))

    removeListener()
  })

  it('通过同一鉴权连接接收待办、会议、消息并在断线后重连', async () => {
    mocks.get.mockResolvedValue({ rows: [] })
    const listener = vi.fn()
    const removeListener = sysMessageService.onMessage(listener)

    sysMessageService.connect('10002')
    const firstSocket = FakeWebSocket.instances[0]
    firstSocket.open()
    for (const bizType of [1, 2, 3]) {
      firstSocket.message({
        type: 'sys_message',
        id: `message-${bizType}`,
        msgSubject: `提醒 ${bizType}`,
        msgContent: '测试消息',
        msgStatus: 0,
        msgType: 1,
        bizType,
      })
    }

    expect(listener.mock.calls.map(([message]) => message.bizType)).toEqual([1, 2, 3])

    firstSocket.serverClose()
    await vi.advanceTimersByTimeAsync(2_999)
    expect(FakeWebSocket.instances).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(FakeWebSocket.instances).toHaveLength(2)

    removeListener()
  })
  it('补入有效未读按时间排序，共用一次提示；半小时前消息不复活', async () => {
    mocks.get.mockResolvedValue({ rows: [
      { id: 3, createTime: '2026-07-14 10:04:00' },
      { id: 1, createTime: '2026-07-14 09:34:59' },
      { id: 2, createTime: '2026-07-14 09:36:00' },
    ] })
    const listener = vi.fn()
    const remove = sysMessageService.onMessage(listener)
    sysMessageService.connect('user')
    await vi.advanceTimersByTimeAsync(0)
    expect(listener.mock.calls.map(([message]) => message.id)).toEqual(['2', '3'])
    expect(new Set(listener.mock.calls.map(([message]) => message.attentionKey)).size).toBe(1)
    await vi.advanceTimersByTimeAsync(10_000)
    expect(listener).toHaveBeenCalledTimes(2)
    remove()
  })

  it('补入最多五页，服务端重复页提前结束', async () => {
    mocks.get.mockImplementation(async (_path, options) => ({ rows: Array.from({length: 20}, (_, i) => ({
      id: options.params.pageNum * 20 + i, createTime: '2026-07-14 10:00:00',
    })) }))
    sysMessageService.connect('user')
    await vi.advanceTimersByTimeAsync(0)
    expect(mocks.get).toHaveBeenCalledTimes(5)
    sysMessageService.disconnect()
    mocks.get.mockReset().mockResolvedValue({ rows: Array.from({length: 20}, (_, id) => ({ id })) })
    sysMessageService.connect('user')
    await vi.advanceTimersByTimeAsync(0)
    expect(mocks.get).toHaveBeenCalledTimes(2)
  })

  it('解锁强制重连后补取多页，保留去重且拒绝旧连接迟到消息', async () => {
    const page = Array.from({ length: 20 }, (_, i) => ({ id: i + 1, createTime: '2026-07-14 10:00:00' }))
    mocks.get.mockResolvedValue({ rows: [page[0]] })
    const listener = vi.fn()
    const remove = sysMessageService.onMessage(listener)
    sysMessageService.connect('same-user')
    await vi.advanceTimersByTimeAsync(0)
    const old = FakeWebSocket.instances[0]
    mocks.get.mockReset().mockImplementation(async (_path, options) => ({
      rows: options.params.pageNum === 1 ? page : [{ id: 21, createTime: '2026-07-14 10:01:00' }],
    }))
    sysMessageService.connect('same-user', { force: true, catchUp: true })
    old.message({ type: 'sys_message', id: 'stale', msgSubject: 'old' })
    old.serverClose()
    await vi.advanceTimersByTimeAsync(0)
    expect(FakeWebSocket.instances).toHaveLength(2)
    expect(mocks.get).toHaveBeenCalledTimes(2)
    expect(listener.mock.calls.map(([message]) => message.id)).toEqual(Array.from({ length: 21 }, (_, i) => String(i + 1)))
    expect(new Set(listener.mock.calls.slice(1).map(([message]) => message.attentionKey)).size).toBe(1)
    expect(mocks.put).not.toHaveBeenCalled()
    remove()
  })

})
