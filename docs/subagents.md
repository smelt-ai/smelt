# Pi 子智能体：独立实体、子会话落盘、对话外查看（设计，施工中）

## 要解决的问题

现在的 Pi 子智能体（`packages/pi-agent/src/subagent`）有四个毛病，根子是同一个：它没有自己的模型，后台运行时借的是「挂接的后台任务」，前台运行时只活在一次工具调用的进度里。

1. **跑完什么都不剩。** 子进程用 `--no-session`，父会话的工具结果又只留结果（`finalSubagentDetails` 去掉了 `messages`）。子智能体默认能用全部工具，项目受信时还带 `--approve`，它改了哪些文件、跑了哪些命令，事后只剩它自己写的总结。
2. **后台跑的看不到过程。** 转后台后工具调用立刻返回 `results: []`，进度只以纯文本写进宿主的任务日志（`hostTask.progress(text)`），卡片里没有子行。单任务和并行默认都在后台。
3. **卡片状态是猜的。** 卡片的「失败」只看工具调用自己的状态，后台派发那次调用总是「完成」，子智能体失败或被停止，任务结束后卡片照样打勾。「还在后台跑」靠 `background_task_running_for` 按 `tool_call_id` 去后台任务列表里找；「有 `tool_call_id` 的后台任务就是子智能体」是个隐含约定，全仓库只有一处挂接，碰巧成立。
4. **过程会被新对话冲走。** 后台子智能体本来就是让这一轮先结束、用户接着聊别的。它的卡片留在派发的位置，越聊越远。

业内对照（Claude Code、Codex、OpenCode、Roo Code、PI-Desktop、pi-subagents）：父模型上下文只进结果，子智能体的过程一律落盘、能回看、多数能续跑。只有 Pi 官方的最小示例扩展用 `--no-session`，smelt 是从它继承来的。

## 决定

- **子智能体是独立实体。** 前台、后台、并行里的每一个、串联里的每一步，都是一条子智能体记录，有自己的 id、状态和子会话文件。它不再是后台任务，后台任务列表里不再出现它。
- **一次 `subagent` 工具调用是一次派发。** 派发是投递单位：后台派发跑完，整批结果作为一条消息交回会话。子智能体是查看和停止的单位。
- **子会话单独落盘，父会话只存结果和 id。** 父模型上下文不变：只进结果。父会话的 JSONL 不存子会话的任何过程。
- **状态只认显式 id。** 卡片、底部入口、侧边查看都按子智能体 id 或派发的 `tool_call_id` 查，不靠「某个字段有没有值」推断类别。
- **界面分三处：** 对话里一行卡片标明派发位置；输入栏「N 个子智能体」入口列出正在跑的；点开在对话自己的侧边视图里看过程。

不在这次范围：续跑（按 id 接着给某个子智能体发话）、会话编排（Worker）、手机端界面。数据和接口按能支撑它们来设计。

## 存储

### 子会话文件

```text
~/.smelt/subagents/<子智能体 id>.jsonl      目录 0700，文件 0600
```

- 子 Pi 进程用 `--session <这个路径>` 启动（Pi 1.1 的 `--session` 接受文件路径），不再用 `--no-session`。文件就是 Pi 原生的会话格式，由子进程自己写，每条消息结束时落一次。
- **不放在 Pi 的会话目录里。** smelt 的历史列表会递归扫 Pi 会话目录下所有 `*.jsonl`、只按头部的 `cwd` 过滤（`session_history.rs` 的 `walk_pi_session_files`），放进去每个子会话都会冒成一条独立历史。Pi profile 还会改会话目录（`PI_CODING_AGENT_DIR`），放在 `~/.smelt` 下由运行时统一管，不跟 profile 走。
- **路径只按子智能体 id 定，不按父会话。** 父会话的 Pi 会话 id 会变：对话内回退（Pi `fork(entryId)`）换一份新文件，桌面「从这里分叉」用 `--fork` 复制出新会话。复制出来的记录里引用的还是同一批子智能体 id，按 id 找文件不受影响。
- 子智能体 id 由会话宿主分配（`sa-` 加 12 位十六进制），文件路径也由宿主算好交给扩展。扩展不自己拼 `~/.smelt` 路径。

### 引用表（运行时库）

子会话什么时候能删，取决于还有没有会话记录引用它。分叉之后两份父会话引用同一个子智能体，所以要记引用，不能跟着某一份父会话删。

```sql
CREATE TABLE subagent_ref (
    pi_session_id TEXT NOT NULL,   -- 引用它的 Pi 会话 id（父会话，或者分叉/回退出来的会话）
    subagent_id   TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (pi_session_id, subagent_id)
);
CREATE INDEX subagent_ref_by_subagent ON subagent_ref(subagent_id);
```

- 运行时库加一个版本（v23 → v24），按现有规矩写迁移。
- 只有 smeltd 写这张表（`docs/local-storage.md`「谁写」）。会话宿主不碰 SQLite：子智能体一登记就出现在会话状态的 `subagents` 列表里（见下），smeltd 镜像状态时看到新 id 就插一行引用。
- **分叉和回退复制引用。** smeltd 看到某段对话的 `history_session_id` 从 A 变成 B（回退），或者新对话带着 `--fork A` 启动、报出自己的 B，就把 A 的引用复制给 B。实现时确认这两个变化在 smeltd 里各自从哪里观察到。
- **删除。** `history.delete` 删掉一份 Pi 会话文件时，删掉这份会话的引用；某个子智能体没有引用了，就删它的文件。`session.delete` 本来就不删 Pi 记录，子会话也不删。
- **孤儿文件。** 登记后、smeltd 插引用前崩溃，会留下没有引用的文件。smeltd 启动时清掉没有引用、且最后修改超过 7 天的子会话文件。

不另建「子智能体运行记录」表：标题、状态、结果在父会话 JSONL 的工具结果和结果消息里，过程在子会话文件里，运行中的状态在会话宿主内存里。再存一份只会出现两个来源。

## 运行时

### 扩展（`packages/pi-agent`）

**一次派发：**

1. 每次 `subagent` 工具调用向会话宿主开一条派发连接：`subagent_dispatch { owner, toolCallId, background, title }`。连接一直保持到这次派发结束；连接断了而没有 `finish`，宿主把其中还在跑的子智能体记成「已中断」，没有结果可交。
2. 每起一个子智能体，在这条连接上登记：`{"op":"start", ref, parentSessionId, title, agent, model}`。`parentSessionId` 取 `ctx.sessionManager.getSessionId()`（Pi 1.1 的 `ReadonlySessionManager` 有）。宿主在同一条连接上回 `{"op":"started", ref, id, sessionFile}`。
3. 用 `--session <sessionFile>` 起子进程，其余参数不变（`--exclude-tools subagent` 等）。
4. 运行中按 500ms 限频上报 `{"op":"progress", id, turns, toolCalls, lastActivity}`。`lastActivity` 是最近一次工具调用的标题，给底部入口显示「正在做什么」。不再带消息窗口：过程以子会话文件为准。
5. 每个子智能体结束上报 `{"op":"end", id, status}`，`status` 是 `completed | failed | stopped`。
6. 整次派发结束上报 `{"op":"finish", text}`。后台派发的 `text` 就是交回会话的结果；前台派发不交回（结果已经是工具返回值）。
7. 宿主可以随时写 `{"op":"stop", id}`（停一个）或 `{"op":"stop"}`（停全部），扩展中止对应的子进程。

前台、后台、并行、串联都走这一套，区别只在工具调用什么时候返回。

**工具结果（写进父会话 JSONL）：**

```ts
details: {
  mode: "single" | "parallel" | "chain",
  background: boolean,
  runs: Array<{
    id: string;            // 子智能体 id，打开侧边视图、找状态都用它
    title: string;
    agent: string;
    model?: string;
    status: "running" | "completed" | "failed" | "stopped";
    usage: Usage;
    output: string;        // 前台定稿时是截断后的结果；后台启动时为空
  }>
}
```

- 前台：工具调用返回时 `runs` 已是最终状态。
- 后台：立刻返回，`runs` 里是已登记的子智能体，状态 `running`。并行时每个子智能体的 id 在启动前就分好，返回时全部带上。

**后台结果消息：** 一次后台派发跑完发一条 `smelt.subagent_result`：

```ts
details: {
  toolCallId: string,      // 对应哪次派发
  runs: Array<{ id; title; status }>,
  outputTail: string,
}
```

送达走现有的可靠投递：`listen` 连接、宿主等 Pi 的 `message_start` 才算送到、`clear_queue` 后静默重发、回退后交给新会话的监听。这段代码保留，只是它投递的对象从「后台任务」变成「后台任务或后台派发」。

**给模型的停止：** 新工具 `subagent_stop { ids?: string[] }`，不传 id 就停掉本会话所有在跑的。`background_task_stop` 不再管子智能体。

**不在 Smelt 里时（没有宿主 socket）：** 退回 `--no-session`，进程内编号。这是给测试和单独跑 Pi 用的，界面上不存在这种情况。

### 会话宿主（`smelt-core`）

- 派发和子智能体两份记录，和后台任务并列，**共用同一个 socket 服务和同一套投递**（一个是传输，一个是投递，模型各是各的）。
- 删掉 `attach` 操作和 `LiveTask` 里只为它存在的字段（`attached`、`result`、`attach_poke`、`tool_call_id`），删掉 `BackgroundTaskView.tool_call_id`。全仓库只有子智能体在挂接。
- 会话状态加一个字段，和 `background_tasks` 并列，走快照信封：

```rust
pub struct SubagentView {
    pub id: String,
    pub tool_call_id: String,
    pub parent_session_id: String,   // smeltd 据此写引用
    pub title: String,
    pub agent: String,
    pub model: Option<String>,
    pub background: bool,
    pub status: SubagentStatus,      // Running | Completed | Failed | Stopped | Interrupted
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub turns: u32,
    pub tool_calls: u32,
    pub last_activity: Option<String>,
    pub revision: u64,               // 子会话每多一条消息加一，侧边视图据此刷新
}
```

  列表里留着本次宿主生命期内结束的子智能体，卡片靠它拿最终状态。宿主退出后列表随之消失，重启后的状态从父会话记录里恢复（见「卡片状态」）。
- 新增用户操作 `AcpUserAction::StopSubagent { id }`，和 `StopBackgroundTask` 同一条路走到宿主。

### 读取子会话（Control API）

新增守护进程操作：

```text
subagent_transcript { session_id, subagent_id, before?, limit? }
  → { entries: Vec<AcpEntry>, revision, has_more }
```

- 校验：`subagent_id` 必须被这段对话当前的 `history_session_id` 引用（查 `subagent_ref`）。不能拿任意 id 读任意文件。
- 解析复用 `pi_rpc` 的历史回放（`replay_messages`），子会话里的工具调用、输出和普通对话一样显示。
- 桌面（本机和远程运行时）、手机都走这个操作，界面不直接读文件。

## 界面

### 哪张卡片是 Pi 子智能体：显式字段

`AcpEntry::ToolCall` 加一个字段 `subagent_ids: Vec<String>`。Pi 驱动（`pi_rpc`）在 `subagent` 工具的开始、结束和历史回放时，从 `details.runs[].id` 填进去。非空就按下面的子智能体卡片画。

ACP 智能体（Claude Code、Codex 经 ACP）自己的子智能体卡片不变：它们由 `acp_conn` 产出 `ToolChildren`，过程本来就在它们自己的记录里，这次不动。所以 `ToolChildren` 和嵌套渲染保留，只删 Pi 这一路的产出。

### 卡片状态：只认 id

卡片按 `subagent_ids` 和派发的 `tool_call_id` 依次查：

1. 会话状态 `subagents` 里有这个 id → 用它（运行中、刚结束都在这里）。
2. 父会话里有 `toolCallId` 对得上的 `smelt.subagent_result` → 用里面的状态。
3. 工具结果本身是定稿状态（前台跑完的）→ 用它。
4. 都没有 → **已中断**（宿主在它跑完前退出了）。

删掉 `background_task_running_for` 和卡片里「工具调用完成就打勾」的判断。失败、已停止、已中断各有自己的样子，不会再出现失败还打勾。

### 对话里：一行卡片

```text
 ◆ Antigravity follow-up review   运行中 · 12m 03s · 正在：运行了 rg …   [查看]
 ◆ 3 个子智能体   已完成 1/3 · 4m 10s   [查看]
```

- 只占一行，不再在对话里展开子会话过程，前台和后台一样。它的作用是标明「在这里派出去的」。
- 并行时这一行可以展开成每个子智能体一行（名称、状态、耗时、「查看」），不展开过程。
- Pi 子智能体自己的工具调用不再以 `ToolChildren` 出现在父会话里，`pi_rpc` 的 `subagent_children_from_details` 删掉。

### 输入栏：「N 个子智能体」

```text
 [● 2 个子智能体] [● 1 个后台任务]
```

- 和「后台任务」并列，各自计数，没有就不显示。两个弹层互斥，和用量弹层也互斥。
- 弹层每行：名称、状态、耗时、`last_activity`、停止。点一行打开侧边视图。底部「全部停止」。
- 只列运行中的。结束的看对话里那一行卡片。

### 侧边视图：跟对话走

- 放在对话视图（`AcpView`）自己的右侧，是它的一部分，所以天然跟对话走：切到别的对话就是别的对话的侧边视图。右侧工具面板（文件树、变更、历史、插件）按项目走（`ProjectUiState`），不放那里。
- 内容：子智能体的标题、状态、模型、耗时，下面是子会话的对话条目，只读，没有输入框，运行中有「停止」。
- 打开时和 `revision` 变化时取一页 `subagent_transcript`，结果缓存在视图状态上；render 只读缓存（AGENTS.md「render 只读内存」）。
- 开着哪个子智能体、宽度，不落盘：重开对话时侧边视图是关的。
- 具体用哪个 gpui-kit 组件、宽度和动效，实现前按 gpui-kit 设计规范定。

### 手机

这次不做界面。`ConversationView` 加上 `subagents` 列表，`subagent_transcript` 和停止操作手机也能调，界面下一步做。在那之前手机继续显示结果状态行（`TaskNote`）。

## 删除的东西

- 宿主在时子进程的 `--no-session`。
- 后台任务的 `attach` 操作及其字段，`BackgroundTaskView.tool_call_id`，`background_task_running_for`。
- 进度里的消息窗口（`progressMessage` / `progressWindow`），`pi_rpc` 里从子智能体 `details` 生成 `ToolChildren` 的链路。ACP 智能体的 `ToolChildren` 保留。
- Pi 子智能体卡片里按工具调用状态打勾的逻辑。

## 测试（先写，先失败）

扩展（`bun test`）：

- 宿主在时，子进程参数带 `--session <宿主给的路径>`，不带 `--no-session`。
- 前台、后台、并行、串联，每个子智能体都登记一次，结束都上报一次 `end`，派发结束上报一次 `finish`。
- 后台启动的工具结果带全部 `runs[].id`，状态都是 `running`。
- 并行后台批次只发一条结果消息，带 `toolCallId` 和每个子智能体的状态。
- `subagent_stop` 只停自己会话的子智能体；不传 id 停全部。

会话宿主和状态（`cargo test`）：

- 登记后 `subagents` 出现一条，`progress` 更新计数和 `revision`，`end` 定状态。
- 派发连接断开而没有 `finish` → 还在跑的子智能体 `Interrupted`，没有结果交回。
- `StopSubagent` 让扩展收到 `stop`，最终状态是 `Stopped`。
- 后台派发的结果经 `listen` 交回，ack 前 `clear_queue` 会静默重发。
- 后台任务列表里不再出现子智能体。

界面（`cargo test`，`acp_view/tests.rs`）：

- 后台子智能体失败后，卡片显示失败，不打勾。
- 重新加载后：有结果消息的显示结果消息里的状态；没有的显示「已中断」。
- 输入栏子智能体和后台任务分开计数。
- `pi_rpc` 在开始、结束、历史回放时都给 `subagent` 工具调用填上 `subagent_ids`；ACP 智能体带 `ToolChildren` 的卡片照旧显示。

smeltd：

- 新子智能体出现在状态里时写一行引用。
- 回退和分叉后，新会话继承引用；`subagent_transcript` 用新会话能读。
- `history.delete` 删掉最后一个引用时删子会话文件，还有别的引用时保留。
- 读不属于这段对话的子智能体 id 被拒绝。
- 没有引用且超过 7 天的文件在启动时清掉。

## 实施顺序

每块验证通过就提交。

1. 会话宿主：派发和子智能体记录、`subagents` 状态、`StopSubagent`；删 `attach`。
2. 扩展：派发连接、`--session`、新的 `details` 和结果消息、`subagent_stop`。1 和 2 要一起提交，中间状态不能用。
3. smeltd：`subagent_ref` 表和迁移、写引用、分叉/回退复制、删除和孤儿清理、`subagent_transcript`。
4. 界面：卡片状态、一行卡片、输入栏入口和弹层、侧边视图。
5. 手机 API：`ConversationView.subagents`。

## 待定

- 侧边视图的组件形态：对话内可调宽的右侧分栏，还是浮在对话上的侧边抽屉。按 gpui-kit 设计规范定。
