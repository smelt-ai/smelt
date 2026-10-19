# 手机接入运行时（P4）实施计划

设计依据见 `docs/remote-runtime.html`（「协议客户端：桌面和手机共用一份 Rust 实现」与 P4「手机收敛」）。
本文只记实施拆分和每一步的接口约定。

## 现状

9505f375 删掉了 `smelt-remote-gateway`、长期 token 和 `smelt/tunnel/1`。手机还在走旧路：

- `crates/smelt-mobile` 拨 `smelt/tunnel/1`，在 127.0.0.1 开端口，Dart 自己说 JSON-WS（`/acp/ws`、`/terminal/{id}/ws`）。
  运行时只认 `smelt/runtime/1`，握手直接失败（TLS alert 120，no_application_protocol）。
- 配对串要 `token`，新格式是 `smelt+iroh://<endpoint>/?code=<一次性码>[&relay=…]`。
- 每次启动随机生成 iroh 密钥。新协议里设备身份就是这把密钥对应的 EndpointId，必须持久化。

网关原来替手机做的事，现在要分到两处：

| 网关做的事 | 新归属 |
|---|---|
| 读 SQLite / 扫目录 / 起进程（启动项目录、CLI 探测、智能体选项、建会话、找智能体 space、历史列表） | 运行时（smeltd Control API） |
| 会话列表投影、attention、自动化目录投影（纯计算） | `smelt-mobile` Rust 层（手机本地，attention 已读状态本来就该按客户端） |
| 大图缩小（省带宽，必须在发送端） | 运行时 |
| 帧解码、增量镜像（`FrameMirror`）、分页合并 | `smelt-mobile` Rust 层 |

## 完成标准

- `smelt-mobile` 不再开本地端口；Dart 里没有协议解析代码（不再有 JSON-WS、不再解析 `ConversationSnapshot` JSON）。
- 手机用新配对串配对，能连任何运行时（含 Linux）。
- 原有页面功能不退化：指挥台、项目树、对话、审批与询问、模型与配置切换、技能、图片、历史与续接、改名、新建与删除、自动化、终端。

## 架构

```
Flutter UI ──FRB(async fn / Stream)──> smelt-mobile（Rust 客户端内核）
                                          ├─ identity   设备密钥持久化、配对
                                          ├─ connection IrohClient + RuntimeClient，健康检查、链路状态
                                          ├─ catalog    事件订阅 → 会话摘要 / attention / 工作区 / 自动化
                                          ├─ conversation acp_watch + FrameMirror → 移动端快照视图模型
                                          └─ terminal   watch(controls_geometry) 字节流 + input/resize
                                                  │
                                          smelt-runtime-client / smelt-iroh::runtime（与桌面共用）
                                                  │ iroh smelt/runtime/1
                                               smeltd
```

FRB 只暴露 `smelt-mobile` 自己定义的视图模型（只含界面用到的字段），不直接镜像 `smelt-model` 的内部类型。
内部类型改字段不会连带改 Dart。

## 分步

### P4.0 共享代码与运行时补口

1. **事件订阅解码挪到可移动端编译的位置。** `smelt-client::daemon_state::DaemonStateEventClient` 的解码状态机
   （快照凑批、revision 去重、会话水位与 tombstone、lag 重收）不依赖 SQLite，挪进 `smelt-model::daemon_state`；
   socket IO 留在调用方。桌面改为引用新位置，行为不变。
2. **运行时新 Control 方法**（类型放 `smelt-model::control_api`，实现在 smeltd，逻辑搬自旧网关）：
   - `new_session.catalog {}` → `{agents: [AcpAgentOption], sections: NewSessionSections}`。
     用 `stored_conversation_options` 与 `stored_new_session_sections`，CLI 探测结果按 120 秒缓存。
   - `session.create {project_root, agent_option_id?, launch_key?, resume_id?, kind?, request_id?}` → `{session_id}`。
     搬 `create_mobile_session`：`request_id`（UUID）让重试落到同一会话；`project_root` 为空时开在智能体自己的 space。
   - `session.delete {session_id}` → `{}`。运行时自己判断终端还是对话；自动化的运行拒绝删除。
   - `session.agent_bindings {}` → `{bindings: {session_id: agent_definition_id}}`。
3. **图片按需取。** `acp_watch` / `acp_snapshot` 新增 `image_refs: bool`（默认 false，桌面不受影响）。
   打开后，条目里超过 64 KB 的图片不再内联：`data_b64` 置空，补 `original: {entry, digest}`。
   新 Control 方法 `conversation.image {session_id, entry, digest, max_edge?}` → `{mime, data_b64}`：
   带 `max_edge` 时出缩略图（短边 320、长边裁到 960 的旧规则改为按 `max_edge` 等比缩放），不带时出可看的原图
   （≤1 MB 原样，否则长边缩到 2560）。按 digest 缓存。图片不在出帧热路径上解码。
4. 确认 `smelt-model` 能编进 iOS / Android（手机内核要直接用它的投影、attention、`FrameMirror` 和快照类型），
   并把这个检查加进移动端 CI。

### P4.1 身份与连接（smelt-mobile）

- 设备密钥：Dart 传入 App 数据目录，Rust 用 `smelt_iroh::load_or_create_secret` 存 `client-iroh-secret`（0600）。
- 配对：`parse_pairing_uri` → `device.pair {code, name}` → `system.describe`，返回运行时身份（runtime_id、hostname、endpoint、relay）。
  已保存的运行时只存 endpoint、relay、名字与 runtime_id，没有 token。
- 连接：每个运行时一个 `RuntimeClient`（`IrohClient::connector`），状态流（connecting / connected / reconnecting / offline）、
  链路类型与 RTT、心跳（`system.describe` 或订阅流存活）。阻塞调用一律放 `spawn_blocking`。
- 删除 `iroh_tunnel.rs`、`api_iroh.rs` 与 `smelt_iroh::ALPN`。
- iOS DNS：`IrohClient::new` 固定用 n0 预设，走 iroh 自带 DNS，iOS 上会退回硬编码公共 DNS、绕开系统代理
  （见 `smelt-iroh/src/system_dns.rs`，旧隧道修过）。配对串带 relay 时，手机的 endpoint 要用 Minimal + 自建 relay +
  系统 DNS（同 `bind_endpoint`）；`IrohClient` 补一个按 relay 构造的入口。链路类型与 RTT 用
  `RuntimeConnector` 上新增的 `path_status()`（搬旧隧道的选中路径判断）。

FRB 接口（已实现，`crates/smelt-mobile/src/api`）。会阻塞的调用都是普通 `pub fn`：FRB 2.12 把它们放在自己的
线程池上跑，Dart 拿到 Future；`async fn` 会进 FRB 的 tokio 运行时，而 `RuntimeConnector` 内部 `block_on`，不能在那里调用。
一次只连一个当前运行时，与现在「当前桌面」一致。

```rust
fn init_client(data_dir) -> DeviceIdentity                       // 密钥落盘
#[frb(sync)] fn parse_pairing(uri) -> PairingTarget               // 只解析，给界面预览
fn pair(uri, device_name) -> RuntimeInfo                          // device.pair + system.describe
fn connect(endpoint_id, relay_url?) -> RuntimeInfo                // 切换当前运行时
fn disconnect(); fn verify_connection()
fn connection_events(sink: StreamSink<ConnectionEvent>)          // 状态、链路、错误
// 错误：SmeltError { kind: Unpaired | Offline | InvalidPairing | Incompatible | Cancelled | Other, message }
```

crate 内部给后续阶段的入口：`client::current_runtime()`（generation + RuntimeClient）与 `client::subscribe`
（generation 变了就丢掉旧运行时的状态）。`IrohClient` 的 `Drop` 会 `block_on`，不能在 tokio 线程上释放。

### P4.2 会话目录

Rust 侧开一条 `event_subscribe`（topic：`session.state_changed`、`session.removed`、`remote_sessions.changed`、
`workspace_menu.changed`、`automations.changed`；不订阅 `conversation.wake_requested` 和外观请求，否则运行时会误以为
有人能唤醒对话），把旧网关的投影搬过来：

- 会话摘要（`mobile_workspace_menu` + 自动化运行行 + `mobile_summary`，排序规则不变）。
- attention：`apply_daemon_transition`，冷启动用 baseline，不把已在审批中的会话记成新提醒。
- 工作区：菜单 projects + `new_session.catalog`。自动化：`automation.list` + `agent.definition.list` 投影。
- 历史：`history.list`（agent / profile 由 agent option 推出），改名走 `history_rename`。
- 新建、删除走 P4.0 的新方法；`markRead` 在本地。

Dart：`GatewayService` 换成薄的 `RuntimeService`，只转发 FRB 流和调用；配对页与 `PairingStorage` 改成新格式。

### P4.3 对话

- `acp_watch {id, include_debug:false, image_refs:true, history_session_id, known_entries, snapshot_revision, tail_limit}`，
  整段帧 `FrameMirror::full`，增量帧 `FrameMirror::apply`；镜像对不上时发 `acp_action Refresh` 等下一份整段帧。
- `ConversationSnapshot` → 移动端视图模型，分页合并（`acp_snapshot` 取更早的页）挪到 Rust。
- 发送（`acp_submit_input`，`submission_id` 用客户端的 request id，重试不重复提交）、取消、配置、审批、询问、技能、图片。

### P4.4 终端

`watch {controls_geometry:true, cols, rows, cell_w, cell_h, max_scrollback_lines}`：头行 + 关键帧 + 原始字节流；
resize 在同一连接写 type 1 帧；输入走 `input` op。终端配色由手机自己定（设计文档：不再随运行时下发）。

### P4.5 收尾

- 离线缓存：快照模型已在 Rust，缓存随之挪到 Rust；存储方式（SQLite）单独确认后再做。
- 删除旧 Dart 协议代码与假网关测试，补 Rust 单测与 Dart 层测试。
- iOS / Android 构建，真机验证配对、对话、终端。
- 更新 `docs/remote-runtime.html` 的 P4 进度。
