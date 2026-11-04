# 对话的置顶与归档

把侧栏里一条条对话分成三种状态：置顶、普通、归档。置顶的排在所属项目组最前；归档的从项目
分组里撤走，收进侧栏底部一个与「项目」平级的「归档」分段，随时能恢复回原来的项目。

状态存在运行时的工作区目录里，跨设备一致：Mac 上归档的对话，手机上也不再列出来。

## 为什么

对话只能关、不能收。跑完的任务堆在项目下面，想留着回看就得忍受列表越来越长，想清爽就只能
关掉、连同终端历史一起丢。归档给出第三条路：从列表里撤走，但留着。

置顶解决的是反面问题：常用的那两三条对话混在一堆里，每次都得找。项目已经能置顶
（`pinned_project`），对话还不能。

## 范围

本期做：

- 对话级的置顶与归档（桌面端完整交互，手机端只过滤掉归档项）
- 归档时收掉对话在运行时上的进程，恢复时重新起
- 侧栏「归档」分段，就地恢复

本期不做：

- 终端 pane 级的置顶与归档。侧栏一条行 = 一段对话，分屏的多个 pane 是这段对话的子项，
  跟着整段对话走。
- 手机端发起归档/恢复。手机只是不再列出归档的对话。
- 归档的保留期限、自动归档、批量操作。

## 数据模型

### 运行时库（`smelt-store`，schema 28 → 29）

```sql
ALTER TABLE session_group ADD COLUMN archived_at INTEGER;  -- NULL = 未归档
ALTER TABLE session_group ADD COLUMN pinned_at   INTEGER;  -- NULL = 未置顶
```

存 unix 秒而不是 bool：归档分段按归档时间倒序排，置顶区按置顶时间排，不必再加排序字段。

迁移是纯加列，旧行两列都是 NULL，等于「既没归档也没置顶」，语义正确，不需要数据搬运。

### wire 类型（`smelt-model::WorkspaceConversation`）

```rust
#[serde(default)] pub archived_at: Option<i64>,
#[serde(default)] pub pinned_at: Option<i64>,
```

`#[serde(default)]` 让旧客户端发来的、不带这两个字段的对话照常解析。

### 这两个字段只由运行时写

`workspace.conversation.update` 是客户端整段覆盖写。如果让它能改这两个字段，桌面端任何一次
常规 update（改了 cwd、换了 launch 参数）都会把归档状态连带清掉。

所以走和 `history_session_id` 同一条路线（见 `workspace.rs` 的 `preserved_history_session_ids`）：
行已经在库里，就以库里的值为准，客户端带来的一律忽略；只有新建那一次用请求里的值（新建的
对话永远是未归档未置顶，等于两个 NULL）。

配套地，桌面端 `workspace_directory.rs` 的 `without_names()`——算「这段对话要不要发 update」时
剥掉名字那一层——要一并剥掉这两个字段。不剥的话，运行时改完归档状态推回来，客户端拿本地
那份（没有这两个字段）去比，每轮都判成「变了」，发一次注定被忽略的 update，来回打架。

## 协议

两个新 control 方法（`smelt-runtime-client::ControlMethod`）：

| 方法 | 参数 | 行为 |
|---|---|---|
| `workspace.conversation.archive` | `{ conversation_id, archived: bool }` | `true` 归档，`false` 恢复 |
| `workspace.conversation.pin` | `{ conversation_id, pinned: bool }` | 切换置顶 |

两个都进 `ControlMethod::OPTIONAL`。旧运行时缺它们只是归档和置顶用不了，不该让整个客户端判成
「运行时比客户端旧」。客户端调到 `MethodNotFound` 时把菜单项置灰。

不另设 restore 方法：恢复就是 `archived: false`。恢复回哪个项目不用存——归属本来就是按 cwd
现算的（`project_root_of`）。

归档和置顶互斥由运行时保证：归档一段置顶的对话，同时清掉 `pinned_at`。恢复出来的对话是普通
对话，不自动恢复置顶。

## 运行时行为

### 归档

`smeltd` 收到 `archive: true` 时，按顺序：

1. 写库：`archived_at = now`，`pinned_at = NULL`。
2. 收掉进程：这段对话的 ACP 会话走 `kill_acp_session`，每个终端走 `kill_terminal_session`
   （`session_control.rs` 里现成的）。**不调 `forget_session`**——目录行要留着，归档分段
   还要列它。
3. 广播 `workspace.changed`。

收进程失败只记日志，不把归档报成失败：库已经写了，列表状态是用户看得见的那个，进程收不掉
是运行时内部的事，下次重启自然就没了。

### 恢复

写库 `archived_at = NULL` 并广播即可，**不预先起进程**。对话回到项目分组里，用户点进去时
按既有的恢复路径起——和桌面重启后点一条没在跑的对话是同一条路。

### 自动化运行中的会话不能归档

`delete_session` 已经挡掉了自动化 Run 的会话（`is_background_acp_session_id` /
`is_run_session`），理由是 Run 的执行现场归运行时管。归档同样会收掉进程，所以照搬这道门，
返回 `FailedPrecondition`。

## 桌面客户端

### 对账：归档的对话不实体化

`workspace_directory.rs` 的 `receive()` 是目录推送和本地会话对账的地方。归档的对话在这里
**从名册里摘掉**：不进 `ids`，因而

- 本地开着它 → 落进 `removed` → 桌面自动关掉这个会话的视图。别的设备归档了，这台机器跟着收。
- 本地没有它 → 不进 `added` → 启动时不会把几十条归档对话全恢复成活动会话。
- 恢复时它重新出现在 `ids` 里而本地没有 → 进 `added` → 自动实体化回来。

对称且不需要额外的状态机。

但归档分段要把它们列出来，所以 `WorkspaceDirectoryState` 另存一份
`archived: Vec<WorkspaceConversation>`，**只用于渲染归档分段**，不参与 `commands()` 的对账，
也不建会话实体。

本机发起归档时，先本地 `close_session`（收掉视图），再发 `archive` 命令——不发
`conversation.delete`。等推送回来时本地已经没有它，`removed` 为空，不会重复收一次。

### 侧栏

「归档」是侧栏底部一个与「项目」平级的分段标题，和项目分段共用同一个滚动区：

```
项目                                    [筛选与分组]
  ├ smelt
  │   ├ ★ 重构 store          ← 置顶，排在组最前
  │   ├ 修 CI
  │   └ 终端
  └ other-project
归档 (12)                               ← 默认折叠
  ├ 上周的调研
  └ ...
```

- 分段标题右侧写归档条数；没有归档的对话时整个分段不画。
- 折叠状态存客户端库（`workspace_ui` 加一列 `archive_collapsed`，schema 7 → 8）。
- 归档行比普通行暗一档，不显示状态点和「需要你」提示——它们都已经停了。
- 行尾的 × 换成「恢复」图标；右键菜单是「恢复 / 删除」。
- 归档行不参与拖拽排序。

### 置顶

置顶的对话排在**所属项目组内最前**，和项目置顶的做法一致（`pinned_projects_first`）。不做
跨项目的全局置顶区——那会把对话从它的项目里拔出来，和「项目分组」这个主结构打架。

按状态/时间分组（`SidebarGrouping::Status` / `LastUpdated`）时置顶同样生效，排在各组最前。

置顶与客户端本地的手动拖拽序（`sidebar_conversation_order`）的关系：置顶先分区，区内才按手动
序排。和项目置顶的 `pinned_projects_first` 一个写法。

### 菜单与确认

对话行右键菜单（`session_list.rs` 里现在是「重命名 / 复制会话 ID / 强制重启」）加两项：

- 「置顶」/「取消置顶」
- 「归档」

归档正在跑的对话先确认：复用现有的 `CloseConfirm` 机制和 `closing_needs_confirm(status)`，加一个
`CloseConfirm::Archive { ui_id, title }` 变体，文案写明「归档会终止这一轮，对话和历史会留在归档里」。

运行时不支持这两个方法时（`MethodNotFound`），菜单项置灰。

## 手机端

`smelt-mobile` 读的是同一份目录投影。改一处：`catalog/projection.rs` 的 `listing()` 里

```rust
for conversation in &directory.conversations {
    if conversation.archived_at.is_some() { continue; }
    listing.push_conversation(conversation);
}
```

置顶在手机上同样排到所属项目组最前，和桌面一致。

手机端本期不提供归档/恢复入口。

## 测试

跟着各层现有的测试文件走，不新开测试体系。

| 层 | 用例 |
|---|---|
| `smelt-store` | 归档写 `archived_at` 并清 `pinned_at`；客户端的 update 改不动这两个字段；v28 → v29 迁移后旧行两列为 NULL |
| `smelt-core` | `archive_conversation_on` / `pin_conversation_on` 改完目录版本号推进 |
| `smeltd` | 归档收掉进程但目录行还在；自动化 Run 的会话拒绝归档 |
| `smelt`（桌面） | `receive()` 摘掉归档对话：本地开着的落进 `removed`、本地没有的不进 `added`、恢复时进 `added`；`without_names()` 剥掉两个字段后不产生无谓 update；置顶排序在项目组内生效 |
| `smelt-mobile` | 归档的对话不出现在列表里；置顶排到组最前 |

## 边界

- **归档一段对话时它正是当前打开的那段**：按关闭会话的既有规则回退到上一个看过的会话
  （`previous_viewed`）。
- **归档的对话被别的设备删掉**：目录里没有了，归档分段跟着少一条，和普通对话一个路径。
- **旧运行时**：两个方法都缺，菜单置灰，侧栏不画归档分段（没有归档的对话）。
- **项目被关掉，但它下面有归档的对话**：归档分段按对话自己的 cwd 显示项目标签，项目关掉
  不影响归档列表——归档的对话本来就不在项目分组里。恢复时如果项目没开着，按既有的隐式
  组规则落到按 cwd 现算的分组里。
