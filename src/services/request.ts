import { isTauri } from '@tauri-apps/api/core'
import { fetch as nativeFetch } from '@tauri-apps/plugin-http'
import { env } from '../utils/env'
import { storage } from '../utils/storage'
import { getDiagnosticCredentialMetadata, recordDesktopDiagnostic } from './diagnostic.service'

interface BusinessResponse<T = unknown> {
  code?: number
  success?: boolean
  msg?: string
  message?: string
  data?: T
}

export class DesktopRequestError extends Error {
  status?: number
  code?: number

  constructor(message: string, options: { status?: number; code?: number } = {}) {
    super(message)
    this.name = 'DesktopRequestError'
    this.status = options.status
    this.code = options.code
  }
}

interface DesktopRequestOptions {
  timeoutMs?: number
  params?: Record<string, string | number | boolean | null | undefined>
  reportUnauthorized?: boolean
}

export interface DesktopUnauthorizedContext {
  token: string
}

type UnauthorizedListener = (context: DesktopUnauthorizedContext) => void
type DesktopRequestMethod = 'GET' | 'POST' | 'PUT'

const unauthorizedListeners = new Set<UnauthorizedListener>()
const REQUEST_TIMEOUT = 12_000

function getDiagnosticRoute(path: string) {
  const pathname = path.split('?')[0]
  if (pathname.startsWith('/smart-todo/complete/')) return '/smart-todo/complete/:id'
  if (/^\/smart-todo\/[^/]+$/.test(pathname)) return '/smart-todo/:id'
  return pathname
}

function notifyUnauthorized(token: string, status?: number, code?: number) {
  // 401 means the token is no longer accepted. A 403 can instead mean this
  // user lacks one optional desktop permission, so it must not erase an
  // otherwise valid login.
  if (status !== 401 && code !== 401) return
  unauthorizedListeners.forEach((listener) => listener({ token }))
}

function getBusinessMessage(response: BusinessResponse, fallback: string) {
  return response.msg || response.message || fallback
}

function buildRequestUrl(path: string, options: DesktopRequestOptions = {}) {
  const configuredBaseUrl = env.apiBaseUrl.trim().replace(/\/+$/, '')
  const baseUrl = configuredBaseUrl || window.location.origin
  const url = new URL(`${baseUrl}/${path.replace(/^\/+/, '')}`)

  Object.entries(options.params ?? {}).forEach(([key, value]) => {
    if (value !== null && value !== undefined) url.searchParams.set(key, String(value))
  })
  return url.toString()
}

async function parseResponse(response: Response) {
  const text = await response.text()
  if (!text) return undefined

  try {
    return JSON.parse(text) as BusinessResponse
  } catch {
    throw new DesktopRequestError('后台返回了无法识别的内容', { status: response.status })
  }
}

async function fetchResponse(url: string, init: RequestInit, timeoutMs = REQUEST_TIMEOUT) {
  const controller = new AbortController()
  let timer: ReturnType<typeof setTimeout> | undefined
  const expired = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(new DesktopRequestError('连接后台服务超时，操作结果尚未确认'))
      controller.abort()
    }, Math.max(1, Math.min(REQUEST_TIMEOUT, timeoutMs)))
  })
  try {
    // The deadline includes connection, headers AND body consumption. Race as
    // well as abort: a stalled native bridge must still release the UI.
    return await Promise.race([
      (async () => {
        const response = isTauri()
          ? await nativeFetch(url, { ...init, signal: controller.signal, connectTimeout: REQUEST_TIMEOUT })
          : await window.fetch(url, { ...init, signal: controller.signal })
        const result = await parseResponse(response)
        return { response, result }
      })(),
      expired,
    ])
  } finally {
    if (timer !== undefined) clearTimeout(timer)
  }
}

async function send<T>(
  method: DesktopRequestMethod,
  path: string,
  body?: unknown,
  options: DesktopRequestOptions = {},
): Promise<T> {
  const headers = new Headers({ Accept: 'application/json' })
  const token = storage.getToken() || env.mockToken
  const diagnosticRoute = getDiagnosticRoute(path)
  if (token) headers.set('Authorization', `Bearer ${token}`)
  if (body !== undefined) headers.set('Content-Type', 'application/json')

  let response: Response
  let result: BusinessResponse | undefined
  try {
    ;({ response, result } = await fetchResponse(buildRequestUrl(path, options), {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    }, options.timeoutMs))
  } catch (error) {
    recordDesktopDiagnostic('request.transport_failed', {
      method,
      route: diagnosticRoute,
      tokenPresent: Boolean(token),
      tokenLength: token.length,
      errorName: error instanceof Error ? error.name : 'unknown',
      ...getDiagnosticCredentialMetadata(token),
    })
    const message = error instanceof Error && error.name === 'AbortError'
      ? '连接后台服务超时'
      : error instanceof Error
        ? error.message
        : '无法连接后台服务'
    throw new DesktopRequestError(message)
  }

  const code = typeof result?.code === 'number' ? result.code : undefined

  if (!response.ok || result?.success === false || (code !== undefined && code !== 200)) {
    recordDesktopDiagnostic('request.rejected', {
      method,
      route: diagnosticRoute,
      responseStatus: response.status,
      businessCode: code ?? null,
      reportUnauthorized: options.reportUnauthorized !== false,
      tokenPresent: Boolean(token),
      tokenLength: token.length,
      ...getDiagnosticCredentialMetadata(token),
    })
    if (options.reportUnauthorized !== false) {
      notifyUnauthorized(token, response.status, code)
    }
    throw new DesktopRequestError(
      getBusinessMessage(result ?? {}, response.ok ? '接口请求失败' : `请求失败（${response.status}）`),
      { status: response.status, code },
    )
  }

  return (result?.data ?? result) as T
}

export const request = {
  get<_Request = unknown, ResponseData = unknown>(path: string, options?: DesktopRequestOptions) {
    return send<ResponseData>('GET', path, undefined, options)
  },
  post<_Request = unknown, ResponseData = unknown>(
    path: string,
    body?: unknown,
    options?: DesktopRequestOptions,
  ) {
    return send<ResponseData>('POST', path, body, options)
  },
  put<_Request = unknown, ResponseData = unknown>(
    path: string,
    body?: unknown,
    options?: DesktopRequestOptions,
  ) {
    return send<ResponseData>('PUT', path, body, options)
  },
}

export function onDesktopUnauthorized(listener: UnauthorizedListener) {
  unauthorizedListeners.add(listener)
  return () => unauthorizedListeners.delete(listener)
}
