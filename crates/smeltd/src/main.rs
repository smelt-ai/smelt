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
//!   {"op":"shutdown"}                                        → 回 {"ok":true} 后进程退出（杀掉所有会话！）
//!   {"op":"upgrade"}                                         → 回 {"ok":true} 后 exec 磁盘上的新二进制，
//!                                                              PTY/ACP session-host fd 原地交接；仅旧版
//!                                                              direct ACP 遗留连接活跃时回 busy（见下）
//!   {"op":"upgrade","exe":"/path/to/smeltd"}                 → 同上，但 exec 指定路径（装 DMG 时先
//!                                                              handoff 到暂存包，再替换 .app，避免
//!                                                              整包覆盖把旧守护 SIGKILL、会话全灭）
//!   {"op":"remote_start","bind":"..","port":0,"write":false}  → 回 {"ok":true,"token":"..","addr":"..","write":bool}，
//!                                                              见下「内嵌远程网关」（bind/port/write 都可省，
//!                                                              默认回环随机口 + 只读）
//!   {"op":"remote_stop"}                                     → 回 {"ok":true} 后关闭
//!   {"op":"remote_set_write","write":true}                 → 热更新已连接远程 ACP 的写权限
//!   {"op":"remote_rotate_token"}                              → 停止远程服务、持久化新 token，旧配对失效
//!   {"op":"remote_status"}                                   → 回 {"running":bool,"token":"..","addr":"..","write":bool} 后关闭
//!   {"op":"iroh_start","write":false}                         → 回 {"ok":true,"endpoint_id":"..","token":"..",
//!                                                              "addr":"..","write":bool}，把远程网关经 iroh
//!                                                              P2P 暴露出去（见下「iroh 隧道」）
//!   {"op":"iroh_stop"}                                       → 回 {"ok":true} 后关闭
//!   {"op":"iroh_status"}                                     → 回 {"running":bool,"endpoint_id":"..","token":"..",
//!                                                              "write":bool} 后关闭
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
//! ## 无缝升级（"upgrade" op，交接 v2 双进程事务）
//!
//! 老进程 spawn 新进程，经 socketpair + `SCM_RIGHTS` 传 manifest 与 fd，
//! READY/COMMIT 两阶段提交。COMMIT 发出前任何失败都回滚、老进程原地继续服务。
//! 不变量：同一时刻只有一个进程读每个 PTY；传输不经盘；失败不丢会话。流程：
//! 1. 先拿 SPAWN_GATE 独占锁挡住新 shell/ACP 子进程的 fork，再短暂持 sessions 锁
//!    克隆一份 Arc 列表后放开——避免 open/list/kill/version 长期卡在 sessions 锁上，
//!    同时保证任何已 fork 的 ACP 都先把 pid/fd 发布完，才开始收集交接快照；
//!    独立 ACP host 不需要静默；只为旧版 direct-fd 遗留连接保留一次兼容屏障；
//! 2. 逐会话拿输入/输出闸门（持有至事务结束=pause），再按 ctl → term → out 锁
//!    做快照（shell pid / 尺寸 / 按真实终端模式生成的 keyframe：主屏含 history，
//!    备用屏仅 viewport）；终端输出以每 attachment 独立队列转发；状态/ACP socket
//!    写入仍有 CLIENT_WRITE_TIMEOUT，不会无限期阻塞会话管理；
//! 3. manifest 版本化、分级、校验：MUST（fd 表/ids/尺寸/ACP 纯数据，整体校验）
//!    vs BEST-EFFORT（grid 各自校验，坏只丢画面不丢会话）；**画面恢复只认 grid**；
//! 4. spawn successor（暂存 `.next` 先扶正），传 manifest → fd（dup，原件保持
//!    CLOEXEC 全程不动）→ grid；successor 恢复会话后报 READY；
//! 5. predecessor 刷 EventHub → 发 COMMIT → 停 sidecars/插件 → 回 {"ok":true}
//!    → exit(0)。successor 收到 COMMIT 后开始服务（accept/插件/网关自启）。
//!
//! 回滚（COMMIT 发出前）：杀 successor，drop guards 恢复服务，回 {"ok":false}，
//! 插件/sidecar 全程没动过。客户端连接随老进程退出断开，GUI 按会话 id 重连即
//! 恢复——跟 GUI 自己重启走的是同一条 reattach 路。
//! fork-exec 后子进程重定父到 launchd：终端子进程经 ChildMonitor（kqueue/pidfd）
//! 收养观察退出（退出码本来就不存）；ACP/menubar 对收养天然免疫（kill+ECHILD 容忍）。
//! 回滚 tripwire：successor 若已迁移 store sqlite schema，老进程不可再服务，
//! 此时 exit 交由拉起逻辑重开新版（结局=旧版最坏情况，显式可观测）。
//!
//! legacy exec-self 文件交接（一代兼容）：旧二进制仍会 exec 新二进制并传
//! SMELTD_HANDOFF 文件路径；新二进制保留文件读端。TODO(下版)：删文件读端、
//! `handoff_path` 清理逻辑与 `.next` 启动自提升。
//!
//! ## 内嵌远程网关（`remote_start`/`remote_stop`/`remote_status`）
//!
//! 路由/handler 全在 `smelt_remote_gateway` crate（跟独立进程版 `gateway.rs` 共用一份，
//! 见该 crate 文档）——这里只是按需把它跑起来。守护本身是同步/阻塞线程模型，**不**把
//! `main()` 整个改成 async；`remote_start` 只是另起一条 OS 线程，在那条线程里私自建
//! 一个 tokio runtime 跑 axum server，跟守护主循环完全隔离，互不影响。
//!
//! 幂等：已经开着时 `remote_start` 直接回现有的 token/addr，不重启、不换 token。
//! token 单独保存在 `~/.smelt/remote-token`（0600），冷启动和无缝升级都复用；只有
//! `remote_rotate_token` 会轮换并让旧配对失效。网关运行态本身**不**参与无缝升级交接：
//! `upgrade` 之后旧进程里的网关随之关闭，新进程内存里是空的——但新进程启动时会读
//! 主库远程配置快照，用户之前开着远程就自动拉回来（见
//! `autostart_remote_from_config`）。这条自愈路径不能少：守护重启（硬重启 / 升级 exec /
//! 崩溃后被拉起）之后若没人重新 `remote_start`，手机侧就会静默失联，只能靠用户去设置页
//! 把远程「关掉再打开」。安全默认跟 `watch` 一致：没配置过就是关闭、绑回环，
//! 见 collaboration.md 的安全底线。
//!
//! 网关在 macOS 上按供电决定是否持有 `PreventUserIdleSystemSleep`：插电时远程开着
//! 就握着（旧行为），屏幕可灭但整机不因空闲睡眠把网关和 iroh 挂起；电池上绝不握
//! 断言，不阻止任何休眠，远程只能在电脑醒着、正在用的时候连。合盖仍走 Clamshell
//! Sleep。机器一旦睡着，手机连不上。`exec()` 不跑 `Drop`，IOKit 断言却跟着 PID
//! 活过映像替换；升级前必须先禁止 autostart 再释放，新进程启动时再清掉本 PID 上
//! 同名残留。关掉远程之后只许剩 0 条。
//!
//! ## iroh 隧道（`iroh_start`/`iroh_stop`/`iroh_status`）
//!
//! 解决「内嵌远程网关默认绑回环，手机切到蜂窝网络就连不上」这个问题：iroh 优先
//! 打洞直连，打不通才回退到中继。
//!
//! 这是**唯一**的公网通路。早先还有 Cloudflare quick tunnel 和自建信令 + WebRTC
//! 两条，都已经删掉：前者的 URL 每次重开都变，手机上存的配对必然失效；后者要自建
//! 信令 + coturn，且只对浏览器有意义。iroh 的 `endpoint_id` 由 `~/.smelt/iroh-secret`
//! 里的私钥决定，重启不变，于是二维码可以一次扫、长期用——这是留下它的主要理由。
//!
//! 实现上没有子进程，因此没有孤儿进程那套；跟远程网关一样另起一条 OS 线程跑自己的
//! tokio runtime。转发逻辑在 `smelt-iroh` crate，与命令行
//! 版 `smelt-iroh-host` 共用一份（一条 iroh 双向流 ⟷ 一条到网关的 TCP 连接，逐字节转发，
//! 上层 HTTP/WebSocket/token 鉴权完全不变）。
//!
//! 注意 `endpoint_id` **不是**授权凭证：拿到它的人只是能连上网关，能不能操作仍由网关的
//! token 决定，所以配对码必须 endpoint_id + token 一起给。
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
//! 授权模型：链接本身就是授权；写权限（action + input）由生成链接时的开关决定
//! （GUI 的"允许写入"），网关侧 `write_enabled` 把关，smeltd 的 action 门闩只管
//! 时机、不管权限。

mod acp_host;
mod acp_registry;
mod acp_runtime_host;
mod automation_runtime;
mod control;
mod event_hub;
mod handoff_v2;
#[cfg(target_os = "macos")]
mod menubar;
mod peer_messaging;
mod plugin_runtime;
mod protocol;
mod remote;
mod session_catalog;
mod session_directory;
mod session_state;
mod terminal_output;
mod terminal_registry;
mod terminal_snapshot;
mod webhook;

use acp_host::*;
use protocol::*;
use remote::*;

use acp_registry::{AcpRegistry, AcpSlot};
use event_hub::EventHubHandle;
use session_directory::SessionDirectory;
#[cfg(test)]
pub(crate) use session_state::AgentBlocker;
pub(crate) use session_state::{
    Phase, PhaseSource, SessionState, apply_agent_event, apply_terminal_title, bump_state_revision,
    commit_session_phase, next_session_instance, now_unix,
};
use smelt_core::agent_event::{AGENT_EVENT_VERSION, AgentEvent};
use smelt_core::automation::{AutomationCommand, AutomationFile, AutomationInboundEvent};
use smelt_core::automation_store::{
    AutomationStore, local_timezone_fingerprint, new_automation_store,
};
use smelt_core::osc::{TerminalGeometryOsc, terminal_geometry_osc};
use smelt_core::session_control::{
    RemoteAcpSession, RemoteSessionCatalog, RemoteSessionKind, RemoteSessionLifecycle,
    RemoteSessionSnapshot, RemoteTerminalSession, find_agent_option_for,
};
use smelt_core::workspace_menu::{
    WorkspaceMenuSnapshot, load_published_workspace_menu, persist_published_workspace_menu,
};
use smelt_remote_gateway as remote_gateway;
use terminal_output::{OutputAttachment, enqueue_session_streams};
use terminal_registry::{TerminalRegistry, TerminalSlot};
use terminal_snapshot::{
    SNAPSHOT_MAX_LINES, available_snapshot_lines, snapshot_ansi, snapshot_ansi_for_watch,
};

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
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
/// 挡住「spawn 新 shell/ACP 子进程」与「upgrade 清 CLOEXEC 准备 exec」并发的门闩：
/// 不挡会有极小窗口——CLOEXEC 刚被清、我们自己还没 exec 时，恰好 fork 出一个新进程，
/// 会把当时暴露出去的全部 fd（其它会话的 PTY master、监听 socket）一并带走。
/// spawn 拿共享锁（多个新会话可以互相并发起），upgrade 拿独占锁（跟所有 spawn 互斥）。
static SPAWN_GATE: LazyLock<Arc<RwLock<()>>> = LazyLock::new(|| Arc::new(RwLock::new(())));

fn acquire_upgrade_spawn_gate(gate: &Arc<RwLock<()>>) -> std::sync::RwLockWriteGuard<'_, ()> {
    gate.write().unwrap()
}

fn sock_path() -> std::path::PathBuf {
    let dir = smelt_paths::smelt_home().unwrap_or_else(|| "/tmp/.smelt".into());
    let _ = std::fs::create_dir_all(&dir);
    dir.join("smeltd.sock")
}

fn is_staged_daemon_executable(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("smeltd") && name.ends_with(".next"))
}

/// 从暂存文件接过 handoff 的新映像已经成功通过了一次 `exec`。在启动任何线程、
/// 消费交接文件之前，把它原子提升为正式路径；随后 main 会再做一次不涉及会话快照的
/// 轻量 `exec`，让 macOS 的进程名也成为 `smeltd`。
///
/// 普通手工运行 `.next` 不得触发安装，只有携带 `SMELTD_HANDOFF` 的升级映像才允许。
fn promote_staged_handoff_executable(
    current: &std::path::Path,
    came_from_handoff: bool,
) -> std::io::Result<Option<std::path::PathBuf>> {
    if !came_from_handoff || !is_staged_daemon_executable(current) {
        return Ok(None);
    }
    let stable = current.with_file_name("smeltd");
    std::fs::rename(current, &stable)?;
    Ok(Some(stable))
}

fn finalize_staged_handoff_executable() {
    let came_from_handoff = std::env::var_os("SMELTD_HANDOFF").is_some();
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let stable = match promote_staged_handoff_executable(&current, came_from_handoff) {
        Ok(Some(stable)) => stable,
        Ok(None) => return,
        Err(error) => {
            eprintln!(
                "smeltd 暂存映像提升到正式路径失败（{}）：{error}",
                current.display()
            );
            return;
        }
    };

    // 此时没有业务线程，SMELTD_HANDOFF、插件指纹与上一映像继承的 fd 都保持原样。
    // 第二次 exec 只校正 executable path / 进程显示名，不重新生成或消费 handoff。
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(&stable).exec();
    // 同一 inode 刚刚已从暂存路径成功 exec，正常不会走到这里。失败时继续当前映像，
    // daemon 仍可恢复会话；daemon_executable_from_current 会回退到正式路径。
    eprintln!(
        "smeltd 从正式路径重新 exec 失败（{}）：{error}",
        stable.display()
    );
}

/// 返回 `current_exe()` 这条路径现在指向的文件。参数化一层是为了覆盖 macOS 上
/// `current_exe()` 在文件被 rename 后仍返回旧启动路径的行为。
///
/// 路径还在不等于映像还是启动时那一份：原地替换（rename 覆盖 `smeltd`）后路径
/// 仍在，inode 已经换了。会话宿主不能走这里，必须用 [`session_host_executable`]。
fn daemon_executable_from_current(
    current: std::path::PathBuf,
) -> std::io::Result<std::path::PathBuf> {
    // 路径没了（rename 掉 smeltd.next、删掉历史硬链、测试误伤）时，回退到同目录的
    // 正式 `smeltd`，不能把「文件不存在」抛给调用方。
    if current.is_file() {
        return Ok(current);
    }

    let stable = current.with_file_name("smeltd");
    if stable.is_file() {
        return Ok(stable);
    }

    Err(std::io::Error::new(
        ErrorKind::NotFound,
        format!(
            "守护可执行文件不存在：{}（稳定路径：{}）",
            current.display(),
            stable.display()
        ),
    ))
}

fn daemon_executable_path() -> std::io::Result<std::path::PathBuf> {
    daemon_executable_from_current(std::env::current_exe()?)
}

/// 启动时 `current_exe()` 的 inode。安装协议保证这条路径在进程活着时不会被换成
/// 另一份文件；spawn 前再对一次，对不上就拒绝启动，而不是去跑目录里的新文件。
struct PinnedDaemonInode {
    dev: u64,
    ino: u64,
}

static PINNED_DAEMON_INODE: OnceLock<PinnedDaemonInode> = OnceLock::new();

fn pin_running_daemon_image() {
    let Ok(path) = std::env::current_exe() else {
        return;
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    use std::os::unix::fs::MetadataExt;
    let _ = PINNED_DAEMON_INODE.set(PinnedDaemonInode {
        dev: meta.dev(),
        ino: meta.ino(),
    });
}

fn path_has_inode(path: &std::path::Path, dev: u64, ino: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    use std::os::unix::fs::MetadataExt;
    meta.dev() == dev && meta.ino() == ino
}

fn session_host_executable_from(
    current: std::path::PathBuf,
    pinned: Option<(u64, u64)>,
) -> std::io::Result<std::path::PathBuf> {
    let Some((dev, ino)) = pinned else {
        return Ok(current);
    };
    if path_has_inode(&current, dev, ino) {
        return Ok(current);
    }
    Err(std::io::Error::other(format!(
        "smeltd 路径已指向另一份映像，拒绝用它启动会话宿主：{}",
        current.display()
    )))
}

/// 会话宿主必须和本进程是同一个 inode。路径被换掉时直接失败，不能 exec 新文件，
/// 也不能另拷一份再跑——那份拷贝的 `current_exe()` 和签名都不再是正在映射的映像。
fn session_host_executable() -> std::io::Result<std::path::PathBuf> {
    let current = daemon_executable_path()?;
    let pinned = PINNED_DAEMON_INODE
        .get()
        .map(|pinned| (pinned.dev, pinned.ino));
    session_host_executable_from(current, pinned)
}

/// 已 stage、尚未应用到运行中守护的候选映像。daemon 在跑时安装只写这个文件，
/// 不覆盖正在 exec 的 `smeltd`，所以 `current_exe()` 派生的 ACP host 永远跟主进程同版。
fn staged_successor_executable(running: &std::path::Path) -> Option<std::path::PathBuf> {
    let next = running.with_file_name("smeltd.next");
    (next.is_file() && next != running).then_some(next)
}

/// 进程指纹钉死（启动时调一次）：env 传过来的优先（handoff successor 的候选
/// 指纹 / GUI 拉起时对 spawn 目标的哈希，与本进程映像一致）；否则对启动时刻
/// 的 current_exe 文件哈希。None 只在文件已消失且无 env 时出现（.app 被删后
/// 的孤儿进程），此时插件/版本判断一律按“未知”等待，不猜。
fn pinned_daemon_fingerprint(env: Option<String>) -> Option<String> {
    if let Some(fingerprint) = env.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Some(fingerprint);
    }
    daemon_executable_path()
        .ok()
        .and_then(|exe| smelt_plugin_host::executable_fingerprint(&exe).ok())
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
    let lock = std::fs::OpenOptions::new()
        .create(true)
        // 锁文件只借 flock 语义，不关心内容，绝不截断（可能已有旧内容）。
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    if check_existing && UnixStream::connect(path).is_ok() {
        return Ok(None);
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).map(Some)
}

fn bind_fresh_daemon(
    path: &std::path::Path,
    stale_handoff: &std::path::Path,
    check_existing: bool,
) -> std::io::Result<Option<UnixListener>> {
    let listener = bind_single_instance(path, check_existing)?;
    if listener.is_some() {
        // 只有确认自己取得 listener 后才能清理。若已有 daemon 正在 upgrade，
        // 它刚写下的 handoff 是活数据，竞争启动者必须原样保留并直接退出。
        let _ = std::fs::remove_file(stale_handoff);
    }
    Ok(listener)
}

fn secure_daemon_socket(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
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
/// 持有的是 PTY master 的**裸 fd**（File 包装）而非 portable_pty 的类型：无缝升级要把
/// fd 原样带过 exec，portable_pty 的 MasterPty/Child 无法从裸 fd 重建。spawn 仍用
/// portable_pty（openpty + 环境 + 会话组等脏活），起完就把 fd dup 出来自己管。
struct Ctl {
    /// PTY master：写输入 + ioctl(TIOCSWINSZ) resize；泵线程的读端是它的 try_clone。
    master: std::fs::File,
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
    /// spawn 时的静态目录（作战地图要）。**不**跟随 shell 的 `cd`——真实 cwd 要
    /// OSC 7，这里只是「这个会话是从哪打开的」，见 SessionState.cwd 用法。
    cwd: Option<String>,
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
    /// 交接 v2 收养的子进程退出（经 ChildMonitor 观察，非亲生、无 wait）。
    /// 退出码本来就不存，此变体与 Reaped 同等干净。
    AdoptedExited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalChildState {
    Running,
    Finished(TerminalChildExit),
}

/// 一个终端 shell 的唯一 `waitpid` 所有者。
///
/// PTY EOF 代表所有持有 slave 的后代都离开了，不等于直接 shell 此刻才退出。若把
/// `waitpid` 放在 EOF 后，shell 先退、后台后代继续持有 PTY 时会永久留成僵尸。这里在
/// PID 发布前立即挂一个精确 waiter；kill、PTY pump 和 handoff 恢复都只能观察结果，
/// 不能再竞争退出状态。
struct TerminalChild {
    pid: i32,
    state: Mutex<TerminalChildState>,
    changed: Condvar,
}

impl TerminalChild {
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
                // 状态锁内完成 waitpid 与状态发布：kill/handoff 要么先锁住 Running，
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

    /// 恢复 handoff 中仍拥有退出状态的直接子进程。调用方已经排除了旧映像完成
    /// wait 的条目；这一步再确认 PID 目前确实仍是本进程可等待的直接子进程。
    fn restore(pid: i32) -> std::io::Result<Arc<Self>> {
        if pid <= 1 {
            return Ok(Self::finished(pid, TerminalChildExit::AlreadyReaped));
        }
        if !exact_child_is_waitable(pid)? {
            return Ok(Self::finished(pid, TerminalChildExit::AlreadyReaped));
        }
        Self::start(pid)
    }

    /// 交接 v2 收养 fork-exec 前任的子进程：已重定父到 launchd，不能 waitpid，
    /// 经 ChildMonitor（kqueue/pidfd）观察退出。形态与 [`TerminalChild::start`]
    /// 镜像：同样一子进程一等待线程，同样经状态锁发布、同样 Condvar 唤醒。
    fn adopt(pid: i32, monitor: &handoff_v2::child_monitor::ChildMonitor) -> Arc<Self> {
        let Some(waiter) = monitor.waiter(pid) else {
            // pid 不在监控集合里 = 内部不变量已破（successor 按 manifest 建监控，
            // Live 条目必在其中）。fail-closed + 留痕：不定住 Running（kill 路径
            // 按 pid 直接 SIGKILL，会话仍经 PTY EOF 正常结束）。
            dlog(&format!(
                "handoff: 收养 pid={pid} 不在监控集合中，直接记已退出"
            ));
            return Self::finished(pid, TerminalChildExit::AdoptedExited);
        };
        if waiter.is_exited() {
            return Self::finished(pid, TerminalChildExit::AdoptedExited);
        }
        let child = Arc::new(Self {
            pid,
            state: Mutex::new(TerminalChildState::Running),
            changed: Condvar::new(),
        });
        let adopted = Arc::clone(&child);
        if let Err(error) = thread::Builder::new()
            .name(format!("smelt-terminal-adopt-{pid}"))
            .spawn(move || {
                waiter.wait();
                let mut state = adopted
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                *state = TerminalChildState::Finished(TerminalChildExit::AdoptedExited);
                adopted.changed.notify_all();
            })
        {
            // 线程都起不来：资源枯竭。fail-closed：SIGKILL 该子进程并直接记
            // 已退出（launchd 会 reap 它），绝不留下无人观察的 Running。
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            dlog(&format!(
                "handoff: 收养 pid={pid} 等待线程启动失败，已 SIGKILL：{error}"
            ));
            return Self::finished(pid, TerminalChildExit::AdoptedExited);
        }
        child
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
                TerminalChildExit::Reaped
                    | TerminalChildExit::AlreadyReaped
                    | TerminalChildExit::AdoptedExited
            )
        )
    }

    fn terminate_and_wait(&self, timeout: Duration) -> bool {
        if self.pid <= 1 {
            return true;
        }
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(*state, TerminalChildState::Running) {
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
        }
        drop(state);
        self.wait_reaped(timeout)
    }

    /// upgrade 从快照到 exec 全程持有这把锁。若 child 在此期间退出，waiter 会停在
    /// waitid(WNOWAIT) 之后，退出状态与 PID 都不会被释放；新映像可安全按原 PID 接管。
    fn lock_for_handoff(&self) -> std::sync::MutexGuard<'_, TerminalChildState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

trait PtyReader: Read + AsRawFd + Send {}

impl<T: Read + AsRawFd + Send> PtyReader for T {}

/// 本进程启动时刻（unix 秒），`version` op 回给 GUI 展示「守护跑了多久」。
/// 必须在 main 最开头取一次，否则记的是「首次有人问」的时间。
///
/// 无缝升级（exec 交接）后是全新进程，这个值会重置，而会话照旧活着——设置页因此
/// 会显示「守护刚起、会话仍在」，那是如实反映，不是 bug。
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
    event_hub::DaemonEventHub::in_memory()
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

/// 瞬时展示状态只进订阅总线，不进崩溃恢复目录。OSC 标题可能是
/// spinner 帧；将它落盘会让整份 sessions 文档持续重写。与持久广播共用
/// commit gate，避免 EventHub 发布和目录提交交错；迟到 revision 由 EventHub 拒绝。
fn broadcast_transient_state(event_hub: &EventHubHandle, state: &SessionState) {
    let _commit = STATE_DIRECTORY_COMMIT_GATE.lock().unwrap();
    if let Err(error) = event_hub.publish_session(state) {
        eprintln!("[event-hub] transient session state publish failed: {error}");
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

fn handle_history_rename(
    mut conn: UnixStream,
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
    let _ = writeln!(conn, "{response}");
}

fn handle_workspace_menu(
    mut conn: UnixStream,
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
    let _ = writeln!(conn, "{response}");
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

fn handle_automations_snapshot(
    mut conn: UnixStream,
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
) {
    recover_locked_automations(automations, event_hub);
    let snapshot = webhook::annotate_snapshot(automations.lock().unwrap().snapshot());
    let _ = writeln!(
        conn,
        "{}",
        serde_json::json!({ "ok": true, "automations": snapshot })
    );
}

fn handle_automation_command(
    mut conn: UnixStream,
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
        Ok(applied) => serde_json::json!({
            "ok": true,
            "revision": applied.snapshot.revision,
            "automations": webhook::annotate_snapshot(applied.snapshot),
            "result": applied.result,
        }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(conn, "{response}");
}

fn handle_event_publish(
    mut conn: UnixStream,
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
        Ok(applied) => serde_json::json!({
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
        }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(conn, "{response}");
}

/// 落盘记录不能证明进程还在。handoff 重建 runtime 之后、对外服务之前，按
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

/// 常驻 Term 的事件监听：接住 alacritty 解析出的 `Event::Title`/`Event::Bell`，
/// 写进共享的 `SessionState`，顺带广播给所有 `subscribe` 连接。标题仅更新展示
/// 元数据，绝不能影响 phase 或其它回合状态。
#[derive(Clone)]
struct StateListener {
    state: Arc<Mutex<SessionState>>,
    event_hub: EventHubHandle,
    /// `ColorRequest` 在 `Term` 网格锁内回调。这里只入队，等 PTY 泵释放网格锁后
    /// 再经 `Ctl.master` 写入，避免与 resize 的 ctl -> term 锁序相反而死锁。
    color_replies: PendingColorReplies,
}

type PendingColorReplies = Arc<Mutex<VecDeque<String>>>;

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
                replies.push_back(format(resolve_terminal_color(*index)));
            }
            return;
        }

        let (snapshot, transient) = {
            let Ok(mut st) = self.state.lock() else {
                return;
            };
            let transient = match event {
                Event::Title(t) => {
                    let title_changed = apply_terminal_title(&mut st, &t);
                    if !title_changed {
                        return;
                    }
                    // 标题 spinner 不是会话活动：不改 updated_at，不干扰
                    // “最近会话”排序和空闲时长。
                    true
                }
                Event::Bell => {
                    st.updated_at = now_unix();
                    false
                }
                _ => return,
            };
            bump_state_revision(&mut st);
            (st.clone(), transient)
        };
        if transient {
            broadcast_transient_state(&self.event_hub, &snapshot);
        } else {
            broadcast_state(&self.event_hub, &snapshot);
        }
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
    let (fd, jolt, xpixel, ypixel, marker) = {
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
        (fd, jolt, xpixel, ypixel, marker)
    };
    if let Some(marker) = marker {
        dispatch_session_outputs_under_gate(sess, &marker, "<resize>", true, false);
    }

    if jolt {
        resize_fd(fd, rows.saturating_add(1), cols, xpixel, ypixel);
    }
    resize_fd(fd, rows, cols, xpixel, ypixel);
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
fn set_cloexec(fd: RawFd, on: bool) {
    unsafe {
        let cur = libc::fcntl(fd, libc::F_GETFD);
        if cur >= 0 {
            let new = if on {
                cur | libc::FD_CLOEXEC
            } else {
                cur & !libc::FD_CLOEXEC
            };
            libc::fcntl(fd, libc::F_SETFD, new);
        }
    }
}

/// dup 一个 fd 并包成 File。dup 出的新 fd 默认**不带** CLOEXEC，这里立即补上——
/// 否则它会泄漏进之后 spawn 的每个 shell（占着 PTY master 不放，会话杀不干净）。
fn dup_file(fd: RawFd) -> anyhow::Result<std::fs::File> {
    let d = unsafe { libc::dup(fd) };
    anyhow::ensure!(d >= 0, "dup({fd}) 失败");
    set_cloexec(d, true);
    Ok(unsafe { std::fs::File::from_raw_fd(d) })
}

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
        ..TermConfig::default()
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

fn main() {
    // Grok/CI 会给进程打 NO_COLOR=1。必须在任何线程/子进程之前卸掉，否则
    // 交互式 PTY（agy 等）会继承关色开关，整屏只剩默认灰白。
    smelt_core::tty_color::clear_process();
    // 独立 ACP 会话宿主只跑一条私有控制连接，不能初始化 SQLite、绑定全局
    // smeltd.sock 或启动菜单/远程服务。尽量早分流也避免每个会话宿主各起一套
    // app_log tee 后台线程。
    if acp_runtime_host::is_session_host_process() {
        acp_runtime_host::run_session_host();
        return;
    }
    // 在线升级先 exec 已验证存在的暂存映像；候选映像一旦真的启动，就在任何线程和
    // 状态初始化之前把自身提升到正式路径，再以正式名继续同一份 handoff。
    finalize_staged_handoff_executable();
    pin_running_daemon_image();
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
    if let Some(home) = std::env::var_os("HOME")
        && let Err(e) = std::env::set_current_dir(&home)
    {
        smelt_core::app_log::error(
            "smeltd",
            &format!("启动时 cwd 校正失败（HOME={home:?}）：{e}"),
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
    // 无缝升级交接：上一代进程 exec 本二进制前写好交接文件并把路径放在环境变量里。
    // 立即摘掉环境变量：它只对"本次 exec 交接"有意义，不能传染给之后 spawn 的 shell。
    let handoff = std::env::var("SMELTD_HANDOFF").ok();
    // 交接 v2 import 模式：socketpair 端 fd 号（与 legacy 文件路径互斥，v2 优先）。
    let import_sock = std::env::var("SMELTD_HANDOFF_SOCK")
        .ok()
        .and_then(|fd| fd.parse::<RawFd>().ok());
    let handoff_plugin_daemon_fingerprint = std::env::var("SMELTD_PLUGIN_DAEMON_FINGERPRINT").ok();
    // Edition 2024：`remove_var` 标为 unsafe（多线程改 env 非同步）。此时只有不访问
    // 环境变量的 stderr tee 读线程，尚未启动任何会并发读环境的业务线程。
    unsafe {
        std::env::remove_var("SMELTD_HANDOFF");
        std::env::remove_var("SMELTD_HANDOFF_SOCK");
        std::env::remove_var("SMELTD_PLUGIN_DAEMON_FINGERPRINT");
    }
    let came_from_handoff = handoff.is_some();
    // 进程指纹启动时钉死：env（handoff successor / GUI 拉起传入）优先，否则对
    // current_exe 文件哈希一次。此后进程生命期内不再重哈希磁盘——StageDiskOnly
    // 后磁盘是新的、进程还是老的，重哈希会拿错插件集（与 -67034 同一类耦合）。
    let daemon_fingerprint = pinned_daemon_fingerprint(handoff_plugin_daemon_fingerprint);
    if let Some(sock_fd) = import_sock {
        import_main(sock_fd, daemon_fingerprint);
        return;
    }

    // `exec` 保留直接子进程与已经退出但尚未 wait 的状态，却销毁上一映像的所有
    // reaper 线程。必须在恢复终端/ACP/插件 Host、启动任何新 child owner 之前做一次
    // 有界 WNOHANG sweep；运行期绝不再用 waitpid(-1)，后续全部交给精确 PID owner。
    if came_from_handoff {
        let reaped = reap_inherited_exited_children_at_exec_boundary();
        if reaped > 0 {
            dlog(&format!(
                "upgrade: 启动边界已回收 {reaped} 个上一映像遗留的退出子进程"
            ));
        }
    }

    let path = sock_path();
    // EventHub 的实时连接列表不参与无缝升级交接；每个进程从空列表开始，
    // durable log/cursor 则复用 smelt.sqlite3。
    // 建在 resume_handoff 之前：交接恢复的会话也要把状态投影到同一个 EventHub。
    let event_hub = match event_hub::DaemonEventHub::open_default() {
        Ok(event_hub) => event_hub,
        Err(error) => {
            eprintln!("[event-hub] 初始化失败，守护退出：{error}");
            smelt_core::app_log::error("smeltd", &format!("EventHub 初始化失败：{error}"));
            return;
        }
    };
    // 远程目录不参与 fd handoff：它已经是 daemon 独占的持久化状态，新进程重新打开
    // 即可。读取异常时 fail-closed，绝不把损坏文档当成空目录发布给 GUI。
    let remote_sessions = new_remote_sessions();
    let workspace_menu = new_workspace_menu();
    let (listener, sessions, acp_sessions) = match handoff.and_then(|p| {
        resume_handoff_with_remote(
            &p,
            &event_hub,
            Some(Arc::clone(&remote_sessions)),
            /* legacy_rehome */ true,
        )
    }) {
        Some(x) => {
            let acp_n = x.2.snapshot().len();
            dlog(&format!(
                "upgrade: 交接完成，恢复 {} 个终端会话 + {acp_n} 个 ACP 会话",
                x.1.len()
            ));
            // 交接的会话是活的：空身份目录，由后续广播持续更新落盘。
            *SESSION_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) =
                Some(SessionDirectory::new());
            x
        }
        None => {
            if came_from_handoff {
                dlog("upgrade: 交接文件恢复失败，走全新启动（会话丢失但守护存活）");
            }
            // 单实例检查只在「不是从交接来的」这条路径上做：能连上说明已有活守护，
            // 直接退出。若 came_from_handoff 为真，说明本进程就是刚从上一代 exec
            // 过来的替身——这种情况下绝不能做这个检查：上一代把监听 fd 的 CLOEXEC
            // 清掉了，我们已经继承着它，此时 connect 这个 path 会连上我们自己继承
            // 的那份监听 fd（进 backlog 即成功），于是把「自己」误判成「已有别的
            // 守护」而直接 return 退出——刚交接过来的进程当场自杀，所有会话陪葬。
            // 交接失败时唯一正确的动作是：忽略那份不可追溯的旧监听 fd（它会作为
            // 一个泄漏的 fd 留在本进程里，无害但也无法优雅关闭——resume_handoff
            // 失败通常发生在 JSON 都解析不出来的极端情况，代价可接受），把 socket
            // 文件净空重 bind，保证守护本身不能倒。
            let listener = match bind_fresh_daemon(&path, &handoff_path(), !came_from_handoff) {
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
            // 崩溃恢复：非 handoff 启动时，落盘有内容就说明上次非正常退出
            // （正常 shutdown 会清空）。灌进身份目录，runtime=false，保留最后相位。
            let directory = SessionDirectory::restore_from_disk();
            let recovered = directory.snapshot().len();
            if recovered > 0 {
                dlog(&format!("崩溃恢复：{recovered} 个会话恢复为已断连状态"));
            }
            *SESSION_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) = Some(directory);
            (listener, new_sessions(), new_acp_sessions())
        }
    };

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
        came_from_handoff,
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
        None,
    );
}

/// 交接 v2 import 模式：经 socketpair 收齐→恢复→READY→COMMIT 后直接服务。
/// 任何失败都 exit(2)——predecessor 见 EOF/ABORT 回滚，老进程原地继续服务。
/// 注意与正常 main 的差异：不 bind（认领 fd）、不读落盘身份（全新空目录，
/// 与成功 resume 一致）、插件/sidecar/menubar 全推迟到 COMMIT 之后。
fn import_main(sock_fd: RawFd, daemon_fingerprint: Option<String>) {
    if unsafe { libc::fcntl(sock_fd, libc::F_GETFD) } < 0 {
        dlog("handoff: SMELTD_HANDOFF_SOCK 无效，import 退出");
        std::process::exit(2);
    }
    // CLOEXEC 复位：spawn 时放行过，此后沿用"一切 fd 默认 CLOEXEC"纪律。
    set_cloexec(sock_fd, true);
    // SAFETY：fd 有效性刚校验过，且该号只属于这次 import（predecessor 传参保证）。
    let sock = unsafe { UnixStream::from_raw_fd(sock_fd) };

    // store 照常 open（含迁移——tripwire 在 predecessor 侧守回滚，见
    // store_migrated_since）。open 失败则直接 exit（ABORT 都发不出，
    // predecessor 见 EOF 回滚；store 未被动过，无迁移之忧）。
    let event_hub = match event_hub::DaemonEventHub::open_default() {
        Ok(event_hub) => event_hub,
        Err(error) => {
            eprintln!("[event-hub] import 初始化失败，退出：{error}");
            std::process::exit(2);
        }
    };
    let remote_sessions = new_remote_sessions();
    let workspace_menu = new_workspace_menu();
    *SESSION_DIRECTORY.write().unwrap_or_else(|e| e.into_inner()) = Some(SessionDirectory::new());

    match handoff_v2::successor::run_import(
        &sock,
        &event_hub,
        Some(Arc::clone(&remote_sessions)),
        /* legacy_rehome */ true,
    ) {
        handoff_v2::successor::ImportOutcome::Restored {
            listener,
            sessions,
            acp_sessions,
            ready,
        } => {
            dlog(&format!(
                "handoff: import 接管（终端 {} + ACP {}，丢 grid {:?}）",
                ready.restored_terminals, ready.restored_acp, ready.dropped_grids
            ));
            let automations = new_automation_store();
            reconcile_remote_catalog_runtime(
                &remote_sessions,
                &sessions,
                &acp_sessions,
                &event_hub,
            );
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
                /* came_from_handoff */ true,
            );
            webhook::spawn(Arc::clone(&automations), Arc::clone(&event_hub));
            // 交接 sock 移给 serve loop：插件启动推迟到 predecessor EOF（=老插件
            // 已停 + 老进程已退）之后，避免新旧两套 user 插件同时在线。
            run_serve_loop(
                listener,
                sessions,
                acp_sessions,
                remote_sessions,
                workspace_menu,
                automations,
                event_hub,
                daemon_fingerprint,
                Some(sock),
            );
        }
        handoff_v2::successor::ImportOutcome::Aborted { reason } => {
            dlog(&format!("handoff: import ABORT，退出：{reason}"));
            std::process::exit(2);
        }
    }
}

/// 交接后等 predecessor 退出：它 COMMIT 后先停插件/sidecar 再 drop socket 退出，
/// EOF 即“老插件已停”。10s 兜底（stop 最多 2s grace + KILL，10s 慷慨上限），
/// 超时也起——不能让插件无限等一个 hung 住的老进程。
fn wait_for_predecessor_exit(sock: &UnixStream, timeout: Duration) {
    let _ = sock.set_read_timeout(Some(timeout));
    let mut buf = [0u8; 64];
    let mut sock_ref = sock;
    loop {
        match sock_ref.read(&mut buf) {
            Ok(0) => return,
            Ok(_) => continue,
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                dlog("handoff: 等 predecessor 退出超时（10s），插件先起");
                return;
            }
            // 对端已死/通道坏：效果等同 EOF（老进程没了，老插件随它去了）。
            Err(_) => return,
        }
    }
}

/// headless 空闲自升级：没人看时，把已 stage 的 `smeltd.next` 应用到运行中的守护。
/// 每 60s 看一次（与 GUI 的 watch 同频，首轮延迟 60s，把先手让给 GUI）。
/// 门控：
/// - 指纹未知（None）不升：证明不了自己旧，别乱动。
/// - 有活着的观看连接（桌面 event subscribe / 终端 open·watch / ACP open·watch）
///   不升：GUI 活着，它有空闲门控（agent 忙不闪终端），让它拥有升级时机。
/// - 只 exec `smeltd.next`。正在跑的 `smeltd` 在升级完成前不准被替换，ACP host
///   从 `current_exe()` 派生，必须跟主进程同版。
/// - ACP/peer 忙：不自判，直接发 upgrade op 让 handler 回 busy（单一数据源），
///   等下一轮。
/// 发的是对自己 socket 的 upgrade op，复用同一套事务/回滚/busy 语义；
/// COMMIT 后本进程 exit，线程随之死，无需清理。
fn spawn_headless_self_upgrade(daemon_fingerprint: Option<String>) {
    let Some(pinned) = daemon_fingerprint else {
        return;
    };
    std::thread::Builder::new()
        .name("smelt-headless-self-upgrade".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(60));
                // 有人在看就让 GUI 定点：它的空闲门控比我们懂用户。
                if protocol::viewer_is_present() {
                    continue;
                }
                let Ok(current) = daemon_executable_path() else {
                    continue;
                };
                let Some(target) = staged_successor_executable(&current) else {
                    continue;
                };
                let disk = match smelt_plugin_host::executable_fingerprint(&target) {
                    Ok(fingerprint) => fingerprint,
                    Err(_) => continue,
                };
                if disk == pinned {
                    continue;
                }
                dlog("headless 自升级：已 stage 新映像且没有观看连接，尝试 upgrade");
                match request_self_upgrade(&target) {
                    SelfUpgradeOutcome::Upgraded => return, // COMMIT 后进程即退，defensive
                    SelfUpgradeOutcome::Busy(reason) => {
                        dlog(&format!("headless 自升级 busy，60s 后重试：{reason}"));
                    }
                    SelfUpgradeOutcome::Failed(reason) => {
                        dlog(&format!("headless 自升级失败，60s 后重试：{reason}"));
                    }
                }
            }
        })
        .ok();
}

#[derive(Debug, PartialEq, Eq)]
enum SelfUpgradeOutcome {
    Upgraded,
    Busy(String),
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
    if value["busy"].as_bool() == Some(true) {
        SelfUpgradeOutcome::Busy(reason)
    } else {
        SelfUpgradeOutcome::Failed(reason)
    }
}

fn request_self_upgrade(target: &std::path::Path) -> SelfUpgradeOutcome {
    let sock = match UnixStream::connect(sock_path()) {
        Ok(sock) => sock,
        Err(error) => return SelfUpgradeOutcome::Failed(format!("连自己失败：{error}")),
    };
    let _ = sock.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = sock.set_read_timeout(Some(Duration::from_secs(30)));
    let mut sock = sock;
    if let Err(error) = writeln!(
        sock,
        "{}",
        serde_json::json!({ "op": "upgrade", "exe": target.to_string_lossy() })
    ) {
        return SelfUpgradeOutcome::Failed(format!("发 upgrade 失败：{error}"));
    }
    let mut reply = String::new();
    if let Err(error) = BufReader::new(&sock).read_line(&mut reply) {
        return SelfUpgradeOutcome::Failed(format!("读回包失败：{error}"));
    }
    classify_self_upgrade_reply(&reply)
}

/// 服务主循环：远端自愈→bun→插件→accept（+macOS 菜单栏主线程编排）。
/// 正常启动与 v2 import 共用；import 在 COMMIT 之后调这里。
/// `handoff_sock`：Some=刚交接过来，插件启动推迟到 predecessor EOF 之后；
/// None=正常启动，插件立即起。
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
    handoff_sock: Option<UnixStream>,
) {
    let listen_fd = listener.as_raw_fd();
    let exe_mtime = exe_mtime_secs();
    // 不参与无缝升级交接：每次进程启动（含 upgrade 后的新进程）都是全新的 None，
    // 见 RemoteGateway / IrohTunnel 定义处注释。运行态丢了不等于用户意愿丢了——
    // 下面的 autostart_remote_from_config 会按远程配置快照把它们拉回来。
    let remote_state = new_remote_state(None);
    let iroh_state: IrohState = Arc::new(Mutex::new(None));
    // 全局连接池，由 iroh 隧道回调更新，供 iroh_connections op 查询。
    let iroh_connections = new_iroh_connections();
    // acp_sessions 现在参与无缝升级交接了（见上面 resume_handoff 的返回值）：
    // 正常冷启动时是空表，upgrade 交接恢复时带着接过来的会话。
    // 菜单栏 quit / 任何路径 cleanup 都要够得着这两份状态。
    register_lifecycle(Arc::clone(&remote_state), Arc::clone(&iroh_state));

    // exec 活下来的同名断言没有 Rust 对象可 Drop，必须在自愈拉起之前清掉。
    #[cfg(target_os = "macos")]
    SystemSleepAssertion::release_orphans();

    // 远程访问自愈：见 autostart_remote_from_config 的注释。必须在 accept 循环之前
    // 挂起（它自己起线程，不阻塞），否则守护重启后手机要一直等到用户下次开 GUI。
    autostart_remote_from_config(
        Arc::clone(&remote_state),
        Arc::clone(&iroh_state),
        Arc::clone(&iroh_connections),
    );

    // 受管 bun 跟 helper 一样是版本单元：锁定版本变了由守护代用户下载并清旧目录，
    // 不堵 accept 循环。ACP 启动路径会再 ensure 一次（同目录锁串行）。
    // 受管 bun 既是 ACP 适配器的运行时，也是脚本插件的运行时。首次下载完成时插件集
    // 可能已经按"没有 bun"起过一轮了，所以下完要把它重新拉起来。
    std::thread::spawn(|| {
        let had_bun = smelt_core::acp_conn::managed_bun_if_ready().is_some();
        match smelt_core::acp_conn::sync_managed_bun(&|message| {
            smelt_core::app_log::info("bun", message)
        }) {
            Ok(path) => {
                smelt_core::app_log::info("bun", &format!("受管 bun 已就绪：{}", path.display()));
                if !had_bun {
                    plugin_runtime::reload_for_runtime_change();
                }
            }
            Err(error) => {
                smelt_core::app_log::warn("bun", &format!("同步受管 bun 失败：{error}"));
            }
        }
    });

    // 插件启动：正常启动立即起；交接过来则等 predecessor EOF（=老插件已停
    // +老进程已退）再起，避免新旧两套 user 插件同时在线。终端/ACP 不等——
    // 它们已经恢复，accept 立刻开始服务。
    match handoff_sock {
        None => plugin_runtime::start(daemon_fingerprint.clone()),
        Some(sock) => {
            let fingerprint = daemon_fingerprint.clone();
            std::thread::Builder::new()
                .name("smelt-handoff-plugin-wait".into())
                .spawn(move || {
                    wait_for_predecessor_exit(&sock, Duration::from_secs(10));
                    plugin_runtime::start(fingerprint);
                })
                .ok();
        }
    }

    // headless 空闲自升级：没有观看连接时磁盘新了没人触发 upgrade，这里兜底。
    // GUI 连着时它 60s 探一次、有空闲门控，让它拥有升级时机（见函数注释）。
    spawn_headless_self_upgrade(daemon_fingerprint.clone());

    // thread-per-connection 的 accept 主循环。抽成闭包，好让主线程在 macOS 上腾出来
    // 跑菜单栏 runloop——AppKit 铁律：NSApplication/NSStatusItem 只能在主线程摸。
    let accept_loop = move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let sessions = Arc::clone(&sessions);
            let acp_sessions = Arc::clone(&acp_sessions);
            let remote_state = Arc::clone(&remote_state);
            let iroh_state = Arc::clone(&iroh_state);
            let iroh_connections = Arc::clone(&iroh_connections);
            let event_hub = Arc::clone(&event_hub);
            let remote_sessions = Arc::clone(&remote_sessions);
            let workspace_menu = Arc::clone(&workspace_menu);
            let automations = Arc::clone(&automations);
            let daemon_fingerprint = daemon_fingerprint.clone();
            thread::spawn(move || {
                handle_conn(
                    conn,
                    ServerContext {
                        sessions,
                        acp_sessions,
                        exe_mtime,
                        daemon_fingerprint,
                        listen_fd,
                        remote_state,
                        iroh_state,
                        iroh_connections,
                        event_hub,
                        remote_sessions,
                        workspace_menu,
                        automations,
                    },
                )
            });
        }
        plugin_runtime::stop();
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
        return;
    }

    accept_loop();
}

/// 交接文件路径（跟 socket 同目录）。
fn handoff_path() -> std::path::PathBuf {
    sock_path().with_file_name("handoff.json")
}

#[derive(Debug, PartialEq, Eq)]
struct OwnedAcpHandoff {
    pid: i32,
    stdin_fd: RawFd,
    stdout_fd: RawFd,
}

#[derive(Debug)]
struct ValidatedAcpHandoff {
    id: String,
    owned: OwnedAcpHandoff,
    snapshot: smelt_core::acp_session::ConversationSnapshot,
    acp_session_id: String,
    cwd: Option<String>,
    launch: smelt_core::agent_kind::ConversationLaunchSpec,
    agent_needs_transcript_check: bool,
    pending_raw_line: Option<String>,
    conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
}

#[derive(Debug, PartialEq, Eq)]
struct OwnedHostedAcpHandoff {
    host_pid: i32,
    host_fd: RawFd,
    provider_pid: Option<i32>,
}

#[derive(Debug)]
struct ValidatedHostedAcpHandoff {
    id: String,
    owned: OwnedHostedAcpHandoff,
    snapshot: smelt_core::acp_session::ConversationSnapshot,
    host_snapshot_revision: u64,
    cwd: Option<String>,
    launch: smelt_core::agent_kind::ConversationLaunchSpec,
    runtime_spec_fingerprint: Option<String>,
    agent_mcp: bool,
    agent_token: String,
    agent_needs_transcript_check: bool,
    conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
}

fn acp_turn_is_active(
    phase: smelt_core::daemon_state::DaemonPhase,
    turn_started_at_ms: Option<u64>,
    replaying_history: bool,
    entries: &[smelt_core::acp_chat::AcpEntry],
) -> bool {
    !replaying_history
        && ((matches!(phase, smelt_core::daemon_state::DaemonPhase::Thinking)
            && turn_started_at_ms.is_some())
            || smelt_core::acp_chat::has_unfinished_tool_call(entries))
}

#[cfg(test)]
fn snapshot_has_active_turn(snapshot: &smelt_core::acp_session::ConversationSnapshot) -> bool {
    acp_turn_is_active(
        snapshot.phase,
        snapshot.turn_started_at_ms,
        snapshot.replaying_history,
        &snapshot.entries,
    )
}

fn state_has_active_turn(state: &smelt_core::acp_session::AcpSessionState) -> bool {
    acp_turn_is_active(
        state.phase,
        state.turn_started_at_ms,
        state.replaying_history,
        &state.entries,
    )
}

/// `Restore` 携带完整的接管快照（含 fd 与会话状态），用 Box 避免把每个校验结果都
/// 扩成 700 多字节；这条冷启动交接路径的一次分配不在热循环里。
enum AcpHandoffItemValidation {
    SkipUnowned,
    CloseDescriptors { stdin_fd: RawFd, stdout_fd: RawFd },
    CleanupRequired(OwnedAcpHandoff),
    Restore(Box<ValidatedAcpHandoff>),
}

enum HostedAcpHandoffItemValidation {
    SkipUnowned,
    CloseDescriptor { host_fd: RawFd },
    CleanupRequired(OwnedHostedAcpHandoff),
    Restore(Box<ValidatedHostedAcpHandoff>),
}

fn owned_process_group_cleanup_pid(pid: i32) -> Option<i32> {
    (pid > 1).then_some(pid)
}

fn launch_spec_from_handoff_item(
    item: &serde_json::Value,
) -> smelt_core::agent_kind::ConversationLaunchSpec {
    item.get("launch")
        .cloned()
        .and_then(|value| {
            serde_json::from_value::<smelt_core::agent_kind::ConversationLaunchSpec>(value).ok()
        })
        .filter(|launch| !launch.command.trim().is_empty())
        .unwrap_or_else(|| {
            smelt_core::agent_kind::ConversationLaunchSpec::from_command(
                item["cmd"].as_str().unwrap_or_default(),
            )
        })
}

fn validate_acp_handoff_item(
    item: &serde_json::Value,
    fd_is_valid: impl Fn(RawFd) -> bool,
) -> AcpHandoffItemValidation {
    let Some(stdin_fd) = item["stdin_fd"]
        .as_i64()
        .and_then(|fd| RawFd::try_from(fd).ok())
        .filter(|fd| *fd >= 0)
    else {
        return AcpHandoffItemValidation::SkipUnowned;
    };
    let Some(stdout_fd) = item["stdout_fd"]
        .as_i64()
        .and_then(|fd| RawFd::try_from(fd).ok())
        .filter(|fd| *fd >= 0)
    else {
        return AcpHandoffItemValidation::SkipUnowned;
    };
    if !fd_is_valid(stdin_fd) || !fd_is_valid(stdout_fd) {
        return AcpHandoffItemValidation::SkipUnowned;
    }

    let Some(pid) = item["pid"].as_i64().and_then(|pid| i32::try_from(pid).ok()) else {
        return AcpHandoffItemValidation::CloseDescriptors {
            stdin_fd,
            stdout_fd,
        };
    };
    let Some(pid) = owned_process_group_cleanup_pid(pid) else {
        return AcpHandoffItemValidation::CloseDescriptors {
            stdin_fd,
            stdout_fd,
        };
    };
    let owned = OwnedAcpHandoff {
        pid,
        stdin_fd,
        stdout_fd,
    };

    let Some(id) = item["id"].as_str() else {
        return AcpHandoffItemValidation::CleanupRequired(owned);
    };
    let Some(snapshot_v) = item.get("snapshot") else {
        return AcpHandoffItemValidation::CleanupRequired(owned);
    };
    let Ok(snapshot) =
        serde_json::from_value::<smelt_core::acp_session::ConversationSnapshot>(snapshot_v.clone())
    else {
        return AcpHandoffItemValidation::CleanupRequired(owned);
    };
    let Some(acp_session_id) = snapshot.acp_session_id.clone() else {
        return AcpHandoffItemValidation::CleanupRequired(owned);
    };

    AcpHandoffItemValidation::Restore(Box::new(ValidatedAcpHandoff {
        id: id.to_string(),
        owned,
        snapshot,
        acp_session_id,
        cwd: item["cwd"].as_str().map(String::from),
        launch: launch_spec_from_handoff_item(item),
        agent_needs_transcript_check: item["agent_needs_transcript_check"]
            .as_bool()
            .unwrap_or(false),
        pending_raw_line: item["pending_raw_line"].as_str().map(String::from),
        conversation_binding: item
            .get("conversation_binding")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    }))
}

fn validate_hosted_acp_handoff_item(
    item: &serde_json::Value,
    fd_is_valid: impl Fn(RawFd) -> bool,
) -> HostedAcpHandoffItemValidation {
    let Some(host_fd) = item["host_fd"]
        .as_i64()
        .and_then(|fd| RawFd::try_from(fd).ok())
        .filter(|fd| *fd >= 0)
    else {
        return HostedAcpHandoffItemValidation::SkipUnowned;
    };
    if !fd_is_valid(host_fd) {
        return HostedAcpHandoffItemValidation::SkipUnowned;
    }
    let Some(host_pid) = item["host_pid"]
        .as_i64()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(owned_process_group_cleanup_pid)
    else {
        return HostedAcpHandoffItemValidation::CloseDescriptor { host_fd };
    };
    let provider_pid = item["provider_pid"]
        .as_i64()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(owned_process_group_cleanup_pid);
    let owned = OwnedHostedAcpHandoff {
        host_pid,
        host_fd,
        provider_pid,
    };

    let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
        return HostedAcpHandoffItemValidation::CleanupRequired(owned);
    };
    let Some(snapshot_value) = item.get("snapshot") else {
        return HostedAcpHandoffItemValidation::CleanupRequired(owned);
    };
    let Ok(snapshot) = serde_json::from_value(snapshot_value.clone()) else {
        return HostedAcpHandoffItemValidation::CleanupRequired(owned);
    };
    let agent_token = item["agent_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(String::from)
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let agent_mcp = item["agent_mcp"].as_bool().unwrap_or(false)
        && item["agent_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty());

    HostedAcpHandoffItemValidation::Restore(Box::new(ValidatedHostedAcpHandoff {
        id: id.to_string(),
        owned,
        snapshot,
        host_snapshot_revision: item["host_snapshot_revision"].as_u64().unwrap_or(0),
        cwd: item["cwd"].as_str().map(String::from),
        launch: launch_spec_from_handoff_item(item),
        runtime_spec_fingerprint: item["runtime_spec_fingerprint"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(String::from),
        agent_mcp,
        agent_token,
        agent_needs_transcript_check: item["agent_needs_transcript_check"]
            .as_bool()
            .unwrap_or(false),
        conversation_binding: item
            .get("conversation_binding")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    }))
}

fn waitpid_retry(pid: i32, options: i32) -> i32 {
    loop {
        let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), options) };
        if waited >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return waited;
        }
    }
}

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

/// 不消费退出状态地确认 PID 仍是本进程的直接子进程。恢复损坏/陈旧 handoff 时，
/// ECHILD 必须先变成惰性的 AlreadyReaped owner，不能让稍后的清理路径误杀同号进程。
fn exact_child_is_waitable(pid: i32) -> std::io::Result<bool> {
    match waitid_exact_child(pid, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => Ok(false),
        Err(error) => Err(error),
    }
}

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

/// `exec` 交接会保留 daemon PID 与父子关系，却会丢掉上一映像中的回收线程。
/// 这里只允许在新映像尚未恢复任何 child owner 的独占启动边界调用：此刻旧线程已随
/// `exec` 消失，新 waiter 尚未建立，因此不会抢 ACP、插件 Host 或终端的退出状态。
/// 存活子进程由 WNOHANG 原样保留，稍后交回各自的精确 PID owner。
fn reap_inherited_exited_children_at_exec_boundary() -> usize {
    let mut reaped = 0;
    loop {
        let waited = waitpid_retry(-1, libc::WNOHANG);
        if waited > 0 {
            reaped += 1;
            continue;
        }
        if waited == 0 {
            break;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ECHILD) => break,
            error => {
                dlog(&format!("upgrade: 启动边界清理遗留子进程失败：{error:?}"));
                break;
            }
        }
    }
    reaped
}

/// 交接清理动刀前的身份门：pid 启动早于快照时刻才是原进程。复用只能发生在
/// 原进程死后、即快照之后（因果律），晚于此时刻的一律陌生。
/// 读不出/晚于/未知一律跳过击杀（fail closed）：fd 照关，进程不动。漏杀最多留
/// 个拿着 EOF 管道的孤儿（多半自退），误杀却是 SIGKILL 无辜进程组。
///
/// 戳记信任：v2 经私有 socketpair 来（前任即本二进制，可信）；legacy 文件与
/// 目录同信任域（能写目录的本来就能换守护二进制，见 auth 威胁模型——门只防
/// pid 复用事故，不防恶意文件）。
fn handoff_pid_predates_snapshot(pid: i32, snapshot_wall: std::time::SystemTime) -> bool {
    owned_process_group_cleanup_pid(pid).is_some()
        && smelt_core::acp_conn::process_start_time(pid)
            .is_some_and(|started| started < snapshot_wall)
}

/// 从恢复载荷读快照时刻（前任盖章）。缺失/为零（legacy 老文件/坏帧）回退到
/// 恢复入口时刻：legacy 是 exec-self（亲缘不断，门近乎恒过），坏帧则宁可多验。
fn snapshot_wall_from_value(v: &serde_json::Value) -> std::time::SystemTime {
    v["snapshot_wall_ms"]
        .as_u64()
        .filter(|&ms| ms > 0)
        .and_then(|ms| std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(ms)))
        .unwrap_or_else(std::time::SystemTime::now)
}

fn cleanup_rejected_acp_handoff(owned: OwnedAcpHandoff, snapshot_wall: std::time::SystemTime) {
    unsafe {
        libc::close(owned.stdin_fd);
        libc::close(owned.stdout_fd);
    }

    let Some(pid) = owned_process_group_cleanup_pid(owned.pid) else {
        return;
    };
    if !handoff_pid_predates_snapshot(pid, snapshot_wall) {
        dlog(&format!(
            "handoff: ACP pid={pid} 验明非原进程（已复用/读不出），跳过击杀"
        ));
        return;
    }
    if !smelt_core::acp_conn::kill_and_reap_process_group(pid, Duration::from_secs(1)) {
        dlog(&format!("handoff: ACP pid={pid} 未能验证死亡"));
    }
}

fn cleanup_rejected_hosted_acp_handoff(
    owned: OwnedHostedAcpHandoff,
    snapshot_wall: std::time::SystemTime,
) {
    unsafe {
        libc::close(owned.host_fd);
    }
    cleanup_hosted_acp_processes(owned.host_pid, owned.provider_pid, snapshot_wall);
}

fn cleanup_hosted_acp_processes(
    host_pid: i32,
    provider_pid: Option<i32>,
    snapshot_wall: std::time::SystemTime,
) {
    // provider 是独立进程组（宿主 spawn 时 process_group(0)），必须单独杀；
    // 关 fd 在先，宿主若活着会自己优雅收尾，SIGKILL 只是加速+兜底宿主已死。
    if let Some(provider_pid) = provider_pid.and_then(owned_process_group_cleanup_pid) {
        if !handoff_pid_predates_snapshot(provider_pid, snapshot_wall) {
            dlog(&format!(
                "handoff: ACP provider pid={provider_pid} 验明非原进程，跳过击杀"
            ));
        } else if !smelt_core::acp_conn::kill_and_reap_process_group(
            provider_pid,
            Duration::from_secs(2),
        ) {
            dlog(&format!(
                "handoff: ACP provider pid={provider_pid} 未能验证死亡"
            ));
        }
    }
    if !handoff_pid_predates_snapshot(host_pid, snapshot_wall) {
        dlog(&format!(
            "handoff: ACP session host pid={host_pid} 验明非原进程，跳过击杀"
        ));
        return;
    }
    if !smelt_core::acp_conn::kill_and_reap_process_group(host_pid, Duration::from_secs(2)) {
        dlog(&format!(
            "handoff: ACP session host pid={host_pid} 未能验证死亡"
        ));
    }
}

fn resume_hosted_acp_handoff_item(
    validated: ValidatedHostedAcpHandoff,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
    snapshot_wall: std::time::SystemTime,
) {
    let ValidatedHostedAcpHandoff {
        id,
        owned,
        snapshot,
        host_snapshot_revision,
        cwd,
        launch,
        runtime_spec_fingerprint,
        agent_mcp,
        agent_token,
        agent_needs_transcript_check,
        conversation_binding,
    } = validated;
    let snapshot_revision = snapshot.snapshot_revision;
    let pending_agent_preset = snapshot
        .conversation_state
        .as_ref()
        .and_then(|state| state.pending_agent_preset.clone());
    let agent_session = snapshot
        .conversation_state
        .as_ref()
        .and_then(|state| state.agent_session.clone());
    let mut reduced = smelt_core::acp_session::AcpSessionState::default();
    if let Err(error) = reduced.merge_hosted_snapshot(snapshot) {
        dlog(&format!("handoff: ACP 宿主镜像 {id} 无效：{error}"));
        cleanup_rejected_hosted_acp_handoff(owned, snapshot_wall);
        return;
    }
    let prompt_in_flight = reduced.turn_started_at_ms.is_some()
        || matches!(
            reduced.phase,
            smelt_core::daemon_state::DaemonPhase::Thinking
                | smelt_core::daemon_state::DaemonPhase::ExecutingTool
                | smelt_core::daemon_state::DaemonPhase::AwaitingApproval
                | smelt_core::daemon_state::DaemonPhase::WaitingForUser
        );
    let owner_ids = live_acp_owner_ids(&reduced);
    let instance = next_session_instance();
    let state = Arc::new(Mutex::new(SessionState {
        id: id.clone(),
        instance,
        cwd: cwd.clone(),
        launch: Some(launch.command.clone()),
        agent_mcp,
        agent_token,
        ..Default::default()
    }));
    let launch_spec = Mutex::new(Some(launch));
    let (slot, created) = acp_sessions.reserve_with(&id, || AcpSession {
        instance,
        reduced: Mutex::new(reduced),
        snapshot_revision: AtomicU64::new(snapshot_revision),
        connection_generation: AtomicU64::new(0),
        turn_completion: Mutex::new(()),
        prompt_in_flight: AtomicBool::new(prompt_in_flight),
        pending_prompts: Mutex::new(VecDeque::new()),
        hosted_handle: Mutex::new(None),
        host_snapshot_revision: AtomicU64::new(host_snapshot_revision),
        handle: Mutex::new(None),
        unreaped_pid: Mutex::new(None),
        cwd,
        agent_needs_transcript_check,
        state,
        output_gate: Mutex::new(()),
        out: Mutex::new(AcpOut {
            client: None,
            watchers: Vec::new(),
        }),
        launch_spec,
        runtime_spec_fingerprint: Mutex::new(runtime_spec_fingerprint),
        restore_state: Mutex::new(AcpRestoreState::Fresh),
        ephemeral_env: Mutex::new(BTreeMap::new()),
        conversation_binding: Mutex::new(conversation_binding),
        agent_session: Mutex::new(agent_session),
        conversation_submit: Mutex::new(()),
        pending_agent_preset: Mutex::new(pending_agent_preset),
    });
    if !created {
        cleanup_rejected_hosted_acp_handoff(owned, snapshot_wall);
        return;
    }
    if let Err(error) = acp_sessions.try_acquire_resumes(&owner_ids, &id) {
        eprintln!("[acp] 拒绝宿主 handoff 会话 {id}：{error}");
        acp_sessions.remove_if_same(&id, &slot);
        cleanup_rejected_hosted_acp_handoff(owned, snapshot_wall);
        return;
    }

    let OwnedHostedAcpHandoff {
        host_pid,
        host_fd,
        provider_pid,
    } = owned;
    let hosted = match unsafe {
        acp_runtime_host::HostedConversationHandle::from_handoff(host_pid, host_fd, provider_pid)
    } {
        Ok(hosted) => hosted,
        Err(error) => {
            dlog(&format!("handoff: ACP session host {id} 接管失败：{error}"));
            acp_sessions.remove_if_same(&id, &slot);
            cleanup_hosted_acp_processes(host_pid, provider_pid, snapshot_wall);
            return;
        }
    };
    let snapshot_rx = hosted.snapshot_rx();
    *slot.value.hosted_handle.lock().unwrap() = Some(hosted);
    let refresh_ok = slot
        .value
        .hosted_handle
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|host| host.request_full_snapshot().is_ok());
    if !refresh_ok {
        dlog(&format!("handoff: ACP session host {id} 无法请求全量快照"));
        let _ = retire_acp_runtime(&slot.value);
        acp_sessions.remove_if_same(&id, &slot);
        return;
    }
    update_acp_daemon_state(&slot.value, event_hub);
    start_hosted_snapshot_drain(
        slot,
        snapshot_rx,
        event_hub.clone(),
        id,
        Arc::clone(acp_sessions),
        0,
    );
}

/// 交接恢复时能否安全地把遗留的直连 ACP runtime 搬进独立宿主：只允许在完全
/// 静默的边界上动手——没有进行中的回合、没有待处理的审批/追问、没有半行未解析
/// 的输入，且重启命令记得住。任何一条不满足就维持原样，宁可下一轮再迁，也不能
/// 在活跃会话上杀连接。
///
/// 这里是纯函数：不读全局态、不看构建配置，因此六个条件都能被单测逐条覆盖。
fn legacy_rehome_is_safe(
    snapshot: &smelt_core::acp_session::ConversationSnapshot,
    pending_raw_line: Option<&str>,
    cmd: &str,
) -> bool {
    snapshot.phase == smelt_core::daemon_state::DaemonPhase::Idle
        && snapshot.turn_started_at_ms.is_none()
        && snapshot.pending_permissions.is_empty()
        && snapshot.pending_elicitation.is_none()
        && pending_raw_line.is_none()
        && !cmd.trim().is_empty()
}

/// `legacy_rehome`：是否执行上面那步迁移。它会杀掉旧连接并重新 spawn 宿主，
/// 属于有副作用的动作，所以由调用方显式选择，而不是靠 `cfg!(test)` 猜构建
/// 类型——那样测试里这条分支按构造永远不可达，等于没有覆盖。
fn resume_handoff_with_remote(
    path: &str,
    event_hub: &EventHubHandle,
    remote_sessions: Option<RemoteSessions>,
    legacy_rehome: bool,
) -> Option<(UnixListener, Sessions, AcpSessions)> {
    // legacy 文件入口（一代兼容）：读文件 → Value → 恢复核心。
    // 交接 v2 走 socket + typed manifest，经 bridge 调同一恢复核心。
    let data = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path); // 读到手就删，避免残留被下次启动误认
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    resume_from_value(
        &v,
        &handoff_v2::successor::ResumeOptions::legacy(),
        event_hub,
        remote_sessions,
        legacy_rehome,
    )
}

/// 恢复核心：legacy 文件与 v2 socket 共用。`opts` 只带两处 v2 差异
/// （收养监控器 + grid 字节侧通道），其余逻辑逐行一致，旧单测原样覆盖。
pub(crate) fn resume_from_value(
    v: &serde_json::Value,
    opts: &handoff_v2::successor::ResumeOptions,
    event_hub: &EventHubHandle,
    remote_sessions: Option<RemoteSessions>,
    legacy_rehome: bool,
) -> Option<(UnixListener, Sessions, AcpSessions)> {
    // 清理击杀的身份基准：只读一次（恢复入口时刻兜底），全程传参。
    let snapshot_wall = snapshot_wall_from_value(v);
    let listen_fd = v["listen_fd"].as_i64()? as RawFd;
    // 校验这个 fd 真的有效（exec 前若忘了清 CLOEXEC，这里会拿到无效 fd）。
    if unsafe { libc::fcntl(listen_fd, libc::F_GETFD) } < 0 {
        return None;
    }
    set_cloexec(listen_fd, true);
    let listener = unsafe { UnixListener::from_raw_fd(listen_fd) };

    #[cfg(target_os = "macos")]
    {
        // 损坏 handoff 若把终端/ACP PID 同时写成菜单 GUI，终端/ACP 的领域 owner
        // 优先，避免两个模块竞争同一个退出状态。
        let mut reserved_pids = HashSet::new();
        for item in v["sessions"]
            .as_array()
            .map(|items| items.as_slice())
            .unwrap_or_default()
        {
            let child_needs_reaper = item["child_needs_reaper"].as_bool().unwrap_or(true);
            if child_needs_reaper
                && let Some(pid) = item["pid"]
                    .as_i64()
                    .map(|pid| pid as i32)
                    .filter(|pid| *pid > 1)
            {
                reserved_pids.insert(pid);
            }
        }
        for item in v["acp_sessions"]
            .as_array()
            .map(|items| items.as_slice())
            .unwrap_or_default()
        {
            for field in ["pid", "host_pid", "provider_pid"] {
                if let Some(pid) = item[field]
                    .as_i64()
                    .map(|pid| pid as i32)
                    .filter(|pid| *pid > 1)
                {
                    reserved_pids.insert(pid);
                }
            }
        }
        let menu_gui_pids: Vec<_> = v["menu_gui_pids"]
            .as_array()
            .map(|items| items.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|pid| pid.as_i64().map(|pid| pid as i32))
            .filter(|pid| !reserved_pids.contains(pid))
            .collect();
        // 交接 v2 的收养 pid 在 reaper 首轮即 ECHILD、自清出集合——语义正确：
        // 非亲生进程本就无 wait 义务，launchd 会回收；无需 adopted 分支。
        if let Err(error) = menubar::restore_child_pids(&menu_gui_pids) {
            dlog(&format!("handoff: 恢复菜单 GUI child owner 失败：{error}"));
        }
    }

    let sessions = new_sessions();
    // 损坏 handoff 可能把同一 pid 写进多个条目；按 pid 复用 owner，确保即便输入异常
    // 也永远只有一个线程能 waitpid 这名直接子进程。
    let mut terminal_children: HashMap<i32, Arc<TerminalChild>> = HashMap::new();
    let mut claimed_terminal_pids = HashSet::new();
    for item in v["sessions"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
    {
        let Some(id) = item["id"].as_str() else {
            continue;
        };
        let fd = item["fd"].as_i64().unwrap_or(-1) as RawFd;
        let pid = item["pid"].as_i64().unwrap_or(0) as i32;
        // 旧 handoff 没有这个字段；当时终端只在 PTY EOF 后 wait，因此只要会话还在，
        // 裸 PID 的退出状态就仍归 daemon 所有。缺字段按 true 才兼容旧版升级。
        let child_needs_reaper = item["child_needs_reaper"].as_bool().unwrap_or(true);
        if pid <= 0 {
            // fd 有效但 pid 信息坏了：没法按 pid 去 waitpid/kill 这个孤儿 shell，
            // 干脆关掉 master fd——PTY 挂断会让前台进程组收到 SIGHUP，大概率跟着
            // 退出；不关的话这个 fd 就白白泄漏在新进程里，永远够不着。
            if fd >= 0 && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
                unsafe {
                    libc::close(fd);
                }
            }
            continue;
        }
        let child = if !child_needs_reaper {
            TerminalChild::finished(pid, TerminalChildExit::AlreadyReaped)
        } else if let Some(child) = terminal_children.get(&pid) {
            Arc::clone(child)
        } else if let Some(monitor) = opts.adopted_monitor() {
            // 交接 v2：pid 已重定父到 launchd，不能 waitpid，经监控器收养。
            // adopt() 内部 fail-closed（infallible），无需错误分支。
            let child = TerminalChild::adopt(pid, monitor);
            terminal_children.insert(pid, Arc::clone(&child));
            child
        } else {
            let child = match TerminalChild::restore(pid) {
                Ok(child) => child,
                Err(error) => {
                    dlog(&format!(
                        "handoff: 无法为终端 pid={pid} 建立唯一 reaper：{error}"
                    ));
                    if fd >= 0 && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
                        unsafe {
                            libc::close(fd);
                        }
                    }
                    continue;
                }
            };
            terminal_children.insert(pid, Arc::clone(&child));
            child
        };
        if fd < 0 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            continue; // fd 本身缺失/已失效，没有可恢复的东西
        }
        if child_needs_reaper && claimed_terminal_pids.contains(&pid) {
            // 一个直接 shell 只能属于一个恢复会话。重复 PID 的后续条目只关自己的 fd；
            // 不能让两个 Session 共用同一个 kill/reap owner。
            dlog(&format!("handoff: 跳过重复终端 pid={pid} 的后续条目"));
            unsafe {
                libc::close(fd);
            }
            continue;
        }
        set_cloexec(fd, true);
        let master = unsafe { std::fs::File::from_raw_fd(fd) };
        if set_fd_nonblocking(master.as_raw_fd(), true).is_err() {
            drop(master);
            continue;
        }
        let Ok(reader) = master.try_clone() else {
            // master 已被 from_raw_fd 接管，这里 drop 会关掉 fd（PTY 挂断，shell
            // 大概率收到 SIGHUP 退出）；精确 PID owner 已经在等，循环结束后统一
            // 清理由任何有效条目都没有认领的 child，不能在这里误杀稍后的同 PID 条目。
            drop(master);
            continue;
        };
        let cols = item["cols"].as_u64().unwrap_or(80) as u16;
        let rows = item["rows"].as_u64().unwrap_or(24) as u16;
        let cwd = item["cwd"].as_str().map(String::from);
        let launch = item["launch"].as_str().map(String::from);
        let agent_token = item["agent_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let agent_mcp = item["agent_mcp"].as_bool().unwrap_or(false)
            && item["agent_token"]
                .as_str()
                .is_some_and(|token| !token.is_empty());
        // handoff 文件理论上不会有重复 ID，但它属于可损坏的外部输入。沿用正常
        // open 的 reserve/commit 协议，后一个条目绝不能覆盖已经恢复的 runtime。
        let (slot, created) = sessions.reserve(id);
        if !created {
            dlog(&format!("handoff: 跳过重复终端会话 id={id}"));
            drop(reader);
            drop(master);
            continue;
        }
        let lifecycle = slot.lifecycle.lock().unwrap();
        let alt_flag = item["alt_screen"].as_bool().unwrap_or(false);
        // 旧 handoff 文件可能仍带 "buf"（环形原始字节）——**忽略，永不 feed**。
        // 状态通道不参与交接：新进程里全新一份 SessionState（launch 会写回，便于
        // snapshot 识别 agent）。hook/ACP 很快会补 phase，终端标题会独立更新。
        let instance = next_session_instance();
        let state = Arc::new(Mutex::new(SessionState {
            id: id.to_string(),
            instance,
            cwd: cwd.clone(),
            launch: launch.clone(),
            agent_mcp,
            agent_token,
            ..Default::default()
        }));
        // —— 画面恢复：全会话同一条 grid keyframe 路径，模式由快照自身携带 ——
        //
        // 唯一信源：upgrade 时从常驻 Term 导出的 keyframe（主屏含 history，备用屏为
        // viewport），`grid` 可自洽地 feed 进空 Term。环形字节可能在 CSI 中间腰斩，
        // **永远不 feed**（按类型特判 ring = 拆东墙补西墙）。
        //
        // 无 grid（极老交接文件）：若交接前在备用屏，只注 1049h 模式位；其余空白 + jolt。
        let color_replies = Arc::new(Mutex::new(VecDeque::new()));
        let listener = StateListener::with_color_replies(
            Arc::clone(&state),
            Arc::clone(event_hub),
            Arc::clone(&color_replies),
        );
        let mut term = new_daemon_term(rows, cols, listener);
        // 交接 v2 的 grid 走字节侧通道（免 hex 膨胀）；legacy 文件走 hex 字段。
        // 两边都没有 = "无 grid 空 Term + jolt"，正是 BEST-EFFORT 语义。
        let grid = opts.grid_blob(id).cloned().unwrap_or_else(|| {
            item["grid"]
                .as_str()
                .and_then(hex_decode)
                .unwrap_or_default()
        });
        let was_alt = alt_flag || buf_looks_like_alt_screen(&grid);
        if !grid.is_empty() {
            feed_term(&mut term, &grid);
            dlog(&format!(
                "handoff: 恢复会话 id={id} rows={rows} cols={cols} alt={was_alt} launch={:?} grid_len={} (feed keyframe)",
                launch,
                grid.len()
            ));
        } else if was_alt || alt_flag {
            feed_term(&mut term, b"\x1b[?1049h");
            dlog(&format!(
                "handoff: 恢复会话 id={id} rows={rows} cols={cols} alt=true launch={:?} (无 grid，仅 1049h + jolt)",
                launch
            ));
        } else {
            dlog(&format!(
                "handoff: 恢复会话 id={id} rows={rows} cols={cols} alt=false launch={:?} (无 grid，空 Term + jolt)",
                launch
            ));
        }
        let sess = Arc::new(Session {
            instance,
            geometry_token: uuid::Uuid::new_v4().simple().to_string(),
            child,
            ctl: Mutex::new(Ctl {
                master,
                // 一律 jolt：有 grid 时对齐真 cell 尺寸；无 grid 时逼进程自绘。
                jolt: true,
                cols,
                rows,
                cell_w: 0,
                cell_h: 0,
                remote_viewports: 0,
                remote_grace: 0,
                cwd,
            }),
            input_gate: Mutex::new(()),
            out: Mutex::new(Out {
                clients: Vec::new(),
                watchers: Vec::new(),
            }),
            output_gate: Mutex::new(()),
            color_replies,
            term: Mutex::new(term),
            state,
        });
        if !sessions.commit_if_current(id, &slot, Arc::clone(&sess)) {
            // 这里仅可能是内部不变量被破坏；不要留下 Starting slot 或泄漏 PTY fd。
            let _ = sessions.remove_if_same(id, &slot);
            drop(lifecycle);
            drop(reader);
            drop(sess);
            continue;
        }
        if child_needs_reaper {
            claimed_terminal_pids.insert(pid);
        }
        drop(lifecycle);
        start_pty_pump(
            sess,
            Box::new(reader),
            id.to_string(),
            Arc::clone(&sessions),
            Arc::clone(event_hub),
            remote_sessions.clone(),
            slot,
        );
    }

    // 等全部条目都看完才清理未认领 child：同一 PID 可能先出现在坏 fd 条目、后出现在
    // 有效条目。逐条即时 SIGKILL 会把本来可恢复的会话误杀。这里仍只通知各自的精确
    // owner，不新增 waitpid 竞争者。
    for (pid, child) in &terminal_children {
        if !claimed_terminal_pids.contains(pid)
            && !child.terminate_and_wait(TERMINAL_CHILD_REAP_TIMEOUT)
        {
            dlog(&format!(
                "handoff: 未能确认无主终端 child pid={pid} 已退出并回收"
            ));
        }
    }

    // ACP 会话：fd 裸传跟终端同一招，多一步"回放 pending_raw_line 再接上
    // 实时字节"（见 acp_conn::resume_acp_from_fds），把交接过来的快照数据
    // 重建成活体状态。
    let acp_sessions = new_acp_sessions();
    for item in v["acp_sessions"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
    {
        if item["runtime"].as_str() == Some("hosted") {
            match validate_hosted_acp_handoff_item(item, |fd| unsafe {
                libc::fcntl(fd, libc::F_GETFD) >= 0
            }) {
                HostedAcpHandoffItemValidation::SkipUnowned => {}
                HostedAcpHandoffItemValidation::CloseDescriptor { host_fd } => unsafe {
                    libc::close(host_fd);
                },
                HostedAcpHandoffItemValidation::CleanupRequired(owned) => {
                    cleanup_rejected_hosted_acp_handoff(owned, snapshot_wall);
                }
                HostedAcpHandoffItemValidation::Restore(validated) => {
                    resume_hosted_acp_handoff_item(
                        *validated,
                        &acp_sessions,
                        event_hub,
                        snapshot_wall,
                    );
                }
            }
            continue;
        }
        let validated = match validate_acp_handoff_item(item, |fd| unsafe {
            libc::fcntl(fd, libc::F_GETFD) >= 0
        }) {
            AcpHandoffItemValidation::SkipUnowned => continue,
            AcpHandoffItemValidation::CloseDescriptors {
                stdin_fd,
                stdout_fd,
            } => {
                unsafe {
                    libc::close(stdin_fd);
                    libc::close(stdout_fd);
                }
                continue;
            }
            AcpHandoffItemValidation::CleanupRequired(owned) => {
                cleanup_rejected_acp_handoff(owned, snapshot_wall);
                continue;
            }
            AcpHandoffItemValidation::Restore(validated) => *validated,
        };
        let ValidatedAcpHandoff {
            id,
            owned,
            snapshot,
            acp_session_id,
            cwd,
            launch,
            agent_needs_transcript_check,
            pending_raw_line,
            conversation_binding,
        } = validated;
        let legacy_rehome_safe = legacy_rehome
            && legacy_rehome_is_safe(&snapshot, pending_raw_line.as_deref(), &launch.command);
        let supports_image = snapshot.supports_image;
        let snapshot_revision = snapshot.snapshot_revision;
        let pending_agent_preset = snapshot
            .conversation_state
            .as_ref()
            .and_then(|state| state.pending_agent_preset.clone());
        let agent_session = snapshot
            .conversation_state
            .as_ref()
            .and_then(|state| state.agent_session.clone());
        let reduced = smelt_core::acp_session::AcpSessionState::from_snapshot(snapshot);
        // `from_snapshot` 会收尾已结束回合中永远不会补到的工具终态。必须在
        // 这一步之后再决定是否恢复活跃 RPC；否则 `Idle + Pending tool` 已经
        // 归约为空闲，却仍会把 prompt gate / in-flight RPC 错当作活跃回合。
        let recover_running_turn = state_has_active_turn(&reduced);
        let handoff_owner_ids = live_acp_owner_ids(&reduced);

        let agent_token = item["agent_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let agent_mcp = item["agent_mcp"].as_bool().unwrap_or(false)
            && item["agent_token"]
                .as_str()
                .is_some_and(|token| !token.is_empty());
        let instance = next_session_instance();
        let state = Arc::new(Mutex::new(SessionState {
            id: id.clone(),
            instance,
            cwd: cwd.clone(),
            launch: Some(launch.command.clone()),
            agent_mcp,
            agent_token,
            ..Default::default()
        }));
        let launch_spec = Mutex::new(Some(launch.clone()));
        let (slot, created) = acp_sessions.reserve_with(&id, || AcpSession {
            instance,
            reduced: Mutex::new(reduced),
            snapshot_revision: AtomicU64::new(snapshot_revision),
            connection_generation: AtomicU64::new(0),
            turn_completion: Mutex::new(()),
            prompt_in_flight: AtomicBool::new(recover_running_turn),
            pending_prompts: Mutex::new(VecDeque::new()),
            hosted_handle: Mutex::new(None),
            host_snapshot_revision: AtomicU64::new(0),
            handle: Mutex::new(None),
            unreaped_pid: Mutex::new(None),
            cwd,
            agent_needs_transcript_check,
            state,
            output_gate: Mutex::new(()),
            out: Mutex::new(AcpOut {
                client: None,
                watchers: Vec::new(),
            }),
            launch_spec,
            runtime_spec_fingerprint: Mutex::new(
                item["runtime_spec_fingerprint"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(String::from),
            ),
            restore_state: Mutex::new(AcpRestoreState::Fresh),
            // 无缝升级接到的是仍在运行的子进程，环境已在其进程中生效；不把
            // potentially secret 的网页变量写入 handoff 文件。
            ephemeral_env: Mutex::new(BTreeMap::new()),
            conversation_binding: Mutex::new(conversation_binding),
            agent_session: Mutex::new(agent_session),
            conversation_submit: Mutex::new(()),
            pending_agent_preset: Mutex::new(pending_agent_preset),
        });
        if !created {
            cleanup_rejected_acp_handoff(owned, snapshot_wall);
            continue;
        }
        if let Err(error) = acp_sessions.try_acquire_resumes(&handoff_owner_ids, &id) {
            eprintln!("[acp] 拒绝 handoff 会话 {id}：{error}");
            acp_sessions.remove_if_same(&id, &slot);
            cleanup_rejected_acp_handoff(owned, snapshot_wall);
            continue;
        }

        let OwnedAcpHandoff {
            pid,
            stdin_fd,
            stdout_fd,
        } = owned;
        set_cloexec(stdin_fd, true);
        set_cloexec(stdout_fd, true);
        let drain_id = id.clone();
        let event_rx = {
            let _lifecycle = slot.lifecycle.lock().unwrap();
            let handle = smelt_core::acp_conn::resume_acp_from_fds(
                id,
                smelt_core::acp_conn::ResumedSession {
                    stdin_fd,
                    stdout_fd,
                    pid,
                    acp_session_id,
                    supports_image,
                    pending_raw_line,
                    recover_running_turn,
                },
            );
            let event_rx = handle.event_rx.clone();
            *slot.value.handle.lock().unwrap() = Some(handle);
            event_rx
        };
        if legacy_rehome_safe {
            let resume_id = known_acp_resume_id(&slot.value.reduced.lock().unwrap());
            let resume_ids = resume_id.iter().cloned().collect::<Vec<_>>();
            let _lifecycle = slot.lifecycle.lock().unwrap();
            if retire_acp_runtime(&slot.value) {
                // 旧 daemon 只能在静默边界交出直接 SDK 连接。新 daemon 落地后
                // 立刻把这类遗留 runtime 搬进独立宿主；否则它下一轮一旦活跃，
                // 后续开发安装仍会再次被旧 callback 边界卡住。
                acp_sessions.retain_resumes_for(&drain_id, &resume_ids);
                acp_relaunch(
                    &slot,
                    &drain_id,
                    launch,
                    resume_id,
                    None,
                    None,
                    Arc::clone(&acp_sessions),
                    event_hub,
                );
            } else {
                let mut reduced = slot.value.reduced.lock().unwrap();
                smelt_core::acp_session::force_end(
                    &mut reduced,
                    smelt_core::acp_session::AcpEndKind::ProviderFailed,
                    "旧版 ACP runtime 未能退出，无法迁移到独立宿主",
                );
                drop(reduced);
                push_acp_snapshot_since(&slot.value, true, None);
                update_acp_daemon_state(&slot.value, event_hub);
            }
            continue;
        }
        // 落地就有一份现成快照，不用等下一次协议事件才让 subscribe 订阅者
        // 看到这条会话——跟终端那边"resume 完成靠后续 PTY 输出自然触发广播"
        // 不同，ACP 没有"泵线程闲着也吐字节"这回事。
        update_acp_daemon_state(&slot.value, event_hub);
        start_acp_event_drain(
            slot,
            event_rx,
            event_hub.clone(),
            None,
            drain_id,
            Arc::clone(&acp_sessions),
            0,
        );
    }
    Some((listener, sessions, acp_sessions))
}

/// 把字节喂进常驻 Term；panic 时吞掉，避免畸形序列拖死整个守护。
fn feed_term<T: EventListener>(term: &mut Term<T>, bytes: &[u8]) {
    let mut parser: Processor = Processor::new();
    let _ = catch_unwind(AssertUnwindSafe(|| {
        parser.advance(term, bytes);
    }));
}

fn buf_looks_like_alt_screen(buf: &[u8]) -> bool {
    buf.windows(8).any(|w| w == b"\x1b[?1049h")
}

/// keyframe / 交接 payload 的二进制字段编码（hex，无额外依赖）。
/// 生产侧已不再写 hex（v2 走字节侧通道）；单测搭 legacy fixture 还用得到。
#[cfg(test)]
fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// 按字节解码 hex——交接文件是外部数据（可能损坏/被篡改）；全程字节级 match，
/// 不用 `&s[i..i+2]`，避免非字符边界 panic（resume 时 panic = 全会话陪葬）。
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    (0..b.len())
        .step_by(2)
        .map(|i| Some((nibble(b[i])? << 4) | nibble(b[i + 1])?))
        .collect()
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

/// 无缝升级：交接 v2 双进程事务（快照→spawn successor→传 manifest/fd/grid→
/// READY→COMMIT，COMMIT 前任何失败都回滚、老进程原地继续服务；流程见文件头注释）。
///
/// 锁策略：只短暂持 sessions 锁拿一份 Arc 列表就放掉——不像早期版本那样一直攥到
/// exec，那样会让 open/list/kill/version 在升级期间全部卡在 sessions 锁上。逐会话
/// 先拿 output/input 闸门，再拿 ctl/term/out 做快照；客户端 socket 写入有超时，且
/// 不持有 `ctl` 或 `out` 成员锁进入 kernel，因此冻结 GUI 不会把升级链路永久焊死。
/// （极小残余窗口：某泵线程恰好已 read 出 ≤8KB 还没拿到锁，这部分随 exec 丢失。
/// 丢的只是"显示字节"不是输入；重连后的 jolt 全屏重绘会盖掉，可接受。）
fn handle_upgrade(
    conn: UnixStream,
    req: &serde_json::Value,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
    listen_fd: RawFd,
) {
    let mut c = conn;
    // 可选 `"exe":"/path/to/smeltd"`：spawn 指定二进制做 successor（暂存
    // `.next` 由事务内先扶正到正式路径再 spawn）；未传则 spawn current_exe。
    let exe = if let Some(p) = req["exe"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let path = std::path::PathBuf::from(p);
        if !path.is_file() {
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({ "ok": false, "err": format!("exe 不存在：{}", path.display()) })
            );
            return;
        }
        path
    } else {
        match daemon_executable_path() {
            Ok(p) => p,
            Err(_) => {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "current_exe 失败" })
                );
                return;
            }
        }
    };

    // 从收集任何会话列表/快照之前就冻结全部真实 spawn，并一直持有到 exec 成功
    // （不返回）或失败回滚结束。这样快照里不会漏掉已经 fork、但尚未把 pid/fd
    // 发布到 ConversationHandle.stdio 的 ACP 子进程。
    let _spawn_gate = acquire_upgrade_spawn_gate(&SPAWN_GATE);

    // 跨会话消息投递不能跨 exec；升级期间封住新的总线操作，已经进入投递的消息
    // 完成后再升级，避免 MCP helper 的 socket 响应落到已替换的 daemon 上。
    let _peer_messaging_upgrade = match peer_messaging::begin_upgrade() {
        Ok(guard) => guard,
        Err(active_operations) => {
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({
                    "ok": false,
                    "busy": true,
                    "err": "cross-agent 请求仍在进行，请等待其完成后重试升级",
                    "peer_messaging_operations": active_operations,
                })
            );
            return;
        }
    };

    // 独立 session host 的 SDK future 不穿过 exec，只交接控制 socket，所以活跃
    // 回合不会出现在 blockers。这里的屏障仅服务从旧版 handoff 恢复、尚未迁入
    // host 的 direct-fd 连接；它们静默接管后会立即完成一次性迁移。
    let blockers = acp_upgrade_blockers(acp_sessions);
    if !blockers.is_empty() {
        let _ = writeln!(
            c,
            "{}",
            serde_json::json!({
                "ok": false,
                "busy": true,
                "err": "ACP 会话仍有未完成请求，请在当前回合结束后重试",
                "sessions": blockers,
            })
        );
        return;
    }

    // 交接 v2：快照→typed manifest→spawn successor→传 manifest/fd/grid→
    // READY→COMMIT。COMMIT 发出前任何失败都回滚（reply false，原地继续服务）；
    // 只有库被 successor 迁移过才 exit（结局=旧版最坏情况，显式可观测）。
    let pre_schema = handoff_v2::predecessor::store_schema_before();
    let outcome =
        handoff_v2::predecessor::with_snapshot(sessions, acp_sessions, listen_fd, |snapshot| {
            handoff_v2::predecessor::run_transaction(&exe, &snapshot.staged, event_hub)
        });
    match outcome {
        handoff_v2::predecessor::TransactionOutcome::Committed => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
            std::process::exit(0);
        }
        handoff_v2::predecessor::TransactionOutcome::RolledBack { reason } => {
            if handoff_v2::predecessor::store_migrated_since(pre_schema) {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "store 已被 successor 迁移，守护退出交由拉起逻辑重开新版" })
                );
                dlog(
                    "upgrade: 回滚时发现 store 已迁移，退出（结局=旧版 exec 失败路径，会话丢但守护新）",
                );
                std::process::exit(2);
            }
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": reason }));
        }
    }
}

/// 开 PTY + 起 shell（环境设置与 GUI 内嵌版完全一致，见 terminal_view.rs 的注释）。
/// `launch`：项目「+」悬浮菜单的 Claude Code / Codex 快捷入口——把要跑的命令直接编进
/// 启动命令行（`-ilc '<launch>; exec <shell> -l'`），而不是等 shell 起来后再补发按键。
/// 这样从根上没有"shell 是否已经在读 stdin"的时序问题，命令跑完会 exec 回一个
/// 正常交互 login shell，之后就是一个普通会话。
fn shell_launch_args(shell: &str, launch: Option<&str>) -> Vec<String> {
    match launch {
        Some(launch) => vec!["-ilc".to_string(), format!("{launch}; exec {shell} -l")],
        None => vec!["-l".to_string()],
    }
}

/// 交互式 PTY 是真终端：声明真彩能力，并卸掉宿主漏进来的关色开关。
fn apply_interactive_pty_env(cmd: &mut CommandBuilder) {
    cmd.env("TERM", "xterm-256color");
    // 少数 CLI 只认 COLORTERM 才开 24-bit 真彩（Zed 也会设）。
    cmd.env("COLORTERM", "truecolor");
    // 伪装 iTerm2：让 Claude Code 自动发 OSC 9 通知（GUI 侧捕获），见 terminal.rs 注释。
    cmd.env("TERM_PROGRAM", "iTerm.app");
    cmd.env("TERM_PROGRAM_VERSION", "3.5.0");
    for key in smelt_core::tty_color::SUPPRESSION_VARS {
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
        .unwrap_or_else(|| "/tmp/.smelt".into())
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

fn launch_agent_kind(launch: &str) -> Option<smelt_core::agent_kind::ConversationAgentKind> {
    let program = launch_program(launch)?;
    smelt_core::agent_kind::ConversationAgentKind::from_id(program)
        .or_else(|| smelt_core::agent_kind::ConversationAgentKind::from_command_loose(program))
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

fn launch_with_agent_mcp(launch: &str, session_id: &str, agent_token: &str) -> (String, bool) {
    if !smelt_core::agent_bus::cross_agent_enabled() {
        return (launch.to_string(), false);
    }
    let Some(agent) = launch_agent_kind(launch) else {
        return (launch.to_string(), false);
    };
    let executable_path = smelt_core::agent_bus::mcp_executable_path();
    if !executable_path.is_file() {
        return (launch.to_string(), false);
    }
    let executable = executable_path.to_string_lossy().into_owned();
    let socket = sock_path().to_string_lossy().into_owned();
    match agent.terminal_mcp_inject() {
        smelt_core::agent_kind::TerminalMcpInject::McpConfigJson => {
            let config =
                smelt_mcp_server_json(&executable, session_id, agent_token, &socket, false);
            (
                format!("{launch} --mcp-config {}", shell_quote(&config.to_string())),
                true,
            )
        }
        smelt_core::agent_kind::TerminalMcpInject::CodexToml => {
            let executable = serde_json::to_string(&executable)
                .unwrap_or_else(|_| "\"smelt-agent-mcp\"".to_string());
            let session_id =
                serde_json::to_string(session_id).unwrap_or_else(|_| "\"\"".to_string());
            let agent_token =
                serde_json::to_string(agent_token).unwrap_or_else(|_| "\"\"".to_string());
            let socket = serde_json::to_string(&socket).unwrap_or_else(|_| "\"\"".to_string());
            let config = format!(
                "{{ command = {executable}, env = {{ SMELT_SESSION_ID = {session_id}, SMELT_AGENT_TOKEN = {agent_token}, SMELT_SOCK = {socket} }}, tool_timeout_sec = 610 }}"
            );
            (
                format!(
                    "{launch} -c {}",
                    shell_quote(&format!("mcp_servers.smelt={config}"))
                ),
                true,
            )
        }
        smelt_core::agent_kind::TerminalMcpInject::AdditionalMcpConfigJson => {
            let config = smelt_mcp_server_json(&executable, session_id, agent_token, &socket, true);
            (
                format!(
                    "{launch} --additional-mcp-config {}",
                    shell_quote(&config.to_string())
                ),
                true,
            )
        }
        smelt_core::agent_kind::TerminalMcpInject::OpenCodeConfigContent => (
            launch_with_opencode_mcp(launch, &executable, session_id, agent_token, &socket),
            true,
        ),
        smelt_core::agent_kind::TerminalMcpInject::None => (launch.to_string(), false),
    }
}

fn launch_with_opencode_mcp(
    launch: &str,
    executable: &str,
    session_id: &str,
    agent_token: &str,
    socket: &str,
) -> String {
    let config = opencode_mcp_config_json(executable, session_id, agent_token, socket);
    format!(
        "OPENCODE_CONFIG_CONTENT={} {launch}",
        shell_quote(&config.to_string())
    )
}

/// OpenCode 的进程级配置覆盖层。不能复用 `smelt_mcp_server_json`：OpenCode
/// 使用自己的 `mcp.<name>` schema，且把 stdio 命令表达为 argv 数组。配置只会
/// 放进当前启动命令的 `OPENCODE_CONFIG_CONTENT`，不会写入用户或项目配置文件。
fn opencode_mcp_config_json(
    executable: impl serde::Serialize,
    session_id: &str,
    agent_token: &str,
    socket: impl serde::Serialize,
) -> serde_json::Value {
    serde_json::json!({
        "mcp": {
            "smelt": {
                "type": "local",
                "command": [executable],
                "enabled": true,
                "environment": {
                    "SMELT_SESSION_ID": session_id,
                    "SMELT_AGENT_TOKEN": agent_token,
                    "SMELT_SOCK": socket,
                },
                "timeout": 610_000,
            }
        }
    })
}

fn smelt_mcp_server_json(
    executable: impl serde::Serialize,
    session_id: &str,
    agent_token: &str,
    socket: impl serde::Serialize,
    copilot_extras: bool,
) -> serde_json::Value {
    let mut server = serde_json::json!({
        "type": "stdio",
        "command": executable,
        "args": [],
        "env": {
            "SMELT_SESSION_ID": session_id,
            "SMELT_AGENT_TOKEN": agent_token,
            "SMELT_SOCK": socket,
        }
    });
    if copilot_extras {
        server["tools"] = serde_json::json!(["*"]);
        server["timeout"] = serde_json::json!(610_000);
    }
    serde_json::json!({ "mcpServers": { "smelt": server } })
}

/// 不接受 ACP `session/new` stdio MCP 的 adapter（见 `acp_accepts_session_mcp`）
/// 把 Smelt MCP 放进独立 argv，避免 JSON 被空白分词拆开。
fn acp_agent_mcp_cli_args(
    launch: &smelt_core::agent_kind::ConversationLaunchSpec,
    session_id: &str,
    agent_token: &str,
) -> Vec<String> {
    let Some(agent) = launch_agent_kind(&launch.command) else {
        return Vec::new();
    };
    if agent.acp_accepts_session_mcp() {
        return Vec::new();
    }
    let executable_path = smelt_core::agent_bus::mcp_executable_path();
    if !executable_path.is_file() {
        return Vec::new();
    }
    let config = smelt_mcp_server_json(executable_path, session_id, agent_token, sock_path(), true);
    vec!["--additional-mcp-config".to_string(), config.to_string()]
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

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(shell.clone());
    // 快捷启动必须同时是 interactive + login：用户级 CLI 安装器通常把 PATH
    // 写进 .zshrc，只用 `-lc` 读不到，Dock 启动的 smeltd 又只有系统 PATH。
    let agent_token = uuid::Uuid::new_v4().simple().to_string();
    let status_extension = pi_status_extension_path();
    let (injected_launch, agent_mcp) = launch
        .map(|launch| {
            let launch = if status_extension.is_file() {
                launch_with_terminal_status_bridge(launch, &status_extension)
            } else {
                launch.to_string()
            };
            launch_with_agent_mcp(&launch, id, &agent_token)
        })
        .map(|(launch, supported)| (Some(launch), supported))
        .unwrap_or((None, false));
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
    cmd.env("SMELT_SOCK", sock_path());
    cmd.env(
        "SMELT_NOTIFY_BIN",
        smelt_core::agent_event::notify_executable_path(),
    );
    // 共享锁：多个新会话可以互相并发 spawn，但跟 handle_upgrade 的独占锁互斥——
    // 挡住「fork 出的子进程意外继承 upgrade 正在清 CLOEXEC 的其它会话 fd」（见
    // SPAWN_GATE 定义处注释）。
    let child = {
        let _gate = SPAWN_GATE.read().unwrap();
        pair.slave.spawn_command(cmd)?
    };
    let pid = child
        .process_id()
        .map(|p| p as i32)
        .ok_or_else(|| anyhow::anyhow!("拿不到 shell pid"))?;
    let terminal_child = TerminalChild::start(pid)?;

    // 把 master fd dup 成自己持有的 File（写端 + 读端各一份），portable_pty 的 pair
    // 在函数结尾 drop、关掉它自己那份 fd——PTY 只要还有 fd 开着就活着。child 句柄
    // 一并丢弃：kill/收尸都用 pid 直接做（portable_pty 的 Child drop 不杀进程）。
    let raw = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| anyhow::anyhow!("拿不到 PTY master fd"))?;
    let master = dup_file(raw)?;
    set_fd_nonblocking(master.as_raw_fd(), true)?;
    let pty_reader = master.try_clone()?;
    let state = Arc::new(Mutex::new(SessionState {
        id: id.to_string(),
        instance,
        cwd: cwd.map(String::from),
        launch: launch.map(String::from),
        agent_mcp,
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
            jolt: false,
            cols,
            rows,
            cell_w: 0,
            cell_h: 0,
            remote_viewports: 0,
            remote_grace: 0,
            cwd: cwd.map(String::from),
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
    let replies = {
        let Ok(mut pending) = sess.color_replies.lock() else {
            return;
        };
        std::mem::take(&mut *pending)
    };
    if replies.is_empty() {
        return;
    }

    for reply in replies {
        if write_session_input(sess, reply.as_bytes()).is_err() {
            break;
        }
    }
}

/// 常驻 Term 在解析 OSC 颜色查询时只能入队（它当时持有网格锁）。这里在网格锁释放后
/// 统一写回 child，沿用 `Ctl` 的串行写入路径，避免与 resize 反向抢锁。
fn flush_terminal_color_replies(sess: &Session) {
    write_terminal_color_replies(sess);
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
        // resume 的旧 keyframe 理论上不含查询序列；即便有，也要在首次 read 前答复。
        flush_terminal_color_replies(&sess);
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) => break,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
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
                    continue;
                }
                Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
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
