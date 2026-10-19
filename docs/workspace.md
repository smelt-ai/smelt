# smelt workspace

基于 [GPUI](https://gpui.rs) 的个人工作台桌面应用：内嵌一个真正的终端，让你在自己的项目里跑
`claude` / `codex` / `copilot` / `pi` 等交互式 agent，定位是「AI coding 驾驶舱」——多项目 × 多终端的外壳。

外壳不绑死某一家 agent。会话状态来自 agent hook（`smelt-notify`）和结构化协议
（ACP 或 provider 原生 RPC），不靠轮询 transcript。会话名来自 ACP `session_title` 或 hook
`SessionTitleChanged`，没有时用首条用户请求；OSC 0/2 不是会话名。OSC 9/99/777 通知与响铃只用于信息提示。
用量是账户状态，按 provider 查询（Claude、Copilot、Codex、Cursor、Grok）。历史会话按来源读各家本机存档，
不只 Claude Code。

运行：

```bash
cargo run --bin smelt
```

> 注：`smelt` 原有的 instincts 蒸馏 CLI（`observe`/`digest`/`merge` 等）已整体移除。
> 工作区二进制：`smelt`（GUI）、`smeltd`（运行时守护）、`smelt-notify`
> （`crates/smeltd/src/bin/`）、`smelt-installer`（打包进 App）、`smelt-iroh-connect`（iroh 调试）。
> 没有独立的 `gateway` 或 `smelt-iroh-host`。随 App 分发的插件在 `plugins/`（gpui-shell 脚本应用）。
> 成员列表见根目录 `Cargo.toml`。

---

## 技术栈

| 层 | 选型 | 说明 |
|----|------|------|
| GUI 框架 | **GPUI**（`gpui-pre` 0.3.9）+ `longbridge/gpui-kit` | GPU 加速、Metal 渲染 |
| 组件库 | **gpui-component**（`longbridge/gpui-kit`，不再用 zzfn fork） | Dock/补全/主题等，与 GPUI 同一套修订 |
| 终端后端 | **portable-pty** `0.9`（在 `smeltd`） | 起 `$SHELL` 子进程并拿到 PTY |
| 终端状态机 | **alacritty_terminal** `0.26` | ANSI 解析 + 网格模型 + 滚动缓冲 |
| 定时器 | **smol** `2` | 驱动网格定时快照重绘 |

**免完整 Xcode**：通过 `gpui_platform` 的 `runtime_shaders` feature 让 Metal 着色器改到
运行时编译，编译期不调 `xcrun metal`，只装 Command Line Tools 即可构建。

---

## 目录结构

工作区是虚拟 workspace，代码全部分布在 `crates/` 下（见根目录 `Cargo.toml` 的成员列表）：

- **`crates/smelt`** —— GUI 主程序（bin 名 `smelt`）：窗口 / 会话列表 / 标签分屏 /
  文件树 / Git 面板 / 插件（`plugins/`，gpui-shell 脚本应用）等。`terminal.rs` 是内嵌终端前端
  （连 smeltd 拿字节流 + alacritty 状态机 + 颜色解析），`terminal_view.rs` 是单个终端视图
  （渲染/输入/IME/选区/滚动/resize）。PTY 与 shell 在 `smeltd`。
- **`crates/smelt-model`** —— 桌面、守护、手机共用的类型：Control API、会话状态、事件订阅解码。
  不许引 GPUI，要能编进 iOS / Android。
- **`crates/smelt-core`** —— GUI 与守护共用的无 UI 逻辑（不许引 GPUI）：
  ACP 连接层（`acp_conn.rs`）、agent 注册/身份（`agent_kind/`）、
  主库入口（`sqlite_state.rs`，关系表在 `smelt-store`）。`daemon_state` 和 `agent_status`
  的定义在 `smelt-model`，这里只再导出。
- **`crates/smelt-client`** / **`crates/smelt-client-store`** —— 桌面客户端。后者是
  `~/.smelt/client.sqlite3`（外观、每段对话的界面状态和分屏布局、连过哪些运行时）。
- **`crates/smelt-runtime-client`** —— 桌面和手机共用的运行时协议客户端（unix socket 与 iroh）。
- **`crates/smeltd`** —— 守护侧二进制（`smeltd` / `smelt-notify`）：
  终端持久化、ACP 会话托管（`acp_registry.rs`/`acp_host.rs`）、设备配对。远程不再经过网关进程。
- **`crates/smelt-paths`** —— 统一环境路径推导（`~/.smelt`）与单用例测试隔离沙箱。
- **`crates/smelt-sys`** —— 桌面和运行时共用的底层工具（进程等待、日志、守护映像、主机名）。没有单独的 `smelt-process-wait` crate，等待进程在这里。
- **`crates/smelt-git`** —— 独立的 Git 领域层（多仓库发现/状态解析/diff/写操作，不含 UI）。smelt 不管 worktree。
- **`crates/smelt-event-bus`** —— 系统内部进程间事件总线（EventHub）。
- **`crates/smelt-event-protocol`** —— 事件总线的 wire 协议（话题、事件信封、订阅与投递确认）。
- **`crates/smelt-acp-view`** —— ACP 对话原生界面（消息流/工具卡片/审批/composer）。
- **`crates/smelt-ui`** —— 主 GUI 与 acp-view 共用的 UI 基建（`ui_theme`/`markdown_mermaid`/`agent_host_state` 等）。
- **`crates/smelt-store`** —— 运行时库 `~/.smelt/smelt.sqlite3`（schema 23，按字段建表，没有通用 kv）。
- **`crates/smelt-pairing`** —— 桌面/移动共用的一次性配对串格式。
- **`crates/smelt-iroh`** —— 运行时协议的 iroh 传输，另有调试二进制 `smelt-iroh-connect`。
- **`crates/smelt-mobile`** —— 手机 Rust 内核（身份、连接、目录、对话、终端、离线缓存），经 flutter_rust_bridge 给 Flutter。

终端数据流：后台线程读守护 socket → `vte` 解析器 → 更新共享的 alacritty `Term` 网格；
UI 线程对网格做快照并重绘。PTY 本身在 `smeltd`。

### 导航

桌面没有 URL 路由器。当前显示哪一面由 `WorkspaceNav` 记录：智能体、自动化、
会话。

左侧是「新对话 / 智能体 / 自动化 / 最近对话」。点「智能体」「自动化」进该面根页。
不属于项目的对话按最近活动排在「最近对话」，行首头像标出智能体；项目里的对话
留在项目下，同样带头像。点对话是独立页。

会话右侧 Tool Panel、设置窗、确认框都不是一级入口。落盘是 `route`。实现见 `crates/smelt/src/workspace_nav.rs`。

---

## 已实现功能

### 工作台外壳
- **多会话 / 多终端**：每个会话是独立的终端或 ACP 对话（各自的 PTY/连接、历史、焦点、状态）
- **会话列表**：左侧按项目分组，点击切换、`+` 新建、关闭（至少保留一个）

### 产品级智能体

- 左侧「智能体」和「自动化」是两个独立的一级入口，都不属于项目会话列表。智能体详情是枢纽页：主栏列出绑了它的全部对话（含项目里开的）和自动化，侧栏是设置摘要。项目行 `+` 创建对话或终端，其中对话既可直接选择 provider，也可选择一个用户智能体。
- `AgentDefinition` 只保存智能体自己的稳定 ID、名称、长期指令与执行引擎。第一阶段的引擎注册表只开放 **Pi**。自动化通过稳定 ID 引用定义，并单独保存每次运行的输入；工作区不由用户填写：智能体自动化跑在它的 space 里，daemon 按自动化 ID 在 `~/.smelt/agents/<智能体 id>/automations/` 下分配稳定子目录，能读到智能体在对话里攒下的文件，又不和别的自动化互相覆盖；Shell 自动化没有智能体，留在 `~/.smelt/workspaces/automations`。以前放在后者里的智能体自动化目录，载入时连同产出一起搬进 space。运行带的智能体指令和对话是同一份（含绑定的目录与链接），另外写明 space 的位置。自动化固定使用 `FullAccess`，不提供或持久化权限策略。
- 时机是 **定时** 或 **外部触发**。定时仍是工作日、每天和按间隔三种简单模式。外部触发是本机 HTTP：`smeltd` 在 `127.0.0.1` 接收，每条自动化一个地址，同机程序 POST JSON 即可开 Run；过滤留在发送方。编辑器按「何时 → 做什么」排列，目录只显示「外部触发」不展示令牌。`smeltd` 是调度、claim、外部触发接收、投递、对账和清理的单一所有者；GUI 只编辑定义并投影状态，退出 GUI 不会把执行权交回界面。
- 定时、外部触发和「运行一次」都会创建可追踪的 `AgentRun`。手动运行只使用已保存定义，不推进自动日程游标，暂停时仍可使用；等待用户输入也占用同一条自动化的运行槽，防止重叠执行。
- 智能体 Run 不投递空消息。定时触发不带外部内容，所以带定时的智能体自动化保存时必须有指令；只有外部触发或事件的可以留空，把收到的内容当指令；写了指令时，收到的内容附在指令后面，指令里写 `{{payload}}` 或 `{{字段名}}` 则放到指定位置。指令和触发内容都为空时，手动运行直接拒绝，定时和事件到点则记一条失败的 Run。
- 新建自动化不自动保存：名称、触发器、智能体（定时还要指令）填齐后点「创建」才落库，缺什么就在表单顶部列出来；创建之后的修改照常自动保存，缺项时提示「补齐后自动保存」。模板「添加」只预填新建表单，模板指令用「填写…」留出要读的路径或对象，因为运行目录在智能体的 space 里，用户的项目不会自动带上。
- Run 在 claim 时固化智能体身份与指令、输入和 daemon 托管工作区。投递时 daemon 固定启用 `FullAccess`，使用稳定 Run ID 并等待 ACP 接收账本确认；可重试故障按指数退避，最多尝试 5 次后明确失败。终态结果有界保存，执行器在终态后由 daemon 回收。
- GUI 通过 EventHub 的首帧快照和 `automations.changed` 增量接收自动化投影，不再按秒轮询。自动化页保留有界 Run 历史，每条 Run 可查看固化输入、执行上下文、时间线、完整结果和错误。
- Pi 由 Smelt 的受管 Bun 启动，直接使用官方 JSONL RPC。智能体本身不启动进程；用户每次选择它都会创建一段独立的普通对话，同一定义可以同时拥有多段上下文。
- 对话继续由 `WsState.sessions` 持久化，并在 ACP 元数据中保存 `agent_definition_id`。自动化权威状态在主库 `~/.smelt/smelt.sqlite3` 的关系表中；旧 `agent_ui.json` 自动化数据不再读取，新功能从空存储开始。0.8.0 开发版写出的单例 `agent_runs` 会在读取时迁移为普通对话，之后不再写回。
- 自动化通知只有「应用内通知」和「不通知」，仍受设置里任务完成 / 失败通知的总开关约束。存储 schema v2 删掉了从未投递过的飞书 / Webhook sink，载入 v1 数据时剔除，剩下为空就回到默认的应用内通知。
- 股票、飞书监听与输出目标属于后续 Smelt capability/plugin 层，不写进 Pi 引擎或智能体核心。

### 历史会话页
按来源列出当前项目下各家 agent 的本地历史（`HistorySourceKind`：九家结构化 agent，
外加只有 TUI 的 Antigravity）。左列表 + 右详情还原对话；有 ACP 身份的来源可
「ACP 继续」/「CLI/TUI 继续」，Antigravity 没有 ACP 身份，只能起 CLI。
实现见 `session_history.rs`。

### 终端能力
- **内嵌真终端**：能跑交互式程序与全屏 TUI（`claude`、`htop`、`vim` 等）
- **随窗口 resize**：网格行列跟随窗口大小，同步 alacritty 与 PTY（`SIGWINCH`）
- **精确字宽**：用 `text_system.layout_line` 量等宽字符实宽，列对齐准确
- **滚动回看**：10000 行历史缓冲，鼠标滚轮 / `Shift+PageUp`/`Shift+PageDown` 翻看
- **光标**：块状光标（反色），基于 `renderable_content`

### 颜色与渲染
- **完整配色**：Tokyo Night 16 色 ANSI + xterm 256 色 + 24-bit RGB
- **文本属性**：粗体、下划线、反色（INVERSE）
- **Nerd Font**：`Maple Mono NF`，图标 / powerline / git 字形正常显示
- **网格绘制**：每行用 canvas 直接 paint，按样式切批后 `shape_line(force_width = cell_w)`。
  不走 `StyledText`：它没有强制列宽，中文 fallback 会把列对齐推歪

### 输入
- **键盘输入**：特殊键（回车/退格/方向键/Home/End/Delete/Page）、Ctrl 组合（如 `Ctrl+C` = SIGINT）
- **中文输入（IME）**：实现 `EntityInputHandler` + `canvas` 注册；中文合成、可打印字符、
  空格统一走 IME 提交路径写入 PTY，`on_key_down` 只处理特殊键与 Ctrl 组合
- **复制粘贴**：`Cmd+C` 复制选区、`Cmd+V` 粘贴剪贴板

### 鼠标
- **框选**：拖拽选择，选区高亮
- **双击选词 / 三击选行**
- 坐标换算基于 `canvas` 记录的网格原点（`absolute inset_0`）+ 实测字宽

---

## 快捷键

### 全局（挂在 `Workspace` 根节点 `on_key_down`，见 `main.rs`）

| 快捷键 | 作用 |
|---|---|
| `Cmd+K` | 命令面板 |
| `Cmd+B` | 切换左侧栏 |
| `Opt+Cmd+B` | 切换右侧 Tool Panel（Files / Git / History，以及插件的 tab） |
| `Esc` | 收起舞台上的全屏覆盖页（总览/文件树/Git/历史），回到当前会话 |
| `Cmd+[` / `Cmd+]` | 循环切换当前会话内的 pane（分屏） |
| `Cmd+1` ~ `Cmd+9` | 跳到会话列表里第 N 个会话（按显示顺序，跨项目连续数） |
| `Cmd+D` | 竖切分屏（右侧并排） |
| `Cmd+Shift+D` | 横切分屏（下方堆叠） |
| `Cmd+W` | 关闭当前 pane（会话只剩一个 pane 时关掉整个会话） |
| `Cmd+S` | 保存文件（舞台上是文件内容 / 文件树双栏时生效） |
| `Cmd+Shift+F` | 切换调试 HUD（gpui-fps：帧耗时、慢帧、掉帧、GPU/CPU/内存） |
| `Cmd+Q` | 退出（`cx.bind_keys`，也在应用菜单） |
| `Cmd+,` | 打开设置窗口 |

> **会话两种通道**：**终端**（agent 跑在内嵌终端里，你看到的是它自带的 TUI，
> 会话列表里是绿点）与**对话**（agent 经 ACP 或 provider 原生 RPC 接进 smelt 界面：结构化消息、
> 工具卡片、内嵌 diff、按钮审批、PLAN 进度条，列表里是紫点）。引擎是同一个 CLI，
> 区别只在「看它的画面」还是「跟它讲协议」。项目行「+」下拉按这两个通道分组。
>
> 对话通道支持八家裸 agent（`settings::ConversationAgentKind`，会话列表头「+Agent」下拉 /
> 项目行「+」里选）：**Claude Code**（`@agentclientprotocol/claude-agent-acp`，
> 自动使用本机 `claude` CLI）、**GitHub Copilot**（CLI 自带 `copilot --acp`）、
> **Codex**（经 `@agentclientprotocol/codex-acp` 标准 ACP 通道）、**Grok**（CLI 自带
> `grok agent stdio`，凭据 `~/.grok/auth.json`）、**Cursor** / **OpenCode** /
> **Kiro**，以及 **Pi**（终端仍用 Pi CLI；对话由受管 Bun 启动 Pi 官方
> JSONL RPC，Smelt 直接归一化事件，写入/执行类 Pi Tool 走 Smelt 权限审批）。**DSH** 是
> profile-only 的第九种 ACP 引擎；终端通道另有 **Antigravity** 与 **Crush**
> （`settings::TerminalAgentKind`）。
> 各条启动命令可在设置 →「Agent 运行环境」和「对话与工作区」改。provider 支持历史恢复时，
> 「重新开始」是协议级续接而非摆样子的新对话（Pi 使用自身 session id）。
>
> 各家实测差异（决定了下面几个 UI 决策）：首块响应 Claude ~2.6s / Copilot ~7s /
> Codex ~15s——所以回合跑着但还没出正文时，消息流末尾必须有个转着的 spinner，
> 否则那十几秒是一整屏纯黑。`promptCapabilities.image` 只有 Grok 是 false，
> 其余已接入的结构化 agent（包括 Pi）可以往对话里粘图片（⌘V，见下）。
>
> 2026-07 布局改版（起点是 claude.ai/design「桌面开发者客户端设计」稿，落地时按
> 实测调整）：窗口为「Session Panel 280px（按项目上下分组）→ Stage → Tool Panel
> 344px + 图标条 56px」+ Bottom Panel（按需展开）与底部 26px 状态栏；旧的左 Sidebar 与主区 TabBar 已
> 移除（这些是设计稿数值，代码中的最小宽度常量见 `main.rs` 的
> `MIN_SIDEBAR_WIDTH=240` / `MIN_TOOL_PANEL_WIDTH=320`）。设计稿原本的「64px 项目
> rail」左右两列实测割裂（项目只剩一个字母、必须先切项目才看得到会话），改为单列分组。
>
> 舞台有两种覆盖模式：**详情**（从停靠面板点文件/变更 → 舞台只出内容或 diff，
> 面板原样停靠）与**双栏全宽**（面板头 ⤢ → 树/列表 + 详情占满舞台，此时面板让位）；
> 总览/历史仍是整页覆盖。Esc 或返回条收回。主题按名字切换（默认是库自带的
> Default Dark / Default Light，同目录里还有 Ayu、Catppuccin 等），设置 → 外观 → 主题。
> 色板见 `ui_theme.rs`，切换入口 `settings::apply_named_theme`。

### 终端内（`TerminalView` 聚焦时，见 `terminal_view.rs`）

| 快捷键 | 作用 |
|---|---|
| `Cmd+C` | 复制选区 |
| `Cmd+V` | 粘贴剪贴板 |
| `Cmd+F` | 打开终端内搜索 |
| `Shift+PageUp` / `Shift+PageDown` | 翻滚历史缓冲 |
| `Cmd+点击` | 打开光标处识别到的链接 |
| `Shift+Enter` | 在开了 kitty keyboard protocol 的 TUI 里换行而非提交（见下） |

---

## 关键技术决策

- **终端渲染必须按网格定位，不能交给文本排版器自由流**：终端是字符网格，第 N 列就得画在
  `N × cell_w`；字体的字形宽度只决定「字长什么样」，不决定「它在哪」。早先整行拼成一个
  `StyledText` 让排版器自由流，位置就由字体 advance 说了算——主字体 JetBrains Mono
  （advance 0.6em）没有中文字形，中文 fallback 到 PingFang（1.0em），而两格 = 1.2em，
  **每个中文字亏 0.2em**，误差沿行累积：Claude Code 输出的表格里含中文的行比纯 `─` 横线
  短，`│` 一路往左漂。（当时的对策是让鼠标命中测试反过来「复现这个歪几何」，治标不治本。）
  现在照 Zed 终端：`paint_row` 用 canvas 直接画，`shape_line(.., force_width = Some(cell_w))`
  把每个 glyph 的 x 钉到 `序号 × cell_w`，字体 advance 作废。`StyledText` 走 `shape_text`，
  没有 force_width，不能用。一行仍按「样式相同 + 列号连续」切批。
  **宽字符必须落在批尾**：force_width 按 glyph 序号钉位，宽字符占两格却只算一个 glyph，
  后面若还有同批字符就会少一格。宽字符后面的 `'\0'` 占位格让下一个字符列号对不上而断批。
  顺带收掉了三个补丁：`pos_to_cell` 的「重新整形反查列号」workaround（现在 `col = x/cell_w`
  直接成立）、IME 候选框与渲染互相矛盾的定位、以及「截断行尾防自动折行」。
- **Shift+Enter 与 kitty keyboard protocol**：遗留终端编码里 Enter 只有 `\r` 一种字节，
  Shift/Alt/Ctrl+Enter 全塌缩成同一个 `\r`，修饰键信息在协议层就丢了——所以「Shift+Enter
  换行、Enter 提交」不是 UI 能自己决定的事。kitty keyboard protocol 用 CSI u 编码补回修饰键
  （Shift+Enter → `ESC[13;2u`），TUI 进入时发 `CSI > 1 u` 开启，alacritty_terminal 会解析并置
  `TermMode::DISAMBIGUATE_ESC_CODES`。`keystroke_to_bytes` 据此分流：**开了才发 CSI u，没开
  一律发 `\r`**。这个 gate 不能省——bash/zsh 不认 CSI u，硬发会被 readline 当文本吐出 `[13;2u`。
  Claude Code 从 v2.1 起会主动开；老版本或不开协议的 TUI 还有 Alt+Enter（回退到 meta 前缀
  `ESC`+`CR`）这条传统通道。
  两个坑：① alacritty 的 `Config::kitty_keyboard` **默认 false**，关着时它会把 `CSI > 1 u`
  在 `push_keyboard_mode` 里静默 return 掉，mode 位永远置不上——建 `Term` 时必须显式开。
  ② 发送侧目前只给 Enter 编了 CSI u，其余键（Escape、Ctrl+字母、带修饰的方向键）在协议开着时
  仍发遗留编码，不是完整的 level-1 实现。够用是因为 Claude Code 本来就得兼容 iTerm2 那种
  「只把 Shift+Enter 映射成 CSI u、其余照旧」的手工配置；真要补全得照搬 alacritty 的
  `build_key_sequence`。
- **runtime_shaders**：绕开完整 Xcode 依赖，是能在只装 CLT 的机器上构建的关键。
- **portable-pty + alacritty（仅状态机）**：与 GUI 集成比 alacritty 自带 event_loop 更干净，
  与 codux 的做法一致。
- **IME 走 `EntityInputHandler`**：中文等合成输入不经 `on_key_down`，必须实现输入处理器
  并在 paint 阶段用 `window.handle_input` 注册；同一 canvas 顺便记录网格原点供鼠标换算。
- **IME 的 `selectedRange` 必须返回有效折叠区间，不能恒为 `None`**：一直返回 `None` 会
  桥接成 AppKit 的 `{NSNotFound, 0}`，等于告诉系统「这里没有文字光标」——系统切换输入法
  时的提示气泡就不会出现（候选窗本身走 `hasMarkedText`/`setMarkedText`，不受影响）。

---

## 运行时、多机与手机

桌面 GUI 和手机都是客户端。机器上的会话、项目、智能体、自动化在那台机器的 `smeltd` 里。
一个桌面窗口可以同时挂上本机和多台远程运行时：侧栏按运行时分块，Git、文件、历史和额度打到会话所在的那一台。
添加远程运行时是把一次性配对串粘进桌面。设置里标了「只作用于本机运行时」的页面不改远程机器。

### Linux 无头运行时

发布包是 `smeltd-<版本>-linux-<架构>.tar.gz`（`ci/linux/ci.sh package`）。在机器上解包后：

```bash
./install.sh           # 装 systemd 用户服务、启动，并打印配对串
./install.sh --no-pair # 只安装并启动
```

脚本要求 Linux 和 systemd。它把 `smelt-notify`、`smelt-up` 放进 `~/.smelt/bin`，再执行 `smeltd install`（不可变映像；已经在跑就交接，会话保留）。之后：

```bash
smeltd pair                      # 打开远程访问，打印一次性配对串和终端二维码
smeltd devices                   # 列出已配对设备
smeltd devices revoke <endpoint> # 撤销并立刻断开
smeltd status
```

`pair` 会把远程访问打开（保留已填写的自建 relay）。配对串 10 分钟、用一次。配对等于把这台机器上当前用户的 shell 交给对方。macOS 上守护仍由桌面拉起，`smeltd install` 只在 Linux 注册 systemd 单元。细节在 `crates/smeltd/src/service.rs` 和 `scripts/install-smeltd.sh`。

### 远程桌面

配对之后，桌面可以打开那台 Linux 的整块屏幕：运行时转发 VNC，`smelt-runtime-client` 在本机随机端口提供 noVNC 页面，桌面用网页视图打开。打开即接管；切到「只看」后不再发送键鼠。中文直接打在画面上。安装包里的 `--with-desktop`（把 Xvnc、桌面环境和常驻浏览器一并装上）还没做，现有机器上的显示栈是单独配的。见 `docs/archive/remote-runtime.html` 的 D9。

### 终端 Dock

`Cmd+D` / `Cmd+Shift+D` 仍是左右或上下分屏。同一格还可以叠几个终端（标签组）。运行时只记一段对话里有哪些终端、按什么顺序（`session.position`，运行时库 schema v22 起没有 `layout_node`）；怎么分屏是每台设备自己的界面状态，存在客户端库的 `conversation_route.dock_layout`（带版本号的 JSON，见 `crates/smelt/src/conversation_layout.rs`），和工具面板、打开的文件等放在一起。

### 闲置挂起

结构化对话闲置 30 分钟后，`smeltd` 停掉那场会话的宿主进程（`crates/smeltd/src/acp_idle.rs` 的 `ACP_IDLE_SUSPEND_AFTER`）。会话还在目录里，界面仍显示空闲。发消息、改配置、翻历史或跨智能体投递时先唤醒，并把挂起前的模型、权限模式和推理强度写回新进程。有进行中的回合、待审批、排队输入、后台任务，或属于自动化 / 远程手机建的会话，不会挂起。

### 手机

`mobile/` 的 Flutter 界面经 FRB 调 `smelt-mobile`。内核负责设备密钥、配对、每个运行时一条连接、会话目录投影、对话（增量帧）和终端字节流。离线时读 App 数据目录里的 `smelt-mobile-cache.sqlite3`：按 endpoint 存上次的会话列表，以及每个运行时最近几个对话的快照尾部（最多 5 段、合计 32MB）。这是缓存，库坏了就重建，不迁版本。终端配色由手机按明暗自己定，不跟运行时下发的主题走。构建见 [`mobile/README.md`](../mobile/README.md)，接入步骤的记录见 [`archive/mobile-runtime-p4.md`](archive/mobile-runtime-p4.md)。

## 路线图

产品待做见 [`product-roadmap.md`](product-roadmap.md)，主航道外的点子见 [`roadmap.md`](roadmap.md)。
会话状态不靠解析 `~/.claude/projects/*.jsonl`。用量按 provider 查账户。
[`archive/remote-ops-roadmap.md`](archive/remote-ops-roadmap.md) 是已删除的 JSON-WS 网关路线图，不是现行协议。
