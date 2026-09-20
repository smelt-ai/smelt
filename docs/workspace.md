# smelt workspace

基于 [GPUI](https://gpui.rs) 的个人工作台桌面应用：内嵌一个真正的终端，让你在自己的项目里跑
`claude` / `codex` / `gemini` 等交互式 agent，定位是「AI coding 驾驶舱」——多项目 × 多终端的外壳。

外壳与状态感知都与具体 agent 无关——状态靠结构化 agent hook 与 ACP 协议；终端标题
（OSC 0/2）只用于展示，OSC 9/99/777 通知与响铃只用于信息提示。只有用量统计与历史会话浏览要读
`~/.claude/projects/**/*.jsonl`，这两项仅支持 Claude Code。

运行：

```bash
cargo run --bin smelt
```

> 注：`smelt` 原有的 instincts 蒸馏 CLI（`observe`/`digest`/`merge` 等）已整体移除。
> 工作区现含多个二进制：`smelt`（GUI 主程序）、`smeltd`（终端持久化守护，另含
> `gateway` / `smelt-notify` 等辅助二进制，见 `crates/smeltd/src/bin/`），以及独立打包的
> 插件，详见根目录 `Cargo.toml` 的 workspace 注释与成员列表。

---

## 技术栈

| 层 | 选型 | 说明 |
|----|------|------|
| GUI 框架 | **GPUI**（zed-industries/zed，锁定 commit `e0931d5`） | GPU 加速、Metal 渲染 |
| 组件库 | **gpui-component**（zzfn，Apache-2.0） | Dock/补全/主题等，构建于同一 gpui |
| 终端后端 | **portable-pty** `0.9` | 起 `$SHELL` 子进程并拿到 PTY |
| 终端状态机 | **alacritty_terminal** `0.26` | ANSI 解析 + 网格模型 + 滚动缓冲 |
| 定时器 | **smol** `2` | 驱动网格定时快照重绘 |

**免完整 Xcode**：通过 `gpui_platform` 的 `runtime_shaders` feature 让 Metal 着色器改到
运行时编译，编译期不调 `xcrun metal`，只装 Command Line Tools 即可构建。

---

## 目录结构

工作区是虚拟 workspace，代码全部分布在 `crates/` 下（见根目录 `Cargo.toml` 的成员列表）：

- **`crates/smelt`** —— GUI 主程序（bin 名 `smelt`）：窗口 / 会话列表 / 标签分屏 /
  文件树 / Git 面板 / 通用插件 UI 等。`terminal.rs` 是终端后端
  （PTY + alacritty 状态机 + 颜色解析），`terminal_view.rs` 是单个终端视图
  （渲染/输入/IME/选区/滚动/resize）。
- **`crates/smelt-core`** —— GUI 与守护共用的无 UI 逻辑（不许引 GPUI）：
  ACP 连接层（`acp_conn.rs`）、agent 注册/身份（`agent_kind.rs`）、会话状态
  （`agent_status.rs`/`daemon_state.rs`）、JSON/SQLite 存储（`sqlite_state.rs`）、
  通用项目注册与隔离 workspace 服务等。
- **`crates/smeltd`** —— 守护侧二进制（`smeltd` / `gateway` / `smelt-notify`）：
  终端持久化、ACP 会话托管（`acp_registry.rs`/`acp_host.rs`）、插件 Host 与远程网关。
- **`crates/smelt-paths`** —— 统一环境路径推导（`~/.smelt`）与单用例测试隔离沙箱。
- **`crates/smelt-git`** —— 独立的 Git 领域层（多仓库发现/状态解析/diff/写操作，不含 UI）。
- **`crates/smelt-event-bus`** —— 系统内部进程间与插件事件总线（EventHub）。
- **`crates/smelt-plugin-api`** —— 插件系统跨进程 API 契约与 DTO 定义。
- **`crates/smelt-plugin-host`** —— 宿主侧插件进程管理与 Web/Node 插件执行环境。
- **`crates/smelt-webview`** —— 基于 macOS 原生 WKWebView 的独立窗口化视图容器。
- **`crates/smelt-acp-view`** —— ACP 对话原生界面（消息流/工具卡片/审批/composer）。
- **`crates/smelt-ui`** —— 主 GUI 与 acp-view 共用的 UI 基建（`ui_theme`/`markdown_mermaid`/`agent_ui_config` 等）。
- **`crates/smelt-store`** —— 桌面 SQLite/KV 存储与 legacy 导入原语（移动端不编 native SQLite）。
- **`crates/smelt-pairing`** —— 桌面/移动共用的配对码格式。
- **`crates/smelt-remote-gateway`** —— 移动端远程 ACP/终端网关（不含 GPUI）。
- **`crates/smelt-iroh`** —— P2P 隧道（iroh）封装。
- **`crates/smelt-mobile`** —— 移动端复用 Rust 侧 iroh 隧道的桥（cargokit）。

终端数据流：后台线程读 PTY 输出 → `vte` 解析器 → 更新共享的 alacritty `Term` 网格；
UI 线程对网格做快照并重绘。

### 导航

桌面没有 URL 路由器。当前显示哪一面由 `WorkspaceNav` 记录：智能体、自动化、
会话、插件工作台。

左侧「智能体 / 自动化」是分组标题：每次点击都进该面根页。智能体对话挂在工作台
里、自动化下面的「对话」子栏（空时仍显示）；点对话是独立页，不再套智能体页头
和返回。插件工作台挂在「插件」分组下。

会话右侧 Tool Panel、设置窗、确认框都不是一级入口。落盘仍是 `route` +
`active_workspace_surface`。实现见 `crates/smelt/src/workspace_nav.rs`。

---

## 已实现功能

### 工作台外壳
- **多会话 / 多终端**：每个会话是独立的终端或 ACP 对话（各自的 PTY/连接、历史、焦点、状态）
- **会话列表**：左侧按项目分组，点击切换、`+` 新建、关闭（至少保留一个）

### 产品级智能体

- 左侧「智能体」和「自动化」是两个独立的一级入口，都不属于项目会话列表。项目行 `+` 创建对话或终端，其中对话既可直接选择 provider，也可选择一个用户智能体。
- `AgentDefinition` 只保存智能体自己的稳定 ID、名称、长期指令与执行引擎。第一阶段的引擎注册表只开放 **Pi**。自动化通过稳定 ID 引用定义，并单独保存每次运行的输入；工作区不由用户填写，daemon 按自动化 ID 在 `~/.smelt/workspaces/automations` 下分配稳定目录。自动化固定使用 `FullAccess`，不提供或持久化权限策略。
- 时机是 **定时** 或 **外部触发**。定时仍是工作日、每天和按间隔三种简单模式。外部触发是本机 HTTP：`smeltd` 在 `127.0.0.1` 接收，每条自动化一个地址，同机程序 POST JSON 即可开 Run；过滤留在发送方。编辑器按「何时 → 做什么」排列，目录只显示「外部触发」不展示令牌。`smeltd` 是调度、claim、外部触发接收、投递、对账和清理的单一所有者；GUI 只编辑定义并投影状态，退出 GUI 不会把执行权交回界面。
- 定时、外部触发和「运行一次」都会创建可追踪的 `AgentRun`。手动运行只使用已保存定义，不推进自动日程游标，暂停时仍可使用；等待用户输入也占用同一条自动化的运行槽，防止重叠执行。
- Run 在 claim 时固化智能体身份与指令、输入和 daemon 托管工作区。投递时 daemon 固定启用 `FullAccess`，使用稳定 Run ID 并等待 ACP 接收账本确认；可重试故障按指数退避，最多尝试 5 次后明确失败。终态结果有界保存，执行器在终态后由 daemon 回收。
- GUI 通过 EventHub 的首帧快照和 `automations.changed` 增量接收自动化投影，不再按秒轮询。自动化页保留有界 Run 历史，每条 Run 可查看固化输入、执行上下文、时间线、完整结果和错误。
- Pi 由 Smelt 的受管 Bun 启动，直接使用官方 JSONL RPC。智能体本身不启动进程；用户每次选择它都会创建一段独立的普通对话，同一定义可以同时拥有多段上下文。
- 对话继续由 `WsState.sessions` 持久化，并在 ACP 元数据中保存 `agent_definition_id`。自动化权威状态在主库 `~/.smelt/smelt.sqlite3` 的关系表中；旧 `agent_ui.json` 自动化数据不再读取，新功能从空存储开始。0.8.0 开发版写出的单例 `agent_runs` 会在读取时迁移为普通对话，之后不再写回。
- 股票、飞书监听与输出目标属于后续 Smelt capability/plugin 层，不写进 Pi 引擎或智能体核心。

### 历史会话页（Claude Code 专属）
读取 `~/.claude/projects/<编码后的项目路径>/*.jsonl`，以「左列表 + 右详情」还原
完整对话；右键条目可「ACP 继续」/「CLI/TUI 继续」协议级续接（不再是只读浏览）。
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
- **整行渲染**：每行作为单个 `StyledText` + 多 `TextRun` 上色，整行只整形一次 ——
  拖选拆分不抖动、宽度精确不截断，且比逐格 span 更高效

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
| `Cmd+B` | 切换右侧 Tool Panel（Files / Git / History，以及插件贡献的 tab） |
| `Esc` | 收起舞台上的全屏覆盖页（总览/文件树/Git/历史），回到当前会话 |
| `Cmd+[` / `Cmd+]` | 循环切换当前会话内的 pane（分屏） |
| `Cmd+1` ~ `Cmd+9` | 跳到会话列表里第 N 个会话（按显示顺序，跨项目连续数） |
| `Cmd+D` | 竖切分屏（右侧并排） |
| `Cmd+Shift+D` | 横切分屏（下方堆叠） |
| `Cmd+W` | 关闭当前 pane（会话只剩一个 pane 时关掉整个会话） |
| `Cmd+S` | 保存文件（舞台上是文件内容 / 文件树双栏时生效） |
| `Cmd+Shift+F` | 切换调试 HUD（右上角帧率 + 帧耗时） |
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
> 各条启动命令可在设置 →「Agent 集成」页改。provider 支持历史恢复时，
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
> 总览/历史仍是整页覆盖。Esc 或返回条收回。主题支持深色/浅色，
> 设置 → 外观 → 主题模式切换（色板见 `ui_theme.rs`，切换入口
> `settings::apply_theme_mode`）。

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
  现改为照 Zed 终端（同 GPUI 栈）的做法：`render_row` 按「样式相同 + 列号连续」把一行切成
  若干批，每批绝对定位在 `起始列 × cell_w`。宽字符第二格是 `'\0'` 占位，跳过它但列号照常
  前进，后面的字符便「对不上列号」而断批。
  **关键性质：宽字符永远落在批尾**（它后面必有占位格 → 下一个字符必断批），所以批内除
  末位外全是 advance == cell_w 的窄字符，位置天然正确；宽字符那点亏空后面再无字符可推。
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
- **整行 `StyledText`**：解决「逐格定宽 span」带来的子像素抖动与截断，是终端网格渲染的正确姿势。
- **IME 的 `selectedRange` 必须返回有效折叠区间，不能恒为 `None`**：一直返回 `None` 会
  桥接成 AppKit 的 `{NSNotFound, 0}`，等于告诉系统「这里没有文字光标」——系统切换输入法
  时的提示气泡就不会出现（候选窗本身走 `hasMarkedText`/`setMarkedText`，不受影响）。

---

## 路线图

- [ ] **每标签独立项目目录**：新建标签时可选不同项目
- [ ] **会话监控层**：`~/.claude/projects/*.jsonl` 实时解析各终端 agent 状态
  （thinking / 等待输入 / 完成、token 用量、最近工具调用）
- [ ] 更像 IDE：分屏
