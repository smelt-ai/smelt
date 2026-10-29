# 开发

面向想从源码构建、了解内部架构、或者参与开发的人。日常使用 smelt 请看
[使用文档](/docs)。

## 从源码构建

下面是 macOS。需要 Rust 工具链和 Xcode Command Line Tools（不需要完整 Xcode）。Windows 的发版 zip 在 Windows 上编，见后面的「打包发布」。

```sh
cargo run --bin smelt   # GUI 主程序
```

GUI 启动时会自动去可执行文件同目录找 `smeltd` 并拉起。只跑过 `cargo run --bin
smelt` 的话，`target/debug/` 下还没有 `smeltd`，自动拉起会静默落空——不影响
正常使用，只是这次跑起来的终端会话不会持久化。想要持久化能力，先单独构建一次：

```sh
cargo build --bin smeltd
```

之后 smelt 每次启动都会在同目录找到它并自动拉起，不需要另开一个终端手动跑
`cargo run --bin smeltd`（除非你想单独盯着它的日志调试）。

## 二进制与架构

核心二进制：

- **smelt**（`crates/smelt/`）—— GUI 本体：多项目 × 多标签、文件树、Git、智能体和自动化。
- **smeltd**（`crates/smeltd/`）—— 后台守护，管 PTY / ACP 子进程生命周期。macOS 和
  Windows 桌面按需拉起它，Linux 上可以装成 systemd 用户服务。跟 GUI 之间靠本机
  socket 通信（attach 时先回报真实尺寸，再发网格 keyframe 快照与增量输出，重放区间
  按 `replay_len` 划界）。Windows 上 Rust 客户端连 `%USERPROFILE%\.smelt\smeltd.sock`
  （AF_UNIX，`stream/windows.rs` 自己包的非阻塞套接字），Bun / Pi 走当前用户的命名管道。就算 GUI 挂了，`smeltd`
  照样把 shell 养着；重开 GUI 按会话 id 自动 reattach。升级守护是先优雅停机再换
  新版本，不交接 fd，进行中的终端会话和回合会中断。手机和另一台桌面经 iroh 连的也是它。
- **smelt-pi** —— Pi 运行时单二进制，跑官方 JSONL RPC。`make smelt-pi` 走
  `scripts/build-smelt-pi.sh`。装到用户机器上是 `~/.smelt/bin/smelt-pi`。受管 Bun
  在 `~/.smelt/runtime`，只负责按需启动 ACP 适配器和 MCP 服务器。

辅助二进制：`smelt-notify`（agent hook 状态上报，在
`crates/smeltd/src/bin/`）、`smelt-installer`（打包进 App，退出后替换安装）、`smelt-iroh-connect`
（iroh 调试）。没有 `gateway`、`smelt-task`、`smelt-iroh-host`。`crates/smelt-mobile`
是给 Flutter 手机端用的 Rust 内核，经 `smelt-runtime-client` 连运行时，不是隧道。

常用目标：

```sh
make dev          # 编译 release 并安装到 /Applications
make smelt-pi     # 编译 smelt-pi
make check-all    # clippy、shell、路径、测试、Pi Agent
make test         # cargo nextest
make ios          # scripts/build-ios.sh，在发版 tag 上出 iOS 包
make dist-build   # macOS dmg
```

Linux 运行时包是 `scripts/package-linux.sh`。Windows 安装包不在这些 Make 目标里：发版的 `windows` job 在 `windows-latest` 上执行 `cargo build --release -p smelt -p smelt-cli -p smeltd --locked`，再用 `scripts/build-smelt-pi.sh` 编 `smelt-pi.exe`，用 Inno Setup 按 `scripts/windows-installer.iss` 编成 `smelt-<版本>-windows-x86_64-setup.exe`。第一版不签名；桌面的应用内更新下载的就是它。日期版本号是 `YYYY.MDD.N`（当天第 N 个号，预发布加 `-rc.N`），由 `scripts/release.sh` 取号。规则见 [`docs/release.md`](https://github.com/smelt-ai/smelt/blob/main/docs/release.md)，Windows 行为见 [`docs/windows-support.md`](https://github.com/smelt-ai/smelt/blob/main/docs/windows-support.md)。

两者解耦：`smelt` 可以单独跑（这时终端会话就跟着 GUI 生命周期走），也可以接上
`smeltd` 拿到跨重启的会话持久化。打包发布时核心二进制会被塞进同一个
`Smelt.app/Contents/MacOS/` 目录，`smelt` 靠 `current_exe().with_file_name
("smeltd")` 按同目录寻址、按需自动拉起它——这也是打包版用户完全不用关心 `smeltd`
的原因。

## 技术栈

- Rust 2024，tokio async
- GPUI（`gpui-pre` 0.3.9）+ gpui-component（桌面 UI，GPU 渲染；组件 crate 名仍是 gpui-component）
- portable-pty + alacritty_terminal（内嵌终端：PTY + ANSI 状态机）

## 打包发布

```sh
./scripts/package-mac.sh --build
# 或
make dist-build
```

会先 `cargo build --release` 编译核心二进制，再组装成 `dist/Smelt.app`，最后打成
定制过拖拽安装窗口的 `dist/Smelt.dmg`（仅 Apple Silicon / arm64；带签名——优先用
`scripts/setup-codesign-identity.sh` 生成的自签名身份，找不到时回退 ad-hoc，两种
都过不了 Gatekeeper 公证）。不加 `--build` 则跳过编译，直接用已有的 release
产物组装。

Windows 没有对应的 `package-windows.sh`。安装包由 `.github/workflows/release.yml` 的
`windows` job 在 `windows-latest` 上用 Inno Setup 按 `scripts/windows-installer.iss` 编，装进去的是：`smelt.exe`、`smelt.com`（`smelt-cli.exe`
改名）、`smeltd.exe`、`smelt-notify.exe`、`smelt-pi.exe`，以及 Pi 要的 wasm。产物名
是 `smelt-<版本>-windows-x86_64-setup.exe`。这一版不签名。

## 目录结构

- `crates/smelt/` —— GUI
- `crates/smeltd/` —— 运行时守护（含 `smelt-notify`）
- `crates/smelt-model/` —— 桌面、守护、手机共用的类型（Control API、会话状态）
- `crates/smelt-core/` —— GUI 与守护共用的无 UI 逻辑；`daemon_state` / `agent_status` / `control_api` 从 `smelt-model` 再导出
- `crates/smelt-client/`、`crates/smelt-client-store/` —— 这台桌面自己的状态（`client.sqlite3`）
- `crates/smelt-runtime-client/` —— 桌面和手机共用的运行时协议客户端
- `crates/smelt-store/` —— 运行时库 `smelt.sqlite3`
- `crates/smelt-sys/` —— 进程等待、日志、守护映像、主机名
- `crates/smelt-pairing/` —— 一次性配对串
- `crates/smelt-git/` —— Git 领域层（不管 worktree）
- `crates/smelt-event-bus/`、`crates/smelt-event-protocol/` —— 进程间事件
- `crates/smelt-acp-view/` —— ACP 结构化对话界面
- `crates/smelt-ui/` —— 共享 UI 组件
- `crates/smelt-iroh/` —— iroh 传输，另有调试二进制 `smelt-iroh-connect`
- `crates/smelt-mobile/`、`mobile/` —— 手机 Rust 内核与 Flutter App
- `scripts/package-mac.sh` —— macOS 打包；`make dist` / `make dist-build` 是入口
- `scripts/package-linux.sh` —— Linux 运行时 tar.gz
- `.github/workflows/release.yml` 的 `windows` job —— Windows x86_64 zip，没有单独的打包脚本
- `scripts/build-smelt-pi.sh` —— 编译 `smelt-pi`
- `scripts/build-ios.sh` —— `make ios`
- `scripts/release.sh` —— 日期版本号 `YYYY.MDD.N`
- `docs/workspace.md` —— 现行架构
- `docs/local-storage.md` —— 两份 SQLite 各存什么

## 原则

- 每步 `cargo check` 通过再继续
- 业务状态进 SQLite：运行时 `~/.smelt/smelt.sqlite3`，这台桌面 `~/.smelt/client.sqlite3`。
  JSON 只用于配置传输或临时交换，不当数据库

## 反馈与贡献

Bug / 功能建议去 [GitHub Issues](https://github.com/smelt-ai/smelt/issues/new/choose) 提，
选对应模板即可；其他情况先看 [README](https://github.com/smelt-ai/smelt) 或直接去看源码。
