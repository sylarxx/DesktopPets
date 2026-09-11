export type SysMessageStatus = 0 | 1

export interface SysMessagePushPayload {
  type: 'sys_message'
  id?: string | number
  msgSubject?: string
  msgContent?: string
  msgStatus?: number
  msgType?: number
  bizType?: number
  bizId?: string | number
  createTime?: string
}

export interface SysMessageNotification {
  id: string
  rawId: string | number
  /** A bounded initial catch-up batch shares one quiet attention cycle. */
  attentionKey?: string
  /** 用于短时去重，避免服务端重复推送同一条消息 */
  dedupeKey: string
  msgSubject: string
  msgContent: string
  msgStatus: SysMessageStatus
  msgType: number
  bizType?: number
  bizId?: string
  createTime?: string
}
