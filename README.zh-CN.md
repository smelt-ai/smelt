[English](README.md) | 简体中文

<div align="center">

<img src="assets/icon-1024.png" alt="smelt" width="128">

# smelt

**Mac 上的 AI coding 驾驶舱 —— 一个专为「同时指挥多个 CLI coding agent 干活」设计的桌面工作台。**

基于 [GPUI](https://gpui.rs) 的原生应用，内嵌真终端，多项目 × 多标签。
Claude Code、Codex、Gemini CLI……凡是跑在终端里的 agent，都能在这里并排看住。

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
应用内置在线更新，后续版本会自动检查并静默下载。

> 目前仅支持 **macOS（Apple Silicon）**。

## 功能

- **工作台**：多项目 × 多标签内嵌真终端（跑 `claude`/`codex`/`copilot`/`vim`/`htop` 等交互式程序与全屏 TUI），分屏、命令面板（`Cmd+K`）、可自定义的快捷启动
- **看住 agent**：靠终端标题（OSC 0/2）+ OSC 9/777 通知 + 响铃感知会话状态（等你批准 / 等你输入 / 跑着呢 / 有结果可看），需要关注时角标提醒 + 系统通知；不依赖任何一家的私有格式
- **读写代码**：文件树 + 搜索、内置编辑器、Git diff 视图、Markdown/Mermaid 渲染
- **Claude Code 专属**：用量统计、历史会话与记忆浏览（读本地 `~/.claude/projects/**` transcript）
- **远程访问**（默认关闭）：用 Smelt 手机 App 查看/操控本机 agent 会话，局域网直连，跨网走 iroh P2P（打洞直连，打不通自动回退中继），配对二维码永久有效。
  **配对码即权限，务必先读[远程访问文档](https://github.com/smelt-ai/smelt/blob/main/website/content/docs.md#远程访问)再开**
- **其它**：终端会话持久化（GUI 退出/崩溃不影响 shell，重开自动 reattach）、可选接 LLM 大脑的桌面宠物

完整功能清单、快捷键与架构细节见 [`docs/workspace.md`](docs/workspace.md)；产品方向见 [`docs/product-roadmap.md`](docs/product-roadmap.md)。

## 从源码构建

需要 Rust stable 与 macOS。**无需安装完整 Xcode**——项目通过 `gpui_platform` 的 `runtime_shaders` feature 把 Metal 着色器改到运行时编译，只装 Command Line Tools 即可。

打包 DMG 的 `make dist-build` 需要 **Python 3.10+**（Command Line Tools 自带的 `/usr/bin/python3` 可能仍是 3.9，版本不够可 `brew install python` 或用 `SMELT_PYTHON=/opt/homebrew/bin/python3 make dist-build` 指定解释器）。

```sh
cargo run --bin smelt       # 开发模式直接跑 GUI
make dist-build             # 编译 release 并打包出 dist/Smelt.dmg
make help                   # 查看全部构建目标
```

跑测试与类型检查：

```sh
cargo check --all-targets
cargo test
```

## 架构

Rust 2021 + [GPUI](https://github.com/zed-industries/zed) / [gpui-component](https://github.com/longbridge/gpui-component)（GUI）、portable-pty + alacritty_terminal（内嵌终端）、tokio、axum（远程网关）、iroh（P2P 隧道）。
配置放 `~/.smelt/`。

仓库产出四个二进制，日常只需要跑第一个：`smelt`（GUI 主程序）、`smeltd`（终端持久化守护，类 tmux，由 GUI 按需拉起，不需要手动运行）、`gateway`（远程网关的独立可执行版，用于开发调试）、`smelt-notify`（Claude Code hooks 调用的状态上报小工具）。
移动端在 `mobile/`（Flutter），经 `crates/smelt-mobile` 复用 Rust 侧的 iroh 隧道。

详细架构、目录结构与已实现功能清单见 [`docs/workspace.md`](docs/workspace.md)，产品主航道见 [`docs/product-roadmap.md`](docs/product-roadmap.md)，杂项 backlog 见 [`docs/roadmap.md`](docs/roadmap.md)，远程协议细节见 [`docs/remote-ops-roadmap.md`](docs/remote-ops-roadmap.md)。

## 贡献

欢迎 issue 与 PR。提交前请确保 `cargo check --all-targets` 与 `cargo test` 通过，commit message 遵循 [Conventional Commits](https://www.conventionalcommits.org/)。

## License

[Apache License 2.0](LICENSE)
