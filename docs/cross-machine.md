# 跨机器：节点模型

状态：设计已定（#367），分三个 PR 落地。

一台电脑是一个**节点**，节点身份归常驻的 smeltd 所有。桌面、命令行、Pi 都只是本机 smeltd 的客户端，经它去其它节点。手机没有常驻守护，仍以客户端设备配对。

## 1 · 为什么要改

- Pi 的 `session_*` 工具只看得到本机对话，`smelt` 命令行只连本机 smeltd，远程机器上的自动化、智能体、MCP 都管不了。
- 「这台电脑连哪些机器、用什么身份连」只有桌面知道：机器清单在客户端库的 `runtime_connection`，身份是桌面自己的 `client-iroh-secret`。smeltd 和命令行都不知道。
- 不能让命令行也拿 `client-iroh-secret` 去拨：同一个 endpoint id 第二个连上 relay 时，前一个会被挤掉（iroh-relay `SameEndpointIdConnected`）。

## 2 · 模型

| 概念 | 是什么 | 存在哪 |
|---|---|---|
| 节点身份 | 运行时的 `iroh-secret`。同一个 endpoint 既接受连入，也拨出 | 文件，smeltd 独有 |
| 对端节点（peer） | 和本节点双向配对过的节点：它是谁、怎么连、本机智能体能不能用 | 运行时库 `peer_node`，两边各记对方 |
| 客户端设备 | 手机等只连入、不被连的设备 | 运行时库 `paired_device`（不变） |

- 配对是双向的：A 用 B 的配对串配对后，A 的 `peer_node` 有 B，B 的 `peer_node` 有 A，两边都能主动连对方，也都接受对方连入。每一边只记自己信任谁、怎么连对方，就像 `known_hosts` 加 `authorized_keys`，不是同一份数据存两处。
- 显示名各记各的：A 怎么称呼 B 只存在 A 这边。
- 运行时的「远程访问」开关管所有连入：关着时设备和对端节点都连不进来，只能往外拨。
- 往外拨用哪个 endpoint：远程访问在跑就借它的（同一个 endpoint 收发）；正在绑定就等它；关着才用同一把密钥单独绑一个只拨出的。远程访问的 endpoint 绑上时丢掉只拨出的那个，不让两个同时在线。
- 节点间互相信任到和已配对设备同等的程度：都是同一个用户的机器。

## 3 · 本机出口：`op=forward`

```
桌面 / 命令行 / Pi ──unix socket──▶ 本机 smeltd ──iroh 流──▶ 对端 smeltd
```

- 首帧 `{"op":"forward","runtime_id":"…"}`。smeltd 拨通后回一行 `{"ok":true}`，之后两边只搬字节，不解析里面的协议；失败回 `{"ok":false,"code":…,"error":…}` 并断开。客户端读到回执才发后续帧，所以回执之后没有预读的字节。
- 只收 `Principal::Local`。经 iroh 进来的连接一律拒绝，不会被串成多跳。
- `smelt-runtime-client` 提供 `ForwardConnector { socket, runtime_id }`，上层的 `RuntimeClient`、订阅、终端、对话流都不用改。
- 对端看到的调用方是 `Principal::Peer(runtime_id)`，权限同已配对设备。

### 权限

出口按调用方判断：

| 调用方 | 能转发到哪 |
|---|---|
| 签过名的桌面 | 所有对端 |
| 智能体会话进程树里的进程（Pi 的 bash、Pi 自己） | 只有打开了「允许本机智能体使用」（`peer_node.agent_access`）的对端 |
| 本机其它进程（终端里的人） | 所有对端 |

智能体会话进程树的判断复用记忆权限那套（`memory_peer_auth::deny_agent_session_peer`），认不出调用方时按智能体处理。对端的增删改（`peer.pair/update/remove`）同样拒绝智能体会话进程树，智能体改不了自己的权限。

## 4 · 控制方法

本机（`Principal::Local`）：

| method | 说明 |
|---|---|
| `peer.list` | 本节点的对端，按配对先后排 |
| `peer.pair` | `{pairing_uri}`：用对方的配对串双向配对，回对端记录 |
| `peer.update` | `{runtime_id, name?, relay_url?, os?, agent_access?}` |
| `peer.remove` | `{runtime_id}`：删本地记录、断开它连进来的连接，再在后台请它也删掉本节点 |
| `peer.link` | `{runtime_id}`：当前路径和延迟，不为此拨号 |
| `peer.reset` | 丢掉往外拨的连接（和只拨出的 endpoint），下次重拨。系统睡醒后桌面调用 |

节点之间（`Principal::Unpaired` / `Principal::Peer`）：

| method | 说明 |
|---|---|
| `node.pair` | `{code, node: NodeInfo}` → `NodeInfo`。未配对的连接可调。对方的 endpoint id 取自连接本身，不信参数 |
| `node.unpair` | 对端请本节点删掉它 |

`NodeInfo = {runtime_id, name, relay_url, os}`，`name` 是对方机器名，只作首次显示名的默认值。

连接路径变化（直连 / 中继切换、断开）用单独的长连接 op `peer_link_watch` 推送，桌面据此显示连接质量。

## 5 · 命令行

- 全局参数 `smelt --machine <名字|runtime_id> <子命令> …`：所有子命令都用 `ForwardConnector` 连那台机器，不为每项功能另做远程版。
- `smelt machine list`：列出对端节点。
- 配对、删除、改权限不放进命令行：这些会改变智能体能碰到的范围，只在桌面上做。
- smelt 技能文档补上 `--machine` 的用法。

## 6 · 桌面迁移

桌面不再有自己的 iroh 身份，远程机器一律经本机 smeltd 转发（`crates/smelt/src/runtimes.rs`）。

- 机器列表取自 `peer.list`。启动时同步读一次（本机 socket，最多等 1.5 秒）；读不到（守护还没起来、是旧版本）就等连上本机运行时再读。之后本机运行时推 `peers.changed`（单独订阅，不进核心订阅）时重读，别的节点来配对、取消配对也会推。
- 每台机器的客户端用 `SwitchConnector`：每次连接时看这台机器眼下走哪条路（经本机转发 / 旧直连）。迁移时就地换掉，已经开着的终端、对话拿的是同一个客户端，下次开流就走新的。
- 连接质量：对端节点订 `peer_link_watch`；设置页露出来时用 `peer.link` 采样。系统睡醒调 `peer.reset`。
- 添加机器调 `peer.pair`，删除调 `peer.remove`，改名、改系统、改中继调 `peer.update`。机器右键菜单加「允许本机智能体使用」开关（宿主命令 `smelt.app/set-machine-agent-access`，`Connection.agent_access`）。
- **迁移旧机器**：客户端库里的旧机器连上后，用旧身份调 `device.pair_begin` 拿配对码，交给本机 smeltd 走 `peer.pair`，再把这边的名字、系统写进去。然后就地换成经本机转发，删掉客户端库那一行，再用旧身份撤销旧设备；撤销会断开旧连接，订阅随之按断线重连，走的就是新链路。对方还不认 `node.pair`（旧版本）就先留着直连，等它升级后连上时再迁。
- 本机 smeltd 还是旧版本（会话在跑、还没换守护）时，读不到 `peer.list`：所有机器照旧直连，新加的机器也按旧方式配对，等本机守护换上新版本后再自动迁。
- 旧直连机器都迁完后，桌面自己的 iroh endpoint 就关掉、不再绑。客户端库的 `runtime_connection` 表和旧直连代码等都迁移完后再删（之后单独发一版）。

## 7 · 跨节点会话消息

实现在 `crates/smeltd/src/peer_messages.rs`，扩展工具不加参数。

- 地址：别的机器上的对话写成 `<runtime_id>/<会话 id>`；本机对话的 id 里没有 `/`。`session_send` 的 `to`、`session_read` 的 `sessionId` 照 `session_list` 给的原样填。
- 发过去：本机门卫检查完，信照样进发件队列；投递线程经节点连接调对方的 `node.peer_deliver`（只收对端节点）。对方用自己的门卫和发件队列投进对话，权限档位、积压上限在那边比。对方收下，这边就算交出去了（`sent` 里是 `forwarded`）；对方投不进去，会把退信发回来。
- 来源：交出去时，本机对话的 id 带上本机的 runtime id，对方收到的来源就是 `<本机 runtime_id>/<会话 id>`。收信端的提示里写着回给谁，照着回就送回来。对方只收来源是调用方自己那台机器上的对话，不能冒充别的机器。标题后面加上机器名。
- 权限：智能体主动找别的机器（发、读、列），要用户在机器菜单里打开「允许本机智能体使用」。回给发消息过来的那个对话不需要。跳数预算跟着消息走。
- `session_list`：同时问允许智能体使用的那几台机器（`node.peer_list`），问不到的写在最后；`session_read` 经 `node.peer_read`。
- `session_spawn` 只在本机：新对话照抄发起方的启动规格和项目路径，换台机器就不成立。
- 只在内存里：「收到过谁的消息」最多记 1024 对，守护重启后就没了，那之后回信要用户允许过那台机器。

## 8 · 数据库

运行时库升到 v40，新增：

```sql
CREATE TABLE peer_node (
  runtime_id   TEXT PRIMARY KEY,
  endpoint_id  TEXT NOT NULL UNIQUE,
  name         TEXT NOT NULL,
  relay_url    TEXT,
  os           TEXT,
  agent_access INTEGER NOT NULL DEFAULT 0 CHECK (agent_access IN (0, 1)),
  added_at_ms  INTEGER NOT NULL
);
```

客户端库这次不动；`runtime_connection` 等迁移完再删。

## 9 · 分阶段

| PR | 内容 |
|---|---|
| PR1 | smelt-iroh 节点端点（同一个 endpoint 收发）、`peer_node`、`node.pair/unpair`、`peer.*`、`op=forward` 和权限、命令行 `--machine` / `machine list` |
| PR2 | 桌面改走本机转发、迁移旧机器、机器页的智能体开关、连接质量 |
| PR3 | 跨节点会话消息（`node.peer_deliver/list/read`，`<runtime_id>/<会话 id>` 地址） |
