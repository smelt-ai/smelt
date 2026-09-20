# smelt —— Mac 上的 AI coding 驾驶舱

*内部技术分享稿 · 2026-07-09*

> 一个基于 GPUI 的桌面工作台，内嵌真终端，专为「同时指挥多个 Claude Code / Codex agent 干活」设计——多项目 × 多标签，会话状态一目了然，关掉 GUI 也不打断正在跑的 agent。

**状态**：working prototype · 持续迭代中
**语言**：Rust 2024 ｜ **GUI**：GPUI

---

## 01 · 为什么做这个

### 编辑工具进化史：为什么终端会成为下一个入口

1. **IDE 时代** —— IDEA、VS Code 这类图形化编辑器，靠语法高亮、代码补全把「写代码」标准化，代价是内存占用大、启动慢、扩展生态越叠越臃肿。
2. **AI 插件化时代** —— Copilot、Tabnine 等以插件形式接入已有编辑器：光标后拉出一行灰色的补全建议，或者在右侧拽出一个窄侧边栏对话。这本质上是一种折中——**编辑器的核心交互逻辑没有变**，人依然是敲键盘的那个苦力，AI 只是个被动响应、等你问了才说话的侧边栏挂件。
3. **AI 原生编辑器时代** —— Cursor 这类产品把 AI 从插件位置挪进编辑器内核：inline diff、多文件改动、agent 模式，AI 的话语权明显变大了，但用户打开的依然是一个「编辑器」，本质上还是在盯着一份代码改。
4. **AGI 时代：主驾驶的交接** —— 早期的 AI 插件是 IDE 里的「副驾驶」，回答你问、补全你打；而当 agent 真正能独立跑完一整个任务（读代码、改代码、跑测试、提交），人的角色就该从「打字的人」变成「看导航、下指令的人」。这时候需要的不再是一个更聪明的编辑器，而是一个能同时看住好几个正在跑的 agent 的**驾驶舱**——这正是 smelt 想站的位置：不做第 3 阶段那个更聪明的编辑器，直接跳到第 4 阶段，把终端（agent 真正干活的地方）变成主战场。

### 从「蒸馏 CLI」到「驾驶舱」的一次换赛道

smelt 最早做的其实是另一件事：一条 **instincts 蒸馏** 链路，从飞书聊天记录里提炼个人工作习惯 / 决策偏好（`observe` 采集 → `digest` 提炼 → `merge` 归档成规则），配一个飞书客户端做交互入口。

这条链路跑了一段时间后，发现真正每天磨人的问题根本不在这里，而在更基础的地方：**同时开好几个 Claude Code 会话，各自散在系统终端的一堆窗口 / tab 里**，切来切去分不清谁在等输入、谁跑完了、谁还在 thinking；GUI 一关或者崩了，终端里的会话也就跟着没了。加上飞书接入本身要走权限申请、流程不轻，两相权衡下，「蒸馏」这条链路无论是需求迫切度还是接入成本都比不过这个更基础的问题。

于是做了一次明确的取舍：**砍掉 instincts 蒸馏链路和飞书客户端**，仓库收敛成两个二进制——`smelt`（GUI 主程序；早期叫 `workspace`）和 `smeltd`（终端持久化守护），目标很单一：**把「多项目 × 多个 agent 终端」这件事，做成一个专用的驾驶舱外壳**，而不是在系统终端里硬凑。

---

## 02 · 核心功能演示

### 打开就是一个能干活的多 agent 工作台

```
┌─ smelt · claude ─┬─ docs-site · codex ─┬─ infra · claude ──┐
│ ●等待           │ ●thinking           │ ●完成             │
├───────────────────┴──────────────────────────────────────┤
│ ⠿ smelt                    │ ~/dev/smelt ❯ claude          │
│    ▸ claude · 会话 1        │ ╭──────────────────────────╮ │
│    ▸ claude · 会话 2        │ │ ? 给侧边栏项目行加个「＋」 │ │
│ ⠿ docs-site                │ │   一键起 Claude Code/Codex│ │
│ ⠿ infra  ＋                 │ ╰──────────────────────────╯ │
│                             │ ● Thinking…（标签圆点已变色）│
└─────────────────────────────┴────────────────────────────┘
```
*示意图（非真实截图）—— 多标签 + 侧边栏项目/会话拖拽排序 + agent 状态圆点*

**工作台外壳**
- 多标签 / 多终端，每个标签独立 PTY、历史、焦点
- 侧边栏项目 / 会话拖拽排序，随手整理常用项目
- 项目行「＋」一键起 Claude Code / Codex / Copilot 等，启动项列表可在设置里自定义（名称 + 命令）

**终端能力**
- 真终端：能跑 `claude`、`vim`、`htop` 等全屏 TUI
- 完整 ANSI / 256 色 / 24-bit 色 + Nerd Font
- 中文 IME、框选复制、10000 行滚动回看

**项目工具**
- 文件树浏览 + 文件名 / 内容搜索
- Git diff 视图，文件监听自动刷新（不轮询）

**会话持久化**
- `smeltd` 类 tmux 守护进程
- GUI 退出 / 崩溃不影响 shell 存活
- 重开 GUI 按会话 id 自动 reattach

**状态总览**
- 用量页、历史会话浏览
- Dock 角标提示待处理会话

**设置**
- 设置页迁移到 gpui-component Settings 组件，主题可切换

---

## 04 · 技术架构亮点

### 选型服从一个目标：真终端 + 不卡

| 层 | 选型 | 说明 |
|---|---|---|
| GUI 框架 | GPUI（zed-industries/zed） | GPU 加速，Metal 渲染，锁定固定 commit |
| 组件库 | gpui-component（longbridge） | Dock / Sidebar / Settings 等，同一 gpui 生态 |
| 终端后端 | portable-pty | 起 `$SHELL` 子进程并拿到 PTY |
| 终端状态机 | alacritty_terminal | 只用它的 ANSI 解析 + 网格模型，不用自带 event loop |
| 定时器 | smol | 驱动终端网格快照重绘 |

**关键决策**

- **runtime_shaders —— 免装完整 Xcode**：Metal 着色器改到运行时编译，编译期不调 `xcrun metal`，只装 Command Line Tools 就能构建，降低团队里其他人上手的门槛。
- **portable-pty + alacritty「仅状态机」**：不用 alacritty 自带的 event loop，只借它的 ANSI 解析和网格模型，自己接 GPUI 的渲染循环——集成更干净，也更好控制重绘时机。
- **IME 走 EntityInputHandler**：中文等合成输入不经普通按键事件，必须实现输入处理器并在 paint 阶段注册；同一个 canvas 顺便记录网格原点，供鼠标坐标换算复用。
- **整行 StyledText 渲染**：放弃逐格定宽 span，改成整行一次性成形、多 TextRun 上色——解决了拖选时的子像素抖动和宽度截断，比逐格渲染更快。

### 为什么不是 Electron / Tauri

桌面应用现在更主流的选择是 Electron / Tauri，smelt 选了 GPUI，取舍点主要在下面几个维度。

| 维度 | Electron | Tauri | GPUI（smelt 用的） |
|---|---|---|---|
| 渲染内核 | 自带完整 Chromium | 系统 WebView（WKWebView / WebView2） | 原生 GPU 渲染（Metal），没有浏览器内核 |
| 包体 / 内存 | 大（Chromium 全家桶） | 小（复用系统 WebView） | 小 |
| 终端怎么画 | xterm.js 跑在 DOM / Canvas 里 | 同上，还多套一层 WebView 桥 | 直接拿网格数据 GPU 绘制，没有 DOM / WebView 中间层 |
| 生态成熟度 | 非常成熟，组件/文档海量 | 成熟 | 早期——Zed 团队自用为主，文档少，很多基础设施要自己写 |
| 上手门槛 | 低，web 前端友好 | 中，Rust + web 两套技能 | 较高，API 还在变 |

smelt 的核心体验是「能跑 `vim`/`htop` 这类全屏 TUI 还不卡」，终端渲染是重点而不是锦上添花的一个功能。Electron / Tauri 的终端方案本质上是 xterm.js 在网页里模拟终端，GPU 加速也要隔着一层 DOM 或 WebView；GPUI 是直接原生绘制，没有中间层，帧率更稳、资源占用更低——代价是生态几乎从零开始，很多 Electron / Tauri 里现成的东西（窗口管理、组件库、输入法处理）在 GPUI 里都得自己写，这也是下面这堆坑的来源。

### 踩坑速览

选 GPUI 的代价基本都体现在这几个坑上，详细经过见对应小节：

- **编译依赖**：默认编译期要装完整 Xcode（见上文「关键决策」）
- **中文输入法**：合成输入默认打不出字（见上文「关键决策」）
- **窗口拖拽冻结动画**：系统自带拖拽 API 会卡住 App 动画（见 03 节，`baa612f`）
- **鼠标事件悄悄失灵**：内部 id 按帧分配、跨帧判断会失效（见 03 节，`baa612f`）
- **定时器无脑刷新浪费性能**：终端定时器以前无条件 30ms 触发一次重绘，哪怕 shell 完全空闲也按 33 次/秒重画；改用 alacritty 的 damage tracking 判断内容是否真变化，空闲终端 5 秒内 `render()` 从理论值降到 8 次——过程中顺带挖出一个真 bug：`damage_cursor()` 曾无条件标脏光标格，得排除「仅光标那一格」才算数，现有回归测试 `damage_gate_tests` 覆盖（`016c227`）
- **做不出无边框窗口**：GPUI 在 macOS 上只会建带圆角 + 阴影的标准窗口，全透明穿透浮窗做不到，得越过框架直接拿底层 `NSWindow` 用 `objc` 手改 `styleMask` / 阴影 / 背景色（`106433d`）

一句话总结：**生态越早期，越要自己把「别人已经踩过的坑」重新踩一遍**——这是选 GPUI 这条路要付的主要成本。

### 打包分发也踩过一次坑

最早的打包脚本（`5e2f62b`）就是把 app 塞进 `hdiutil create` 直接出 dmg：图标位置随 Finder 默认摆，没有任何引导，还得另外配一份 `INSTALL.txt` 文字教程告诉同事怎么手动绕过 Gatekeeper（右键打开 / 终端敲 `xattr -dr`）。后来加了一版「dmg 定制」（`5402f7f`）：AppleScript 操作 Finder 的挂载窗口——隐藏工具栏、固定尺寸、app 图标摆左边、软链到 `/Applications` 摆右边；再配一张 Pillow 代码现算的深色渐变背景图（`make-dmg-bg.py`，不找设计稿）画上箭头和「拖到应用程序即可安装」的引导文字。打开就看得懂怎么装，原来那份 `INSTALL.txt` 教程也就没必要了。

### smeltd：持久化守护的三条不变量

```
smelt（GUI） ──attach / 输入──▶ smeltd（守护） ──PTY 管理──▶ shell / claude
可随时退出 / 崩溃                常驻 Term 导出网格 keyframe 快照    真实子进程，GUI 生死与它无关
```

1. 会话 **id 创建后立即落盘**（spawn 成功后写 `workspace.json`），避免「刚建就丢」
2. **attach 先回报 PTY 真实尺寸**，GUI 按它建终端，再收网格快照（重放区间与实时输出按
   `replay_len` 划界），杜绝尺寸协商时序错乱导致的花屏
3. 同一会话的读写走**同一把锁串行**，防止并发 attach 把输出交叉打乱

> 说明：早期「按会话 id 落盘 + 重放字节流」的环形缓冲方案已废弃（smeltd 改为常驻
> `Term` 导出网格 keyframe 快照，历史字节缓冲被测试锁死永不 feed）；ACP 会话则走
> `ConversationSnapshot` + `resume_session_id` 的协议级续接。文档中残留的「字节流重放」说法
> 一律以本条为准。

---

## 05 · 后续规划 / 怎么参与

### 下一步往「看得见 agent 在干嘛」走

上一版待办里的可点击链接、守护版本检测 + 重启确认、设置页拆独立窗口、分屏、命令面板 Cmd+K 这几项，核对下来都已经合并完成，从表里挪走了。

| 状态 | 事项 | 说明 |
|---|---|---|
| 🔲 待做 | 每标签独立项目目录 | 新建标签时可单独选项目，不再绑定整个窗口一个目录 |
| 🚧 部分完成 | 会话监控层 | UI 三态（要你 / 运行中 / 空闲）由 hook / ACP 压成 `AgentStatus`；未读走 attention，不占第四态。后续可补实时解析 `~/.claude/projects/*.jsonl`，附 token 用量与最近工具调用 |
| 🔲 待做 | smeltd 落盘增强 | 会话历史写 JSONL + SQLite 索引，守护重启后可恢复完整历史、总览页可列历史会话 |

### 想试试 / 想一起做

- `cargo run --bin smelt` 直接跑起来（GUI 会按需拉起 smeltd），可选单独 `cargo run --bin smeltd` 体验会话持久化
- 想法和待做点子集中记在 `docs/roadmap.md`，做完了从那挪走或标 ✅

---

## 附录 · 参考材料

- [longbridge/gpui-component](https://github.com/longbridge/gpui-component) —— smelt 直接依赖的 GPUI 组件库（Dock / Sidebar / Settings 等 UI 组件的来源，12k+ star）
- [jamesrochabrun/AgentHub](https://github.com/jamesrochabrun/AgentHub) —— 同类方向的 macOS app：统一管理 Claude Code / Codex 的所有会话，支持建 worktree、并行跑多个终端、diff 预览与内联改动
- [manaflow-ai/cmux](https://github.com/manaflow-ai/cmux) —— 基于 Ghostty 的开源 macOS 终端，给 AI coding agent 做了竖排标签 + 通知，主打多任务与可编程性（24k+ star）
