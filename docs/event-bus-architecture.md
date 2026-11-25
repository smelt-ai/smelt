# Smelt 事件总线架构

> 定位：当前实现（`crates/smelt-event-protocol`、`crates/smelt-event-bus`、`crates/smeltd/src/protocol/auth.rs`）。
>
> 事件只在 `smeltd` 进程内按发布顺序编号并投递（ephemeral），不落盘、不确认、不重放；订阅方滞后或重连时用对应快照补齐。

## 协议版本与演进

- 当前 `EVENT_BUS_PROTOCOL_VERSION = 7`，`MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION = 6`（真源在 `crates/smelt-event-protocol/src/lib.rs`）。
- **宽容读取**：v7 起运行时推给客户端的载荷忽略未知字段，标了会增长的显示类枚举落到 `Unknown`；新增字段不升版本。
- **独立订阅话题**：加在 v7 之后的新话题不进核心订阅，走独立订阅连接（例如 `pi_config.changed`、`history.changed`、`appearance.apply_requested`、`app.command_requested`、`attention.changed`、`agent_host.changed`）。旧运行时只拒掉那一条订阅，不影响核心连接。
- **不抬最低版本**：新客户端读得了的旧运行时照常连；只有形状变化、无法宽容读取时才抬 `MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION`。
- `CORE_PROJECTION_VERSION` / `CORE_SESSION_PROJECTION_VERSION` / `CORE_SESSION_STATE_SCHEMA_VERSION` 已冻结，仅为兼容 v6 客户端在 `system.describe` 中照旧报出。

## 核心话题与能力

话题真源在 `crates/smelt-event-protocol/src/lib.rs` 的 `core_topic_descriptors()`（共 16 个核心话题）：

| 话题 | 所需订阅能力 | 快照类型 | 上限 |
| --- | --- | --- | --- |
| `session.state_changed` | `session.read` | `sessions` | 64 KB |
| `session.removed` | `session.read` | `sessions` | 16 KB |
| `workspace.changed` | `workspace.read` | `workspace` | 4 MB |
| `automations.changed` | `automation.read` | `automations` | 1 MB |
| `conversation.wake_requested` | `conversation.wake` | — | 4 KB |
| `appearance.change_requested` | `appearance.control` | — | 4 KB |
| `appearance.apply_requested` | `appearance.control` | — | 4 KB |
| `app.command_requested` | `app.command` | — | 64 KB |
| `project.changed` | `workspace.read` | — | 4 KB |
| `agent_memory.changed` | `workspace.read` | — | 4 KB |
| `pi_config.changed` | `workspace.read` | — | 1 KB |
| `history.changed` | `workspace.read` | — | 1 KB |
| `terminal.notification` | `session.read` | — | 16 KB |
| `terminal.progress` | `session.read` | — | 4 KB |
| `attention.changed` | `session.read` | `attention` | 48 KB |
| `agent_host.changed` | `workspace.read` | `agent_host` | 1 MB |

## 鉴权与身份

- `EventClientAuth` 目前仅有 `FirstParty { kind: FirstPartyClientKind::Desktop }`（`crates/smelt-event-protocol/src/lib.rs`），不接受未认证或未知字段的订阅请求。
- 本机连接校验对端进程身份；远程连接经 iroh 配对白名单鉴权。
- 桌面端默认授予的能力（`crates/smeltd/src/protocol/auth.rs`）：`session.read`、`workspace.read`、`automation.read`、`conversation.wake`、`appearance.control`、`app.command`。

## 进程内路由（`smelt-event-bus`）

- **话题登记与发布**：`TopicRegistry` 登记全部合法话题、载荷上限与所需能力；事件序列化一次（`StoredEvent.wire_bytes`），各订阅方共享同一份线上字节。
- **快照先于增量**：订阅建立时先对关联的 `SnapshotKind` 发当前快照，再推其后的增量事件。
- **有界邮箱与滞后恢复**：每个订阅方拥有独立有界队列；慢消费者溢出时不阻塞发布者，而是标记滞后并在恢复时重推快照对齐。
- **防环与有序**：内置 `DEFAULT_MAX_HOPS = 16`、因果去重缓存（`DEFAULT_CAUSAL_CACHE_CAPACITY = 1024`）以及聚合单调版本号校验（`aggregate_revisions`）。
- **与 Control API 分工**：事件总线只负责推送状态与变化通知；短请求命令统一走 `op=control`（见 [`control-api.md`](control-api.md)），终端与 ACP 数据流走各自的长连接协议。
