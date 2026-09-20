[English](README.md) | 简体中文

<div align="center">

<img src="assets/icon-1024.png" alt="smelt" width="128">

# smelt

**Mac 上的 AI coding 驾驶舱 —— 一个专为「同时指挥多个 CLI coding agent 干活」设计的桌面工作台。**

基于 [GPUI](https://gpui.rs) 的原生应用，内嵌真终端，多项目 × 多标签。
Claude Code、GitHub Copilot、Codex、Grok、Cursor、OpenCode、Kiro、Pi、Crush、DSH……凡是跑在终端里的 agent，都能在这里并排看住。

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/smelt-ai/smelt)](https://github.com/smelt-ai/smelt/releases)
[![Platform](https://img.shields.io/badge/platform-macOS%20(Apple%20Silicon)-lightgrey)](https://github.com/smelt-ai/smelt/releases)

> **状态**：working prototype，持续迭代中。

</div>

---

## 为什么

AI 插件让编辑器更聪明，但人还是那个敲键盘的苦力。当 agent 能独立跑完读代码、改代码、跑测试、提交的整条链路，人的角色就该从「打字的人」变成「看导航、下指令的人」。

这时候需要的不是一个更聪明的编辑器，而是一个能同时看住好几个正在跑的 agent 的**驾驶舱**。smelt 把终端——agent 真正干活的地方——变成主战场。

## 安装

从 [Releases](https://github.com/smelt-ai/smelt/releases) 下载 `Smelt.dmg`，拖进 Applications 即可。
应用在启动时与手动检查更新（stable/beta 渠道），有新版本时会提示。

> 目前仅支持 **macOS（Apple Silicon）**。

## 功能

- **工作台**：多项目 × 多标签内嵌真终端（跑 `claude`/`codex`/`copilot`/`pi`/`vim`/`htop` 等交互式程序与全屏 TUI），分屏、命令面板（`Cmd+K`）、可自定义的快捷启动
- **看住 agent**：基于 agent hook（`smelt-notify`）与结构化协议（ACP 或 provider 原生协议）跟踪会话状态（等你批准 / 等你输入 / 跑着呢 / 有结果可看）。终端标题（OSC 0/2）仅作展示，OSC 9/777 通知与响铃仅作信息提示；需要关注时角标提醒 + 系统通知；不依赖任何一家的私有格式
- **原生 agent 对话**：通过 ACP 或 provider 原生协议接入 Claude Code、GitHub Copilot、Codex、Grok、Cursor、OpenCode、Kiro、Pi、DSH，提供结构化消息流、工具卡片、内嵌 diff、权限审批、PLAN 进度和可续接会话（Pi 由受管 Bun 启动其官方 JSONL RPC，不经 ACP 转换）
- **持久化智能体与自动化**：左侧「智能体」管理可复用定义，「自动化」独立管理时机、每次运行的输入、权限与结果历史；每条自动化由 daemon 在 `~/.smelt/workspaces/automations` 下分配稳定工作区。时机是定时或外部触发：定时仍是工作日/每天/按间隔；外部触发是本机 `127.0.0.1` 上的接收地址，同机程序 POST JSON 即可开 Run。目录只显示「外部触发」，不展示令牌。当前引擎只开放 Pi，定时、外部触发和手动 Run 均由 `smeltd` 托管
- **读写代码**：文件树 + 搜索、内置编辑器、Git diff 视图、Markdown/Mermaid 渲染
- **Claude Code 专属**：用量统计、历史会话与记忆浏览（读本地 `~/.claude/projects/**` transcript）
- **远程访问**（默认关闭）：用 Smelt 手机 App 查看/操控本机结构化 agent 对话与实时终端/CLI agent 会话。终端页直接渲染守护转发的 PTY 流，并按当前手机视口调整共享 PTY 尺寸。连接统一走 iroh P2P：同网设备可以直连，跨网先打洞，失败后回退到用户配置的 relay。应用不会暴露普通局域网监听端口。配对 Token 会跨重启保留，直到手动刷新。
  **配对码即权限，务必先读[远程访问文档](docs/remote-ops-roadmap.md)再开**
- **其它**：终端会话持久化（GUI 退出/崩溃不影响 shell，重开自动 reattach）

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
make check-all              # rustfmt + clippy + 测试 + 插件 SDK
make test                   # cargo nextest（与 CI 同一运行器）
```

## 架构

Rust 2024 + [GPUI](https://github.com/zed-industries/zed) / [gpui-component](https://github.com/zzfn/gpui-component)（GUI）、portable-pty + alacritty_terminal（内嵌终端）、tokio、axum（远程网关）、iroh（P2P 隧道）。
配置放 `~/.smelt/`。

工作区包含 `smelt`（GUI 主程序）、`smeltd`（终端持久化守护，类 tmux，由 GUI 按需拉起）、`gateway`（远程网关的独立可执行版，用于开发调试）、`smelt-notify`（Claude Code hooks 调用的状态上报小工具），以及用于开发和调试 P2P 隧道的 `smelt-iroh-host` / `smelt-iroh-connect`。普通用户主要使用 `smelt`，守护进程由 GUI 按需管理。
移动端在 `mobile/`（Flutter），经 `crates/smelt-mobile` 复用 Rust 侧的 iroh 隧道，iOS 与 Android 均可运行，目前尚未上架，需要自行构建，见 [`mobile/README.md`](mobile/README.md)。

详细架构、目录结构与已实现功能清单见 [`docs/workspace.md`](docs/workspace.md)，产品主航道见 [`docs/product-roadmap.md`](docs/product-roadmap.md)，杂项 backlog 见 [`docs/roadmap.md`](docs/roadmap.md)，远程协议细节见 [`docs/remote-ops-roadmap.md`](docs/remote-ops-roadmap.md)。

## 贡献

欢迎 issue 与 PR。提交前请确保 `make check-all` 通过，commit message 遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

## License

[Apache License 2.0](LICENSE)
