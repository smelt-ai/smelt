# 远程机器与手机端的自动化

状态：设计中，未开工。对应 issue #79（跟踪 issue #76）。

两件事：

1. **桌面**：远程机器上的自动化看得到、能新建 / 编辑 / 删除 / 启停 / 跑一次，运行结果、通知、运行详情和本机的一样进桌面。
2. **手机**：在现有的自动化列表和详情上，加新建、编辑、删除。

---

## 1 · 现状

### 运行时：方法都在

- 每个运行时有自己的 `AutomationStore`（`crates/smelt-core/src/automation_store.rs`，SQLite），调度器在
  `crates/smeltd/src/automation_runtime.rs`。
- 控制方法：`automation.list` / `get` / `upsert` / `delete` / `set_enabled` / `run_once` / `run_detail`
  （类型在 `crates/smelt-model/src/control_api.rs`）。另有老的守护操作 `automation_command`
  （`Upsert` / `Delete` / `SetEnabled` / `RunOnce` / `CancelRun`，返回结果连同整份快照），桌面现在只用它。
  配对过的设备两种都能调。
- 推送：事件 topic `automations.changed`，内容是整份 `AutomationFile.live_projection()`
  （裁掉了运行输出、触发载荷和指令快照，正文用 `run_detail` 取），附带这台机器的 `webhook_base_url`。
- 定义 `Automation`（`crates/smelt-model/src/automation.rs`）：`id`（客户端生成）、`name`、`enabled`、
  `trigger{schedules, webhooks, events}`（至少一条）、`action`（`Agent{agent_definition_id, prompt}` 或
  `Shell{command, args}`）、`sinks`；`workspace_dir` 由运行时分配。调度规则五种：每天、每周（星期位掩码）、
  每 N 分钟、每 N 小时、时段内按间隔。没有 cron。
- **upsert 是整对象覆盖，没有版本检查**；webhook 令牌下发时换成 `<redacted>`，upsert 时按位置换回真令牌。

### 桌面：整条链路只认本机

- `main.rs` 的事件循环里，远程机器首帧里的自动化只 `save_state`，远程的 `Automations` 增量直接丢掉。
- 自动化投影存在全局 `AgentHostState`（`crates/smelt-ui/src/agent_host_state.rs`）里，一份单例：
  `automations`、`automation_store_id`、`automation_revision`、`automation_states`、`automation_runs`、
  `automation_store_error`、`webhook_base_url`，不带机器。对照：智能体配置已经是
  `AgentHosts { machines: BTreeMap<RuntimeKey, …> }`。
- 写操作 `submit_automation_command`（`agents/workspace.rs`）固定发给 `runtimes::local()`；运行详情
  `automation_run_detail.rs` 用本机的 `runtime_call::call`；运行现场 `attach_automation_run_session` 用
  `RuntimeKey::Local` / `Runtime::local()`。
- 编辑页、列表、默认智能体都只取 `agents_on(&RuntimeKey::Local)`；`agent_automation_count`、
  `missing_fields`、删智能体时关联自动化的检查也只看本机。
- 通知标识 `smelt.automation.run.{run_id}`、跳转 `JumpToAutomationRun{automation_id, run_id}` 不带机器；
  终态提醒去重按 `session_id` 比，不看机器。
- 导航 `AutomationsView::List / Editor`（`workspace_nav.rs`）里的自动化只用 `id` 指。

### 手机：只能看、开关、跑一次

- 内核 `api/catalog.rs` 只有 `automation_catalog`、`set_automation_enabled`、`run_automation_once`；
  `AutomationSummary` 里没有提示词、智能体 id、Shell 命令、sinks。
- 界面 `features/agents/agents_page.dart`（列表）、`automation_detail_page.dart`（详情，底部写着
  「自动化在电脑上编辑」），人话文案在 `automation_text.dart`。
- 没有时间选择组件。#78 的智能体编辑页（`agent_edit_page.dart`）可以参考：先取编辑器（草稿 + 可选项），
  保存后刷新列表，离开时有未保存确认。

---

## 2 · 桌面（PR1）

### 2.1 投影按机器存

`AgentHostState` 里的自动化字段拆进按机器的表：

```text
AgentHostState
  automations: BTreeMap<RuntimeKey, MachineAutomations>
MachineAutomations
  store_id, retired_store_ids, revision, automations, states, runs, store_error, webhook_base_url
```

- `replace_automation_snapshot(runtime, …)` 按机器判断新旧、退休旧 store，互不覆盖。
- 读的地方（列表、编辑页、导航提示、通知比对）都按机器取。列表、侧栏「自动化」上的失败标记看所有机器。
- 机器被移除时丢掉它的那份。

### 2.2 事件不再丢远程

`main.rs` 里远程的首帧和增量都带着 `runtime` 走 `apply_automation_projection`。通知比对（新旧 runs）
按机器各算一份；首帧不重放历史这条规则对每台机器各自成立。

### 2.3 自动化按「机器 + id」指

- 新类型 `AutomationRef { runtime, id }`，编辑器 `AutomationEditor`、导航 `AutomationsView::Editor`、
  运行详情、行菜单都用它。
- 写操作 `submit_automation_command` 带上机器：主线程 `runtime_call::target` 取客户端，后台
  `smelt_client::session_ops::submit_automation_command(&client, …)`。继续用 `automation_command`：它有
  `CancelRun`，返回整份快照，桌面保存后立刻有新投影，不用等推送；控制方法这边没有取消运行。
- 运行详情、运行现场（`AcpView`）用那台机器的客户端和 `Runtime`。

### 2.4 列表和新建

- 和智能体列表一样：多于一台机器时按机器分组，加机器标题；离线、读失败的机器也列出来并写状态。
- 「新建」菜单按机器分段（照 `fill_create_agent_menu`），模板卡片也先选机器（只有本机时不问）。
- 编辑页的智能体选项、默认智能体取 `agents_on(&editor.runtime)`；`agent_automation_count`、`missing_fields`、
  删智能体时关联自动化的检查都按智能体所在的机器。
- Webhook 地址用那台机器自己推来的 `webhook_base_url`。

### 2.5 通知

- 通知标识编进机器（格式见第 5 节），`JumpToAutomationRun` 带上 `runtime`，点进去打开那台机器上的运行；
  远程运行的通知标题前写上机器名。按机器撤回通知：一台机器的快照只撤它自己的运行通知。
- 终态提醒去重按「机器 + session_id」比。

### 2.6 测试

- 两台机器各推一份同 id 的自动化，投影互不覆盖；一台换 store 不影响另一台。
- 远程首帧不重放历史通知，远程运行结束会通知；通知标识带机器，解析回来能跳到对的机器。
- 写操作发给编辑器所在的机器（用假运行时记录请求）。

---

## 3 · 手机（PR2）

### 3.1 内核

新模块 `crates/smelt-mobile/src/automations.rs` + `api/automations.rs`，写法同 #78 的 `agents`：

| 接口 | 背后 | 说明 |
|---|---|---|
| `automation_editor(runtime_id, id?)` | `automation.get` + `agent_definition.list` | 返回 `{draft, agents, event_topics}`；`id` 为空给新建的默认草稿（每天 09:00、第一个智能体） |
| `save_automation(runtime_id, id?, draft)` | （编辑时 `automation.get` → 合并）→ `automation.upsert` | 新建时内核生成 id；返回保存后的目录 |
| `delete_automation(runtime_id, id)` | `automation.delete` | 返回删除后的目录 |

不做「接下来几次运行时间」的预览：调度按那台机器的时区算，手机和机器可能不在同一个时区，算出来会错。
保存后详情页显示运行时给的下次运行时间。

- **整对象覆盖下不丢字段**：upsert 没有 patch。更新时先 `get` 拿完整对象，只把表单里有的字段换上去
  （名称、动作、定时规则、webhook 开关、通知），`events` 等手机不编辑的字段原样带回；webhook 令牌保持
  `<redacted>`，运行时按位置换回。
- 校验用 `smelt-model` 新加的 `Automation::validate_for_save`（`validate` + 定时的智能体动作要有指令，和
  运行时收到保存时的规则一致）。它的错误文案只有中文，所以表单上能当场看出来的（名字空、没有触发、
  缺智能体 / 指令 / 命令）由 Dart 按 l10n 提示「还差：…」并禁用保存，`validate_for_save` 只做最后一道关。
- 草稿 `AutomationDraft`：`name`、`action_kind`（智能体 / 命令）、`agent_definition_id`、`prompt`、
  `command`、`schedules: Vec<ScheduleDraft>`（沿用 `AutomationSummary` 那种扁平结构：kind + 小时 / 分钟 /
  间隔 / 星期位掩码 / 时段）、`webhook: bool`、`notify: bool`。

### 3.2 界面

- 自动化列表加「新建」；没有可用智能体时说明并引导去建智能体。
- 详情页加「编辑」和「删除」（二次确认，说明运行历史随之删除）；去掉「自动化在电脑上编辑」那句。
- 编辑页 `automation_edit_page.dart`：
  - 名称；动作（智能体 + 提示词的多行编辑器 / 命令）；
  - 定时规则可多条：类型用分段按钮（每天、每周、每隔、时段内），时间用系统的时间选择器
    （`showTimePicker`，iOS 上是滚轮），星期用一排可多选的标签，间隔用步进 + 单位；
  - Webhook 开关。地址里带密钥，运行时下发时已隐去，手机上不显示，说明去电脑上复制；
  - 通知开关。
  - 保存后刷新列表、返回；有未保存改动离开时确认（同智能体编辑页）。
- 从 `agent_edit_page.dart` 把输入框和「添加」行提成 `agents_widgets.dart` 里的 `EditField`、`AddRow` 共用。

### 3.3 不做

- 编辑事件触发（`events`）：桌面也还没有编辑入口，手机只读显示。
- 自动化运行结果推送到手机（运行时的推送订阅现在不含自动化），另开 issue。

### 3.4 测试

- 内核：用假运行时验证编辑时先 `get` 再整对象 `upsert`、手机不编辑的字段原样带回、令牌不被替换；新建
  时缺指令不发；表单和定时规则互转。
- Flutter：新建、编辑、删除确认、未保存离开确认、时间选择后草稿正确。

---

## 4 · 拆分

| PR | 内容 | 依赖 |
|---|---|---|
| PR1 | 桌面（第 2 节） | 无 |
| PR2 | 手机（第 3 节） | 无，可与 PR1 并行 |

## 5 · 风险与取舍

- **并发编辑不处理**：upsert 整对象覆盖、没有版本检查，两端同时改同一条时后保存的赢。正常用户不会同时在
  多端操作同一条自动化，不为此加版本检查（2026-11 定）。
- **通知标识**：本机运行沿用老格式（`smelt.automation.run.{run_id}`），已经挂在通知中心里的旧通知点了照样能
  跳；远程运行用 `smelt.automation.remote-run.{运行时 id}.{run_id}`，线程标识 `automation@{运行时 id}:{id}`。
