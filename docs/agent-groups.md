# 本地智能体群组（Agent Groups）

状态：PR 1（本地异构智能体群组）已实现；跨机群组（PR 2）与盲评轮次 / 隔离 worktree 合并（PR 3）将在后续独立 PR 推进。

## 目标与分层

让用户在同一个群组里编排多个异构智能体会话（Pi、Claude Code、Codex、Antigravity、自定义 Agent 等），统一通过 `@alias` / `@all` 与协调者模式协同，并保证：

- **复用现有投递总线**：群组唤醒不另起投递通道，也不伪造用户输入（`AcpUserAction::Prompt`）；统一经 `peer_messages` + `acp_host::deliver_peer_message` 投递，以确定性 `pm-grp-<message_id>-<target_member_id>` ID 做幂等去重与冷恢复识别。
- **数据单一权威**：四张规范化表位于运行时库 `~/.smelt/smelt.sqlite3`（`STORE_SCHEMA_VERSION = 43`），零 JSON 列、零冗余派生列。
- **宿主/插件边界**：`smeltd` 提供持久化、路由、安全闸门、MCP/Pi 工具和 Control API；界面与交互完全在 JS 插件 `plugins/groups/` 中实现，删掉插件 Rust 无需改动。

## 存储结构（`smelt.sqlite3` v43）

| 表 | 职责 |
| --- | --- |
| `agent_group` | 群组元信息、路由模式（`mention` / `coordinator`）、熔断配额、暂停/归档与单调序号 `next_seq` |
| `agent_group_member` | 成员别名（活跃部分唯一）、所有权（`group_owned` / `linked`）、底层 `session_id`、角色与提示词、投递/已读游标 |
| `agent_group_message` | 群组时间线消息（`chat` / `task` / `decision` / `system`）、线程回复、任务状态、链路深度与幂等键 |
| `agent_group_message_mention` | 每条消息针对每个目标成员的 `@` 投递状态（`queued` / `pending_approval` / `delivered` / `coalesced` / `skipped` / `rejected` / `failed`）与确定性 `delivery_id` |

## 路由、熔断与权限水位线

- **发言权与唤醒**：
  - 用户消息开启新的 `chain_id` 并重置熔断计数，但**不清空**其他成员尚未送达的排队唤醒。
  - `mention` 模式只唤醒被 `@alias` / `@all` 提及的活跃未静音成员；`coordinator` 模式在用户未显式 `@` 时默认唤醒协调者。
  - 成员发言遵守群组的 `allow_worker_cross_talk`、`max_chain_hops`、`max_chain_depth` 与 `max_pair_ping_pong` 熔断上限，超限时落盘为 `skipped` 并写明原因。
  - **权限水位线**：当低权限/只读成员（如 `observer` / `reviewer` / `read_only` 或审批姿态更低）尝试 `@` 唤醒更高权限成员时，投递落为 `pending_approval`，等待用户在群组界面点击批准或拒绝；批准后复用原 `delivery_id` 入队投递。
  - **Auto-Bridge**：默认关闭（`auto_bridge_mentions = false`）；仅在群组显式开启且成员回合结束未调用 `group_post` / `group_yield` 时，才将包含有效 `@alias` 的最后一段助手回复桥接进群。

## 所有权隔离（`group_owned` vs `linked`）

- `group_owned`：由群组新建的专属成员会话。通过 `group.changed` 快照中的 `group_owned_session_ids` 从主侧栏隐藏；暂停中止（`abort_running`）或删群时会一并中止/清理。
- `linked`：挂接用户已有会话。始终保留在主侧栏；暂停中止、移出成员或删群**绝不**中止或删除它。

## 安全与访问控制

- 所有 `group.*` Control API（读与写）对来自智能体进程树或未通过本机认证的对端均 **fail-closed** 拒绝；智能体只能经由 MCP / Pi 扩展工具（`group_post`、`group_yield`、`group_read`、`group_members`、`group_task_update`）或 `smelt group` CLI 回退路径访问自己所属的群组，且调用方会话 ID 由守护按对端连接身份反查。
- 在只读白名单（`agent/src/smelt-permission.ts`）中，仅 `group_read` 与 `group_members` 允许免审批自动执行。
