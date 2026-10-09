import { describe, expect, it } from 'vitest'
import { formatSysMessageActionError } from './sys-message-action-error'

describe('message read feedback', () => {
  it('distinguishes expired login, permission denial and unconfirmed server state', () => {
    expect(formatSysMessageActionError({ status: 401 })).toContain('登录状态已失效')
    expect(formatSysMessageActionError({ code: 403 })).toContain('当前账号无法标记已读')
    expect(formatSysMessageActionError(new Error('服务端未确认消息已读'))).toContain('后台尚未确认消息已读')
  })
  it('never assumes a timed out operation failed or blames every error on the network', () => {
    expect(formatSysMessageActionError(new Error('连接后台服务超时'))).toContain('已读结果尚未确认')
    expect(formatSysMessageActionError(new Error('unknown'))).not.toContain('请检查网络')
    expect(formatSysMessageActionError(null, { all: true, viewed: true })).toBe('详情已打开，未能同步全部消息已读状态，可重试或关闭提醒')
  })
})
