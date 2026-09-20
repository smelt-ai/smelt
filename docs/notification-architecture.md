# 状态与通知架构

> 定位:当前实现(领域模型、出口规则、扩展约束)。设计动机与状态语义推导见 [`agent-state-notification-design.md`](agent-state-notification-design.md)。

## 目标

状态描述 agent 当前事实，关注事件描述一次需要告知用户的变化，投递渠道只决定在哪里
展示。三者不得互相代替：

```text
各 provider hook / ACP → AgentEvent v1 生命周期 / v2 标题元数据
                                      ↓
                               smeltd reducer → DaemonPhase → AgentStatus + AttentionItem
终端标题 ───────────────────────────────────────→ 展示元数据
OSC / BEL ──────────────────────────────────────→ Informational AttentionItem
                                                       ↓
                                            铃铛 / 系统通知 / Dock / 菜单栏
```

## 领域模型

- `DaemonPhase`：守护状态通道的细粒度事实。
- `AgentStatus`：UI 三态（要你 / 运行中 / 空闲），由 `AgentStatus::from_daemon_phase` 压缩。未读不占一态。
- `AttentionItem`：一次性关注事件，包含会话、标题、正文和 `AttentionKind`。
- `AttentionKind`：审批、输入、成功、失败、响铃、普通提示。
- `AttentionStore`：按 session id 保存当前事件、已读状态、行动项解决状态、投递队列
  和 60 秒去重指纹。

`AgentStatus` 不携带未读；`AttentionItem` 不充当持续状态。用户读过一条完成消息后，
会话仍可处于 `Succeeded`，但不应继续显示未读。

## 当前实现

Claude/Grok、Codex、Copilot、Antigravity、Cursor、OpenCode、Kiro v3 的 hook / plugin
事件，以及 Pi 的进程级 extension 事件，先由各自适配规则归一化为版本化 `AgentEvent`；
事件包含生命周期语义以及可用的 tool/agent 身份。Cursor 使用 `~/.cursor/hooks.json`，
OpenCode 使用 `~/.config/opencode/plugins/smelt-notifications.js`（设置 `XDG_CONFIG_HOME`
时跟随该目录），Kiro v3 engine 使用 `~/.kiro/hooks/smelt-notifications.json`；同一 CLI
的默认 v2 engine 不读取该全局 hooks 目录。Pi 快捷终端由 smeltd 进程级注入随应用打包的
`--extension`，不修改用户 `~/.pi` 配置；扩展用 `agent_start` / `tool_execution_*` /
`agent_settled` 上报运行、工具和最终完成状态，其中 `agent_settled` 保证自动重试、压缩重试
和排队 follow-up 尚未结束时不会提前报完成。Pi 的原生 RPC 对话驱动、DSH 和其它以 ACP 方式启动的
Agent 直接提供结构化状态，不重复注入终端桥。smeltd reducer 统一维护 phase，审批和输入
等待保持粘性，只有对应工具完成、新 prompt 或明确回合终态才能解除，避免并行子任务消息覆盖。
helper 每条 hook 只发送一次向后兼容的 `state + event` 信封：新 daemon 读取完整 event，
旧 daemon 忽略 event、读取 phase。helper 等 reducer 提交确认后才退出，不按超时重放同一
事件。provider 的同步 hook 因此把生命周期顺序延伸到 daemon。占用意图是
`Occupancy` 枚举：元数据不改相位，`OpenTurn`/`ResumeWork` 打开或重开回合，
`Progress`/`Wait`/`Finish` 不能把已关闭的回合重开。旧纯 phase 载荷仍可读取，
但终态后保守保持终态。

`AgentEvent v1` 覆盖会话开始/结束、用户提交、工具开始/成功/失败、审批、用户输入、
子任务开始/结束、回合成功/失败；这些既有生命周期事件继续按 v1 发送。v2 只新增纯元数据
事件 `SessionTitleChanged`：它更新 provider 发布的权威标题，但不改变 phase、`phase_since`、
等待状态或回合权限。新 reducer 同时接受 v1/v2；标题事件的旧 daemon 兼容 phase 为 null，
不会把一次改名误判成 Idle。

`PromptSubmitted` 可附带由首条用户请求压缩出的 `conversation_title`：它只在 provider
尚未给出语义标题时充当本地兜底，不混入 `pending_question`。后续 ACP/OSC 显式标题（例如
Codex `/rename`）仍优先；纯产品名、项目名和 spinner 帧不能反向覆盖已经建立的对话标题。
provider 工具名只在适配层用于把原始 hook 判定为这些稳定语义之一，不进入 UI 的状态判断
逻辑。

结构化 hook/ACP 通过 `apply_daemon_transition` 写入 store。终端标题始终只是展示元数据；
会话尚未收到有效结构化事件时，OSC/BEL 可以在 `TerminalView` 中翻译成信息性
`AttentionItem`，但绝不能改变 phase 或伪造完成。结构化事件一旦接通，低可信终端通知
永久退出该会话的 agent 通知链路，避免重复投递。
hook 检测到且已在 `TerminalAgentKind` 注册的 provider 独立保存在会话状态中，不改写不可变
的启动命令 `launch`；它与标题通过 `session.state_changed` 的会话投影 v2 下发。客户端仍可
读取旧 daemon 的 v1 会话投影，其他核心投影继续保持 v1。

所有出口读取同一个 store：

- 正在查看对应 pane：标记已读，不弹通知。
- 「智能体 > 对话」与项目会话都通过同一个 `viewed_session_id` 判断当前 pane，不能因
  顶层 route 不是 `Session` 就把用户眼前的智能体对话误判成后台会话。
- 其余未查看事件一律走 macOS 系统通知。
- 前后台以 `NSApplication.isActive` 为事实源；不能用全屏窗口的 key 状态代替。抑制
  系统通知还要求主工作区窗口本身聚焦；焦点在设置窗口时仍投递系统通知。
- 应用级 observer 在事件到达时立即把投递 batch 转交给渠道协调器并同步 Dock/菜单栏；
  窗口 render 不参与通知正确性，切换 Space 或窗口暂停绘制不会延迟通知。
- daemon 首次订阅快照只建立基线，不把历史终态当作新边沿：成功不生成关注项；审批、
  等待输入和失败静默恢复成已读行动项，保留角标但不进入系统投递队列。断线重连保留
  旧镜像并正常比较边沿，不能漏掉断线期间的变化。
- macOS 系统渠道使用 `UNUserNotificationCenter`。首次确有待投递项时才请求授权；授权状态、
  请求错误和 `addNotificationRequest` completion 都写回可观测桥状态，不能把“调用了 API”
  当作系统已接收。
- 通知设置页直接展示该桥的授权状态和待发送数量；拒绝态提供 macOS 系统设置入口与
  手动重新检查，提醒类型偏好开启不再伪装成系统渠道可用。
- 系统通知按 `smelt.session.<session-id>` 使用稳定 identifier。权限尚未决定、被拒绝或提交
  失败时，桥按 identifier 合并并保留最新待投递项；提交错误会暂停自动重试，应用重新激活
  或新事件到达后重读系统设置，再尝试一次，不能形成失败自旋。
- 会话已读、已解决、关闭，或新事件改走 suppress 时，同时撤回该 identifier 对应的
  pending 与 delivered 系统通知。这样 AttentionStore 的投递 batch 即使已经取走，异步系统
  拒绝也不会静默丢失，Notification Center 里也不会长期保留已处理提醒。
- ACP `TurnEnded` 在 smeltd 中直接投影为 `Succeeded`；状态栏、角标和通知读取同一份
  daemon 镜像，不能再以 ACP 视图快照作为第二个跨窗口事实源。取消、限额、拒绝等结果
  保存在独立 `turn_outcome`，不会把所有 Idle 都猜成成功。
- daemon 的每次状态提交带进程内全局单调 revision；唯一广播出口和每条订阅连接都拒绝
  旧/重复 revision。状态锁负责互斥，revision 负责锁外广播的可观察顺序。
- 结构化 hook 一旦接通，OSC/BEL 不再参与该会话的 agent phase 或关注通知；终端原始
  内容照常解析和显示，但不能反向覆盖 hook 事实。
- 铃铛展示未读；Dock 数字统计未读或仍需行动的会话（每个会话计 1）。菜单栏图标右侧
  使用紧凑的 `N@·M`：N 是待关注会话数，M 是运行中会话数；为 0 的项省略。
- 用户看过审批/输入只会标记已读；daemon 离开等待 phase 后才真正解决行动项。
- 用户关闭会话或 pane 时清理对应 store 条目和待投递事件。
- 系统通知点击携带稳定 daemon session id。主窗口已关闭或仍在恢复时先挂起跳转，目标
  会话一恢复就消费；不能只重开窗口，也不能使用已经失效的侧栏下标。
- 通知设置只控制系统通知渠道，不会抹掉状态事实或未读记录。

### 自动化 Run 结果

智能体自动化当前只允许 Pi。它虽然复用历史命名为 `AcpSession` / `acp_host` 的结构化
会话宿主，但实际 provider 进程走 Pi 官方 `--mode rpc` JSONL 协议，且完全由 smeltd
驱动，不创建 `AcpView`。这里的 session 是可取消、可恢复、可收集流式结果的无头执行容器；
产品生命周期仍以 `AgentRun` 为唯一事实源。Shell 自动化不创建该容器。

自动化结果通知直接读取 `automations.changed` 中的 Run 终态边沿：`Completed` 走成功通知，
`Failed` / `Skipped` 走失败通知，用户主动 `Cancelled` 不提醒。首次连接的全量快照只建立
基线，不补发历史结果；同一 `store_id` 断线重连时继续和内存中的旧投影比较，不能漏掉断线
期间完成的 Run。Agent 自动化的隐藏 runtime 进入 `Succeeded` / `Failed` / `Dead` 时不再
生产普通 session 完成通知，避免同一结果双投递。

macOS request 使用独立的 `smelt.automation.run.<run-id>` identifier，并以 automation id
作为 thread identifier。点击事件同时携带 `automation_id + run_id`，直接打开自动化 Run
详情；runtime session 随后被回收不会破坏通知目标。session 已读/删除与自动化历史裁剪分别
只清理自己的 identifier namespace。

关闭主窗口不会退出菜单栏中的 Smelt，系统通知仍可正常投递。用户用 Cmd-Q 显式结束整个
Smelt 进程后，smeltd 虽会继续执行自动化，但应用的 UserNotifications bridge 已不存在；
当前不会在下次冷启动时补发这段历史。若产品要求显式退出 App 后也立即通知，需要独立的
签名通知 helper 或 daemon outbox，不能让无 app bundle 身份的 managed smeltd 直接冒充 GUI。

### 应用内浮层

插件 `WKWebView` 为隔离快捷键、IME 和 first responder 使用独立的 AppKit child window，
因此主 GPUI 内容树里的元素无法盖住插件。确认框、命令面板和图片预览都留在主窗口 GPUI
树中；出现这些浮层、菜单或拖拽预览时，宿主暂时隐藏插件 WebView，并把 first responder
交回 GPUI。不为浮层再开透明 NSWindow。

会话关注、自动化提醒和操作结果一律走 macOS 系统通知。仅为消除
WebView child window 而 fork GPUI 不属于当前方案。

`AgentStatus` 的 session/pane 映射统一由 `AgentStatus::from_daemon_phase` 处理，且只接受
带回合级结构化来源的 daemon phase。`Succeeded` 仍是 `DaemonPhase`，不映射成 UI 态；
未读走 `AttentionItem`，列表里归空闲。

## 扩展约束

手机推送、深链和远程审批必须订阅同一 store 或同源的 `AttentionItem` 流，不得重新解释
daemon phase。新增事件类型时必须补齐状态转换、去重、已读/解决和投递渠道测试。

provider 安装器由同一注册表驱动状态探测、安装和卸载；新增官方事件接口时应增加注册项
及独立适配器，不在设置页或批量操作路径继续追加 provider 分支。每个适配器只能移除自己
写入的 handler 或带 Smelt 所有权标记的文件，不能覆盖其它用户 hooks / plugins。
