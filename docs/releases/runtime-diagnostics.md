# 管理员故障定位记录

1.0.55 的运行记录仅用于复发定位。每个用户最多两个固定文件，各不超过 65536 字节，总量不超过 131072 字节（128 KiB）；写入前检查，满后覆盖轮换，超限单条丢弃，启动时清理旧超限文件。无法写入时不影响正常使用。

路径：`%LOCALAPPDATA%\com.huali.ai.mascot\logs\runtime-health.jsonl` 和 `runtime-health.jsonl.1`。用户无需执行命令；发生问题后由管理员提取两文件，保留故障时间及安装版本。文件含事件名、UTC 时间戳、相对运行时间、进程号、恢复轮次及固定窗口标签，不含账号、消息 ID/正文、URL、凭据或异常原文。

| 事件 | 已观察到的结果 |
| --- | --- |
| `notification-layout-unconfirmed` | 卡片布局确认未收到，内容发布或卡片界面响应未完成 |
| `notification-show-unconfirmed` | 原生显示步骤未确认成功 |
| `notification-paint-unconfirmed` | 原生显示后，当前批次的渲染/可见性确认未收到 |
| `notification-delivery-stopped` | 三次或十秒预算用完，保留消息并释放不可见队首 |
| `notification-window-unavailable` / `notification-mascot-unavailable` | 对应原生窗口不存在 |
| `notification-position-failed` / `notification-native-show-failed` | 原生定位或显示失败 |
| `notification-visible-rejected` | 帧回执不符合当前批次、会话或真实窗口可见状态 |
| `notification-visible-confirmed` | 当前批次的显示后帧及原生可见性确认成功；只在实际显示确认时记录，不逐帧或随正文刷新记录 |
| `main-thread-probe-stalled` | 单个已投递 UI 探针超过 25 秒未执行；仅记一次，锁屏/休眠及其恢复宽限不计为故障 |
| `message-read-reconciled` | 写接口返回 false，但已明确回读确认同一消息已读 |
| `message-read-unconfirmed` | 写接口返回 false，限定范围内未确认同一消息已读 |
| `message-read-unauthorized` / `message-read-forbidden` | 已读操作遇到登录失效或权限不足 |
| `message-read-timeout` / `message-read-failed` | 已读操作超时或其他失败，后台是否写成功仍需核对 |
| `reminder-poll-failed` / `reminder-websocket-failed` | 消息轮询或连接失败 |

原生会话、渲染进程/浏览器进程故障和重载/重建事件可结合相同进程、时间、恢复轮次判断先后关系。记录证明的是这些程序结果；渲染确认仍不能代替实际 GPU 合成像素或现场 HTTP 响应。管理员可据已失败的阶段再取对应系统或服务器证据，避免从泛化的“网络错误”提示猜测原因。

旧的 `desktop-diagnostic.jsonl` / `.1` 在客户端启动时清理，旧的 `C:\ProgramData\HualiAI\Logs\launch-after-install.log` 在 MSI 的管理员/SYSTEM 启动辅助路径清理。只删除已知文件，不删除日志目录或其他用户文件。MSI 安装日志由管理员部署平台控制，和这两个客户端运行记录分开。
