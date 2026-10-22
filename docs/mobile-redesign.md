# 手机端重构：多机指挥台

手机端整版重做。两件事一起做：

1. **内核同时连上所有配对过的机器**（之前一次只连一台「当前运行时」）。
2. **界面按新视觉方向「出菜口」重画**，信息架构围绕跨机器的待办重排。

产品事实见 [`mobile/PRODUCT.md`](../mobile/PRODUCT.md)，视觉系统见
[`mobile/DESIGN.md`](../mobile/DESIGN.md)。

---

## 1 · 内核：从「当前运行时」到「机队」

### 现状

`crates/smelt-mobile` 里连接（`ConnectionHub`）、会话目录（`Catalog`）、对话（`Conversations`）、
终端（`TerminalHub`）都是进程级单例，经 `RuntimeSource` 读「当前运行时」。配对即切换；
已配对机器列表由 Dart 以 JSON 存在钥匙串里；消息草稿以 JSON 文件存在 App 目录。

### 目标结构

```text
Fleet（进程级单例）
 ├─ Store            smelt-mobile.sqlite3：机器登记、消息草稿（业务数据，按版本迁移）
 ├─ Cache            smelt-mobile-cache.sqlite3：会话列表、对话尾部（缓存，坏了重建）
 ├─ IrohDialer       全进程一个 iroh endpoint，按所有机器的 relay 设置决定形态
 ├─ Node(runtime_id) × N
 │    ├─ ConnectionHub     这台机器的连接状态机（原样复用）
 │    ├─ Catalog           这台机器的会话目录（原样复用，source 指向本节点）
 │    ├─ Conversations     这台机器的对话订阅（改为可同时订多个会话）
 │    └─ TerminalHub       这台机器的终端（原样复用）
 └─ Pass             跨机器投影：单据（等你处理的事 + 详情）与各灶台的在做 / 刚完成
```

- 每个 `Node` 用一个 `NodeSource` 实现三个模块各自的 `RuntimeSource`：`current()` 读本节点的
  `ConnectionHub`，`subscribe()` 挂本节点的状态，`cache()` 共用一份缓存。模块内部逻辑不变。
- `init_client` 读出登记表，为每台机器建节点并在后台线程各自 `connect`。离线的机器进入
  `Reconnecting` 自己退避重试，互不影响。
- 删除全部「当前运行时」概念：`connect` / `disconnect` / `setActive` 不再存在。

### iroh endpoint

同一把设备密钥只能挂一个 endpoint，所以全进程只有一个 `IrohClient`。它的形态由**所有已登记机器**
的 relay 设置决定：

| 登记的机器 | endpoint 形态 |
|---|---|
| 都没指定 relay | n0 预设（与之前一致） |
| 都指定了**同一个** relay | Minimal + 这个 relay + 系统 DNS（与之前一致，iOS 开代理也能连） |
| 其它组合 | n0 预设；指定了 relay 的机器在拨号地址里带上它的 relay（桌面多运行时就是这样） |

形态只在配对、忘记机器时可能变化。变化时先重建 `IrohClient`，再让所有节点重新 `connect`
（旧会话换代退出，模块按代号丢旧状态）。第三种组合下，自建 relay 的域名走 iroh 自带解析器，
iOS 开着系统代理时可能连不上那台机器的 relay；这是混用时的已知限制，界面在机器详情里写明链路。

### 机器登记（Store）

```sql
CREATE TABLE runtime (
    runtime_id   TEXT PRIMARY KEY,
    endpoint_id  TEXT NOT NULL UNIQUE,
    relay_url    TEXT,
    name         TEXT NOT NULL,      -- 用户起的名字，默认主机名
    hostname     TEXT,
    os           TEXT,
    version      TEXT NOT NULL,
    accent       INTEGER NOT NULL,   -- 灶台标签色在色板里的序号
    position     INTEGER NOT NULL,   -- 列表顺序
    paired_at_ms INTEGER NOT NULL
);
CREATE TABLE draft (
    runtime_id    TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    content       TEXT NOT NULL,
    request_id    TEXT,              -- 发过但没等到确认：重发沿用同一个 id
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (runtime_id, session_id)
);
CREATE TABLE draft_image (
    runtime_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    position   INTEGER NOT NULL,
    mime       TEXT NOT NULL,
    bytes      BLOB NOT NULL,
    PRIMARY KEY (runtime_id, session_id, position)
);
```

`PRAGMA user_version` 记库版本，升级按步迁移，不删库。忘记一台机器时删它的登记、草稿和缓存行。

旧数据一次性迁移：Dart 启动时读到钥匙串里的 `smelt.runtimes.v1`，调 `import_runtimes` 写进登记表，
成功后删掉那个键。旧 JSON 草稿文件同样读一次、导入、删除。迁移代码只在 `mobile/lib/migration/`。

### 配对

`pair(uri, device_name)`：解析配对串 → 若 endpoint 已登记，用那台机器的节点重新配对（被撤销后再配）；
否则按新 relay 设置调整 endpoint 形态，新建节点配对 → 成功后写登记（默认名字取主机名，标签色取
色板里还没用过的第一个），失败则丢掉节点、恢复原形态。配对不影响其它机器的连接。

### 对话订阅

`Conversations` 由「全局一个订阅」改为按 `session_id` 存多个订阅。同一会话可以有几个订阅者，各带
调用方给的 `watch_id`：共用一条 watch、都收到视图，`unwatch(session_id, watch_id)` 只摘自己，最后
一个摘掉才停。同一会话的页面换下来时新页先订、旧页后退，旧页的退订不能把新页一起停掉。平板双栏
同时开着列表和对话、逐张处理时预载下一张，都需要多个订阅；Dart 侧原来那个「订阅栈」随之删除，
Dart 的订阅流被取消就是退订。

### 跨机器投影（Pass）

原来 Dart 的 `PendingActionsController` 与 `session_filters.dart` 挪进内核 `pass` 模块：

- **单据**：所有机器上需要你处理的会话（attention 为审批 / 输入 / 失败，或状态为 NeedsYou），按
  等待时长从久到新排（先来先处理）。每张单据带上 `pending_actions` 取回的审批 / 询问详情；会话摘要里
  与「在等什么」相关的字段一变就重取，取回前 `details_loaded = false`。
- **灶台**：每台机器一组：在做（Running）、刚完成（成功且未读）、最近跑过（成功且已读，最多 5 条）。
  其余闲置会话不进指挥台，在「会话」页按机器 → 项目浏览。
- 投影变化合并后整份推送（与会话列表同一节奏）。

### 对外 API（`crate::api`）

| 模块 | 入口 |
|---|---|
| `identity` | `init_client(data_dir)`、`parse_pairing(uri)`、`pair(uri, device_name) -> RuntimeRecord` |
| `fleet`（替换 `connection`） | `fleet_events(sink)`（整份机队：登记 + 连接状态）、`rename_runtime`、`set_runtime_accent`、`move_runtime`、`forget_runtime`、`reconnect_runtime`、`verify_connections`、`import_runtimes` |
| `pass` | `pass_events(sink)` |
| `catalog` | 原有入口全部加 `runtime_id`；`catalog_events` 推所有机器的事件，事件带 `runtime_id` |
| `conversation` | 原有入口全部加 `runtime_id`；`watch_conversation(runtime_id, session_id, watch_id, sink)`、`unwatch_conversation(runtime_id, session_id, watch_id)` |
| `terminal` | 原有入口全部加 `runtime_id` |
| `draft` | `load_draft`、`save_draft`、`clear_draft`（键为 `runtime_id + session_id`） |

---

## 2 · 界面

### 信息架构

| 去处 | 内容 |
|---|---|
| **指挥台**（首屏） | 单据轨（跨机器、等你处理的事，原地批 / 答）；按灶台（机器）分组的在做 / 刚完成；右上「+」新建会话 |
| **会话** | 全部会话：按机器 → 项目分组，可按机器、agent、状态筛选，搜索；历史会话（恢复、改名） |
| **智能体** | 智能体定义与自动化（开关、立即运行、最近运行），按机器分组 |
| **设置** | 机器（配对新机器、改名、标签色、排序、链路详情、忘记）、外观、终端、关于 |

- 手机：底部 tab 栏。平板：左侧导航栏 + 列表 / 详情双栏。
- 对话、终端、新建会话、单据逐张处理是压栈页或底部弹层，不进 tab。
- 「机器是上下文不是门槛」：任何列表行都带灶台标签色和机器名，不要求先选机器。

### 招牌动作：单据轨

需要你动手的事是**纸**（单据），其余界面都是钢灰底和白色搁板。单据挂在指挥台顶部的轨上（一道平涂的
结构线，不做金属渐变）：新单据从右侧挂上；处理后盖章（「已允许 · 14:02」）、上滑离轨，下一张补位。

- **逐张处理**：点开单据进入。每一页是一张撑满高度的长单据，内容从上往下、按钮在纸的底边（单手够得着），
  下一张从右边露出一截、横滑即到；第几张写在标题里。处理完盖章、自动翻到下一张。
- **决定留痕**：在单据上批过的审批，对话时间线里那条工具调用下面留一枚章，不随单据离轨消失。
- **对话页**：待处理的审批 / 询问以同一张单据停靠在输入框上方。
- **平板**：单据只在右侧详情栏（逐张处理）里出现；左栏顶上只有一行「N 件事等你」，点了把详情栏切过去。
  同一张单据不在一屏上出现两次。详情栏不留空：指挥台默认逐张处理，会话默认最近那条，设置默认第一台机器。

### 诚实的状态

- 机器没连上时，所有地方用同一种说法（重连中 / 离线 / 被移除），显示缓存时加上「显示的是 HH:MM 的数据」；
  机器连着但数据还没到时说「同步中」。
- 缓存里「在做」的会话画静止的空心点和静态时间，不呼吸、不走表。终端标题在机器断开时写机器状态，不写「实时」。

### 视觉规则（细节见 `mobile/DESIGN.md`）

- 琥珀只给「需要你」：单据上你要按的那颗按钮、等你处理的计数。主动作用实心墨，次要用中性底。
- 等宽字（JetBrains Mono NL，随包）只给单据头、正在走的耗时、命令、代码、diff 和终端；相对时间、工具名用系统字。
- 每条内容带它所在机器的灶台标签色。状态记号 = 图标 + 颜色 + 读屏文字，不只靠颜色。

### 文字

中英双语，跟随系统语言（Flutter `gen-l10n`）。文案按功能分片写在 `mobile/lib/l10n/src/<功能>_{en,zh}.arb`，
`dart run tool/merge_arb.dart` 合并成 `lib/l10n/app_{en,zh}.arb` 再生成 `L10n`；CI 检查生成物是否最新。
不出现厨房行话（「86」「出餐」等），隐喻只活在视觉里。

### 视觉审查

`--dart-define=SMELT_DEMO=true` 用合成数据启动（`lib/data/demo_backend.dart`），`mobile/tool/demo_screenshots.sh`
（iOS 模拟器）与 `mobile/tool/demo_screenshots_android.sh`（Android 模拟器）把每一屏的深浅色截下来。
