# Windows 支持

对应 issue [#74](https://github.com/smelt-ai/smelt/issues/74)。两步都已落地：

1. Windows 桌面端连远程运行时（Linux / macOS 上的 smeltd）。
2. Windows 本机跑 smeltd：冷启动、Rust 客户端走 AF_UNIX、Bun 走命名管道、升级即重启。

数据目录是 `%USERPROFILE%\.smelt`，和其它平台同一套 `smelt_paths`。第一版只出 x86_64，不签名。发版起初是 zip 便携包，后来换成按用户安装的 Inno Setup 安装程序（`scripts/windows-installer.iss`）。

## 第 1 步的范围

做：

- `smelt` 桌面端在 Windows 10 1803 及以上（x86_64）编过、跑起来，通过 iroh 连已配对的 Linux / macOS 运行时，对话、终端、文件树、Git 面板、远程桌面照常用。
- Windows 原生 CI 检查，发版产出 zip 便携包。

不做：

- 自动更新、代码签名、安装包。
- 系统通知、状态栏图标、Dock 角标、液态玻璃。这些模块的非 macOS 实现保持为空（`status_item.rs`、`dock.rs`、`liquid_glass.rs`）。

## 前提

- 桌面端（`crates/smelt`）不依赖 smeltd 和 smelt-core。第 1 步只动它的依赖链：smelt-client、smelt-sys、smelt-paths、smelt-runtime-client、smelt-iroh、smelt-client-store、smelt-ui、smelt-acp-view。
- 依赖库没有阻塞：GPUI 有 `gpui-pre-windows` 后端，quickjs-jit 的 `sys/build.rs` 有 MSVC 分支，gpui-kit 的 `shell`、`webview` 有 Windows 代码，iroh、rusqlite bundled、alacritty_terminal 都支持 Windows。以上只读了源码，没实际编过。
- 桌面端至今只在 macOS 上编过，`not(target_os = "macos")` 分支没被编译器检查过。本文对改动面的估计要用 Windows 上的真实报错校准（施工顺序第 1 项）。

阻塞点是三处结构问题（D1–D3）和一批零散的平台调用（D4），另加打包和 CI（D5）。编号即施工顺序。

## D1 连接流类型

### 现状

`smelt-runtime-client/src/connector.rs` 的 `Connector::connect` 返回 `std::os::unix::net::UnixStream`。文件头写明了原意：本机直接连 socket，远程 iroh 连接器把 QUIC 流接到 `UnixStream::pair()` 的一端，这样上层的阻塞读写、`try_clone`、读写超时对两种传输完全一样，协议代码不用分叉。

这个类型因此穿透了所有调用方：smelt-runtime-client 的 `client.rs`、`desktop.rs`，`smelt-client/src/acp_client.rs`，`smelt/src/terminal.rs`、`smelt/src/runtimes.rs`，以及 smelt-mobile 的 catalog、conversation、terminal。iroh 侧 `smelt-iroh/src/runtime.rs` 的 `bridge_tracked` 用 `UnixStream::pair()` 加 `tokio::net::UnixStream::from_std` 搬运。Windows 上 std 没有 `UnixStream`，tokio 也没有 AF_UNIX，所以连远程也编不过。

### 方案

保留「一种流类型、协议不分叉」的原意，只把类型按平台换掉。

1. smelt-runtime-client 新增 `stream` 模块，定义 `RuntimeStream` 和 `stream_pair()`：
   - Unix：`std::os::unix::net::UnixStream` 的类型别名，行为不变。
   - Windows：`stream/windows.rs` 自己的 `UnixStream` / `UnixListener`（系统 AF_UNIX，Windows 10 1803 起支持），提供 `pair`、`connect`、`try_clone`、`set_read_timeout`、`set_write_timeout`、`set_nonblocking`、`shutdown`、`incoming`，和 std 的语义一致。
   - 不在驱动上挂任何阻塞调用：afunix.sys 在阻塞 `recv` 刚挂上、对端恰好写入或关闭时偶尔丢掉唤醒，读一直挂到超时（10060）；本地 `shutdown` 也叫不醒另一条线程里阻塞的 `recv`。所以套接字建出来就是非阻塞的，读、写、accept、connect 遇到 WouldBlock 都用 AFD poll（WSAPoll 底下那层，可取消）等就绪，和本地 `shutdown` 的事件、读写超时一起等。最早用的 `uds_windows` 是阻塞实现，已去掉。
2. 所有签名里的 `UnixStream` 换成 `RuntimeStream`，是机械替换。
3. iroh 搬运：Unix 保持现有 tokio 实现。Windows 上 AF_UNIX 套接字进不了 tokio，`bridge_tracked` 改为每个方向一条线程：一条阻塞读本地端、经 `Handle::block_on` 写 QUIC `SendStream`，另一条反过来。语义和现有 `pump` 一致：一方读到 EOF 只半关闭对应方向，另一方向继续；任一方向出错，两端一起关。
   - 每条远程流两条线程。桌面同时开着的流是事件订阅加每个打开的终端、对话各一条，几十条量级，可以接受。
   - `runtime.rs` 里现有的桥接测试在 Windows CI 上跑同一套。

没选的做法：

- **回环 TCP 对**：拿不到对端进程，第 2 步本机通信不能用它（见第 2 步）。第 1 步用 AF_UNIX，第 2 步若也选 AF_UNIX，就不必再加一层桥。
- **trait object**：调用方要 `try_clone` 出读写两端，trait object 得自己补这套能力，只多一层间接。

影响面：smelt-mobile 走 Unix 分支，行为不变，但它依赖的签名改了，要跑移动端测试。

## D2 运行时路径

### 现状

协议里的路径（`cwd`、项目根等）已经是 `String`，是运行时那台机器的路径。但桌面端用 `std::path` 拼接，按桌面所在系统的规则处理：

- `file_tree/view.rs`：`Path::new(dir).join(name)`
- `git_panel/mod.rs`：`Path::new(root).join(&row.path)`

在 Windows 上会拼出 `/home/u/proj\src` 发给 Linux 运行时。macOS 连 Linux 时这些地方是碰巧对的。`crates/smelt` 里非测试的 `Path::new(` 约 40 处，集中在 `file_tree/workspace.rs`、`terminal.rs`、`ide.rs`、`file_tree/view.rs`，其中一部分处理的是本机路径（smeltd、IDE），不受影响。

### 方案

- 规则：**运行时给的路径只按运行时那台机器的规则处理**，桌面端不拿 `std::path` 碰它。
- smelt-client 加 `runtime_path` 模块，提供 `join`、`parent`、`file_name`。第 1 步运行时只有 macOS、Linux，统一按 `/` 处理，不带参数。
- 逐个筛那约 40 处 `Path::new(`，以及 `PathBuf::from(` 接运行时字符串的地方（如 `git_panel/view.rs`），属于运行时路径的换成 `runtime_path`。已经按 `/` 手写切分的约 13 处对 POSIX 运行时本来就对，不动。
- 第 2 步有了 Windows 运行时，按运行时 `system.describe` 已有的 `os` 字段选分隔符，`runtime_path` 的函数届时多一个运行时参数。

## D3 本机运行时可选

### 现状

「本机运行时总在」写死在结构里，不只是约定：

- `smelt-client/src/runtime.rs`：`Runtime::local()` 无条件返回本机运行时。
- `Workspace` 有固定的 `local: RuntimeSpace` 字段，远程的在 `remote_spaces`（`main.rs`）。`runtime_space.rs` 的 `space()`、`space_mut()`、`space_keys()` 把本机特判在前。
- 14 处直接读 `self.local` 的字段，多数是冷启动闸门 `sessions_restored` / `restore_pending`，另有 `catalog_loaded`、`write_queue`（`workspace_sessions.rs`、`workspace_prepare.rs`、`workspace_attention.rs`、`workspace_persist.rs`、`main.rs`）。
- `runtime_events.rs` 启动即订阅本机；`RuntimeHub.local_status` 记本机状态。
- smeltd 生命周期：`terminal.rs` 的 `connect_daemon`、`connect_daemon_control`、`daemon_outdated`、`restart_daemon`、`ensure_daemon_running`（用 `flock`、`kill`），被 `runtimes.rs`、`workspace_update.rs`、`workspace_persist.rs`、`workspace_sessions.rs` 调用。
- 另有一批只认本机的功能，见下面的表。

### 方案

1. **本机运行时在所有桌面平台上都有**：Windows 也跑 smeltd 之后，`local_runtime_supported()` 恒为真，已删除。
2. **`Runtime::local()` 直接返回 `Runtime`**。过渡期它返回过 `Option<Runtime>`，用来逼出每个调用点的处理；三个桌面平台都有本机之后，`None` 分支全部是死代码，一并删掉。
3. **空间合一**：`local` 和 `remote_spaces` 合成 `spaces: Vec<RuntimeSpace>`，有本机时本机在前，`space()` 等去掉本机特判。那 14 处 `self.local` 原本为本机冷启动恢复而设，逐个确认原意后改成读会话所属运行时的空间，改之前要确认远程空间的恢复语义一致。
4. **smeltd 生命周期独立成模块**：`terminal.rs` 里上面那段挪到 `local_daemon.rs`。现在是 `cfg(any(unix, windows))`，`runtimes::connect_terminal` 在本机分支调它。Windows 用 `spawn_detached`、`ExclusiveFile`、`pid_is_alive`，升级不交接。
5. **只认本机的功能**按下表处理：

| 功能 | 位置 | 没有本机运行时时 |
|---|---|---|
| 机器面板 | `session_list/machines.rs`（以 `vec![Runtime::local()]` 起头） | 只列远程 |
| 侧栏分段默认折叠 | `runtime_space.rs` 的 `runtime_folded` | 第一个运行时展开 |
| 打开项目 | `workspace_sessions.rs` 的 `open_project` | 先选机器，只有一台就直接用 |
| 历史页 | `session_history.rs`、`workspace_sessions.rs` | 待定，见末尾 |
| 能力库、自动化 | `agents/workspace.rs` | 隐藏 |
| 「Agent」页（有远程运行时时，标题旁挂「只作用于本机运行时」） | `settings/workspace_view/render/mod.rs` | 隐藏 |
| Pi 配置 | `settings/pi_config.rs` | 已按 `RuntimeKey` 选客户端，默认选第一个可用运行时 |
| smeltd 升级、重启、过期探测 | `workspace_update.rs`，`main.rs` 的 `start_daemon_outdated_watch` | 不启动 |
| smelt-notify 同步 | `settings/bundled_helpers.rs` | 不调用 |
| 命令行子命令 | `cli.rs`（经 `daemon_request` 直连本机） | Windows 上不提供 |

6. **首次启动的空状态**：没配对任何运行时时侧栏是空的，放一个引导去设置页配对的空状态。配对输入已有（`RuntimeHub.pairing_input`）。

## D4 零散的平台调用

| 位置 | 现状 | 处理 |
|---|---|---|
| `smelt-sys/src/lib.rs` `hostname()` | `libc::gethostname` | Windows 用 `GetComputerNameExW` |
| `smelt-sys` `process_wait`、`daemon_image`、`fd_limit` | Unix 进程和文件描述符 | 只被 smeltd 生命周期用，`cfg(unix)` |
| `smelt-sys/src/app_log.rs` | 已有 `cfg(not(unix))` 分支 | 确认能编过 |
| `smelt-paths` `pid_is_alive` | `libc::kill(pid, 0)` | 只服务测试沙箱清理；Windows 用 `OpenProcess` 判断，或该清理整体 `cfg(unix)` |
| `smelt-paths` `smelt_home()` | `dirs::home_dir().join(".smelt")` | 待定，见末尾 |
| `smelt-iroh/src/lib.rs` 密钥文件权限 | `cfg(unix)` 设 0600，已有非 Unix 分支 | 不改，用户目录默认 ACL 只给本人 |
| `smelt-client-store` `path_bytes` | Unix `OsStrExt` | 只服务更新器状态，随更新器 `cfg` |
| `smelt-client/src/updater/macos.rs`、`app_location.rs` | `.app` 替换，`flock`、`renamex_np`、`dlopen` | `cfg(target_os = "macos")`；Windows 的更新另走 `updater/windows.rs`，见「待办」上面的「应用内更新」 |
| `smelt/src/plugins/host_fs.rs` | 软链接、原子交换 | 软链接用 `std::os::windows::fs::symlink_file` / `symlink_dir`，失败按不支持报错；原子交换走现有的「不支持」分支 |
| `smelt/src/terminal.rs` 其余部分 | 前端渲染与连接 | D1 换类型，smeltd 部分已在 D3 挪走 |

## D5 CI、发版与打包

- **CI**：`ci.yml` 加 `windows-latest` 任务，跑 `cargo check -p smelt` 和受影响 crate 的测试（smelt-runtime-client、smelt-iroh、smelt-client）。在 Windows runner 上原生编译，不交叉编译。桌面端在 Windows 上编过之前不设为必需检查，编过后加进 `ci-ok`。
- **发版**：`release.yml` 加 Windows 任务，只编 `smelt`，产出 `smelt-<版本>-windows-x86_64.zip`，挂到同一个 Release，写进校验和清单。
- **图标与清单**：Windows 清单由 `gpui-pre-platform` 的 `windows-manifest` 特性带上；exe 图标要新加 `crates/smelt/build.rs`，只在 Windows 目标上嵌资源。
- **签名与更新**：第一版不签名，下载后有 SmartScreen 提示，在 Release 说明里写明。安装包已定为 Inno Setup 按用户安装；应用内更新见下面「应用内更新」。

## 施工顺序

每一块单独可合入，macOS 行为不变。

1. 在 Windows 机器上跑 `cargo check -p smelt --target x86_64-pc-windows-msvc`，拿报错清单校准本文。
2. D1 连接流类型。
3. D2 运行时路径。
4. D3 本机运行时可选。
5. D4 零散平台调用，直到 Windows 上编过、跑通；CI 的 Windows 任务转为必需。
6. D5 发版与打包。

## 第 2 步：本机 smeltd

已实现。Unix 行为不变。Windows 是一套真的本机运行时，不是远程客户端的空壳。

- **IPC**。Rust 客户端（桌面、`smelt-notify`、runtime client）继续连 AF_UNIX 文件 `~/.smelt/smeltd.sock`。Windows 上这个类型是 `smelt_runtime_client::stream` 自己包的 AF_UNIX。守护的 accept 循环是同步线程（`incoming` / `accept`），不把监听塞进 tokio，所以不要求 tokio 的 Windows AF_UNIX。Bun / Node 的 `net.connect(path)` 在 Windows 上连的是命名管道，不是这个文件。第二条监听只在 Windows 上建：`PipeListener::bind(user_pipe_path("daemon"))`，accept 之后进同一个连接处理函数。不把管道桥进 AF_UNIX，否则对端 pid 会变成 smeltd 自己，鉴权直接拒绝。交给 Bun / Pi / shell 的 `SMELT_SOCK` 在 Windows 上是 `\\.\pipe\smelt-<sid>-daemon` 这种管道名；Rust 代码连守护仍然用 sock 文件。
- **鉴权**。macOS 的 codesign 路径不动。Windows 走同一用户 SID：pid 来自 AF_UNIX 的 `SIO_AF_UNIX_GETPEERPID` 或管道的 `GetNamedPipeClientProcessId`，exe 来自 `process_executable`，必须 `same_user_as_current`，并且不能是守护自己的 pid。basename 和 `"smelt"` 比的时候去掉一个结尾的 `.exe`、大小写不敏感，不另加一行表，免得 macOS 的 signing-id 列分叉。路径要和守护目录旁边的 `smelt.exe` 相同，或者和 `SMELT_BUNDLE_DIR`（桌面拉起守护时写成 GUI exe 所在目录）旁边的 `smelt.exe` 相同。托管映像是 `~/.smelt/bin/smeltd-<指纹>.exe`，GUI 在安装目录，只认守护目录的兄弟会把桌面拒掉。不做 Authenticode。
- **升级**。不交接 fd（`handoff_v2` 已删）。升级先拨启动名，再优雅停机：收割子进程、回写未完成回合、WAL checkpoint，最后回执。Unix 回执后在本进程里 `exec` 新映像；Windows 不能 `exec`，拉起目标映像当接班进程（`SMELTD_PREDECESSOR_PID` 告诉它前任是谁），接班进程等前任退出、放掉监听再绑定。没有桌面的运行时（推送升级、空闲自升级）也能换上新版。终端会话和进行中的回合会中断，历史还在。
- **守护映像**。不建符号链接（那要额外权限）。发布是复制或硬链接到 `smeltd-<指纹>.exe`，启动名是旁边的 `smelt-image-v1` 指针文件。`launch_executable` 读出真正要执行的那份。
- **Bun**。`scripts/fetch-bun.sh` 在 Windows 上取 `bun-windows-x64.zip`（Bun 1.4.3），装成 `bin/bun.exe`（硬链接或复制）。`scripts/build-smelt-pi.sh` 编出 `smelt-pi.exe`。
- **Shell 和进程**。交互终端是 `COMSPEC` 或 `cmd.exe`，参数 `/d /k`。没有参数的自动化 Shell 脚本同样走 `cmd.exe /d /c`，不调用 bash。子进程放进 Job Object，`KILL_ON_JOB_CLOSE`。ConPTY 不能 `CREATE_SUSPENDED`，shell 起来之后立刻 `attach_running`。PTY 尺寸走 master 的 `resize`，不用 `as_raw_fd`。
- **发版**。安装程序 `smelt-<版本>-windows-x86_64-setup.exe` 装进安装目录的是 `smelt.exe`、`smelt.com`、`smeltd.exe`、`smelt-notify.exe`、`smelt-pi.exe`，外加 Pi 要的 wasm。桌面在自己旁边找这几个 exe。`smelt.com` 是控制台子系统的命令行入口（`smelt-cli.exe` 改名），cmd / PowerShell 敲 `smelt` 先找到它。

## 应用内更新

已实现。通道、查新版、设置页和 macOS 共用（`smelt-client/src/updater.rs`），Windows 只换「下什么、怎么装」（`updater/windows.rs`）：

- **谁能更新**。只有安装程序装的发版构建：有发版号 `smelt_version::RELEASE`，exe 旁边有 Inno 的卸载程序 `unins000.exe`。开发构建、拷出来的 exe 停在「这个 Smelt 不能自动更新」，不检查、不下载。
- **查**。认 Release 里的 `smelt-<版本>-windows-x86_64-setup.exe`，同一个 Release 还要有 `SHA256SUMS`，而且清单里已经有它（发版流水线等所有产物到齐才重写清单）；最新的还没齐就退到上一个，最多看三个。是不是新版比的是发版号：只有比装着的新才装，切回生产通道或退到比装着的旧时都不装回去（旧版读不了新版升级过的客户端库）。版本号认不出的候选跳过，所以更新清单要给出能解析的版本号。
- **下**。下到 `%LOCALAPPDATA%\Smelt\update`，边下边算 SHA-256，对上清单才算下好；同一版本下过就不再下。不记持久化事务：下好的安装程序只在这次运行里等着装，重启后重新检查。启动时删掉已经装上的那个版本的安装程序和日志。
- **装**。拉起前再校验一遍，再用 `spawn_detached` 拉起 `setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /WAITPID=<桌面 pid> /LOG=<日志> [/RELAUNCH=<桌面 exe>]`，脱离桌面的 Job，桌面随即退出。安装程序在 `InitializeSetup` 里等桌面进程退出（上限 2 分钟，超时这次不装），之后才查 `AppMutex`、换文件（Inno 源码里这两步就是这个顺序）。给了 `/RELAUNCH` 时，装好由 `[Run]` 打开新版；没装成（超时、文件被占用、又有 Smelt 开着）由 `DeinitializeSetup` 把原来那个打开。下次启动在 app.log 里记一笔，指向留下的安装日志。
- **退出时装**。Quit 动作和关掉主窗口（Windows 上关主窗口就是退出，不走 Quit）都会装待装的更新，不重新打开。
- **守护不停**。smeltd、smelt-pi、smelt-notify 跑的都是 `~/.smelt/bin` 里的副本，不占安装目录，换文件不用停守护，会话不断，也就不用像 macOS 那样先确认。新版桌面起来后照「守护落后就在空闲时重启」换守护。安装目录里还可能被占着的是 `smelt.com`（终端里开着的命令行），交给 Inno 的 Restart Manager 关。

## 已定

原先写成「未定」的，按上面落地：

- 本机通信是 Rust AF_UNIX（自己包的非阻塞套接字，等待走 AFD poll，sock 文件）加 Bun 命名管道。不是回环 TCP，也不要求 tokio AF_UNIX。
- 鉴权是同用户 SID、peer exe 的 basename、守护目录或 `SMELT_BUNDLE_DIR` 的兄弟。没有 Authenticode。
- 交接不做。升级重启，终端会话丢失。
- 数据目录沿用 `%USERPROFILE%\.smelt`。
- 第一版只出 x86_64。

## 待办

- **远程升级**（10-27 记）。现在 Windows 上的运行时回不了 `runtime_upgrade`：它跟桌面装在同一个安装程序里，只能到那台机器上手动跑新版的 `setup.exe` 覆盖安装。打算的做法：smeltd 收到升级请求后从 Release 下载 `smelt-<版本>-windows-x86_64-setup.exe`，按校验和清单核对哈希，拉起一个脱离自己的安装进程（`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`）后退出，好让 Inno Setup 替换正在用的 exe；装完重新拉起 smeltd，桌面那边照现有流程等它重启、重连。代价：终端会话不保留；那台机器上开着的 Smelt 桌面会被关掉。下载、校验、静默安装可以复用桌面应用内更新那套（`updater/windows.rs`、`.iss` 的 `/WAITPID`）。做的时候要在 Windows 机器上实测。
