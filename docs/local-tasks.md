# 本地任务列表

**先做本机任务编排，再挂远程 / 看板 / 飞书。**  
产品仍是驾驶舱「从想法到 agent」，不是 Jira / Linear 替代品。

**UI：**
- 侧栏「任务面板」：任务总览入口 · 新建 · 执行中快捷项
- 右侧 Inspector 的 **TASK** tab（位于 GIT 与 SKILL 之间）：仅显示当前项目及其子目录的任务，可直接新建、运行或打开
- 主区 **任务总览**页（状态看板：待办、执行中、遇到阻碍、待确认、已完成分列；可拖动卡片跨列改状态，状态 pill 可筛选）
- 会话「总览」只做会话监控，**不**列任务

**Agent 绑定（当前阶段）：** `Task` 只保存目标、项目和调度信息，不保存 Agent、
启动命令或执行通道。实际运行时才使用当前默认启动项，并把实际命令和通道写入
`TaskRun` 作为执行记录。

**执行目标（运行时选择）：** 普通「运行」走 TUI 终端；任务卡上的 **ACP** 菜单会按
当次选择的 Agent 新开一个独立 ACP 对话，或把首包发送到一个空闲的已打开 ACP 对话。
指定时间任务和自动续跑保持走默认 TUI，不会在后台替用户选择 ACP Agent。

**TUI 开跑：新开终端 + startup-arg**

```text
新开 smeltd 会话
  launch = `<base> "$(cat ~/.smelt/tasks/prompts/<id>.txt)"`
  例：claude --dangerously-skip-permissions "$(cat '…/id.txt')"
  column = Running
  agent 标题 spinner 消失 → column = Done
```

- **交互**会话，侧栏可见、可接管  
- **不是** `claude -p` 无头批跑  
- **当前终端上下文**：侧栏会话/分屏行右键，或终端区域右键（TUI 未抢鼠标时）→「新建任务」→ 开跑时键入+回车进该会话

配套：[collaboration.md](collaboration.md)、[remote-ops-roadmap.md](remote-ops-roadmap.md)、[roadmap.md](roadmap.md)。

---

## 目标

| 能力 | 本地版含义 |
|------|------------|
| **新建** | ⌘⇧N / 侧栏「新建任务…」：执行频率（按队列/仅一次/每小时/每天）、标题、首包 prompt、项目 cwd；不绑定 Agent |
| **创建并继续** | 主按钮；创建后保持弹窗、清空输入与图片，焦点回首包框，连续批量建；「取消」才关闭。「创建并运行」为次级按钮 |
| **图片** | 弹窗 ⌘V 粘贴截图/图片（缩略图条可逐张删除）；随首包发给 agent——ACP 通道内嵌 base64，终端通道落盘 `~/.smelt/tasks/images/` 并提示 agent 用 Read 查看 |
| **执行频率** | 选「仅一次」「每小时」或「每天」+ 首次本地执行时间；到点由后台扫描原子认领并启动。重复任务会按原计划时间滚动到下一次 |
| **运行** | 待办点「运行」：有执行中的会话则注入，否则按当前默认启动项新开终端 + 首包参数；**ACP** 菜单可在新 ACP 对话中执行 |
| **打开** | 执行中/完成：切到已绑的 TUI 终端或 ACP 对话 |
| **做完续跑** | 绑定任务 Done 后，同 cwd 由 smeltd 原子 claim 下一条 **`auto_run` 待办** 并后台启动 |
| **自动执行** | 顶部全局「自动认领中 / 暂停」控制后续领取；任务级字段决定任务能否被领取 |
| **自循环（方向）** | agent 写 TaskStore 塞队（`auto_run`）→ 完成边沿 drain → 同一套运行时启动契约 |

**不做定位：** 完整项目管理、云端任务库、`-p` 无头批跑、任意 cron 表达式。

---

## 数据模型

```text
Task {
  id,
  title,               // 给人看的侧栏名（可空→用 body 首行）
  body,                // 给 agent 的首包（开跑唯一进 CLI 的内容）
  column,              // 待办 | 执行中 | 待审查 | 失败 | 完成
  project_cwd,         // 在哪跑
  session_id?,         // 实际执行后关联的 smeltd 会话
  current_run_id?,     // 当前/最近一次执行
  kind,                // once | scheduled（缺省 once，兼容旧数据）
  schedule_frequency,  // once | hourly | daily（缺省 once，兼容旧数据）
  run_at?,             // Unix 秒；scheduled 时的下一次开跑时间
  auto_run,            // 是否允许系统自动开跑（缺省 true）；false = 仅手动
  created_at, updated_at,
}

TaskRun {
  id,
  task_id,
  attempt,             // 第几次尝试
  channel,             // 本次实际执行的通道快照
  launch,              // 本次实际执行的启动命令快照
  session_id?,
  status,              // starting | running | completed | failed | cancelled
  error?,
  created_at, started_at?, finished_at?,
}
```

`Task` 表示用户要完成的目标，`TaskRun` 表示一次具体执行尝试；重试不会覆盖上次失败记录。
agent 从 Running 变 Idle 时，本次 Run 标 `completed`；单次任务进入「待审查」，重复任务回到
「待办」并写入下一次执行时间。旧任务中的 `launch` / `channel` 字段会被兼容读取但不再保留，后续保存时会移除。

落盘：`~/.smelt/tasks.json`（`tasks` + `runs`）；首包文件：
`~/.smelt/tasks/prompts/<id>.txt`（内容 = body）。旧文件没有 `runs/current_run_id` 时按空值兼容读取。

**全局行为：** 任务面板顶部的「自动认领中 / 暂停」持久化到工作区。暂停时不领取新任务，
但不会打断已运行任务；新建及旧工作区未保存该设置时默认暂停。

**指定时间执行：** 每 30s 扫描可运行候选，并由 smeltd 原子领取
`auto_run && scheduled && 待办 && run_at<=now` 的任务；同 cwd 维持串行。执行频率为：

- **仅一次**：执行后进入待审查，保持既有单次任务流程。
- **每小时**：从首次执行时间起，按整小时周期执行。
- **每天**：从首次执行时间起，每天在同一当地时刻执行（DST 歧义取较早时刻；不存在的当地时刻跳过）。

重复任务按原计划时间计算下一次；应用离线、项目被占用或一次执行过久造成的错过周期会跳过，不会连续补跑。

**Agent CLI：** `smelt-task add` 可用同一套规则创建指定时间任务：

```sh
smelt-task add --cwd . --body "检查并整理日报" \
  --schedule "2026-08-08 09:30" --frequency daily
```

`--frequency` 可选 `once`、`hourly` 或 `daily`，且必须与 `--schedule` 一起使用；未指定频率时保持单次执行。

**做完自动续跑：**

```text
spinner 落下（Running→Idle）
  → 当前 TaskRun → Completed
  → 单次任务 → Review；重复任务 → Backlog + 下一次 run_at
  → smeltd 原子 claim 同 project_cwd 下一条 auto_run 待办（FIFO / created_at）
  → 后台启动新终端 + startup-arg（不抢当前焦点）
```

- 仅当本 session **确实收尾了任务** 才续跑
- 同 cwd **串行**；`auto_run=false` 的待办不会被 claim（仍可手动「运行」）
- 指定时间任务创建时强制 `auto_run=true`
- GUI 与 `smelt-task run` 共用 smeltd 的原子 claim，不会重复领取同一条任务

---

## 分阶段

| 阶段 | 交付 | 验收 |
|------|------|------|
| **T0** ✅ | `Task` + 全局 store + 单测 | 重启不丢 |
| **T1** ✅ | 侧栏入口 + 主区任务总览页 + 新建弹窗 | 全量可管 |
| **T2** ✅ | 终端开跑（运行时默认启动项 + startup-arg 首包） | agent 自动开干 |
| **T2.5** ✅ | 执行频率：按队列、仅一次、每小时、每天（`run_at` + 扫描） | 到点自动开跑并按频率续排 |
| **T2.6** ✅ | 完成边沿 → 同 cwd 自动 claim 下一条 | 队列串行续跑 |
| **T3** | 会话「钉成任务」、右键删/改状态 | 双向不割裂 |
| **T4+** ✅ | agent CLI 塞队 / 状态通道（`smelt-task` add/list/done/remove/show/run、`task_session_done`/`task_session_failed` 状态边沿） | 真自循环 |
| **T5** | 远程 / 飞书卡片 | 手机上看任务、IM 里处理 |

---

## 与远程操作

远程看/控的是 **session**；任务列表负责 **何时**产生并记住该 session，启动项在执行时才解析。
