import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  isTauri: vi.fn(),
  nativeFetch: vi.fn(),
  getToken: vi.fn(),
}))

vi.mock('@tauri-apps/api/core', () => ({
  isTauri: mocks.isTauri,
}))

vi.mock('@tauri-apps/plugin-http', () => ({
  fetch: mocks.nativeFetch,
}))

vi.mock('../utils/env', () => ({
  env: {
    apiBaseUrl: 'http://hlai.hlmc.cn:5900/backendApi',
    mockToken: '',
  },
}))

vi.mock('../utils/storage', () => ({
  storage: { getToken: mocks.getToken },
}))

import { DesktopRequestError, onDesktopUnauthorized, request } from './request'

describe('desktop request', () => {
  afterEach(() => vi.useRealTimers())
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.isTauri.mockReturnValue(true)
    mocks.getToken.mockReturnValue('desktop-token')
  })

  it('uses the native HTTP client for intranet requests', async () => {
    mocks.nativeFetch.mockResolvedValue(
      new Response(JSON.stringify({ code: 200, data: { rows: [{ id: 1 }] } }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    )

    await expect(
      request.get('/sys-message/page', { params: { pageNum: 1, msgStatus: 0 } }),
    ).resolves.toEqual({ rows: [{ id: 1 }] })

    const [url, init] = mocks.nativeFetch.mock.calls[0] as [string, RequestInit]
    expect(url).toBe('http://hlai.hlmc.cn:5900/backendApi/sys-message/page?pageNum=1&msgStatus=0')
    expect(new Headers(init.headers).get('Authorization')).toBe('Bearer desktop-token')
    expect(init).toMatchObject({ method: 'GET', connectTimeout: 12_000 })
  })

  it('surfaces backend business errors', async () => {
    mocks.nativeFetch.mockResolvedValue(
      new Response(JSON.stringify({ code: 500, msg: '消息服务不可用' }), { status: 200 }),
    )

    await expect(request.get('/sys-message/page')).rejects.toEqual(
      expect.objectContaining<Partial<DesktopRequestError>>({
        name: 'DesktopRequestError',
        message: '消息服务不可用',
        code: 500,
      }),
    )
  })

  it('reports which session token produced an unauthorized response', async () => {
    const listener = vi.fn()
    const removeListener = onDesktopUnauthorized(listener)
    mocks.getToken.mockReturnValue('old-session-token')
    mocks.nativeFetch.mockResolvedValue(
      new Response(JSON.stringify({ code: 401, msg: '登录状态已过期' }), { status: 200 }),
    )

    await expect(request.get('/getInfo')).rejects.toBeInstanceOf(DesktopRequestError)
    expect(listener).toHaveBeenCalledWith({ token: 'old-session-token' })

    removeListener()
  })

  it('does not treat a permission-only 403 as an expired login', async () => {
    const listener = vi.fn()
    const removeListener = onDesktopUnauthorized(listener)
    mocks.nativeFetch.mockResolvedValue(
      new Response(JSON.stringify({ code: 403, msg: '无权访问此消息接口' }), { status: 200 }),
    )

    await expect(request.get('/sys-message/page')).rejects.toBeInstanceOf(DesktopRequestError)
    expect(listener).not.toHaveBeenCalled()

    removeListener()
  })

  it('allows the dedicated session confirmation request to suppress recursive unauthorized events', async () => {
    const listener = vi.fn()
    const removeListener = onDesktopUnauthorized(listener)
    mocks.nativeFetch.mockResolvedValue(
      new Response(JSON.stringify({ code: 401, msg: '登录状态已过期' }), { status: 200 }),
    )

    await expect(
      request.get('/getInfo', { reportUnauthorized: false }),
    ).rejects.toBeInstanceOf(DesktopRequestError)
    expect(listener).not.toHaveBeenCalled()

    removeListener()
  })
  it('releases an operation when response headers arrive but the body never completes', async () => {
    vi.useFakeTimers()
    const text = vi.fn(() => new Promise<string>(() => {}))
    mocks.nativeFetch.mockResolvedValue({ ok: true, status: 200, text })
    const outcome = expect(request.put('/sys-message/read', { id: 1 }))
      .rejects.toThrow('连接后台服务超时')
    await vi.advanceTimersByTimeAsync(12_000)
    await outcome
    expect(mocks.nativeFetch).toHaveBeenCalledTimes(1)
    expect(mocks.nativeFetch.mock.calls[0][1].signal.aborted).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('ignores a late unauthorized body after the request deadline', async () => {
    vi.useFakeTimers()
    let finish!: (body: string) => void
    mocks.nativeFetch.mockResolvedValue({ ok: true, status: 200,
      text: () => new Promise<string>(resolve => { finish = resolve }) })
    const listener = vi.fn()
    const remove = onDesktopUnauthorized(listener)
    const outcome = expect(request.get('/getInfo')).rejects.toThrow('超时')
    await vi.advanceTimersByTimeAsync(12_000)
    await outcome
    finish(JSON.stringify({ code: 401 }))
    await vi.advanceTimersByTimeAsync(0)
    expect(listener).not.toHaveBeenCalled()
    remove()
  })

})
