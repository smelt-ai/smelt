# Smelt 事件总线与插件事件架构

## 状态

EventHub、稳定 wire DTO、`event_subscribe`、桌面端和 Remote Gateway 一等客户端已实现。
插件管理器、插件凭据签发和完整 Action SDK 仍是后续工作；在此之前 daemon 明确拒绝
`plugin` 事件身份，不允许插件自报 ID 或 capability。

## 背景

Smelt 正在从内建功能集合转向插件化架构。插件需要观察会话、Agent、自动化和工作区等领域事实，
并通过受控动作影响后续行为。当前代码已经存在几种不同用途的通信机制：

- `DaemonStateBus` 使用 `watch + broadcast` 向 GUI 和远程端发布最新状态与增量。
- `agent_bus` 在 Agent 之间投递消息，实际语义是带副作用的命令通道。
- Control API 承载一次请求、一次响应的控制操作。
- `open`、`watch` 和 ACP 连接承载长连接或二进制会话数据流。

这些机制不能简单合并成一个万能消息通道。事件、状态、命令和数据流具有不同语义、可靠性与
安全边界。新架构统一跨模块和跨进程的领域事实入口，但不混淆这些通信类型。

## 目标

- 所有允许插件观察的领域事实通过统一 EventHub 发布。
- 核心内部使用强类型事件，跨进程边界使用稳定、版本化协议。
- 插件保持进程外隔离，不能直接进入 smeltd、访问内部锁或绕过 capability 校验。
- 慢插件、崩溃插件和恶意插件不能阻塞核心状态提交或影响其他订阅者。
- 重要事件支持持久化、确认、重试和断点恢复。
- 同一领域对象内的事件顺序稳定，迟到事件不能回滚较新状态。
- 插件通过显式 Action/Command 产生副作用，而不是直接修改核心状态。
- daemon、桌面端、Mobile、MCP 和插件在同一版本完成切换。

## 非目标

- 不把终端字节、鼠标移动、渲染刷新、锁状态等内部实现信号放入事件总线。
- 不把全部应用状态改造成 event sourcing。
- 不承诺跨所有对象的全局事件顺序。
- 不承诺 exactly-once；跨进程系统无法在不绑定业务事务的情况下可靠提供该语义。
- 不用事件总线替代终端、ACP 等双向或二进制数据流。

## 核心原则

### 事件是已发生的事实

事件只能在领域变更提交成功后发布，例如：

```text
session.state_changed
agent.message_delivered
attention.created
```

事件不能被取消，也不能用于询问“是否允许执行”。需要执行前拦截、拒绝或改写的能力，应使用
独立的 interceptor/policy 扩展点。

### 副作用必须走 Action/Command

插件订阅事件后，如果需要影响系统，必须调用受 capability 约束的 Action API：

```text
插件收到 session.state_changed
    -> 调用 workspace.create_isolated
    -> 领域提交成功
    -> 发布 workspace_menu.changed
```

事件处理器不能持有核心领域对象，也不能直接写 SQLite、PTY、ACP socket 或内存状态。

### 统一入口，不统一传输语义

| 通信类型 | 用途 | 可靠性与连接 |
|---|---|---|
| Domain Event | 已发生的业务事实 | durable 或 ephemeral，多订阅者 |
| State Snapshot | 当前可收敛状态 | 最新值替换，可从快照恢复 |
| Action/Command | 请求产生副作用 | 点对点、鉴权、幂等 |
| Session Stream | 终端或 ACP 数据 | 独立长连接，可双向或二进制 |

## Crate 边界

最佳依赖结构拆成两个新 crate，而不是把协议与运行时放在同一个包中：

```text
smelt-plugin-api          # 稳定公共协议，低依赖
        ^
        |
smelt-event-bus           # 通用事件运行时
        ^
        |
smeltd                    # 领域适配、插件管理、权限决策

smelt-core --------------> smelt-plugin-api
GUI / Mobile / MCP ------> smelt-plugin-api
Plugin SDK --------------> smelt-plugin-api
```

### `smelt-plugin-api`

该 crate 是插件与 Smelt 共享的稳定协议层，只依赖 `serde` 等基础库，不能依赖 Tokio、
GPUI、SQLite 或 `smelt-core`。

包含：

- `EventEnvelope`、`EventId`、`Topic`、`AggregateRef`。
- 核心事件的稳定 DTO。
- topic 和 payload schema 版本。
- 插件 manifest、capability 和订阅声明。
- subscribe、ack、nack、cursor 和 lag 协议。
- Action/Command 请求、响应、错误码和幂等键。
- 协议版本协商。

该 crate 不暴露内部 `SessionState`、ACP runtime 等领域实现。smeltd 负责把
内部类型映射为稳定 DTO，避免内部重构直接破坏插件协议。

### `smelt-event-bus`

该 crate 实现通用事件基础设施，但不知道 session、Agent 或 ACP 的具体含义。

包含：

- topic descriptor registry。
- payload schema 和命名空间校验。
- durable/ephemeral 两种 QoS。
- 每订阅者独立的有界 mailbox。
- aggregate 内有序调度。
- ack、重试、cursor 和 dead-letter。
- 背压、限流、lag 检测和循环检测。
- snapshot provider 注册。
- capability-aware subscription filter。
- `EventStore` trait。
- 基于 `smelt-store` 的 SQLite EventStore/outbox 实现。

该 crate 不负责：

- 启动或终止插件进程。
- 决定某个插件拥有哪些 capability。
- 把领域对象转换成事件 DTO。
- 执行 Action/Command。

### `smeltd`

smeltd 是事件架构的组合根和唯一权威宿主：

- 创建全局 EventHub。
- 注册核心 topic descriptor。
- 注册 snapshot provider。
- 在领域提交出口发布事件。
- 加载插件 manifest 并计算有效 capability。
- 管理插件子进程、连接、健康状态和重启。
- 将插件 Action 请求路由到领域服务。
- 将事件协议适配到 Unix socket 或其他受控传输。

## 总体架构

```text
领域状态变更
     |
     +-- 持久状态 + outbox 同事务提交
     |
     `-- EventHub dispatcher
             |
             +-- SQLite EventStore
             +-- state snapshot projection
             +-- GUI/Mobile subscription
             +-- plugin mailbox A
             +-- plugin mailbox B
             `-- diagnostics/metrics

插件 mailbox
     |
     +-- delivery
     +-- ack/nack
     `-- cursor advancement

插件需要产生副作用
     |
     `-- Action API + command_id + capability
             |
             `-- 领域提交 -> 新事实事件
```

## 事件模型

### 信封

```rust
pub struct EventEnvelope<P> {
    pub event_id: EventId,
    pub topic: Topic,
    pub schema_version: u16,
    pub occurred_at_ms: u64,
    pub aggregate: Option<AggregateRef>,
    pub aggregate_revision: Option<u64>,
    pub source: EventSource,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<EventId>,
    pub hop_count: u16,
    pub payload: P,
}

pub struct AggregateRef {
    pub kind: String,
    pub id: String,
}

pub enum EventSource {
    Core,
    Plugin { plugin_id: String },
}
```

字段语义：

- `event_id`：全局唯一，用于日志、幂等和 ack。
- `topic`：稳定事件名称，例如 `session.phase_changed`。
- `schema_version`：该 topic payload 的版本，不等同于总线协议版本。
- `aggregate`：事件所属领域对象。
- `aggregate_revision`：对象内单调递增版本，用于排序和拒绝旧事件。
- `correlation_id`：一次用户操作或自动化链路的统一追踪 ID。
- `causation_id`：直接触发当前事件的前一条事件。
- `hop_count`：插件链路深度，用于阻断循环。

### 内部与 wire 类型

核心内部禁止到处传递 `serde_json::Value`。内部生产者发布强类型事件：

```rust
pub enum CoreEvent {
    SessionCreated(SessionCreated),
    SessionPhaseChanged(SessionPhaseChanged),
    SessionRemoved(SessionRemoved),
    AttentionCreated(AttentionCreated),
}
```

EventHub 在协议边界根据 topic descriptor 序列化为 wire payload。插件自定义事件使用经过
schema 校验的 JSON payload，但只能位于自己的命名空间。

## Topic 与命名空间

### 核心命名空间

核心 topic 只能由 smeltd 发布：

```text
session.state_changed
session.removed
agent.message_delivered
attention.created
attention.cleared
remote_sessions.changed
workspace_menu.changed
automations.changed
plugin.started
plugin.stopped
plugin.failed
```

### 插件命名空间

插件只能发布：

```text
plugin.<plugin_id>.*
```

例如：

```text
plugin.com.example.auto-review.review_requested
```

EventHub 必须校验发布者身份与 topic 前缀，插件不能伪造 `session.*`、`workspace_menu.*` 或其他核心
事实。

### Topic descriptor

每个 topic 在启动时注册：

```rust
pub struct TopicDescriptor {
    pub topic: Topic,
    pub schema_version: u16,
    pub delivery: DeliveryClass,
    pub required_subscribe_capability: Capability,
    pub required_publish_capability: Option<Capability>,
    pub sensitivity: DataSensitivity,
    pub max_payload_bytes: usize,
    pub snapshot_kind: Option<SnapshotKind>,
}
```

重复注册、非法名称、未知 schema 或 capability 冲突必须让启动失败，不能静默覆盖。

## 可靠性模型

### Delivery class

```rust
pub enum DeliveryClass {
    Durable,
    Ephemeral,
}
```

`Durable` 用于会影响插件自动化决策的领域事实。事件先进入持久化 event log/outbox，再进入
订阅者 mailbox。

`Ephemeral` 用于允许丢失且可从当前状态恢复的提示或观测数据。它仍使用有界队列和显式 lag
报告，不能静默丢弃。该类事件只进入内存队列，不写 SQLite。

### 投递语义

- durable 事件提供 at-least-once。
- 插件必须按 `event_id` 实现幂等。
- EventHub 不承诺 exactly-once。
- Action/Command 必须携带 `command_id`，服务端记录执行结果并拒绝重复副作用。
- 同一 `(aggregate.kind, aggregate.id)` 按 `aggregate_revision` 有序。
- 不提供跨 aggregate 的全局顺序保证。

### Outbox

SQLite 在本架构中不是给人排障的文本日志，而是保证系统恢复正确性的 `EventStore` /
`DeliveryStore`。它保存 outbox、durable event、subscription cursor、重试状态、
dead-letter 和 command 幂等结果。

现有 `app_log`、`log` 或 `tracing` 只用于可观测性：它们允许轮转、采样或写入失败，不能作为
可靠投递的真源。EventStore 写入失败必须显式返回错误，不能像诊断日志一样静默丢弃。

持久领域变更必须尽量与 outbox 在同一 SQLite 事务中提交：

```text
BEGIN
  update domain state
  insert event_outbox
COMMIT
```

dispatcher 读取 outbox，将事件写入 event log 并投递，成功后标记已分发。这样可以避免状态已
提交但 daemon 在发布前崩溃导致事件永久丢失。

这不是“每个事件额外执行一次独立 commit”：领域事务本来就要提交，outbox 只在同一事务中
增加一条 `INSERT`，不会增加第二次 fsync。

纯运行时事件无法与外部进程状态形成数据库原子事务。需要 durable 语义时，它们先进入
EventStore 单写者队列，由 writer 按以下任一条件触发微批提交：

- 队列累计到 64 条。
- 最早待写事件等待达到 10ms。
- 调用方显式要求 `flush`，例如 daemon 准备退出或执行无缝升级。

`64 / 10ms` 是初始默认值，应作为内部配置和基准测试参数，而不是 wire contract。writer 在
一个事务中使用 prepared statement 批量插入，生产者等待的是该批次的 durability 结果，不为
每条事件单独创建连接、开启事务和 fsync。

如果纯运行时事件可以通过 snapshot、revision 或启动对账恢复，应定义为 `Ephemeral`，完全
不进入 EventStore。会话 phase、attention 展示和 UI 投影默认走该路径；不能为了“以后也许
有用”全部落盘。

### SQLite 写入模型

EventStore 复用已有 `smelt-store` 和同一数据库迁移体系，不新增第二个日志文件或数据库。
桌面端数据库使用：

- WAL 模式。
- `synchronous=NORMAL`，除非某类数据明确要求更强的 durability。
- 单写者，避免多个插件各持一条写连接争抢 SQLite writer lock。
- 长寿命连接和 prepared statement。
- 短事务；事务内禁止 socket I/O、插件调用和等待 mailbox。
- 按 sequence、topic、subscription cursor 所需查询建立复合索引。

写入分为三条明确路径：

| 数据 | SQLite 行为 | 延迟路径 |
|---|---|---|
| Ephemeral event | 不写 | 内存直接分发 |
| 持久领域变更产生的 durable event | 与领域数据同事务写 outbox | 原事务只多一次插入 |
| 无领域事务的 durable event | 单写者微批提交 | 最多等待批次窗口 |

EventHub 必须暴露批次大小、等待时间、提交耗时和队列深度指标，以便基准测试后调整默认值。

### Cursor、ack 与重试

每个 durable subscription 保存独立 cursor：

```text
(plugin_id, subscription_id) -> last_acked_sequence
```

- 只有 ack 后才能推进 cursor。
- nack 可声明 retryable 或 permanent。
- retryable 使用有上限的指数退避。
- permanent 或超过重试上限的事件进入 dead-letter。
- 每个 durable subscription 同时最多投递一条未确认事件，retryable nack 完成重投前不会
  投递后继事件。
- 投递并行性来自不同 subscription；单个 subscription 内严格保持顺序。mailbox 仍可容纳
  初始化快照，但扩大 mailbox 不会增加该订阅的 durable 并行度。

dispatcher 从 EventStore 分页批量读取，初始建议每批最多 128 条。插件可以逐条 ack，但
EventHub 在内存中累计连续水位，只将“已连续确认到 sequence N”写入 cursor：

- 连续确认达到 50 条时刷盘。
- 距离上次 cursor 持久化达到 100ms 时刷盘。
- 插件断开、daemon 退出或无缝升级前强制刷盘。

`128 / 50 / 100ms` 同样是可基准调整的内部默认值。cursor 尚未刷盘时 daemon 崩溃，允许重启
后重复投递少量已处理事件；插件必须通过 `event_id` 幂等。不能为了避免这种可接受的重复而
对每个 ack 单独提交 SQLite。

dead-letter、永久 nack 和 command 幂等结果属于低频正确性数据，可在对应领域事务中直接
写入，不需要人为等待微批。

## 背压与故障隔离

每个插件拥有独立、有界 mailbox 和 worker：

- 核心生产者只提交到 EventHub，不能直接调用插件代码。
- 慢插件不能持有 EventHub、领域状态或 SQLite 事务锁。
- mailbox 满时 durable 订阅停止拉取并保留 cursor；ephemeral 订阅收到显式 lag。
- 插件进程崩溃只影响自己的 worker。
- 插件持续失败时进入熔断状态，发布 `plugin.failed` 并等待人工或策略重启。
- socket 写入必须有超时，不能让冻结的客户端阻塞 dispatcher。

EventHub 的锁内只允许更新内存索引和复制待发送数据，禁止执行 socket I/O、插件调用或领域
Action。

内存热路径传递共享的只读事件表示，例如 `Arc<StoredEvent>`。同一事件只序列化一次 wire
payload，并由多个订阅者共享字节缓冲；不能针对每个插件重复构造完整 JSON。

snapshot 按领域拆分并使用 revision 更新。单个 session phase 变化不能 clone 全部 session
和 workspace 状态；完整快照只在新订阅、显式恢复或 lag 后生成。

## Snapshot 与状态收敛

EventHub 支持按 `SnapshotKind` 注册 snapshot provider。新订阅或发生 lag 时：

1. 先建立增量订阅水位。
2. 读取与该水位一致的快照。
3. 发送快照。
4. 从水位之后继续发送增量。

客户端通过 aggregate revision 拒绝重复和倒退事件。现有 `DaemonStateBus` 的
`watch + broadcast + lag recovery` 能力由该机制替代，不再作为独立内部总线存在。

快照不是事件历史，也不能用于触发“每个对象刚创建”的自动化。需要完整历史语义的插件必须
使用 durable 订阅和 cursor。

## 插件模型

### Manifest

```json
{
  "id": "com.example.auto-review",
  "version": "1.0.0",
  "api_version": 1,
  "subscriptions": [
    {
      "id": "review-sessions",
      "topics": ["session.state_changed", "agent.message_delivered"],
      "delivery": "durable"
    }
  ],
  "publishes": [
    "plugin.com.example.auto-review.review_requested"
  ],
  "capabilities": [
    "session.read",
    "agent.message.read",
    "notification.publish"
  ]
}
```

manifest 声明的是申请能力，最终有效 capability 由 smeltd 根据用户授权和系统策略计算。

### 进程隔离

插件跑在守护监管的共享 Bun 进程里，不是 smeltd 动态库：

- 不发给 daemon socket、凭证或直接 Action 通道。
- 每个包装有自己的 `plugin-data` 目录；capability 约束的是宿主代办的 Smelt API。
- Bun 拥有当前用户的 OS 权限，不是 sandbox。
- daemon 负责生命周期、退出状态、重启和日志归属。

## Action/Command API

事件与 Action 使用不同信封：

```rust
pub struct ActionRequest<P> {
    pub command_id: CommandId,
    pub action: String,
    pub api_version: u16,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<EventId>,
    pub params: P,
}
```

服务端处理顺序：

1. 校验插件身份和 capability。
2. 校验 action schema。
3. 查询 `command_id` 是否已执行。
4. 执行领域事务。
5. 记录结果和新事件 outbox。
6. 返回稳定错误码或结果。

插件不能通过发布事件替代 Action，也不能通过自定义事件诱导核心执行未注册的动作。

## Interceptor 与 Policy

有些插件能力需要在核心动作执行前参与，例如：

- 禁止危险命令。
- 修改任务路由目标。
- 对外发消息做合规校验。

这些能力必须使用独立、显式注册的 interceptor/policy 扩展点：

```text
Action request
    -> ordered policy chain
    -> allow / deny / transform
    -> domain commit
    -> post-commit event
```

policy 必须声明超时、失败策略、执行顺序和 capability。普通事件订阅没有阻塞核心动作的权力，
否则慢插件会把整个系统变成隐式同步调用链。

## 循环与风暴保护

插件 A 和插件 B 可能通过事件互相触发。EventHub 必须同时实施：

- `correlation_id` 链路追踪。
- `causation_id` 因果关系。
- 最大 `hop_count`。
- 每插件、每 topic 的速率限制。
- 同一插件对同一事件的幂等记录。
- 反复失败或高频循环时自动熔断。

超过链路深度或速率限制必须产生可诊断错误，不能静默丢弃。

## 协议与版本

需要区分三种版本：

- plugin API version：整体连接和能力协议。
- event bus protocol version：subscribe/ack/cursor wire 协议。
- topic schema version：单个事件 payload 版本。

连接建立时协商支持范围。未知 topic 可以按订阅过滤忽略；已订阅 topic 的未知 schema 必须
nack 并报告不兼容，不能尝试按旧结构解析。

即使所有组件在同一发布中切换，仍必须支持 daemon 与 GUI/Mobile 在升级瞬间短暂版本错位。
版本协商是正式架构的一部分，不是保留旧内部总线。

## 安全模型

- 订阅、发布和 Action 分别校验 capability。
- capability 默认拒绝，不能因字段缺失回退为全权限。
- `EventClientAuth::FirstParty` 的 kind 只是请求值，不是身份凭据。smeltd 从 Unix peer
  credential 取得 pid 和 audit token。macOS 身份只问内核：`csops_audittoken` 读
  team + signing-id（audit 绑定，无 TOCTOU；Santa/Chromium 同款），永不读磁盘、
  永不调 Security framework 的动态查询——自更新把磁盘文件换掉后，运行中进程
  照样能自证，不再 `-67034`（旧链条先后在此炸过两次）。比对分三条 leg：
  生产（双边 team 一致 + identifier 相符，与 Apple XPC team-identity 要求同构，
  不 pin 具体证书，证书轮换不影响）；本地自签名（无 team，比较磁盘静态证书，
  同 keychain 重签 skew 下恒成立）；ad-hoc 仅 debug 认同目录兄弟，release 照样
  拒绝。Desktop 的 signing-id 必须为 `com.zzfn.smelt`，daemon 为 `smeltd`，
  独立 Remote Gateway 为 `gateway`，内嵌 Gateway 必须与 daemon 同 pid。
  basename 单独永远不足以认证（只用于 kind 选择）。请求 kind 与 peer 事实不一致、
  签名检查失败、平台无法可靠取证或 wire 试图附加 capability 时均 fail closed。
  认证后 Desktop 可读 session、remote sessions、workspace 和 automations。
- 一等客户端使用稳定 plugin identity，并为每条进程内连接生成唯一 subscription ID。
  当前订阅为 ephemeral，不发送 ack，也不依赖 cursor。
- 事件 payload 按 sensitivity 做字段裁剪。
- 插件不能订阅自己未获授权的 session、workspace 或 automation 范围。
- 插件 token 必须可撤销，并绑定插件实例。
- 日志不得记录 token、凭据或未脱敏敏感 payload。
- 单事件大小、订阅数量、发布速率和 mailbox 容量必须有上限。

## 可观测性

EventHub 至少提供以下指标和诊断：

- topic 发布量、失败量和 payload 大小。
- 每插件 mailbox 深度。
- 未 ack 数量和最旧未 ack 时长。
- 重试次数、dead-letter 数量和 lag 次数。
- Action 成功、拒绝、超时和幂等命中次数。
- correlation trace 查询。
- 插件运行状态、退出原因和熔断原因。

系统设置或诊断页应能查看插件订阅、有效 capability、cursor、积压和 dead-letter，并支持
暂停插件、重放 dead-letter 和撤销授权。

## 与现有模块的关系

### `DaemonStateBus`

删除。其状态快照、增量广播和 lag recovery 由 EventHub snapshot projection 统一提供。

### `agent_bus`

改名为 `peer_messaging`。Agent 消息投递是带副作用的 command，不属于事件总线。
`source + command_id` 提供 daemon/store 幂等，ACP/Terminal 目标也按稳定 delivery ID
去重。投递成功后的 command result 与 `agent.message_delivered` outbox 在同一 SQLite
事务提交。PTY/provider 外部副作用无法与 SQLite exactly-once 原子化；事务失败时返回
`result_unknown`，daemon 保留结果和待持久化事实并后台重试，而不是返回普通可重试错误。
command ID 还绑定请求的 SHA-256 指纹（source、请求 target 原文、message）；同 ID 改参
返回 `conflict`，不会因 provider alias 后续解析到别的会话而复用旧结果。

durable cursor 同时保存 delivery + 排序后 topics 的 SHA-256 declaration fingerprint。
相同 plugin/subscription ID 重连时声明必须一致，cursor flush 只更新水位，不覆盖指纹。
schema v4 把已有 v3 cursor 标成 `legacy-v3-unbound`：升级后的第一次 durable bind 可在
同一事务中写入新 declaration fingerprint 并把 cursor 重置为 0，安全重放匹配 topic 的
历史，而不是让未知旧声明的水位跳过新 topic 历史；之后声明固定。v3 peer-message 结果同样
带 legacy sentinel，但因旧请求内容不可恢复，后续同 ID 请求会安全地返回冲突而不是猜测
绑定。

### Control API

保留“一次请求、一次响应”的职责，并扩展为插件 Action API 的传输入口。协议契约从
`smelt-core` 的内部模块迁移到稳定公共 API crate，领域实现仍由 smeltd 持有。

### `event_subscribe`

正式内部路径已由版本化 `event_subscribe` 替代。桌面端订阅 session state/removed、
remote sessions、workspace menu 和 automations；Remote Gateway 订阅同样的投影
topic。两者消费稳定 DTO / `CoreProjectionEvent`，Lag 后等待恢复 snapshots。旧
`op=subscribe` 已移除，避免它绕过正式 peer 认证。

### `AgentEvent`

provider hook 归一化仍保留。`AgentEvent` 是输入协议；smeltd reducer 提交状态后，再发布
稳定的领域事件。不能把 provider 原始 hook 直接暴露为核心插件契约。

## 一次性切换范围

本设计不采用长期双轨迁移。实现分支内完成以下协调切换后再合入：

- 新增 `smelt-plugin-api` 与 `smelt-event-bus`。
- EventHub、SQLite EventStore、outbox、cursor 和 dead-letter 完整可用。
- smeltd 所有领域提交出口接入 EventHub。
- `DaemonStateBus` 删除。
- `agent_bus` 重命名并明确 command 语义。
- Control API 契约迁移到公共协议 crate。
- 桌面端、Mobile、MCP 切换到新订阅和 Action 协议。
- 插件 manifest、capability、进程管理和 SDK 同时落地。
- 无旧内部总线路径、无 legacy handler 复制、无双写。

协议版本协商和升级窗口兼容仍需保留，但它们只处理不同版本二进制短暂共存，不作为长期双轨
架构。

## 必须验证的场景

- 慢插件不会阻塞状态提交、GUI 或其他插件。
- 插件崩溃后 durable cursor 可以恢复，事件不会永久丢失。
- 重复投递不会产生重复副作用。
- 同一 aggregate 的旧 revision 不能覆盖新 revision。
- 快照与增量订阅之间不存在丢事件窗口。
- mailbox 满、socket 冻结和 ack 超时均有显式诊断。
- 插件不能发布核心 topic 或执行未授权 Action。
- token 撤销立即终止订阅和 Action 权限。
- 插件互相触发的循环会被 hop count、限流或熔断终止。
- outbox 在 daemon 崩溃和重启后仍会完成投递。
- dead-letter 可查询、可审计、可受控重放。
- daemon、桌面端和 Mobile 版本错位时明确拒绝不兼容协议，而不是错误解析。
- ephemeral 发布不产生 SQLite 写入。
- durable outbox 不产生领域事务之外的第二次 commit。
- 微批 writer 和 cursor batching 在目标负载下不会阻塞事件生产者。
- 单个状态增量不会复制完整全局快照，也不会为每个订阅者重复序列化 payload。

## 决策摘要

Smelt 使用两个新 crate 建立插件化基础：

- `smelt-plugin-api` 定义稳定、低依赖的插件公共协议。
- `smelt-event-bus` 实现与领域无关的可靠事件运行时。

smeltd 作为唯一组合根，负责领域提交、事件发布、插件隔离和 capability 决策。领域事实通过
EventHub 发布；状态通过 snapshot 收敛；副作用通过 Action/Command 执行；终端和 ACP 数据
继续使用独立 stream。该边界确保插件可以互相影响系统行为，同时不会把核心状态、可靠性和
安全控制交给插件。
