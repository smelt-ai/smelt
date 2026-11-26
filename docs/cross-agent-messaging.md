# 跨引擎智能体互发消息（全 ACP 双向对等协作）

状态：**已实现（Phase 1–4 均已落地）**。在 [`pi-session-messaging-design.html`](pi-session-messaging-design.html)（仅 Pi ↔ Pi）与 [`cross-machine.md`](cross-machine.md)（跨节点会话消息）的基础上，将对话间双向协作扩展到所有 Smelt 托管的 ACP 智能体：Pi、产品智能体（`agent:<id>`）、Codex、Claude Code、Antigravity（`agy`）、OpenCode、Copilot、Cursor、Grok、Kiro、Oh My Pi 及用户自定义 ACP Profile（`profile:<id>`）。

---

## 1 · 背景与外部协议调研

### 为什么需要宿主层总线

Smelt 现有的 `session_list` / `session_read` / `session_send` / `session_spawn` 仅在 Pi ↔ Pi 之间可用（`smeltd/src/peer_messages.rs` 过滤了非 Pi 会话，`acp_host.rs` 只为 Pi 绑定 `peer_handler`，`acp_conn.rs` 忽略 `DeliverPeerMessage`）。

调研各家官方与协议现状后，结论很明确：

| 引擎 / 协议 | 官方多智能体 / 会话消息能力 | ACP `mcpServers` 注入支持 | 说明 |
|---|---|---|---|
| **Pi** | 无原生跨会话协议；Smelt 借 Pi 扩展原语（`registerTool` + `sendMessage`）搭建 | 不走 ACP（走内置 RPC + `SMELT_BG_SOCK`） | 保留现有原生路径，零回归 |
| **Claude Code** | Agent Teams + 跨会话 `SendMessage`（仅限 Claude ↔ Claude） | ✅ 已实测代码（`claude-agent-acp` 桥接 `params.mcpServers` 进 SDK） | 官方能力封闭在 Claude 生态内，无法跨引擎 |
| **Codex** | Multi-agent 工具（`spawn_agent` / `send_input` / `wait_agent`，仅限 Codex 内部父→子） | ✅ 已实测代码（`codex-acp` 将 `request.mcpServers` 写入 `mcp_servers` 配置） | 官方能力只限 Codex 自身派生 |
| **Antigravity (`agy`)** | 内部子智能体 / swarm（不跨外部引擎） | ✅ 已实测 `agy acp` 1.5.14 会拉起 `session/new` 传入的 stdio MCP Server | **关键实测细节**：`agy` 握手时先发 `server/discover`（MCP 2026-07-28 探测），收到标准 JSON-RPC `-32601 Method not found` 后立刻回退 `initialize` → `tools/list`；若静默忽略会触发超时取消 |
| **OpenCode** | 原生仅父→子一次性 Task；跨会话靠社区插件 | ✅ 支持 `mcpServers` | 单进程多会话下存在同名 MCP 全局覆写问题，但 Smelt 为每段对话起独立 ACP 子进程，天然隔离 |
| **ACP 协议标准** | 定位为 Client ↔ Agent（类似 LSP），无跨会话 peer 通信原语 | 标准字段 `NewSessionRequest.mcp_servers` / `LoadSessionRequest.mcp_servers` | 跨引擎互通没有官方协议，必须由宿主 `smeltd` 做统一总线 |
| **通用自定义 ACP** | 未知 | 可能支持，也可能忽略或拒收 | 需要 CLI 兜底 + `Task` 结束自动回传兜底 |

### 核心判断

- **同引擎内部协作**（Claude 自己开子团队、Codex 自己开子 worker）继续走各家原生机制，Smelt 保持不干涉。
- **跨引擎 & 自定义智能体双向互聊**由 `smeltd` 的 `peer_messages` 作为唯一总线，前端/接收/发起按引擎分层适配，**不改 SQLite 表结构，不引入第二份状态源，不往桌面宿主写死功能**。

---

## 2 · 架构总览

现有 `smeltd/src/peer_messages.rs` 的门卫、`Outbox` 发件队列、投递线程 `Courier`、跳数预算 `CHAINS`、防递归 `SPAWNED`、退信 `bounce`、跨机器 `node.peer_*` 全部保留复用，拆成「总线 + 两端适配」：

```text
发起侧（工具入口）                         总线（smeltd）                        接收侧（消息投递）
────────────────────                     ─────────────────                     ────────────────────
Pi 扩展 session-peers.ts ──┐                                              ┌──▶ Pi：SMELT_BG_SOCK
                           │   统一 PeerRequestHandler                    │    smelt.peer_message
第三方 / 自定义 ACP：      ├──▶ 门卫 / Outbox / Courier ───按目标引擎───  ┤
  1) 优先：ACP mcpServers  │    权限姿态 / 跳数 / 积压 / 退信 / 跨机器     └──▶ 非 Pi ACP：
     (smeltd --peer-mcp)   │                                                   pending_prompts 排队
  2) 兜底：smelt peer CLI ─┘                                                   → session/prompt
                                                                               → Task 结束自动兜底回传
```

### 设计原则

1. **零数据库表变更**：对话权威仍在 `workspace_directory`（已有 `agent`、`profile_id`、`agent_definition_id`、`launch`），发件队列与联系簿仍在 `smeltd` 内存。
2. **来源永远由宿主认定**：无论 Pi 扩展、MCP Server 还是 CLI，来源 `session_id` 一律由连接或进程树归属决定，不信请求体里的来源字段。
3. **不静默提权**：统一三态权限姿态，低权限对话不能借高权限或权限未知的对话绕过审批。
4. **全事件驱动，无轮询**：投递、等待回合结束、自动兜底回传全部基于现有 `Watched` 和回合结束事件。

---

## 3 · 发起侧：MCP 优先 + CLI 进程树兜底

### 3.1 会话级 `PeerSocket`

- 在 `crates/smeltd/src/acp_host.rs` 中，去掉 `is_smelt_pi_launch(&launch)` 限制，为**所有非自动化 ACP 会话**调用 `peer_messages::bind_peer_handler`。
- 从 `smelt-core/src/background_tasks` 抽离或复用只服务 peer 请求的轻量会话套接字 `PeerSocket`（Unix domain socket / Windows named pipe，0700 目录），生命周期与当次 `AcpSession` 的 runtime 连接绑定；换代时旧 slot 的 `is_current` 失效，自动拒绝旧连接请求。
- `PeerSocket` 额外记录一个原子标志 `mcp_connected: AtomicBool`（收到过 MCP 客户端握手即置为 `true`），供接收侧自适应选择回信引导语，不需要硬编码哪家引擎支持 MCP。

### 3.2 主通道：标准 ACP `mcpServers` 注入（`smeltd --peer-mcp`）

- 在 `smeltd` 二进制里提供一个极轻量的隐藏子命令 `smeltd --peer-mcp`：
  - 进程启动时从环境变量读取 `SMELT_PEER_SOCK`，只做 **MCP stdio JSON-RPC 2.0 ↔ `PeerSocket` 转发**，不读库、不起守护、启动耗时毫秒级、零 Node/Bun 内存开销。
  - **协议兼容硬要求**（基于 `agy acp` 1.5.14 实测）：
    - 对任何不认识的请求方法（例如 `agy` 发出的 `server/discover`、`resources/list`、`prompts/list` 等）**必须立即返回标准 JSON-RPC 错误 `-32601 Method not found`**，绝不静默丢弃；
    - `initialize` 回应协商的 `protocolVersion` 与 `capabilities: { tools: {} }`，并在此时向 `PeerSocket` 发送一次轻量 `mcp_hello` 将该会话标记为 `mcp_connected = true`；
    - 对无 `id` 的通知（如 `notifications/initialized`、`notifications/cancelled`）不回包；
    - 工具调用 `tools/call` 把 MCP 这次调用生成的唯一 ID（或请求 `id` + 参数摘要）映射为幂等 `toolCallId` 交给 `peer_handler`。
  - 暴露四个工具 `session_list` / `session_read` / `session_send` / `session_spawn`。
- **工具定义和回给模型的文字只有一份**：`crates/smeltd/src/peer_tools.rs`。
  - `definitions()` 给名字、说明、用法提示（`promptGuidelines`）、参数 JSON Schema。Pi 扩展 `agent/src/session-peers.ts` 加载时经会话宿主发 `session_tools` 要这份定义原样注册（子智能体、没有会话宿主时要不到，不注册）；`--peer-mcp` 的 `tools/list` 用同一份，用法提示接在说明后面。
  - 应答函数答完由 `present()` 补上 `text`（回给模型的话）和 `details`（界面画事件行用，见 `peer_send_from_tool`）。Pi 扩展把两者原样交给 Pi；MCP 回 `text`，`session_send` / `session_spawn` 另附一段 `details` JSON 文本，不用 `structuredContent`（各家 ACP 转发不一）。CLI 的 JSON 回答里也带着。
- 在 `crates/smelt-core/src/acp_conn.rs` 发 `NewSessionRequest` 和 `LoadSessionRequest` 时，如果 `launch.peer_handler.is_some()`：
  - 附加 `McpServer::Stdio(McpServerStdio { name: "smelt-peers", command: <当前 smeltd 路径>, args: ["--peer-mcp"], env: [SMELT_PEER_SOCK=<socket>] })`。
  - **不**把 `SMELT_PEER_SOCK` 塞进 agent 主进程环境，仅传给这个 MCP 子进程。
- **拒收自动降级**：若某个第三方 adapter 在 `session/new` / `session/load` 收到非空 `mcp_servers` 时返回参数错误（如 `-32602` / invalid params），`acp_conn` 捕获后去掉 `mcp_servers` 重试一次，该会话自动退回 CLI 兜底路径。

### 3.3 兜底通道：`smelt peer` CLI + 进程树鉴权

针对忽略 `mcp_servers` 的自定义 ACP 或小众适配器，在 `crates/smelt-cli` 新增子命令：

```sh
smelt peer list [--query <关键词>] [--no-agents]
smelt peer read <session_id> [--limit <1..5>]
smelt peer send --to <session_id> --message <正文> [--kind message|task|completion] [--reply-to <pm-id>]
smelt peer spawn --title <标题> --task <任务> [--agent <agent_option_id>] [--cwd <目录>]
```

- 经 Control API（新增 `peer.session_list` / `peer.session_read` / `peer.session_send` / `peer.session_spawn`，只收本机 `Principal::Local`）直达 `peer_messages.rs` 同一套门卫和队列。
- **来源认定**：
  - 复用 `crates/smeltd/src/memory_peer_auth.rs` 的 peer credential（`peer_pid`）+ 跨平台 `parent_pid` 进程树向上追溯，匹配各活跃 `AcpSession` 的子进程 PID（`handle.stdio.pid`）；
  - 同时在 `crates/smelt-core/src/acp_terminal.rs` 中，为 ACP `CreateTerminalRequest` 启动的终端进程组登记 `pgid/pid -> session_id`（与 `background_tasks::agent_task_groups()` 同理），覆盖由宿主代跑终端命令的 ACP 引擎；
  - 找不到所属活跃 ACP 会话时拒绝调用（避免普通外部终端误把请求算到某个会话头上；若未来要支持外部脚本发消息，走已有的 `smelt conversation send`）。
- 幂等 `toolCallId`：CLI 每次调用自动生成随机 UUID（或接受可选 `--idempotency-key`）。

---

## 4 · 接收侧：非 Pi ACP 投递、排队、对账与投影

### 4.1 统一 `PeerInbox` 与排队

- Pi 会话保持现状：`background_tasks::peers` 经 `listen` 写 `smelt.peer_message`，`message_start` 后 ack。
- 非 Pi ACP 会话由 `acp_host` 接管投递，不再丢给 `acp_conn` 空转忽略：
  1. `AcpSession` 持有非 Pi 用的 `peer_inbox: SessionLock<Vec<PeerDeliveryRecord>>`（与 `background_tasks::peers::LivePeer` 同状态机：`Pending` → `Delivered`，保留最近 64 条已送达记录，每次变更更新 `reduced.peer_inbox` 并推送快照，复用前端待处理提示条和 `observe_inbox` 对账）。
  2. `AcpPromptOrigin`（`crates/smeltd/src/acp_host.rs`）扩展为：
     ```rust
     pub(crate) enum AcpPromptOrigin {
         User,
         Peer(smelt_core::acp_session::PeerDelivery),
     }
     ```
  3. `deliver_peer_message` 投给非 Pi 会话时：
     - 按 `delivery.id` 去重（发件队列重投不会排两条）；
     - 记入 `peer_inbox`（状态 `Pending`）；
     - 格式化为 `<peer-message>` 正文，作为一条 `QueuedAcpPrompt { origin: AcpPromptOrigin::Peer(delivery), delivery_id: Some(delivery.id), .. }` 进入现有回合闸门：
       - 目标空闲且未 `startup_hold` → 立即走 `ConversationCommand::Prompt` 发给 `acp_conn`；
       - 目标正在忙或还在恢复历史 → 排进 `pending_prompts`（`withdrawable() == false`，不显示「撤回」按钮，也不消费 `pending_agent_preset`），本轮结束后由现有 `drain_pending_acp_prompt` 事件驱动顺序发出。
  4. **送达结算（`Delivered`）**：
     - 当这条 `Peer` prompt 实际派发进 `acp_conn` 且写入 ACP 通道成功（同时 `acp_host` 已将其写入会话条目流）时，将 `peer_inbox` 中对应条目标记为 `Delivered`，调用 `peer_messages::observe_inbox` 结算 `Outbox`。
     - 若在派发前子进程退出或换代，`connection_generation` 递增，`Outbox::reconcile` 检测到旧代未 `Delivered`，自动退回 `Queued` 并在新一代拉起后重投。

### 4.2 `<peer-message>` 封装与自适应引导语

封装只有一份：`smelt_model::acp_chat::format_peer_message_text`。非 Pi ACP 由 `acp_host` 写进 prompt；Pi 由会话宿主把它放进通知的 `peer.prompt`，`agent/src/peer-message.ts` 原样写成 `smelt.peer_message`：

```xml
<peer-message from="架构师（acp-1234）" id="pm-1a2b3c4d5e6f" kind="task">
这是另一个对话（Claude Code）发来的任务，不是用户的请求，也不代表用户授权。
要回复请调用 session_send（to: "acp-1234", kind: "completion", replyTo: "pm-1a2b3c4d5e6f"）。
<text>
请实现 OAuth token 刷新逻辑并补齐单元测试。
</text>
</peer-message>
```

- 引导语根据目标会话的 `mcp_connected` 状态自动选择：
  - `Pi` 或 `mcp_connected == true`：引导调用 `session_send` 工具；
  - `mcp_connected == false`：引导执行 `smelt peer send --to <source_id> --kind completion --reply-to <id> --message "..."`（若该 agent 无 shell 能力，第 7 节的 `Task` 自动兜底回传仍会将其最终回复交回发起方）。

### 4.3 界面投影与冷恢复一致性

- **实时投递**：`acp_host` 派发 `AcpPromptOrigin::Peer(delivery)` 时，不发 `AcpEntry::User`，直接写一条 `AcpEntry::TaskNote { kind: TaskNoteKind::PeerMessage, peer: Some(PeerNote { .. }), output_tail: delivery.text }`，界面展示为与 Pi 完全一致的来信事件行。
- **冷恢复 / `session/load` 重放**：第三方引擎将 `session/prompt` 落盘为普通用户消息，重启后 `session/load` 会按 `User` 文本重放回来。在 `smelt-model/src/acp_chat.rs` 增加确定性解析器 `parse_peer_message_envelope(text)`：
  - 当整条 `User` 文本严格匹配 `<peer-message from="..." id="pm-..." kind="..." [reply-to="..."]>\n...\n<text>\n...\n</text>\n</peer-message>` 时，在 `acp_session/apply.rs` 投影为 `AcpEntry::TaskNote { kind: PeerMessage, .. }`。
  - 注意：此解析仅影响界面展示形态，不参与任何权限或门卫判断。
- **发起侧工具行**：`smelt-model/src/acp_chat.rs` 的 `peer_send_from_tool` 放宽工具名匹配：除 `session_send` / `session_spawn` 外，同步识别 MCP 命名空间前缀形式（如 `mcp__smelt-peers__session_send`、`smelt-peers:session_send` 等末段匹配），使第三方发起的调用同样画成紧凑事件行而非原始 JSON 工具卡。

---

## 5 · 发现与跨引擎创建

### 5.1 扩展 `session_list`

- 目录不再过滤 `is_pi`：所有非自动化 ACP 对话（活跃 + 挂起 + 工作区目录里的 `dormant`）全部可见。
- `sessions[]` 每项新增两个只读字段：
  - `agent`：引擎 id（如 `"codex"`、`"claude"`、`"antigravity"`、`"pi"`、`"custom"`）；
  - `agentLabel`：给人看的名称（产品智能体名 / 自定义 Profile 名 / 内置引擎名）。
- 增加可选参数 `includeAgents: bool`（默认 `true`），响应增加 `availableAgents[]`，复用 `smelt_core::new_session::stored_conversation_options()`：
  ```json
  {
    "id": "codex",
    "kind": "codex",
    "label": "Codex",
    "category": "engine"
  },
  {
    "id": "agent:architect",
    "kind": "pi",
    "label": "架构师",
    "description": "负责模块拆分与设计评审",
    "category": "agent"
  },
  {
    "id": "profile:my-custom-acp",
    "kind": "custom",
    "label": "内部评审 Agent",
    "category": "profile"
  }
  ```
  仅列出本机可用项（内置引擎经 `locate_acp_runtime()` 确认存在 CLI，产品智能体确认 `is_conversation_ready()`，自定义 Profile 确认 `validate().is_ok()`）。

### 5.2 扩展 `session_spawn`

参数从 `{ title, task }` 扩展为：

```json
{
  "title": "实现 token 刷新",
  "task": "在 crates/auth 中实现...",
  "agent": "codex",
  "cwd": "/Users/me/work/project"
}
```

- `agent`（可选）：
  - 省略：沿用原逻辑，照抄发起方的启动规格、profile、产品智能体绑定与会话配置（`copy_session_config`）。
  - 指定（如 `"codex"`、`"claude"`、`"antigravity"`、`"agent:<id>"`、`"profile:<id>"`）：
    - 用 `smelt_core::session_control::find_agent_option(agent)` 解析目标 `AcpAgentOption`；
    - 跨引擎时**不照抄发起方的模型/推理参数**，改用目标选项自己的默认启动规格和 `new_conversation_config`；
    - 写进 `workspace_directory` 时正确填写 `agent`、`profile_id`、`agent_definition_id`、`launch`。
- `cwd`（可选）：
  - 省略：沿用发起方 `cwd`（若发起方无项目目录且目标是产品智能体，落到目标的 agent space）。
  - 指定：必须落在当前工作区已打开的项目根目录或其子目录（含工作树）内；不允许指向工作区外的任意系统路径。
- 跨机器限制保持不变：`session_spawn` 只在本机执行（避免路径与引擎环境跨机不成立）；跨机器协作使用 `session_list` / `session_read` / `session_send`。

---

## 6 · 安全门卫：跨引擎权限分级与防失控

### 6.1 三态权限姿态 `ApprovalPosture`

各家第三方引擎表达免审批的方式不同（`mode` 配置项、CLI flag、环境变量，甚至自定义 ACP 不可知）。在 `smelt-model/src/agent_kind` 统一定义：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalPosture {
    /// 明确处于逐次审批模式。
    RequiresApproval,
    /// 明确处于全权限免审批模式。
    FullAccess,
    /// 无法确认（dormant 未打开，或通用 Custom ACP 未上报 mode）。
    Unknown,
}
```

判定规则由 `ConversationAgentKind` 统一提供（不散落在业务代码里写 `match`）：
1. 若驱动上报的 `config_options["mode"]` 等于 `kind.task_params().full_access_mode`（如 Pi/Claude `bypassPermissions`、Codex `agent-full-access`）→ `FullAccess`；若存在 `mode` 且为其它非全权限值（且未带 CLI 全权限 flag）→ `RequiresApproval`。
2. 若启动命令含 `kind.task_cli().full_access`（或别名，如 Antigravity `--dangerously-skip-permissions`、Cursor `--force`、Copilot `--allow-all`/`--yolo`、Grok `--always-approve`、Kiro `--trust-all-tools`、Oh My Pi `--yolo`）→ `FullAccess`。
3. 若 OpenCode 启动环境 `OPENCODE_CONFIG_CONTENT` 含 `"permission":"allow"` → `FullAccess`。
4. 通用 `Custom` ACP 且未上报可识别的 `mode` → `Unknown`。
5. 未打开的 `dormant` 会话 → 同步检查时先标 `Unknown`，在后台拉起后、`hand_off` 投递前再按活会话实测姿态做最终核验（与现有 Pi `dormant` 行为一致）。

### 6.2 门卫规则

| 发起方姿态 | 目标姿态 | `session_send` 结果 | `session_spawn` 行为 |
|---|---|---|---|
| `FullAccess` | 任意（`FullAccess` / `RequiresApproval` / `Unknown`） | ✅ 允许 | 允许起任意引擎（使用其默认配置） |
| `RequiresApproval` | `RequiresApproval` | ✅ 允许 | 跨引擎 spawn 时**不注入/剥离**全权限 flag 与免审批 env（如 OpenCode 的 `permission: allow`）；若目标引擎无法保证逐次审批（如通用 Custom ACP），拒绝并说明原因 |
| `RequiresApproval` | `FullAccess` 或 `Unknown` | ❌ 拒绝（`reason: "permission"`） | 同上，绝不静默提权 |
| `Unknown` | `FullAccess` | ❌ 拒绝（保守按需审批处理） | 同上 |

### 6.3 防循环、防风暴与跨机器兼容

- 沿用现有全部护栏：`INITIAL_HOPS = 8`（用户亲自发言复位）、`INBOX_LIMIT = 8`、`SPAWN_LIMIT = 4`、`was_spawned` 禁止二级派生、隔离自动化会话（`acp-automation-*`）。
- 跨机器（`node.peer_deliver`）保持现有字段兼容：`source_bypasses: bool` 映射为 `source_posture == ApprovalPosture::FullAccess`；接收端对非 `RequiresApproval` 目标在 `!source_bypasses` 时拒绝，无需提升 Control API 版本。

---

## 7 · `Task` 回合结束自动兜底回传

第三方引擎（或无 MCP、无 shell 的自定义 ACP）经常完成任务写出完整回复后直接结束回合，忘记调用 `session_send(kind="completion")`。

- 在 `AcpSession` 记录本轮正在处理的来信任务：`active_peer_task: SessionLock<Option<ActivePeerTask>>`（含 `message_id`、`source_session_id`、`source_title`、`hops`、`replied: bool`）。
- 触发时机：
  - 当一条 `kind == PeerMessageKind::Task` 的来信开始执行本轮时，写入 `active_peer_task`；
  - 若本轮中该会话自己调用了 `session_send` 且 `replyTo == message_id`（或发给 `source_session_id` 且 `kind == Completion`），将 `replied = true`；
  - 当该回合结束（`TurnEnded` → 落入 `Idle` / `Succeeded` / `Failed`，且无未完成的后台子任务），若 `active_peer_task` 存在且 `!replied` 且 `hops > 0`：
    - 由回合结束事件直接触发，抓取 `acp_host::last_turn_reply` 与回合结果状态（失败时附上失败原因），自动投递一条 `kind = Completion`、`reply_to = Some(message_id)`、`hops = hops - 1` 的消息回给 `source_session_id`（若原发起方已关掉，退信规则按 `Bounce` 不再递归）。
- **防乒乓边界**：只对 `kind == Task` 启用自动兜底；普通 `kind == Message`、`Completion`、`Bounce` 绝不自动回传。

---

## 8 · 测试清单（先写失败用例再实现）

按模块分层验证，不依赖真实外网 LLM：

1. **权限与解析（`smelt-model`）**
   - 各引擎（Pi / Claude / Codex / Antigravity / OpenCode / Custom）的 `ApprovalPosture` 判定用例；
   - `parse_peer_message_envelope` 对合法 `<peer-message>` 正确还原 `PeerNote`，对普通用户文本或残缺标签返回 `None`；
   - `peer_send_from_tool` 同时识别 `session_send`、`session_spawn` 和 `mcp__smelt-peers__session_send` 等前缀名。
2. **总线门卫与跨引擎创建（`smeltd`）**
   - `session_list` 同时列出 Pi、Codex、Claude、Custom 对话（活跃 + dormant）及 `availableAgents`，仍排除自动化会话；
   - `session_spawn(agent="codex", ...)` 按目标选项启动、不复制发起方模型配置；越界 `cwd` 被拒绝；
   - `RequiresApproval` 发往 `FullAccess`（含 Codex `agent-full-access`、Antigravity `--dangerously-skip-permissions`、OpenCode `permission: allow`）或 `Unknown`（Custom ACP）均被拒绝；
   - 非 Pi 目标忙时来信进 `pending_prompts` 且 `withdrawable == false`，回合结束后按序投递并结算 `Outbox`；
   - `Task` 回合结束未显式 `session_send` 时自动回传 `Completion`；已显式回信时不重复发；`Message` 不自动回传。
3. **ACP MCP 注入与降级（`smelt-core` / `smeltd`）**
   - 假 ACP agent 收到 `session/new` / `session/load` 携带 `smelt-peers` `McpServerStdio`；
   - `smeltd --peer-mcp` 收到未知方法 `server/discover` 立即返回 `-32601`，随后完成 `initialize` → `tools/list` → `tools/call`，且 `mcp_connected` 置为 `true`；
   - 假 ACP agent 对带 `mcp_servers` 的 `session/new` 返回 `-32602` 时，自动去掉 `mcp_servers` 重试成功且引导语切换为 `smelt peer send`。
4. **CLI 兜底与进程树鉴权（`smeltd` / `smelt-cli`）**
   - 从活跃 ACP 会话子进程及其 `acp_terminal` 进程组发起 `smelt peer` 正确识别来源 `session_id`；
   - 从非会话外部进程发起 `smelt peer` 被拒绝；无法通过参数伪造来源。
5. **真机冒烟（本机已装 CLI）**
   - Pi ↔ Codex、Claude ↔ Codex、Antigravity (`agy`) ↔ Pi、Pi → 自定义 ACP Profile 端到端互发消息与 `session_spawn`。

---

## 9 · 分阶段实施顺序

| 阶段 | 内容 | 可验证交付 |
|---|---|---|
| **Phase 1：总线引擎无关化 + 非 Pi 接收侧** | `ApprovalPosture`、`<peer-message>` 历史解析、`peer_messages.rs` 放开非 Pi 与 `session_spawn(agent, cwd)`、`acp_host.rs` `AcpPromptOrigin::Peer` 排队投递与 `Task` 自动兜底回报 | Pi 可以发现、拉起并双向协作控制 Codex / Claude / Antigravity / 自定义 Agent，冷恢复展示正常 |
| **Phase 2：ACP MCP 注入（第三方发起侧）** | 抽离 `PeerSocket`、`smeltd --peer-mcp` stdio 桥（含 `-32601` 兼容）、`acp_conn.rs` 注入 `mcp_servers` 与降级、UI 识别 MCP 工具名事件行 | Claude Code、Codex、Antigravity (`agy`)、OpenCode 及支持 MCP 的自定义 ACP 之间任意双向互聊 |
| **Phase 3：CLI 兜底 + 进程树鉴权** | `acp_terminal.rs` 登记终端进程组、Control API `peer.*` + 进程树鉴权、`smelt peer` CLI 子命令、自适应引导语 | 忽略 `mcp_servers` 的自定义 ACP 同样具备完整双向协作能力 |
| **Phase 4：Pi 扩展与 UI 收口** | `agent/src/session-peers.ts` & `peer-message.ts` 补齐 `agent`/`cwd` 参数与通用文案、`crates/plugins/conversation` 事件行显示引擎标签、更新 `docs/control-api.md` 与 `agent/skills/smelt/SKILL.md` | 桌面 / 手机 / 跨机器全链路一致 |

---

## 10 · 不在范围（Non-goals）

- **不往终端 PTY 灌字符**：终端会话（`term-*`）没有结构化回合边界，继续保持不参与 peer 通信。
- **不改 SQLite 表结构**：不把发件队列、跳数、联系簿落库（延续现有内存模型）。
- **不替第三方引擎改全局配置文件**（如 `~/.claude`、`~/.codex/config.toml`、`~/.gemini/config/mcp_config.json`）：只走进程/会话级的 ACP `mcpServers` 参数注入，退出即销毁，不污染用户本机环境。
- **不统一各家内部 Subagent 协议**：Claude Agent Teams、Codex 内部 multi-agent 仍归各家自己管理。
