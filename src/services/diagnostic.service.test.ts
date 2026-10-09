import { beforeEach, describe, expect, it, vi } from 'vitest'

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}))

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}))

import {
  getDiagnosticCredentialMetadata,
  maskDiagnosticIdentifier,
  recordDesktopDiagnostic,
} from './diagnostic.service'
import diagnosticServiceSource from './diagnostic.service.ts?raw'
import rustSource from '../../src-tauri/src/main.rs?raw'

function toBase64Url(value: object) {
  return btoa(JSON.stringify(value))
    .replace(/=/g, '')
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
}

describe('desktop diagnostics', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue(true)
  })

  it('masks user identifiers before they are logged', () => {
    expect(maskDiagnosticIdentifier('employee-123456')).toBe('em***56')
    expect(maskDiagnosticIdentifier('u001')).toBe('****')
    expect(maskDiagnosticIdentifier('')).toBe('')
  })

  it('does not send renderer diagnostics to the native writer in formal releases', async () => {
    recordDesktopDiagnostic('session.restore', {
      rawUrl: 'huali-ai-mascot://auth-callback?token=complete-token&state=complete-state',
      token: 'complete-token',
      state: 'complete-state',
      tokenPresent: true,
      tokenLength: 128,
      userId: 'employee-123456',
      userIdMasked: maskDiagnosticIdentifier('employee-123456'),
    })
    await Promise.resolve()

    expect(invokeMock).not.toHaveBeenCalled()
  })

  it('sends only the fixed runtime event and drops all legacy fields', async () => {
    recordDesktopDiagnostic('notification.delivery.paint_unconfirmed', {
      token: 'complete-token', userId: 'employee-123456', rawUrl: 'https://private/path',
      messageContent: 'private message', generation: 123,
    })
    await Promise.resolve()
    expect(invokeMock).toHaveBeenCalledExactlyOnceWith('record_desktop_runtime_event', {
      event: 'notification-paint-unconfirmed',
    })
    recordDesktopDiagnostic('toString', { token: 'complete-token' })
    recordDesktopDiagnostic('unknown.failure', { token: 'complete-token' })
    expect(invokeMock).toHaveBeenCalledTimes(1)
  })

  it.each([
    ['notification.delivery.layout_unconfirmed', 'notification-layout-unconfirmed'],
    ['notification.delivery.show_unconfirmed', 'notification-show-unconfirmed'],
    ['notification.delivery.paint_unconfirmed', 'notification-paint-unconfirmed'],
    ['notification.delivery.stopped', 'notification-delivery-stopped'],
    ['message.read.reconciled', 'message-read-reconciled'],
    ['message.read.unconfirmed', 'message-read-unconfirmed'],
    ['reminder.poll.failed', 'reminder-poll-failed'],
    ['reminder.websocket.failed', 'reminder-websocket-failed'],
  ])('maps %s to one fixed native event', (source, event) => {
    recordDesktopDiagnostic(source, { token: 'discarded', messageId: 'discarded' })
    expect(invokeMock).toHaveBeenCalledExactlyOnceWith('record_desktop_runtime_event', { event })
  })

  it('classifies read failures with fixed events and never passes status or error content', () => {
    recordDesktopDiagnostic('message.read.failed', { responseStatus: 401, timedOut: true })
    recordDesktopDiagnostic('message.read.failed', { businessCode: 403 })
    recordDesktopDiagnostic('message.read.failed', { timedOut: true })
    recordDesktopDiagnostic('message.read.failed', { error: 'raw private failure' })
    expect(invokeMock.mock.calls).toEqual([
      ['record_desktop_runtime_event', { event: 'message-read-unauthorized' }],
      ['record_desktop_runtime_event', { event: 'message-read-forbidden' }],
      ['record_desktop_runtime_event', { event: 'message-read-timeout' }],
      ['record_desktop_runtime_event', { event: 'message-read-failed' }],
    ])
  })

  it('does not let unavailable native logging affect the business caller', async () => {
    invokeMock.mockRejectedValueOnce(new Error('native logging unavailable'))
    expect(() => recordDesktopDiagnostic('message.read.unconfirmed')).not.toThrow()
    await Promise.resolve()
    invokeMock.mockImplementationOnce(() => { throw new Error('bridge unavailable') })
    expect(() => recordDesktopDiagnostic('message.read.reconciled')).not.toThrow()
  })

  it('removes legacy JSONL files at startup and keeps the legacy native writer disabled', () => {
    const rendererWriter = diagnosticServiceSource.slice(
      diagnosticServiceSource.indexOf('export function recordDesktopDiagnostic'),
    )
    const nativeWriter = rustSource.slice(
      rustSource.indexOf('fn write_desktop_diagnostic_event'),
      rustSource.indexOf('fn diagnostic_fields'),
    )

    expect(rendererWriter).toContain("'record_desktop_runtime_event', { event }")
    expect(nativeWriter).toContain('false')
    expect(nativeWriter).not.toContain('OpenOptions')
    expect(nativeWriter).not.toContain('write_all')
    expect(rustSource).toContain('path.is_file()')
    expect(rustSource).toContain('fs::remove_file(path)')
    expect(rustSource).toContain('primary_path.with_extension("jsonl.1")')
    expect(rustSource).toContain('cleanup_desktop_diagnostic_logs(app.handle());')
  })

  it('extracts only JWT lifetime metadata and never returns credential content', () => {
    const credential = [
      toBase64Url({ alg: 'none' }),
      toBase64Url({ sub: 'employee-123456', iat: 1_000, exp: 4_600 }),
      'signature-secret',
    ].join('.')

    const metadata = getDiagnosticCredentialMetadata(credential, 2_000_000)

    expect(metadata).toEqual({
      credentialFormat: 'jwt',
      credentialExpiryKnown: true,
      credentialExpired: false,
      credentialAgeSeconds: 1_000,
      credentialLifetimeSeconds: 3_600,
      credentialRemainingSeconds: 2_600,
    })
    expect(JSON.stringify(metadata)).not.toContain('employee-123456')
    expect(JSON.stringify(metadata)).not.toContain('signature-secret')
  })

  it('marks opaque and missing credentials without inspecting their content', () => {
    expect(getDiagnosticCredentialMetadata('opaque-secret')).toEqual({
      credentialFormat: 'opaque',
      credentialExpiryKnown: false,
    })
    expect(getDiagnosticCredentialMetadata('')).toEqual({
      credentialFormat: 'missing',
      credentialExpiryKnown: false,
    })
  })
})
