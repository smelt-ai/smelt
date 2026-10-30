# 状态与通知架构

> 定位:当前实现(领域模型、出口规则、扩展约束)。设计动机与状态语义推导见 [`archive/agent-state-notification-design.md`](archive/agent-state-notification-design.md)。

## 目标

状态描述 agent 当前事实，关注事件描述一次需要告知用户的变化，投递渠道只决定在哪里
展示。三者不得互相代替：

```text
各 provider hook / ACP → AgentEvent v1 生命周期 / v2 会话名
OSC 7501 程序状态根记录（smeltd 扫）──┐
                                      ↓
                               smeltd reducer → DaemonPhase + 会话名 → AgentStatus + AttentionItem
OSC 9/99/777（smeltd 扫）/ BEL（TerminalView）──→ Informational AttentionItem
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
和排队 follow-up 尚未结束时不会提前报完成。Pi 的原生 RPC 对话驱动和其它以 ACP 方式启动的
Agent 直接提供结构化状态，不重复注入终端桥。smeltd reducer 统一维护 phase，审批和输入
等待保持粘性，只有对应工具完成、新 prompt 或明确回合终态才能解除，避免并行子任务消息覆盖。
helper 每个事件只发送一次向后兼容的 `state + event` 信封：新 daemon 读取完整 event，
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
尚未给出语义标题时充当本地兜底，不混入 `pending_question`。显式改名走 ACP `session_title`
或 `SessionTitleChanged`，不走 OSC。

Claude 不发改名事件，helper 在 SessionStart / UserPromptSubmit / Stop 的生命周期事件之后
补发一条 `SessionTitleChanged`：`/rename`、`--name` 起的名优先（hook 的 `session_title`，
Stop 不带就读转录里最新的 `custom-title`），其次是 Claude 生成的标题（转录里最新的
`ai-title`，hook 里没有）。转录只读末尾一段；这两种记录不在公开文档里，认不出就不发，
会话名留在首条 prompt。生命周期事件先发，hook 超时只会丢标题。

Claude 的子智能体默认在后台跑，派出去后主回合马上 Stop。Stop 带的 `background_tasks` 是
此刻在跑的任务快照：里面还有 `subagent` / `workflow` 时，这次 Stop 翻成带「后台还有 N 个
子任务在跑」的工具完成，不算回合完成；子智能体跑完会把主回合叫醒，快照为空的那次 Stop 才
是完成。只看快照，不配对 SubagentStart/Stop（TaskStop 停掉的不发 SubagentStop）。后台
shell、monitor、teammate 会一直挂着，不计入。用户在任务面板手动停掉子智能体时 Claude 什么
事件都不发，要等下一次 Stop 才对上。会报 OSC 7501 的 Claude 版本以 7501 为准（见下），
这一条只给不报的版本兜底。
provider 工具名只在适配层用于把原始 hook 判定为这些稳定语义之一，不进入 UI 的状态判断
逻辑。

结构化 hook/OSC 7501/ACP 通过 `apply_daemon_transition` 写入 store。终端 OSC 标题不进入
会话名；会话尚未收到有效结构化事件时，OSC 9/99/777 和 BEL 可以翻译成信息性
`AttentionItem`，但绝不能改变 phase 或伪造完成。结构化事件一旦接通，低可信终端通知永久
退出该会话的 agent 通知链路，避免重复投递。

OSC 9/99/777 由 smeltd 在 PTY 输出泵里扫（alacritty 不解析），会话没有结构化事件时发
`terminal.notification`，桌面收到后进同一个 store。只扫实时输出：reattach / resume 的
keyframe 不经过这里，历史通知不会被当成新的再弹。这样通知不依赖终端 pane 是否打开、
终端连接是否 attach、慢客户端是否被流控合并跳过了这段输出。没有任何桌面订阅（应用
没开）时发生的通知直接丢弃，不补发。BEL 仍由 `TerminalView` 处理（有宽限期，受设置
开关控制）。

OSC 7501（[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)）
在同一处扫。smeltd 回应 `OSC 7501 ; ?` 探测（在把这批输出交给客户端之前写回 PTY，保证
先于客户端回应的 DA），Claude Code、Pi、Grok 据此开始上报；Codex 不探测、直接发。只认根
记录，它直接改 phase：`working` → 思考中，`blocked` → 等批准（`kind=permission`）或等输入，
`done` → 已完成，`error` → 失败，`idle` → 空闲。程序把后台子智能体、自动重试都算进了自己
的状态：实测 Claude Code 派出后台子智能体后主回合马上 Stop，根记录却一直 `working`，等
子智能体结束、主回合汇总完才 `done`。

会话报过一次 7501 根记录，相位就以它为准（`SessionState::program_status`），同一会话不让
hook 和 7501 两边各改一份：hook 的回合开始、等待、完成都不再改相位，只在 7501 报 `working`
时把它细分成思考 / 执行工具（7501 不带工具名），以及在进程退出（`SessionEnded`）时置
`Dead`。会话名、对话 id 照旧来自 hook，7501 不带对话 id。根记录被 `clear`（程序退出前会
撤掉全部记录）或进程退出时，相位交还给 hook，`working` / `blocked` 作废。规范还要求新的
shell 提示符（OSC 133 `A`）出现时作废，smelt 目前没有 shell 集成，靠 SessionEnd 兜底。

7501 是 PTY 里任何程序都能写的低可信来源，规范要求不能据此授权。它只决定显示的状态；
`blocked` 会打开远程审批快捷操作（`action` 只在等待相位下写入），这个门闩只防在 agent
没在等的时候误发按键，不是权限边界：已配对设备本来就能经原始输入写任何内容，能伪造 7501
的程序本来就控制着这块屏幕。它的状态边沿由客户端按相位变化生成关注事件，不再另发
`terminal.notification`。

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
- 结构化 hook 或 OSC 7501 一旦接通，OSC 9/99/777 和 BEL 不再参与该会话的关注通知；终端
  原始内容照常解析和显示。OSC 7501 根记录是唯一能改 phase 的终端序列，接通后由它而不是
  hook 决定该会话的 phase（见上）。
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

### 手机推送

iOS 把 App 挂起后 iroh 连接就断了，等批准、等输入、运行失败经 APNs 推到手机。推送由各台
smeltd 自己产生，不依赖手机连着，也不依赖桌面 GUI 在线；APNs 的 `.p8` 私钥在推送中继
（`smelt-push-relay`）上，部署见 [`push-relay-deployment.md`](push-relay-deployment.md)。

- **同源判定**：smeltd 在进程内订阅自己的 `session.state_changed` / `session.removed`，用
  客户端同一个 `DaemonStateEventClient` 解码，再用同一套 `apply_daemon_baseline` /
  `apply_daemon_transition` 算边沿（`crates/smeltd/src/push.rs`）。首份快照只建基线；落后时
  按快照重新对齐，之后照常比较边沿。只推需要用户处理的三种：审批、输入、失败；成功不推。
- **登记**：手机先找中继用 APNs 令牌换**句柄**（中继用自己的密钥加密过的令牌，中继不存
  设备表），再经 iroh 调 `push.register` 把句柄、中继地址和要推的类型登记到每台机器。
  smeltd 按连接的设备身份写 `push_registration` 表（外键挂在 `paired_device` 上，撤销设备
  时跟着删），不看参数里自称是谁。手机每次启动、每次连上一台机器、改了设置时重新登记；
  关掉推送、忘记一台连着的机器时调 `push.unregister`。旧运行时没有这两个方法，手机在机器
  详情里提示升级。
- **outbox**：提醒先在内存里等 30 秒，期间这件事已经处理掉（行动项解决、会话删除）就取消，
  同一会话的新提醒替换旧的。到点时按当时的登记展开到各台手机；`410` 删掉那台手机的登记，
  `429` / `5xx` / 网络错误按 5 秒起翻倍、封顶 2 分钟退避重试，从提醒产生起超过 10 分钟丢弃，
  已处理的停止重试。重试用同一个 `apns-id`。outbox 不落盘，smeltd 重启后不补发。发送线程
  只在有提醒到点时醒来，没有待发的提醒时不醒。
- **载荷**：只有提醒类型（APNs `loc-key`，文案在 App 的 `Localizable.strings`）和不透明的
  `runtime_id`、`session_id`，不含标题、正文、命令、路径、机器名。`apns-collapse-id` 是
  句柄和会话 id 的哈希：同一会话的新推送顶掉旧的，外面推不出会话 id。`thread-id` 按机器
  分组。格式唯一定义在 `crates/smelt-push`。
- **手机**：前台时不弹系统横幅，沿用应用内提示。点通知时记下目标，会话目录里出现那段
  会话再打开（冷启动时目录还在路上）。行动项解决时、回到前台时，清掉通知中心里已经处理
  掉的会话的推送（APNs 没法远程撤回）。
- 通知栏里直接批准 / 回复、端到端加密带上具体内容（Notification Service Extension）是后续
  工作，见 issue #18 的第二阶段。

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

手机推送（见上文「手机推送」）、深链和远程审批必须订阅同一 store 或同源的
`AttentionItem` 流，不得重新解释 daemon phase。新增事件类型时必须补齐状态转换、去重、已读/解决和投递渠道测试。

provider 安装器由同一注册表驱动状态探测、安装和卸载；新增官方事件接口时应增加注册项
及独立适配器，不在设置页或批量操作路径继续追加 provider 分支。每个适配器只能移除自己
写入的 handler 或带 Smelt 所有权标记的文件，不能覆盖其它用户 hooks / plugins。
