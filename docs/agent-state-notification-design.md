# Agent 状态与通知统一设计

> 定位：设计稿与动机(目标、状态语义、参考实现)。当前实现以 [`notification-architecture.md`](notification-architecture.md) 为准——两篇不是重复,是「为什么这么设计」与「现在怎么实现」的分工。

## 目标

为 Claude Code、Codex、GitHub Copilot CLI、Grok、Antigravity、Pi，以及 ACP 会话建立同一套
状态与通知模型。
重点解决以下问题：

- 普通完成消息、OSC 通知或终端响铃被误判为审批。
- “需要处理”同时表示等待回复、等待审批、普通通知和完成，语义不稳定。
- 状态、通知、未读和进程存活混在同一个枚举中。
- 同一事件由 hook、ACP、OSC、BEL 等多个来源重复上报，产生重复通知。
- 旧 turn 的消息与新 turn 的状态拼接，出现“等你批准 + 已完成”之类的组合。

本设计只统一语义与边界，不要求所有 agent 第一天就提供同等精度。缺少结构化信号时必须保守
降级，不能用自由文本猜出高风险状态。

---

## 一、状态模型

### 1.1 六个核心状态

```rust
enum AgentPhase {
    Idle,
    Running,
    WaitingForUser,
    WaitingForApproval,
    Succeeded,
    Failed,
}
```

| 状态 | 含义 | 是否需要用户操作 |
|---|---|---:|
| `Idle` | 会话已就绪，当前没有运行中的 turn | 否 |
| `Running` | agent 正在思考或执行工具 | 否 |
| `WaitingForUser` | agent 在等回答、选择或补充信息 | 是 |
| `WaitingForApproval` | agent 在等用户允许或拒绝某项操作 | 是 |
| `Succeeded` | 当前 turn 正常完成 | 否 |
| `Failed` | 当前 turn 失败或异常中止 | 否 |

`WaitingForUser` 与 `WaitingForApproval` 必须分开：前者回答问题，后者作安全决策。

> 注：落地为 9 态 `DaemonPhase`——connecting / thinking / executing_tool / awaiting_approval /
> waiting_for_user / succeeded / failed / idle / dead（`daemon_state.rs`）。本设计的六态基础上
> 拆出了 `connecting`，并把 `Running` 细化为 `thinking`（思考）与 `executing_tool`（执行工具）。

### 1.2 子状态与附加字段

以下信息不增加顶层状态：

```rust
enum RunningActivity {
    Thinking,
    ExecutingTool,
}

enum CompletionOutcome {
    Success,
    Failure,
    Cancelled,
}

struct AgentState {
    phase: AgentPhase,
    running_activity: Option<RunningActivity>,
    completion_outcome: Option<CompletionOutcome>,
    session_alive: bool,
    turn_id: Option<String>,
    phase_since: u64,
}
```

- `Thinking` / `ExecutingTool` 只是 `Running` 的展示细节。
- `Cancelled` 是完成结果，不需要新增第七个顶层状态。
- `session_alive` 表示进程或连接是否存活。旧的 `Dead` 不再与 agent 工作状态混用。
- `turn_id` 标识一轮用户请求；无法从 agent 获取时由 smelt 在提交 prompt 时生成。

### 1.3 不属于状态的内容

- “有结果可看”是 `Succeeded + unread`，不是独立状态。
- 收到 BEL、OSC 9/99/777 或系统通知不代表 phase 发生变化。
- 用户查看通知只改变未读状态，不改变 agent phase。
- 切到等待中的会话不代表已经回答或审批，等待状态必须由后续结构化事件解除。

### 1.4 基本状态转换

```text
Idle / Succeeded / Failed
        |
        | user prompt
        v
     Running <-----------------------------+
        |                                  |
        +---- question ----> WaitingForUser |
        |                         | answer  |
        +---- permission --> WaitingForApproval
        |                         | decision|
        +---- success ----> Succeeded       |
        +---- error ------> Failed          |
        +---- cancel -----> Failed(Cancelled)
```

新的用户 prompt 会开始新 turn，并清理上一个 turn 的完成上下文和去重键。

---

## 二、通知模型

### 2.1 五类通知

```rust
enum AgentNotificationKind {
    ApprovalRequired,
    UserInputRequired,
    TurnSucceeded,
    TurnFailed,
    Informational,
}
```

| 类型 | 产生条件 | 生命周期 |
|---|---|---|
| `ApprovalRequired` | 进入 `WaitingForApproval` | 状态解除前保持可操作 |
| `UserInputRequired` | 进入 `WaitingForUser` | 用户回复前保持可操作 |
| `TurnSucceeded` | 进入 `Succeeded` | 查看后清未读 |
| `TurnFailed` | 进入 `Failed` | 查看后清未读，保留错误结果 |
| `Informational` | 普通 OSC、BEL、后台 shell 完成等 | 短暂展示，不改变状态 |

> 注：落地命名统一为 **`AttentionKind`**（Approval / Input / Success / Failure / Bell / Notice），
> `AgentNotificationKind` 只是本设计的过渡叫法。

通知事件使用独立数据结构：

```rust
struct AgentNotification {
    id: String,
    session_id: String,
    pane_id: Option<String>,
    turn_id: Option<String>,
    kind: AgentNotificationKind,
    title: String,
    body: String,
    source: NotificationSource,
    created_at: u64,
    dedupe_key: String,
    read: bool,
}

enum NotificationSource {
    Acp,
    AgentHook,
    TerminalOsc,
    TerminalBell,
    ProcessLifecycle,
}
```

### 2.2 状态与通知的关系

状态转换是结构化通知的主要生产者：

| 状态转换 | 通知 |
|---|---|
| 非等待态 -> `WaitingForApproval` | `ApprovalRequired` |
| 非等待态 -> `WaitingForUser` | `UserInputRequired` |
| 非完成态 -> `Succeeded` | `TurnSucceeded` |
| 非完成态 -> `Failed` | `TurnFailed` |

重复上报同一个 phase 不产生新通知。离开等待态后再次进入，可以产生新通知。

BEL 与普通 OSC 的固定规则：

```text
BEL / 普通 OSC -> Informational -> 不改变 AgentPhase
```

自由文本即使包含 `approval`、`permission`、`权限` 或 `批准`，也不能升级为
`WaitingForApproval`。完成摘要经常会提到这些词，按关键词分类会产生误报。

### 2.3 展示策略

| 场景 | 行为 |
|---|---|
| smelt 在后台 | 发送系统通知 |
| smelt 在前台，事件来自其他 pane | 发送系统通知 |
| 用户正在看对应 pane | 不弹系统通知，只更新内联状态 |
| 等待审批或回复 | 保持行动状态，直到 agent 确认已继续 |
| 完成或失败 | 标记结果未读；打开对应 pane 后清未读 |
| 普通信息 | 系统通知；不进入阻塞卡，未读时计入统一数字角标 |

点击通知必须精确跳到 `session_id + pane_id`。审批正文也必须来自产生审批事实的同一个
pane，不能从会话中任取一条旧通知。

### 2.4 未读与行动状态

未读和行动状态是两个维度：

```rust
struct SessionAttention {
    unread_result: bool,
    unread_notifications: usize,
}
```

- 查看 `Succeeded` / `Failed` 结果可以清 `unread_result`。
- 查看 `WaitingForUser` / `WaitingForApproval` 只清通知未读；状态仍然需要用户操作。
- Dock 数字统计未读或仍需行动的会话；同一会话连续多条事件只计一次。菜单栏图标后
  使用 `N@·M`，`@` 前是同一待关注计数，后面是运行中会话数；为 0 的项省略。
- 通知中心可以同时展示行动通知和最近的信息通知，但必须使用不同视觉等级。

### 2.5 去重

优先使用以下去重键：

```text
session_id + turn_id + notification_kind + request_id
```

没有 `request_id` 时退化为 phase transition 序号，不能依赖正文。来源不同但指向同一状态转换
时只保留一条，例如 ACP snapshot 与 daemon state 同时报告同一个审批请求。

---

## 三、信号来源与可信度

| 来源 | 能否改变 phase | 用途 |
|---|---:|---|
| ACP permission / elicitation / completion | 是 | 协议事实，最高可信度 |
| agent 官方 hook | 是 | 结构化生命周期事实 |
| 进程退出 | 可进入完成/失败 | 必须结合退出码和当前 turn |
| OSC 标题（含 spinner） | 否 | 仅作任务标题等展示元数据 |
| OSC 9/99/777 | 否 | 仅产生 `Informational` 通知 |
| BEL | 否 | `Informational` 通知 |

只有结构化生命周期事件能改变 phase。终端标题、OSC 与 BEL 不能覆盖或推断等待、运行、
完成或失败状态。

---

## 四、Agent 事件映射

### 4.1 Claude Code

| Hook | 统一状态 |
|---|---|
| `UserPromptSubmit` | `Running` |
| `PreToolUse` | `Running(ExecutingTool)` |
| `PostToolUse` / `PostToolUseFailure` | `Running(Thinking)` |
| `PermissionRequest` / `permission_prompt` | `WaitingForApproval` |
| AskUser / `elicitation_dialog` | `WaitingForUser` |
| `idle_prompt` | `Succeeded`（本轮完成，等待下一条 prompt） |
| `Stop` | `Succeeded` |
| `StopFailure` | `Failed` |
| `SessionEnd` | `session_alive = false` |

### 4.2 GitHub Copilot CLI

Copilot 应使用 `~/.copilot/hooks/*.json` 的结构化 hooks，不以 `beep` 为主通道。

| Hook | 统一状态 |
|---|---|
| `userPromptSubmitted` | `Running` |
| `preToolUse` | `Running(ExecutingTool)` |
| `postToolUse` / `postToolUseFailure` | `Running(Thinking)` |
| `notification: permission_prompt` | `WaitingForApproval` |
| `notification: elicitation_dialog` | `WaitingForUser` |
| `agentStop` | `Succeeded` |
| `errorOccurred` | `Failed`，可恢复错误除外 |
| `sessionEnd` | `session_alive = false` |

`permissionRequest` 在 Copilot 权限规则引擎和自动允许判断之前发生，不能单独证明用户已经看到
审批框。只有真正的 `permission_prompt`，或明确的 AskUser 工具，才进入等待态。

`beep` 与 `beepOnSchedule` 只作为未安装 hooks 时的兼容信号，映射为 `Informational`。

### 4.3 Codex

- ACP/app-server 模式直接映射 permission、user input、turn completion 和 error 事件。
- 普通终端模式优先使用官方结构化事件；没有时，OSC 只产生 `Informational`。
- 不再把 Codex OSC 9 自由文本一律当作成功或审批事实。

### 4.4 Grok

- ACP 模式使用 ACP phase 和 request。
- 普通终端模式优先使用 Grok hook 的事件类型；自动批准模式下的例行
  `permission_prompt` 不应产生用户行动通知。快捷终端出厂命令为
  `grok --always-approve`（全权限，与其它家对齐）。
- AskUser 类工具映射为 `WaitingForUser`。
- 没有结构化事件时，OSC/BEL 只产生 `Informational`。

### 4.5 Antigravity

Antigravity 只接入 PTY/TUI，不进入 ACP。smelt 的快捷终端出厂启动命令为
`agy --dangerously-skip-permissions`（全权限，与 Claude/Codex/Copilot/Grok 对齐；
`agy` 官方支持该参数，需要保守的用户可自行去掉）。smelt 在
`~/.gemini/config/hooks.json` 中安装以下 hooks：

| Hook | 统一状态 |
|---|---|
| `PreInvocation` | `Running(Thinking)` |
| `PostInvocation` | `Running(Thinking)` |
| `PostToolUse` | `Running(Thinking)`，并记录已完成的步骤、工具和错误摘要 |
| `Stop` 且 `fullyIdle = true` | `Succeeded`；停止原因为错误、取消或步数上限时为 `Failed` |
| `Stop` 且 `fullyIdle = false` | `Running(Thinking)`，表示仍有后台任务 |

不安装 `PreToolUse`：Antigravity 要求该 hook 返回权限决策，安装它会改变用户原有的工具授权
策略。出厂全权限启动后 Antigravity 不再弹自己的审批框，因此该接入提供生命周期和已完成步骤进度，
但不伪造 `WaitingForApproval` 或 `WaitingForUser`。

### 4.6 Pi

- 对话模式由 Smelt 直接驱动 Pi 官方 JSONL RPC，提供 phase、工具生命周期、
  权限审批、模型/推理强度切换与会话恢复；进程由 Smelt 受管 Bun 启动，不走 ACP。
- 普通终端模式由 smeltd 在启动命令中进程级注入受管 `--extension`；不写入用户
  `~/.pi/agent/extensions` 或项目 `.pi` 配置。
- `agent_start` 映射 `Running(Thinking)`，`tool_execution_start/end` 映射工具执行与继续思考，
  `agent_settled` 映射最终成功或失败。完成边沿必须用 `agent_settled`，不能用每次低层
  `agent_end`，否则自动重试、压缩重试和排队 follow-up 会被提前报告为完成。
- 对话模式的写入、执行和 Extension Tool 默认通过 Pi Extension UI 走 Smelt 权限弹窗；只有任务显式声明
  FullAccess 时才切成 `bypassPermissions`。终端扩展不从输出文本猜测审批状态。

> 注：代码注册表另有 Cursor / OpenCode / Kiro / Pi / DSH（`agent_kind.rs` 的
> `ConversationAgentKind`）；Antigravity 单独走 `TerminalAgentKind`。

---

## 五、设置

建议提供以下用户设置：

- 系统通知总开关。
- 等你批准通知。
- 等你回复通知。
- 任务完成通知。
- 任务失败通知。
- 普通终端通知。
- 通知声音。
- 前台未查看的会话也发送系统通知。

Agent 集成设置单独展示：

- Claude hooks：安装 / 状态 / 还原。
- Copilot hooks：安装 / 状态 / 还原。
- Codex 结构化集成状态。
- Grok 结构化集成状态。
- Antigravity hooks：安装 / 状态 / 还原。
- Pi 状态桥：快捷终端自动按进程注入，无需全局安装或设置项。

“Copilot 响铃通知”已删除，smelt 不再修改 Copilot 的全局 `beep` 配置。Copilot 改由
`~/.copilot/hooks/smelt.json` 的结构化 hooks 上报审批、提问、完成和失败。

终端解析器仍接收 BEL，但当前不把它写入通知槽，也不产生 Agent 状态；OSC 9/99/777 保留为
`Informational` 通知，并且不参与左侧状态派生。

---

## 六、现有实现迁移

当前 `AgentStatus`：

```text
WaitingApproval / NeedsAttention / Running / Done / Idle
```

> 注：现已收敛为三态 **NeedsYou / Running / Idle**（`agent_status.rs`），`NeedsAttention`
> 已删除——未读与行动状态分离为独立的 attention 维度，不再占独立状态。

迁移原则：

| 旧值 | 新模型 |
|---|---|
| `WaitingApproval` | `WaitingForApproval` |
| `NeedsAttention` | 删除；按事实拆为 `WaitingForUser` 或 `Informational` |
| `Running` | `Running`，细分 activity |
| `Done` | `Succeeded + unread_result` |
| `Idle` | `Idle` |
| daemon `Dead` | `session_alive = false` |

建议分阶段实施：

1. 引入新数据结构和纯映射函数，保留旧 UI 派生层。
2. 让 ACP 与 daemon hook 使用新状态，建立 `turn_id` 和 transition 去重。
3. 安装并接入 Copilot 与 Codex hooks，验证审批、提问、完成和失败事件。
4. 删除“Copilot 响铃通知”设置以及对 `~/.copilot/settings.json` 的写入。（已完成）
5. 将 OSC/BEL 从状态输入改为 `Informational` 通知输入。
6. 删除 `NeedsAttention` 与正文关键词审批分类。
7. 统一 Dock、菜单栏、侧栏、总览和系统通知的数据源。
8. 迁移其余设置并保留旧配置兼容读取。

---

## 七、验收标准

- 普通完成正文即使包含 `approval` 或 `permission`，也不会显示审批卡。
- BEL、OSC 9/99/777 不会改变 agent phase。
- 只有结构化审批事件能进入 `WaitingForApproval`。
- `WaitingForUser` 与 `WaitingForApproval` 使用不同文案、按钮和通知类型。
- 同一 phase transition 即使由多个来源上报，也只通知一次。
- Claude、Codex、Copilot、Grok、Pi 和 ACP 的完成、失败、等待输入、等待审批映射一致；
  Antigravity 快捷终端以全权限启动，保证 `Running`、`Succeeded` 和 `Failed`，无审批/提问
  事件可上报。
- 当前 pane 不弹系统通知；其他 pane 和应用后台一律显示系统通知。
- 点击通知精确定位到产生事件的 pane。
- 查看结果会清未读，但查看等待中的会话不会解除等待状态。
- 进程死亡不伪装成成功、失败或等待状态。

---

## 八、参考实现

- [Orca](https://github.com/stablyai/orca)：使用 `working / blocked / waiting / done`，并为
  Copilot 安装完整生命周期 hooks。其状态名称较少，但结构化事件归约和旧 turn 防串线值得参考。
- [Warp](https://github.com/warpdotdev/warp)：内部区分 `InProgress / Blocked / Success /
  Failed`，并将“任务完成”与“需要注意”作为不同通知触发器；普通 OSC 通知保持为可插拔通知。
- [GitHub Copilot hooks reference](https://docs.github.com/en/copilot/reference/hooks-reference)：
  定义 `permission_prompt`、`elicitation_dialog`、`agentStop` 等结构化事件。
- [Antigravity hooks reference](https://www.antigravity.google/docs/hooks)：定义
  `PreInvocation`、`PostInvocation`、`PostToolUse`、`Stop` 以及 hook 输出约束。
- ACP：permission、elicitation、running 与 ended phase 是 smelt 内部最可靠的跨 agent 数据源。
