[English](README.md) | 简体中文

<div align="center">

<img src="assets/icon-1024.png" alt="smelt" width="128">

# smelt

**macOS 与 Windows 上的 AI coding 驾驶舱 —— 一个专为「同时指挥多个 CLI coding agent 干活」设计的桌面工作台。**

基于 [GPUI](https://gpui.rs) 的原生应用，内嵌真终端，多项目 × 多标签。
Claude Code、GitHub Copilot、Codex、Grok、Cursor、OpenCode、Kiro、Pi、Crush……凡是跑在终端里的 agent，都能在这里并排看住。

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/smelt-ai/smelt)](https://github.com/smelt-ai/smelt/releases)
[![Platform](https://img.shields.io/badge/platform-macOS%20%2B%20Windows%20desktop%20%2B%20Linux%20runtime-lightgrey)](https://github.com/smelt-ai/smelt/releases)

> **状态**：working prototype，持续迭代中。桌面 App 跑在 macOS（Apple Silicon）和 Windows 10 1803+（x86_64）上；运行时 `smeltd` 也可以无头跑在 Linux 上。Windows 是不签名的安装包。

</div>

---

## 为什么

AI 插件让编辑器更聪明，但人还是那个敲键盘的苦力。当 agent 能独立跑完读代码、改代码、跑测试、提交的整条链路，人的角色就该从「打字的人」变成「看导航、下指令的人」。

这时候需要的不是一个更聪明的编辑器，而是一个能同时看住好几个正在跑的 agent 的**驾驶舱**。smelt 把终端——agent 真正干活的地方——变成主战场。

## 安装

### macOS（Apple Silicon）

从 [Releases](https://github.com/smelt-ai/smelt/releases) 下载 `Smelt.dmg`，拖进 Applications 即可。
应用在启动时与手动检查更新（stable/beta 渠道），有新版本时会提示。

### Windows（x86_64）

从同一个 [Releases](https://github.com/smelt-ai/smelt/releases) 页下载 `smelt-<版本>-windows-x86_64-setup.exe` 运行。按当前用户安装，不要管理员权限，默认装到 `%LOCALAPPDATA%\Programs\Smelt`，建开始菜单项，可选把 `smelt` 命令加入 PATH。和 macOS 一样，启动时和手动都会检查更新（生产版 / 内测版通道）；应用更新时 Smelt 退出，新版安装程序静默装好后重新打开，后台会话不受影响。

数据目录是 `%USERPROFILE%\.smelt`。

这一版 Windows：

- 只有 x86_64，系统是 Windows 10 1803 及以上。
- 安装程序和 exe 都不签名。SmartScreen 可能提示未知发布者，选择仍要运行即可。
- 交互 shell 是 `cmd.exe`（ConPTY），或 `COMSPEC` 指向的程序。
- 没有菜单栏图标、Dock 角标、系统通知。这些只在 macOS 上。

### Linux 无头运行时

Linux 机器可以无头跑 `smeltd`，再从 macOS、Windows 或手机配对过去。

```sh
curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash
curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash -s -- --with-desktop
```

也可以下载 `smeltd-<版本>-linux-<架构>.tar.gz`，解压后运行 `./install.sh`。`--with-desktop` 会装上 TigerVNC、XFCE 和常驻 Chrome，并执行 `smeltd mcp add playwright`。`--relay=<地址>` 写入初始 iroh 中继。`--apt-mirror=<镜像>` 指定这次安装用的 apt 源。

```sh
smeltd status
smeltd pair
smeltd devices
smeltd relay [<url>|off]     # 查看、设置或清除自定义 iroh 中继
smeltd mcp add <id>          # playwright、figma、rea、browser-mcp
```

## 功能

- **工作台**：多项目 × 多标签内嵌真终端（跑 `claude`/`codex`/`copilot`/`pi`/`vim`/`htop` 等交互式程序与全屏 TUI），分屏、命令面板（`Cmd+K`）、可自定义的快捷启动
- **看住 agent**：基于 agent hook（`smelt-notify`）与结构化协议（ACP 或 provider 原生协议）跟踪会话状态（等你批准 / 等你输入 / 跑着呢 / 有结果可看）。会话名来自 ACP `session_title` 或 hook `SessionTitleChanged`，没有时用首条用户请求；OSC 0/2 不是会话名。OSC 9/777 通知与响铃仅作信息提示；需要关注时角标提醒 + 系统通知；不依赖任何一家的私有格式
- **原生 agent 对话**：通过 ACP 或 provider 原生协议接入 Claude Code、GitHub Copilot、Codex、Grok、Cursor、OpenCode、Kiro、Crush、Pi，提供结构化消息流、工具卡片、内嵌 diff、权限审批、PLAN 进度和可续接会话。Pi 编译为单二进制 `smelt-pi`（`~/.smelt/bin/smelt-pi`），跑官方 JSONL RPC，不经 ACP 转换。受管 Bun（`~/.smelt/runtime`）按需启动 ACP 适配器和 MCP 服务器
- **持久化智能体与自动化**：左侧「智能体」管理可复用定义，「自动化」独立管理时机、每次运行的输入与结果历史。智能体自动化在该智能体 space 下分配稳定目录（`~/.smelt/agents/<id>/automations/`）；Shell 自动化留在 `~/.smelt/workspaces/automations`。时机是定时或外部触发：定时仍是工作日/每天/按间隔；外部触发是本机 `127.0.0.1` 上的接收地址，同机程序 POST JSON 即可开 Run。目录只显示「外部触发」，不展示令牌。当前引擎只开放 Pi，定时、外部触发和手动 Run 均由 `smeltd` 托管。自动化固定全权限跑，不另存一套权限策略
- **读写代码**：文件树 + 搜索、内置编辑器、Git diff 视图、Markdown/Mermaid 渲染
- **用量与历史**：用量按 provider 查账户（Claude、Copilot、Codex、Cursor、Grok），不读 transcript。历史会话读各家本机存档，不只 Claude Code
- **远程访问**（默认关闭）：把另一台 Smelt 桌面或手机 App 配对到这台机器的运行时，连接走 iroh。同网直连，否则打洞，再走 relay。relay 留空用 n0 的公共 relay，也可以填自建地址。配对串是 `smelt+iroh://<endpoint_id>/?code=<一次性码>`，10 分钟内、只能用一次。没有长期共享 token，也没有单独的「允许远程写入」开关：配对就是把这台机器上当前用户的 shell 交给那台设备。在设置里撤销设备，或执行 `smeltd devices revoke`。应用不开放普通局域网监听端口。
  **配对码即权限，开之前先读 [relay 说明](docs/iroh-relay-deployment.md)**
- **其它**：终端会话持久化（GUI 退出/崩溃不影响 shell，重开自动 reattach）。升级或重启本机 `smeltd` 会停掉它（各平台都一样）：正在跑的终端会话会关掉，进行中的回合会中断，对话历史保留。同一格终端可以叠成 Dock，也可以分屏。结构化对话闲置 30 分钟会挂起宿主进程，再用时唤醒，并恢复挂起前的模型、权限模式和推理强度。一个窗口可以挂多台运行时，包括无头 Linux，并能经 noVNC 看那台机器的桌面。细节见 [`docs/workspace.md`](docs/workspace.md)。

完整功能清单、快捷键与架构细节见 [`docs/workspace.md`](docs/workspace.md)；产品方向见 [`docs/product-roadmap.md`](docs/product-roadmap.md)。

## 从源码构建

需要 Rust stable。编 macOS App 还需要 macOS。**无需安装完整 Xcode**——项目通过 `gpui_platform` 的 `runtime_shaders` feature 把 Metal 着色器改到运行时编译，只装 Command Line Tools 即可。

打包 DMG 的 `make dist-build` 需要 **Python 3.10+**（Command Line Tools 自带的 `/usr/bin/python3` 可能仍是 3.9，版本不够可 `brew install python` 或用 `SMELT_PYTHON=/opt/homebrew/bin/python3 make dist-build` 指定解释器）。

```sh
make dev                    # 编译 release 并原子安装到 /Applications
cargo run --bin smelt       # 仅运行 GUI，需确保 smeltd 已经可用
make dist-build             # 编译 release 并打包出 dist/Smelt.dmg
make help                   # 查看全部构建目标
```

Windows 的安装包在 Windows 上编，不走 `make dist-build`。发版任务是 `cargo build --release -p smelt -p smelt-cli -p smeltd --locked`，再跑 `bash scripts/build-smelt-pi.sh`。见 [`docs/release.md`](docs/release.md)。

跑测试与类型检查：

```sh
make check-all              # rustfmt + clippy + 测试 + Pi Agent
make test                   # cargo nextest（与 CI 同一运行器）
```

## 架构

Rust 2024 + [GPUI](https://github.com/zed-industries/zed)（`gpui-pre` 0.3.9）/ [gpui-kit](https://github.com/longbridge/gpui-kit)（GUI）、portable-pty + alacritty_terminal（内嵌终端）、tokio、iroh（远程连接）。axum 用于自动化 webhook 服务（只听本机回环）。结构化状态在 `~/.smelt/` 的 SQLite（Windows 上是 `%USERPROFILE%\.smelt`）：运行时库 `smelt.sqlite3`，这台桌面的客户端库 `client.sqlite3`。见 [`docs/local-storage.md`](docs/local-storage.md)。

工作区包含 `smelt`（GUI 主程序）、`smeltd`（运行时守护：PTY、ACP 宿主、自动化；macOS 与 Windows 桌面按需拉起，在 Linux 上可以装成 systemd 用户服务）、`smelt-pi`（Pi 运行时单二进制）、`smelt-notify`（agent hook 的状态上报）、`smelt-installer`（退出后替换 macOS App 的安装助手）。`smelt-iroh-connect` 用来调试运行时的 iroh 路径。普通用户主要使用 `smelt`，本机守护由 GUI 按需管理。
Windows 上，Rust 客户端（桌面、`smelt-notify`、runtime client）连 `%USERPROFILE%\.smelt\smeltd.sock`（经 `uds_windows` 的 AF_UNIX）。Bun 和 Pi 走当前用户的命名管道。子进程放进 Job Object（`KILL_ON_JOB_CLOSE`）。受管 Bun 下载 `bun-windows-x64.zip`，装到 `%USERPROFILE%\.smelt\runtime`。
移动端在 `mobile/`（Flutter），是跨机器指挥台「出菜口」。启动后同时连接所有已配对机器（登记在 `smelt-mobile.sqlite3`）。首屏是待拍板单据和各机器的实时进展。四栏是指挥台、会话、智能体、设置；平板是三栏；中英双语。`crates/smelt-mobile` 是它的 Rust 内核，走和桌面同一套 `smelt-runtime-client`。每次发版的 GitHub Release 提供签名的 `Smelt-<版本>-android-arm64.apk`。iOS 用 `make ios`（`scripts/build-ios.sh`）打 TestFlight 归档。见 [`mobile/README.md`](mobile/README.md)。

架构、目录、Linux 安装、多运行时窗口、远程桌面和闲置挂起见 [`docs/workspace.md`](docs/workspace.md)。产品主航道见 [`docs/product-roadmap.md`](docs/product-roadmap.md)，杂项 backlog 见 [`docs/roadmap.md`](docs/roadmap.md)。

## 文档

文档入口是 [`docs/README.md`](docs/README.md)。现行说明：[`工作台`](docs/workspace.md)、[`本地存储`](docs/local-storage.md)、[`iroh relay`](docs/iroh-relay-deployment.md)、[`Windows`](docs/windows-support.md)、[`产品路线图`](docs/product-roadmap.md)、[`杂项 backlog`](docs/roadmap.md)、[`视觉设计语言`](docs/ui-design-language.md)、[手机客户端](mobile/README.md)。

## 贡献

欢迎 issue 与 PR。提交前请确保 `make check-all` 通过，commit message 遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

## License

[Apache License 2.0](LICENSE)
