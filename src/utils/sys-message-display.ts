export function formatSysMessageDisplayTime(rawValue?: string, now = new Date()) {
  const raw = rawValue?.trim()
  if (!raw) return '刚刚'

  const parsed = new Date(raw.replace(' ', 'T'))
  if (Number.isNaN(parsed.getTime())) return raw.slice(0, 16)

  const time = new Intl.DateTimeFormat('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23'
  }).format(parsed)
  const isToday = parsed.getFullYear() === now.getFullYear()
    && parsed.getMonth() === now.getMonth()
    && parsed.getDate() === now.getDate()

  if (isToday) return `今天 ${time}`
  return `${parsed.getMonth() + 1}月${parsed.getDate()}日 ${time}`
}

export function normalizeSysMessageDateTime(rawValue?: string) {
  const raw = rawValue?.trim()
  if (!raw) return undefined

  const normalized = raw.replace(' ', 'T')
  return Number.isNaN(new Date(normalized).getTime()) ? undefined : normalized
}

/** Business type owns the color, icon and label; text is fallback only. */
export function classifySysMessage(bizType: number | undefined, title: string) {
  if (bizType !== undefined) return bizType === 2 ? 'meeting' : bizType === 1 ? 'todo' : 'notice'
  if (/会议/.test(title)) return 'meeting'
  if (/待办|任务|todo/i.test(title)) return 'todo'
  return 'notice'
}
