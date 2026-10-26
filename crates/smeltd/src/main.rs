//! smeltd —— 终端持久化守护进程（tmux 的最小替身）。
//!
//! 所有 shell / PTY 活在这里而非 GUI 进程里：GUI 退出、崩溃，会话照常运行；
//! 重开 GUI 按会话 id 重连（attach）。
//!
//! ## 画面恢复（类 tmux，不是「字节磁带重放」）
//!
//! 每个会话在守护内常驻一份 `alacritty_terminal::Term`：PTY 输出一边转发给 client，
//! 一边 `parser.advance` 进这份网格。attach 时**不**依赖可能被环形缓冲腰斩的原始
//! 字节重放，而是把当前网格序列化成一段自洽的 ANSI「整屏快照」发给客户端——空 Term
//! 解析后即当前画面，避免长 detach 后 Ctrl+C 大重绘错位（见 docs/roadmap.md）。
//! 仍保留一小段原始字节环形缓冲，**只**给尚未 attach 的瞬间攒实时输出；
//! **绝不**用它在 upgrade 后重建 Term（环形缓冲会在 CSI 中间腰斩，feed 必花屏）。
//!
//! 协议（Unix socket ~/.smelt/smeltd.sock）——连接后客户端先发一行 JSON：
//!   {"op":"open","id":"..","cwd":"..","cols":120,"rows":30}  → 进入流模式（唯一 client，
//!                                                              同 id 第二次 open 顶掉前一个）
//!   {"op":"watch","id":".."}                                 → 进入**只读**流模式（旁观，见下）
//!   {"op":"acp_action","id":"..","action":{...}}            → 对现有 ACP 会话执行一次动作，
//!                                                              不占用或替换 control client
//!   {"op":"control","control_version":1,"request_id":"..",  → 版本化短 RPC；method/params 与
//!    "method":"system.describe","params":{}}                    统一响应见 docs/control-api.md
//!   {"op":"list"}                                            → 回 {"sessions":[..]} 后关闭
//!   {"op":"kill","id":".."}                                  → 回 {"ok":true} 后关闭
//!   {"op":"version"}                                         → 回 {"version":"..","exe_mtime":123} 后关闭
//!   {"op":"shutdown"}                                        → 优雅停机（收割子进程、checkpoint 状态库）后
//!                                                              回 {"ok":true} 再退出，所有会话结束
//!   {"op":"upgrade"}                                         → 先把启动名拨到本进程钉住的映像，同样优雅停机、
//!                                                              回执；Unix 随后在本进程 exec，Windows 拉起接班
//!                                                              进程后退出。不交接 fd，进行中的回合中断
//!   {"op":"upgrade","exe":"/path/to/smeltd"}                 → 同上，换到指定映像
//!   {"op":"runtime_upgrade","version":"2026.1023.0"}         → 回 {"state":"accepted"}，从 GitHub Release
//!                                                              下载该版本校验通过后重启守护；见 runtime_upgrade.rs
//!   {"op":"agent_event","id":"..","event":{...}}             → 回 {"ok":true} 后关闭，归一化 hook 事件
//!   {"op":"action","id":"..","kind":"approve|deny|reply","text":".."}
//!                                                            → 回 {"ok":true}/{"ok":false,"err":".."} 后关闭，
//!                                                              见下「远程操控」（text 仅 reply 需要）
//!   {"op":"input","id":"..","data":".."}                     → 回 {"ok":true}/{"ok":false,"err":".."} 后关闭，
//!                                                              `data` 是 UTF-8 字符串（控制字符用 JSON
//!                                                              `\u00xx`），原样写入 PTY，**无 phase 门闩**
//!   {"op":"resize","id":"..","cols":N,"rows":M}              → 回 {"ok":true} 后关闭，改 PTY 窗口尺寸
//!                                                              （SIGWINCH，供手机端按视口重排 TUI）
//!
//! 流模式：
//!   守护 → 客户端：先发 JSON 尺寸行（含 replay_len=快照字节数）→ Codux 风格 keyframe
//!                   ANSI（模式前缀 + 按行 CUP + 绝对 SGR，见 snapshot_ansi）
//!                   → 再实时转发 PTY 输出
//!   客户端 → 守护：帧 `[type:u8][len:u32 BE][payload]`
//!     type 0 = 键盘输入字节；type 1 = resize
//!       payload 8 字节：cols u32 BE + rows u32 BE（兼容旧客户端，像素 = 0）
//!       payload 16 字节：cols + rows + cell_w + cell_h（各 u32 BE）→
//!         ws_xpixel = cols*cell_w，ws_ypixel = rows*cell_h
//! shell 退出 → 守护关闭该连接（客户端读到 EOF）。
//!
//! ## `watch`：只读旁观，不参与「同 id 唯一 client」的顶替
//!
//! 远程操作/观战席这类场景需要「GUI 开着的同时，另一路也能看画面」——但 `open` 的语义
//! 是「同 id 只允许一个 GUI」（第二次 open 会 shutdown 前一个连接），不能照搬。`watch`
//! 是独立的第二条路径：会话必须已存在（不会像 `open` 那样兜底新建）；进来后收一份和
//! `open` 一样的尺寸行 + ANSI 快照，但**不进入帧循环**——不认输入/resize，收到任何客户端
//! 发来的字节都当异常直接断开。多个 `watch` 连接可以并存，也不影响 `open` 的那个唯一
//! client；某个 watcher 断线只清自己，不影响其他 watcher 或 client。
//!
//! 例外是移动端：它声明 `controls_geometry`，在连接存活期间持有 PTY 尺寸租约（桌面跟随
//! 这个 canonical grid，不能抢回去），并可发 type 1 / type 2 帧。租约在连接**意外**断开
//! 后还会保留一段宽限期（[`REMOTE_VIEWPORT_GRACE`]）：手机切后台、退出会话页都是连接
//! 没了，立刻归还会让桌面把尺寸抢回去，手机下次进来再抢一次——不切备用屏的 CLI（整段
//! 对话都躺在 scrollback 里的那类）每收一次 SIGWINCH 就把对话重排重印一遍（实测一次
//! 560KB / 8 秒），手机上就是「一进来又滚很久」。真正的归还信号是桌面侧有人敲键盘。
//!
//! ## 远程访问（`remote.*` / `device.*` 控制方法，见 `remote.rs`、`devices.rs`）
//!
//! 远程只走 iroh：endpoint 直接接收运行时协议的流，每条 QUIC 双向流桥接成一条
//! socketpair，交给和 unix socket 同一个 `handle_conn`。`endpoint_id` 由
//! `~/.smelt/iroh-secret` 的私钥决定，重启不变。鉴权按设备：已配对设备的 endpoint id
//! 记在 `paired_device` 表里，没配对的只能提交一次性配对码。
//!
//! 开关归运行时：`remote.config.set` 存配置并照着开关；守护每次启动按落盘配置自愈，
//! 不等客户端。macOS 上远程开着、插着电时持有 `PreventUserIdleSystemSleep`，电池上
//! 不握。升级前先禁止再拉起并释放断言，新进程启动时清掉本 PID
//! 上同名残留。
//!
//! ## 远程操控（`action` + `input` op）
//!
//! Phase 6：远程端是 PC 工作的**延续**——能力上要能往 PTY 写任意字节，交互上再
//! 用操作台按钮减负。两条 op 分工：
//!
//! **`input`**：原始字节写入 PTY，和本机键盘同权。**没有 phase 门闩**——用户可能
//! 随时要 Ctrl+C、在 agent 思考时补一句、或在 TUI 里方向键导航。`data` 是 UTF-8
//! 字符串（控制字符走 JSON `\u00xx`，xterm onData 出来的串 `JSON.stringify` 即可）；
//! 空串拒绝。
//!
//! **`action`**：approve/deny/reply 映射成固定按键序列，是高频快捷方式，**不是**
//! 能力上限。门闩（`phase` 必须是 `AwaitingApproval`/`WaitingForUser`）是**正确性**
//! 保护，防止误点「批准」时 agent 其实在跑别的——不排队，直接拒绝：
//! - `approve` → `\r`（回车，接受当前高亮的默认项）
//! - `deny` → `\x1b`（Esc，不管菜单形状直接取消/拒绝）
//! - `reply` → 文本 + `\r`（便捷回复；自由输入更推荐走 `input`）
//!
//! 授权模型：按设备。已配对的设备和本机同权，没配对的连接到不了这两个 op（见上
//! 「远程访问」）。smeltd 的 action 门闩只管时机、不管权限。

mod acp_host;
mod acp_idle;
mod acp_registry;
mod acp_runtime_host;
mod agent_memory_control;
mod automation_runtime;
mod control;
mod conversation_image;
mod desktop;
mod devices;
mod event_hub;
mod files_control;
mod client_conn;
mod git_control;
mod history_control;
#[cfg(target_os = "macos")]
mod menubar;
mod peer_messages;
mod pi_control;
mod subagent_transcript;
use devices::Principal;
mod program_status;
mod project_watch;
mod protocol;
mod remote;
mod runtime_identity;
mod runtime_upgrade;
mod service;
mod session_catalog;
mod session_control;
mod session_directory;
mod session_state;
mod terminal_output;
mod terminal_progress;
mod terminal_registry;
mod terminal_snapshot;
mod webhook;

use acp_host::*;
use protocol::*;
use remote::*;

use acp_registry::{AcpRegistry, AcpSlot};
use event_hub::EventHubHandle;
use program_status::ProgramStatusNotices;
use session_directory::SessionDirectory;
#[cfg(test)]
pub(crate) use session_state::AgentBlocker;
pub(crate) use session_state::{
    Phase, PhaseSource, SessionState, apply_agent_event, bump_state_revision, commit_session_phase,
    next_session_instance, now_unix,
};
use smelt_core::agent_event::{AGENT_EVENT_VERSION, AgentEvent};
use smelt_core::automation::{AutomationCommand, AutomationFile, AutomationInboundEvent};
use smelt_core::automation_store::{
    AutomationStore, local_timezone_fingerprint, new_automation_store,
};
use smelt_core::osc::{
    OscEvent, OscScan, TerminalGeometryOsc, program_status_reply, terminal_geometry_osc,
};
use smelt_core::session_control::{
    RemoteAcpSession, RemoteSessionCatalog, RemoteSessionKind, RemoteSessionLifecycle,
    RemoteSessionSnapshot, RemoteTerminalSession, find_agent_option_for,
};
use smelt_core::workspace_menu::{
    WorkspaceMenuSnapshot, load_published_workspace_menu, persist_published_workspace_menu,
};
use terminal_output::{KeyframeNeed, OutputAttachment, enqueue_session_streams};
use terminal_progress::ProgressTracker;
use terminal_registry::{TerminalRegistry, TerminalSlot};
use terminal_snapshot::{
    SNAPSHOT_MAX_LINES, available_snapshot_lines, snapshot_ansi, snapshot_ansi_for_watch,
    snapshot_ansi_screen,
};

use client_conn::{ClientConn, ClientSocket, ConnId};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use smelt_runtime_client::{UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(unix)]
use std::sync::Once;
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{NamedColor, Processor, Rgb};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
#[cfg(test)]
use terminal_snapshot::CellStyle;

/// 常驻 Term 的 scrollback 行数（状态机 history-limit）。
const TERM_HISTORY: usize = 10_000;

/// 状态订阅和 ACP 流的客户端 socket 写超时。终端 PTY 输出不使用这个超时：它走
/// 每 attachment 独立的异步队列，短暂的 GUI 停顿不能被误判为终端连接已失效。
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(3);
/// provider 正常响应 Shutdown 的宽限；超时后连接层会对整个进程组 SIGKILL。
const ACP_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// PTY master 也不能使用无限期阻塞写。agent 被暂停、输入队列填满或 TUI 卡在
/// 内核时，写请求最多等这一段时间，然后把错误返回给调用方；期间不持有 `ctl`。
const PTY_WRITE_TIMEOUT: Duration = Duration::from_secs(3);
/// 终端 kill/异常收尾等待唯一 child reaper 的上限。SIGKILL 后仍无法确认回收时只记错，
/// 不能让一条控制连接永久卡住整个会话生命周期锁。
const TERMINAL_CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(2);
/// 多个新 shell / ACP 子进程可以同时 spawn。锁仍留在各 spawn 点，避免以后
/// 又有一条需要独占 fork 的路径时漏掉调用方。
static SPAWN_GATE: LazyLock<Arc<RwLock<()>>> = LazyLock::new(|| Arc::new(RwLock::new(())));

fn sock_path() -> std::path::PathBuf {
    let dir = smelt_paths::smelt_home().unwrap_or_else(|| std::env::temp_dir().join(".smelt"));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("smeltd.sock")
}

/// 启动时钉住的那份普通文件。会话宿主只 exec 这条路径，不再回头看启动名现在指向谁。
struct PinnedDaemonImage {
    path: std::path::PathBuf,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

static PINNED_DAEMON_IMAGE: OnceLock<PinnedDaemonImage> = OnceLock::new();

fn daemon_executable_path() -> std::io::Result<std::path::PathBuf> {
    PINNED_DAEMON_IMAGE
        .get()
        .map(|pinned| pinned.path.clone())
        .ok_or_else(|| std::io::Error::other("没有钉住守护映像"))
}

/// 钉住本次 exec 真正映射的普通文件。
///
/// `current_exe()` 经常是启动时用的那条路径。若有人从符号链接 `smeltd` 拉起，
/// 这里只顺着链接读一次，打开目标文件（`O_NOFOLLOW`，不再二次解析）。
/// 之后启动名被拨到别的映像，也不改变已经钉住的路径。
fn pin_running_daemon_image() {
    let Ok(path) = std::env::current_exe() else {
        eprintln!("smeltd 无法读取当前可执行路径，会话宿主将拒绝启动");
        return;
    };
    let Some(pinned) = open_mapped_image(&path) else {
        eprintln!(
            "smeltd 没有钉住守护映像（{}），会话宿主将拒绝启动",
            path.display()
        );
        return;
    };
    let _ = PINNED_DAEMON_IMAGE.set(pinned);
}

fn open_mapped_image(current: &std::path::Path) -> Option<PinnedDaemonImage> {
    #[cfg(windows)]
    {
        // `current_exe()` 在 Windows 上就是正在映射的那份 exe（哈希副本），不是启动名。
        let path = std::fs::canonicalize(current).ok()?;
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() {
            return None;
        }
        return Some(PinnedDaemonImage { path });
    }
    #[cfg(unix)]
    {
        if let Some(pinned) = open_regular_nofollow(current) {
            return Some(pinned);
        }
        let target = std::fs::read_link(current).ok()?;
        let resolved = if target.is_absolute() {
            target
        } else {
            current.parent()?.join(target)
        };
        open_regular_nofollow(&resolved)
    }
}

#[cfg(unix)]
fn open_regular_nofollow(path: &std::path::Path) -> Option<PinnedDaemonImage> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    Some(PinnedDaemonImage {
        path: path.to_path_buf(),
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// 不跟随符号链接。钉住的必须是那个普通文件本身；路径变成链接就当成映像已经换了。
fn path_has_inode(path: &std::path::Path, dev: u64, ino: u64) -> bool {
    #[cfg(windows)]
    {
        let _ = (dev, ino);
        // 钉住的是 canonicalize 之后的路径。路径还在、并且规范化结果没换，才是同一份映像。
        match std::fs::canonicalize(path) {
            Ok(canonical) => canonical == path,
            Err(_) => false,
        }
    }
    #[cfg(unix)]
    {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        use std::os::unix::fs::MetadataExt;
        meta.dev() == dev && meta.ino() == ino
    }
}

fn session_host_executable_from(
    pinned_path: &std::path::Path,
    pinned: Option<(u64, u64)>,
) -> std::io::Result<std::path::PathBuf> {
    let Some((dev, ino)) = pinned else {
        return Err(std::io::Error::other("没有钉住守护映像"));
    };
    if path_has_inode(pinned_path, dev, ino) {
        return Ok(pinned_path.to_path_buf());
    }
    Err(std::io::Error::other(format!(
        "smeltd 路径已指向另一份映像，拒绝用它启动会话宿主：{}",
        pinned_path.display()
    )))
}

/// 会话宿主必须和本进程是同一个 inode。钉住的文件被换掉时直接失败。
/// 不能改去 exec 启动名现在指向的文件，也不能另拷一份再跑。
fn session_host_executable() -> std::io::Result<std::path::PathBuf> {
    let pinned = PINNED_DAEMON_IMAGE
        .get()
        .ok_or_else(|| std::io::Error::other("没有钉住守护映像"))?;
    #[cfg(unix)]
    let identity = Some((pinned.dev, pinned.ino));
    #[cfg(windows)]
    let identity = Some((0, 0));
    session_host_executable_from(&pinned.path, identity)
}

/// 已经写好、还没切到运行中守护的下一份映像。只认 `smeltd.pending` 这根链接，
/// 不认会被覆盖的 `smeltd.next`。正在跑的就是这份文件时，不当成候选。
fn staged_successor_executable(running: &std::path::Path) -> Option<std::path::PathBuf> {
    let pending = smelt_core::daemon_image::pending_image(running.parent()?)?;
    (!same_device_inode(running, &pending)).then_some(pending)
}

fn same_device_inode(left: &std::path::Path, right: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        matches!(
            (std::fs::canonicalize(left), std::fs::canonicalize(right)),
            (Ok(left), Ok(right)) if left == right
        )
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(left), Ok(right)) = (std::fs::metadata(left), std::fs::metadata(right)) else {
            return false;
        };
        left.dev() == right.dev() && left.ino() == right.ino()
    }
}

/// 进程指纹钉死（启动时调一次）：GUI 拉起时对 spawn 目标的哈希优先；否则对
/// 启动时刻的 current_exe 文件哈希。None 只在文件已消失且无 env 时出现。
fn pinned_daemon_fingerprint(env: Option<String>) -> Option<String> {
    if let Some(fingerprint) = env.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Some(fingerprint);
    }
    daemon_executable_path()
        .ok()
        .and_then(|exe| smelt_core::daemon_image::content_fingerprint(&exe).ok())
}

/// 串行化“检查现有实例 → 清理僵尸 socket → bind”整段启动流程。
///
/// 只做 connect 后 remove_file 存在 TOCTOU：两个并发启动者都可能先观察到
/// socket 不存在，后启动者再把先启动者刚 bind 的有效路径删掉，令先启动者变成
/// 仍托管会话但无法接受新连接的孤立 daemon。flock 随进程退出自动释放，也能覆盖
/// 多个 GUI 进程同时拉起守护的情况。
fn bind_single_instance(
    path: &std::path::Path,
    check_existing: bool,
) -> std::io::Result<Option<UnixListener>> {
    let lock_path = path.with_extension("lock");
    // 锁只覆盖「检查 → 删僵尸 sock → bind」。函数结束就放锁；长期单实例靠套接字文件本身。
    #[cfg(unix)]
    let _lock = {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            // 锁文件只借 flock 语义，不关心内容，绝不截断（可能已有旧内容）。
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        lock
    };
    #[cfg(windows)]
    let _lock = smelt_sys::file_lock::ExclusiveFile::lock(&lock_path)?;

    if check_existing && UnixStream::connect(path).is_ok() {
        return Ok(None);
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).map(Some)
}

fn bind_fresh_daemon(path: &std::path::Path) -> std::io::Result<Option<UnixListener>> {
    bind_single_instance(path, true)
}

fn secure_daemon_socket(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        // sock 文件在用户配置目录 `~/.smelt` 下。目录 ACL 加上同用户对端校验就是边界。
        // 不在 AF_UNIX 文件上再套一层 DACL，也不调用 PermissionsExt。
        let _ = path;
        Ok(())
    }
}

/// 追加一行到 ~/.smelt/daemon.log。给守护交接故障和需要跨进程查看的网络状态留痕——
/// 守护被 SIGKILL（例：装新版时用 cp 覆盖了已签名二进制，upgrade 的 exec 会被
/// macOS 内核直接杀掉，无输出无崩溃报告）或静默 return 时，这份日志是唯一线索：
/// 日志停在「即将 exec」而没有下一行「交接完成」，就是 exec 被杀。
///
/// 同时转一份进全 app 通用的 `app_log`（~/.smelt/app.log，见该模块）——这里记录的
/// 全是异常/生命周期事件，天然也是「关键操作/错误」，没必要在两份日志里分别手写。
pub(crate) fn dlog(msg: &str) {
    use std::io::Write;
    smelt_core::app_log::info("daemon", msg);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sock_path().with_file_name("daemon.log"))
    {
        let _ = writeln!(f, "[{ts}] pid={} {msg}", std::process::id());
    }
}

/// 本进程可执行文件的 mtime（unix 秒）：作为「版本身份」上报给 GUI。GUI 拿磁盘上
/// smeltd 二进制的当前 mtime 一比，就知道正在跑的守护是不是重装/重编译前的旧进程。
fn exe_mtime_secs() -> u64 {
    daemon_executable_path()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 会话控制端：PTY 输入 / resize / 杀进程。
///
/// 持有的是 PTY master 的**裸 fd**（File 包装）而非 portable_pty 的类型。
/// spawn 仍用 portable_pty（openpty + 环境 + 会话组等脏活），起完就把 fd dup 出来自己管。
struct Ctl {
    /// Unix：PTY master 的裸 fd。交接要把这份 fd 原样带过 exec。
    #[cfg(unix)]
    master: std::fs::File,
    /// Windows：ConPTY。不能 CREATE_SUSPENDED，也不把句柄交出去。
    #[cfg(windows)]
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// 会话唯一的 ConPTY 写线程。写端握在那个线程里。按键不再各起一个线程。
    #[cfg(windows)]
    input_tx: std::sync::mpsc::SyncSender<PtyInputWrite>,
    /// reattach 后首个 resize 强制「抖动」（先 rows+1 再回正）：即使尺寸与断开前相同也
    /// 制造 SIGWINCH，让备用屏 TUI（Claude Code 等）重绘整屏，避免重连花屏。
    jolt: bool,
    /// PTY 当前行列。attach 时回报给客户端：重放字节按此宽度生成，GUI 必须把本地
    /// 终端建成同尺寸再解析，否则行宽错位（zsh 行尾 % 盖不掉、TUI 布局撕裂）。
    cols: u16,
    rows: u16,
    /// Canonical cell metrics associated with `cols` / `rows`.
    cell_w: u16,
    cell_h: u16,
    /// A remote watch connection owns PTY geometry while this is non-zero.
    /// Desktop renderers remain attached but must follow the canonical grid
    /// instead of resizing it back to their local viewport.
    remote_viewports: usize,
    /// 远程租约的宽限代号：最后一个远程视口断开后不立刻归还尺寸，这里记下本次
    /// 宽限的编号（0 = 没在宽限）。手机切后台再切回来是最常见的动作，归还再抢回
    /// 会让 PTY 在「桌面尺寸 ↔ 手机尺寸」之间来回 resize，主屏 CLI 每次都把整段
    /// 对话重排重印一遍。见 `pause_remote_viewport`。
    remote_grace: u64,
}

impl Ctl {
    /// 尺寸是不是还握在远程手里：正在看的远程视口，或刚断开、还在宽限期内的那一个。
    fn remote_geometry_pinned(&self) -> bool {
        self.remote_viewports > 0 || self.remote_grace != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalChildExit {
    Reaped,
    AlreadyReaped,
    WaitFailed(Option<i32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalChildState {
    Running,
    Finished(TerminalChildExit),
}

#[cfg(unix)]
fn waitpid_retry(pid: i32, options: i32) -> i32 {
    loop {
        let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), options) };
        if waited >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return waited;
        }
    }
}

#[cfg(unix)]
fn waitid_exact_child(pid: i32, options: i32) -> std::io::Result<()> {
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result =
            unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, info.as_mut_ptr(), options) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

#[cfg(unix)]
/// 不消费退出状态，先确认这个 pid 已经退出，并且仍是本进程的直接子进程。
fn wait_for_exact_child_exit(pid: i32) -> Result<(), TerminalChildExit> {
    match waitid_exact_child(pid, libc::WEXITED | libc::WNOWAIT) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
            Err(TerminalChildExit::AlreadyReaped)
        }
        Err(error) => {
            let code = error.raw_os_error();
            dlog(&format!(
                "terminal: waitid({pid}, WNOWAIT) 失败，退出状态可能未回收：{code:?}"
            ));
            Err(TerminalChildExit::WaitFailed(code))
        }
    }
}

#[cfg(unix)]
fn wait_for_exact_child(pid: i32) -> TerminalChildExit {
    let waited = waitpid_retry(pid, 0);
    if waited == pid {
        return TerminalChildExit::Reaped;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ECHILD) => TerminalChildExit::AlreadyReaped,
        error => {
            dlog(&format!(
                "terminal: waitpid({pid}) 失败，退出状态可能未回收：{error:?}"
            ));
            TerminalChildExit::WaitFailed(error)
        }
    }
}

/// 一个终端 shell 的唯一 `waitpid` 所有者。
///
/// PTY EOF 代表所有持有 slave 的后代都离开了，不等于直接 shell 此刻才退出。若把
/// `waitpid` 放在 EOF 后，shell 先退、后台后代继续持有 PTY 时会永久留成僵尸。这里在
/// PID 发布前立即挂一个精确 waiter；kill 和 PTY pump 都只能观察结果，
/// 不能再竞争退出状态。
struct TerminalChild {
    pid: i32,
    state: Mutex<TerminalChildState>,
    changed: Condvar,
}

impl TerminalChild {
    #[cfg(unix)]
    fn start(pid: i32) -> std::io::Result<Arc<Self>> {
        if pid <= 1 {
            return Ok(Self::finished(pid, TerminalChildExit::AlreadyReaped));
        }
        let child = Arc::new(Self {
            pid,
            state: Mutex::new(TerminalChildState::Running),
            changed: Condvar::new(),
        });

        let reaper = Arc::clone(&child);
        if let Err(error) = thread::Builder::new()
            .name(format!("smelt-terminal-reaper-{pid}"))
            .spawn(move || {
                // waitid(WNOWAIT) 先确认退出，但保留 wait status/PID 槽位。随后在同一把
                // 状态锁内完成 waitpid 与状态发布：kill 要么先锁住 Running，
                // 要么只能看到已经回收完成的 Finished，不存在“PID 已释放、状态仍显示
                // Running”的窗口。
                let observed = wait_for_exact_child_exit(pid);
                let mut state = reaper
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let outcome = match observed {
                    Ok(()) => wait_for_exact_child(pid),
                    Err(outcome) => outcome,
                };
                *state = TerminalChildState::Finished(outcome);
                reaper.changed.notify_all();
            })
        {
            // Session 尚未发布，没有别的 owner。线程都起不来时必须在这里同步止损，
            // 不能把一个从此无人 wait 的直接子进程交给调用方。
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = waitpid_retry(pid, 0);
            return Err(error);
        }
        Ok(child)
    }

    /// ConPTY 不能 CREATE_SUSPENDED。先记下 Child，再尽量放进 Job。
    /// Job 没挂上时进程还在，结束时 `terminate_pid` 收它自己。
    #[cfg(windows)]
    fn start_with_child(
        child: Box<dyn portable_pty::Child + Send>,
    ) -> std::io::Result<Arc<Self>> {
        let Some(pid) = child.process_id().and_then(|pid| i32::try_from(pid).ok()) else {
            return Err(std::io::Error::other("拿不到 shell pid"));
        };
        if pid <= 1 {
            return Ok(Self::finished(pid, TerminalChildExit::AlreadyReaped));
        }
        if let Err(error) = smelt_sys::process::attach_running(pid as u32) {
            dlog(&format!(
                "terminal: shell pid={pid} 没能放进 Job（{error}），结束时改为结束这个 pid"
            ));
        }
        let tracked = Arc::new(Self {
            pid,
            state: Mutex::new(TerminalChildState::Running),
            changed: Condvar::new(),
        });
        let child = Mutex::new(child);
        let reaper = Arc::clone(&tracked);
        if let Err(error) = thread::Builder::new()
            .name(format!("smelt-terminal-reaper-{pid}"))
            .spawn(move || {
                let outcome = loop {
                    match child.lock().unwrap().try_wait() {
                        Ok(Some(_)) => break TerminalChildExit::Reaped,
                        Ok(None) => thread::sleep(Duration::from_millis(200)),
                        Err(error) => {
                            break TerminalChildExit::WaitFailed(error.raw_os_error())
                        }
                    }
                };
                // shell 已经退出。丢掉 Job，KILL_ON_JOB_CLOSE 收掉还在组里的孙子。
                smelt_sys::process::release_tree(pid as u32);
                let mut state = reaper
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                *state = TerminalChildState::Finished(outcome);
                reaper.changed.notify_all();
            })
        {
            let _ = smelt_sys::process::terminate_pid(pid as u32);
            return Err(error);
        }
        Ok(tracked)
    }

    fn finished(pid: i32, exit: TerminalChildExit) -> Arc<Self> {
        Arc::new(Self {
            pid,
            state: Mutex::new(TerminalChildState::Finished(exit)),
            changed: Condvar::new(),
        })
    }

    fn pid(&self) -> i32 {
        self.pid
    }

    fn wait_reaped(&self, timeout: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                matches!(state, TerminalChildState::Running)
            })
            .unwrap_or_else(|error| error.into_inner());
        matches!(
            *state,
            TerminalChildState::Finished(
                TerminalChildExit::Reaped | TerminalChildExit::AlreadyReaped
            )
        )
    }

    fn terminate_and_wait(&self, timeout: Duration) -> bool {
        if self.pid <= 1 {
            return true;
        }
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(*state, TerminalChildState::Running) {
            #[cfg(unix)]
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
            #[cfg(windows)]
            {
                let pid = self.pid as u32;
                let _ = smelt_sys::process::signal_tree(pid, smelt_sys::process::TreeSignal::Kill);
                smelt_sys::process::release_tree(pid);
                let _ = smelt_sys::process::terminate_pid(pid);
            }
        }
        drop(state);
        self.wait_reaped(timeout)
    }
}

#[cfg(unix)]
trait PtyReader: Read + AsRawFd + Send {}

#[cfg(unix)]
impl<T: Read + AsRawFd + Send> PtyReader for T {}

#[cfg(windows)]
trait PtyReader: Read + Send {}

#[cfg(windows)]
impl<T: Read + Send> PtyReader for T {}

/// ConPTY 的读端没有 poll。单独线程阻塞读，泵用 1 秒超时醒过来刷进度。
#[cfg(windows)]
struct ThreadedPtyReader {
    rx: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    offset: usize,
}

#[cfg(windows)]
fn threaded_pty_reader(mut reader: Box<dyn Read + Send>) -> ThreadedPtyReader {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok(Vec::new()));
                    break;
                }
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                    break;
                }
            }
        }
    });
    ThreadedPtyReader {
        rx,
        pending: Vec::new(),
        offset: 0,
    }
}

#[cfg(windows)]
impl Read for ThreadedPtyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.offset < self.pending.len() {
            let n = (self.pending.len() - self.offset).min(buf.len());
            buf[..n].copy_from_slice(&self.pending[self.offset..self.offset + n]);
            self.offset += n;
            if self.offset == self.pending.len() {
                self.pending.clear();
                self.offset = 0;
            }
            return Ok(n);
        }
        match self.rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(data)) if data.is_empty() => Ok(0),
            Ok(Ok(data)) => {
                self.pending = data;
                self.offset = 0;
                self.read(buf)
            }
            Ok(Err(error)) => Err(error),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(std::io::Error::new(
                ErrorKind::WouldBlock,
                "pty read timeout",
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(0),
        }
    }
}

/// 本进程启动时刻（unix 秒），`version` op 回给 GUI 展示「守护跑了多久」。
/// 必须在 main 最开头取一次，否则记的是「首次有人问」的时间。
///
/// 守护重启后是全新进程，这个值会重置。进行中的 PTY 不会跟过来。
fn started_at() -> u64 {
    static STARTED_AT: OnceLock<u64> = OnceLock::new();
    *STARTED_AT.get_or_init(now_unix)
}

/// 从 daemon 领域 owner 收集的一次投影种子。它只用于刷新 EventHub 的 snapshot
/// provider，不承担分发职责。
#[derive(Clone, Default)]
struct DaemonProjectionSeed {
    sessions: Vec<SessionState>,
    remote_sessions: Option<RemoteSessionSnapshot>,
    workspace_menu: Option<WorkspaceMenuSnapshot>,
    automations: Option<AutomationFile>,
}

type RemoteCatalogLoader = fn() -> Result<RemoteSessionCatalog, String>;

/// A daemon owns this catalog for its entire lifetime. A broken historical file is fail-closed:
/// normal terminal/ACP service continues, but remote commands cannot overwrite an unknown catalog.
struct RemoteCatalogState {
    catalog: Option<RemoteSessionCatalog>,
    load_error: Option<String>,
    loader: RemoteCatalogLoader,
    /// Daemon-local catalog ownership. Active persistence and this generation bind under the
    /// same mutex; delayed cleanup must compare it before an id-scoped catalog deletion.
    runtime_instances: HashMap<(RemoteSessionKind, String), u64>,
}

impl RemoteCatalogState {
    fn load_default() -> Self {
        Self::with_loader(RemoteSessionCatalog::load_default)
    }

    fn with_loader(loader: RemoteCatalogLoader) -> Self {
        let mut state = Self {
            catalog: None,
            load_error: None,
            loader,
            runtime_instances: HashMap::new(),
        };
        state.ensure_loaded();
        state
    }

    #[cfg(test)]
    fn in_memory() -> Self {
        Self {
            catalog: Some(RemoteSessionCatalog::in_memory()),
            load_error: None,
            loader: RemoteSessionCatalog::load_default,
            runtime_instances: HashMap::new(),
        }
    }

    /// 目录还没加载出来就再试一次。
    ///
    /// 打开失败往往只是**一瞬间**的事：磁盘临时不可写、sidecar 和主库对不上、
    /// 另一个进程正在替换文件。把首次失败当成终局，代价是整个 daemon 生命周期
    /// 里远程目录都不可用——手机端永远只有一句英文错误，桌面也发布不了菜单，
    /// 而唯一的出路是重启进程。故障消失之后，下一次访问就该自己走出来。
    ///
    /// 不做节流：只有目录处于不可用状态时才会走到这里，而这条路上的调用（远程
    /// 生命周期命令、订阅建立）本来就不是高频路径。
    fn ensure_loaded(&mut self) {
        if self.catalog.is_some() {
            return;
        }
        match (self.loader)() {
            Ok(catalog) => {
                if self.load_error.take().is_some() {
                    eprintln!("[remote] 远程会话目录已恢复，重新接受远程生命周期命令");
                }
                self.catalog = Some(catalog);
            }
            Err(error) => {
                // 故障持续时每次访问都会重试，日志只在错况变化时记一条，不刷屏。
                if self.load_error.as_deref() != Some(error.as_str()) {
                    eprintln!("[remote] 远程会话目录不可用，拒绝远程生命周期命令：{error}");
                }
                self.load_error = Some(error);
            }
        }
    }

    fn catalog(&mut self) -> Result<&RemoteSessionCatalog, String> {
        self.ensure_loaded();
        self.catalog.as_ref().ok_or_else(|| {
            self.load_error
                .clone()
                .unwrap_or_else(|| "remote session catalog unavailable".to_string())
        })
    }

    fn catalog_mut(&mut self) -> Result<&mut RemoteSessionCatalog, String> {
        self.ensure_loaded();
        let error = self.load_error.clone();
        self.catalog.as_mut().ok_or_else(|| {
            error.unwrap_or_else(|| "remote session catalog unavailable".to_string())
        })
    }
}

type RemoteSessions = Arc<Mutex<RemoteCatalogState>>;
type WorkspaceMenuStore = Arc<Mutex<WorkspaceMenuSnapshot>>;

fn new_remote_sessions() -> RemoteSessions {
    Arc::new(Mutex::new(RemoteCatalogState::load_default()))
}

fn new_workspace_menu() -> WorkspaceMenuStore {
    Arc::new(Mutex::new(load_published_workspace_menu()))
}

#[cfg(test)]
fn new_test_automation_store() -> AutomationStore {
    Arc::new(Mutex::new(
        smelt_core::automation_store::AutomationOwner::in_memory(),
    ))
}

#[cfg(test)]
fn new_test_remote_sessions() -> RemoteSessions {
    Arc::new(Mutex::new(RemoteCatalogState::in_memory()))
}

#[cfg(test)]
fn new_test_workspace_menu() -> WorkspaceMenuStore {
    Arc::new(Mutex::new(WorkspaceMenuSnapshot::default()))
}

#[cfg(test)]
pub(crate) fn new_event_hub() -> EventHubHandle {
    event_hub::DaemonEventHub::new().unwrap()
}

/// 已发布身份目录。只在 main() 里初始化一次；测试默认不初始化，
/// 不会写真实会话持久层。
static SESSION_DIRECTORY: RwLock<Option<SessionDirectory>> = RwLock::new(None);
#[cfg(test)]
static SESSION_DIRECTORY_TEST_OWNER: Mutex<Option<thread::ThreadId>> = Mutex::new(None);

/// 订阅快照与身份目录是同一份逻辑状态的两个投影。所有 state/remove 必须通过这一
/// 提交门线性化，避免某个生产者的旧状态被总线拒绝却仍写入目录。
static STATE_DIRECTORY_COMMIT_GATE: Mutex<()> = Mutex::new(());

/// 测试辅助：注入身份目录。中毒锁照常恢复，避免污染后续用例。
#[cfg(test)]
pub(crate) fn set_session_directory_for_test(directory: SessionDirectory) {
    *SESSION_DIRECTORY_TEST_OWNER
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(thread::current().id());
    *SESSION_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) = Some(directory);
}

#[cfg(test)]
pub(crate) fn clear_session_directory_for_test() {
    *SESSION_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) = None;
    *SESSION_DIRECTORY_TEST_OWNER
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

/// 在身份目录（未初始化时为 None）上执行闭包。guard 不逃出闭包。
fn with_session_directory<T>(f: impl FnOnce(&SessionDirectory) -> T) -> Option<T> {
    #[cfg(test)]
    if SESSION_DIRECTORY_TEST_OWNER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        != Some(&thread::current().id())
    {
        return None;
    }
    let guard = SESSION_DIRECTORY.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(f)
}

/// State producers only touch EventHub. Socket fanout happens in independent
/// subscription tasks, so a frozen reader cannot serialize or block future state transitions.
///
/// 持久状态的广播出口：hook / ACP / 生命周期变化走这里。
/// 测试未初始化全局时不写真实会话持久层。
fn broadcast_state(event_hub: &EventHubHandle, state: &SessionState) {
    let _commit = STATE_DIRECTORY_COMMIT_GATE.lock().unwrap();
    match event_hub.publish_session(state) {
        Ok(true) => {
            with_session_directory(|directory| directory.upsert(state));
        }
        Ok(false) => {}
        Err(error) => eprintln!("[event-hub] session state publish failed: {error}"),
    }
}

/// 身份删除口：精确 runtime 由 instance 退休；无 instance 只清理没有代际信息的
/// 历史条目，避免迟到的旧清理覆盖同 id 的替换 runtime。
fn forget_session(event_hub: &EventHubHandle, id: &str, instance: u64) {
    let _commit = STATE_DIRECTORY_COMMIT_GATE.lock().unwrap();
    match event_hub.remove_session(id, instance) {
        Ok(true) => {
            with_session_directory(|directory| directory.remove_instance(id, instance));
        }
        Ok(false) => {}
        Err(error) => eprintln!("[event-hub] session removal publish failed: {error}"),
    }
}

/// Remote ownership is a small catalog, so each change publishes a complete replacement. This
/// gives lagging clients a convergent state without treating catalog events as a durable log.
fn broadcast_remote_sessions(event_hub: &EventHubHandle, snapshot: &RemoteSessionSnapshot) {
    if let Err(error) = event_hub.publish_remote_sessions(snapshot) {
        eprintln!("[event-hub] remote sessions publish failed: {error}");
    }
}

fn broadcast_workspace_menu(event_hub: &EventHubHandle, snapshot: &WorkspaceMenuSnapshot) {
    if let Err(error) = event_hub.publish_workspace_menu(snapshot) {
        eprintln!("[event-hub] workspace menu publish failed: {error}");
    }
}

fn publish_workspace_menu_snapshot(
    store: &WorkspaceMenuStore,
    event_hub: &EventHubHandle,
    mut menu: WorkspaceMenuSnapshot,
) -> Result<WorkspaceMenuSnapshot, String> {
    let mut stored = store.lock().unwrap();
    // 同一桌面实例的 RPC 可能因断线重试或后台完成顺序而倒序抵达。daemon revision
    // 只能表达“已接收顺序”，不能判断哪个 payload 更新，因此同时保存发布者序号。
    if !menu.source_id.is_empty()
        && menu.source_id == stored.source_id
        && menu.source_revision != 0
        && menu.source_revision <= stored.source_revision
    {
        return Ok(stored.clone());
    }
    menu.revision = stored.revision.saturating_add(1);
    persist_published_workspace_menu(&menu)?;
    *stored = menu.clone();
    drop(stored);
    broadcast_workspace_menu(event_hub, &menu);
    Ok(menu)
}

fn workspace_menu_snapshot(store: &WorkspaceMenuStore) -> WorkspaceMenuSnapshot {
    store.lock().unwrap().clone()
}

fn remote_session_snapshot(
    remote_sessions: &RemoteSessions,
) -> Result<RemoteSessionSnapshot, String> {
    let mut catalog = remote_sessions.lock().unwrap();
    Ok(catalog.catalog()?.snapshot())
}

fn mutate_remote_catalog<T>(
    remote_sessions: &RemoteSessions,
    mutation: impl FnOnce(&mut RemoteSessionCatalog) -> Result<T, String>,
) -> Result<(T, RemoteSessionSnapshot), String> {
    let mut state = remote_sessions.lock().unwrap();
    let value = {
        let catalog = state.catalog_mut()?;
        mutation(catalog)?
    };
    let snapshot = state.catalog()?.snapshot();
    Ok((value, snapshot))
}

fn set_remote_lifecycle(
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
    kind: RemoteSessionKind,
    id: &str,
    lifecycle: RemoteSessionLifecycle,
) -> Result<bool, String> {
    let (record, snapshot) = mutate_remote_catalog(remote_sessions, |catalog| {
        catalog.set_lifecycle_for_kind(kind, id, lifecycle)
    })?;
    let changed = record.is_some();
    if changed {
        broadcast_remote_sessions(event_hub, &snapshot);
    }
    Ok(changed)
}

#[cfg(test)]
fn bind_remote_session_instance(
    remote_sessions: &RemoteSessions,
    kind: RemoteSessionKind,
    id: &str,
    instance: u64,
) {
    remote_sessions
        .lock()
        .unwrap()
        .runtime_instances
        .insert((kind, id.to_string()), instance);
}

fn activate_remote_session_instance(
    remote_sessions: &RemoteSessions,
    kind: RemoteSessionKind,
    id: &str,
    instance: u64,
) -> Result<RemoteSessionSnapshot, String> {
    let mut state = remote_sessions.lock().unwrap();
    let record =
        state
            .catalog_mut()?
            .set_lifecycle_for_kind(kind, id, RemoteSessionLifecycle::Active)?;
    if record.is_none() {
        return Err("remote session disappeared".to_string());
    }
    state
        .runtime_instances
        .insert((kind, id.to_string()), instance);
    Ok(state.catalog()?.snapshot())
}

fn remove_remote_session_for_instance(
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
    kind: RemoteSessionKind,
    id: &str,
    instance: u64,
) -> Result<bool, String> {
    let key = (kind, id.to_string());
    let mut state = remote_sessions.lock().unwrap();
    if state.runtime_instances.get(&key).copied() != Some(instance) {
        return Ok(false);
    }
    // All production callers hold the slot lifecycle while retiring this instance. Once the
    // generation matches, no replacement can bind the same id before this function returns.
    // Release the in-memory binding even when durable deletion fails; otherwise a later
    // no-runtime retry is rejected forever by `remove_remote_session_if_unbound`.
    let remove_result = state
        .catalog_mut()
        .and_then(|catalog| catalog.remove_for_kind(kind, id));
    state.runtime_instances.remove(&key);
    let record = remove_result?;
    let snapshot = state.catalog()?.snapshot();
    drop(state);
    let changed = record.is_some();
    if changed {
        broadcast_remote_sessions(event_hub, &snapshot);
    }
    Ok(changed)
}

fn remove_remote_session_if_unbound(
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
    kind: RemoteSessionKind,
    id: &str,
) -> Result<bool, String> {
    let key = (kind, id.to_string());
    let mut state = remote_sessions.lock().unwrap();
    if state.runtime_instances.contains_key(&key) {
        return Ok(false);
    }
    let record = state.catalog_mut()?.remove_for_kind(kind, id)?;
    let snapshot = state.catalog()?.snapshot();
    drop(state);
    let changed = record.is_some();
    if changed {
        broadcast_remote_sessions(event_hub, &snapshot);
    }
    Ok(changed)
}

fn apply_history_rename(
    kind: smelt_core::agent_kind::HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
    title: Option<&str>,
    cwd: Option<&str>,
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
) -> Result<(), String> {
    if resume_id.trim().is_empty() {
        return Err("missing resume_id".to_string());
    }
    smelt_core::session_metadata::set_custom_title(kind, profile_id, resume_id, title)?;
    // 远程会话目录只认 ACP 会话；纯终端来源（Antigravity）改的名只落在标题
    // 覆盖层，不需要、也无法去同步一条并不存在的 ACP 记录。
    let option = kind
        .acp()
        .and_then(|acp| find_agent_option_for(acp, profile_id));
    let catalog_title = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let option = option.as_ref()?;
            let cwd = cwd.filter(|value| !value.trim().is_empty())?;
            smelt_core::session_control::list_history_for(
                kind,
                profile_id,
                cwd,
                option.history_dir.as_deref(),
            )
            .into_iter()
            .find(|session| session.resume_id == resume_id)
            .map(|session| session.display_title().to_string())
        });
    if let (Some(option), Some(catalog_title)) = (option, catalog_title)
        && !catalog_title.is_empty()
    {
        let (changed, snapshot) = mutate_remote_catalog(remote_sessions, |catalog| {
            catalog.rename_acp_by_resume_id(&option.id, resume_id, &catalog_title)
        })?;
        if changed {
            broadcast_remote_sessions(event_hub, &snapshot);
        }
    }
    Ok(())
}

fn handle_history_rename<C: ClientSocket>(
    mut conn: C,
    v: &serde_json::Value,
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
) {
    let result = (|| -> Result<(), String> {
        let agent = v["agent"].as_str().unwrap_or_default();
        let kind = smelt_core::agent_kind::HistorySourceKind::from_id(agent)
            .ok_or_else(|| "unknown history agent".to_string())?;
        let resume_id = v["resume_id"].as_str().unwrap_or_default();
        let profile_id = v["profile_id"].as_str();
        let title = v.get("title").and_then(serde_json::Value::as_str);
        let cwd = v["cwd"].as_str();
        apply_history_rename(
            kind,
            profile_id,
            resume_id,
            title,
            cwd,
            remote_sessions,
            event_hub,
        )
    })();
    let response = match result {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
}

fn handle_workspace_menu<C: ClientSocket>(
    mut conn: C,
    v: &serde_json::Value,
    store: &WorkspaceMenuStore,
    event_hub: &EventHubHandle,
) {
    let result = (|| -> Result<WorkspaceMenuSnapshot, String> {
        let menu = v
            .get("menu")
            .cloned()
            .ok_or_else(|| "missing menu".to_string())?;
        let menu: WorkspaceMenuSnapshot = serde_json::from_value(menu)
            .map_err(|error| format!("invalid workspace menu: {error}"))?;
        if menu.version == 0 {
            return Err("workspace menu version must be current".to_string());
        }
        publish_workspace_menu_snapshot(store, event_hub, menu)
    })();
    let response = match result {
        Ok(menu) => serde_json::json!({ "ok": true, "revision": menu.revision }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
}

pub(crate) fn recover_locked_automations(
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
) {
    let mut owner = automations.lock().unwrap();
    if owner.recover_if_locked(chrono::Local::now(), local_timezone_fingerprint())
        && let Err(error) =
            event_hub.publish_automations(&webhook::annotate_snapshot(owner.snapshot()))
    {
        eprintln!("[automation-runtime] 发布自动化投影失败: {error}");
    }
}

fn handle_automations_snapshot<C: ClientSocket>(
    mut conn: C,
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
) {
    recover_locked_automations(automations, event_hub);
    let snapshot = webhook::annotate_snapshot(automations.lock().unwrap().snapshot());
    let _ = smelt_runtime_client::protocol::write_json_line(
        &mut conn,
        &serde_json::json!({ "ok": true, "automations": snapshot }),
    );
}

fn handle_automation_command<C: ClientSocket>(
    mut conn: C,
    v: &serde_json::Value,
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
) {
    recover_locked_automations(automations, event_hub);
    let result = (|| -> Result<_, String> {
        let command = v
            .get("command")
            .cloned()
            .ok_or_else(|| "missing automation command".to_string())?;
        let command: AutomationCommand = serde_json::from_value(command)
            .map_err(|error| format!("invalid automation command: {error}"))?;
        let mut owner = automations.lock().unwrap();
        let applied = match command {
            AutomationCommand::RunOnce { automation_id } => {
                owner.run_once(automation_id, chrono::Local::now())
            }
            command => owner.apply(command, chrono::Local::now()),
        }?;
        if applied.changed
            && let Err(error) =
                event_hub.publish_automations(&webhook::annotate_snapshot(applied.snapshot.clone()))
        {
            eprintln!("[automation-runtime] 发布自动化投影失败: {error}");
        }
        Ok(applied)
    })();
    let response = match result {
        Ok(applied) => {
            if applied.changed {
                automation_runtime::nudge_automation_driver();
            }
            serde_json::json!({
                "ok": true,
                "revision": applied.snapshot.revision,
                "automations": webhook::annotate_snapshot(applied.snapshot),
                "result": applied.result,
            })
        }
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
}

fn handle_event_publish<C: ClientSocket>(
    mut conn: C,
    v: &serde_json::Value,
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
) {
    recover_locked_automations(automations, event_hub);
    let result = (|| -> Result<_, String> {
        let event = v
            .get("event")
            .cloned()
            .ok_or_else(|| "missing event".to_string())?;
        let event: AutomationInboundEvent =
            serde_json::from_value(event).map_err(|error| format!("invalid event: {error}"))?;
        let mut owner = automations.lock().unwrap();
        let applied = owner.publish_event(event, chrono::Local::now())?;
        if applied.changed
            && let Err(error) =
                event_hub.publish_automations(&webhook::annotate_snapshot(applied.snapshot.clone()))
        {
            eprintln!("[automation-runtime] 发布自动化投影失败: {error}");
        }
        Ok(applied)
    })();
    let response = match result {
        Ok(applied) => {
            if !applied.published.runs.is_empty() {
                automation_runtime::nudge_automation_driver();
            }
            serde_json::json!({
                "ok": true,
                "event_id": applied.published.event_id,
                "topic": applied.published.topic,
                "matched": applied.published.runs.len() + applied.published.already_recorded,
                "accepted": applied.published.runs.len(),
                "duplicate": applied.published.runs.is_empty()
                    && applied.published.already_recorded > 0,
                "runs": applied.published.runs.iter().map(|run| serde_json::json!({
                    "id": run.id,
                    "automation_id": run.automation_id,
                    "status": run.status,
                })).collect::<Vec<_>>(),
                "revision": applied.snapshot.revision,
            })
        }
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &response);
}

/// 落盘记录不能证明进程还在。对外服务之前，按
/// `catalog ∩ runtime` 对账：活着的提成 Active，没有进程的删掉。
fn reconcile_remote_catalog_runtime(
    remote_sessions: &RemoteSessions,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
) {
    let mut terminal_slots = sessions.snapshot_slots();
    terminal_slots.sort_by(|left, right| left.0.cmp(&right.0));
    let mut terminal_lifecycles = Vec::with_capacity(terminal_slots.len());
    let mut live_terminal_instances = HashMap::new();
    for (id, slot) in &terminal_slots {
        let lifecycle = slot.lifecycle.lock().unwrap();
        if sessions.is_current(id, slot)
            && let Some(session) = sessions.live_in_slot(slot)
        {
            live_terminal_instances.insert(id.clone(), session.instance);
        }
        terminal_lifecycles.push(lifecycle);
    }

    let mut acp_slots = acp_sessions.snapshot();
    acp_slots.sort_by(|left, right| left.0.cmp(&right.0));
    let mut acp_lifecycles = Vec::with_capacity(acp_slots.len());
    let mut live_acp_instances = HashMap::new();
    for (id, slot) in &acp_slots {
        let lifecycle = slot.lifecycle.lock().unwrap();
        if acp_sessions.is_current(id, slot) {
            live_acp_instances.insert(id.clone(), slot.value.instance);
        }
        acp_lifecycles.push(lifecycle);
    }
    let live_terminal_ids = live_terminal_instances
        .keys()
        .cloned()
        .collect::<HashSet<_>>();
    let live_acp_ids = live_acp_instances.keys().cloned().collect::<HashSet<_>>();
    let result = (|| -> Result<(bool, RemoteSessionSnapshot), String> {
        let mut state = remote_sessions.lock().unwrap();
        let changed = state
            .catalog_mut()?
            .retain_live_runtimes(&live_acp_ids, &live_terminal_ids)?;
        let snapshot = state.catalog()?.snapshot();
        state.runtime_instances.clear();
        for record in &snapshot.sessions {
            let instance = match record.kind {
                RemoteSessionKind::Terminal => live_terminal_instances.get(&record.id),
                RemoteSessionKind::Acp => live_acp_instances.get(&record.id),
            };
            if let Some(instance) = instance {
                state
                    .runtime_instances
                    .insert((record.kind, record.id.clone()), *instance);
            }
        }
        Ok((changed, snapshot))
    })();
    // 不要把 registry 生命周期锁带进广播；目录提交完成后，正常的 kill/cleanup
    // 可以继续取得 slot 锁并观察到同一份 runtime 绑定。
    drop(terminal_lifecycles);
    drop(acp_lifecycles);
    match result {
        Ok((true, snapshot)) => broadcast_remote_sessions(event_hub, &snapshot),
        Ok((false, _)) => {}
        Err(error) => eprintln!("[remote] 启动时远程目录对账失败：{error}"),
    }
}

/// `None` means the catalog itself is unavailable. Callers may still service an ordinary local
/// runtime, but must not create a new remote-owned one in that state.
fn is_known_remote_session(
    remote_sessions: &RemoteSessions,
    kind: RemoteSessionKind,
    id: &str,
) -> Option<bool> {
    let state = remote_sessions.lock().unwrap();
    state
        .catalog
        .as_ref()
        .map(|catalog| catalog.kind_for(id) == Some(kind))
}

/// 常驻 Term 的事件监听。OSC 标题留在终端网格自己的标题栏，这里直接丢掉。
/// Bell 只刷新活动时间并广播。颜色查询在网格锁内入队，泵释放锁后再写回。
#[derive(Clone)]
struct StateListener {
    state: Arc<Mutex<SessionState>>,
    event_hub: EventHubHandle,
    /// `ColorRequest` 在 `Term` 网格锁内回调。这里只入队，等 PTY 泵释放网格锁后
    /// 再经 `Ctl.master` 写入，避免与 resize 的 ctl -> term 锁序相反而死锁。
    color_replies: PendingColorReplies,
}

/// 一条待回应的颜色查询：查哪个颜色槽，和把颜色格式化成回应的函数。颜色到写回时
/// 才定：程序用 OSC 4 / 10 / 11 改过的值要优先，而它记在 `Term` 里，入队时拿不到锁。
type PendingColorReply = (usize, Arc<dyn Fn(Rgb) -> String + Send + Sync>);
type PendingColorReplies = Arc<Mutex<VecDeque<PendingColorReply>>>;

impl StateListener {
    #[cfg(test)]
    fn new(state: Arc<Mutex<SessionState>>, event_hub: EventHubHandle) -> Self {
        Self::with_color_replies(state, event_hub, Arc::new(Mutex::new(VecDeque::new())))
    }

    fn with_color_replies(
        state: Arc<Mutex<SessionState>>,
        event_hub: EventHubHandle,
        color_replies: PendingColorReplies,
    ) -> Self {
        Self {
            state,
            event_hub,
            color_replies,
        }
    }
}

impl EventListener for StateListener {
    fn send_event(&self, event: Event) {
        if let Event::ColorRequest(index, format) = &event {
            if let Ok(mut replies) = self.color_replies.lock() {
                replies.push_back((*index, Arc::clone(format)));
            }
            return;
        }
        // OSC 0/2 不是会话名，也不广播。
        let Event::Bell = event else {
            return;
        };

        let snapshot = {
            let Ok(mut st) = self.state.lock() else {
                return;
            };
            st.updated_at = now_unix();
            bump_state_revision(&mut st);
            st.clone()
        };
        broadcast_state(&self.event_hub, &snapshot);
    }
}

/// 把守护所在机器最近一次由 GUI 发布的终端主题转换成 OSC 颜色查询应答。
/// GUI 尚未 attach 时，PTY 输出已经先经过这里；因此 Grok 这类启动即问 `OSC 11`
/// 的 TUI 不会因首包被无人接收而退回错误的配色分支。
fn resolve_terminal_color(index: usize) -> Rgb {
    let theme = smelt_core::terminal_theme::load();
    resolve_terminal_theme_color(&theme, index)
}

fn resolve_terminal_theme_color(
    theme: &smelt_core::terminal_theme::TerminalThemeSnapshot,
    index: usize,
) -> Rgb {
    let color = theme.palette.get(index).copied().unwrap_or({
        if index == NamedColor::Background as usize {
            theme.background
        } else {
            theme.foreground
        }
    });
    Rgb {
        r: ((color >> 16) & 0xff) as u8,
        g: ((color >> 8) & 0xff) as u8,
        b: (color & 0xff) as u8,
    }
}

/// 按行列 + 可选像素尺寸 resize PTY（TIOCSWINSZ）。
/// `xpixel`/`ypixel` 是**整窗**像素（cols×cell_w / rows×cell_h），不是单格。
#[cfg(unix)]
fn resize_fd(fd: RawFd, rows: u16, cols: u16, xpixel: u16, ypixel: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: xpixel,
        ws_ypixel: ypixel,
    };
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
    }
}

/// 会话 resize：PTY ioctl + 常驻 Term 同步 + 可选 jolt 抖动。
/// 手机远程与 GUI open 帧共用，避免两套尺寸逻辑漂移。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResizeOrigin {
    Desktop,
    Remote,
}

/// Upper bounds on a session's grid. These mirror the limits `attach` already
/// applied to its JSON payload; they exist to keep a bad request from asking
/// for an allocation large enough to kill the daemon, not to describe any
/// terminal anyone actually uses.
const MAX_SESSION_COLS: u16 = 1000;
const MAX_SESSION_ROWS: u16 = 1000;
const MAX_SESSION_CELL_PX: u16 = 256;

fn resize_session(sess: &Session, cols: u16, rows: u16, cell_w: u16, cell_h: u16) {
    let _ = resize_session_from(sess, cols, rows, cell_w, cell_h, ResizeOrigin::Desktop);
}

fn resize_session_remote(sess: &Session, cols: u16, rows: u16, cell_w: u16, cell_h: u16) {
    let _ = resize_session_from(sess, cols, rows, cell_w, cell_h, ResizeOrigin::Remote);
}

fn resize_session_from(
    sess: &Session,
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
    origin: ResizeOrigin,
) -> bool {
    // Clamp here rather than at each call site. `attach` already bounded its
    // JSON fields, but the in-band resize frame did not, and a grid is
    // allocated eagerly — 65535x65535 is four billion cells, so an oversized
    // request aborts the daemon and takes every session on the machine with it.
    // Anything that can reach the socket can send that frame, so the bound
    // belongs on the one path they all funnel through.
    let cols = cols.clamp(1, MAX_SESSION_COLS);
    let rows = rows.clamp(1, MAX_SESSION_ROWS);
    let cell_w = cell_w.min(MAX_SESSION_CELL_PX);
    let cell_h = cell_h.min(MAX_SESSION_CELL_PX);
    // 先拿输出顺序门闩。PTY 泵、attach、watch 和 resize 必须共享同一条输出序列，
    // 但门闩与 `out` 列表锁分离，慢 socket 写不会阻塞其它会话管理操作。
    let _output_gate = sess.output_gate.lock().unwrap();
    let refused_marker = {
        let mut ctl = sess.ctl.lock().unwrap();
        if origin == ResizeOrigin::Desktop && ctl.remote_viewports > 0 {
            // 手机正在看：尺寸归它，桌面跟随。拒了还得把 canonical grid 再告诉桌面
            // 一次——它刚才为了抢尺寸已经把本地 VT 改成自己的网格了（focus claim），
            // 不回一帧标记它就会按错尺寸画到下一次 resize。
            Some(terminal_geometry_osc(
                &sess.geometry_token,
                TerminalGeometryOsc {
                    cols: ctl.cols,
                    rows: ctl.rows,
                    cell_width: ctl.cell_w,
                    cell_height: ctl.cell_h,
                    remote_controlled: true,
                },
            ))
        } else {
            if origin == ResizeOrigin::Desktop {
                // 只剩宽限期的话，桌面主动改尺寸就是「人回到 PC 并且在用这个终端」的信号
                // （GUI 只在终端获得焦点时发这一帧，见 terminal_view 的 focus claim），
                // 租约当场作废。
                ctl.remote_grace = 0;
            }
            None
        }
    };
    if let Some(marker) = refused_marker {
        dispatch_session_outputs_under_gate(sess, &marker, "<resize-refused>", true, false);
        return false;
    }
    let (resize_token, jolt, xpixel, ypixel, marker) = {
        let mut ctl = sess.ctl.lock().unwrap();
        if cell_w > 0 {
            ctl.cell_w = cell_w;
        }
        if cell_h > 0 {
            ctl.cell_h = cell_h;
        }
        let cell_w = ctl.cell_w;
        let cell_h = ctl.cell_h;
        let xpixel = cols.saturating_mul(cell_w);
        let ypixel = rows.saturating_mul(cell_h);
        #[cfg(unix)]
        let fd = ctl.master.as_raw_fd();
        let jolt = std::mem::take(&mut ctl.jolt);
        ctl.cols = cols;
        ctl.rows = rows;
        let remote_controlled = ctl.remote_geometry_pinned();

        // Serialize the invisible geometry marker before SIGWINCH can produce
        // cursor-addressed output at the new size. Desktop renderers resize their
        // local VT model from this marker without echoing a resize frame.
        let marker = if let Ok(mut term) = sess.term.lock() {
            term.resize(DaemonTermSize {
                rows: rows as usize,
                cols: cols as usize,
            });
            Some(terminal_geometry_osc(
                &sess.geometry_token,
                TerminalGeometryOsc {
                    cols,
                    rows,
                    cell_width: cell_w,
                    cell_height: cell_h,
                    remote_controlled,
                },
            ))
        } else {
            None
        };
        #[cfg(unix)]
        let resize_token = fd;
        #[cfg(windows)]
        let resize_token = ();
        let _ = (xpixel, ypixel);
        (resize_token, jolt, xpixel, ypixel, marker)
    };
    if let Some(marker) = marker {
        dispatch_session_outputs_under_gate(sess, &marker, "<resize>", true, false);
    }

    #[cfg(unix)]
    {
        if jolt {
            resize_fd(resize_token, rows.saturating_add(1), cols, xpixel, ypixel);
        }
        resize_fd(resize_token, rows, cols, xpixel, ypixel);
    }
    #[cfg(windows)]
    {
        let _ = resize_token;
        let size = |rows: u16| portable_pty::PtySize {
            rows,
            cols,
            pixel_width: xpixel,
            pixel_height: ypixel,
        };
        let ctl = sess.ctl.lock().unwrap();
        if jolt {
            let _ = ctl.master.resize(size(rows.saturating_add(1)));
        }
        let _ = ctl.master.resize(size(rows));
    }
    true
}

/// 远程视口接入。返回本次是否真的需要把进程抖醒（几何变了，或本来就挂着 jolt）。
///
/// jolt（先 rows+1 再回正，制造一次必然的 SIGWINCH）只在几何**真的变了**时才打：
/// 它的作用是盖掉 reflow 后的旧尺寸残帧、逼只重排半屏的 TUI 重画。尺寸没变就没有
/// reflow，守护的常驻 Term 本身就是准确画面，快照直接可用。而反过来，白打一次
/// SIGWINCH 对不切备用屏的 CLI（整段对话都躺在 scrollback 里的那类）代价极大——
/// 它会把整段对话重排重印一遍，手机上就是「一切回来又滚很久」。
fn begin_remote_viewport(sess: &Session, cols: u16, rows: u16, cell_w: u16, cell_h: u16) -> bool {
    let kick = {
        let mut ctl = sess.ctl.lock().unwrap();
        ctl.remote_viewports = ctl.remote_viewports.saturating_add(1);
        // 续租：宽限期内切回来的就是同一个消费者。
        ctl.remote_grace = 0;
        let changed = ctl.cols != cols
            || ctl.rows != rows
            || (cell_w > 0 && ctl.cell_w != cell_w)
            || (cell_h > 0 && ctl.cell_h != cell_h);
        // 别抹掉别人挂好的 jolt（比如从 handoff 恢复的会话，靠它逼进程自绘）。
        let kick = ctl.jolt || changed;
        ctl.jolt = kick;
        kick
    };
    resize_session_remote(sess, cols, rows, cell_w, cell_h);
    kick
}

/// 宽限期长度。
///
/// 这不是「等他切回来」的那几秒，而是「在有人真的回到桌面动这个终端之前，尺寸就先
/// 留在手机这边」。手机侧退出会话页、切后台、锁屏都是常态，而每还一次尺寸、桌面就
/// 立刻按自己的视口抢回去，手机下次进来又抢一次——一来一回就是两次 SIGWINCH，不切
/// 备用屏的 CLI 每次都把整段对话重排重印一遍（实测一次改尺寸 = 560KB / 8 秒的输出）。
/// 所以宽限给得很长，真正的归还信号是「桌面侧有人敲键盘」（见
/// `cancel_remote_viewport_grace`）。
const REMOTE_VIEWPORT_GRACE: Duration = Duration::from_secs(300);

static REMOTE_GRACE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 最后一个远程视口断开：先进入宽限，不归还尺寸。返回本次宽限的代号，交给调用方
/// 在 [`REMOTE_VIEWPORT_GRACE`] 之后调 [`expire_remote_viewport`]；返回 None 表示
/// 还有别的远程视口在，什么都不用做。
fn pause_remote_viewport(sess: &Session) -> Option<u64> {
    let mut ctl = sess.ctl.lock().unwrap();
    ctl.remote_viewports = ctl.remote_viewports.saturating_sub(1);
    if ctl.remote_viewports != 0 {
        return None;
    }
    let seq = REMOTE_GRACE_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    ctl.remote_grace = seq;
    Some(seq)
}

/// 宽限到期。代号对不上（期间被续租或被取消）就什么都不做。
fn expire_remote_viewport(sess: &Session, seq: u64) {
    {
        let mut ctl = sess.ctl.lock().unwrap();
        if ctl.remote_grace != seq {
            return;
        }
        ctl.remote_grace = 0;
        if ctl.remote_viewports != 0 {
            return;
        }
    }
    publish_remote_geometry_release(sess);
}

/// 桌面侧有人动这个会话（敲键盘）：人已经回到 PC 了，宽限立即作废，尺寸还给桌面。
fn cancel_remote_viewport_grace(sess: &Session) {
    {
        let mut ctl = sess.ctl.lock().unwrap();
        if ctl.remote_grace == 0 {
            return;
        }
        ctl.remote_grace = 0;
        if ctl.remote_viewports != 0 {
            return;
        }
    }
    publish_remote_geometry_release(sess);
}

fn end_remote_viewport(sess: &Session) {
    {
        let mut ctl = sess.ctl.lock().unwrap();
        ctl.remote_viewports = ctl.remote_viewports.saturating_sub(1);
        ctl.remote_grace = 0;
        if ctl.remote_viewports != 0 {
            return;
        }
    }
    publish_remote_geometry_release(sess);
}

/// 告诉桌面「尺寸归你了」：下发一条 remote_controlled=false 的几何标记。
fn publish_remote_geometry_release(sess: &Session) {
    let _output_gate = sess.output_gate.lock().unwrap();
    let marker = {
        let geometry = {
            let ctl = sess.ctl.lock().unwrap();
            TerminalGeometryOsc {
                cols: ctl.cols,
                rows: ctl.rows,
                cell_width: ctl.cell_w,
                cell_height: ctl.cell_h,
                remote_controlled: false,
            }
        };
        // Keep term ordering while constructing the marker, then release all
        // session guards before writing to potentially slow clients.
        let Ok(_term) = sess.term.lock() else { return };
        terminal_geometry_osc(&sess.geometry_token, geometry)
    };
    dispatch_session_outputs_under_gate(sess, &marker, "<remote-viewport>", true, false);
}

/// 开/关 fd 的 CLOEXEC 标志。平时所有 fd 都应带 CLOEXEC（不泄漏给 spawn 出的 shell）；
/// 仅在 exec 交接前对要带过去的 fd 关掉。
#[cfg(unix)]
fn set_cloexec(fd: RawFd, on: bool) -> std::io::Result<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let flags = if on {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        if libc::fcntl(fd, libc::F_SETFD, flags) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// dup 一个 fd 并包成 File。dup 出的新 fd 默认**不带** CLOEXEC，这里立即补上——
/// 否则它会泄漏进之后 spawn 的每个 shell（占着 PTY master 不放，会话杀不干净）。
#[cfg(unix)]
fn dup_file(fd: RawFd) -> anyhow::Result<std::fs::File> {
    let d = unsafe { libc::dup(fd) };
    anyhow::ensure!(d >= 0, "dup({fd}) 失败");
    set_cloexec(d, true)?;
    Ok(unsafe { std::fs::File::from_raw_fd(d) })
}

#[cfg(unix)]
fn set_fd_nonblocking(fd: RawFd, enabled: bool) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let next = if enabled {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, next) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 会话输出端：交互 attachment + watch 旁观者。
/// 「快照→接管」与实时转发共用 output_gate，严格串行；每个 attachment 自己拥有
/// 一条写队列和写线程，PTY 泵绝不直接等待 GUI socket 变为可写。
/// 画面恢复只靠常驻 Term 的 keyframe，**不再**维护环形字节缓冲。
struct Out {
    /// `open` 连接：每个桌面渲染层各占一路，可同时输入并接收同一份 PTY 输出。
    clients: Vec<OutputAttachment>,
    /// `watch` 连接：只读旁观，可多个并存。
    watchers: Vec<OutputAttachment>,
}

/// 向 PTY master 写入用户输入。`Ctl` 只在复制 fd 时短暂持有，真正的系统调用在锁外
/// 完成；fd 临时切成 non-blocking，再用 `poll` 等待可写，因而即使子进程停止消费
/// 输入队列，也不会把会话控制锁永久占住。写入期间用 `input_gate` 保证多个输入源
/// 不交错，且让 resize/升级与输入保持同一顺序。
pub(crate) fn write_session_input(sess: &Session, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        write_session_input_unix(sess, bytes)
    }
    #[cfg(windows)]
    {
        write_session_input_windows(sess, bytes)
    }
}

#[cfg(windows)]
struct PtyInputWrite {
    bytes: Vec<u8>,
    done: std::sync::mpsc::Sender<std::io::Result<()>>,
    /// 写线程开写前和调用方超时时各抢一次，谁先到算谁。取消了的不再写，
    /// 超时报给调用方的「没写进去」才是真的。
    state: Arc<std::sync::atomic::AtomicU8>,
}

#[cfg(windows)]
const PTY_INPUT_QUEUED: u8 = 0;
#[cfg(windows)]
const PTY_INPUT_STARTED: u8 = 1;
#[cfg(windows)]
const PTY_INPUT_CANCELLED: u8 = 2;

/// 一个会话一条写线程，队列最多 8 笔。ConPTY 缓冲满时线程停在 `write_all`，
/// 后来的按键进队列，满了就返回超时，不再按每次输入起线程。
#[cfg(windows)]
fn spawn_pty_input_thread(
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
) -> std::sync::mpsc::SyncSender<PtyInputWrite> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<PtyInputWrite>(8);
    let _ = thread::Builder::new()
        .name("smelt-pty-input".to_string())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                if job
                    .state
                    .compare_exchange(
                        PTY_INPUT_QUEUED,
                        PTY_INPUT_STARTED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    continue;
                }
                let result = writer
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .write_all(&job.bytes);
                let _ = job.done.send(result);
            }
        });
    tx
}

#[cfg(windows)]
fn write_session_input_windows(sess: &Session, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let _input_gate = sess.input_gate.lock().unwrap();
    let input_tx = {
        let ctl = sess.ctl.lock().unwrap();
        ctl.input_tx.clone()
    };
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let state = Arc::new(std::sync::atomic::AtomicU8::new(PTY_INPUT_QUEUED));
    if input_tx
        .try_send(PtyInputWrite {
            bytes: bytes.to_vec(),
            done: done_tx,
            state: Arc::clone(&state),
        })
        .is_err()
    {
        return Err(std::io::Error::new(
            ErrorKind::TimedOut,
            "PTY input write timed out",
        ));
    }
    match done_rx.recv_timeout(PTY_WRITE_TIMEOUT) {
        Ok(result) => result,
        Err(_) => {
            // 还在排队的撤下来，之后不会再写进去。已经开写的和 Unix 一样，可能写进去了一部分。
            let _ = state.compare_exchange(
                PTY_INPUT_QUEUED,
                PTY_INPUT_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "PTY input write timed out",
            ))
        }
    }
}

#[cfg(unix)]
fn write_session_input_unix(sess: &Session, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let _input_gate = sess.input_gate.lock().unwrap();
    let mut master = {
        let ctl = sess.ctl.lock().unwrap();
        ctl.master.try_clone()?
    };
    set_fd_nonblocking(master.as_raw_fd(), true)?;
    let fd = master.as_raw_fd();
    let deadline = Instant::now() + PTY_WRITE_TIMEOUT;
    let mut offset = 0usize;

    loop {
        if offset == bytes.len() {
            break Ok(());
        }
        match master.write(&bytes[offset..]) {
            Ok(0) => {
                break Err(std::io::Error::new(
                    ErrorKind::WriteZero,
                    "PTY master accepted zero bytes",
                ));
            }
            Ok(written) => {
                offset += written;
                continue;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) => break Err(error),
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "PTY input write timed out",
            ));
        }
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            break Err(error);
        }
        if polled == 0 {
            break Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "PTY input write timed out",
            ));
        }
        if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            break Err(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "PTY master is closed",
            ));
        }
    }
}

fn dispatch_session_outputs_under_gate(
    sess: &Session,
    bytes: &[u8],
    id: &str,
    send_clients: bool,
    send_watchers: bool,
) {
    let (clients, watchers) = {
        let mut out = sess.out.lock().unwrap();
        (
            std::mem::take(&mut out.clients),
            std::mem::take(&mut out.watchers),
        )
    };
    let live_clients = if send_clients {
        enqueue_session_streams(clients, bytes, id, "attachment")
    } else {
        clients
    };
    let live_watchers = if send_watchers {
        enqueue_session_streams(watchers, bytes, id, "watcher")
    } else {
        watchers
    };

    // 所有会修改连接集合的路径都在 output_gate 下运行，因此这里不会和 attach/
    // disconnect/kill 交叉覆盖 Vec；`out` 锁本身只覆盖这次 swap/merge 的短临界区。
    let mut out = sess.out.lock().unwrap();
    out.clients.extend(live_clients);
    out.watchers.extend(live_watchers);
}

/// 交互连接的关键帧：几何标记 + 终端重置（RIS）+ 画面快照。客户端不管原来处在
/// 什么状态，解析完都和刚附着时一样，所以它可以随时插进实时流，取代跳过的输出。
/// 调用方持有 output_gate；这里依次拿 ctl、term，和 attach 的加锁顺序一致。
fn terminal_keyframe(sess: &Session, need: KeyframeNeed) -> Vec<u8> {
    let ctl = sess.ctl.lock().unwrap();
    let term = sess.term.lock().unwrap();
    let mut keyframe = terminal_geometry_osc(
        &sess.geometry_token,
        TerminalGeometryOsc {
            cols: ctl.cols,
            rows: ctl.rows,
            cell_width: ctl.cell_w,
            cell_height: ctl.cell_h,
            remote_controlled: ctl.remote_viewports > 0,
        },
    );
    keyframe.extend_from_slice(b"\x1bc");
    keyframe.extend(match need {
        KeyframeNeed::Screen => snapshot_ansi_screen(&term),
        KeyframeNeed::Full => snapshot_ansi(&term, None),
    });
    keyframe
}

/// 客户端确认处理到第 `consumed` 个字节。合并中的连接追上来了，就补一帧关键帧。
fn acknowledge_terminal_output(sess: &Session, fd: ConnId, consumed: u64) {
    let _output_gate = sess.output_gate.lock().unwrap();
    let need = {
        let mut out = sess.out.lock().unwrap();
        let Some(client) = out.clients.iter_mut().find(|client| client.fd == fd) else {
            return;
        };
        client.acknowledge(consumed)
    };
    let Some(need) = need else {
        return;
    };
    // 生成快照要拿 ctl/term，不能在 `out` 锁里拿；output_gate 保证这期间没有新输出插进来。
    let keyframe = terminal_keyframe(sess, need);
    let mut out = sess.out.lock().unwrap();
    let Some(pos) = out.clients.iter().position(|client| client.fd == fd) else {
        return;
    };
    if !out.clients[pos].send_keyframe(&keyframe, need) {
        out.clients.swap_remove(pos).close();
    }
}

/// 守护侧常驻终端状态机尺寸（实现 alacritty Dimensions）。
#[derive(Clone, Copy)]
struct DaemonTermSize {
    rows: usize,
    cols: usize,
}

impl Dimensions for DaemonTermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn daemon_term_config() -> TermConfig {
    TermConfig {
        scrolling_history: TERM_HISTORY,
        ..smelt_core::term_config::emulation_config()
    }
}

fn new_daemon_term<T: EventListener>(rows: u16, cols: u16, listener: T) -> Term<T> {
    let size = DaemonTermSize {
        rows: rows.max(1) as usize,
        cols: cols.max(1) as usize,
    };
    Term::new(daemon_term_config(), &size, listener)
}

struct Session {
    /// 每次 runtime 实例化都分配新的代际。它不出现在对外协议中，只用于让状态总线
    /// 拒绝已删除实例的迟到事件。
    instance: u64,
    /// Per-session capability for daemon-only geometry control sequences.
    /// The PTY child never receives this token, so terminal output cannot
    /// forge a desktop resize or remote-viewport lock.
    geometry_token: String,
    child: Arc<TerminalChild>,
    ctl: Mutex<Ctl>,
    /// 串行化同一 PTY 的输入，但与 `ctl` 分离；慢写只会堵住这个会话的后续输入。
    input_gate: Mutex<()>,
    out: Mutex<Out>,
    /// 输出顺序锁。与 `out` 分离，禁止慢 socket 写操作阻塞连接集合和会话管理。
    output_gate: Mutex<()>,
    /// 守护 Term 收到的 OSC 颜色查询应答；由 PTY 泵在网格锁释放后串行写回 child。
    color_replies: PendingColorReplies,
    /// 常驻网格：PTY 输出持续 advance；attach 时序列化成 ANSI 快照。挂的是
    /// `StateListener`（不再是 `VoidListener`）——守护自己也要看得见 Title/Bell。
    term: Mutex<Term<StateListener>>,
    /// 结构化状态（见 SessionState）。跟 `term` 的监听器共用同一个 Arc，
    /// `state` op（hook 直写）和 `subscribe` 的转发都读/改这一份。
    state: Arc<Mutex<SessionState>>,
}

type Sessions = Arc<TerminalRegistry<Session>>;

fn new_sessions() -> Sessions {
    Arc::new(TerminalRegistry::new())
}

/// 卸掉继承来的信号掩码。`posix_spawn` 复制的是调用线程的掩码，不是进程默认值。
#[cfg(unix)]
fn clear_inherited_signal_mask() {
    unsafe {
        let mut empty = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
fn clear_inherited_signal_mask() {}

/// 停机自管道的写端。信号处理器只许 `write` 这一个 fd，不能动掩码、不能分配。
#[cfg(unix)]
static SHUTDOWN_SIGNAL_WRITE: AtomicI32 = AtomicI32::new(-1);

/// `SIGTERM` / `SIGINT` 共用。把信号号写进自管道，立刻返回。
#[cfg(unix)]
extern "C" fn shutdown_signal_handler(signal: libc::c_int) {
    let fd = SHUTDOWN_SIGNAL_WRITE.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte = [signal as u8];
    unsafe {
        libc::write(fd, byte.as_ptr().cast(), 1);
    }
}

/// 装上 `sigaction`（`SA_RESTART`），不调用 `pthread_sigmask` / `sigprocmask`。
/// 写端 `O_NONBLOCK | O_CLOEXEC`，读端只 `O_CLOEXEC`，子进程既不继承掩码阻塞，也不继承管道。
#[cfg(unix)]
fn install_shutdown_signal_handlers() -> std::io::Result<RawFd> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if let Err(error) = (|| {
        set_cloexec(fds[0], true)?;
        set_cloexec(fds[1], true)?;
        set_fd_nonblocking(fds[1], true)?;
        Ok(())
    })() {
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        return Err(error);
    }
    let previous = SHUTDOWN_SIGNAL_WRITE.swap(fds[1], Ordering::Relaxed);
    if previous >= 0 {
        unsafe { libc::close(previous) };
    }
    if let Err(error) = arm_shutdown_sigactions() {
        reset_shutdown_signals_to_default();
        unsafe { libc::close(fds[0]) };
        return Err(error);
    }
    Ok(fds[0])
}

#[cfg(unix)]
fn arm_shutdown_sigactions() -> std::io::Result<()> {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = shutdown_signal_handler as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 读端阻塞，直到处理器写进一个字节。`SA_RESTART` 之下仍对其他来源的 `EINTR` 重试。
#[cfg(unix)]
fn read_shutdown_signal(fd: RawFd) -> Option<u8> {
    let mut buf = [0u8; 1];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), 1) };
        if n > 0 {
            return Some(buf[0]);
        }
        if n == 0 {
            return None;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return None;
        }
    }
}

/// 接住 SIGTERM / SIGINT，转到和 shutdown op 同一条停机。信号掩码保持不动。
#[cfg(unix)]
fn install_shutdown_signals() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let read = match install_shutdown_signal_handlers() {
            Ok(fd) => fd,
            Err(error) => {
                dlog(&format!("安装停机信号失败：{error}"));
                return;
            }
        };
        let spawned = thread::Builder::new()
            .name("smelt-shutdown-signals".into())
            .spawn(move || {
                if let Some(signal) = read_shutdown_signal(read) {
                    dlog(&format!("收到信号 {signal}，开始优雅停机"));
                    protocol::graceful_shutdown(None, None);
                }
            });
        if spawned.is_err() {
            dlog("停机信号线程没有起来，恢复默认终止动作");
            reset_shutdown_signals_to_default();
        }
    });
}

#[cfg(unix)]
fn reset_shutdown_signals_to_default() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        let write = SHUTDOWN_SIGNAL_WRITE.swap(-1, Ordering::Relaxed);
        if write >= 0 {
            libc::close(write);
        }
    }
}

#[cfg(not(unix))]
fn install_shutdown_signals() {}

#[cfg(all(test, unix))]
#[test]
fn startup_clears_inherited_sigchld_mask() {
    unsafe {
        let mut blocked = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGCHLD);
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut()),
            0
        );
        let mut current = std::mem::zeroed();
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &mut current),
            0
        );
        assert_eq!(libc::sigismember(&current, libc::SIGCHLD), 1);
    }
    clear_inherited_signal_mask();
    unsafe {
        let mut current = std::mem::zeroed();
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &mut current),
            0
        );
        assert_eq!(libc::sigismember(&current, libc::SIGCHLD), 0);
    }
}

#[cfg(all(test, unix))]
fn signal_is_blocked(signal: libc::c_int) -> bool {
    unsafe {
        let mut current = std::mem::zeroed();
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &mut current),
            0
        );
        libc::sigismember(&current, signal) == 1
    }
}

/// 子进程探针。正常跑时断言 SIGINT/SIGTERM 都没被挡住；
/// `SMELTD_EXPECT_SIGTERM_BLOCKED=1` 时断言只挡住了 SIGTERM（证明探针看得见继承来的掩码）。
#[cfg(all(test, unix))]
#[test]
fn shutdown_signal_mask_probe() {
    let sigint = signal_is_blocked(libc::SIGINT);
    let sigterm = signal_is_blocked(libc::SIGTERM);
    if std::env::var_os("SMELTD_EXPECT_SIGTERM_BLOCKED").is_some() {
        assert!(sigterm, "child should inherit a blocked SIGTERM");
        assert!(!sigint, "SIGINT was blocked along with SIGTERM");
        return;
    }
    assert!(!sigint, "SIGINT is blocked");
    assert!(!sigterm, "SIGTERM is blocked");
}

#[cfg(all(test, unix))]
fn run_shutdown_mask_probe(expect_sigterm_blocked: bool) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["shutdown_signal_mask_probe", "--exact"]);
    if expect_sigterm_blocked {
        command.env("SMELTD_EXPECT_SIGTERM_BLOCKED", "1");
    } else {
        command.env_remove("SMELTD_EXPECT_SIGTERM_BLOCKED");
    }
    command.output().unwrap()
}

#[cfg(all(test, unix))]
struct RestoredShutdownSignals {
    term: libc::sigaction,
    int: libc::sigaction,
}

#[cfg(all(test, unix))]
impl Drop for RestoredShutdownSignals {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGTERM, &self.term, std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &self.int, std::ptr::null_mut());
            let write = SHUTDOWN_SIGNAL_WRITE.swap(-1, Ordering::Relaxed);
            if write >= 0 {
                libc::close(write);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[test]
fn shutdown_handlers_leave_child_signals_unblocked_and_wake_listener() {
    // 先挡住再拉子进程，确认探针真的能看见继承来的掩码，而不是永远报告「没挡住」。
    struct UnblockSigterm;
    impl Drop for UnblockSigterm {
        fn drop(&mut self) {
            unsafe {
                let mut set = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGTERM);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            }
        }
    }
    let unblock = UnblockSigterm;
    unsafe {
        let mut set = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
            0
        );
    }
    let inherited = run_shutdown_mask_probe(true);
    drop(unblock);
    assert!(
        inherited.status.success(),
        "probe did not observe inherited SIGTERM block\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&inherited.stdout),
        String::from_utf8_lossy(&inherited.stderr)
    );

    let restore = unsafe {
        let mut term = std::mem::zeroed();
        let mut int = std::mem::zeroed();
        assert_eq!(
            libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut term),
            0
        );
        assert_eq!(libc::sigaction(libc::SIGINT, std::ptr::null(), &mut int), 0);
        RestoredShutdownSignals { term, int }
    };
    let read_fd = install_shutdown_signal_handlers().expect("install shutdown handlers");
    assert!(!signal_is_blocked(libc::SIGINT));
    assert!(!signal_is_blocked(libc::SIGTERM));

    let cleared = run_shutdown_mask_probe(false);
    assert!(
        cleared.status.success(),
        "child inherited a blocked shutdown signal\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&cleared.stdout),
        String::from_utf8_lossy(&cleared.stderr)
    );

    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let got = read_shutdown_signal(read_fd);
        unsafe { libc::close(read_fd) };
        let _ = tx.send(got);
    });
    unsafe { libc::raise(libc::SIGINT) };
    let woke = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("shutdown listener did not wake");
    assert_eq!(woke, Some(libc::SIGINT as u8));
    drop(restore);
}

fn main() {
    // GUI 从 libdispatch 工作线程 posix_spawn 本进程时，会把那套「屏蔽 SIGCHLD」
    // 的线程掩码遗传进来。async-process 靠 SIGCHLD 回收子进程；掩码不清掉，
    // Pi 退出后 status() 永远等不到通知，会话就停在「正在启动」。
    // 必须赶在任何 thread::spawn / 再拉起 session host 之前做，新建线程才会复制空掩码。
    clear_inherited_signal_mask();
    // Grok/CI 会给进程打 NO_COLOR=1。必须在任何线程/子进程之前卸掉，否则
    // 交互式 PTY（agy 等）会继承关色开关，整屏只剩默认灰白。
    smelt_core::tty_color::clear_process();
    // 独立 ACP 会话宿主只跑一条私有控制连接，不能初始化 SQLite、绑定全局
    // smeltd.sock 或启动菜单/远程服务。尽量早分流也避免每个会话宿主各起一套
    // app_log tee 后台线程。
    // 会话宿主和主进程都要钉住自己映射的那份文件。钉晚了，启动名若已被拨走，
    // 再去解析 `current_exe()` 就会拿到下一份映像。
    pin_running_daemon_image();
    // 会话宿主的 `--acp-session-host` 是内部参数，必须先于子命令分流认出来，
    // 否则会被当成未知子命令退出，对话会话起来就挂。
    if acp_runtime_host::is_session_host_process() {
        acp_runtime_host::run_session_host();
        return;
    }
    // `smeltd install` / `status` 这类子命令跑完就退出，不起守护。
    if let Some(code) = service::maybe_run(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    exit_with_test_owner();
    smelt_core::sqlite_state::enable_sqlite_state();
    // 全 app 通用运行日志（~/.smelt/app.log，默认开、大小有上限，见 app_log 模块）：
    // 先装 panic hook，再记一条启动事件——守护本身没有终端可看，崩溃/异常全靠这份
    // 日志留痕。跟已有的 daemon.log（只记交接/网络这类守护自身生命周期事件）互补。
    smelt_core::app_log::install_panic_hook("smeltd");
    smelt_core::app_log::tee_stderr("smeltd");
    smelt_core::app_log::info("smeltd", "守护启动");
    // 启动瞬间打一行自身内核身份（team/signing-id 进程生命期内恒定）。
    // 鉴权排障锚点：磁盘被换后框架查询会撒谎（-67034），内核不会。
    #[cfg(target_os = "macos")]
    protocol::log_self_kernel_identity();
    // 子进程（ACP agent 等）用的是低层 spawn_process 逃生口，SDK 不支持单独
    // 指定子进程 cwd——它们一律继承本进程的 cwd。本进程的 cwd 又是从
    // launchd/Finder 或上一次 `cd` 到的目录继承来的，可能是个已被删除/挪进
    // 废纸篓的目录（比如某次在临时 worktree 里启动过守护，之后那个目录被
    // 清理掉）。cwd 指向不存在的路径时，很多用 Node 写的 CLI（含 Copilot
    // CLI）在启动阶段调 `process.cwd()` 直接抛异常退出——外部表现就是"所有
    // ACP 会话的 initialize 都失败，transport closed"，坑了很久才排出来。
    // 钉死到 HOME：一个几乎不可能被删除、稳定存在的目录，一劳永逸避免这个坑。
    if let Some(home) = dirs::home_dir()
        && let Err(e) = std::env::set_current_dir(&home)
    {
        smelt_core::app_log::error(
            "smeltd",
            &format!("启动时 cwd 校正失败（home={}）：{e}", home.display()),
        );
    }
    // 尽早提 fd 上限：晚了的话，前面已经开的 fd 会先一步顶到旧上限。
    smelt_core::fd_limit::raise_fd_limit();
    // Pi 没有全局 hook 配置；把随 daemon 编译的进程级扩展同步到稳定路径，后续
    // 快捷终端通过 --extension 注入。失败只降级状态精度，不能阻止 daemon 启动。
    if let Err(error) = sync_pi_status_extension() {
        smelt_core::app_log::error("smeltd", &format!("同步 Pi 状态扩展失败：{error}"));
    }
    // 钉住启动时刻：晚一步取到的就是「首次有人问 version」的时间，不是启动时间。
    started_at();
    let env_daemon_fingerprint = std::env::var("SMELTD_DAEMON_FINGERPRINT").ok();
    #[cfg(windows)]
    let predecessor_pid = std::env::var(PREDECESSOR_PID_ENV)
        .ok()
        .and_then(|pid| pid.parse::<i32>().ok());
    // Edition 2024：`remove_var` 标为 unsafe（多线程改 env 非同步）。此时只有不访问
    // 环境变量的 stderr tee 读线程，尚未启动任何会并发读环境的业务线程。
    unsafe {
        std::env::remove_var("SMELTD_HANDOFF");
        std::env::remove_var("SMELTD_HANDOFF_SOCK");
        std::env::remove_var("SMELTD_DAEMON_FINGERPRINT");
        #[cfg(windows)]
        std::env::remove_var(PREDECESSOR_PID_ENV);
    }
    // 进程指纹启动时钉死：env（GUI 拉起时对 spawn 目标的哈希）优先，否则对
    // current_exe 文件哈希一次。此后进程生命期内不再重哈希磁盘。
    let daemon_fingerprint = pinned_daemon_fingerprint(env_daemon_fingerprint);

    let path = sock_path();
    // EventHub 的实时连接列表不跨进程保留；每个进程从空列表开始。
    let event_hub = match event_hub::DaemonEventHub::new() {
        Ok(event_hub) => event_hub,
        Err(error) => {
            eprintln!("[event-hub] 初始化失败，守护退出：{error}");
            smelt_core::app_log::error("smeltd", &format!("EventHub 初始化失败：{error}"));
            return;
        }
    };
    // 远程目录是 daemon 独占的持久化状态，新进程重新打开
    // 即可。读取异常时 fail-closed，绝不把损坏文档当成空目录发布给 GUI。
    let remote_sessions = new_remote_sessions();
    let workspace_menu = new_workspace_menu();
    // 升级的接班进程：前任回执后才退出，退出前监听还在它手里。等它退了再绑。
    #[cfg(windows)]
    if let Some(pid) = predecessor_pid
        && !smelt_core::process_wait::wait_until_exit(pid, Duration::from_secs(30))
    {
        dlog(&format!("升级接班：前任 {pid} 30 秒内没退出，照常尝试绑定"));
    }
    let listener = match bind_fresh_daemon(&path) {
        Ok(Some(l)) => l,
        Ok(None) => return,
        Err(e) => {
            // 曾经是静默 return：守护无声消失、sock 残留，外面完全查不到
            // 死因（排障时被坑过——必须留痕）。
            dlog(&format!("bind {} 失败，守护退出：{e}", path.display()));
            return;
        }
    };
    // socket 仅本用户可读写。权限收紧失败时不能留下一个仍接受连接的 daemon。
    if let Err(error) = secure_daemon_socket(&path) {
        dlog(&format!(
            "设置 {} 权限失败，守护退出：{error}",
            path.display()
        ));
        drop(listener);
        let _ = std::fs::remove_file(&path);
        std::process::exit(1);
    }
    // 拿到监听之后才接住 SIGTERM/SIGINT：停机会摘掉 socket 路径，还没拿到监听的进程
    // 摘掉的是正在服务的那个守护的。在这之前收到信号按默认动作退出，本来也没有要收尾的。
    // 处理器只往自管道写一个字节，不改信号掩码，子进程仍能收到 Ctrl+C 和 SIGTERM。
    install_shutdown_signals();
    // 崩溃恢复：落盘有内容就说明上次非正常退出（正常 shutdown 会清空）。
    // 灌进身份目录，runtime=false，保留最后相位。
    let directory = SessionDirectory::restore_from_disk();
    let recovered = directory.snapshot().len();
    if recovered > 0 {
        dlog(&format!("崩溃恢复：{recovered} 个会话恢复为已断连状态"));
    }
    *SESSION_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) = Some(directory);
    let sessions = new_sessions();
    let acp_sessions = new_acp_sessions();

    let automations = new_automation_store();

    reconcile_remote_catalog_runtime(&remote_sessions, &sessions, &acp_sessions, &event_hub);
    event_hub.synchronize_projection(collect_subscription_snapshot(
        &sessions,
        &acp_sessions,
        &remote_sessions,
        &workspace_menu,
        automations.lock().unwrap().snapshot(),
    ));

    automation_runtime::spawn(
        Arc::clone(&automations),
        Arc::clone(&sessions),
        Arc::clone(&acp_sessions),
        Arc::clone(&remote_sessions),
        Arc::clone(&event_hub),
    );
    webhook::spawn(Arc::clone(&automations), Arc::clone(&event_hub));

    run_serve_loop(
        listener,
        sessions,
        acp_sessions,
        remote_sessions,
        workspace_menu,
        automations,
        event_hub,
        daemon_fingerprint,
    );
}

/// 测试拉起的守护跟着测试进程退出（见 [`smelt_paths::TEST_OWNER_PID_ENV`]）。
///
/// 守护自成进程组，测试进程退出时它收不到信号；不跟着退就在沙箱里一直跑，开着
/// 库和上百 MB 的 debug 映像，曾攒出几百个。变量留在环境里，重启出去的
/// 新进程继承到它，同样要跟着退。
fn exit_with_test_owner() {
    let Some(owner) = smelt_paths::test_owner_pid() else {
        return;
    };
    thread::spawn(move || {
        wait_for_test_owner_exit(owner);
        dlog(&format!("测试进程 {owner} 已退出，守护跟着退出"));
        cleanup_sidecar_services();
        std::process::exit(0);
    });
}

/// 没有截止时间：一次等一小时，没等到就接着等。
fn wait_for_test_owner_exit(owner: i32) {
    while !smelt_core::process_wait::wait_until_exit(owner, Duration::from_secs(3600)) {}
}

/// headless 自升级：没人看时，把 `smeltd.pending` 指向的新映像应用到运行中的守护。
/// 每 60s 看一次（与 GUI 的 watch 同频，首轮延迟 60s，把先手让给 GUI）。
/// 门控：
/// - 指纹未知（None）不升：证明不了自己旧，别乱动。
/// - 有活着的观看连接就不升：GUI 在，由它提示用户并重启。
/// - 不等待 agent 空闲。upgrade 会停掉本进程，进行中的回合中断，历史在库里。
fn spawn_headless_self_upgrade(daemon_fingerprint: Option<String>) {
    let Some(pinned) = daemon_fingerprint else {
        return;
    };
    std::thread::Builder::new()
        .name("smelt-headless-self-upgrade".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(60));
                // 有人在看就让 GUI 提示并重启，避免无界面地把对话打断。
                if protocol::viewer_is_present() {
                    continue;
                }
                let Ok(current) = daemon_executable_path() else {
                    continue;
                };
                let Some(target) = staged_successor_executable(&current) else {
                    continue;
                };
                let disk = match smelt_core::daemon_image::content_fingerprint(&target) {
                    Ok(fingerprint) => fingerprint,
                    Err(_) => continue,
                };
                if disk == pinned {
                    continue;
                }
                dlog("headless 自升级：已 stage 新映像且没有观看连接，重启到新映像");
                match request_self_upgrade(&target) {
                    SelfUpgradeOutcome::Upgraded => return,
                    SelfUpgradeOutcome::Failed(reason) => {
                        dlog(&format!("headless 自升级未完成，60s 后重试：{reason}"));
                    }
                }
            }
        })
        .ok();
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SelfUpgradeOutcome {
    Upgraded,
    Failed(String),
}

/// 对自己 socket 发一次 upgrade op（纯 IO，可单测判定；socket 交互在调用方）。
fn classify_self_upgrade_reply(reply: &str) -> SelfUpgradeOutcome {
    let value: serde_json::Value = match serde_json::from_str(reply.trim()) {
        Ok(value) => value,
        Err(_) => return SelfUpgradeOutcome::Failed(format!("回包非 JSON：{reply:?}")),
    };
    if value["ok"].as_bool() == Some(true) {
        return SelfUpgradeOutcome::Upgraded;
    }
    let reason = value["err"]
        .as_str()
        .or_else(|| value["error"].as_str())
        .unwrap_or("未知")
        .to_string();
    SelfUpgradeOutcome::Failed(reason)
}

pub(crate) fn request_self_upgrade(target: &std::path::Path) -> SelfUpgradeOutcome {
    let sock = match UnixStream::connect(sock_path()) {
        Ok(sock) => sock,
        Err(error) => return SelfUpgradeOutcome::Failed(format!("连自己失败：{error}")),
    };
    let _ = sock.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
    let mut sock = sock;
    if let Err(error) = smelt_runtime_client::protocol::write_json_line(
        &mut sock,
        &serde_json::json!({ "op": "upgrade", "exe": target.to_string_lossy() }),
    ) {
        return SelfUpgradeOutcome::Failed(format!("发 upgrade 失败：{error}"));
    }
    let mut reply = String::new();
    if let Err(error) = BufReader::new(&sock).read_line(&mut reply) {
        return SelfUpgradeOutcome::Failed(format!("读回包失败：{error}"));
    }
    classify_self_upgrade_reply(&reply)
}

/// 服务主循环：远端自愈→accept（+macOS 菜单栏主线程编排）。
#[allow(clippy::too_many_arguments)]
fn run_serve_loop(
    listener: UnixListener,
    sessions: Sessions,
    acp_sessions: AcpSessions,
    remote_sessions: RemoteSessions,
    workspace_menu: WorkspaceMenuStore,
    automations: AutomationStore,
    event_hub: EventHubHandle,
    daemon_fingerprint: Option<String>,
) {
    let exe_mtime = exe_mtime_secs();
    // 第一次启动就把运行时身份落盘，不等第一个客户端来问；失败只记日志，describe 时再试。
    let _ = runtime_identity::runtime_id();
    thread::spawn(smelt_core::managed_runtime::retire_pi_source_generations);
    thread::spawn(smelt_core::control_skill::remove_legacy_copy);
    #[cfg(target_os = "linux")]
    thread::spawn(runtime_upgrade::ensure_smelt_pi);
    // 远程访问每次进程启动都是空的，按落盘配置重新拉起。
    // 菜单栏 quit / 任何路径 cleanup 都要够得着它。
    let remote_state = new_remote_state();
    register_lifecycle(Arc::clone(&remote_state));

    // exec 活下来的同名断言没有 Rust 对象可 Drop，必须在自愈拉起之前清掉。
    #[cfg(target_os = "macos")]
    SystemSleepAssertion::release_orphans();

    let server = ServerShared {
        sessions,
        acp_sessions,
        remote_sessions,
        workspace_menu,
        automations,
        exe_mtime,
        daemon_fingerprint,
        event_hub,
    };
    // 先登记再拉起远程访问：iroh 流一进来就要从这里取服务状态。
    let _ = SERVER.set(server.clone());
    // 自己起线程，不阻塞 accept 循环。
    autostart_remote_from_config(&remote_state);

    // headless 空闲自升级：没有观看连接时磁盘新了没人触发 upgrade，这里兜底。
    // GUI 连着时它 60s 探一次、有空闲门控，让它拥有升级时机（见函数注释）。
    spawn_headless_self_upgrade(server.daemon_fingerprint.clone());

    // 闲置会话挂起（见 acp_idle）。冷启动和升级交接都走到这里；会话宿主进程不走。
    acp_idle::spawn_acp_idle_reaper(
        Arc::clone(&server.acp_sessions),
        Arc::clone(&server.remote_sessions),
    );

    // thread-per-connection 的 accept 主循环。抽成闭包，好让主线程在 macOS 上腾出来
    // 跑菜单栏 runloop——AppKit 铁律：NSApplication/NSStatusItem 只能在主线程摸。
    #[cfg(windows)]
    if let Ok(pipe_name) = smelt_sys::named_pipe::daemon_pipe_path(&sock_path()) {
        client_conn::remember_daemon_pipe(pipe_name.clone());
        let pipe_server = server.clone();
        thread::Builder::new()
            .name("smelt-pipe-accept".into())
            .spawn(move || {
                let listener = match smelt_sys::named_pipe::PipeListener::bind(pipe_name) {
                    Ok(listener) => listener,
                    Err(error) => {
                        dlog(&format!("命名管道监听失败，Bun 连不上守护：{error}"));
                        return;
                    }
                };
                loop {
                    match listener.accept() {
                        Ok(stream) => {
                            let context = pipe_server.context(Principal::Local);
                            let conn = ClientConn::from_pipe(stream);
                            thread::spawn(move || handle_conn(conn, context));
                        }
                        Err(error) => {
                            dlog(&format!("命名管道 accept 失败：{error}"));
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .ok();
    }

    let accept_loop = move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let context = server.context(Principal::Local);
            thread::spawn(move || handle_conn(ClientConn::from_unix(conn), context));
        }
    };

    // 只有被 GUI 拉起时（SMELT_MENUBAR=1，说明继承了登录会话、连得上 WindowServer）
    // 才在顶部状态栏挂图标；命令行 / 无 GUI 会话下老老实实 headless 跑，绝不让「图标」
    // 这个锦上添花的东西把守护本身拖垮。
    //
    // 菜单栏失败时必须继续 accept：历史上 SMELT_MENUBAR 路径在 NSApplication 缺失时
    // panic，整个守护带走、只剩僵尸 sock → GUI 所有「打开项目 / 拖入 / +」全失败。
    #[cfg(target_os = "macos")]
    if std::env::var_os("SMELT_MENUBAR").is_some() {
        let daemon = thread::spawn(accept_loop);
        match menubar::run_event_loop() {
            Ok(()) => {
                // runloop 正常结束（菜单「退出」走 process::exit，一般到不了这里）
                let _ = daemon.join();
            }
            Err(e) => {
                dlog(&format!("menubar 不可用，守护继续 headless：{e}"));
                // accept 在后台线程，主线程 join 撑住进程，效果等同 headless accept_loop
                let _ = daemon.join();
            }
        }
        // accept 循环退出后的兜底。正常停机已经在回执前摘掉路径。
        let _ = std::fs::remove_file(sock_path());
        return;
    }

    accept_loop();
    let _ = std::fs::remove_file(sock_path());
}

/// `input` op 的载荷解析：取 `data` 字段的 UTF-8 字节。空串 / 缺字段 → `None`。
/// 不在这里做 phase 门闩——那是 `action` 的事。
fn input_payload(v: &serde_json::Value) -> Option<Vec<u8>> {
    let s = v["data"].as_str()?;
    if s.is_empty() {
        return None;
    }
    Some(s.as_bytes().to_vec())
}

/// `action` op 的 kind → PTY 字节映射。`text` 只有 `reply` 用得上。
/// `Err` 是给客户端看的错误文案——未知 kind / 空 reply 都走这里，不是默认行为。
fn action_payload(kind: Option<&str>, text: Option<&str>) -> Result<Vec<u8>, &'static str> {
    match kind {
        Some("approve") => Ok(b"\r".to_vec()),
        Some("deny") => Ok(b"\x1b".to_vec()),
        Some("reply") => {
            // 空 reply 若退化成单独 `\r` 就和 approve 一样——误点会当成批准。
            let t = text.unwrap_or("");
            if t.is_empty() {
                return Err("需要非空 text");
            }
            let mut bytes = t.as_bytes().to_vec();
            bytes.push(b'\r');
            Ok(bytes)
        }
        _ => Err("未知 kind"),
    }
}

/// 升级换到哪份映像。可选 `"exe"` 指定不可变映像；没传就是本进程钉住的那份文件。
fn upgrade_target(req: &serde_json::Value) -> Result<std::path::PathBuf, String> {
    if let Some(p) = req["exe"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let path = std::path::PathBuf::from(p);
        if !path.is_file() {
            return Err(format!("exe 不存在：{}", path.display()));
        }
        return Ok(path);
    }
    daemon_executable_path().map_err(|error| format!("找不到当前守护映像：{error}"))
}

/// Windows 升级时接班进程从这个变量拿到前任的 pid，等它退出再绑定监听。
#[cfg(windows)]
const PREDECESSOR_PID_ENV: &str = "SMELTD_PREDECESSOR_PID";

fn handle_upgrade<C: ClientSocket>(conn: C, req: &serde_json::Value) {
    let mut c = conn;
    let exe = match upgrade_target(req) {
        Ok(exe) => exe,
        Err(error) => {
            let _ = smelt_runtime_client::protocol::write_json_line(
                &mut c,
                &serde_json::json!({ "ok": false, "err": error }),
            );
            return;
        }
    };
    // 启动名先拨到这份映像。失败就停在当前映像上，不把进程换到一个拨不上的路径。
    if let Err(error) = smelt_core::daemon_image::retarget_launch_after_commit(&exe) {
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut c,
            &serde_json::json!({ "ok": false, "err": format!("设置启动名失败：{error}") }),
        );
        return;
    }
    // 先收割子进程并回执，再换映像：Unix 在本进程里 exec，systemd 看到的还是同一个
    // 主进程；Windows 拉起接班进程后退出。
    protocol::graceful_shutdown(Some(c.into_client_conn()), Some(exe));
}

/// 开 PTY + 起 shell。交互环境在下面的 `apply_interactive_pty_env`：PTY 只活在守护里，
/// GUI 的 `terminal.rs` 只消费字节流。
/// `launch`：项目「+」悬浮菜单的 Claude Code / Codex 快捷入口——把要跑的命令直接编进
/// 启动命令行（`-ilc '<launch>; exec <shell> -l'`），而不是等 shell 起来后再补发按键。
/// 这样从根上没有"shell 是否已经在读 stdin"的时序问题，命令跑完会 exec 回一个
/// 正常交互 login shell，之后就是一个普通会话。
fn shell_launch_args(shell: &str, launch: Option<&str>) -> Vec<String> {
    #[cfg(windows)]
    {
        // 交互终端：cmd 不读 profile（/d），/k 留下提示符。不走 zsh 的 -ilc。
        let _ = shell;
        match launch {
            Some(launch) => vec!["/d".into(), "/k".into(), launch.to_string()],
            None => vec!["/d".into(), "/k".into()],
        }
    }
    #[cfg(not(windows))]
    {
        match launch {
            Some(launch) => vec!["-ilc".to_string(), format!("{launch}; exec {shell} -l")],
            None => vec!["-l".to_string()],
        }
    }
}

/// 宿主终端的身份变量。smeltd 从别的终端里起（开发时 `cargo run`、命令行里跑 `smelt`）
/// 会把它们带进会话：不少程序先看这些、再看 `TERM_PROGRAM`（Pi 认 `KITTY_WINDOW_ID`、
/// Claude Code 认 `ITERM_SESSION_ID`），漏进来的 `TMUX` 还会让程序把转义序列包成 tmux
/// 透传。会话的终端是 smelt，这些一律去掉。
const HOST_TERMINAL_IDENTITY_VARS: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "STY",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_INSTALLATION_DIR",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_UNIX_SOCKET",
    "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID",
    "WARP_IS_LOCAL_SHELL_SESSION",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "ALACRITTY_LOG",
    "KONSOLE_VERSION",
    "VTE_VERSION",
    "TERMINAL_EMULATOR",
    "WT_SESSION",
    "ZED_TERM",
];

/// 交互式 PTY 是真终端：声明真彩能力，并卸掉宿主漏进来的关色开关和终端身份。
fn apply_interactive_pty_env(cmd: &mut CommandBuilder) {
    cmd.env("TERM", "xterm-256color");
    // 少数 CLI 只认 COLORTERM 才开 24-bit 真彩（Zed 也会设）。
    cmd.env("COLORTERM", "truecolor");
    // 自报 Ghostty 1.2：程序按 TERM_PROGRAM 白名单开功能，没有 smelt 这一项。Ghostty
    // 白名单打开的正好是 smelt 支持的——OSC 8 超链接、OSC 777 / 9 通知、OSC 9;4 进度
    // （Claude Code 要 Ghostty ≥ 1.2）、7501。伪装 iTerm 时 Claude Code 每 200ms 跑一次
    // 清屏探测、Codex 多发 iTerm 私有的 OSC 21337，换掉就没了。TERM 不能跟着改成
    // `xterm-ghostty`：那要 Ghostty 自己的 terminfo。
    cmd.env("TERM_PROGRAM", "ghostty");
    cmd.env("TERM_PROGRAM_VERSION", "1.2.0");
    for key in smelt_core::tty_color::SUPPRESSION_VARS
        .iter()
        .chain(HOST_TERMINAL_IDENTITY_VARS)
    {
        cmd.env_remove(key);
    }
    // UTF-8 locale 兜底（无 LANG 时 zsh 落 C locale 会把 UTF-8 续字节转成乱码）。
    if std::env::var("LANG").is_err() {
        cmd.env("LANG", "en_US.UTF-8");
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

const PI_STATUS_EXTENSION_SOURCE: &str = include_str!("../assets/pi-status-extension.ts");

fn pi_status_extension_path() -> std::path::PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| std::env::temp_dir().join(".smelt"))
        .join("integrations")
        .join("pi-status-extension.ts")
}

fn sync_pi_status_extension() -> std::io::Result<()> {
    sync_pi_status_extension_at(&pi_status_extension_path())
}

fn sync_pi_status_extension_at(path: &std::path::Path) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|source| source == PI_STATUS_EXTENSION_SOURCE) {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("无效的 Pi 状态扩展路径：{}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let staged = parent.join(format!(".pi-status-extension.{}.next", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    let result = (|| {
        std::fs::write(&staged, PI_STATUS_EXTENSION_SOURCE)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&staged, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result
}

/// 返回快捷启动字符串中第一个真正的程序 token。终端启动项允许在前面放
/// `VAR=value`/`env`，也允许写 CLI 的绝对路径；所有进程级集成必须共用这套
/// 识别语义，不能只认字面以 provider 名开头的默认命令。
fn launch_program(launch: &str) -> Option<&str> {
    for token in launch.split_whitespace() {
        if token == "env" || smelt_core::workspace_override::split_env_assignment(token).is_some() {
            continue;
        }
        let program = token.trim_matches(['\'', '"']);
        return std::path::Path::new(program)
            .file_name()
            .and_then(|name| name.to_str());
    }
    None
}

fn launch_terminal_agent_kind(launch: &str) -> Option<smelt_core::agent_kind::TerminalAgentKind> {
    let program = launch_program(launch)?;
    smelt_core::agent_kind::TerminalAgentKind::ALL
        .into_iter()
        .find(|kind| kind.cli_program() == program)
}

fn launch_with_terminal_status_bridge(launch: &str, extension: &std::path::Path) -> String {
    let Some(agent) = launch_terminal_agent_kind(launch) else {
        return launch.to_string();
    };
    match agent.status_bridge() {
        smelt_core::agent_kind::TerminalStatusBridge::None => launch.to_string(),
        smelt_core::agent_kind::TerminalStatusBridge::PiExtension => {
            let extension = extension.to_string_lossy();
            if launch.contains(extension.as_ref()) {
                return launch.to_string();
            }
            let option = format!("--extension {}", shell_quote(&extension));
            if let Some(index) = launch.find(" -- ") {
                format!("{} {option}{}", &launch[..index], &launch[index..])
            } else if let Some(prefix) = launch.strip_suffix(" --") {
                format!("{prefix} {option} --")
            } else {
                format!("{launch} {option}")
            }
        }
    }
}

fn spawn_session(
    id: &str,
    instance: u64,
    rows: u16,
    cols: u16,
    cwd: Option<&str>,
    launch: Option<&str>,
    event_hub: &EventHubHandle,
) -> anyhow::Result<(Session, Box<dyn PtyReader>)> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    #[cfg(windows)]
    let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    #[cfg(not(windows))]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(shell.clone());
    // 快捷启动必须同时是 interactive + login：用户级 CLI 安装器通常把 PATH
    // 写进 .zshrc，只用 `-lc` 读不到，Dock 启动的 smeltd 又只有系统 PATH。
    let agent_token = uuid::Uuid::new_v4().simple().to_string();
    let status_extension = pi_status_extension_path();
    let injected_launch = launch.map(|launch| {
        if status_extension.is_file() {
            launch_with_terminal_status_bridge(launch, &status_extension)
        } else {
            launch.to_string()
        }
    });
    for arg in shell_launch_args(&shell, injected_launch.as_deref()) {
        cmd.arg(arg);
    }
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }
    apply_interactive_pty_env(&mut cmd);
    // 整条 hook 链路的地基：没有它，smelt-notify 没法知道自己在哪个会话里，
    // 后面的 state op 全是空中楼阁（见 docs/archive/state-channel-plan.md）。
    cmd.env("SMELT_SESSION_ID", id);
    cmd.env("SMELT_AGENT_TOKEN", &agent_token);
    cmd.env("SMELT_SOCK", client_conn::child_smelt_sock());
    cmd.env(
        "SMELT_NOTIFY_BIN",
        smelt_core::agent_event::notify_executable_path(),
    );
    // 与其它 spawn 共用这把锁（见 SPAWN_GATE）。
    let child = {
        let _gate = SPAWN_GATE.read().unwrap();
        pair.slave.spawn_command(cmd)?
    };
    #[cfg(unix)]
    let (terminal_child, master, pty_reader) = {
        let pid = child
            .process_id()
            .map(|p| p as i32)
            .ok_or_else(|| anyhow::anyhow!("拿不到 shell pid"))?;
        // child 句柄丢弃：kill/收尸都用 pid（portable_pty 的 Child drop 不杀进程）。
        drop(child);
        let terminal_child = TerminalChild::start(pid)?;
        let raw = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| anyhow::anyhow!("拿不到 PTY master fd"))?;
        let master = dup_file(raw)?;
        set_fd_nonblocking(master.as_raw_fd(), true)?;
        let pty_reader = master.try_clone()?;
        (terminal_child, master, pty_reader)
    };
    #[cfg(windows)]
    let (terminal_child, master, input_tx, pty_reader) = {
        let terminal_child = TerminalChild::start_with_child(child)?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| anyhow::anyhow!("拿不到 ConPTY 读端：{error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| anyhow::anyhow!("拿不到 ConPTY 写端：{error}"))?;
        let input_tx = spawn_pty_input_thread(Arc::new(Mutex::new(writer)));
        let pty_reader = threaded_pty_reader(reader);
        (terminal_child, pair.master, input_tx, pty_reader)
    };
    let state = Arc::new(Mutex::new(SessionState {
        id: id.to_string(),
        instance,
        cwd: cwd.map(String::from),
        launch: launch.map(String::from),
        agent_token,
        ..Default::default()
    }));
    let color_replies = Arc::new(Mutex::new(VecDeque::new()));
    let sess = Session {
        instance,
        geometry_token: uuid::Uuid::new_v4().simple().to_string(),
        child: terminal_child,
        ctl: Mutex::new(Ctl {
            master,
            #[cfg(windows)]
            input_tx,
            jolt: false,
            cols,
            rows,
            cell_w: 0,
            cell_h: 0,
            remote_viewports: 0,
            remote_grace: 0,
        }),
        input_gate: Mutex::new(()),
        out: Mutex::new(Out {
            clients: Vec::new(),
            watchers: Vec::new(),
        }),
        output_gate: Mutex::new(()),
        color_replies: Arc::clone(&color_replies),
        term: Mutex::new(new_daemon_term(
            rows,
            cols,
            StateListener::with_color_replies(
                Arc::clone(&state),
                Arc::clone(event_hub),
                color_replies,
            ),
        )),
        state,
    };
    Ok((sess, Box::new(pty_reader)))
}

fn write_terminal_color_replies(sess: &Session) {
    let pending = {
        let Ok(mut pending) = sess.color_replies.lock() else {
            return;
        };
        std::mem::take(&mut *pending)
    };
    if pending.is_empty() {
        return;
    }
    // 只为读改过的颜色短暂拿一下网格锁，写回 PTY 前放开。
    let overrides = sess.term.lock().ok().map(|term| *term.colors());

    for (index, format) in pending {
        let color = overrides
            .and_then(|colors| colors[index])
            .unwrap_or_else(|| resolve_terminal_color(index));
        if write_session_input(sess, format(color).as_bytes()).is_err() {
            break;
        }
    }
}

/// 常驻 Term 在解析 OSC 颜色查询时只能入队（它当时持有网格锁）。这里在网格锁释放后
/// 统一写回 child，沿用 `Ctl` 的串行写入路径，避免与 resize 反向抢锁。
fn flush_terminal_color_replies(sess: &Session) {
    write_terminal_color_replies(sess);
}

/// 把一批输出里扫到的 OSC 通知发给订阅的客户端。会话已有结构化 hook 事件时不发：
/// 那之后 agent 的状态和通知都以 hook 为准，终端通知只会重复。
fn publish_terminal_notifications(
    state: &Mutex<SessionState>,
    id: &str,
    event_hub: &EventHubHandle,
    messages: Vec<String>,
) {
    if messages.is_empty() {
        return;
    }
    let structured = state
        .lock()
        .map(|state| state.structured_events)
        .unwrap_or(true);
    if structured {
        return;
    }
    for message in messages {
        if let Err(error) = event_hub.publish_terminal_notification(id, &message) {
            eprintln!("[event-hub] terminal notification publish failed: {error}");
        }
    }
}

fn publish_terminal_progress(progress: &mut ProgressTracker, id: &str, event_hub: &EventHubHandle) {
    if let Some(update) = progress.take_update(Instant::now())
        && let Err(error) = event_hub.publish_terminal_progress(id, update)
    {
        eprintln!("[event-hub] terminal progress publish failed: {error}");
    }
}

/// PTY 输出泵：读 PTY → advance 常驻 Term → 转发 client / watchers。
/// shell 退出（EOF）：移除会话、断开客户端、收割子进程。
fn start_pty_pump(
    sess: Arc<Session>,
    mut pty_reader: Box<dyn PtyReader>,
    id: String,
    sessions: Sessions,
    event_hub: EventHubHandle,
    remote_sessions: Option<RemoteSessions>,
    slot: Arc<TerminalSlot<Session>>,
) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut parser: Processor = Processor::new();
        // OSC 9/99/777 通知、9;4 进度和 7501 程序状态 alacritty 不解析，在这里逐字节扫
        // 一次。只扫实时输出：resume 喂进网格的旧 keyframe 不经过这里，历史通知不会被
        // 当成新的再发。
        let mut osc_scan = OscScan::default();
        let mut status_notices = ProgramStatusNotices::default();
        let mut progress = ProgressTracker::default();
        // resume 的旧 keyframe 理论上不含查询序列；即便有，也要在首次 read 前答复。
        flush_terminal_color_replies(&sess);
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) => break,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    #[cfg(unix)]
                    {
                        let mut pollfd = libc::pollfd {
                            fd: pty_reader.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        let polled = unsafe { libc::poll(&mut pollfd, 1, 1000) };
                        if polled < 0
                            && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted
                        {
                            break;
                        }
                        if polled > 0 {
                            let events = pollfd.revents;
                            if events & libc::POLLNVAL != 0
                                || (events & libc::POLLIN == 0
                                    && events & (libc::POLLERR | libc::POLLHUP) != 0)
                            {
                                break;
                            }
                        }
                    }
                    // 没输出时每秒醒一次：顺带把不再刷新的进度条清掉、定期重发还在的。
                    // Windows 的读端在超时后返回 WouldBlock，这里不再 poll。
                    publish_terminal_progress(&mut progress, &id, &event_hub);
                    continue;
                }
                Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    let mut notifications = Vec::new();
                    for event in chunk.iter().filter_map(|&byte| osc_scan.feed(byte)) {
                        match event {
                            OscEvent::Notification(notification) => {
                                notifications.push(notification.text);
                            }
                            OscEvent::ProgramStatus(status) => {
                                notifications.extend(status_notices.observe(status));
                            }
                            OscEvent::Progress(report) => progress.observe(report, Instant::now()),
                            // 在这批输出交给客户端之前就回应：Pi 把探测和 DA 查询一起发，
                            // 拿 DA 的回应当探测结束；DA 由客户端的终端回应，绕一圈回来
                            // 一定在这之后。
                            OscEvent::ProgramStatusQuery(terminator) => {
                                let _ =
                                    write_session_input(&sess, &program_status_reply(terminator));
                            }
                        }
                    }
                    // Claim the output sequence before advancing the grid. Resize uses
                    // the same gate, so its geometry marker cannot overtake bytes from
                    // this read. The gate is released before any potentially blocking
                    // PTY input or subscriber broadcast.
                    let output_gate = sess.output_gate.lock().unwrap();
                    let mut term_guard = sess.term.lock().ok();
                    if let Some(term) = term_guard.as_mut() {
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            parser.advance(&mut **term, chunk);
                        }));
                    }
                    drop(term_guard);
                    dispatch_session_outputs_under_gate(&sess, chunk, &id, true, true);
                    drop(output_gate);
                    // 颜色查询应答可能等待 PTY 输入队列变得可写；必须在释放网格锁后
                    // 再做，避免把 resize 的 ctl -> term 路径拖住。
                    flush_terminal_color_replies(&sess);
                    publish_terminal_notifications(&sess.state, &id, &event_hub, notifications);
                    publish_terminal_progress(&mut progress, &id, &event_hub);
                }
            }
        }
        // 主动 kill 可能已经摘掉并替换了同 ID slot。这里只能清理自己仍拥有的实例，
        // 否则旧 PTY EOF 会删掉刚创建的新会话和它的远程目录。
        let _lifecycle = slot.lifecycle.lock().unwrap();
        let removed_current = if sessions.is_current(&id, &slot) {
            if let Some(remote_sessions) = remote_sessions
                && let Err(error) = remove_remote_session_for_instance(
                    &remote_sessions,
                    &event_hub,
                    RemoteSessionKind::Terminal,
                    &id,
                    sess.instance,
                )
            {
                eprintln!("[remote] 终端会话 {id} 退出后清理目录失败：{error}");
            }
            sessions.remove_if_same(&id, &slot).is_some()
        } else {
            false
        };
        if removed_current {
            forget_session(&event_hub, &id, sess.instance);
            smelt_core::app_log::info("session", &format!("会话 {id} 已结束（shell 退出）"));
        }
        drop(_lifecycle);
        let _output_gate = sess.output_gate.lock().unwrap();
        let mut out = sess.out.lock().unwrap();
        for c in out.clients.drain(..) {
            c.close(); // GUI 读到 EOF 即知 shell 退出
        }
        for w in out.watchers.drain(..) {
            w.close(); // 旁观者同样该收到 EOF
        }
        drop(out);
        // PTY EOF 与直接 shell 退出不是同一时刻；精确 PID waiter 从 spawn/恢复时就
        // 已经独立运行。EOF 这里只确认它完成，异常断流时再发 SIGKILL，但绝不自己
        // waitpid 与唯一 owner 抢退出状态。
        if !sess.child.wait_reaped(Duration::from_millis(100))
            && !sess.child.terminate_and_wait(TERMINAL_CHILD_REAP_TIMEOUT)
        {
            dlog(&format!(
                "terminal: PTY 结束后未能确认 shell pid={} 已回收",
                sess.child.pid()
            ));
        }
    });
}

#[cfg(test)]
mod main_tests;
