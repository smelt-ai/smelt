# 会话跨 agent 迁移方案

> 当前策略（2026-08-20）：只保留历史会话页的「迁移到」。活体 ACP 回答气泡不再
> 提供继续/迁移入口，也不再把完整 transcript 复制到 `~/.smelt/handoffs`；目标会话
> 只接收受预算约束的摘要。下文保留早期方案的调查背景，实施表标明已下线部分。

把一段已归档的对话，从一家 agent 交到另一家手上继续干活：
Claude → Codex、Grok → Claude、Claude 默认 workspace → Claude Quant profile。

配套 [workspace.md](workspace.md) 的会话模型。这份讲「为什么只能这么做」和「改哪些函数」。

---

## 历史背景：活体会话曾有同 agent 交接骨架

这不是从零开始的功能，仓库里已经存在一条**同 agent** 的交接链路，读代码确认：

| 环节 | 位置 | 现状 |
| --- | --- | --- |
| 触发入口 | `acp_view.rs:2561` 「在新会话中继续」按钮 | 只挂在最终回答气泡上，agent 写死 `this.agent` |
| 交接文本 | `build_handoff_prompt()` `acp_view.rs:4435` | 24k 字符总预算、单条 4k 截断、**从尾部倒序装填**保留最近上下文 |
| 请求结构 | `AcpHandoffRequest` `acp_view.rs:220` | `agent` / `launch` / `profile_id` / `config_values` 全部照抄原会话 |
| 建会话 | `add_acp_handoff_session()` `main.rs:3714` | 标题固定「继续：<原标题>」 |
| 首条派发 | `start_with_handoff_sid()` `acp_view.rs:457` → `pending_initial_prompt` `acp_view.rs:1512` | 握手完成后自动发出，用户无感 |

历史会话侧则完全没有跨 agent 入口：`session_history.rs` 四家读取器统一产出
`SessionSummary`/`Turn`/`SessionDetail`，右键只有「ACP 继续」（`session/load`，同 agent）
和「CLI/TUI 继续」（`--resume`，同 agent）。

**协议层不给第二条路**：ACP 的 `session/load` 只认目标 agent 自己 session store 里的 id
（`acp_conn.rs:836` 的注释已经把这条写死：冷恢复只有 `session/load` 一条路径）。
协议里没有「导入一段外部历史」的方法。所以跨 agent 迁移只有两种物理实现：
**把历史变成 prompt**，或者**把历史伪造进目标 agent 的私有存档**。

---

## Grok 侧调查（决定第二种实现是否可行）

`~/.grok/bin/grok` 是 Mach-O arm64 闭源二进制，没有源码可读，以下是 CLI 接口
与磁盘格式的实测事实。

**存档形状**（`~/.grok/sessions/<urlencode(cwd)>/<uuid>/`）：

```
chat_history.jsonl   events.jsonl      updates.jsonl     rewind_points.jsonl
hunk_records.jsonl   summary.json      system_prompt.txt prompt_context.json
resources_state.json signals.json      plan_mode.json    terminal/ recap_requests/
```

外加 `sessions/session_search.sqlite`（FTS5 虚表 + 三个同步触发器）和
`~/.grok/active_sessions.json`（session_id/pid/cwd/opened_at）。
`summary.json` 里有 `chat_format_version` / `next_trace_turn` / `request_id` 这类内部字段。

**官方接口里对迁移有用的**：

- `grok export <SESSION_ID> [OUTPUT]` —— 直接导出 Markdown transcript，**不用自己解析
  `chat_history.jsonl`**，是现成的高保真交接原料。
- `grok --session-id <UUID>` —— 给**新**会话指定 UUID（要求该目录下尚不存在），
  意味着 smelt 可以在建会话前就掌握 id，迁移后的会话天然可追踪、可续接。
- `-r/--resume [ID|TITLE]`、`--fork-session`、`--restore-code`。
- `-p/--single` + `--output-format streaming-json`（NDJSON of agent native ACP updates），
  headless 生成交接书用得上。
- `grok agent stdio` —— 就是 smelt 现在接的 ACP 通道（`agent_kind.rs:109`）。

**结论**：往 Grok 里伪造一个可 resume 的会话，要写 6+ 个私有文件、维护 FTS 索引、
猜对内部计数字段。而 `--session-id` + prompt 注入是官方接口。第二种实现在 Grok 上
性价比极低——这一条同时也是整个「存档转写」路线的否决证据。

---

## 业界怎么做

- **authsec-bridge**：读源 CLI 的会话，转成目标 CLI 的 schema 落盘，然后走目标原生
  `--resume`。体验最无缝，代价是把私有格式当 API 用。
- **AgentsRoom multi-provider**：切 provider 时构造 handoff context（改动文件清单、
  会话活动、可选由当前 agent 亲手写的详细摘要），作为新 provider 的首条 prompt。
  ——**和 smelt 现有 `build_handoff_prompt` 同构**。
- **Claude Session Handoff / Migrate Session 之类 skill**：本质是「让 agent 写一份
  交接文档存文件，下一个会话读它」。
- **协议层没救**：MCP/A2A/AAIF 解决的是工具调用与 agent 定义的互操作，没有一个定义
  「会话逐字可移植」；而且各家正在往 encrypted reasoning trace / 服务端保留历史走，
  本地存档越来越只是个指针。

行业共识很清楚：**语义交接是主干且唯一稳的路；存档转写是脆弱的加速道。**
下面的方案照这个共识分层。

---

## 实现进度

**MVP + 二期都已落地**（活跃会话迁移、历史会话迁移、全文外置、来源横幅），见下方
「实施拆解」表的状态列。三期（让源 agent 亲手写交接书）仍未做。

两处与最初设计不同，都是接第二个数据源时才看清的：

1. **IR 的形状**。原计划的 `HandoffBundle` 带 `touched_files` / `plan` 两个聚合字段，
   实际落成 `HandoffTurn` 枚举（User / Assistant / Tool）+ `HandoffContext`。因为文件
   路径挂在具体某一步工具上才有意义——「这一轮 Edit 了 src/main.rs」比一个全会话的
   文件清单更能让下一个 agent 定位进度。`plan` 则暂缺可靠数据源，历史 transcript 里
   四家都没有稳定的 PLAN 记录。
2. **Codex 交不出文件路径**。抽样实测：它的工具是 `exec_command`，入参只有
   `cmd`/`workdir`/`max_output_tokens`，一整条 shell 命令字符串，没有结构化文件参数。
   从命令行里正则猜路径不如不猜——猜错了写进交接记录，会把下一个 agent 引到不存在
   的文件上。所以 Codex 源的 `tool_paths` 恒为空，另外三家（Claude `file_path`、
   Grok `target_file`/`path`/`target_directory`、Copilot `path`）正常取。

## 方案：三层能力，L1 必做，L2 选做，L3 不做

### L1 语义交接（覆盖 4×4 全组合 + profile）

把现有的同 agent 交接泛化成中立的三段式：**源适配器 → 中立 IR → 目标方言渲染**。

**1. 中立 IR 下沉到 smelt-core**（新增 `smelt-core/src/session_handoff.rs`）：

```rust
pub struct HandoffBundle {
    pub source: HandoffSource,       // agent kind / profile / session_id / title / model
    pub cwd: Option<String>,
    pub git: Option<GitAnchor>,      // branch + head commit（Grok summary.json 就存了这两个，
                                     // Claude/Codex 侧现算，用来让目标 agent 校验起点）
    pub turns: Vec<HandoffTurn>,     // role / text / Vec<ToolTrace{name, kind, status, paths}>
    pub touched_files: Vec<String>,  // 从 diff path 聚合去重
    pub plan: Vec<String>,           // 未完成的 PLAN 条目
    pub full_transcript: Option<PathBuf>, // 见下方「全文外置」
    pub truncated: bool,
}
```

`build_handoff_prompt()`（含 24k/4k 预算裁剪与倒序装填）整体搬到这里，单测一并迁移。
GUI 与将来的 daemon/mobile 侧共用一份，不需要 GPUI。

**2. 两个源适配器**：

- 活跃 ACP 会话：`&[AcpEntry]` → bundle（现成逻辑改个出口）。
- 历史存档：`SessionDetail` → bundle。四家读取器已经在 `session_history.rs`，
  需要各补一处：`Turn` 现在只有 `tools: Vec<String>`（工具名），要补工具涉及的
  文件路径；`SessionDetail` 要补 `model`。Grok 分支可以直接调 `grok export` 拿
  Markdown 当 `full_transcript`，省一份解析。

**3. 目标方言渲染** `render_handoff_prompt(&bundle, target: ConversationAgentKind) -> String`：

- 统一头部：来源、cwd、git 锚点、「这是精简交接记录，不是无损副本」。
- 统一尾部：先核对工作区与 git 状态再继续，不要假设未列出的工具输出仍有效。
- 按目标微调：Grok 的 `promptCapabilities.image = false`（`agent_kind.rs:113` 已记录），
  迁往 Grok 时图片直接丢弃并在文中说明；源会话里的 slash 命令痕迹要剥掉，不然目标
  agent 会当成自己的命令去解释。

**4. 全文外置（已撤销）**：
早期实现会把完整 transcript 写到 `~/.smelt/handoffs/<ts>-<from>-to-<to>.md`。
当前不再复制对话原文，只生成受预算约束的摘要；源 agent 自己的历史存档仍是唯一全文。

**5. UI 入口**：

- 活跃会话入口已下线，不再从回答气泡创建交接会话。
- 历史页右键：「ACP 继续」「CLI/TUI 继续」之后加「迁移到 →」子菜单，四家历史都可用。
- 新会话标题：「Claude→Codex：<原标题>」。

**6. 一个必须一起修的坑**：`AcpHandoffRequest.config_values` 现在把源会话的模型/配置
原样透传（`acp_view.rs:2580-2613`）。模型 id 是各家私有的，跨 agent 透传等于给目标发一个
它不认识的 config。**目标 != 源时不透传，用目标默认值，并在 UI 上明说「模型与配置未继承」。**

**7. 溯源**：目标会话存档记 `migrated_from { agent, session_id, path }`，会话头和历史页
显示「↩ 由 Claude 迁移而来」，点击可跳回源会话。

### L2 保真升级（开关，默认关）

- **由源 agent 亲手写交接书**（质量最高的一档）：活跃会话在迁移前内部发一条
  「写交接文档」的 prompt，输出填进 `bundle.summary`；历史会话则用 headless 跑一次
  （`claude -p` / `codex exec` / `grok -p --output-format json` / `copilot -p`）。
  代价是一次额外调用和几秒等待，所以做成用户可选的「精细迁移」。
- **迁移前置检查**：有未完成的工具调用或待审批权限时拒绝迁移，先让用户结束当前回合。

### L3 原生存档转写（明确不做）

即 authsec-bridge 那条路。不做的理由是三条硬事实，不是口味问题：

1. 与仓库既定原则冲突——`agent_kind.rs` / `acp_conn.rs` 的注释已经明确「不检查任何 agent
   的私有 transcript 路径，只认 `loadSession` 声明」。
2. Grok 侧要写 6+ 文件 + 维护 FTS sqlite + 猜内部计数字段（见上文调查）。
3. 失败模式最坏：上游改格式后不是报错，而是**目标 agent 拿到半截历史继续干活**，
   比迁移失败严重得多。

如果将来一定要做，只对 Claude ←→ Codex（纯 jsonl、无索引）开实验开关，且必须带
版本探测 + 写前 dry-run 校验 + 失败自动回退 L1。

---

## 明确不迁移的东西（要在 UI 上说清楚）

模型与推理档位、权限模式、MCP 服务器配置、各家自己注入的规则文件
（Grok 的 `prompt_context.json` 里能看到它把 `CLAUDE.md` 也读进去了，四家读的集合并不一致）、
图片附件（迁往 Grok 时）、以及任何工具调用的实际输出——目标 agent 必须自己重新读文件。

---

## 实施拆解

| # | 改动 | 文件 | 状态 |
| --- | --- | --- | --- |
| 1 | 交接渲染器 + 两端身份 + 预算裁剪（含单测迁移） | 新建 `smelt-core/src/session_handoff.rs`（源自 `acp_view.rs` 的 `build_handoff_prompt`） | ✅ |
| 2 | `ConversationAgentKind::accepts_images()`（Grok = false）；`completion_summary_text` 下沉 | `agent_kind.rs`、`acp_chat.rs` | ✅ |
| 3 | `AcpHandoffTarget` / `AcpForkOrigin` 支撑历史迁移；活体 `emit_continue_in_new_session` 已删除 | `acp_view.rs` | ✅ 当前策略 |
| 4 | 回答气泡的 `handoff_menu` | `acp_view.rs` | 已下线 |
| 5 | 迁移会话标题「Claude→Codex：X」 | `main.rs` `handoff_session_title` | ✅ |
| 6 | 测试：同 agent / 跨 agent / 跨 profile / Grok 图片提示 / 预算边界 / 标题五例 | `session_handoff.rs`、`main.rs` | ✅ 11 例 |
| 7 | `Turn` 补 `tool_paths`、`SessionDetail` 补 `model`；四家读取器各补一处 | `session_history.rs` 四个 `load_*_detail` | ✅ |
| 8 | 历史 transcript → 中立 IR（`handoff_turns_from_history`）+ 历史页右键「迁移到 →」+ 后台解析后建会话 | `session_history.rs` `migration_menu` / `migrate_history_session` | ✅ |
| 9 | 来源横幅区分「继续 / 迁移而来」，历史来源不给「返回原会话」；完整 transcript 导出已删除 | `session_handoff.rs`、`acp_view.rs` `fork_banner_text` | ✅ 当前策略 |
| 10 | 测试：四家读取器的 model/路径断言、IR 转换、历史迁移请求组装、摘要预算 | 各自 `#[cfg(test)]` | ✅ |

**当前保留范围**：历史读取、IR、摘要渲染、历史页迁移入口和来源横幅。活体会话入口和
全文外置不再属于产品行为。

## 实测（本机真实 transcript）

四家各取一份真实会话跑完整链路（读取 → IR → prompt）：

| 源 | 轮次 | 模型读取 | 带文件路径的轮次 | prompt |
| --- | --- | --- | --- | --- |
| Claude | 262 | `claude-opus-5` | 103 | 15.3k 字 |
| Codex | 398 | `gpt-5.6-sol` | 0（如实反映，见上文） | 24k（触发预算裁剪） |
| Grok | 15 | `grok-4.5` | 10 | 5.1k 字 |
| Copilot | 1161 | `claude-opus-5` | 284 | 24k（触发预算裁剪） |

实测暴露并修掉了两个只有真实数据才会出现的问题：

1. **工具行噪声吃掉预算**。262 轮的 Claude 会话展开后有上百行孤零零的「工具：Bash」，
   398 轮的 Codex 会话里一轮连调五次 `wait`。现在连续同名且**无文件路径**的调用折成
   `工具：Bash ×3`；带路径的逐条保留（每次指向不同文件，是定位进度的依据）。同样的
   24k 预算里因此塞进了明显更多的对话内容。
## 交接文件

当前不会生成新的交接文件。目标 agent 只收到裁剪后的 prompt；需要全文时，用户仍从
源 agent 自己的历史存档进入原会话。旧版已经生成的 `~/.smelt/handoffs` 不由本功能
自动删除，避免把行为下线和破坏性清理绑在同一个提交里。
