# Windows 支持

对应 issue [#74](https://github.com/smelt-ai/smelt/issues/74)。Windows 桌面端与本机 `smeltd` 运行时均已落地：

1. Windows 桌面端（Windows 10 1803+ x86_64）：连本机或远程运行时，对话、终端、文件树、Git 面板、远程桌面照常用。
2. Windows 本机 `smeltd`：冷启动、本机连接一律走命名管道、升级即重启。
3. 打包与更新：按用户安装的 Inno Setup 安装程序（`scripts/windows-installer.iss`）与应用内自动更新（`crates/smelt-client/src/updater/windows.rs`）；exe 与安装程序当前不签名。

数据目录是 `%USERPROFILE%\.smelt`，与其它平台共用 `smelt_paths`。系统通知、状态栏图标、Dock 角标在 Windows 上未启用（对应模块的非 macOS 实现保持为空）。

## 窗口材质

`window_backdrop.rs`：Windows 11 22H2（build 22621）起用 GPUI 的 `MicaBackdrop`，整窗拿 Mica 做底，布局声明了材质的地方透出它、别处铺实色（系统背景材质只能整窗一种，侧栏与窗口底在 Windows 上不区分），窗口深色标记跟随 smelt 主题；更早的 Windows 版本主窗口不透明、铺主题底色，不退到 Acrylic（`Blurred`，拖窗口会卡）。

## 连接流类型（`RuntimeStream`）

`smelt-runtime-client` 通过平台统一的 `RuntimeStream` / `stream_pair()` 保持「一种流类型、协议不分叉」：

- **Unix**：`std::os::unix::net::UnixStream` 的类型别名。
- **Windows**：`stream/windows.rs` 的 `RuntimeStream` / `RuntimeListener` 走命名管道（`smelt_sys::named_pipe`），`pair()` 走进程内内存流（`stream/memory.rs`）。
  - **为什么不用 AF_UNIX**：曾用过系统 AF_UNIX，后撤掉——`afunix.sys` 在 Windows 10 22H2 上存在内核释放后再用，桌面反复建连断开会导致整机蓝屏（0x3B，[#251](https://github.com/smelt-ai/smelt/issues/251)）。
  - 命名管道没有半关闭，`shutdown(Write)` 报 `Unsupported`；`shutdown(Both)` 等读写退出后关句柄，对端读完已写数据再读到 EOF（不用 `DisconnectNamedPipe`，会丢未读数据）。需要半关闭的 iroh 桥接用 `pair()`。
- **iroh 搬运**：Windows 上 `pair()` 内存流进不了 tokio，`crates/smelt-iroh/src/runtime.rs` 的 `bridge_tracked` 按方向各起一条阻塞线程桥接 QUIC 流，半关闭与出错关闭语义和 Unix 一致。
- **运行时路径规则**：运行时协议里的路径只按运行时所在系统规则处理（`smelt_model::runtime_path`，按 `system.describe` 的 `os` 字段选分隔符），桌面端不拿 `std::path` 拼远程路径。

## 本机 `smeltd`

Windows 是一套完整的本机运行时，不是远程客户端的空壳：

- **IPC**：守护只有一条监听：`smeltd.sock` 路径对应的命名管道 `\\.\pipe\smelt-<sid>-daemon-<路径哈希>`（`daemon_pipe_path`），DACL 只给当前用户、拒绝远程客户端。Rust 客户端照旧传 sock 路径调 `RuntimeStream::connect`，Windows 上自动换成管道名，不建 sock 文件；交给 Bun / Pi / shell 的 `SMELT_SOCK` 直接是管道名（Node 的 `net.connect(path)` 只认它）。守护的 accept 循环是同步线程，不塞进 tokio。
- **单实例**：守护活着一直握着 `smeltd.instance.lock`（进程退出由系统释放），拿到锁才接手管道名（`bind_taking_over`）。停机在回执前叫停监听、放掉锁，接班守护才能接手。
- **排队**：管道没有套接字那样的 backlog，只有正在等客户端的实例能连。`PipeListener` 在 `bind` 时起接受线程，持续建实例并把连上的放进 64 槽有界队列供 `accept` 取走；叫停时队列中未取走的连接一并关闭，由客户端重连接班守护。
- **鉴权**：校验同一用户 SID（`GetNamedPipeClientProcessId` + `process_executable` + `same_user_as_current`），排除守护自身 pid；basename 与 `"smelt"` 大小写不敏感比对（去 `.exe`），路径必须位于守护目录或 `SMELT_BUNDLE_DIR` 旁。不做 Authenticode。
- **升级**：不交接 fd（`handoff_v2` 已删）。升级先优雅停机：收割子进程、回写未完成回合、WAL checkpoint，最后回执。Windows 不能 `exec`，拉起新版本作接班进程（经 `SMELTD_PREDECESSOR_PID` 传入前任 pid），接班进程等前任退出放掉监听再绑定；30 秒未退则按创建时间确认身份后终止。终端会话和进行中的回合会中断，历史保留。
- **守护与 `smelt-agent` 位置**：不建符号链接（需要额外权限）。守护复制成 `~/.smelt/bin/smeltd.exe`，`smelt-agent` 一整套复制进 `~/.smelt/bin/agent\`；桌面先停守护再替换。
- **Bun**：`scripts/fetch-bun.sh` 在 Windows 上从 npm 取 `@oven/bun-windows-x64`，`agent/scripts/build-smelt-agent.sh` 编出 `smelt-agent.exe`。
- **Shell 与进程**：交互终端用 `COMSPEC` 或 `cmd.exe /d /k`；无参数自动化 Shell 脚本走 `cmd.exe /d /c`。子进程放进 Job Object（`KILL_ON_JOB_CLOSE`）；ConPTY 起来后立刻 `attach_running`，尺寸走 master 的 `resize`。
- **发版产物**：`smelt-<版本>-windows-x86_64-setup.exe` 安装 `smelt.exe`、`smelt.com`（`smelt-cli.exe` 改名，提供控制台输出）、`smeltd.exe`、`smelt-notify.exe`、`smelt-agent.exe`、Pi 所需 wasm 与内置 `plugins/`。

## 应用内更新

已实现。通道、查新版、设置页与 macOS 共用（`crates/smelt-client/src/updater.rs`），Windows 只换下载和安装实现（`crates/smelt-client/src/updater/windows.rs`）：

- **谁能更新**：仅由安装程序安装的发版构建（有 `smelt_version::RELEASE` 且旁有 `unins000.exe`）。开发构建和散拷 exe 不检查、不下载。
- **查**：认 Release 里的 `smelt-<版本>-windows-x86_64-setup.exe` 且同版本 `SHA256SUMS` 已含其校验和；最新未齐则退看前序，最多看 3 个；只升不降。
- **下**：下到 `%LOCALAPPDATA%\Smelt\update` 并校验 SHA-256，不记持久化事务；启动时清理已装版本的安装程序和日志。
- **装**：再次校验后以 `spawn_detached` 拉起 `setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /WAITPID=<桌面 pid> /LOG=<日志> [/RELAUNCH=<桌面 exe>]`，桌面退出后由 Inno Setup 等待退出（上限 2 分钟）再覆盖安装，成功按 `/RELAUNCH` 打开新版，失败则由 `DeinitializeSetup` 重新拉起旧版。
- **停守护与退出安装**：更新时停本机守护（有活跃会话先确认）；Quit 或关主窗口时会安装待装更新（关主窗口无处确认时若有活跃会话则跳过）；被终端占用的 `smelt.com` 交 Inno Restart Manager 处理。

## 待办

- **远程升级**：当前 Windows 上的运行时未实现 `runtime_upgrade`（`crates/smelt/src/runtimes.rs`），只能到目标机手动运行新版 `setup.exe` 覆盖安装。计划方案：smeltd 收到升级请求后下载校验 `setup.exe`，以静默参数脱离自身拉起安装后退出，装完重新拉起 smeltd（复用 `updater/windows.rs` 与 `.iss` 的 `/WAITPID`），需在 Windows 真机实测。
