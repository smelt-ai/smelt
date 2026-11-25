# 插件驱动对话

状态：宿主这边已实现；第一个用户是开源工作台插件。

插件（第一个用户是开源工作台）要能起一段对话、往里发话、准确知道某一轮结束了以及结果，
并且对无人值守的轮次限制工具。对话本身就是普通对话：进工作区目录、在侧栏里、能被人看和
插手；插件把它嵌进自己的页面（从 `"smelt/conversation"` 导入 `<SmeltConversation>`，
参数 `machine` / `sessionId` / `directoryAcp` 或 `spec`）是显示上的事，不影响它存在哪。

## 原则

- **一轮是显式的操作，不靠猜。** 插件发话时带自己的投递 id，这一轮结束时运行时推出
  「哪个投递结束了」，结果按 id 取。不看 phase 落到空闲去推断。
- **结果是这一轮的最终回复，校验归插件。** 运行时只交回原文。按 schema 取 JSON、不合格
  在同一对话里追问，插件自己就能做对，不进宿主。
- **工具限制由执行方机械执行。** 交给 agent 自己的权限引擎（它懂自己的工具和命令语法），
  不靠提示词，也不在运行时里自己猜命令。做不到限制的 agent 直接拒绝，不悄悄放开跑。
- **数据只存一处。** 对话的权威记录在工作区目录；插件只存 `session_id` 和投递 id 的引用。
- **能扩已有方法就不另开。** 起对话就是 `session.create`，只多几个参数。

## 已有、直接复用

| 机制 | 位置 |
|---|---|
| 新建对话、按 `request_id` 幂等重试、写进工作区目录 | `session.create`（`smeltd/src/session_control.rs`） |
| 外部投递 id：`accepted_delivery_ids` / `active_delivery_id` / `completed_delivery_id`，按 id 去重 | `smelt-model/src/acp_session.rs`，归约在 `smelt-core/src/acp_session/apply.rs` |
| 回合结果 `AcpTurnOutcome` | 同上 |
| 挂起对话的唤醒、等就绪 | `acp_idle::wake_suspended_acp_session` / `wait_for_acp_ready` |
| 归档 | `workspace.conversation.archive`（按目录里的对话 id，从 `workspace.directory` 按 `acp.sid` 找） |
| JS 调控制接口、看会话状态 | `call(machine, method, params, timeout_ms)`、`session_states(machine)` + `sessions_changed` |

## 新增

### 1. `session.create` 多三个参数（只对对话）

```json
{
  "cwd": "/Users/me/.smelt/worktrees/oss-workbench/owner-repo/pr-240",
  "agent_option_id": "claude",
  "title": "PR #240 Review",
  "disallowed_tools": ["Edit", "Write", "MultiEdit", "Bash(git push:*)"],
  "request_id": "6f1c2b8e-…"
}
// result: { "session_id": "acp-6f1c2b8e…" }
```

- `cwd`：插件自己管的工作树不在项目菜单里，直接给目录（绝对路径、已存在），和
  `project_root` / `workset_id` 二选一。落在某棵工作树的任务目录里时照样绑到那棵树。
- `title`：记成用户起的名字（目录里的 `custom_title`）。
- `disallowed_tools`：Claude 权限规则的写法，开会话（`session/new` 和 `session/load` 都带）
  时经 `_meta.claudeCode.options.disallowedTools` 交给 Claude 适配器（随 `agent/package.json`
  带的版本把它并进 Claude SDK 的同名选项）。deny 规则在任何权限模式下都生效，所以权限
  模式照智能体配置走，无人值守可以开免审批，被禁的照样禁。
  - 哪些选项能带：`new_session.catalog` 的 `restrictable_agent_ids`（启动规格不出线，调用方
    只能从这里知道）。别的 agent 给了这一项：`session.create` 回 `invalid_params`；万一运行时拿着限制去拉起
    别的 agent（包括 Pi），驱动在起进程之前就报错退出。
  - 限制记在运行时内存里的会话上，这个进程里重连、闲置唤醒都带着；不落盘，运行时重启
    后续上的是一段不带限制的普通对话。
- 不发话：发话只有 `conversation.prompt` 一条路。

### 2. `conversation.prompt`

```json
// params
{ "session_id": "…", "delivery_id": "oss-pr-240-review-1", "text": "…",
  "disallowed_tools": ["Edit", "Write", "MultiEdit", "Bash(git push:*)"] }
// result
{}
```

- 用 `AcpUserAction::Prompt { delivery_id }` 投进去。正在跑就排队等这一轮结束，不 steer
  进别人的回合（带投递 id 的一律不 steer）。同一个 `delivery_id` 重发不会再执行一次。
- 挂起的先唤醒；刚拉起、还在续历史的先等就绪，免得重放把这一轮冲掉。
- `disallowed_tools` 是前置条件：必须和运行时现在的完全一样，否则 `failed_precondition`。
  插件据此把任务当成中断，不往不受限的对话里发无人值守的话。人在对话里自己说话不受影响。

### 3. 会话状态加 `completed_delivery_id`

`SessionStateChanged` / `DaemonSessionState`（随现有状态订阅推送，JS 里 `session_states`
拿到、变了发 `sessions_changed`）：最近结束的一轮对应的投递 id，用户自己说的那轮是 `None`。
只加字段，不升协议；旧运行时不报就是 `None`。它只是叫醒信号，结果以 `turn_result` 为准
（下一轮投递结束后它就换成新的 id）。

### 4. `conversation.turn_result`

```json
// params
{ "session_id": "…", "delivery_id": "oss-pr-240-review-1" }
// result 之一
{ "status": "pending" }
{ "status": "finished", "outcome": "succeeded", "text": "…这一轮最后一段助手正文…" }
{ "status": "unknown" }
```

- 回合结束时、推状态之前，运行时按投递 id 记下结果（最终正文，取法同自动化的结果）。
  每段对话留最近 32 条，只在内存里。之后人在对话里接着说话，也不影响按 id 取。
- `pending`：收下了还没结束（含排队）。`unknown`：没投过，或运行时重启过、结果丢了，
  插件按中断处理。

## 不做

- 不在运行时校验 JSON Schema（插件能做对）。
- 不加「对话所有者」、不改表结构（对话就是普通对话）。
- 不另加关闭方法（用归档）。
- 不加代数、不另起方法建对话：限制本身就是 `prompt` 的前置条件，起对话就是 `session.create`。
- Pi 的工具限制这次不做；要无人值守跑 Pi，得先给 Pi 的启动加工具白名单，单独再议。产品智能体
  （智能体定义）目前只能用 Pi 开对话，所以带限制的对话用 Claude 的 agent 选项。

## 开源工作台怎么用

1. 设置里分别选「修复用的 agent」「Review 用的 agent」（`new_session.catalog` 的
   `agents[].id`，要带限制就得是 Claude）。
2. 修复：`session.create`（工作树为 `cwd`，禁合并、强推、直推主分支）→ `conversation.prompt` →
   等 `sessions_changed` 里出现自己的 `completed_delivery_id`（启动、重连时直接查一次
   `turn_result`）→ `turn_result` → 按 schema 取结论，不合格追问一次 → 后续 CI 打回、人类
   拍板都是同一段对话里再 `prompt`。
3. Review：同上，另外禁 `Edit` / `Write` / `MultiEdit` / `Bash(git push:*)` /
   `Bash(gh pr review:*)`。
4. 任务行展开就嵌着那段对话；需要人介入时直接在里面说话，不再有「接管修复对话」。
5. 合并、跳过、重新入队时归档对话。
6. 删掉 `executor.js` 的子进程执行和输出解析、实时活动行。
