# 文档

smelt 是跨机器、多智能体的原生 AI 编程指挥台：macOS（Apple Silicon）和 Windows 10 1803+（x86_64）上的 GPUI 桌面工作台，Linux 上的无头守护进程 `smeltd`，以及 iOS / Android 的手机和平板「出菜口」。
Windows 第一版是不签名的安装包，没有自动更新。Linux 上可以只跑 `smeltd`，再用电脑或手机配对上去。
这页文档只讲怎么用，架构细节不在这——接口和默认行为可能会变。

## 快速开始

### macOS

从 [GitHub Releases](https://github.com/smelt-ai/smelt/releases) 下载 `Smelt.dmg`，拖进
应用程序文件夹，打开即可，只支持 Apple Silicon Mac。应用在启动时和手动检查更新
（stable/beta 渠道），有新版本时会提示。

### Windows

从同一个 [Releases](https://github.com/smelt-ai/smelt/releases) 页下载
`smelt-<版本>-windows-x86_64-setup.exe` 运行。按当前用户安装，不要管理员权限，
默认装到 `%LOCALAPPDATA%\Programs\Smelt`，建开始菜单项，可选把 `smelt` 命令加入 PATH。
升级时先关掉 Smelt，再运行新版本的安装程序。

数据目录是 `%USERPROFILE%\.smelt`。

这一版：

- 只有 x86_64，系统是 Windows 10 1803 及以上。
- 安装程序和 exe 都不签名。SmartScreen 可能提示未知发布者，选择仍要运行即可。
- 没有自动更新。
- 交互 shell 是 `cmd.exe`（ConPTY），或 `COMSPEC` 指向的程序。
- 没有菜单栏图标、Dock 角标、系统通知。这些只在 macOS 上。

### 关掉窗口之后

退出 smelt 甚至它崩溃了，里面跑着的 shell（包括正在干活的 `claude`）不会被杀掉，
后台继续活着；重新打开 smelt，之前的分屏布局和终端会话都会自动恢复，不用重新
`cd` 进项目、重新跑一遍命令。注意这只扛得住"关 smelt"，扛不住重启电脑——重启会
把所有进程一起带走。升级或重启本机守护进程也一样：正在跑的 shell 会被关掉，进行中的
回合会中断，对话历史保留（见[设置窗口](#设置窗口)「维护」里的「后台服务」）。

想从源码构建、或者想搞清楚内部是怎么拼起来的，看[开发文档](/develop)。

## 打开项目

smelt 没有独立的"项目列表"——项目就是某个会话的工作目录（cwd），跟着会话走，
没有单独的项目配置文件。打开一个项目有几种方式：

- **拖拽**：把项目文件夹（或文件）直接拖进窗口，会以该目录为 cwd 开一个新会话。
- **侧栏底部"打开项目"按钮**，或 `Cmd+K` 命令面板里的"打开项目…"：打开运行时里的
  目录浏览器（`crates/smelt/src/dir_picker.rs`），选中后开新会话。这不是系统原生的
  文件夹选择框；插件仍可以另要系统选择框。
- **项目分组行右侧的 `+`**：在这个项目已有的 cwd 下继续开新的终端，或执行设置里
  配置的启动命令。启动项在设置 →「Agent」的「新建菜单」里按显示名和命令增删改。
- 顶栏的 `+` 新建标签：不弹选择框，直接继承当前会话的 cwd。

侧栏里的项目分组按各会话的工作目录派生。会话存在运行时的
`~/.smelt/smelt.sqlite3`。分屏布局存在这台桌面的 `~/.smelt/client.sqlite3`，
表是 `conversation_route`，列是 `dock_layout`。没有 `workspace.json`。

## 功能一览

**终端**
- 完整 ANSI / 256 色 / 24-bit 真彩色，Nerd Font 图标正常显示
- 能跑交互式程序和全屏 TUI：`claude`、`vim`、`htop` 都是真跑起来，不是阉割版伪终端
- 10000 行滚动回看，鼠标滚轮 / `Shift+PageUp` / `Shift+PageDown` 翻页
- 中文输入法（IME）原生支持
- 拖拽框选复制，双击选词，三击选行
- `Cmd+C` 复制选区，`Cmd+V` 粘贴剪贴板

**工作台外壳**
- 左侧是新对话、智能体、自动化、最近对话。能力库有四个平级页签：模型、插件、MCP、记忆
- 智能体列表按机器分组。已配对的远程机器上可以直接新建、编辑智能体；那些对话进入侧栏「最近对话」，并标出机器名
- 能力库里的「记忆」跟着当前选中的机器，不只改本机
- 多标签，每个标签独立 PTY / 历史 / 焦点，`+` 新建、`×` 关闭
- 分屏布局会自动存档。同一格还可以叠几个终端（终端 Dock）
- 右侧工具面板（`Opt+Cmd+B`）放文件、Git、历史，以及插件自己的页
- 每个会话按状态着色，多开几个 agent 时一眼看出该管哪个
- macOS 上，菜单栏（Dock 之外）常驻一个图标，点击会弹出菜单。Windows 第一版没有菜单栏图标，也没有 Dock 角标

**用量**
- 用量是各家账户额度（Claude、Copilot、Codex、Cursor、Grok），按 provider 查询，
  显示在会话行上。不解析 Claude Code 的 transcript，也没有独立的饼图窗口

**文件树视图**
- 支持文件名 / 内容混合搜索：一次搜索同时匹配文件名和文件内容，文件名命中排前、
  内容命中排后并显示行号 + 预览；点击结果会打开对应文件，但**不会自动跳转/定位
  到命中的那一行**
- `.md` 文件顶部有"编辑 / 预览"切换（其他文件类型没有这个切换），预览按
  markdown 渲染标题、列表、代码块等；编辑和预览共享同一份内容，随时切回继续改
- 右键文件/目录 →"发送到终端"：把路径转成相对项目根的 `@路径` 提及文本，写入
  **当前激活终端**的输入框（不自动回车）；在文件内容里选中一段文字右键 →
  "发送选中内容到终端"，发送的是选区文本（带文件名 + 代码块包裹）。两者都固定
  发给当前激活终端，不支持选择目标
- `Cmd+S` 保存：未保存时文件名旁有一个脏点提示；切换到其他未保存文件会弹
  "取消 / 不保存直接切换 / 保存并切换"三选一确认框；磁盘内容被外部改过时会提示
  "文件已被外部修改，再按一次 `Cmd+S` 强制覆盖"

**Git diff 视图**
- 改动一眼看清

**内置 MCP**
- `smeltd mcp add <id>` 把预设写进 Pi 的 `mcp.json`。内置 id：`playwright`、`figma`、`rea`、`browser-mcp`。已经有同名服务器就不动
- `playwright` 看目标机器有没有远程桌面（`~/.smelt/desktop/bin/display`）。有，就连常驻 Chrome：`--cdp-endpoint http://127.0.0.1:9222`。没有，就用浏览器扩展：`--extension`
- 自定义 stdio / HTTP MCP 仍在能力库里加

**Skill**
- 用户级在 `~/.agents/skills`，项目级在 `<项目>/.agents/skills`。这是本体
- 还不读这个目录的 agent，由技能插件从它自己的目录软链回去
- 旧的 `.smelt/skills` 会迁到同层级的 `.agents/skills`

**历史会话**
- 按来源列出当前项目下各家 agent 的本地历史，不只 Claude Code
- 左列表、右详情。有 ACP 身份的来源可以「ACP 继续」或「CLI/TUI 继续」

**通知**（菜单栏图标、Dock 角标和系统通知只在 macOS 上；Windows 第一版没有）
- Dock 图标角标：数字是"需要你处理 + 等你批准"的会话总数；只是切过去看一眼不会
  清除，要等你真正回应了才清
- macOS 系统通知（Notification Center）：只有 smelt 不在前台时才弹，标题固定
  smelt、副标题是"会话名 · 任务名"；同一条文本 60 秒内去重，不会被 Claude Code
  反复上报的 "waiting for input" 轰炸。仅打包成 `smelt.app` 后才会真投递，
  `cargo run` 开发版没有 bundle id，静默不弹
- app 在前台但你没在看那个标签页：不弹系统通知，改在窗口内弹一条 toast；正在看
  的那个标签直接不提示（你自己看得见）

## 远程访问

在这台机器上打开远程访问，用一次性配对串把电脑或手机接上来。配对过的设备经 iroh
连这台机器的 `smeltd`：优先打洞直连，连不上走 relay。没有长期 token，也没有
「允许远程写入」开关。

> 早先的浏览器面板、Cloudflare 隧道、自建 WebRTC 信令、JSON-WS 网关都已下线。
> 手机 App 在仓库的 `mobile/`（Flutter）。GitHub Releases 每次发版提供签名的 Android
> 安装包 `Smelt-<版本>-android-arm64.apk`。iOS 用 `make ios`（`scripts/build-ios.sh`）
> 打 TestFlight 归档。应用商店上架还没做。构建说明在
> [`mobile/README.md`](https://github.com/smelt-ai/smelt/blob/main/mobile/README.md)。

### 开启

设置（`Cmd+,`）→「远程」→「让其他设备连进来」：

- **远程访问**：打开后，配对过的设备可以连这台运行时。
- **自建 relay**：留空用 n0 的公共 relay。填写域名、IP 或完整 URL 则只用它，省略协议时自动补 `https://`。
- **配对新设备**：生成 `smelt+iroh://<endpoint>/?code=<一次性码>`，10 分钟、用一次。
  配对一台设备，就是把这台机器上你这个用户的 shell 交给它。
- **已配对的设备**：可以撤销，撤销后立刻断开。命令行等价物是 `smeltd devices revoke`。

这台运行时的 iroh 身份在 `~/.smelt/iroh-secret`，重启不变。没有 `remote-token`，
也没有 `collab.json`。远程开关记在运行时库里，守护重启之后自己恢复，
不会把远程关掉。

同网段会打洞直连，没有单独的局域网监听端口。跨网打不通时走 relay（留空就是公共 relay）。

### Linux 上的无头运行时

Linux 上可以只跑 `smeltd`，不装桌面。桌面在 macOS 和 Windows 上。

一键安装从 Release 拉取 `install.sh`。脚本按架构下载 `smeltd-<版本>-linux-<架构>.tar.gz`，对照同一份 `SHA256SUMS` 校验，再把参数交给包里的 `install.sh`：

```bash
curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash
curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash -s -- --with-desktop
```

也可以自己下载 `smeltd-<版本>-linux-<架构>.tar.gz`，解压后在包目录里执行 `./install.sh`。参数：

- **`--with-desktop`**：一并安装 TigerVNC / Xvnc、XFCE、Chrome 或 Chromium。配置三个用户级 systemd 服务：`smelt-display`、`smelt-desktop`、`smelt-chrome`。装完执行 `smeltd mcp add playwright`。Xvnc 起始几何是 1440×900，桌面打开远程桌面页后按页面大小改。Chrome 调试端口只绑 `127.0.0.1:9222`。
- **`--relay=<地址>`**：写入初始自定义 iroh 中继。域名、IP 或 URL 都可以，例如 `./install.sh --relay=https://relay.example.com`。不给就用 n0 的公共中继。
- **`--apt-mirror=<镜像>`**：这次安装把 Debian / Ubuntu 官方源换成指定镜像（主机名或 URL）。不给时先试官方源，连不上就按顺序自动换 `mirrors.aliyun.com`、`mirrors.tuna.tsinghua.edu.cn`、`mirrors.ustc.edu.cn` 里第一个连得上的。只用于这次安装，不改 `/etc/apt`。Bun 从 npmmirror 下，npm 包走 `registry.npmmirror.com`（用户自己配了源或代理就不动）。
- **`--no-pair`**：只安装并启动，不打印配对串。

之后：

```bash
smeltd pair                      # 打开远程访问，打印一次性配对串
smeltd devices                   # 列出已配对设备
smeltd devices revoke <endpoint> # 撤销并立刻断开
smeltd status
smeltd relay                     # 查看当前 iroh 中继
smeltd relay <url>               # 只用这个自建中继
smeltd relay off                 # 改回 n0 的公共中继
smeltd mcp add <id>              # 添加内置 MCP 预设
```

`smeltd mcp add` 的内置 id 有 `playwright`、`figma`、`rea`、`browser-mcp`。桌面端改某台远程机器的中继：侧栏那台机器的菜单里点「中继…」。控制接口是 `remote.config.set`，字段名是 `iroh_relay`。

`smeltd install` 只在 Linux 注册 systemd 单元。macOS 和 Windows 上守护由桌面按需拉起。Windows 没有 systemd。

### 一个窗口里的多台机器

桌面可以同时挂上本机和多台远程运行时。侧栏按运行时分块。在另一台机器上执行
`smeltd pair`，把配对串粘进「远程」页的「连到其他机器」。Git、文件、历史和额度
打到会话所在的那一台。连着远程运行时，「Agent」页标题旁会标「只作用于本机运行时」：
这一页改的是本机 CLI、Hooks 和启动命令，不改远程机器。

### 远程桌面

配对之后，桌面可以打开那台 Linux 的屏幕：运行时转发 VNC，本机用 noVNC 页面打开。
打开即接管；切到「只看」后不再发送键鼠。显示栈用上面的 `install.sh --with-desktop` 装好。

### 部署自建 Relay

需要一台有公网 IP 和域名的 Linux，放行 `80/TCP`、`443/TCP`、`7842/UDP`。自建 relay
不使用共享令牌：

```bash
./scripts/deploy-iroh-relay.sh \
  --ssh ubuntu@203.0.113.10 \
  --domain relay.example.com \
  --email admin@example.com
```

脚本在本机下载并校验 iroh 官方二进制后上传，再申请 Let's Encrypt 证书。完整步骤见
[`docs/iroh-relay-deployment.md`](https://github.com/smelt-ai/smelt/blob/main/docs/iroh-relay-deployment.md)。

### 手机出菜口

手机端是跨机器指挥台「出菜口」。启动后同时连接所有已配对机器，机器登记在 App 数据目录的 `smelt-mobile.sqlite3`，没有「当前只连一台」。

- 首屏是待拍板单据（权限审批、追问回答）和各机器正在执行、刚完成的任务，可以原地批准或回复
- 四栏导航：指挥台、会话、智能体、设置。平板是导航、列表、详情三栏
- 中文和英文
- 终端画面配色由手机自己定，不跟电脑下发
- 断线后读本机缓存：每个运行时最近的会话列表，以及最近几个对话快照（最多 5 段、合计 32MB）

Android 安装包是 Release 里的 `Smelt-<版本>-android-arm64.apk`。iOS 归档用 `make ios`。

### 闲置挂起

结构化对话闲置 30 分钟后，`smeltd` 停掉那场会话的宿主进程。会话还在列表里。
下次发消息、改配置或翻历史时先唤醒，并把挂起前的模型、权限模式和推理强度写回去。
正在跑、等审批、排队、有后台任务，或属于自动化 / 手机建的会话，不会挂起。

### 还没做的

- **联机 review**、**任务认领池**、**会话移交**、**飞书卡片**：多人协作仍是设计，见
  [collaboration.md](https://github.com/smelt-ai/smelt/blob/main/docs/collaboration.md)
- **应用商店上架**。Android 签名包已经随 Releases 发布，iOS 可以打 TestFlight 归档

[`remote-ops-roadmap.md`](https://github.com/smelt-ai/smelt/blob/main/docs/archive/remote-ops-roadmap.md)
是已删除的 JSON-WS 网关路线图，不是现行协议。

明确不打算做的：在飞书里内嵌完整终端、自建 WebRTC 信令或 WireGuard 中转、重新做浏览器面板、
把主 GUI 换成 Electron、做成一个完整的项目管理工具。

## 设置窗口

`Cmd+,` 打开设置，`Cmd+W`（Windows / Linux 是 `Ctrl+W`）关掉。左侧是三组，每组下面是独立的页：

- **通用**：外观、通知、快捷键、面板插件
- **Agent 与远程**：Agent、远程
- **系统**：资源占用、维护、关于

外观把主题、字号与字体、窗口与背景放在同一页，页内是「界面」「终端与代码」「窗口」。Agent 页是「本机 Agent」（CLI 和 Hooks）、「新建菜单」、「高级」（对话适配器命令）。远程页是「连到其他机器」和「让其他设备连进来」。维护页是「后台服务」和「旧版本残留」。关于页是版本、「更新」（仅 macOS）和「帮助与反馈」。

连着远程运行时，「Agent」页标题旁会标「只作用于本机运行时」。没连远程就不标。

「维护」里「后台服务」的「重启」和「重启以升级」会关掉后台守护进程。没有正在进行的会话时直接重启。有正在进行的会话时会先确认：这一轮回复会被中断，历史记录已保存，重启后可以继续对话。

## 快捷键

**全局**

| 快捷键 | 作用 |
|---|---|
| `Cmd+K` | 命令面板 |
| `Cmd+B` | 切换左侧栏 |
| `Opt+Cmd+B` | 切换右侧工具面板（文件 / Git / 历史） |
| `Cmd+[` / `Cmd+]` | 循环切换当前会话内的 pane（分屏） |
| `Cmd+1` ~ `Cmd+9` | 跳到侧栏第 N 个会话 |
| `Cmd+D` | 竖切分屏（右侧并排） |
| `Cmd+Shift+D` | 横切分屏（下方堆叠） |
| `Cmd+W` | 关闭当前 pane（会话只剩一个 pane 时关掉整个会话） |
| `Cmd+S` | 保存文件（文件内容可见时） |
| `Cmd+Shift+F` | 切换调试 HUD |
| `Cmd+Q` | 退出 |
| `Cmd+,` | 打开设置窗口 |
| `Cmd+W`（设置窗口） | 关闭设置窗口（Windows / Linux 是 `Ctrl+W`） |

同一格里叠着的终端用鼠标点 Dock 上的标签切换。`Cmd+D` / `Cmd+Shift+D` 仍是左右或上下分屏。

**终端内**

| 快捷键 | 作用 |
|---|---|
| `Cmd+C` | 复制选区 |
| `Cmd+V` | 粘贴剪贴板 |
| `Shift+PageUp` / `Shift+PageDown` | 翻滚历史缓冲 |
| `Cmd+点击` | 打开光标处识别到的链接 |

## 数据与配置

结构化状态在两份 SQLite，都在 `~/.smelt/`（Windows 上是 `%USERPROFILE%\.smelt`）：

| 位置 | 内容 |
|---|---|
| `~/.smelt/smelt.sqlite3` | 这台机器的会话、项目、智能体、自动化、配对设备和远程开关。分屏布局不在这里 |
| `~/.smelt/client.sqlite3` | 这台桌面的主题、字体、窗口背景、侧栏，以及连过哪些运行时。分屏布局在 `conversation_route.dock_layout` |
| `~/.smelt/bin/smelt-pi` | 内置 Pi 运行时，单个二进制，跑官方 JSONL RPC |
| `~/.smelt/runtime/` | 受管 Bun。ACP 适配器和 MCP 服务器按需从这里启动 |
| `~/.smelt/smeltd.sock` | `smeltd` 的本机 socket |
| `~/.smelt/iroh-secret` | 这台运行时的 iroh 身份 |

`workspace.json`、`appearance.json`、`launch.json`、`collab.json`、`pet.json`、`llm.json`
启动时会删掉，不再读写。没有桌面宠物，也没有任务看板。

历史会话读的是各家 agent 自己的本地存档。用量向 provider 查账户额度。

数据默认不出本机。打开远程访问之后，配对过的设备经 iroh 连上来；relay 看到的是加密流量。

## 常见问题

**历史会话页能恢复/继续某次历史对话吗？**
能。有 ACP 身份的来源可以选择「ACP 继续」或「CLI/TUI 继续」。

**支持 Linux / Windows 吗？**
支持。桌面有 macOS（Apple Silicon，`Smelt.dmg`，有自动更新）和 Windows 10 1803+ 的 x86_64 安装包（`smelt-<版本>-windows-x86_64-setup.exe`）。Windows 这一版不签名，没有自动更新，也没有 arm64 包；交互 shell 是 `cmd.exe`。Linux 可以无头跑 `smeltd`，再用 macOS、Windows 或手机配对。

**在哪反馈问题？**
去 [GitHub Issues](https://github.com/smelt-ai/smelt/issues/new/choose) 提，有 Bug 反馈
和功能建议两个模板。

**想从源码构建、了解内部架构、或者参与开发？**
这些不算"怎么用"，单独整理在[开发文档](/develop)里。
