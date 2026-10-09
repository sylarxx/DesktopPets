export function formatSysMessageActionError(error: unknown, options: { all?: boolean; viewed?: boolean } = {}) {
  const failure = error as { status?: number; code?: number; message?: string } | null
  const prefix = options.viewed ? '详情已打开，' : ''
  if (failure?.status === 401 || failure?.code === 401) {
    return `${prefix}登录状态已失效，请重新登录；也可关闭此提醒`
  }
  if (failure?.status === 403 || failure?.code === 403) {
    return `${prefix}当前账号无法标记已读，可关闭提醒并在工作台查看`
  }
  if (failure?.message?.includes('服务端未确认')) {
    return `${prefix}后台尚未确认${options.all ? '全部消息' : '消息'}已读，可重试或关闭提醒`
  }
  if (failure?.message?.includes('超时')) {
    return `${prefix}已读结果尚未确认，可重试或关闭提醒`
  }
  return `${prefix}未能同步${options.all ? '全部消息' : '消息'}已读状态，可重试或关闭提醒`
}
