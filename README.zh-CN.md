[English](README.md) | 简体中文

<div align="center">

<img src="assets/icon-1024.png" alt="smelt" width="128">

# smelt

**Mac 上的 AI coding 驾驶舱 —— 一个专为「同时指挥多个 CLI coding agent 干活」设计的桌面工作台。**

基于 [GPUI](https://gpui.rs) 的原生应用，内嵌真终端，多项目 × 多标签。
Claude Code、GitHub Copilot、Codex、Grok、Cursor、OpenCode、Kiro、Pi、Crush、DSH……凡是跑在终端里的 agent，都能在这里并排看住。

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/smelt-ai/smelt)](https://github.com/smelt-ai/smelt/releases)
[![Platform](https://img.shields.io/badge/platform-macOS%20desktop%20%2B%20Linux%20runtime-lightgrey)](https://github.com/smelt-ai/smelt/releases)

> **状态**：working prototype，持续迭代中。桌面 App 是 macOS（Apple Silicon）；运行时 `smeltd` 也可以无头跑在 Linux 上。

</div>

---

## 为什么

AI 插件让编辑器更聪明，但人还是那个敲键盘的苦力。当 agent 能独立跑完读代码、改代码、跑测试、提交的整条链路，人的角色就该从「打字的人」变成「看导航、下指令的人」。

这时候需要的不是一个更聪明的编辑器，而是一个能同时看住好几个正在跑的 agent 的**驾驶舱**。smelt 把终端——agent 真正干活的地方——变成主战场。

## 安装

从 [Releases](https://github.com/smelt-ai/smelt/releases) 下载 `Smelt.dmg`，拖进 Applications 即可。
应用在启动时与手动检查更新（stable/beta 渠道），有新版本时会提示。

> 桌面 App 目前支持 **macOS（Apple Silicon）**。Linux 机器可以无头跑 `smeltd`，再从 Mac 或手机配对过去。

## 功能

- **工作台**：多项目 × 多标签内嵌真终端（跑 `claude`/`codex`/`copilot`/`pi`/`vim`/`htop` 等交互式程序与全屏 TUI），分屏、命令面板（`Cmd+K`）、可自定义的快捷启动
- **看住 agent**：基于 agent hook（`smelt-notify`）与结构化协议（ACP 或 provider 原生协议）跟踪会话状态（等你批准 / 等你输入 / 跑着呢 / 有结果可看）。会话名来自 ACP `session_title` 或 hook `SessionTitleChanged`，没有时用首条用户请求；OSC 0/2 不是会话名。OSC 9/777 通知与响铃仅作信息提示；需要关注时角标提醒 + 系统通知；不依赖任何一家的私有格式
- **原生 agent 对话**：通过 ACP 或 provider 原生协议接入 Claude Code、GitHub Copilot、Codex、Grok、Cursor、OpenCode、Kiro、Pi、DSH，提供结构化消息流、工具卡片、内嵌 diff、权限审批、PLAN 进度和可续接会话（Pi 由受管 Bun 启动其官方 JSONL RPC，不经 ACP 转换）
- **持久化智能体与自动化**：左侧「智能体」管理可复用定义，「自动化」独立管理时机、每次运行的输入与结果历史。智能体自动化在该智能体 space 下分配稳定目录（`~/.smelt/agents/<id>/automations/`）；Shell 自动化留在 `~/.smelt/workspaces/automations`。时机是定时或外部触发：定时仍是工作日/每天/按间隔；外部触发是本机 `127.0.0.1` 上的接收地址，同机程序 POST JSON 即可开 Run。目录只显示「外部触发」，不展示令牌。当前引擎只开放 Pi，定时、外部触发和手动 Run 均由 `smeltd` 托管。自动化固定全权限跑，不另存一套权限策略
- **读写代码**：文件树 + 搜索、内置编辑器、Git diff 视图、Markdown/Mermaid 渲染
- **用量与历史**：用量按 provider 查账户（Claude、Copilot、Codex、Cursor、Grok），不读 transcript。历史会话读各家本机存档，不只 Claude Code
- **远程访问**（默认关闭）：把另一台 Smelt 桌面或手机 App 配对到这台机器的运行时，连接走 iroh。同网直连，否则打洞，再走 relay。relay 留空用 n0 的公共 relay，也可以填自建地址。配对串是 `smelt+iroh://<endpoint_id>/?code=<一次性码>`，10 分钟内、只能用一次。没有长期共享 token，也没有单独的「允许远程写入」开关：配对就是把这台机器上当前用户的 shell 交给那台设备。在设置里撤销设备，或执行 `smeltd devices revoke`。应用不开放普通局域网监听端口。
  **配对码即权限，开之前先读 [relay 说明](docs/iroh-relay-deployment.md)**
- **其它**：终端会话持久化（GUI 退出/崩溃不影响 shell，重开自动 reattach）。同一格终端可以叠成 Dock，也可以分屏。结构化对话闲置 30 分钟会挂起宿主进程，再用时唤醒，并恢复挂起前的模型、权限模式和推理强度。一个窗口可以挂多台运行时，包括无头 Linux，并能经 noVNC 看那台机器的桌面。细节见 [`docs/workspace.md`](docs/workspace.md)。

完整功能清单、快捷键与架构细节见 [`docs/workspace.md`](docs/workspace.md)；产品方向见 [`docs/product-roadmap.md`](docs/product-roadmap.md)。

## 从源码构建

需要 Rust stable 与 macOS。**无需安装完整 Xcode**——项目通过 `gpui_platform` 的 `runtime_shaders` feature 把 Metal 着色器改到运行时编译，只装 Command Line Tools 即可。

打包 DMG 的 `make dist-build` 需要 **Python 3.10+**（Command Line Tools 自带的 `/usr/bin/python3` 可能仍是 3.9，版本不够可 `brew install python` 或用 `SMELT_PYTHON=/opt/homebrew/bin/python3 make dist-build` 指定解释器）。

```sh
make run                    # 编译 GUI、守护进程和通知器，然后运行 GUI
cargo run --bin smelt       # 仅运行 GUI，需确保 smeltd 已经可用
make dist-build             # 编译 release 并打包出 dist/Smelt.dmg
make help                   # 查看全部构建目标
```

跑测试与类型检查：

```sh
make check-all              # rustfmt + clippy + 测试 + Pi Agent
make test                   # cargo nextest（与 CI 同一运行器）
```

## 架构

Rust 2024 + [GPUI](https://github.com/zed-industries/zed)（`gpui-pre` 0.3.9）/ [gpui-kit](https://github.com/longbridge/gpui-kit)（GUI）、portable-pty + alacritty_terminal（内嵌终端）、tokio、iroh（远程连接）。axum 只负责本机回环上的自动化 webhook。结构化状态在 `~/.smelt/` 的 SQLite：运行时库 `smelt.sqlite3`，这台桌面的客户端库 `client.sqlite3`。见 [`docs/local-storage.md`](docs/local-storage.md)。

工作区包含 `smelt`（GUI 主程序）、`smeltd`（运行时守护：PTY、ACP 宿主、自动化；由 GUI 按需拉起，在 Linux 上则装成 systemd 用户服务）、`smelt-notify`（agent hook 的状态上报）、`smelt-agent-mcp`（跨 agent 消息的 stdio MCP 适配器）、`smelt-installer`（退出后替换 App 的安装助手）。`smelt-iroh-connect` 用来调试运行时的 iroh 路径。普通用户主要使用 `smelt`，本机守护由 GUI 按需管理。
移动端在 `mobile/`（Flutter）。`crates/smelt-mobile` 是它的 Rust 内核（设备身份、配对、目录、对话、终端、离线 SQLite 缓存），走和桌面同一套 `smelt-runtime-client`。目前尚未上架，需要自行构建，见 [`mobile/README.md`](mobile/README.md)。

架构、目录、Linux 安装、多运行时窗口、远程桌面和闲置挂起见 [`docs/workspace.md`](docs/workspace.md)。产品主航道见 [`docs/product-roadmap.md`](docs/product-roadmap.md)，杂项 backlog 见 [`docs/roadmap.md`](docs/roadmap.md)。[`docs/remote-ops-roadmap.md`](docs/remote-ops-roadmap.md) 写的是已经删除的 JSON-WS 网关，不是现行协议。

## 贡献

欢迎 issue 与 PR。提交前请确保 `make check-all` 通过，commit message 遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

## License

[Apache License 2.0](LICENSE)
