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
//!   {"op":"list"}                                            → 回 {"sessions":[..]} 后关闭
//!   {"op":"kill","id":".."}                                  → 回 {"ok":true} 后关闭
//!   {"op":"version"}                                         → 回 {"version":"..","exe_mtime":123} 后关闭
//!   {"op":"shutdown"}                                        → 回 {"ok":true} 后进程退出（杀掉所有会话！）
//!   {"op":"upgrade"}                                         → 回 {"ok":true} 后 exec 磁盘上的新二进制，
//!                                                              PTY/静默 ACP fd 原地交接；ACP 有活跃 RPC
//!                                                              时回 {"ok":false,"busy":true}（见下）
//!   {"op":"upgrade","exe":"/path/to/smeltd"}                 → 同上，但 exec 指定路径（装 DMG 时先
//!                                                              handoff 到暂存包，再替换 .app，避免
//!                                                              整包覆盖把旧守护 SIGKILL、会话全灭）
//!   {"op":"remote_start","bind":"..","port":0,"write":false}  → 回 {"ok":true,"token":"..","addr":"..","write":bool}，
//!                                                              见下「内嵌远程网关」（bind/port/write 都可省，
//!                                                              默认回环随机口 + 只读）
//!   {"op":"remote_stop"}                                     → 回 {"ok":true} 后关闭
//!   {"op":"remote_rotate_token"}                              → 停止远程服务、持久化新 token，旧配对失效
//!   {"op":"remote_status"}                                   → 回 {"running":bool,"token":"..","addr":"..","write":bool} 后关闭
//!   {"op":"iroh_start","write":false}                         → 回 {"ok":true,"endpoint_id":"..","token":"..",
//!                                                              "addr":"..","write":bool}，把远程网关经 iroh
//!                                                              P2P 暴露出去（见下「iroh 隧道」）
//!   {"op":"iroh_stop"}                                       → 回 {"ok":true} 后关闭
//!   {"op":"iroh_status"}                                     → 回 {"running":bool,"endpoint_id":"..","token":"..",
//!                                                              "write":bool} 后关闭
//!   {"op":"state","id":"..","phase":"..","question":".."}    → 回 {"ok":true} 后关闭，hook 直写（见下
//!                                                              「状态通道」），question 可省
//!   {"op":"agent_event","id":"..","event":{...}}             → 回 {"ok":true} 后关闭，v1 归一化
//!                                                              hook 事件；`state` 保留兼容旧 helper
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
//!   客户端 → 守护：帧 [type:u8][len:u32 BE][payload]
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
//! ## 无缝升级（"upgrade" op，nginx 风格 exec 交接）
//!
//! fd 属于进程而非二进制：`exec()` 换掉程序映像但 PID 与打开的 fd 都还在，只要
//! PTY master fd 不关，shell 就活着。流程：
//! 1. 先拿 SPAWN_GATE 独占锁挡住新 shell/ACP 子进程的 fork，再短暂持 sessions 锁
//!    克隆一份 Arc 列表后放开——避免 open/list/kill/version 长期卡在 sessions 锁上，
//!    同时保证任何已 fork 的 ACP 都先把 pid/fd 发布完，才开始收集交接快照；
//!    随后检查 ACP 静默屏障，存在活跃回合/审批/outstanding RPC 就返回 busy，不执行 exec；
//! 2. 逐会话拿 ctl/out 锁做快照（master fd / shell pid / 尺寸 / **Term 可视区 keyframe**）
//!    ——out 锁在 handle_open 里配了写超时（CLIENT_WRITE_TIMEOUT），泵线程不会无限期攥着；
//! 3. 给 master fd 和监听 socket fd 清掉 CLOEXEC，快照写入交接文件（fd 号 + grid ANSI，
//!    0600）；**画面恢复只认 grid**，与 shell/TUI/agent 无关，同一条路径；
//! 4. 回 {"ok":true} 后 `exec()` 磁盘上的 smeltd（同路径新内容），带 SMELTD_HANDOFF 环境变量；
//! 5. 新进程：认领 fd → 空 Term → **只 feed grid keyframe** → 开泵（jolt=true）。
//!    环形 `buf` 若写在交接文件里也**不** feed（历史字段，兼容旧 handoff 文件）。
//! exec 失败则回滚（恢复 CLOEXEC、删交接文件、继续服务，释放 SPAWN_GATE）。客户端连接
//! 是 CLOEXEC 的，随 exec 断开，GUI 按会话 id 重连即恢复——跟 GUI 自己重启走的是同一条
//! reattach 路。shell 子进程的父进程关系不受 exec 影响（同 PID），收尸的 waitpid 照常
//! 工作。交接文件读不出/解析失败（极端情况）时新进程走全新启动兜底：**不**做「能连上
//! 说明已有守护」这条单实例检查——此时我们可能还继承着旧监听 fd，检查会连上自己而
//! 误判、直接自杀，见 main() 里的 came_from_handoff 分支。
//!
//! ## 内嵌远程网关（`remote_start`/`remote_stop`/`remote_status`）
//!
//! 路由/handler 全在 `remote_gateway.rs`（跟独立进程版 `gateway.rs` 共用一份，见那边
//! 的模块注释）——这里只是按需把它跑起来。守护本身是同步/阻塞线程模型，**不**把
//! `main()` 整个改成 async；`remote_start` 只是另起一条 OS 线程，在那条线程里私自建
//! 一个 tokio runtime 跑 axum server，跟守护主循环完全隔离，互不影响。
//!
//! 幂等：已经开着时 `remote_start` 直接回现有的 token/addr，不重启、不换 token。
//! token 单独保存在 `~/.smelt/remote-token`（0600），冷启动和无缝升级都复用；只有
//! `remote_rotate_token` 会轮换并让旧配对失效。网关运行态本身**不**参与无缝升级交接：
//! `upgrade` 之后旧进程里的网关随之关闭，新进程内存里是空的——但新进程启动时会读
//! `~/.smelt/collab.json`，用户之前开着远程就自动拉回来（见
//! `autostart_remote_from_config`）。这条自愈路径不能少：守护重启（硬重启 / 升级 exec /
//! 崩溃后被拉起）之后若没人重新 `remote_start`，手机侧就会静默失联，只能靠用户去设置页
//! 把远程「关掉再打开」。安全默认跟 `watch` 一致：没配置过就是关闭、绑回环，
//! 见 collaboration.md 的安全底线。
//!
//! 网关运行期间在 macOS 持有 `PreventUserIdleSystemSleep` 电源断言：屏幕仍可按系统设置
//! 正常熄灭，但整机不会因空闲睡眠而把网关和 iroh 挂起。`RemoteGateway` 销毁即释放，
//! 所以关闭远程、守护升级和退出都不会留下常驻断言。
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

mod acp_registry;
mod tasks;

use acp_registry::{AcpRegistry, AcpSlot};
use smelt_core::agent_event::{AGENT_EVENT_VERSION, AgentEvent, AgentEventKind};
use smelt_core::osc::{
    OscNotification, OscNotificationKind, OscScan, TerminalGeometryOsc, terminal_geometry_osc,
};
use smelt_core::remote_gateway;
use smelt_core::title_spinner;
use tasks::TaskState;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::Duration;

#[cfg(test)]
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// 常驻 Term 的 scrollback 行数（状态机 history-limit）。
const TERM_HISTORY: usize = 10_000;
/// attach 快照最多带上的历史行数（含可视区）；避免超大会话一次吐爆客户端。
const SNAPSHOT_MAX_LINES: usize = 10_000;

/// attach 客户端 socket 的写超时：泵线程/attach 初始重放都会往客户端 write，客户端
/// 冻结（GUI 被挂起/调试暂停）时不能让这一个 write 无限期占着 Out 锁——handle_upgrade
/// 快照时也要挨个拿这把锁，泵线程如果永久攥着，会把整个 upgrade 拖成全局死锁。
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(3);

/// 挡住「spawn 新 shell/ACP 子进程」与「upgrade 清 CLOEXEC 准备 exec」并发的门闩：
/// 不挡会有极小窗口——CLOEXEC 刚被清、我们自己还没 exec 时，恰好 fork 出一个新进程，
/// 会把当时暴露出去的全部 fd（其它会话的 PTY master、监听 socket）一并带走。
/// spawn 拿共享锁（多个新会话可以互相并发起），upgrade 拿独占锁（跟所有 spawn 互斥）。
static SPAWN_GATE: LazyLock<Arc<RwLock<()>>> = LazyLock::new(|| Arc::new(RwLock::new(())));

fn acquire_upgrade_spawn_gate(gate: &Arc<RwLock<()>>) -> std::sync::RwLockWriteGuard<'_, ()> {
    gate.write().unwrap()
}

#[cfg(test)]
mod spawn_gate_sync_tests {
    use super::{SPAWN_GATE, acquire_upgrade_spawn_gate, new_acp_sessions};
    use std::sync::{Arc, RwLock, mpsc};
    use std::time::Duration;

    #[test]
    fn daemon_write_guard_blocks_acp_spawn_permit() {
        let acp_sessions = new_acp_sessions();
        let acp_gate = acp_sessions.spawn_gate();
        let write_guard = SPAWN_GATE.write().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let _permit = acp_gate.read().unwrap();
            entered_tx.send(()).unwrap();
        });

        ready_rx.recv().unwrap();
        let entered_while_locked = entered_rx.recv_timeout(Duration::from_millis(50)).is_ok();
        drop(write_guard);
        if !entered_while_locked {
            entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("ACP spawn permit should proceed after upgrade releases the gate");
        }
        worker.join().unwrap();
        assert!(
            !entered_while_locked,
            "terminal upgrade gate and ACP registry must share the same lock"
        );
    }

    #[test]
    fn upgrade_snapshot_waits_for_preexisting_spawn_readers() {
        let gate = Arc::new(RwLock::new(()));
        let spawn_permit = gate.read().unwrap();
        let gate_for_upgrade = Arc::clone(&gate);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();

        let upgrade = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let _write_guard = acquire_upgrade_spawn_gate(&gate_for_upgrade);
            snapshot_tx.send(()).unwrap();
        });

        ready_rx.recv().unwrap();
        let snapshot_while_reader_held =
            snapshot_rx.recv_timeout(Duration::from_millis(50)).is_ok();
        drop(spawn_permit);
        if !snapshot_while_reader_held {
            snapshot_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("snapshot collection should start after existing readers finish");
        }
        upgrade.join().unwrap();
        assert!(
            !snapshot_while_reader_held,
            "upgrade must not collect snapshots until existing spawns publish their metadata"
        );
    }
}

fn sock_path() -> std::path::PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| "/tmp".into())
        .join(".smelt");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("smeltd.sock")
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

#[cfg(test)]
mod single_instance_tests {
    use super::*;

    fn test_socket(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("smeltd-{name}-{}-{nonce}.sock", std::process::id()))
    }

    #[test]
    fn concurrent_starters_leave_exactly_one_listener() {
        const STARTERS: usize = 8;
        let path = test_socket("single-instance");
        let barrier = Arc::new(std::sync::Barrier::new(STARTERS));
        let mut starters = Vec::new();

        for _ in 0..STARTERS {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            starters.push(thread::spawn(move || {
                let listener = bind_single_instance(&path, true).expect("并发启动不应 bind 失败");
                barrier.wait();
                listener.is_some()
            }));
        }

        let listeners = starters
            .into_iter()
            .map(|starter| starter.join().unwrap())
            .filter(|has_listener| *has_listener)
            .count();
        assert_eq!(listeners, 1, "并发启动只能有一个进程取得 listener");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    #[test]
    fn stale_socket_path_is_replaced_while_holding_startup_lock() {
        let path = test_socket("stale");
        std::fs::write(&path, b"stale").unwrap();

        let listener = bind_single_instance(&path, true)
            .expect("僵尸 socket 应可恢复")
            .expect("没有活实例时应取得 listener");
        assert!(UnixStream::connect(&path).is_ok());

        drop(listener);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    #[test]
    fn existing_daemon_keeps_live_handoff_file() {
        let path = test_socket("live-handoff");
        let handoff = path.with_extension("handoff");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::write(&handoff, b"live upgrade snapshot").unwrap();

        let result = bind_fresh_daemon(&path, &handoff, true).unwrap();

        assert!(result.is_none(), "已有 daemon 时竞争启动者必须退出");
        assert!(
            handoff.is_file(),
            "竞争启动者不能删除已有 daemon 正在使用的 handoff"
        );
        drop(listener);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&handoff);
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    #[test]
    fn fresh_daemon_removes_stale_handoff_file() {
        let path = test_socket("stale-handoff");
        let handoff = path.with_extension("handoff");
        std::fs::write(&handoff, b"stale").unwrap();

        let listener = bind_fresh_daemon(&path, &handoff, true)
            .unwrap()
            .expect("没有已有 daemon 时应取得 listener");

        assert!(!handoff.exists(), "新 daemon 应清理陈旧 handoff");
        drop(listener);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
}

/// 追加一行到 ~/.smelt/daemon.log。给守护交接故障和需要跨进程查看的网络状态留痕——
/// 守护被 SIGKILL（例：装新版时用 cp 覆盖了已签名二进制，upgrade 的 exec 会被
/// macOS 内核直接杀掉，无输出无崩溃报告）或静默 return 时，这份日志是唯一线索：
/// 日志停在「即将 exec」而没有下一行「交接完成」，就是 exec 被杀。
///
/// 同时转一份进全 app 通用的 `app_log`（~/.smelt/app.log，见该模块）——这里记录的
/// 全是异常/生命周期事件，天然也是「关键操作/错误」，没必要在两份日志里分别手写。
fn dlog(msg: &str) {
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
    std::env::current_exe()
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
    /// shell 进程 pid：kill 会话 / shell 退出后收尸（waitpid）。
    pid: i32,
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
    /// spawn 时的静态目录（作战地图要）。**不**跟随 shell 的 `cd`——真实 cwd 要
    /// OSC 7，这里只是「这个会话是从哪打开的」，见 SessionState.cwd 用法。
    cwd: Option<String>,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 本进程启动时刻（unix 秒），`version` op 回给 GUI 展示「守护跑了多久」。
/// 必须在 main 最开头取一次，否则记的是「首次有人问」的时间。
///
/// 无缝升级（exec 交接）后是全新进程，这个值会重置，而会话照旧活着——设置页因此
/// 会显示「守护刚起、会话仍在」，那是如实反映，不是 bug。
fn started_at() -> u64 {
    static STARTED_AT: OnceLock<u64> = OnceLock::new();
    *STARTED_AT.get_or_init(now_unix)
}

/// 会话状态通道（见 docs/notification-architecture.md）。三个信源按可信度覆盖：
/// hook 事件归约（`agent_event` op，协议事实，最高）> OSC 9/777（终端协议，中）>
/// OSC 0/2 标题的 spinner 猜测（最低，纯猜）。schema 定死，字段不够用再加，
/// 不删不改类型——远程端/GUI 都按这份 schema 解码。
#[derive(Clone, Default, serde::Serialize)]
struct SessionState {
    id: String,
    cwd: Option<String>,
    /// claude / codex / copilot（来自 spawn 时的 launch 命令）。
    launch: Option<String>,
    /// OSC 0/2 标题，GUI 现在也读这个显示 tab 名。
    title: Option<String>,
    phase: Phase,
    /// unix 秒。「空转多久了」靠它算——作战地图用。
    phase_since: u64,
    /// 在问什么——远程遥控的命脉，Phase 6 的 action 门闩靠它判断能不能安全写入。
    pending_question: Option<String>,
    /// 累计花费口径（各轮 cache_read 都加了），**不是**上下文占用，不能当余量分母。
    /// 见 session_history::SessionSummary.total_tokens；目前没接，先占位。
    tokens_used: Option<u64>,
    /// 撞车预警要用；目前没接（见 git_panel.rs 的 GitStatusData），先占位。
    branch: Option<String>,
    dirty_files: Vec<String>,
    updated_at: u64,
    /// 已收到 smelt-notify 的结构化 hook 事件。
    structured_events: bool,
    /// 最近收到的归一化 hook 协议版本；None 表示旧 `state` op 或纯 fallback。
    agent_event_version: Option<u32>,
    /// 等待状态的来源身份，仅供 reducer 防止无关子任务/工具完成误清等待。
    #[serde(skip)]
    active_blocker: Option<AgentBlocker>,
}

#[derive(Clone, Debug, Default)]
struct AgentBlocker {
    tool_use_id: Option<String>,
    agent_id: Option<String>,
    tool_name: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Thinking,
    ExecutingTool,
    AwaitingApproval,
    WaitingForUser,
    Succeeded,
    Failed,
    #[default]
    Idle,
    Dead,
}

fn event_matches_blocker(blocker: &AgentBlocker, event: &AgentEvent) -> bool {
    if let Some(expected) = blocker.tool_use_id.as_deref() {
        return event.tool_use_id.as_deref() == Some(expected);
    }
    if let Some(expected) = blocker.agent_id.as_deref() {
        return event.agent_id.as_deref() == Some(expected);
    }
    if let Some(expected) = blocker.tool_name.as_deref() {
        return event.tool_name.as_deref() == Some(expected);
    }
    true
}

/// 将 provider 适配器产出的统一事件归约进会话。等待状态是粘性的：只有对应的
/// tool/agent 后续事件、用户新 prompt 或明确的回合终态才能清除，子任务噪声不能覆盖。
fn apply_agent_event(state: &mut SessionState, event: &AgentEvent) -> bool {
    if event.version != AGENT_EVENT_VERSION {
        return false;
    }

    let previous_phase = state.phase;
    let waiting = matches!(state.phase, Phase::AwaitingApproval | Phase::WaitingForUser);
    let matching_blocker = state
        .active_blocker
        .as_ref()
        .is_none_or(|blocker| event_matches_blocker(blocker, event));

    match event.kind {
        AgentEventKind::SessionStarted => {
            state.phase = Phase::Idle;
            state.pending_question = None;
            state.active_blocker = None;
        }
        AgentEventKind::PromptSubmitted => {
            state.phase = Phase::Thinking;
            state.pending_question = None;
            state.active_blocker = None;
        }
        AgentEventKind::ToolStarted => {
            if !waiting || matching_blocker {
                state.phase = Phase::ExecutingTool;
                state.pending_question = event.message.clone();
                state.active_blocker = None;
            }
        }
        AgentEventKind::ToolFinished => {
            if !waiting || matching_blocker {
                state.phase = Phase::Thinking;
                state.pending_question = None;
                state.active_blocker = None;
            }
        }
        AgentEventKind::ToolFailed => {
            if !waiting || matching_blocker {
                state.phase = Phase::Thinking;
                state.pending_question = event.message.clone();
                state.active_blocker = None;
            }
        }
        AgentEventKind::ApprovalRequested | AgentEventKind::InputRequested => {
            state.phase = if event.kind == AgentEventKind::ApprovalRequested {
                Phase::AwaitingApproval
            } else {
                Phase::WaitingForUser
            };
            state.pending_question = event.message.clone().or_else(|| event.tool_name.clone());
            state.active_blocker = Some(AgentBlocker {
                tool_use_id: event.tool_use_id.clone(),
                agent_id: event.agent_id.clone(),
                tool_name: event.tool_name.clone(),
            });
        }
        AgentEventKind::SubagentStarted => {
            if !waiting {
                state.phase = Phase::ExecutingTool;
                state.pending_question = event.message.clone();
            }
        }
        AgentEventKind::SubagentStopped => {
            if !waiting {
                state.phase = Phase::Thinking;
                state.pending_question = None;
            }
        }
        AgentEventKind::TurnSucceeded => {
            state.phase = Phase::Succeeded;
            state.pending_question = event.message.clone();
            state.active_blocker = None;
        }
        AgentEventKind::TurnFailed => {
            state.phase = Phase::Failed;
            state.pending_question = event.message.clone();
            state.active_blocker = None;
        }
        AgentEventKind::SessionEnded => {
            state.phase = Phase::Dead;
            state.pending_question = None;
            state.active_blocker = None;
        }
    }

    state.structured_events = true;
    state.agent_event_version = Some(event.version);
    if state.phase != previous_phase {
        state.phase_since = now_unix();
    }
    state.updated_at = now_unix();
    true
}

#[cfg(test)]
mod agent_event_reducer_tests {
    use super::*;

    fn event(kind: AgentEventKind) -> AgentEvent {
        AgentEvent::new("codex", kind)
    }

    #[test]
    fn normalized_lifecycle_reaches_waiting_then_success() {
        let mut state = SessionState::default();
        assert!(apply_agent_event(
            &mut state,
            &event(AgentEventKind::PromptSubmitted)
        ));
        assert_eq!(state.phase, Phase::Thinking);

        let mut waiting = event(AgentEventKind::InputRequested);
        waiting.tool_use_id = Some("question-1".into());
        waiting.message = Some("选一个".into());
        apply_agent_event(&mut state, &waiting);
        assert_eq!(state.phase, Phase::WaitingForUser);
        assert_eq!(state.pending_question.as_deref(), Some("选一个"));

        let mut answered = event(AgentEventKind::ToolFinished);
        answered.tool_use_id = Some("question-1".into());
        apply_agent_event(&mut state, &answered);
        assert_eq!(state.phase, Phase::Thinking);

        apply_agent_event(&mut state, &event(AgentEventKind::TurnSucceeded));
        assert_eq!(state.phase, Phase::Succeeded);
    }

    #[test]
    fn unrelated_child_events_do_not_clear_a_sticky_wait() {
        let mut state = SessionState::default();
        let mut waiting = event(AgentEventKind::ApprovalRequested);
        waiting.tool_use_id = Some("approval-1".into());
        waiting.agent_id = Some("lead".into());
        apply_agent_event(&mut state, &waiting);

        let mut child_finished = event(AgentEventKind::ToolFinished);
        child_finished.tool_use_id = Some("child-tool".into());
        child_finished.agent_id = Some("child".into());
        apply_agent_event(&mut state, &child_finished);
        assert_eq!(state.phase, Phase::AwaitingApproval);

        let mut child_stopped = event(AgentEventKind::SubagentStopped);
        child_stopped.agent_id = Some("child".into());
        apply_agent_event(&mut state, &child_stopped);
        assert_eq!(state.phase, Phase::AwaitingApproval);
    }

    #[test]
    fn tool_name_keeps_wait_sticky_when_provider_has_no_call_id() {
        let mut state = SessionState::default();
        let mut waiting = event(AgentEventKind::InputRequested);
        waiting.tool_name = Some("request_user_input".into());
        apply_agent_event(&mut state, &waiting);

        let mut unrelated = event(AgentEventKind::ToolFinished);
        unrelated.tool_name = Some("shell".into());
        apply_agent_event(&mut state, &unrelated);
        assert_eq!(state.phase, Phase::WaitingForUser);

        let mut answered = event(AgentEventKind::ToolFinished);
        answered.tool_name = Some("request_user_input".into());
        apply_agent_event(&mut state, &answered);
        assert_eq!(state.phase, Phase::Thinking);
    }

    #[test]
    fn prompt_and_terminal_events_always_clear_a_wait() {
        for kind in [
            AgentEventKind::PromptSubmitted,
            AgentEventKind::TurnSucceeded,
            AgentEventKind::TurnFailed,
            AgentEventKind::SessionEnded,
        ] {
            let mut state = SessionState::default();
            let mut waiting = event(AgentEventKind::InputRequested);
            waiting.tool_use_id = Some("question-1".into());
            apply_agent_event(&mut state, &waiting);
            apply_agent_event(&mut state, &event(kind));
            assert!(!matches!(
                state.phase,
                Phase::AwaitingApproval | Phase::WaitingForUser
            ));
            assert!(state.active_blocker.is_none());
        }
    }

    #[test]
    fn unsupported_event_version_is_ignored() {
        let mut state = SessionState::default();
        let mut future = event(AgentEventKind::TurnFailed);
        future.version = AGENT_EVENT_VERSION + 1;
        assert!(!apply_agent_event(&mut state, &future));
        assert_eq!(state.phase, Phase::Idle);
        assert!(!state.structured_events);
    }
}

/// `subscribe` 连接的全局池——状态订阅是「一条连接看全部会话」，不像 `watch`
/// 那样挂在单个 Session 底下（见 docs/state-channel-plan.md 的 subscribe 设计）。
type Subscribers = Arc<Mutex<Vec<UnixStream>>>;

/// 状态变化推给所有订阅者；写失败（已断线）的连接直接摘掉，跟 watchers 的
/// 惰性清理是同一个模式。
fn broadcast_state(subscribers: &Subscribers, state: &SessionState) {
    let payload = serde_json::json!({ "session": state }).to_string();
    subscribers
        .lock()
        .unwrap()
        .retain_mut(|s| writeln!(s, "{payload}").is_ok());
}

/// 常驻 Term 的事件监听：接住 alacritty 解析出的 `Event::Title`/`Event::Bell`，
/// 写进共享的 `SessionState`，顺带广播给所有 `subscribe` 连接。**只在这里猜
/// phase**（OSC 0/2 标题 spinner，最低可信度信源）——`state` op 是协议事实，
/// 可信度最高。spinner 只允许在 `Idle`/`Thinking` 上升为 `Thinking`；绝不能盖掉
/// hook 已写好的 `AwaitingApproval`/`WaitingForUser`/`ExecutingTool`/`Dead`（否则
/// 远程 action 门闩会误拒、操作台按钮错态）。标题不是 spinner 时**不**反过来猜
/// 别的 phase（缺乏证据不代表 idle）。
#[derive(Clone)]
struct StateListener {
    state: Arc<Mutex<SessionState>>,
    subscribers: Subscribers,
}

impl EventListener for StateListener {
    fn send_event(&self, event: Event) {
        let snapshot = {
            let Ok(mut st) = self.state.lock() else {
                return;
            };
            match event {
                Event::Title(t) => {
                    if title_spinner::title_starts_with_spinner(t.trim_start())
                        && matches!(
                            st.phase,
                            Phase::Idle | Phase::Thinking | Phase::Succeeded | Phase::Failed
                        )
                    {
                        // 只在「进入」Thinking 那一刻记起点。agent 思考时 spinner 每秒
                        // 换一帧（⠋→⠙→⠹…），帧帧都是一次 Title 事件；已经在 Thinking
                        // 里还刷起点的话，「已思考 N 秒」会永远在 0~1 之间跳。
                        if st.phase != Phase::Thinking {
                            st.phase = Phase::Thinking;
                            st.phase_since = now_unix();
                        }
                    }
                    st.title = Some(t);
                    st.updated_at = now_unix();
                }
                Event::Bell => {
                    st.updated_at = now_unix();
                }
                _ => return,
            }
            st.clone()
        };
        broadcast_state(&self.subscribers, &snapshot);
    }
}

/// 将终端协议事实归约进守护的唯一 SessionState。Hook 仍是主信号；Codex OSC 9
/// 只负责兼容旧客户端以及纠正偶发丢失的 Stop hook。其他 OSC 种类只是普通通知，
/// 不得改变 agent phase。
fn apply_osc_notification(state: &mut SessionState, notification: &OscNotification) -> bool {
    if notification.kind != OscNotificationKind::Osc9
        || !state.launch.as_deref().is_some_and(is_codex_launch)
        || !matches!(
            state.phase,
            Phase::Idle | Phase::Thinking | Phase::ExecutingTool
        )
    {
        return false;
    }

    state.phase = Phase::Succeeded;
    state.phase_since = now_unix();
    state.pending_question = Some(notification.text.clone());
    state.updated_at = now_unix();
    true
}

fn is_codex_launch(launch: &str) -> bool {
    launch
        .split_whitespace()
        .next()
        .and_then(|command| std::path::Path::new(command).file_name())
        .is_some_and(|command| command == "codex")
}

fn apply_osc_bytes(state: &mut SessionState, scanner: &mut OscScan, bytes: &[u8]) -> bool {
    let mut changed = false;
    for &byte in bytes {
        if let Some(notification) = scanner.feed_notification(byte) {
            changed |= apply_osc_notification(state, &notification);
        }
    }
    changed
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
    let mut ctl = sess.ctl.lock().unwrap();
    if origin == ResizeOrigin::Desktop && ctl.remote_viewports > 0 {
        return false;
    }
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
    let remote_controlled = ctl.remote_viewports > 0;

    // Serialize the invisible geometry marker before SIGWINCH can produce
    // cursor-addressed output at the new size. Desktop renderers resize their
    // local VT model from this marker without echoing a resize frame.
    if let Ok(mut term) = sess.term.lock() {
        term.resize(DaemonTermSize {
            rows: rows as usize,
            cols: cols as usize,
        });
        let marker = terminal_geometry_osc(
            &sess.geometry_token,
            TerminalGeometryOsc {
                cols,
                rows,
                cell_width: cell_w,
                cell_height: cell_h,
                remote_controlled,
            },
        );
        let mut out = sess.out.lock().unwrap();
        out.clients.retain_mut(|client| {
            if client.write_all(&marker).is_ok() {
                true
            } else {
                let _ = client.shutdown(Shutdown::Both);
                false
            }
        });
    }

    if jolt {
        resize_fd(fd, rows.saturating_add(1), cols, xpixel, ypixel);
    }
    resize_fd(fd, rows, cols, xpixel, ypixel);
    true
}

fn begin_remote_viewport(sess: &Session, cols: u16, rows: u16, cell_w: u16, cell_h: u16) {
    {
        let mut ctl = sess.ctl.lock().unwrap();
        ctl.remote_viewports = ctl.remote_viewports.saturating_add(1);
        ctl.jolt = true;
    }
    resize_session_remote(sess, cols, rows, cell_w, cell_h);
}

fn end_remote_viewport(sess: &Session) {
    let mut ctl = sess.ctl.lock().unwrap();
    ctl.remote_viewports = ctl.remote_viewports.saturating_sub(1);
    if ctl.remote_viewports != 0 {
        return;
    }
    let geometry = TerminalGeometryOsc {
        cols: ctl.cols,
        rows: ctl.rows,
        cell_width: ctl.cell_w,
        cell_height: ctl.cell_h,
        remote_controlled: false,
    };
    // The pump keeps `term` through its corresponding `out` write, so taking
    // the same pair here prevents the unlock marker from overtaking bytes
    // already parsed at the mobile geometry. Keep `ctl` as well so a desktop
    // resize cannot slip between decrementing the lease and sending unlock.
    let Ok(_term) = sess.term.lock() else { return };
    let marker = terminal_geometry_osc(&sess.geometry_token, geometry);
    let mut out = sess.out.lock().unwrap();
    out.clients.retain_mut(|client| {
        if client.write_all(&marker).is_ok() {
            true
        } else {
            let _ = client.shutdown(Shutdown::Both);
            false
        }
    });
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

/// 会话输出端：交互 attachment + watch 旁观者。
/// 「快照→接管」与实时转发共用这把锁，严格串行。
/// 画面恢复只靠常驻 Term 的 keyframe，**不再**维护环形字节缓冲。
struct Out {
    /// `open` 连接：每个桌面渲染层各占一路，可同时输入并接收同一份 PTY 输出。
    clients: Vec<UnixStream>,
    /// `watch` 连接：只读旁观，可多个并存。
    watchers: Vec<UnixStream>,
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
    /// Per-session capability for daemon-only geometry control sequences.
    /// The PTY child never receives this token, so terminal output cannot
    /// forge a desktop resize or remote-viewport lock.
    geometry_token: String,
    ctl: Mutex<Ctl>,
    out: Mutex<Out>,
    /// 常驻网格：PTY 输出持续 advance；attach 时序列化成 ANSI 快照。挂的是
    /// `StateListener`（不再是 `VoidListener`）——守护自己也要看得见 Title/Bell。
    term: Mutex<Term<StateListener>>,
    /// 结构化状态（见 SessionState）。跟 `term` 的监听器共用同一个 Arc，
    /// `state` op（hook 直写）和 `subscribe` 的转发都读/改这一份。
    state: Arc<Mutex<SessionState>>,
}

type Sessions = Arc<Mutex<HashMap<String, Arc<Session>>>>;

/// 内嵌远程网关开着时的状态：token、绑定地址、写权限、喊停用的信号。见文件头
/// 「内嵌远程网关」一节——这条不参与无缝升级交接，`upgrade` 后新进程里初值永远是
/// None，由 `autostart_remote_from_config` 按落盘配置重新拉起。
struct RemoteGateway {
    token: String,
    addr: std::net::SocketAddr,
    write: bool,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    _sleep_assertion: Option<SystemSleepAssertion>,
}

#[cfg(target_os = "macos")]
struct SystemSleepAssertion {
    id: u32,
}

#[cfg(target_os = "macos")]
impl SystemSleepAssertion {
    fn acquire() -> Result<Self, i32> {
        use core_foundation::base::TCFType as _;
        use core_foundation::string::{CFString, CFStringRef};

        #[link(name = "IOKit", kind = "framework")]
        unsafe extern "C" {
            fn IOPMAssertionCreateWithName(
                assertion_type: CFStringRef,
                assertion_level: u32,
                assertion_name: CFStringRef,
                assertion_id: *mut u32,
            ) -> i32;
        }

        let assertion_type = CFString::new("PreventUserIdleSystemSleep");
        let reason = CFString::new("Smelt remote access is enabled");
        let mut id = 0;
        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                255,
                reason.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if result == 0 {
            Ok(Self { id })
        } else {
            Err(result)
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for SystemSleepAssertion {
    fn drop(&mut self) {
        #[link(name = "IOKit", kind = "framework")]
        unsafe extern "C" {
            fn IOPMAssertionRelease(assertion_id: u32) -> i32;
        }

        let result = unsafe { IOPMAssertionRelease(self.id) };
        if result != 0 {
            dlog(&format!("释放远程电源断言失败：IOKit {result}"));
        }
    }
}

#[cfg(not(target_os = "macos"))]
struct SystemSleepAssertion;

#[cfg(not(target_os = "macos"))]
impl SystemSleepAssertion {
    fn acquire() -> Result<Self, i32> {
        Ok(Self)
    }
}

struct RemoteStateData {
    gateway: Option<RemoteGateway>,
    /// 持久化的设备凭证。`None` 只存在于冷启动尚未开启远程时；第一次启动网关
    /// 会从磁盘读取或创建，之后重启网关、切换写权限都复用它。
    token: Option<String>,
}

type RemoteState = Arc<Mutex<RemoteStateData>>;

fn new_remote_state(token: Option<String>) -> RemoteState {
    Arc::new(Mutex::new(RemoteStateData {
        gateway: None,
        token,
    }))
}

fn remote_token_path() -> Result<std::path::PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".smelt").join("remote-token"))
        .ok_or_else(|| "找不到用户目录，无法保存远程配对 Token".to_string())
}

fn valid_remote_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn persist_remote_token(path: &std::path::Path, token: &str) -> Result<(), String> {
    use std::io::Write as _;

    let dir = path
        .parent()
        .ok_or_else(|| "远程配对 Token 路径没有父目录".to_string())?;
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
    let staged = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<(), String> {
        let mut file = options
            .open(&staged)
            .map_err(|error| format!("写入 {} 失败：{error}", staged.display()))?;
        file.write_all(token.as_bytes())
            .map_err(|error| format!("写入 {} 失败：{error}", staged.display()))?;
        file.sync_all()
            .map_err(|error| format!("同步 {} 失败：{error}", staged.display()))?;
        std::fs::rename(&staged, path)
            .map_err(|error| format!("替换 {} 失败：{error}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("收紧 {} 权限失败：{error}", path.display()))?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    write_result
}

fn load_or_create_remote_token() -> Result<String, String> {
    load_or_create_remote_token_at(&remote_token_path()?)
}

fn load_or_create_remote_token_at(path: &std::path::Path) -> Result<String, String> {
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let token = raw.trim();
        if valid_remote_token(token) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|error| format!("收紧 {} 权限失败：{error}", path.display()))?;
            }
            return Ok(token.to_string());
        }
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    persist_remote_token(&path, &token)?;
    Ok(token)
}

fn rotate_remote_token(state: &RemoteState) -> Result<String, String> {
    rotate_remote_token_at(state, &remote_token_path()?)
}

fn rotate_remote_token_at(state: &RemoteState, path: &std::path::Path) -> Result<String, String> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let mut guard = state.lock().unwrap();
    if let Some(gateway) = guard.gateway.take() {
        let _ = gateway.shutdown_tx.send(());
    }
    persist_remote_token(path, &token)?;
    guard.token = Some(token.clone());
    Ok(token)
}

/// 幂等：已经开着直接回现有 token/addr/write，不重启、不换 token——包括 `write`
/// 参数：想改写权限得先 `remote_stop` 再 `remote_start`，不支持热切换（跟其余
/// 参数如 bind/port 一样，改配置就是重开一次，这个项目里没有"热更新"这个概念）。
/// bind 非法 / 端口绑不上 / 服务线程起不来都走 Err，调用方原样透传给客户端。
///
/// **先等 serve 就绪再写 `RemoteState`**：以前 spawn 后立刻标 running，子线程
/// `Runtime::new`/`from_std` 失败时状态假活，幂等路径永远回死 token。
fn start_remote_gateway(
    state: &RemoteState,
    bind: &str,
    port: u16,
    write: bool,
) -> Result<(String, std::net::SocketAddr, bool), String> {
    let mut guard = state.lock().unwrap();
    if let Some(g) = guard.gateway.as_ref() {
        return Ok((g.token.clone(), g.addr, g.write));
    }

    let token = match guard.token.clone() {
        Some(token) => token,
        None => {
            let token = load_or_create_remote_token()?;
            guard.token = Some(token.clone());
            token
        }
    };

    let ip: std::net::IpAddr = bind
        .parse()
        .map_err(|e| format!("非法绑定地址 {bind}：{e}"))?;
    let std_listener = std::net::TcpListener::bind((ip, port))
        .map_err(|e| format!("绑定 {bind}:{port} 失败：{e}"))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    let addr = std_listener.local_addr().map_err(|e| e.to_string())?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    // 子线程认领 listener / 建 runtime 成功才算 ready；失败则本函数 Err 且不写 state。
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let token_for_thread = token.clone();
    thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let msg = format!("远程网关起不了 tokio runtime：{e}");
                eprintln!("{msg}");
                let _ = ready_tx.send(Err(msg));
                return;
            }
        };
        rt.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    let msg = format!("远程网关认领监听 fd 失败：{e}");
                    eprintln!("{msg}");
                    let _ = ready_tx.send(Err(msg));
                    return;
                }
            };
            // listener 已就绪，即将 serve——此时可以对外报 running。
            let _ = ready_tx.send(Ok(()));
            let app = remote_gateway::build_router(token_for_thread, write);
            let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(e) = serve.await {
                eprintln!("远程网关退出：{e}");
            }
        });
    });

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            let sleep_assertion = match SystemSleepAssertion::acquire() {
                Ok(assertion) => Some(assertion),
                Err(code) => {
                    dlog(&format!("远程服务无法阻止系统空闲睡眠：IOKit {code}"));
                    None
                }
            };
            guard.gateway = Some(RemoteGateway {
                token: token.clone(),
                addr,
                write,
                shutdown_tx,
                _sleep_assertion: sleep_assertion,
            });
            Ok((token, addr, write))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("远程网关启动超时（5s）".into()),
    }
}

fn stop_remote_gateway(state: &RemoteState) {
    if let Some(g) = state.lock().unwrap().gateway.take() {
        let _ = g.shutdown_tx.send(());
    }
}

/// iroh 隧道（见 `crates/smelt-iroh`）：把本机远程网关经 P2P 暴露出去。
///
/// 跟已下线的 Cloudflare 隧道相比的关键差别 —— 也是留下这条路的理由：
/// 1. `endpoint_id` 由落盘私钥决定，**重启不变**，配对二维码可以永久有效。
/// 2. 优先打洞直连，打不通才走中继，不是全程第三方中转。
/// 3. 没有子进程，因此没有孤儿进程风险。
///
/// 私钥落在 `~/.smelt/iroh-secret`，与命令行 `smelt-iroh-host` 共用同一把，
/// 这样两种起法给出的配对码是同一个。
struct IrohTunnel {
    endpoint_id: String,
    relay: smelt_iroh::RelaySettings,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    /// 已连接的移动端设备（remote_id → 连接时间戳）。
    connections: Arc<Mutex<HashMap<String, u64>>>,
}

/// 单个已连接设备的信息。
#[derive(Clone, Debug, serde::Serialize)]
pub struct IrohConnection {
    /// iroh 节点 ID（公钥的十六进制表示）。
    pub remote_id: String,
    /// 连接建立的时间戳（Unix 秒）。
    pub connected_at: u64,
}

type IrohState = Arc<Mutex<Option<IrohTunnel>>>;
/// 全局连接池：跨隧道重启仍可访问（隧道停后清空）。
type IrohConnections = Arc<Mutex<HashMap<String, u64>>>;

/// 串行化 `start_iroh`：绑定要联网、最长 30s，期间不能一直攥着 `IrohState`
/// （`iroh_status` 等只读路径会被一起堵死），可一旦放开，两个并发调用就会各自
/// 绑一个 endpoint，后写入的顶掉先写入的。守护自愈与 GUI 补发正好可能同时发生，
/// 所以这里单独用一把「启动锁」，把幂等检查和绑定圈在同一段临界区里。
static IROH_START_LOCK: Mutex<()> = Mutex::new(());

/// 幂等：已经开着直接回现有 endpoint_id。会先确保远程网关按 `write` 开着
/// （隧道要转发给它），语义与 `start_tunnel` 一致。
///
/// 与网关同样「先等就绪再写 state」：iroh 绑定要联网发现中继，失败率不低，
/// 抢先标 running 会让幂等路径永远回一个连不上的 endpoint_id。
fn start_iroh(
    iroh_state: &IrohState,
    remote_state: &RemoteState,
    write: bool,
    relay_address: &str,
    connections: IrohConnections,
) -> Result<(String, String, std::net::SocketAddr, bool, String), String> {
    let relay = smelt_iroh::RelaySettings::parse(relay_address)
        .map_err(|e| format!("iroh relay 配置无效：{e:#}"))?;
    // 锁中毒（某次启动 panic 过）不该让远程从此再也起不来：拿回内层的 () 继续。
    let _start_guard = IROH_START_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = iroh_state.lock().unwrap().as_ref() {

        if t.relay != relay {
            return Err("iroh relay 配置已变化，请先停止旧隧道再重试".into());
        }
        let (token, addr, effective_write) = {
            let guard = remote_state.lock().unwrap();
            match guard.gateway.as_ref() {
                Some(g) => (g.token.clone(), g.addr, g.write),
                // 网关被单独停掉了：报错而不是回一个通往虚空的配对码。
                None => return Err("iroh 隧道开着但本机网关已停，请先 iroh_stop".into()),
            }
        };
        return Ok((
            t.endpoint_id.clone(),
            token,
            addr,
            effective_write,
            t.relay.url.to_string(),
        ));
    }

    let (token, addr, effective_write) = ensure_remote_gateway_with_write(remote_state, write)?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String, String>>();

    let tunnel_relay = relay.clone();
    let conn_tracker = Arc::clone(&connections);
    thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("iroh 起不了 tokio runtime：{e}")));
                return;
            }
        };
        rt.block_on(async move {
            let secret = match smelt_iroh::default_secret_path()
                .and_then(|p| smelt_iroh::load_or_create_secret(&p))
            {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("iroh 密钥不可用：{e:#}")));
                    return;
                }
            };
            let endpoint = match smelt_iroh::bind_endpoint(
                secret,
                vec![smelt_iroh::ALPN.to_vec()],
                &tunnel_relay,
            )
            .await
            {
                Ok(ep) => ep,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("iroh 绑定失败：{e:#}")));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(endpoint.id().to_string()));
            let path_observer = std::sync::Arc::new(|status: smelt_iroh::PathStatus| {
                dlog(&format!(
                    "iroh path remote={} kind={} address={} rtt_ms={}",
                    status.remote,
                    status.kind,
                    status.address,
                    status.rtt.as_millis()
                ));
            });
            let conn_observer = std::sync::Arc::new(move |event: smelt_iroh::ConnectionEvent| {
                match event {
                    smelt_iroh::ConnectionEvent::Connected { remote_id, connected_at } => {
                        dlog(&format!("iroh 设备已连接：{remote_id}"));
                        conn_tracker.lock().unwrap().insert(remote_id, connected_at);
                    }
                    smelt_iroh::ConnectionEvent::Disconnected { remote_id } => {
                        dlog(&format!("iroh 设备已断开：{remote_id}"));
                        conn_tracker.lock().unwrap().remove(&remote_id);
                    }
                }
            });
            smelt_iroh::serve_tunnel_with_observers(
                endpoint,
                addr,
                async move {
                    let _ = shutdown_rx.await;
                },
                path_observer,
                conn_observer,
            )
            .await;
        });
    });

    // 30s：绑定要连接用户配置的 relay，比本地绑端口慢得多，5s 在弱网下会误判失败。
    match ready_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(endpoint_id)) => {
            *iroh_state.lock().unwrap() = Some(IrohTunnel {
                endpoint_id: endpoint_id.clone(),
                relay: relay.clone(),
                shutdown_tx,
                connections,
            });
            Ok((
                endpoint_id,
                token,
                addr,
                effective_write,
                relay.url.to_string(),
            ))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("iroh 隧道启动超时（30s）".into()),
    }
}

fn stop_iroh(state: &IrohState) {
    if let Some(t) = state.lock().unwrap().take() {
        // 发送关闭信号，连接会在 tunnel 关闭过程中自然移除
        // 不在此处 clear() 以避免与 conn_observer 回调的竞态条件
        let _ = t.shutdown_tx.send(());
    }
}

fn iroh_status(state: &IrohState) -> Option<(String, smelt_iroh::RelaySettings)> {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|t| (t.endpoint_id.clone(), t.relay.clone()))
}

/// 查询当前已连接的移动端设备列表。
fn get_iroh_connections(state: &IrohState) -> Vec<IrohConnection> {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|t| {
            t.connections
                .lock()
                .unwrap()
                .iter()
                .map(|(remote_id, connected_at)| IrohConnection {
                    remote_id: remote_id.clone(),
                    connected_at: *connected_at,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 守护启动时按落盘配置自动恢复远程访问。
///
/// 为什么必须由守护自己做：网关和隧道的运行态**不**参与无缝升级交接，每个新进程
/// 起来都是空的。而「远程开着」这个意愿只记在 `~/.smelt/collab.json` 里，以前只有
/// GUI 冷启动那一次会去 `remote_start`/`iroh_start`。于是只要守护单独重启过
/// （设置页「重启守护进程」、无缝升级 exec、崩溃后被 `ensure_daemon_running` 拉起），
/// 就没有任何人再把它们拉回来——手机侧表现为「连不上，得去设置页把远程关掉再打开」。
/// 关掉再打开之所以有效，只是因为那条路径重新发了这两条 op。
///
/// 走后台线程：iroh 绑定要联网发现 relay，最坏 30s，绝不能挡住 accept 循环。
/// 登录后网络还没就绪很常见，因此失败要退避重试，而不是一次失败就放弃到下次重启。
fn autostart_remote_from_config(remote_state: RemoteState, iroh_state: IrohState, iroh_connections: IrohConnections) {
    // 逃生阀：跑测试 / 排障时不希望守护自作主张连网。
    if std::env::var_os("SMELT_NO_REMOTE_AUTOSTART").is_some() {
        return;
    }
    spawn_remote_autostart(
        smelt_core::remote_config::load(),
        remote_state,
        iroh_state,
        iroh_connections,
    );
}

/// `autostart_remote_from_config` 里除「读配置」以外的部分。拆出来是为了能测
/// 「配置说关就一动不动」这条——读配置那步依赖 `$HOME`，改环境变量的测试跨线程不可靠。
///
/// 返回是否真的起了后台恢复线程。
fn spawn_remote_autostart(
    config: smelt_core::remote_config::RemoteConfig,
    remote_state: RemoteState,
    iroh_state: IrohState,
    iroh_connections: IrohConnections,
) -> bool {
    if !config.enabled {
        return false;
    }

    thread::spawn(move || {
        // 网关只绑回环、不联网，先起它：即使 relay 没配好，GUI 侧「本机链接」
        // 和后续的 iroh_start 幂等路径也有东西可用。
        match ensure_remote_gateway_with_write(&remote_state, config.write_enabled) {
            Ok((_, addr, _)) => dlog(&format!("按配置自动恢复远程网关：{addr}")),
            Err(e) => {
                dlog(&format!("自动恢复远程网关失败：{e}"));
                return;
            }
        }

        if config.iroh_relay.trim().is_empty() {
            dlog("未配置 iroh relay，跳过隧道自动恢复");
            return;
        }

        // 退避重试：绑定失败几乎都是「网络还没好」，隔一会儿就能成。
        const BACKOFF: [u64; 5] = [0, 3, 10, 30, 60];
        for (attempt, delay) in BACKOFF.iter().enumerate() {
            if *delay > 0 {
                thread::sleep(Duration::from_secs(*delay));
            }
            // 期间用户可能已经手动开好了（GUI 冷启动那条路），幂等直接认账。
            if iroh_state.lock().unwrap().is_some() {
                return;
            }
            match start_iroh(
                &iroh_state,
                &remote_state,
                config.write_enabled,
                &config.iroh_relay,
                Arc::clone(&iroh_connections),
            ) {
                Ok((endpoint_id, _, _, _, _)) => {
                    dlog(&format!("按配置自动恢复 iroh 隧道：{endpoint_id}"));
                    return;
                }
                Err(e) => dlog(&format!(
                    "自动恢复 iroh 隧道失败（第 {} 次）：{e}",
                    attempt + 1
                )),
            }
        }
        dlog("iroh 隧道自动恢复重试用尽，等待 GUI 或用户手动重试");
    });
    true
}

#[cfg(test)]
mod autostart_remote_tests {
    use super::*;

    fn config(enabled: bool) -> smelt_core::remote_config::RemoteConfig {
        smelt_core::remote_config::RemoteConfig {
            enabled,
            // 留空：测试不碰网络，只验证网关那半段和门闩。
            iroh_relay: String::new(),
            write_enabled: false,
        }
    }

    #[test]
    fn disabled_config_starts_nothing() {
        let remote = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
        let iroh: IrohState = Arc::new(Mutex::new(None));
        let iroh_conns: IrohConnections = Arc::new(Mutex::new(HashMap::new()));
        assert!(!spawn_remote_autostart(
            config(false),
            Arc::clone(&remote),
            Arc::clone(&iroh),
            Arc::clone(&iroh_conns),
        ));
        thread::sleep(Duration::from_millis(200));
        assert!(
            remote.lock().unwrap().gateway.is_none(),
            "没开远程时守护绝不能自己把网关开起来"
        );
    }

    #[test]
    fn enabled_config_brings_the_gateway_back() {
        let remote = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
        let iroh: IrohState = Arc::new(Mutex::new(None));
        let iroh_conns: IrohConnections = Arc::new(Mutex::new(HashMap::new()));
        assert!(spawn_remote_autostart(
            config(true),
            Arc::clone(&remote),
            Arc::clone(&iroh),
            Arc::clone(&iroh_conns),
        ));
        // 网关是本机回环 + 端口 0，起得很快；隧道因为没配 relay 会被跳过。
        let mut started = false;
        for _ in 0..50 {
            if remote.lock().unwrap().gateway.is_some() {
                started = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(started, "守护重启后必须按配置把远程网关拉回来");
        assert!(iroh.lock().unwrap().is_none(), "没配 relay 就不该起隧道");
        stop_remote_gateway(&remote);
    }
}

/// 进程退出 / upgrade exec 前清理远程网关与 iroh 隧道。菜单栏 quit 与 accept 线程
/// 不同线程，靠这份 OnceLock 共享 Arc（main 启动时 register）。
static LIFECYCLE: std::sync::OnceLock<(RemoteState, IrohState)> = std::sync::OnceLock::new();

fn register_lifecycle(remote: RemoteState, iroh: IrohState) {
    let _ = LIFECYCLE.set((remote, iroh));
}

/// 关内嵌网关与 iroh 隧道。exit/exec 前必须调——否则 exec 后端口还被占着，
/// 新进程再开网关会撞上「address already in use」。
fn cleanup_sidecar_services() {
    if let Some((remote, iroh)) = LIFECYCLE.get() {
        // iroh 要赶在网关之前停：反过来的话，正在转发的流会先撞上一个已经死掉的
        // 网关端口，手机侧看到的是连接被拒而不是干净的隧道关闭。
        stop_iroh(iroh);
        stop_remote_gateway(remote);
    }
}

fn ensure_remote_gateway_with_write(
    state: &RemoteState,
    write: bool,
) -> Result<(String, std::net::SocketAddr, bool), String> {
    {
        let guard = state.lock().unwrap();
        if let Some(g) = guard.gateway.as_ref() {
            if g.write == write {
                return Ok((g.token.clone(), g.addr, g.write));
            }
        }
    }
    stop_remote_gateway(state);
    start_remote_gateway(state, "127.0.0.1", 0, write)
}

#[cfg(test)]
mod ensure_remote_gateway_write_tests {
    use super::*;

    #[test]
    fn starts_with_requested_write_when_down() {
        let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
        let (token, _addr, write) = ensure_remote_gateway_with_write(&state, true).expect("start");
        assert!(write, "应烤进 write=true");
        assert!(!token.is_empty());
        #[cfg(target_os = "macos")]
        assert!(
            state
                .lock()
                .unwrap()
                .gateway
                .as_ref()
                .and_then(|gateway| gateway._sleep_assertion.as_ref())
                .is_some(),
            "远程网关运行时必须持有系统防休眠断言"
        );
        // 现状一致：再要一次可写必须复用同一 token，不能偷偷再起一个
        let (token2, _, write2) = ensure_remote_gateway_with_write(&state, true).expect("reuse");
        assert_eq!(token, token2);
        assert!(write2);
        stop_remote_gateway(&state);
    }

    #[test]
    fn restarts_and_reuses_token_when_write_changes() {
        let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
        let (token_ro, _, write_ro) =
            ensure_remote_gateway_with_write(&state, false).expect("start ro");
        assert!(!write_ro);

        // 关键回归：幂等的 start_remote_gateway 在已开时会忽略传入 write=true，
        // ensure 必须先停再开，否则隧道路径会静默保持只读。
        let (token_rw, _, write_rw) =
            ensure_remote_gateway_with_write(&state, true).expect("upgrade to rw");
        assert!(write_rw, "write 切换后必须变成可写");
        assert_eq!(token_ro, token_rw, "写权限切换不应让已配对手机失效");

        let (token_ro2, _, write_ro2) =
            ensure_remote_gateway_with_write(&state, false).expect("downgrade to ro");
        assert!(!write_ro2);
        assert_eq!(token_rw, token_ro2);
        stop_remote_gateway(&state);
    }

    #[test]
    fn token_persists_until_explicit_rotation() {
        let dir = std::env::temp_dir().join(format!(
            "smeltd-remote-token-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("remote-token");
        let first = load_or_create_remote_token_at(&path).expect("create token");
        let after_restart = load_or_create_remote_token_at(&path).expect("reload token");
        assert_eq!(first, after_restart);

        let state = new_remote_state(Some(first.clone()));
        let rotated = rotate_remote_token_at(&state, &path).expect("rotate token");
        assert_ne!(first, rotated);
        assert_eq!(
            load_or_create_remote_token_at(&path).expect("reload rotated token"),
            rotated
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("token metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(
            state.lock().unwrap().token.as_deref(),
            Some(rotated.as_str())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plain_start_remote_gateway_is_still_idempotent_on_write() {
        // 对照：裸 start_remote_gateway 的旧语义还在——已开时忽略 write 参数。
        // ensure 才是"按 write 对齐"的入口；别把两个行为搞混。
        let state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
        let (t1, _, w1) = start_remote_gateway(&state, "127.0.0.1", 0, false).expect("ro");
        assert!(!w1);
        let (t2, _, w2) = start_remote_gateway(&state, "127.0.0.1", 0, true).expect("idempotent");
        assert_eq!(t1, t2);
        assert!(!w2, "幂等路径必须继续忽略传入的 write=true");
        stop_remote_gateway(&state);
    }
}

mod menubar {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::OnceLock;

    /// 应用图标母图，编进二进制当菜单栏图标（跟 workspace 用的是同一张）。
    const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon-1024.png");

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSSize {
        width: f64,
        height: f64,
    }

    /// 点「打开 smelt」：拉起同目录的 GUI（dev 的 target 目录和 app 包内都叫 smelt）。
    /// 已在跑的话，由 GUI 自己的单实例逻辑负责前置窗口，这里只管发起。
    extern "C" fn on_open(_this: &Object, _cmd: Sel, _sender: *mut Object) {
        if let Ok(exe) = std::env::current_exe() {
            use std::process::Stdio;
            let gui = exe.with_file_name("smelt");
            let _ = std::process::Command::new(gui)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    /// 点「退出 smelt」：整个守护进程退出。注意这会关掉所有 PTY——所有会话（含正在
    /// 跑的 agent）随之结束。后果已写进菜单项文案里。先清 iroh 隧道与远程网关，
    /// 避免端口残留。
    extern "C" fn on_quit(_this: &Object, _cmd: Sel, _sender: *mut Object) {
        super::cleanup_sidecar_services();
        std::process::exit(0);
    }

    /// 注册（仅一次）点击靶子类：AppKit 菜单项只认 target-action，不认 Rust 闭包，
    /// 得声明一个最小的 `NSObject` 子类当靶子（同 status_item.rs 的做法）。
    fn target_class() -> Result<&'static Class, String> {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        if let Some(c) = CLASS.get() {
            return Ok(*c);
        }
        // 已注册过则直接取，避免 ClassDecl::new 返回 None 再 expect 崩掉守护。
        if let Some(existing) = Class::get("SmeltdMenubarTarget") {
            let _ = CLASS.set(existing);
            return Ok(existing);
        }
        let mut decl = ClassDecl::new("SmeltdMenubarTarget", class!(NSObject))
            .ok_or_else(|| "无法声明 SmeltdMenubarTarget".to_string())?;
        unsafe {
            decl.add_method(
                sel!(smeltdOpen:),
                on_open as extern "C" fn(&Object, Sel, *mut Object),
            );
            decl.add_method(
                sel!(smeltdQuit:),
                on_quit as extern "C" fn(&Object, Sel, *mut Object),
            );
        }
        let cls = decl.register();
        let _ = CLASS.set(cls);
        Ok(cls)
    }

    /// `&str` → 临时 `NSString*`（autorelease，仅供本次调用当参数用）。
    unsafe fn nsstring(s: &str) -> *mut Object {
        let c = std::ffi::CString::new(s).unwrap_or_default();
        msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()]
    }

    /// 建菜单栏图标 + 静态菜单，然后跑 AppKit runloop（阻塞到进程退出）。
    /// **必须在主线程调用。** 图标、菜单、靶子实例都常驻到进程退出，故意不释放。
    ///
    /// AppKit 类拿不到时（cargo 直接跑 / 无 GUI 会话 / 框架未加载）返回 Err——
    /// **绝不能 panic**：accept 在别的线程上，主线程 panic 会把整个守护带走，
    /// 留下僵尸 sock，GUI 所有新建会话全失败（表现为「加项目没反应」）。
    pub fn run_event_loop() -> Result<(), String> {
        // class! 宏在类不存在时直接 panic；先用 Class::get 探测。
        if Class::get("NSApplication").is_none() {
            return Err("NSApplication 不可用（AppKit 未加载）".into());
        }
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            // accessory：不占 Dock、不进 ⌘Tab，只在菜单栏留一枚图标。
            // NSApplicationActivationPolicyAccessory == 1。
            let _: bool = msg_send![app, setActivationPolicy: 1i64];

            let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
            // NSVariableStatusItemLength == -1.0，按内容自适应宽度。
            let item: *mut Object = msg_send![bar, statusItemWithLength: -1.0f64];
            let _: () = msg_send![item, retain]; // 常驻单例，自己按住

            let button: *mut Object = msg_send![item, button];
            let data: *mut Object = msg_send![
                class!(NSData),
                dataWithBytes: APP_ICON_PNG.as_ptr() as *const std::ffi::c_void
                length: APP_ICON_PNG.len()
            ];
            let image: *mut Object = msg_send![class!(NSImage), alloc];
            let image: *mut Object = msg_send![image, initWithData: data];
            if !image.is_null() {
                // 母图 1024×1024，菜单栏按 18pt 显示（跟系统自带图标观感对齐）。
                let _: () = msg_send![image, setSize: NSSize { width: 18.0, height: 18.0 }];
                let _: () = msg_send![button, setImage: image];
            } else {
                let _: () = msg_send![button, setTitle: nsstring("smelt")];
            }

            let target_cls = target_class()?;
            let target: *mut Object = msg_send![target_cls, new]; // +1，永不 release
            let menu: *mut Object = msg_send![class!(NSMenu), new]; // +1，永不 release

            let open_item: *mut Object = msg_send![class!(NSMenuItem), alloc];
            let open_item: *mut Object = msg_send![open_item,
                initWithTitle: nsstring("打开 smelt")
                action: sel!(smeltdOpen:)
                keyEquivalent: nsstring("")];
            let _: () = msg_send![open_item, setTarget: target];
            let _: () = msg_send![menu, addItem: open_item];
            let _: () = msg_send![open_item, release];

            let sep: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
            let _: () = msg_send![menu, addItem: sep];

            let quit_item: *mut Object = msg_send![class!(NSMenuItem), alloc];
            let quit_item: *mut Object = msg_send![quit_item,
                initWithTitle: nsstring("退出 smelt（结束所有会话）")
                action: sel!(smeltdQuit:)
                keyEquivalent: nsstring("")];
            let _: () = msg_send![quit_item, setTarget: target];
            let _: () = msg_send![menu, addItem: quit_item];
            let _: () = msg_send![quit_item, release];

            let _: () = msg_send![item, setMenu: menu];

            // 阻塞跑 runloop：菜单点击的 target-action 全靠它派发。
            let _: () = msg_send![app, run];
        }
        Ok(())
    }
}

/// 尽早把本进程的 fd 软上限提到硬上限：macOS 默认软上限只有 256，一个 PTY 会话
/// 至少占掉「主 fd + 子进程 stdin/stdout/stderr」好几个，同时开十几个终端/ACP
/// 会话（这台机器实测常驻会话就有大几十个）很容易顶到上限——顶到之后 spawn
/// 新会话、accept 新连接会静默 EMFILE 失败，界面上只会看到「新建终端没反应」，
/// 排查起来毫无头绪。只调软上限，不碰硬上限；拿不到/调不了就静默放弃，不阻塞启动
/// （极端受限的沙箱环境里 setrlimit 可能被拒绝，那也不该是守护进程启动失败的理由）。
#[cfg(unix)]
fn raise_fd_limit() {
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        // 部分 macOS 环境把硬上限报成 RLIM_INFINITY，直接照报的数申请反而会被拒绝
        // （内核对 NOFILE 另有一个不通过 rlimit 暴露的绝对上限），封顶到一个够用的数。
        let target = if lim.rlim_max == libc::RLIM_INFINITY {
            65536
        } else {
            lim.rlim_max.min(65536)
        };
        if target > lim.rlim_cur {
            lim.rlim_cur = target;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

#[cfg(not(unix))]
fn raise_fd_limit() {}

fn main() {
    // 全 app 通用运行日志（~/.smelt/app.log，默认开、大小有上限，见 app_log 模块）：
    // 先装 panic hook，再记一条启动事件——守护本身没有终端可看，崩溃/异常全靠这份
    // 日志留痕。跟已有的 daemon.log（只记交接/网络这类守护自身生命周期事件）互补。
    smelt_core::app_log::install_panic_hook("smeltd");
    smelt_core::app_log::tee_stderr("smeltd");
    smelt_core::app_log::info("smeltd", "守护启动");
    // 子进程（ACP agent 等）用的是低层 spawn_process 逃生口，SDK 不支持单独
    // 指定子进程 cwd——它们一律继承本进程的 cwd。本进程的 cwd 又是从
    // launchd/Finder 或上一次 `cd` 到的目录继承来的，可能是个已被删除/挪进
    // 废纸篓的目录（比如某次在临时 worktree 里启动过守护，之后那个目录被
    // 清理掉）。cwd 指向不存在的路径时，很多用 Node 写的 CLI（含 Copilot
    // CLI）在启动阶段调 `process.cwd()` 直接抛异常退出——外部表现就是"所有
    // ACP 会话的 initialize 都失败，transport closed"，坑了很久才排出来。
    // 钉死到 HOME：一个几乎不可能被删除、稳定存在的目录，一劳永逸避免这个坑。
    if let Some(home) = std::env::var_os("HOME") {
        if let Err(e) = std::env::set_current_dir(&home) {
            smelt_core::app_log::error(
                "smeltd",
                &format!("启动时 cwd 校正失败（HOME={home:?}）：{e}"),
            );
        }
    }
    // 尽早提 fd 上限：晚了的话，前面已经开的 fd 会先一步顶到旧上限。
    raise_fd_limit();
    // 钉住启动时刻：晚一步取到的就是「首次有人问 version」的时间，不是启动时间。
    started_at();
    // 无缝升级交接：上一代进程 exec 本二进制前写好交接文件并把路径放在环境变量里。
    // 立即摘掉环境变量：它只对"本次 exec 交接"有意义，不能传染给之后 spawn 的 shell。
    let handoff = std::env::var("SMELTD_HANDOFF").ok();
    // Edition 2024：`remove_var` 标为 unsafe（多线程改 env 非同步）。
    // 此处在 main 最开头、尚未 spawn 任何线程，单线程访问安全。
    unsafe { std::env::remove_var("SMELTD_HANDOFF") };
    let came_from_handoff = handoff.is_some();

    let path = sock_path();
    // 不参与无缝升级交接：每次进程启动（含 upgrade 后的新进程）都是全新的空列表——
    // subscribe 连接是网络层面的东西，跟 out.clients/watchers 一样没必要假装还在。
    // 建在 resume_handoff 之前：交接恢复的会话也需要一份 Subscribers 去广播状态。
    let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
    let task_state = tasks::new_task_state();
    let (listener, sessions, acp_sessions) =
        match handoff.and_then(|p| resume_handoff(&p, &subscribers)) {
            Some(x) => {
                let acp_n = x.2.snapshot().len();
                dlog(&format!(
                    "upgrade: 交接完成，恢复 {} 个终端会话 + {acp_n} 个 ACP 会话",
                    x.1.lock().map(|s| s.len()).unwrap_or(0)
                ));
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
                // socket 仅本用户可读写。
                let _ = std::fs::set_permissions(
                    &path,
                    std::os::unix::fs::PermissionsExt::from_mode(0o600),
                );
                (
                    listener,
                    Arc::new(Mutex::new(HashMap::new())),
                    new_acp_sessions(),
                )
            }
        };

    let listen_fd = listener.as_raw_fd();
    let exe_mtime = exe_mtime_secs();
    // 不参与无缝升级交接：每次进程启动（含 upgrade 后的新进程）都是全新的 None，
    // 见 RemoteGateway / IrohTunnel 定义处注释。运行态丢了不等于用户意愿丢了——
    // 下面的 autostart_remote_from_config 会按 collab.json 把它们拉回来。
    let remote_state = new_remote_state(None);
    let iroh_state: IrohState = Arc::new(Mutex::new(None));
    // 全局连接池，由 iroh 隧道回调更新，供 iroh_connections op 查询。
    let iroh_connections: IrohConnections = Arc::new(Mutex::new(HashMap::new()));
    // acp_sessions 现在参与无缝升级交接了（见上面 resume_handoff 的返回值）：
    // 正常冷启动时是空表，upgrade 交接恢复时带着接过来的会话。
    // 菜单栏 quit / 任何路径 cleanup 都要够得着这两份状态。
    register_lifecycle(Arc::clone(&remote_state), Arc::clone(&iroh_state));

    // 远程访问自愈：见 autostart_remote_from_config 的注释。必须在 accept 循环之前
    // 挂起（它自己起线程，不阻塞），否则守护重启后手机要一直等到用户下次开 GUI。
    autostart_remote_from_config(Arc::clone(&remote_state), Arc::clone(&iroh_state), Arc::clone(&iroh_connections));

    // thread-per-connection 的 accept 主循环。抽成闭包，好让主线程在 macOS 上腾出来
    // 跑菜单栏 runloop——AppKit 铁律：NSApplication/NSStatusItem 只能在主线程摸。
    let accept_loop = move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let sessions = Arc::clone(&sessions);
            let acp_sessions = Arc::clone(&acp_sessions);
            let task_state = Arc::clone(&task_state);
            let remote_state = Arc::clone(&remote_state);
            let iroh_state = Arc::clone(&iroh_state);
            let iroh_connections = Arc::clone(&iroh_connections);
            let subscribers = Arc::clone(&subscribers);
            thread::spawn(move || {
                handle_conn(
                    conn,
                    sessions,
                    acp_sessions,
                    task_state,
                    exe_mtime,
                    listen_fd,
                    remote_state,
                    iroh_state,
                    iroh_connections,
                    subscribers,
                )
            });
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
    snapshot: smelt_core::acp_session::AcpSnapshot,
    acp_session_id: String,
    cwd: Option<String>,
    cmd: String,
    agent_needs_transcript_check: bool,
    pending_raw_line: Option<String>,
}

fn snapshot_has_active_turn(snapshot: &smelt_core::acp_session::AcpSnapshot) -> bool {
    matches!(snapshot.phase, smelt_core::acp_session::AcpPhase::Running)
        && snapshot.turn_started_at_ms.is_some()
}

enum AcpHandoffItemValidation {
    SkipUnowned,
    CloseDescriptors { stdin_fd: RawFd, stdout_fd: RawFd },
    CleanupRequired(OwnedAcpHandoff),
    Restore(ValidatedAcpHandoff),
}

fn owned_process_group_cleanup_pid(pid: i32) -> Option<i32> {
    (pid > 1).then_some(pid)
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
        serde_json::from_value::<smelt_core::acp_session::AcpSnapshot>(snapshot_v.clone())
    else {
        return AcpHandoffItemValidation::CleanupRequired(owned);
    };
    let Some(acp_session_id) = snapshot.acp_session_id.clone() else {
        return AcpHandoffItemValidation::CleanupRequired(owned);
    };

    AcpHandoffItemValidation::Restore(ValidatedAcpHandoff {
        id: id.to_string(),
        owned,
        snapshot,
        acp_session_id,
        cwd: item["cwd"].as_str().map(String::from),
        cmd: item["cmd"].as_str().unwrap_or_default().to_string(),
        agent_needs_transcript_check: item["agent_needs_transcript_check"]
            .as_bool()
            .unwrap_or(false),
        pending_raw_line: item["pending_raw_line"].as_str().map(String::from),
    })
}

fn waitpid_retry(pid: i32, options: i32) -> i32 {
    loop {
        let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), options) };
        if waited >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return waited;
        }
    }
}


fn cleanup_rejected_acp_handoff(owned: OwnedAcpHandoff) {
    unsafe {
        libc::close(owned.stdin_fd);
        libc::close(owned.stdout_fd);
    }

    let Some(pid) = owned_process_group_cleanup_pid(owned.pid) else {
        return;
    };

    let group_kill = unsafe { libc::kill(-pid, libc::SIGKILL) };
    if group_kill < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    let initial_wait = waitpid_retry(pid, libc::WNOHANG);
    if initial_wait != 0 {
        return;
    }

    let (reaped_tx, reaped_rx) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let waited = waitpid_retry(pid, 0);
        let _ = reaped_tx.send(waited);
    });
    if reaped_rx.recv_timeout(Duration::from_secs(1)).is_err() {
        dlog(&format!(
            "handoff: ACP pid={} 未在 1 秒内退出，后台继续 waitpid 收尸",
            pid
        ));
    }
}

/// 从交接文件恢复：认领监听 socket 和各会话的 PTY master fd，重建会话表 + 泵线程。
/// 任何全局性错误（文件读不到/解析失败/监听 fd 无效）返回 None 走全新启动——会话
/// 保不住但守护必须活着；单个会话的 fd 坏了只跳过那一个。
fn resume_handoff(
    path: &str,
    subscribers: &Subscribers,
) -> Option<(UnixListener, Sessions, AcpSessions)> {
    let data = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path); // 读到手就删，避免残留被下次启动误认
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;

    let listen_fd = v["listen_fd"].as_i64()? as RawFd;
    // 校验这个 fd 真的有效（exec 前若忘了清 CLOEXEC，这里会拿到无效 fd）。
    if unsafe { libc::fcntl(listen_fd, libc::F_GETFD) } < 0 {
        return None;
    }
    set_cloexec(listen_fd, true);
    let listener = unsafe { UnixListener::from_raw_fd(listen_fd) };

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
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
        if fd < 0 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            continue; // fd 本身缺失/已失效，没有可恢复的东西
        }
        if pid <= 0 {
            // fd 有效但 pid 信息坏了：没法按 pid 去 waitpid/kill 这个孤儿 shell，
            // 干脆关掉 master fd——PTY 挂断会让前台进程组收到 SIGHUP，大概率跟着
            // 退出；不关的话这个 fd 就白白泄漏在新进程里，永远够不着。
            unsafe {
                libc::close(fd);
            }
            continue;
        }
        set_cloexec(fd, true);
        let master = unsafe { std::fs::File::from_raw_fd(fd) };
        let Ok(reader) = master.try_clone() else {
            // master 已被 from_raw_fd 接管，这里 drop 会关掉 fd（PTY 挂断，shell
            // 大概率收到 SIGHUP 退出）；但没有泵线程去 waitpid，起一个一次性收尸
            // 线程，避免它在进程表里挂成永久僵尸。
            drop(master);
            thread::spawn(move || unsafe {
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            });
            continue;
        };
        let cols = item["cols"].as_u64().unwrap_or(80) as u16;
        let rows = item["rows"].as_u64().unwrap_or(24) as u16;
        let cwd = item["cwd"].as_str().map(String::from);
        let launch = item["launch"].as_str().map(String::from);
        let alt_flag = item["alt_screen"].as_bool().unwrap_or(false);
        // 旧 handoff 文件可能仍带 "buf"（环形原始字节）——**忽略，永不 feed**。
        // 状态通道不参与交接：新进程里全新一份 SessionState（launch 会写回，便于
        // snapshot 识别 agent）。hook/OSC 很快会补 phase/title。
        let state = Arc::new(Mutex::new(SessionState {
            id: id.to_string(),
            cwd: cwd.clone(),
            launch: launch.clone(),
            ..Default::default()
        }));
        // —— 画面恢复：全会话同一条路径，不按 shell/TUI/agent 分支 ——
        //
        // 唯一信源：upgrade 时从常驻 Term 导出的 viewport keyframe（`grid`）。
        // 环形字节可能在 CSI 中间腰斩，**永远不 feed**（按类型特判 ring = 拆东墙补西墙）。
        //
        // 无 grid（极老交接文件）：若交接前在备用屏，只注 1049h 模式位；其余空白 + jolt。
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers: Arc::clone(subscribers),
        };
        let mut term = new_daemon_term(rows, cols, listener);
        let grid = item["grid"]
            .as_str()
            .and_then(hex_decode)
            .unwrap_or_default();
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
            geometry_token: uuid::Uuid::new_v4().simple().to_string(),
            ctl: Mutex::new(Ctl {
                master,
                pid,
                // 一律 jolt：有 grid 时对齐真 cell 尺寸；无 grid 时逼进程自绘。
                jolt: true,
                cols,
                rows,
                cell_w: 0,
                cell_h: 0,
                remote_viewports: 0,
                cwd,
            }),
            out: Mutex::new(Out {
                clients: Vec::new(),
                watchers: Vec::new(),
            }),
            term: Mutex::new(term),
            state,
        });
        sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), Arc::clone(&sess));
        start_pty_pump(
            sess,
            Box::new(reader),
            id.to_string(),
            Arc::clone(&sessions),
            Arc::clone(subscribers),
        );
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
                cleanup_rejected_acp_handoff(owned);
                continue;
            }
            AcpHandoffItemValidation::Restore(validated) => validated,
        };
        let ValidatedAcpHandoff {
            id,
            owned,
            snapshot,
            acp_session_id,
            cwd,
            cmd,
            agent_needs_transcript_check,
            pending_raw_line,
        } = validated;
        let supports_image = snapshot.supports_image;
        let snapshot_revision = snapshot.snapshot_revision;
        // phase=Running 但没有开始时间是旧版被迟到 ACP 更新复活的僵尸相位，
        // 不能在无缝升级后继续把下一条 prompt response 误当成它的收尾。
        let recover_running_turn = snapshot_has_active_turn(&snapshot);
        let reduced = smelt_core::acp_session::AcpSessionState::from_snapshot(snapshot);

        let state = Arc::new(Mutex::new(SessionState {
            id: id.clone(),
            cwd: cwd.clone(),
            launch: Some(cmd.clone()),
            ..Default::default()
        }));
        // 无缝升级交接只带了拼好的 cmd 字符串，没有结构化 env；用它兜底重建一份
        // launch_spec——env 丢了没关系，`acp_restart` 大概率也用不上这条兜底
        // 分支（正常场景很快会有一次真正的 acp_relaunch 把它覆盖成完整版本）。
        let launch_spec = Mutex::new(Some(smelt_core::agent_kind::AcpLaunchSpec::from_command(
            cmd,
        )));
        let (slot, created) = acp_sessions.reserve_with(&id, || AcpSession {
            reduced: Mutex::new(reduced),
            snapshot_revision: AtomicU64::new(snapshot_revision),
            handle: Mutex::new(None),
            cwd,
            agent_needs_transcript_check,
            state,
            out: Mutex::new(AcpOut {
                client: None,
                watchers: Vec::new(),
            }),
            launch_spec,
        });
        if !created {
            cleanup_rejected_acp_handoff(owned);
            continue;
        }

        let OwnedAcpHandoff {
            pid,
            stdin_fd,
            stdout_fd,
        } = owned;
        set_cloexec(stdin_fd, true);
        set_cloexec(stdout_fd, true);
        let event_rx = {
            let _lifecycle = slot.lifecycle.lock().unwrap();
            let handle = smelt_core::acp_conn::resume_acp_from_fds(
                id,
                stdin_fd,
                stdout_fd,
                pid,
                acp_session_id,
                supports_image,
                pending_raw_line,
                recover_running_turn,
            );
            let event_rx = handle.event_rx.clone();
            *slot.value.handle.lock().unwrap() = Some(handle);
            event_rx
        };
        // 落地就有一份现成快照，不用等下一次协议事件才让 subscribe 订阅者
        // 看到这条会话——跟终端那边"resume 完成靠后续 PTY 输出自然触发广播"
        // 不同，ACP 没有"泵线程闲着也吐字节"这回事。
        update_acp_daemon_state(&slot.value, subscribers);
        start_acp_event_drain(slot, event_rx, subscribers.clone());
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
    if b.len() % 2 != 0 {
        return None;
    }
    (0..b.len())
        .step_by(2)
        .map(|i| Some((nibble(b[i])? << 4) | nibble(b[i + 1])?))
        .collect()
}

/// `resume_handoff` 的行为——这是「无缝升级」的落地点，也是全文件最该被守住的一段：
/// 它一旦出错，用户正在跑的 agent 会话会在升级瞬间集体消失，且没有任何补救。
/// 此前这里**一个测试都没有**，几条要命的不变量全靠注释。
#[cfg(test)]
mod resume_handoff_tests {
    use super::*;

    fn no_subs() -> Subscribers {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn test_artifact_path(name: &str, extension: &str) -> std::path::PathBuf {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/smeltd-tests");
        std::fs::create_dir_all(&dir).unwrap();
        if extension == "sock" {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            name.hash(&mut hasher);
            return std::fs::canonicalize(dir).unwrap().join(format!(
                "s-{}-{:x}.sock",
                std::process::id(),
                hasher.finish()
            ));
        }
        dir.join(format!(
            "smelt-test-{name}-{}.{}",
            std::process::id(),
            extension
        ))
    }

    /// 每个用例一个独立文件名：测试是多线程并行跑的，共用路径会互相踩。
    fn tmp_handoff(name: &str) -> String {
        test_artifact_path(&format!("handoff-{name}"), "json")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn missing_file_returns_none() {
        let p = tmp_handoff("missing");
        let _ = std::fs::remove_file(&p);
        assert!(resume_handoff(&p, &no_subs()).is_none());
    }

    #[test]
    fn malformed_json_returns_none() {
        let p = tmp_handoff("malformed");
        std::fs::write(&p, "{ this is not json").unwrap();
        assert!(
            resume_handoff(&p, &no_subs()).is_none(),
            "解析失败必须走全新启动，而不是 panic 把守护带走"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 读到手就删：文件残留下来会被下次启动误认成「有交接要恢复」，
    /// 那时里面的 fd 早已属于别的东西。失败路径也必须删。
    #[test]
    fn consumes_handoff_file_even_when_parse_fails() {
        let p = tmp_handoff("consume");
        std::fs::write(&p, "{ not json").unwrap();
        let _ = resume_handoff(&p, &no_subs());
        assert!(
            !std::path::Path::new(&p).exists(),
            "handoff 文件读完必须删掉，无论恢复成功与否"
        );
    }

    #[test]
    fn missing_listen_fd_returns_none() {
        let p = tmp_handoff("no-listen-fd");
        std::fs::write(&p, r#"{"sessions":[]}"#).unwrap();
        assert!(resume_handoff(&p, &no_subs()).is_none());
        let _ = std::fs::remove_file(&p);
    }

    /// exec 前忘了清 CLOEXEC 的话，这里拿到的就是无效 fd——必须识别出来走全新启动，
    /// 而不是把一个野 fd 当监听 socket 用。
    /// 一个绝不会被分配到的 fd 号：远超 ulimit -n，fcntl 必然 EBADF。
    ///
    /// 不能用「open 一个再 close，拿它的号当无效 fd」——测试是多线程并行跑的，
    /// 号一释放就会被别的用例的 pipe() 拿去，于是「无效 fd」其实是别人的活 fd，
    /// resume_handoff 接管后 close 掉，对面就 double close：
    /// `IO Safety violation: owned file descriptor already closed`。这里踩过。
    const NEVER_VALID_FD: RawFd = 1_000_000;

    /// exec 前忘了清 CLOEXEC 的话，这里拿到的就是无效 fd——必须识别出来走全新启动，
    /// 而不是把一个野 fd 当监听 socket 用。
    #[test]
    fn invalid_listen_fd_returns_none() {
        let p = tmp_handoff("bad-listen-fd");
        std::fs::write(
            &p,
            format!(r#"{{"listen_fd":{NEVER_VALID_FD},"sessions":[]}}"#),
        )
        .unwrap();
        assert!(resume_handoff(&p, &no_subs()).is_none());
        let _ = std::fs::remove_file(&p);
    }

    /// 造一个能被 resume_handoff 认领的监听 fd。
    fn make_listen_fd(name: &str) -> RawFd {
        let sock = test_artifact_path(name, "sock");
        let _ = std::fs::remove_file(&sock);
        let l = UnixListener::bind(&sock).unwrap();
        let _ = std::fs::remove_file(&sock); // 已 bind，文件可以立刻删
        std::os::unix::io::IntoRawFd::into_raw_fd(l)
    }

    /// 造一个「PTY master」替身：用管道写端即可——resume_handoff 只是接管 fd、
    /// try_clone 给泵线程，测试不需要真的跑一个 shell。
    /// 返回 (master_fd, 读端保管者, pid)。
    fn make_fake_pty() -> (RawFd, std::fs::File, i32) {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
        let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        // 用一个真实存在过的 pid：让泵线程结束时的 waitpid 有合法目标，不借用 -1
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        drop(child); // 留成 zombie，交给泵收尸
        (fds[1], read_end, pid)
    }

    fn term_text_of(sess: &Arc<Session>) -> String {
        let term = sess.term.lock().unwrap();
        smelt_core::term_text::text_lines(&term).join("\n")
    }

    /// **永不 feed ring**——本文件头号不变量，此前只有注释在守。
    ///
    /// 旧版 handoff 文件会带 `"buf"`（每会话的环形原始字节）。环形缓冲是按容量截断的，
    /// 截断点可能正落在一条 CSI 序列中间，feed 进去必然花屏。所以画面只认从常驻 Term
    /// 导出的 `grid` keyframe，`buf` 即便存在也必须被忽略。
    ///
    /// 这条一旦被「顺手优化」掉（比如有人觉得「没 grid 时用 buf 兜底也行」），
    /// 症状是升级后终端花屏，且只在带旧交接文件的机器上出现——极难复现。
    #[test]
    fn never_feeds_legacy_ring_buffer_even_when_present() {
        let p = tmp_handoff("no-feed-ring");
        let listen_fd = make_listen_fd("no-feed-ring");
        let (master_fd, _read_end, pid) = make_fake_pty();

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [{
                "id": "s1",
                "fd": master_fd,
                "pid": pid,
                "cols": 80,
                "rows": 24,
                // 旧字段：必须被忽略
                "buf": hex_encode(b"RINGBUF-MUST-NOT-RENDER"),
                // 唯一信源
                "grid": hex_encode(b"GRIDKEYFRAME-OK"),
            }]
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_listener, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        let sess = sessions
            .lock()
            .unwrap()
            .get("s1")
            .cloned()
            .expect("会话 s1 应存在");
        let text = term_text_of(&sess);

        assert!(
            text.contains("GRIDKEYFRAME-OK"),
            "grid keyframe 应被 feed：{text:?}"
        );
        assert!(
            !text.contains("RINGBUF"),
            "buf（环形原始字节）绝不能被 feed——它可能在 CSI 中间腰斩，feed 必花屏：{text:?}"
        );
    }

    /// **没有 grid、只有 buf 时，仍然不许 feed buf**——这才是「永不 feed ring」真正
    /// 会被破坏的地方：老版交接文件就是只有 buf 没有 grid，一旦有人觉得
    /// 「没 grid 时拿 buf 兜一下也行」，花屏就回来了。
    ///
    /// 上面那条 `never_feeds_legacy_ring_buffer_even_when_present` 挡不住这种改法
    /// （它的用例里 grid 存在，走不到兜底分支）——变异测试实测漏过。两条都要有。
    #[test]
    fn ignores_buf_when_grid_absent() {
        let p = tmp_handoff("buf-no-grid");
        let listen_fd = make_listen_fd("buf-no-grid");
        let (master_fd, _read_end, pid) = make_fake_pty();

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [{
                "id": "s1", "fd": master_fd, "pid": pid, "cols": 80, "rows": 24,
                // 老版交接文件的形态：只有 buf，没有 grid
                "buf": hex_encode(b"LEGACYRING-MUST-NOT-RENDER"),
            }]
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        let sess = sessions.lock().unwrap().get("s1").cloned().unwrap();
        let text = term_text_of(&sess);
        assert!(
            !text.contains("LEGACYRING"),
            "没有 grid 时也不能拿 buf 兜底——环形字节可能在 CSI 中间腰斩，feed 必花屏。\
             宁可空屏 + jolt 让进程自绘：{text:?}"
        );
    }

    /// fd 已失效的会话只跳过它自己，不能拖垮整次恢复——其余会话必须照常回来。
    #[test]
    fn skips_session_with_dead_fd_but_keeps_the_rest() {
        let p = tmp_handoff("dead-fd");
        let listen_fd = make_listen_fd("dead-fd");
        let (good_fd, _read_end, pid) = make_fake_pty();

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [
                { "id": "dead", "fd": NEVER_VALID_FD, "pid": pid, "cols": 80, "rows": 24 },
                { "id": "good", "fd": good_fd, "pid": pid, "cols": 80, "rows": 24,
                  "grid": hex_encode(b"ALIVE") },
            ]
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        let map = sessions.lock().unwrap();
        assert!(!map.contains_key("dead"), "fd 失效的会话应被跳过");
        assert!(
            map.contains_key("good"),
            "其余会话必须照常恢复，不能被坏的那个拖垮"
        );
    }

    /// 无 grid、且交接前在备用屏：注 1049h 让 TUI 自己重画，
    /// 而不是把它留在主屏上（那样 agent 的界面会叠在 shell 历史上）。
    #[test]
    fn without_grid_alt_screen_flag_enters_alt_mode() {
        let p = tmp_handoff("alt-no-grid");
        let listen_fd = make_listen_fd("alt-no-grid");
        let (master_fd, _read_end, pid) = make_fake_pty();

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [{
                "id": "s1", "fd": master_fd, "pid": pid, "cols": 80, "rows": 24,
                "alt_screen": true,
            }]
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        let sess = sessions.lock().unwrap().get("s1").cloned().unwrap();
        let term = sess.term.lock().unwrap();
        assert!(
            term.mode().contains(TermMode::ALT_SCREEN),
            "交接前在备用屏、又没有 grid 时，应只注 1049h 把 Term 切回备用屏"
        );
    }

    /// 恢复的会话一律挂 jolt：有 grid 时用于对齐真实 cell 尺寸，无 grid 时逼进程自绘。
    #[test]
    fn restored_session_is_marked_for_jolt() {
        let p = tmp_handoff("jolt");
        let listen_fd = make_listen_fd("jolt");
        let (master_fd, _read_end, pid) = make_fake_pty();

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [{
                "id": "s1", "fd": master_fd, "pid": pid, "cols": 80, "rows": 24,
                "grid": hex_encode(b"X"),
            }]
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, sessions, _acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        let sess = sessions.lock().unwrap().get("s1").cloned().unwrap();
        assert!(sess.ctl.lock().unwrap().jolt, "恢复的会话必须挂 jolt");
    }

    /// 造一对能被 resume_handoff 接管的假 stdin/stdout fd（管道即可，不需要
    /// 真的能跑 JSON-RPC——resume_acp_from_fds 只是起个线程去读它，本测试不
    /// 关心那条线程后续读到什么，只关心 resume_handoff 这一步的解析/建表
    /// 逻辑对不对）。返回 (stdin_fd, stdout_fd, 两端读写口保管者, pid)。
    fn make_fake_acp_stdio() -> (RawFd, RawFd, (std::fs::File, std::fs::File), i32) {
        let mut in_fds = [0i32; 2];
        let mut out_fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(in_fds.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(out_fds.as_mut_ptr()) }, 0);
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        drop(child); // 留成 zombie，交给 resume_acp_from_fds 内部的 KillProcessGroupOnDrop 收尾
        // 写端/读端各自保管一份，避免管道另一头因为「没人拿着」直接 EOF。
        let stdin_fd = in_fds[1]; // 交给 resume_handoff 接管（当作"守护写向 agent"）
        let stdout_fd = out_fds[0]; // 交给 resume_handoff 接管（当作"从 agent 读"）
        let keep_alive = unsafe {
            (
                std::fs::File::from_raw_fd(in_fds[0]),
                std::fs::File::from_raw_fd(out_fds[1]),
            )
        };
        (stdin_fd, stdout_fd, keep_alive, pid)
    }

    fn sample_snapshot(acp_session_id: &str) -> smelt_core::acp_session::AcpSnapshot {
        let mut state = smelt_core::acp_session::AcpSessionState::placeholder(
            vec![smelt_core::acp_chat::AcpEntry::User("hi".into())],
            Some(acp_session_id.to_string()),
            String::new(),
        );
        state.acp_session_id = Some(acp_session_id.to_string());
        state.to_snapshot(false)
    }

    #[test]
    fn handoff_only_recovers_a_running_turn_with_an_active_start_marker() {
        let mut snapshot = sample_snapshot("sid");
        snapshot.phase = smelt_core::acp_session::AcpPhase::Running;
        snapshot.turn_started_at_ms = None;
        assert!(!snapshot_has_active_turn(&snapshot));

        snapshot.turn_started_at_ms = Some(123);
        assert!(snapshot_has_active_turn(&snapshot));
    }

    #[test]
    fn missing_acp_snapshot_requires_owned_resource_cleanup() {
        let item = serde_json::json!({
            "id": "acp-missing-snapshot",
            "stdin_fd": 10,
            "stdout_fd": 11,
            "pid": 42,
        });

        let AcpHandoffItemValidation::CleanupRequired(owned) =
            validate_acp_handoff_item(&item, |_| true)
        else {
            panic!("missing snapshot must reject with transferred ownership");
        };
        assert_eq!(
            owned,
            OwnedAcpHandoff {
                pid: 42,
                stdin_fd: 10,
                stdout_fd: 11,
            }
        );
    }

    #[test]
    fn malformed_acp_snapshot_requires_owned_resource_cleanup() {
        let item = serde_json::json!({
            "id": "acp-malformed-snapshot",
            "stdin_fd": 20,
            "stdout_fd": 21,
            "pid": 84,
            "snapshot": {"not": "an ACP snapshot"},
        });

        let AcpHandoffItemValidation::CleanupRequired(owned) =
            validate_acp_handoff_item(&item, |_| true)
        else {
            panic!("malformed snapshot must reject with transferred ownership");
        };
        assert_eq!(
            owned,
            OwnedAcpHandoff {
                pid: 84,
                stdin_fd: 20,
                stdout_fd: 21,
            }
        );
    }

    #[test]
    fn pid_one_is_never_accepted_as_owned_cleanup_target() {
        let item = serde_json::json!({
            "id": "acp-pid-one",
            "stdin_fd": 30,
            "stdout_fd": 31,
            "pid": 1,
            "snapshot": sample_snapshot("sid-pid-one"),
        });

        let validation = validate_acp_handoff_item(&item, |_| true);
        let AcpHandoffItemValidation::CloseDescriptors {
            stdin_fd,
            stdout_fd,
        } = validation
        else {
            panic!("pid 1 must never be accepted as an owned cleanup target");
        };
        assert_eq!((stdin_fd, stdout_fd), (30, 31));
    }

    #[test]
    fn rejected_acp_handoff_closes_owned_fds_and_reaps_agent() {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;

        let p = tmp_handoff("acp-rejected-cleanup");
        let listen_fd = make_listen_fd("acp-rejected-cleanup");
        let mut stdin_pipe = [0; 2];
        let mut stdout_pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(stdin_pipe.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(stdout_pipe.as_mut_ptr()) }, 0);
        let stdin_read = unsafe { std::fs::File::from_raw_fd(stdin_pipe[0]) };
        let stdin_write = unsafe { std::fs::File::from_raw_fd(stdin_pipe[1]) };
        let stdout_read = unsafe { std::fs::File::from_raw_fd(stdout_pipe[0]) };
        let stdout_write = unsafe { std::fs::File::from_raw_fd(stdout_pipe[1]) };
        let inherited_stdin_fd = unsafe { libc::dup(stdin_write.as_raw_fd()) };
        let inherited_stdout_fd = unsafe { libc::dup(stdout_read.as_raw_fd()) };
        assert!(inherited_stdin_fd >= 0 && inherited_stdout_fd >= 0);

        let child = std::process::Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id() as i32;

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [],
            "acp_sessions": [{
                "id": "acp-rejected",
                "stdin_fd": inherited_stdin_fd,
                "stdout_fd": inherited_stdout_fd,
                "pid": pid,
            }],
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_listener, _sessions, acp) =
            resume_handoff(&p, &no_subs()).expect("handoff should remain globally valid");
        assert!(acp.get("acp-rejected").is_none());
        assert_eq!(
            unsafe { libc::fcntl(inherited_stdin_fd, libc::F_GETFD) },
            -1,
            "rejected inherited stdin fd must be closed"
        );
        assert_eq!(
            unsafe { libc::fcntl(inherited_stdout_fd, libc::F_GETFD) },
            -1,
            "rejected inherited stdout fd must be closed"
        );
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1,
            "cleanup must reap the rejected ACP child"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD),
            "waitpid must fail specifically because no unreaped child remains"
        );

        drop((child, stdin_read, stdin_write, stdout_read, stdout_write));
    }

    #[test]
    fn acp_session_with_valid_fds_and_session_id_is_recovered() {
        let p = tmp_handoff("acp-ok");
        let listen_fd = make_listen_fd("acp-ok");
        let (stdin_fd, stdout_fd, _keep_alive, pid) = make_fake_acp_stdio();

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [],
            "acp_sessions": [{
                "id": "acp-1",
                "stdin_fd": stdin_fd,
                "stdout_fd": stdout_fd,
                "pid": pid,
                "cwd": "/tmp/proj",
                "cmd": "claude --dangerously-skip-permissions",
                "agent_needs_transcript_check": true,
                "snapshot": sample_snapshot("sid-1"),
                "pending_raw_line": null,
            }],
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        let slot = acp.get("acp-1").expect("acp-1 应被恢复");
        let sess = &slot.value;
        assert_eq!(sess.cwd.as_deref(), Some("/tmp/proj"));
        assert!(sess.agent_needs_transcript_check);
        assert!(
            sess.handle.lock().unwrap().is_some(),
            "应该已经起了 resume 连接"
        );
        let reduced = sess.reduced.lock().unwrap();
        assert_eq!(reduced.entries.len(), 1);
        assert_eq!(reduced.acp_session_id.as_deref(), Some("sid-1"));
        assert_eq!(reduced.history_session_id.as_deref(), Some("sid-1"));
    }

    /// 没有 agent 侧 session id 就没法 attach_session——理论上不该发生，但
    /// 交接文件是外部数据，得防御性地跳过而不是 panic 或者留一条永远连不上
    /// 的死会话。
    #[test]
    fn acp_session_without_session_id_is_skipped() {
        let p = tmp_handoff("acp-no-sid");
        let listen_fd = make_listen_fd("acp-no-sid");
        let (stdin_fd, stdout_fd, _keep_alive, pid) = make_fake_acp_stdio();

        let mut snapshot = sample_snapshot("whatever");
        snapshot.acp_session_id = None;
        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [],
            "acp_sessions": [{
                "id": "acp-2",
                "stdin_fd": stdin_fd,
                "stdout_fd": stdout_fd,
                "pid": pid,
                "cwd": null,
                "cmd": "claude",
                "agent_needs_transcript_check": true,
                "snapshot": snapshot,
                "pending_raw_line": null,
            }],
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        assert!(acp.get("acp-2").is_none());
    }

    /// fd 号本身失效（exec 前忘了清 CLOEXEC 之类）：必须跳过，不能把野 fd
    /// 当成活的 stdin/stdout 去用。
    #[test]
    fn acp_session_with_invalid_fd_is_skipped() {
        let p = tmp_handoff("acp-bad-fd");
        let listen_fd = make_listen_fd("acp-bad-fd");

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [],
            "acp_sessions": [{
                "id": "acp-3",
                "stdin_fd": NEVER_VALID_FD,
                "stdout_fd": NEVER_VALID_FD,
                "pid": 1,
                "cwd": null,
                "cmd": "claude",
                "agent_needs_transcript_check": true,
                "snapshot": sample_snapshot("sid-3"),
                "pending_raw_line": null,
            }],
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        assert!(acp.get("acp-3").is_none());
    }

    /// 正卡着一张审批卡片时，pending_raw_line 应该原样透传进 resume_handoff
    /// （具体会不会被正确回放是 acp_conn::resume_acp_from_fds 的职责，这里
    /// 只验证 resume_handoff 这一层没有把它弄丢/挡在门外）。
    #[test]
    fn acp_session_with_pending_raw_line_still_recovers() {
        let p = tmp_handoff("acp-pending");
        let listen_fd = make_listen_fd("acp-pending");
        let (stdin_fd, stdout_fd, _keep_alive, pid) = make_fake_acp_stdio();

        let handoff = serde_json::json!({
            "listen_fd": listen_fd,
            "sessions": [],
            "acp_sessions": [{
                "id": "acp-4",
                "stdin_fd": stdin_fd,
                "stdout_fd": stdout_fd,
                "pid": pid,
                "cwd": null,
                "cmd": "claude",
                "agent_needs_transcript_check": true,
                "snapshot": sample_snapshot("sid-4"),
                "pending_raw_line": r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{}}"#,
            }],
        });
        std::fs::write(&p, handoff.to_string()).unwrap();

        let (_l, _sessions, acp) = resume_handoff(&p, &no_subs()).expect("应能恢复");
        assert!(acp.get("acp-4").is_some());
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    /// grid keyframe 的 hex 字段必须逐字节还原。
    #[test]
    fn hex_roundtrip() {
        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        assert_eq!(
            hex_decode(&hex_encode(&data)).as_deref(),
            Some(data.as_slice())
        );
        assert_eq!(hex_decode("").as_deref(), Some(&[][..]));
        assert_eq!(hex_decode("abc"), None, "奇数长度应判非法");
        assert_eq!(hex_decode("zz"), None, "非 hex 字符应判非法");
    }

    /// 损坏的 hex 字段（多字节 UTF-8）只判非法、绝不 panic。
    #[test]
    fn hex_decode_never_panics_on_multibyte_utf8() {
        assert_eq!(hex_decode("中文"), None); // 6 字节，偶数，非 hex 字符
        assert_eq!(hex_decode("a中"), None); // 1 + 3 字节，奇偶交叉
        assert_eq!(hex_decode("ab中c"), None);
    }

    #[test]
    fn buf_detects_alt_screen() {
        assert!(buf_looks_like_alt_screen(b"\x1b[?1049hTUI"));
        assert!(!buf_looks_like_alt_screen(b"plain shell"));
    }
}

#[cfg(test)]
mod state_listener_tests {
    use super::*;

    fn no_subscribers() -> Subscribers {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn notification(kind: OscNotificationKind) -> OscNotification {
        OscNotification {
            kind,
            text: "done".into(),
        }
    }

    #[test]
    fn codex_osc9_completes_stale_running_state() {
        for phase in [Phase::Idle, Phase::Thinking, Phase::ExecutingTool] {
            let mut state = SessionState {
                launch: Some("codex --dangerously-bypass-approvals-and-sandbox".into()),
                phase,
                structured_events: true,
                pending_question: Some("Bash".into()),
                ..Default::default()
            };
            assert!(apply_osc_notification(
                &mut state,
                &notification(OscNotificationKind::Osc9)
            ));
            assert_eq!(state.phase, Phase::Succeeded);
            assert_eq!(state.pending_question.as_deref(), Some("done"));
        }
    }

    #[test]
    fn osc_state_reducer_survives_pty_read_boundaries() {
        let mut state = SessionState {
            launch: Some("codex".into()),
            phase: Phase::ExecutingTool,
            structured_events: true,
            ..Default::default()
        };
        let mut scanner = OscScan::default();

        assert!(!apply_osc_bytes(
            &mut state,
            &mut scanner,
            b"output\x1b]9;turn "
        ));
        assert_eq!(state.phase, Phase::ExecutingTool);
        assert!(apply_osc_bytes(
            &mut state,
            &mut scanner,
            b"complete\x1b\\prompt"
        ));
        assert_eq!(state.phase, Phase::Succeeded);
    }

    #[test]
    fn osc_fallback_never_overrides_stronger_or_unrelated_state() {
        for phase in [
            Phase::AwaitingApproval,
            Phase::WaitingForUser,
            Phase::Succeeded,
            Phase::Failed,
            Phase::Dead,
        ] {
            let mut state = SessionState {
                launch: Some("codex".into()),
                phase,
                ..Default::default()
            };
            assert!(!apply_osc_notification(
                &mut state,
                &notification(OscNotificationKind::Osc9)
            ));
            assert_eq!(state.phase, phase);
        }

        let mut non_codex = SessionState {
            launch: Some("claude".into()),
            phase: Phase::Thinking,
            ..Default::default()
        };
        assert!(!apply_osc_notification(
            &mut non_codex,
            &notification(OscNotificationKind::Osc9)
        ));

        non_codex.launch = Some("codex-helper".into());
        assert!(!apply_osc_notification(
            &mut non_codex,
            &notification(OscNotificationKind::Osc9)
        ));

        let mut kitty = SessionState {
            launch: Some("codex".into()),
            phase: Phase::Thinking,
            ..Default::default()
        };
        assert!(!apply_osc_notification(
            &mut kitty,
            &notification(OscNotificationKind::Osc99)
        ));
    }

    /// 标题以 spinner 开头 → 认定 Thinking，且更新 phase_since；标题本身也要存。
    #[test]
    fn title_with_spinner_sets_thinking_phase() {
        let state = Arc::new(Mutex::new(SessionState::default()));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers: no_subscribers(),
        };
        listener.send_event(Event::Title("⠋ doing work".to_string()));

        let st = state.lock().unwrap();
        assert_eq!(st.phase, Phase::Thinking);
        assert_eq!(st.title.as_deref(), Some("⠋ doing work"));
        assert!(st.phase_since > 0);
    }

    /// 标题不是 spinner 时**不猜**别的 phase——缺乏证据不等于 idle，避免把更
    /// 可信的信源（hook state op）刚写好的值带偏。标题本身还是要照存。
    #[test]
    fn title_without_spinner_does_not_touch_phase() {
        let state = Arc::new(Mutex::new(SessionState {
            phase: Phase::AwaitingApproval,
            ..Default::default()
        }));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers: no_subscribers(),
        };
        listener.send_event(Event::Title("zsh %".to_string()));

        let st = state.lock().unwrap();
        assert_eq!(st.phase, Phase::AwaitingApproval, "不该被标题猜测覆盖");
        assert_eq!(st.title.as_deref(), Some("zsh %"));
    }

    /// spinner 是最低可信度信源：不得盖掉 hook 已写入的等待/审批/执行态，
    /// 否则远程 action 门闩会误判「agent 不在等你」。
    #[test]
    fn spinner_title_does_not_override_awaiting_approval() {
        let state = Arc::new(Mutex::new(SessionState {
            phase: Phase::AwaitingApproval,
            phase_since: 1,
            ..Default::default()
        }));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers: no_subscribers(),
        };
        listener.send_event(Event::Title("⠋ waiting for permission".to_string()));

        let st = state.lock().unwrap();
        assert_eq!(
            st.phase,
            Phase::AwaitingApproval,
            "spinner 不得覆盖 AwaitingApproval"
        );
        assert_eq!(st.phase_since, 1, "phase_since 也不该被 spinner 刷新");
        assert_eq!(st.title.as_deref(), Some("⠋ waiting for permission"));
    }

    /// 已在 Thinking 中时，spinner 换帧不得重置 phase_since。agent 思考时 spinner
    /// 每秒换一帧（⠋→⠙→⠹…），帧帧都是一次 Title 事件；若每帧都把起点推到 now，
    /// 「已思考 N 秒」就永远在 0~1 之间跳——上面那条 AwaitingApproval 用例覆盖不到
    /// 这条路径（它压根进不了 Idle|Thinking 分支）。
    #[test]
    fn spinner_frame_does_not_refresh_phase_since_while_thinking() {
        let state = Arc::new(Mutex::new(SessionState {
            phase: Phase::Thinking,
            phase_since: 1,
            ..Default::default()
        }));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers: no_subscribers(),
        };
        listener.send_event(Event::Title("⠙ still thinking".to_string()));

        let st = state.lock().unwrap();
        assert_eq!(st.phase, Phase::Thinking);
        assert_eq!(st.phase_since, 1, "spinner 换帧不该把思考计时起点推到 now");
        assert_eq!(st.title.as_deref(), Some("⠙ still thinking"));
    }

    #[test]
    fn spinner_title_does_not_override_executing_tool_or_dead() {
        for phase in [Phase::ExecutingTool, Phase::WaitingForUser, Phase::Dead] {
            let state = Arc::new(Mutex::new(SessionState {
                phase,
                ..Default::default()
            }));
            let listener = StateListener {
                state: Arc::clone(&state),
                subscribers: no_subscribers(),
            };
            listener.send_event(Event::Title("⠋ busy".to_string()));
            assert_eq!(
                state.lock().unwrap().phase,
                phase,
                "{phase:?} 不得被 spinner 覆盖"
            );
        }
    }

    /// Bell 只更新时间戳，不改 phase——单独响铃太不可靠，只能当辅助信号。
    #[test]
    fn bell_touches_timestamp_without_changing_phase() {
        let state = Arc::new(Mutex::new(SessionState {
            phase: Phase::Idle,
            ..Default::default()
        }));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers: no_subscribers(),
        };
        listener.send_event(Event::Bell);

        let st = state.lock().unwrap();
        assert_eq!(st.phase, Phase::Idle);
        assert!(st.updated_at > 0);
    }

    /// 广播：state 变化后，所有订阅者都该收到一行 `{"session": ...}`。
    #[test]
    fn send_event_broadcasts_to_subscribers() {
        let (a, mut a_client) = UnixStream::pair().unwrap();
        let subscribers: Subscribers = Arc::new(Mutex::new(vec![a]));
        let state = Arc::new(Mutex::new(SessionState {
            id: "t".into(),
            ..Default::default()
        }));
        let listener = StateListener { state, subscribers };
        listener.send_event(Event::Title("⠋ working".to_string()));

        let mut line = String::new();
        BufReader::new(&mut a_client).read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["session"]["id"], "t");
        assert_eq!(v["session"]["phase"], "thinking");
    }
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

#[cfg(test)]
mod action_tests {
    use super::*;

    #[test]
    fn approve_is_bare_enter() {
        assert_eq!(action_payload(Some("approve"), None), Ok(b"\r".to_vec()));
    }

    #[test]
    fn deny_is_bare_escape_not_arrow_navigation() {
        // 故意不测"按几次下方向键"——这条路本身就不成立（菜单选项数量不是常数，
        // 见模块注释「远程操控」一节）。Esc 不依赖菜单结构。
        assert_eq!(action_payload(Some("deny"), None), Ok(b"\x1b".to_vec()));
    }

    #[test]
    fn reply_appends_enter_after_text() {
        assert_eq!(
            action_payload(Some("reply"), Some("不用了，换个方式")),
            Ok("不用了，换个方式\r".as_bytes().to_vec())
        );
    }

    #[test]
    fn reply_without_text_is_rejected() {
        assert_eq!(action_payload(Some("reply"), None), Err("需要非空 text"));
        assert_eq!(
            action_payload(Some("reply"), Some("")),
            Err("需要非空 text")
        );
    }

    #[test]
    fn unknown_kind_returns_err() {
        assert_eq!(
            action_payload(Some("do_a_barrel_roll"), None),
            Err("未知 kind")
        );
        assert_eq!(action_payload(None, None), Err("未知 kind"));
    }
}

#[cfg(test)]
mod input_payload_tests {
    use super::*;

    #[test]
    fn data_string_becomes_utf8_bytes() {
        let v = serde_json::json!({ "data": "hello" });
        assert_eq!(input_payload(&v), Some(b"hello".to_vec()));
    }

    #[test]
    fn control_chars_in_json_string_work() {
        // Ctrl+C = \u0003；xterm onData + JSON.stringify 就是这条路
        let v = serde_json::json!({ "data": "" });
        assert_eq!(input_payload(&v), Some(vec![0x03]));
    }

    #[test]
    fn empty_or_missing_data_is_none() {
        assert_eq!(input_payload(&serde_json::json!({ "data": "" })), None);
        assert_eq!(input_payload(&serde_json::json!({})), None);
        assert_eq!(input_payload(&serde_json::json!({ "data": null })), None);
    }
}

/// 端到端走真实的 `handle_conn` 分发，而不是只测 action_payload 这个纯函数——
/// 门闩逻辑（phase 不对就拒绝、不实际写入）本身也得有测试盯着，不能只信任
/// action_payload 测过就够了。
#[cfg(test)]
mod resize_bounds_tests {
    use super::*;

    /// 同 action_integration_tests::make_pipe_session 的构造，独立一份是为了让这
    /// 组测试不依赖那边的 Phase/管道语义——这里只关心几何。
    fn make_session(rows: u16, cols: u16) -> Arc<Session> {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
        // 读端留着不关，否则往写端 resize 时可能吃 SIGPIPE。
        let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        std::mem::forget(read_end);
        let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };

        let state = Arc::new(Mutex::new(SessionState::default()));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers: Arc::new(Mutex::new(Vec::new())),
        };
        Arc::new(Session {
            geometry_token: uuid::Uuid::new_v4().simple().to_string(),
            ctl: Mutex::new(Ctl {
                master,
                pid: -1,
                jolt: false,
                cols,
                rows,
                cell_w: 0,
                cell_h: 0,
                remote_viewports: 0,
                cwd: None,
            }),
            out: Mutex::new(Out {
                clients: Vec::new(),
                watchers: Vec::new(),
            }),
            term: Mutex::new(new_daemon_term(rows, cols, listener)),
            state,
        })
    }

    /// 网格是一次性分配出来的，所以一个离谱的尺寸不是"显示得难看"，是 abort 掉
    /// 守护进程、把这台机器上所有会话一起带走。in-band resize 帧不像 attach 的
    /// JSON 那样在调用点做过约束，任何能连上 socket 的东西都能发，所以下界必须
    /// 落在它们共同经过的这一层。
    #[test]
    fn an_absurd_remote_resize_is_clamped_instead_of_killing_the_daemon() {
        let sess = make_session(24, 80);

        resize_session_remote(&sess, u16::MAX, u16::MAX, u16::MAX, u16::MAX);

        let ctl = sess.ctl.lock().unwrap();
        assert_eq!(ctl.cols, MAX_SESSION_COLS);
        assert_eq!(ctl.rows, MAX_SESSION_ROWS);
        assert_eq!(ctl.cell_w, MAX_SESSION_CELL_PX);
        assert_eq!(ctl.cell_h, MAX_SESSION_CELL_PX);
    }

    /// 钳制只该对离谱值生效——真实终端尺寸必须原样落地，否则这个补丁就把正常
    /// 的 resize 一起改坏了。
    #[test]
    fn an_ordinary_resize_still_lands_untouched() {
        let sess = make_session(24, 80);

        resize_session_remote(&sess, 120, 40, 9, 18);

        let ctl = sess.ctl.lock().unwrap();
        assert_eq!(ctl.cols, 120);
        assert_eq!(ctl.rows, 40);
        assert_eq!(ctl.cell_w, 9);
        assert_eq!(ctl.cell_h, 18);
    }

    /// 零是"没测到"的意思，不是"要一个 0 宽的终端"。
    #[test]
    fn a_zero_sized_resize_falls_back_to_one_cell() {
        let sess = make_session(24, 80);

        resize_session_remote(&sess, 0, 0, 0, 0);

        let ctl = sess.ctl.lock().unwrap();
        assert_eq!(ctl.cols, 1);
        assert_eq!(ctl.rows, 1);
    }
}

#[cfg(test)]
mod action_integration_tests {
    use super::*;

    /// `Ctl.master` 用一根真管道（不是 /dev/null）：这样能从另一头读回真正写
    /// 进去的字节，验证 action 落地的到底是不是预期的按键序列，不是只看 `ok`。
    fn make_pipe_session(rows: u16, cols: u16, phase: Phase) -> (Arc<Session>, std::fs::File) {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
        let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };

        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        drop(child); // 留成 zombie，这个测试不需要真的收尸

        let state = Arc::new(Mutex::new(SessionState {
            phase,
            ..Default::default()
        }));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers,
        };
        let sess = Arc::new(Session {
            geometry_token: uuid::Uuid::new_v4().simple().to_string(),
            ctl: Mutex::new(Ctl {
                master,
                pid,
                jolt: false,
                cols,
                rows,
                cell_w: 0,
                cell_h: 0,
                remote_viewports: 0,
                cwd: None,
            }),
            out: Mutex::new(Out {
                clients: Vec::new(),
                watchers: Vec::new(),
            }),
            term: Mutex::new(new_daemon_term(rows, cols, listener)),
            state,
        });
        (sess, read_end)
    }

    /// 直接走 handle_conn 的真实分发（不是绕过去调内部函数）：action 是一次性
    /// 请求-响应，不像 watch/open 那样要开线程陪它跑一辈子。
    fn call_action(sessions: &Sessions, id: &str, kind: &str) -> serde_json::Value {
        let (server, client) = UnixStream::pair().unwrap();
        let remote_state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
        let iroh_state: IrohState = Arc::new(Mutex::new(None));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let mut client = client;
        writeln!(
            client,
            "{}",
            serde_json::json!({ "op": "action", "id": id, "kind": kind })
        )
        .unwrap();
        handle_conn(
            server,
            Arc::clone(sessions),
            new_acp_sessions(),
            tasks::new_task_state(),
            0,
            -1,
            remote_state,
            iroh_state,
            Arc::new(Mutex::new(HashMap::new())),
            subscribers,
        );
        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        serde_json::from_str(&resp).unwrap()
    }

    #[test]
    fn approve_writes_bare_enter_when_awaiting_approval() {
        let (sess, mut read_end) = make_pipe_session(24, 80, Phase::AwaitingApproval);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions.lock().unwrap().insert("a".to_string(), sess);

        let resp = call_action(&sessions, "a", "approve");
        assert_eq!(resp["ok"], true, "resp={resp}");

        let mut buf = [0u8; 8];
        let n = read_end.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"\r");
    }

    #[test]
    fn deny_writes_bare_escape_when_waiting_for_user() {
        let (sess, mut read_end) = make_pipe_session(24, 80, Phase::WaitingForUser);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions.lock().unwrap().insert("b".to_string(), sess);

        let resp = call_action(&sessions, "b", "deny");
        assert_eq!(resp["ok"], true, "resp={resp}");

        let mut buf = [0u8; 8];
        let n = read_end.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"\x1b");
    }

    /// 门闩：phase 是 Thinking（agent 正忙）时，action 必须被拒绝，且**真的没有
    /// 写入任何字节**——不能只是回错误但底下偷偷写了。
    #[test]
    fn action_rejected_and_no_bytes_written_when_agent_busy() {
        let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Thinking);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions.lock().unwrap().insert("c".to_string(), sess);

        let resp = call_action(&sessions, "c", "approve");
        assert_eq!(resp["ok"], false);
        assert!(
            resp["err"].as_str().unwrap().contains("不是在等你"),
            "resp={resp}"
        );

        // 管道写端没收到任何字节：把它设成非阻塞读一下，读不到东西才对。
        use std::os::fd::AsRawFd;
        unsafe {
            let flags = libc::fcntl(read_end.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(
                read_end.as_raw_fd(),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            );
        }
        let mut buf = [0u8; 8];
        let result = read_end.read(&mut buf);
        assert!(result.is_err(), "门闩失效：agent 忙的时候还是写进去了字节");
    }

    #[test]
    fn action_on_unknown_session_is_rejected() {
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let resp = call_action(&sessions, "does-not-exist", "approve");
        assert_eq!(resp["ok"], false);
    }
}

/// `input` 端到端：无 phase 门闩——agent 忙也能写（跟 action 最关键的差异）；
/// 这是「远程 = 工作延续」的契约，回归测试必须盯死。
#[cfg(test)]
mod input_integration_tests {
    use super::*;

    fn make_pipe_session(rows: u16, cols: u16, phase: Phase) -> (Arc<Session>, std::fs::File) {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe() 失败");
        let read_end = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let master = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        drop(child);
        let state = Arc::new(Mutex::new(SessionState {
            phase,
            ..Default::default()
        }));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers,
        };
        let sess = Arc::new(Session {
            geometry_token: uuid::Uuid::new_v4().simple().to_string(),
            ctl: Mutex::new(Ctl {
                master,
                pid,
                jolt: false,
                cols,
                rows,
                cell_w: 0,
                cell_h: 0,
                remote_viewports: 0,
                cwd: None,
            }),
            out: Mutex::new(Out {
                clients: Vec::new(),
                watchers: Vec::new(),
            }),
            term: Mutex::new(new_daemon_term(rows, cols, listener)),
            state,
        });
        (sess, read_end)
    }

    fn call_input(sessions: &Sessions, id: &str, data: &str) -> serde_json::Value {
        let (server, client) = UnixStream::pair().unwrap();
        let remote_state = new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string()));
        let iroh_state: IrohState = Arc::new(Mutex::new(None));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let mut client = client;
        writeln!(
            client,
            "{}",
            serde_json::json!({ "op": "input", "id": id, "data": data })
        )
        .unwrap();
        handle_conn(
            server,
            Arc::clone(sessions),
            new_acp_sessions(),
            tasks::new_task_state(),
            0,
            -1,
            remote_state,
            iroh_state,
            Arc::new(Mutex::new(HashMap::new())),
            subscribers,
        );
        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        serde_json::from_str(&resp).unwrap()
    }

    #[test]
    fn input_writes_even_when_agent_is_thinking() {
        let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Thinking);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions.lock().unwrap().insert("busy".to_string(), sess);

        // Ctrl+C（0x03）：json! 直接嵌 char，serde 编进 JSON 字符串
        let ctrl_c = "\u{0003}";
        let resp = call_input(&sessions, "busy", ctrl_c);
        assert_eq!(resp["ok"], true, "resp={resp}");

        let mut buf = [0u8; 8];
        let n = read_end.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"\x03");
    }

    #[test]
    fn input_writes_text_while_idle() {
        let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Idle);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions.lock().unwrap().insert("idle".to_string(), sess);

        let resp = call_input(&sessions, "idle", "ls -la\r");
        assert_eq!(resp["ok"], true, "resp={resp}");

        let mut buf = [0u8; 64];
        let n = read_end.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ls -la\r");
    }

    #[test]
    fn empty_input_is_rejected() {
        let (sess, mut read_end) = make_pipe_session(24, 80, Phase::Idle);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions.lock().unwrap().insert("e".to_string(), sess);

        let resp = call_input(&sessions, "e", "");
        assert_eq!(resp["ok"], false);
        assert!(
            resp["err"].as_str().unwrap().contains("data"),
            "resp={resp}"
        );

        use std::os::fd::AsRawFd;
        unsafe {
            let flags = libc::fcntl(read_end.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(
                read_end.as_raw_fd(),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            );
        }
        let mut buf = [0u8; 8];
        assert!(read_end.read(&mut buf).is_err(), "空 input 不该写字节");
    }

    #[test]
    fn input_on_unknown_session_is_rejected() {
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let resp = call_input(&sessions, "nope", "x");
        assert_eq!(resp["ok"], false);
    }
}

fn handle_conn(
    conn: UnixStream,
    sessions: Sessions,
    acp_sessions: AcpSessions,
    task_state: TaskState,
    exe_mtime: u64,
    listen_fd: RawFd,
    remote_state: RemoteState,
    iroh_state: IrohState,
    iroh_connections: IrohConnections,
    subscribers: Subscribers,
) {
    // 头一行 JSON。之后的帧字节可能已被 BufReader 预读，故帧循环必须复用同一个 reader。
    let Ok(rc) = conn.try_clone() else { return };
    let mut reader = BufReader::new(rc);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
        return;
    };

    match v["op"].as_str() {
        Some("open") => handle_open(conn, reader, &v, sessions, Arc::clone(&subscribers)),
        Some("watch") => handle_watch(conn, reader, &v, sessions),
        Some("subscribe") => handle_subscribe(conn, &sessions, &acp_sessions, &subscribers),
        Some("acp_open") => handle_acp_open(conn, reader, &v, acp_sessions, subscribers),
        Some("acp_create") => handle_acp_create(conn, &v, &acp_sessions, &subscribers),
        Some("acp_watch") => handle_acp_watch(conn, reader, &v, acp_sessions),
        Some("acp_snapshot") => handle_acp_snapshot(conn, &v, &acp_sessions),
        Some("acp_action") => handle_acp_action(conn, &v, &acp_sessions, &subscribers),
        Some("acp_kill") => handle_acp_kill(conn, &v, &acp_sessions),
        Some("acp_restart") => handle_acp_restart(conn, &v, &acp_sessions, &subscribers),
        Some("task_add") => tasks::handle_task_add(conn, &task_state, &v),
        Some("task_list") => tasks::handle_task_list(conn, &task_state, &v),
        Some("task_update") => tasks::handle_task_update(conn, &task_state, &v),
        Some("task_remove") => tasks::handle_task_remove(conn, &task_state, &v),
        Some("task_done") => tasks::handle_task_done(conn, &task_state, &v),
        Some("task_claim") => tasks::handle_task_claim(conn, &task_state, &v),
        Some("task_begin_run") => tasks::handle_task_begin_run(conn, &task_state, &v),
        Some("task_attach_session") => tasks::handle_task_attach_session(conn, &task_state, &v),
        Some("task_session_done") => tasks::handle_task_session_done(conn, &task_state, &v),
        Some("task_session_failed") => tasks::handle_task_session_failed(conn, &task_state, &v),
        Some("task_run_failed") => tasks::handle_task_run_failed(conn, &task_state, &v),
        Some("task_due") => tasks::handle_task_due(conn, &task_state, &v),
        Some("task_runs_for") => tasks::handle_task_runs_for(conn, &task_state, &v),
        Some("list") => {
            // 附带每个会话是否「有客户端连接」（connected），供 GUI 每次启动时
            // 自动清理：死会话和长期无人认领的游离会话都没有连接，一个字段覆盖
            // 两种场景。正常使用中的会话（GUI/远程/移动端正 attach 或旁观）都有
            // 连接，不会被误清。
            let mut ids: Vec<String> = Vec::new();
            let mut states: Vec<serde_json::Value> = Vec::new();
            {
                let sessions = sessions.lock().unwrap();
                for (id, s) in sessions.iter() {
                    let mut v = serde_json::to_value(s.state.lock().unwrap().clone())
                        .unwrap_or(serde_json::Value::Null);
                    let (interactive_connections, watcher_connections) = {
                        let out = s.out.lock().unwrap();
                        (out.clients.len(), out.watchers.len())
                    };
                    let connected = interactive_connections > 0 || watcher_connections > 0;
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("connected".to_string(), serde_json::json!(connected));
                        obj.insert(
                            "interactive_connections".to_string(),
                            serde_json::json!(interactive_connections),
                        );
                        obj.insert(
                            "watcher_connections".to_string(),
                            serde_json::json!(watcher_connections),
                        );
                    }
                    ids.push(id.clone());
                    states.push(v);
                }
            }
            for (id, slot) in acp_sessions.snapshot() {
                let mut v = serde_json::to_value(slot.value.state.lock().unwrap().clone())
                    .unwrap_or(serde_json::Value::Null);
                let connected = {
                    let out = slot.value.out.lock().unwrap();
                    out.client.is_some() || !out.watchers.is_empty()
                };
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("connected".to_string(), serde_json::json!(connected));
                }
                ids.push(id);
                states.push(v);
            }
            let mut c = conn;
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({ "sessions": ids, "states": states })
            );
        }
        Some("kill") => {
            let id = v["id"].as_str().unwrap_or_default();
            let s = sessions.lock().unwrap().remove(id);
            if let Some(s) = s {
                let pid = s.ctl.lock().unwrap().pid;
                // 防御坏 pid：kill(-1) 会发给系统所有进程、kill(0) 发给同组进程，
                // pid<=1 一律不真杀（正常 spawn 的 shell pid 必然 >1，只是兜底）。
                if pid > 1 {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                    // 顺手收尸：shell 已死但 PTY pump 线程未必走到 waitpid（历史上
                    // 攒出过几十个僵尸），GUI 清理游离会话/主动关 pane 都走这里，
                    // 借此把僵尸收掉。阻塞等待是微秒~毫秒级，可接受。
                    let _ = waitpid_retry(pid, 0);
                }
                let mut out = s.out.lock().unwrap();
                for c in out.clients.drain(..) {
                    let _ = c.shutdown(Shutdown::Both);
                }
                for w in out.watchers.drain(..) {
                    let _ = w.shutdown(Shutdown::Both);
                }
            }
            let mut c = conn;
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
        }
        Some("upgrade") => handle_upgrade(conn, &v, &sessions, &acp_sessions, listen_fd),
        Some("version") => {
            // pid/started_at/session_count/exe 是后加的：旧 GUI 只读 version/exe_mtime，
            // 多出来的字段它直接忽略，协议向后兼容。
            // `exe`：GUI 用来判断守护是否仍住在 .app 内（装 DMG 会被 SIGKILL）。
            let session_count = sessions.lock().map(|s| s.len()).unwrap_or(0);
            let exe_path = std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            let mut c = conn;
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "exe_mtime": exe_mtime,
                    "exe": exe_path,
                    "pid": std::process::id(),
                    "started_at": started_at(),
                    "session_count": session_count,
                })
            );
        }
        Some("shutdown") => {
            let mut c = conn;
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
            let _ = c.shutdown(Shutdown::Both);
            // 先收 iroh 隧道与远程网关，再 exit——否则手机侧的连接会继续转发到一个
            // 已死的端口。PTY 随本进程死、shell 收 SIGHUP，这是「重启守护」的代价。
            cleanup_sidecar_services();
            std::process::exit(0);
        }
        Some("remote_start") => {
            let bind = v["bind"].as_str().unwrap_or("127.0.0.1").to_string();
            let port = v["port"].as_u64().unwrap_or(0) as u16;
            let write = v["write"].as_bool().unwrap_or(false);
            let mut c = conn;
            match start_remote_gateway(&remote_state, &bind, port, write) {
                Ok((token, addr, write)) => {
                    let _ = writeln!(
                        c,
                        "{}",
                        serde_json::json!({
                            "ok": true, "token": token, "addr": addr.to_string(), "write": write
                        })
                    );
                }
                Err(e) => {
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": e }));
                }
            }
        }
        Some("remote_stop") => {
            stop_remote_gateway(&remote_state);
            let mut c = conn;
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
        }
        Some("remote_rotate_token") => {
            // Token 是设备凭证；只有这条显式操作会轮换。先停隧道和网关，确保
            // 旧连接立即失效，调用方随后按原配置重新拉起服务。
            stop_iroh(&iroh_state);
            let mut c = conn;
            match rotate_remote_token(&remote_state) {
                Ok(_) => {
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
                }
                Err(error) => {
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": error }));
                }
            }
        }
        Some("remote_status") => {
            let mut c = conn;
            let guard = remote_state.lock().unwrap();
            let body = match guard.gateway.as_ref() {
                Some(g) => serde_json::json!({
                    "running": true, "token": g.token, "addr": g.addr.to_string(), "write": g.write
                }),
                None => serde_json::json!({ "running": false }),
            };
            let _ = writeln!(c, "{}", body);
        }
        Some("iroh_start") => {
            let write = v["write"].as_bool().unwrap_or(false);
            let relay = v["relay"].as_str().unwrap_or_default();
            let mut c = conn;
            match start_iroh(&iroh_state, &remote_state, write, relay, Arc::clone(&iroh_connections)) {
                Ok((endpoint_id, token, addr, write, relay)) => {
                    // token 一并回：配对码 = endpoint_id + token，缺一不可
                    // （隧道只负责把字节送到，鉴权仍归网关）。
                    let _ = writeln!(
                        c,
                        "{}",
                        serde_json::json!({
                            "ok": true, "endpoint_id": endpoint_id, "token": token,
                            "addr": addr.to_string(), "write": write,
                            "relay": relay
                        })
                    );
                }
                Err(e) => {
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": e }));
                }
            }
        }
        Some("iroh_stop") => {
            stop_iroh(&iroh_state);
            let mut c = conn;
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
        }
        Some("iroh_status") => {
            let mut c = conn;
            let body = match iroh_status(&iroh_state) {
                Some((endpoint_id, relay)) => {
                    let (token, write) = remote_state
                        .lock()
                        .unwrap()
                        .gateway
                        .as_ref()
                        .map(|g| (g.token.clone(), g.write))
                        .unwrap_or_default();
                    serde_json::json!({
                        "running": true, "endpoint_id": endpoint_id,
                        "token": token, "write": write,
                        "relay": relay.url.to_string()
                    })
                }
                None => serde_json::json!({ "running": false }),
            };
            let _ = writeln!(c, "{}", body);
        }
        Some("iroh_connections") => {
            let mut c = conn;
            let connections = get_iroh_connections(&iroh_state);
            let _ = writeln!(c, "{}", serde_json::json!({ "connections": connections }));
        }
        Some("agent_event") => {
            let id = v["id"].as_str().unwrap_or_default();
            let event = serde_json::from_value::<AgentEvent>(v["event"].clone()).ok();
            let sess = sessions.lock().unwrap().get(id).cloned();
            let mut accepted = false;
            if let (Some(sess), Some(event)) = (sess, event) {
                let snapshot = {
                    let mut st = sess.state.lock().unwrap();
                    apply_agent_event(&mut st, &event).then(|| st.clone())
                };
                if let Some(snapshot) = snapshot {
                    accepted = true;
                    broadcast_state(&subscribers, &snapshot);
                }
            }
            let mut c = conn;
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": accepted }));
        }
        Some("state") => {
            // hook 直写，协议事实——三个信源里可信度最高，无条件覆盖 StateListener
            // 猜出来的值（见 SessionState 定义处注释）。会话不存在（hook 跑得比
            // spawn 还快，或者会话已经被 kill）就静默丢弃，不算错误。
            let id = v["id"].as_str().unwrap_or_default();
            // 先把会话取出来、当场放掉 sessions 锁，再往下走：下面的 broadcast_state
            // 要拿 subscribers，而 handle_subscribe 是「持 subscribers 求 sessions」——
            // 反向持有就是 ABBA 死锁。sessions 一旦锁死，open/list/kill/version/upgrade
            // 全部卡住，PTY 还活着但守护已废，用户只能 pkill，正在跑的会话全灭。
            //
            // 绝不能写回 `if let Some(sess) = sessions.lock().unwrap()...`：if-let 的
            // scrutinee 临时量（那把 guard）活到整个 body 结束，Rust 2024 的 if-let
            // rescope 只改 else 分支、救不了这里。旁边 action/input/resize 用 let-else
            // 正是为此（guard 在语句末即释放）。
            let sess = sessions.lock().unwrap().get(id).cloned();
            if let Some(sess) = sess {
                if let Ok(phase) = serde_json::from_value::<Phase>(v["phase"].clone()) {
                    let snapshot = {
                        let mut st = sess.state.lock().unwrap();
                        st.phase = phase;
                        st.structured_events = true;
                        st.agent_event_version = None;
                        st.active_blocker = None;
                        st.phase_since = now_unix();
                        st.pending_question = v["question"].as_str().map(String::from);
                        st.updated_at = now_unix();
                        st.clone()
                    };
                    broadcast_state(&subscribers, &snapshot);
                }
            }
            let mut c = conn;
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
        }
        Some("action") => {
            let id = v["id"].as_str().unwrap_or_default();
            let mut c = conn;
            let Some(sess) = sessions.lock().unwrap().get(id).cloned() else {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "会话不存在" })
                );
                return;
            };

            // 门闩：只有 agent 真的在等你的时候才允许写入——不然这几个字节会被当成
            // agent 当前正在做的别的事情的输入，把会话搞乱（见 collaboration.md
            // 「联机 review」一节的坑）。不排队，直接拒绝：Phase 5 的操作台本来就是
            // 状态驱动渲染按钮，正常点击时机不该落到这个分支。
            let phase = sess.state.lock().unwrap().phase;
            if !matches!(phase, Phase::AwaitingApproval | Phase::WaitingForUser) {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "agent 现在不是在等你，稍后再试" })
                );
                return;
            }

            let payload = match action_payload(v["kind"].as_str(), v["text"].as_str()) {
                Ok(p) => p,
                Err(err) => {
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": err }));
                    return;
                }
            };

            let write_result = {
                let ctl = sess.ctl.lock().unwrap();
                (&ctl.master).write_all(&payload)
            };
            match write_result {
                Ok(()) => {
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
                }
                Err(e) => {
                    let _ = writeln!(
                        c,
                        "{}",
                        serde_json::json!({ "ok": false, "err": e.to_string() })
                    );
                }
            }
        }
        Some("input") => {
            // 原始输入：工作延续，无 phase 门闩。权限在网关 write_enabled，这里只做
            // 「会话在不在 + 载荷非空 + 写进 master」。
            let id = v["id"].as_str().unwrap_or_default();
            let mut c = conn;
            let Some(sess) = sessions.lock().unwrap().get(id).cloned() else {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "会话不存在" })
                );
                return;
            };

            let Some(payload) = input_payload(&v) else {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "需要非空 data" })
                );
                return;
            };

            let write_result = {
                let ctl = sess.ctl.lock().unwrap();
                (&ctl.master).write_all(&payload)
            };
            match write_result {
                Ok(()) => {
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
                }
                Err(e) => {
                    let _ = writeln!(
                        c,
                        "{}",
                        serde_json::json!({ "ok": false, "err": e.to_string() })
                    );
                }
            }
        }
        Some("resize") => {
            // 手机端按视口改 PTY 尺寸，让 Claude 等 TUI SIGWINCH 重排，
            // 避免「镜像桌面大窗口 → 底部空一大截」。
            let id = v["id"].as_str().unwrap_or_default();
            let cols = v["cols"].as_u64().unwrap_or(0) as u16;
            let rows = v["rows"].as_u64().unwrap_or(0) as u16;
            let cell_w = v["cell_w"].as_u64().unwrap_or(0) as u16;
            let cell_h = v["cell_h"].as_u64().unwrap_or(0) as u16;
            let mut c = conn;
            if cols == 0 || rows == 0 {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "cols/rows 必须 > 0" })
                );
                return;
            }
            let Some(sess) = sessions.lock().unwrap().get(id).cloned() else {
                let _ = writeln!(
                    c,
                    "{}",
                    serde_json::json!({ "ok": false, "err": "会话不存在" })
                );
                return;
            };
            // jolt：确保即使尺寸碰巧与当前相同也发出 SIGWINCH，逼 TUI 全量重绘
            sess.ctl.lock().unwrap().jolt = true;
            resize_session(&sess, cols, rows, cell_w, cell_h);
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({ "ok": true, "cols": cols, "rows": rows })
            );
        }
        _ => {}
    }
}

fn terminal_error_reply(reason: &str, rows: u16, cols: u16) -> Vec<u8> {
    let reason: String = reason
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let header = serde_json::json!({
        "ok": false,
        "err": reason,
        "rows": rows,
        "cols": cols,
        "replay_len": 0,
    });
    format!("{header}\n\r\n\x1b[31m{reason}\x1b[0m\r\n").into_bytes()
}

fn write_terminal_error(mut conn: &UnixStream, reason: &str, rows: u16, cols: u16) {
    let _ = conn.write_all(&terminal_error_reply(reason, rows, cols));
}

fn handle_open(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    sessions: Sessions,
    subscribers: Subscribers,
) {
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return;
    }
    let cols = v["cols"].as_u64().unwrap_or(80) as u16;
    let rows = v["rows"].as_u64().unwrap_or(24) as u16;
    let cwd = v["cwd"].as_str().map(String::from);
    let launch = v["initial_launch"].as_str().map(String::from);
    let create_if_missing = v["create_if_missing"].as_bool().unwrap_or(true);

    // 取既有会话（reattach）或新建。
    let existing = sessions.lock().unwrap().get(&id).cloned();
    let reattach = existing.is_some();
    let sess = match existing {
        Some(s) => {
            // reattach：等客户端首帧 resize（含真实 cell 像素）再 jolt，避免在错误
            // 尺寸下 SIGWINCH → Claude「显示不全」。见下方 delayed jolt 注释。
            s.ctl.lock().unwrap().jolt = true;
            s
        }
        None if !create_if_missing => {
            write_terminal_error(&conn, "终端会话不存在", rows, cols);
            return;
        }
        None => {
            let result = spawn_session(
                &id,
                rows,
                cols,
                cwd.as_deref(),
                launch.as_deref(),
                &subscribers,
            );
            let (sess, pty_reader) = match result {
                Ok(result) => result,
                Err(error) => {
                    smelt_core::app_log::error(
                        "session",
                        &format!("会话 {id} 启动失败：{error:#}"),
                    );
                    write_terminal_error(&conn, &format!("终端启动失败：{error:#}"), rows, cols);
                    return;
                }
            };
            smelt_core::app_log::info("session", &format!("会话 {id} 已创建"));
            let sess = Arc::new(sess);
            sessions
                .lock()
                .unwrap()
                .insert(id.clone(), Arc::clone(&sess));
            let opened_state = sess.state.lock().unwrap().clone();
            broadcast_state(&subscribers, &opened_state);
            start_pty_pump(
                Arc::clone(&sess),
                pty_reader,
                id.clone(),
                Arc::clone(&sessions),
                Arc::clone(&subscribers),
            );
            sess
        }
    };

    // attach：回报 PTY 当前尺寸 → 网格 ANSI 快照 → 接管转发。
    //
    // 锁序与 resize 一致（ctl → term → out），且 snapshot 与装上 client 之间不能放掉 out：
    // 若先 snapshot 再另抢 out，间隙里泵可能 advance(D) 后发现还没 client 而丢弃 D，
    // 新客户端拿到的网格就永久缺字节（正是「吐快照」要避免的 reattach 错位）。
    // 正确做法：持 term 时抢到 out → 再出快照 → 放 term → 写 socket 期间只持 out
    // （泵 advance 后堵在 out，client 装上后再把缺口字节转发给新客户端）。
    let launch_for_snap = sess.state.lock().unwrap().launch.clone();
    let attached_fd = {
        let Ok(mut c) = conn.try_clone() else { return };
        let fd = c.as_raw_fd();
        // 写超时：客户端冻结时不能无限期占着 out 锁（见 CLIENT_WRITE_TIMEOUT）。
        let _ = c.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));

        let (cur_cols, cur_rows, snapshot, mut out) = {
            let ctl = sess.ctl.lock().unwrap();
            let term = sess.term.lock().unwrap();
            let out = sess.out.lock().unwrap();
            let mut snapshot = terminal_geometry_osc(
                &sess.geometry_token,
                TerminalGeometryOsc {
                    cols: ctl.cols,
                    rows: ctl.rows,
                    cell_width: ctl.cell_w,
                    cell_height: ctl.cell_h,
                    remote_controlled: ctl.remote_viewports > 0,
                },
            );
            // launch 参与判定：Grok 等未必进 1049 备用屏，但仍是 TUI，灌网格会顶行乱码。
            snapshot.extend(snapshot_ansi(&term, launch_for_snap.as_deref()));
            drop(term);
            let geometry = (ctl.cols, ctl.rows);
            drop(ctl);
            (geometry.0, geometry.1, snapshot, out)
        };

        // replay_len = 快照字节数：客户端仍用它划「历史/实时」边界，跳过快照里的
        // 历史 OSC 9（网格快照本身不含旧通知序列，但边界语义保留兼容）。
        let replay_len = snapshot.len();
        if writeln!(
            c,
            "{}",
            serde_json::json!({
                "cols": cur_cols,
                "rows": cur_rows,
                "replay_len": replay_len,
                "geometry_token": sess.geometry_token.as_str(),
            })
        )
        .is_err()
        {
            return;
        }
        if replay_len > 0 && c.write_all(&snapshot).is_err() {
            return;
        }
        out.clients.push(c);
        fd
    };

    // reattach jolt 策略（修 Claude 显示不全 / Grok 半残）：
    // **不要**在 attach 当下立刻 SIGWINCH——此时 GUI 往往还是守护旧 cols/rows、cell=0，
    // TUI 按错误尺寸整屏重画 → 显示不全。正确顺序：等客户端 force_resize（真 cell
    // 像素）走 type-1 帧，resize_session 里 jolt 才触发。
    // 兜底：350ms 内若 jolt 仍 true（客户端没发 resize），再强制抖一次。
    if reattach {
        sess.ctl.lock().unwrap().jolt = true;
        let sess2 = Arc::clone(&sess);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(350));
            let (c, r, still) = {
                let Ok(ctl) = sess2.ctl.lock() else { return };
                (ctl.cols, ctl.rows, ctl.jolt)
            };
            if still {
                if let Ok(mut ctl) = sess2.ctl.lock() {
                    ctl.jolt = true;
                }
                resize_session(&sess2, c, r, 0, 0);
            }
            // 再补一枪：部分 TUI（Claude）第一次 SIGWINCH 只重排半屏
            thread::sleep(Duration::from_millis(200));
            if let Ok(mut ctl) = sess2.ctl.lock() {
                ctl.jolt = true;
                let c = ctl.cols;
                let r = ctl.rows;
                drop(ctl);
                resize_session(&sess2, c, r, 0, 0);
            }
        });
    }

    // 帧循环：输入 / resize，直到客户端断开。
    loop {
        let mut hdr = [0u8; 5];
        if reader.read_exact(&mut hdr).is_err() {
            break;
        }
        let len = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        if len > (1 << 20) {
            break; // 异常长度，掐断
        }
        let mut payload = vec![0u8; len];
        if reader.read_exact(&mut payload).is_err() {
            break;
        }
        match hdr[0] {
            0 => {
                let ctl = sess.ctl.lock().unwrap();
                let _ = (&ctl.master).write_all(&payload);
            }
            1 if len == 8 || len == 16 => {
                let cols = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as u16;
                let rows = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as u16;
                // 可选：单元格像素（新客户端 16 字节帧）；整窗像素 = 行列 × 格像素。
                let (cell_w, cell_h) = if len == 16 {
                    let cw = u32::from_be_bytes(payload[8..12].try_into().unwrap()) as u16;
                    let ch = u32::from_be_bytes(payload[12..16].try_into().unwrap()) as u16;
                    (cw, ch)
                } else {
                    (0, 0)
                };
                resize_session(&sess, cols, rows, cell_w, cell_h);
            }
            _ => break,
        }
    }

    // 断开：只摘掉本 attachment，不影响同一 PTY 的其它渲染层。
    let mut out = sess.out.lock().unwrap();
    out.clients.retain(|c| c.as_raw_fd() != attached_fd);
}

struct RemoteViewportLease {
    session: Arc<Session>,
}

impl Drop for RemoteViewportLease {
    fn drop(&mut self) {
        end_remote_viewport(&self.session);
    }
}

/// 旁观/远程渲染连接。普通 watch 仍严格只读；声明
/// `controls_geometry` 的移动端连接在自己的生命周期内持有 PTY 尺寸租约，并可在
/// 同一连接上发送 type-1 resize 帧。跟 `handle_open` 的核心区别——
/// 1. 不兜底 spawn：会话必须已存在，旁观一个不存在的会话没有意义；
/// 2. 不影响 `out.clients`，也不顶替其它 watcher——`push` 进去，多个旁观者可并存；
/// 3. 移动端只取得尺寸所有权，不替换桌面 attachment；断开时自动归还。
fn handle_watch(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    sessions: Sessions,
) {
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return;
    }
    let Some(sess) = sessions.lock().unwrap().get(&id).cloned() else {
        return;
    };

    let controls_geometry = v["controls_geometry"].as_bool().unwrap_or(false);
    let remote_geometry = controls_geometry.then(|| {
        (
            v["cols"].as_u64().unwrap_or(0).min(1000) as u16,
            v["rows"].as_u64().unwrap_or(0).min(1000) as u16,
            v["cell_w"].as_u64().unwrap_or(0).min(256) as u16,
            v["cell_h"].as_u64().unwrap_or(0).min(256) as u16,
        )
    });
    if remote_geometry.is_some_and(|(cols, rows, _, _)| cols == 0 || rows == 0) {
        return;
    }
    let _remote_viewport = remote_geometry.map(|(cols, rows, cell_w, cell_h)| {
        begin_remote_viewport(&sess, cols, rows, cell_w, cell_h);
        RemoteViewportLease {
            session: Arc::clone(&sess),
        }
    });

    let (cur_cols, cur_rows) = {
        let ctl = sess.ctl.lock().unwrap();
        (ctl.cols, ctl.rows)
    };

    // 锁序、snapshot-与-挂载之间不放锁的道理跟 handle_open 完全一致（见其注释）：
    // 用 out 锁本身当「挂载点」，snapshot 拼好、watcher push 进 Vec 一步做完，
    // 中间不放 out 锁，泵线程就不会在这个间隙 advance 出一段没人接住的字节。
    let attached_fd = {
        let Ok(mut c) = conn.try_clone() else { return };
        let fd = c.as_raw_fd();
        let _ = c.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));

        let term = sess.term.lock().unwrap();
        let mut out = sess.out.lock().unwrap();
        let launch = sess.state.lock().unwrap().launch.clone();
        let snapshot = snapshot_ansi_for_watch(&term, launch.as_deref());
        drop(term);

        let replay_len = snapshot.len();
        if writeln!(
            c,
            "{}",
            serde_json::json!({ "cols": cur_cols, "rows": cur_rows, "replay_len": replay_len })
        )
        .is_err()
        {
            return;
        }
        if replay_len > 0 && c.write_all(&snapshot).is_err() {
            return;
        }
        out.watchers.push(c);
        fd
    };

    // 与桌面 reattach 同款补抖（见 handle_open 的 "reattach jolt 策略"）。
    // begin_remote_viewport 的那次 SIGWINCH 发生在 watcher 挂载**之前**，且快照是
    // 紧接着抓的——TUI 的重绘此刻还没吐出来，移动端第一次进入只能看到旧尺寸内容被
    // reflow 后的残帧（且部分 TUI 第一次 SIGWINCH 只重排半屏）。watcher 挂上之后再
    // 抖两次，重绘字节就能直接流给移动端，首次进入不再显示旧内容。
    if controls_geometry {
        let sess2 = Arc::clone(&sess);
        thread::spawn(move || {
            for delay in [Duration::from_millis(300), Duration::from_millis(400)] {
                thread::sleep(delay);
                let (cols, rows) = {
                    let Ok(mut ctl) = sess2.ctl.lock() else { return };
                    if ctl.remote_viewports == 0 {
                        return;
                    }
                    ctl.jolt = true;
                    (ctl.cols, ctl.rows)
                };
                resize_session_remote(&sess2, cols, rows, 0, 0);
            }
        });
    }

    if controls_geometry {
        loop {
            let mut header = [0u8; 5];
            if reader.read_exact(&mut header).is_err() {
                break;
            }
            let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
            if header[0] != 1 || (len != 8 && len != 16) {
                break;
            }
            let mut payload = vec![0u8; len];
            if reader.read_exact(&mut payload).is_err() {
                break;
            }
            let cols = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as u16;
            let rows = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as u16;
            let (cell_w, cell_h) = if len == 16 {
                (
                    u32::from_be_bytes(payload[8..12].try_into().unwrap()) as u16,
                    u32::from_be_bytes(payload[12..16].try_into().unwrap()) as u16,
                )
            } else {
                (0, 0)
            };
            if cols == 0 || rows == 0 {
                break;
            }
            resize_session_remote(&sess, cols, rows, cell_w, cell_h);
        }
    } else {
        // Legacy watch remains read-only. Any byte (or EOF) ends the watch.
        let mut scratch = [0u8; 64];
        let _ = reader.read(&mut scratch);
    }

    let mut out = sess.out.lock().unwrap();
    out.watchers.retain(|w| w.as_raw_fd() != attached_fd);
}

/// 状态订阅：跟 `watch` 是同一种只读连接模式，但订阅面是**全部会话**，不是单个
/// session（见 Subscribers 类型定义处注释）。首帧全量快照，之后每次任何会话的
/// state 变化都会推一行——广播逻辑在 broadcast_state / StateListener::send_event /
/// `state` op 里，这里只管连接的注册与清理。快照汇总终端 + ACP 两张表——四色
/// 状态两边共用同一个 `SessionState`/`Phase`（见下方「ACP 会话托管」一节），GUI
/// 那条既有的 subscribe 监听代码完全不用为 ACP 改一行。
fn handle_subscribe(
    conn: UnixStream,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    subscribers: &Subscribers,
) {
    let Ok(mut c) = conn.try_clone() else { return };
    let fd = c.as_raw_fd();
    // 与 open/watch 一致：冻结订阅者不能无限期占着 broadcast_state 的 subscribers 锁。
    let _ = c.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));

    // snapshot 写出去、push 进订阅列表之间不能放 subscribers 锁：否则中间这个空隙里
    // 一次 broadcast_state 可能两头都漏掉——快照里没有它（早于快照），也没收到广播
    // （晚于注册），这次状态变化对这个订阅者来说凭空消失。跟 handle_watch 的
    // snapshot-与-挂载不放锁是同一个道理。
    let mut subs = subscribers.lock().unwrap();
    let mut snapshot: Vec<SessionState> = {
        let sessions = sessions.lock().unwrap();
        sessions
            .values()
            .map(|s| s.state.lock().unwrap().clone())
            .collect()
    };
    snapshot.extend(
        acp_sessions
            .snapshot()
            .into_iter()
            .map(|(_, slot)| slot.value.state.lock().unwrap().clone()),
    );
    if writeln!(c, "{}", serde_json::json!({ "sessions": snapshot })).is_err() {
        return;
    }
    subs.push(c);
    drop(subs);

    // 只读：不认帧协议，读到任何东西（含 EOF/出错）都收尾，跟 handle_watch 一致。
    let mut reader = BufReader::new(conn);
    let mut scratch = [0u8; 64];
    let _ = reader.read(&mut scratch);

    subscribers.lock().unwrap().retain(|s| s.as_raw_fd() != fd);
}

// ===================== ACP 会话托管 =====================
//
// 跟终端会话是两条平行的托管逻辑：没有 PTY/网格，「画面」就是
// `smelt_core::acp_session::AcpSnapshot`（entries + phase + 待办卡片），由
// `smelt_core::acp_session::apply_event` 把子进程 agent 发来的协议事件归约
// 出来——归约逻辑本身跟 GPUI 无关，谁接手连接谁跑，见该模块文件头注释（核心
// 原因：`AcpEvent::Permission`/`Elicitation` 带的 responder 绑在连接线程上，
// 没法跨进程传，只能是 smeltd 亲自跑完整个事件循环）。
//
// 四色状态复用终端会话已有的 `SessionState`/`Phase`/`broadcast_state`/
// `subscribe` 机制：`AcpSession.state` 就是一份跟终端会话同类型的
// `Arc<Mutex<SessionState>>`，list/subscribe 汇总时两边的 Vec 拼在一起即可。
// ACP 会话 id 沿用 GUI 现有的 `acp-` 前缀约定，GUI 靠这个前缀判断该走
// open/watch 还是 acp_open/acp_watch。
//
// 协议：
//   {"op":"acp_open","id":"acp-..","cwd":"..",
//    "launch":{"command":"..","env":{"KEY":"value"}},"agent":"claude",
//    "resume_id":".."}
//     → 新协议；旧客户端发 `"cmd":".."` 也兼容，守护侧会窄范围兜底转成
//       `AcpLaunchSpec::from_command(cmd)`。
//     → 已存在且还活着（有 handle）就直接接上；已存在但 Ended（没有 handle）
//       就用请求带的 launch + 已知的旧 session id（没有才退回请求带的 resume_id）
//       重新 spawn（「重新开始」）；都不存在就全新建。丢失 daemon slot 时会先起
//       一个 replacement 进程，再尝试按 resume_id 恢复协议状态；只有 typed
//       unsupported / not-found 这两类恢复结果才会继续创建新的 conversation，
//       transient failure 只返回可重试错误，不会偷偷新建。回一份
//       `{"snapshot": AcpSnapshot}`，之后每次归约有实质变化再推一份同形状的
//       行。同 id 只允许一个控制连接，第二次 open 顶掉前一个。
//   {"op":"acp_watch","id":".."} → 只读镜像，会话必须已存在，可多个并存。
//   {"op":"acp_kill","id":".."} → 回 {"ok":true}，杀子进程、从表里摘掉、
//     踢掉所有 client/watcher。
//
// acp_open 连接内不是终端那套二进制帧，是纯 JSON 行、双向：
//   客户端 → 守护：一行 `AcpUserAction` 的 JSON
//   守护 → 客户端：一行 `{"snapshot": AcpSnapshot}`
// 断开 acp_open 连接（切标签/关标签/App 退出）只摘连接，不杀会话——这正是
// 这层要解决的问题（GUI 退出不该带走 ACP 对话）。真要杀走 acp_kill。
//
// 「无缝升级」只交接处于协议静默边界的 ACP 会话：agent 子进程的 stdin/stdout
// fd 跟 PTY master fd 同样裸传过 exec()，快照数据随交接文件走。JSON-RPC 的
// outstanding request callback / responder 是 SDK 进程内状态，不能靠 fd 恢复；
// 只要任一会话仍有活跃回合、审批或未完成 RPC，upgrade 就返回 busy，等待调用方
// 在回合结束后重试。旧交接格式的 raw request 回放只保留作向后兼容，不再作为
// 活跃回合跨 exec 的正确性机制。

struct AcpOut {
    client: Option<UnixStream>,
    watchers: Vec<UnixStream>,
}

struct AcpSession {
    reduced: Mutex<smelt_core::acp_session::AcpSessionState>,
    snapshot_revision: AtomicU64,
    handle: Mutex<Option<smelt_core::acp_conn::AcpHandle>>,
    cwd: Option<String>,
    /// 旧 handoff 格式兼容字段；当前恢复路径不再读取 agent 私有 transcript。
    agent_needs_transcript_check: bool,
    /// 四色状态，跟终端会话共用同一个类型/同一套广播机制。
    state: Arc<Mutex<SessionState>>,
    out: Mutex<AcpOut>,
    /// 最近一次真正 open/relaunch 用过的完整启动规格（含 env），供 `acp_restart`
    /// 重启时复用——`state.launch` 只存了命令字符串给状态展示用，重启需要
    /// 完整结构化的 `AcpLaunchSpec`（env 可能带空格，不能靠字符串拼回去）。
    launch_spec: Mutex<Option<smelt_core::agent_kind::AcpLaunchSpec>>,
}

type AcpSessions = Arc<AcpRegistry<AcpSession>>;

fn new_acp_sessions() -> AcpSessions {
    Arc::new(AcpRegistry::new(Arc::clone(&SPAWN_GATE)))
}

#[derive(Clone)]
struct AcpOpenRequest {
    id: String,
    cwd: Option<String>,
    launch: smelt_core::agent_kind::AcpLaunchSpec,
    agent_needs_transcript_check: bool,
    resume_id: Option<String>,
    tail_limit: Option<usize>,
}

fn parse_acp_open_request(v: &serde_json::Value) -> Option<AcpOpenRequest> {
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return None;
    }
    let launch = v
        .get("launch")
        .cloned()
        .and_then(|value| {
            serde_json::from_value::<smelt_core::agent_kind::AcpLaunchSpec>(value).ok()
        })
        .unwrap_or_else(|| {
            smelt_core::agent_kind::AcpLaunchSpec::from_command(
                v["cmd"].as_str().unwrap_or_default(),
            )
        });
    Some(AcpOpenRequest {
        id,
        cwd: v["cwd"].as_str().map(String::from),
        launch,
        agent_needs_transcript_check: v["agent"].as_str().unwrap_or("claude") == "claude",
        resume_id: v["resume_id"].as_str().map(String::from),
        tail_limit: v["tail_limit"]
            .as_u64()
            .map(|value| (value as usize).clamp(1, 500)),
    })
}

fn select_resume_id(requested: Option<String>, known_history: Option<String>) -> Option<String> {
    // relaunch 时 GUI 持久化的 canonical id 必须优先；daemon 里的 runtime id
    // 可能来自一次空恢复，不能反过来覆盖真正的 transcript id。
    requested.or(known_history)
}

fn known_acp_resume_id(reduced: &smelt_core::acp_session::AcpSessionState) -> Option<String> {
    reduced.history_session_id.clone().or_else(|| {
        (!reduced.entries.is_empty())
            .then(|| reduced.acp_session_id.clone())
            .flatten()
    })
}

fn acp_open_needs_relaunch(created: bool, alive: bool, has_launch_command: bool) -> bool {
    created || (!alive && has_launch_command)
}

/// ACP 相位 → 四色 Phase。`Running` 还要看 entries 里有没有进行中的工具调用，
/// 细分成「执行工具」/「思考中」——跟旧版 GUI `sync_daemon_state` 的判断一致。
fn compute_acp_daemon_phase(reduced: &smelt_core::acp_session::AcpSessionState) -> Phase {
    use smelt_core::acp_chat::{AcpEntry, ToolCallStatus};
    use smelt_core::acp_session::AcpPhase;
    match &reduced.phase {
        AcpPhase::Starting | AcpPhase::Idle => Phase::Idle,
        AcpPhase::Running => {
            let executing = reduced.entries.iter().any(|e| {
                matches!(
                    e,
                    AcpEntry::ToolCall {
                        status: ToolCallStatus::InProgress | ToolCallStatus::Pending,
                        ..
                    }
                )
            });
            if executing {
                Phase::ExecutingTool
            } else {
                Phase::Thinking
            }
        }
        AcpPhase::AwaitingApproval => Phase::AwaitingApproval,
        AcpPhase::AwaitingChoice => Phase::WaitingForUser,
        AcpPhase::Ended(_) => Phase::Dead,
    }
}

fn acp_pending_question(reduced: &smelt_core::acp_session::AcpSessionState) -> Option<String> {
    reduced
        .permissions
        .first()
        .map(|p| p.question.clone())
        .or_else(|| reduced.elicitation.as_ref().map(|e| e.message.clone()))
}

/// 把归约状态里的相位/待办问句同步进四色 `SessionState` 并广播。跟旧版 GUI
/// `AcpView::sync_daemon_state` 是同一件事，只是现在算在 smeltd 侧。
fn update_acp_daemon_state(sess: &AcpSession, subscribers: &Subscribers) {
    let (phase, pending_question, title) = {
        let reduced = sess.reduced.lock().unwrap();
        (
            compute_acp_daemon_phase(&reduced),
            acp_pending_question(&reduced),
            smelt_core::acp_chat::auto_title(&reduced.entries),
        )
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let snapshot = {
        let mut st = sess.state.lock().unwrap();
        if st.phase != phase {
            st.phase_since = now;
        }
        st.phase = phase;
        st.pending_question = pending_question;
        st.title = title;
        st.updated_at = now;
        st.clone()
    };
    broadcast_state(subscribers, &snapshot);
}

/// 推一份最新快照给控制连接 + 全部旁观者，写失败的直接摘掉（对齐终端
/// `out.watchers.retain_mut`/PTY 泵的写失败清理策略）。`should_persist` 是
/// "这次变化是怎么发生的"这个上下文，调用方按场景传：事件驱动的走
/// `ApplyOutcome::should_persist`；用户动作（发 prompt/选权限）驱动的固定
/// false——跟旧版行为一致，用户主动发起的变化不单独触发落盘，等下一次
/// 协议事件（通常是 TurnEnded）时一并存。
fn push_acp_snapshot_since(sess: &AcpSession, should_persist: bool, entries_offset: Option<usize>) {
    let snap = {
        let reduced = sess.reduced.lock().unwrap();
        let offset = entries_offset.unwrap_or(reduced.entries.len());
        let mut snapshot = reduced.to_snapshot_since(should_persist, offset);
        snapshot.snapshot_revision = sess.snapshot_revision.fetch_add(1, Ordering::SeqCst) + 1;
        snapshot
    };
    let payload = serde_json::json!({ "snapshot": snap }).to_string();
    let mut out = sess.out.lock().unwrap();
    if let Some(c) = &mut out.client {
        if writeln!(c, "{payload}").is_err() {
            out.client = None;
        }
    }
    out.watchers
        .retain_mut(|w| writeln!(w, "{payload}").is_ok());
}

fn push_acp_snapshot(sess: &AcpSession, should_persist: bool) {
    let offset = sess.reduced.lock().unwrap().entries.len().saturating_sub(1);
    push_acp_snapshot_since(sess, should_persist, Some(offset));
}

/// 事件 drain：整个会话生命周期只有这一条线程在改 `reduced`（`apply_acp_user_action`
/// 里权限/选择题相关的写也在这条线程外发生，但两边改的是不相交的字段/走
/// 互斥锁，不会踩踏）。通道关闭（连接线程收尾）就退出；如果退出时相位还不是
/// `Ended`（没收到 `Fatal` 就断，比如连接线程 panic），兜底补一个 Ended，不让
/// GUI 永远卡在「运行中」。
fn start_acp_event_drain(
    slot: Arc<AcpSlot<AcpSession>>,
    event_rx: smol::channel::Receiver<smelt_core::acp_conn::AcpEvent>,
    subscribers: Subscribers,
) {
    thread::spawn(move || {
        let sess = &slot.value;
        smol::block_on(async {
            while let Ok(ev) = event_rx.recv().await {
                if matches!(&ev, smelt_core::acp_conn::AcpEvent::TurnEnded(_))
                    && let Some(handle) = sess.handle.lock().unwrap().as_ref()
                {
                    // 兼容从旧版 active-fd handoff 恢复的会话：孤儿 response 由
                    // 原始行解析器直接补 TurnEnded，不经过 drive_session 的计数归零。
                    handle.in_flight_rpc.store(0, Ordering::SeqCst);
                }
                let outcome = {
                    let mut st = sess.reduced.lock().unwrap();
                    smelt_core::acp_session::apply_event(&mut st, ev)
                };
                push_acp_snapshot_since(&sess, outcome.should_persist, outcome.entries_offset);
                update_acp_daemon_state(&sess, &subscribers);
            }
        });
        sess.handle.lock().unwrap().take(); // drop：ChildGuard 收尸子进程组
        let already_ended = matches!(
            sess.reduced.lock().unwrap().phase,
            smelt_core::acp_session::AcpPhase::Ended(_)
        );
        if !already_ended {
            sess.reduced.lock().unwrap().phase =
                smelt_core::acp_session::AcpPhase::Ended("连接意外中断".to_string());
            push_acp_snapshot_since(&sess, true, None);
        }
        update_acp_daemon_state(&sess, &subscribers);
    });
}

/// spawn 一次连接（首次建会话 / 「重新开始」共用）：先按旧版 GUI `restart()`
/// 的规则重置回合态字段，再起连接线程、挂事件 drain。
fn acp_relaunch(
    slot: &Arc<AcpSlot<AcpSession>>,
    id: &str,
    mut launch: smelt_core::agent_kind::AcpLaunchSpec,
    resume_id: Option<String>,
    spawn_gate: Arc<RwLock<()>>,
    subscribers: &Subscribers,
) {
    let sess = &slot.value;
    {
        let mut reduced = sess.reduced.lock().unwrap();
        smelt_core::acp_session::reset_for_restart(&mut reduced);
        reduced.history_session_id = resume_id.clone();
    }
    // 旧存档可能还留着退役的 Codex 原生 app-server driver 命令；自动改回
    // 现在的 ACP 默认值，不然这些会话重开会直接连不上。
    if launch.command.trim() == "codex app-server" {
        launch.command = smelt_core::agent_kind::default_acp_codex_cmd();
    }
    let needs_check = sess.agent_needs_transcript_check;
    // 记住这次真正用过的完整启动规格，`acp_restart` 卡死重启时不必依赖 GUI
    // 重新把 launch 传一遍（GUI 那条 acp_open 连接可能压根没断，不会重新握手）。
    *sess.launch_spec.lock().unwrap() = Some(launch.clone());
    let app_launch = smelt_core::acp_conn::AcpLaunch {
        launch: launch.clone(),
        cwd: sess.cwd.clone(),
        sid: id.to_string(),
        resume_session_id: resume_id.map(agent_client_protocol::schema::v1::SessionId::new),
        resume_needs_transcript_check: needs_check,
    };
    let handle = smelt_core::acp_conn::spawn_acp(app_launch, Some(spawn_gate));
    let event_rx = handle.event_rx.clone();
    *sess.handle.lock().unwrap() = Some(handle);
    sess.state.lock().unwrap().launch = Some(launch.command.clone());
    push_acp_snapshot(sess, false); // 刚 spawn，还没有新内容，不用触发落盘
    update_acp_daemon_state(sess, subscribers);
    start_acp_event_drain(Arc::clone(slot), event_rx, subscribers.clone());
}

fn make_acp_session(
    id: &str,
    cwd: Option<String>,
    agent_needs_transcript_check: bool,
) -> AcpSession {
    AcpSession {
        reduced: Mutex::new(smelt_core::acp_session::AcpSessionState::default()),
        snapshot_revision: AtomicU64::new(0),
        handle: Mutex::new(None),
        cwd: cwd.clone(),
        agent_needs_transcript_check,
        state: Arc::new(Mutex::new(SessionState {
            id: id.to_string(),
            cwd,
            launch: None,
            title: None,
            phase: Phase::Idle,
            phase_since: 0,
            pending_question: None,
            tokens_used: None,
            branch: None,
            dirty_files: Vec::new(),
            updated_at: 0,
            structured_events: true,
            agent_event_version: None,
            active_blocker: None,
        })),
        out: Mutex::new(AcpOut {
            client: None,
            watchers: Vec::new(),
        }),
        launch_spec: Mutex::new(None),
    }
}

fn apply_acp_user_action(
    sess: &AcpSession,
    action: smelt_core::acp_session::AcpUserAction,
    subscribers: &Subscribers,
) -> Result<(), &'static str> {
    use smelt_core::acp_conn::AcpCommand;
    use smelt_core::acp_session::AcpUserAction;
    match action {
        AcpUserAction::Prompt { text, images } => {
            let handle = sess
                .handle
                .lock()
                .unwrap()
                .as_ref()
                .map(|h| (h.cmd_tx.clone(), Arc::clone(&h.in_flight_rpc)));
            let Some((cmd_tx, in_flight_rpc)) = handle else {
                return Err("ACP session is not running");
            };
            let shown_images = images.clone();
            let shown_text = text.clone();
            in_flight_rpc.fetch_add(1, Ordering::SeqCst);
            if cmd_tx.try_send(AcpCommand::Prompt { text, images }).is_ok() {
                smelt_core::acp_session::note_prompt_sent(
                    &mut sess.reduced.lock().unwrap(),
                    shown_text,
                    shown_images,
                );
                push_acp_snapshot(sess, false);
                update_acp_daemon_state(sess, subscribers);
                Ok(())
            } else {
                in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
                Err("ACP command channel is busy or closed")
            }
        }
        AcpUserAction::Cancel => {
            let handle = sess.handle.lock().unwrap();
            let Some(h) = handle.as_ref() else {
                return Err("ACP session is not running");
            };
            h.cmd_tx
                .try_send(AcpCommand::Cancel)
                .map_err(|_| "ACP command channel is busy or closed")
        }
        AcpUserAction::SetConfigOption {
            config_id,
            value_id,
        } => {
            let handle = sess.handle.lock().unwrap();
            let Some(h) = handle.as_ref() else {
                return Err("ACP session is not running");
            };
            h.in_flight_rpc.fetch_add(1, Ordering::SeqCst);
            let result = h
                .cmd_tx
                .try_send(AcpCommand::SetConfigOption {
                    config_id,
                    value_id,
                })
                .map_err(|_| "ACP command channel is busy or closed");
            if result.is_err() {
                h.in_flight_rpc.fetch_sub(1, Ordering::SeqCst);
            }
            result
        }
        AcpUserAction::PermissionSelect {
            tool_call_id,
            option_id,
        } => {
            let mut reduced = sess.reduced.lock().unwrap();
            let exists = reduced.permissions.iter().any(|card| {
                card.tool_call_id == tool_call_id
                    && card
                        .options
                        .iter()
                        .any(|option| option.option_id == option_id)
            });
            if !exists {
                return Err("permission request or option not found");
            }
            smelt_core::acp_session::select_permission(&mut reduced, &tool_call_id, &option_id);
            drop(reduced);
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
        AcpUserAction::ElicitationChoose { field_ix, opt_ix } => {
            let auto_submit = {
                let mut reduced = sess.reduced.lock().unwrap();
                smelt_core::acp_session::choose_elicitation(&mut reduced, field_ix, opt_ix)
            };
            if auto_submit {
                return submit_acp_elicitation(sess, subscribers);
            }
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
        AcpUserAction::ElicitationText { field_ix, value } => {
            smelt_core::acp_session::set_elicitation_text(
                &mut sess.reduced.lock().unwrap(),
                field_ix,
                value,
            );
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
        AcpUserAction::ElicitationSubmit => submit_acp_elicitation(sess, subscribers),
        AcpUserAction::ElicitationDismiss => {
            smelt_core::acp_session::dismiss_elicitation(&mut sess.reduced.lock().unwrap());
            push_acp_snapshot(sess, false);
            update_acp_daemon_state(sess, subscribers);
            Ok(())
        }
    }
}

fn submit_acp_elicitation(
    sess: &AcpSession,
    subscribers: &Subscribers,
) -> Result<(), &'static str> {
    let recovered_answer = {
        let reduced = sess.reduced.lock().unwrap();
        smelt_core::acp_session::recovered_elicitation_answer(&reduced)
    };
    if let Some(text) = recovered_answer {
        smelt_core::acp_session::dismiss_elicitation(&mut sess.reduced.lock().unwrap());
        return apply_acp_user_action(
            sess,
            smelt_core::acp_session::AcpUserAction::Prompt {
                text,
                images: Vec::new(),
            },
            subscribers,
        );
    }

    smelt_core::acp_session::submit_elicitation(&mut sess.reduced.lock().unwrap());
    push_acp_snapshot(sess, false);
    update_acp_daemon_state(sess, subscribers);
    Ok(())
}

/// Apply one action without attaching a control client. Remote/mobile callers must not use
/// `acp_open` for one-shot writes because opening the same id intentionally replaces the GUI.
fn handle_acp_action(
    mut conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &Subscribers,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let result = v
        .get("action")
        .cloned()
        .ok_or("missing ACP action")
        .and_then(|action| serde_json::from_value(action).map_err(|_| "invalid ACP action"))
        .and_then(|action| {
            let slot = acp_sessions.get(id).ok_or("ACP session not found")?;
            apply_acp_user_action(&slot.value, action, subscribers)
        });

    let response = match result {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    };
    let _ = writeln!(conn, "{response}");
}

fn handle_acp_open(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    acp_sessions: AcpSessions,
    subscribers: Subscribers,
) {
    let Some(req) = parse_acp_open_request(v) else {
        return;
    };
    let id = req.id.clone();
    let Some(mut slot) = ensure_acp_session(&req, &acp_sessions, &subscribers) else {
        return;
    };
    let attached_fd = loop {
        let lifecycle = slot.lifecycle.lock().unwrap();
        let still_current = acp_sessions
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(&current, &slot));
        if !still_current {
            drop(lifecycle);
            let Some(current) = ensure_acp_session(&req, &acp_sessions, &subscribers) else {
                return;
            };
            slot = current;
            continue;
        }

        let sess = &slot.value;
        let attached_fd = {
            let Ok(mut c) = conn.try_clone() else { return };
            let fd = c.as_raw_fd();
            let _ = c.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
            let reduced = sess.reduced.lock().unwrap();
            let offset = req
                .tail_limit
                .map(|limit| reduced.entries.len().saturating_sub(limit))
                .unwrap_or(0);
            let snapshot = reduced.to_snapshot_since(false, offset);
            drop(reduced);
            if writeln!(c, "{}", serde_json::json!({ "snapshot": snapshot })).is_err() {
                return;
            }
            let mut out = sess.out.lock().unwrap();
            if let Some(old) = out.client.take() {
                let _ = old.shutdown(Shutdown::Both); // 顶掉旧连接（同 id 只允许一个控制连接）
            }
            out.client = Some(c);
            fd
        };
        drop(lifecycle);
        break attached_fd;
    };
    let sess = &slot.value;

    // 动作循环：一行一个 AcpUserAction 的 JSON，直到客户端断开。
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(action) = serde_json::from_str(line.trim()) else {
            continue; // 认不出的行跳过，别让一条坏数据掐断整条连接
        };
        let _ = apply_acp_user_action(&sess, action, &subscribers);
    }

    let mut out = sess.out.lock().unwrap();
    if out.client.as_ref().map(|c| c.as_raw_fd()) == Some(attached_fd) {
        out.client = None;
    }
}

/// Ensure an ACP runtime exists without attaching a controlling client. Both desktop
/// `acp_open` and mobile `acp_create` use this path, so launch/resume semantics cannot drift.
fn ensure_acp_session(
    req: &AcpOpenRequest,
    acp_sessions: &AcpSessions,
    subscribers: &Subscribers,
) -> Option<Arc<AcpSlot<AcpSession>>> {
    loop {
        let (slot, created) = acp_sessions.reserve_with(&req.id, || {
            make_acp_session(
                &req.id,
                req.cwd.clone(),
                req.agent_needs_transcript_check,
            )
        });
        let lifecycle = slot.lifecycle.lock().unwrap();
        let still_current = acp_sessions
            .get(&req.id)
            .is_some_and(|current| Arc::ptr_eq(&current, &slot));
        if !still_current {
            drop(lifecycle);
            continue;
        }
        let alive = slot.value.handle.lock().unwrap().is_some();
        if acp_open_needs_relaunch(created, alive, !req.launch.command.is_empty()) {
            let known_history = {
                let reduced = slot.value.reduced.lock().unwrap();
                known_acp_resume_id(&reduced)
            };
            acp_relaunch(
                &slot,
                &req.id,
                req.launch.clone(),
                select_resume_id(req.resume_id.clone(), known_history),
                acp_sessions.spawn_gate(),
                subscribers,
            );
        }
        drop(lifecycle);
        return Some(slot);
    }
}

fn handle_acp_create(
    mut conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &Subscribers,
) {
    let response = match parse_acp_open_request(v)
        .and_then(|request| ensure_acp_session(&request, acp_sessions, subscribers))
    {
        Some(_) => serde_json::json!({"ok": true}),
        None => serde_json::json!({"ok": false, "error": "invalid ACP create request"}),
    };
    let _ = writeln!(conn, "{response}");
}

/// 只读旁观：会话必须已存在（没有 ACP 版本的「不存在就兜底 spawn」——旁观一个
/// 没人开过的会话没有意义），不参与 client 顶替，可多个并存。
fn handle_acp_watch(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    acp_sessions: AcpSessions,
) {
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return;
    }
    let Some(slot) = acp_sessions.get(&id) else {
        return;
    };
    let sess = &slot.value;
    let attached_fd = {
        let Ok(mut c) = conn.try_clone() else { return };
        let fd = c.as_raw_fd();
        let _ = c.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
        let reduced = sess.reduced.lock().unwrap();
        let entries_total = reduced.entries.len();
        let current_history_id = reduced
            .history_session_id
            .as_deref()
            .or(reduced.acp_session_id.as_deref());
        let history_matches = v["history_session_id"]
            .as_str()
            .zip(current_history_id)
            .is_some_and(|(known, current)| known == current);
        let known_entries = v["known_entries"].as_u64().map(|n| n as usize);
        let tail_limit = v["tail_limit"].as_u64().map(|n| n as usize);
        let revision_matches =
            v["snapshot_revision"].as_u64() == Some(sess.snapshot_revision.load(Ordering::SeqCst));
        let offset = if history_matches && revision_matches {
            known_entries.unwrap_or(0).min(entries_total)
        } else if let Some(limit) = tail_limit {
            entries_total.saturating_sub(limit)
        } else {
            0
        };
        let mut snapshot = reduced.to_snapshot_since(false, offset);
        snapshot.snapshot_revision = sess.snapshot_revision.load(Ordering::SeqCst);
        drop(reduced);
        if writeln!(c, "{}", serde_json::json!({ "snapshot": snapshot })).is_err() {
            return;
        }
        sess.out.lock().unwrap().watchers.push(c);
        fd
    };
    let mut scratch = [0u8; 64];
    let _ = reader.read(&mut scratch);
    sess.out
        .lock()
        .unwrap()
         .watchers
        .retain(|w| w.as_raw_fd() != attached_fd);
}

/// One-shot bounded history read used by mobile upward pagination. Unlike `acp_watch`,
/// this does not register a watcher and therefore cannot leak a long-lived connection.
fn handle_acp_snapshot(mut conn: UnixStream, v: &serde_json::Value, acp_sessions: &AcpSessions) {
    let id = v["id"].as_str().unwrap_or_default();
    let Some(slot) = acp_sessions.get(id) else {
        return;
    };
    let before = v["before"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or(usize::MAX);
    let limit = v["limit"]
        .as_u64()
        .map(|n| n as usize)
        .unwrap_or(100)
        .clamp(1, 500);
    let reduced = slot.value.reduced.lock().unwrap();
    let end = before.min(reduced.entries.len());
    let start = end.saturating_sub(limit);
    let mut snapshot = reduced.to_snapshot_range(false, start, end);
    snapshot.snapshot_revision = slot.value.snapshot_revision.load(Ordering::SeqCst);
    let _ = writeln!(conn, "{}", serde_json::json!({ "snapshot": snapshot }));
}

/// 杀会话：子进程、连接、旁观者全部收尾，从表里摘掉。跟终端 `kill` 是同一种
/// 「立即生效、不等收尾」的语气。
fn handle_acp_kill(conn: UnixStream, v: &serde_json::Value, acp_sessions: &AcpSessions) {
    let id = v["id"].as_str().unwrap_or_default();
    if let Some(slot) = acp_sessions.get(id) {
        if acp_sessions.remove_if_same(id, &slot).is_some() {
            let _lifecycle = slot.lifecycle.lock().unwrap();
            let sess = &slot.value;
            if let Some(h) = sess.handle.lock().unwrap().take() {
                let _ = h
                    .cmd_tx
                    .try_send(smelt_core::acp_conn::AcpCommand::Shutdown);
            }
            let mut out = sess.out.lock().unwrap();
            if let Some(c) = out.client.take() {
                let _ = c.shutdown(Shutdown::Both);
            }
            for w in out.watchers.drain(..) {
                let _ = w.shutdown(Shutdown::Both);
            }
        }
    }
    let mut c = conn;
    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
}

/// 强制重启：agent 子进程失联/卡死（比如 `session/cancel` 打不断正在跑的工具
/// 调用）时的兜底——跟 `acp_kill` 共享"杀进程组"这一步，但**不**摘表、不断开
/// GUI 连接、不清 watchers：会话本体（entries/id/GUI 那条 acp_open 连接）
/// 全部原样留着，只是换一个新的子进程，带着 resume_session_id 去 `session/load`
/// 接回同一份历史，对用户来说是同一个标签、同一段对话，只是"服务端"心跳重启了。
fn handle_acp_restart(
    conn: UnixStream,
    v: &serde_json::Value,
    acp_sessions: &AcpSessions,
    subscribers: &Subscribers,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let result = (|| -> Result<(), &'static str> {
        let slot = acp_sessions.get(id).ok_or("ACP session not found")?;
        let _lifecycle = slot.lifecycle.lock().unwrap();
        let sess = &slot.value;
        let Some(launch) = sess.launch_spec.lock().unwrap().clone() else {
            return Err("no launch spec recorded for this session yet");
        };
        let resume_id = {
            let reduced = sess.reduced.lock().unwrap();
            known_acp_resume_id(&reduced)
        };
        // 先礼貌地跟旧进程说 Shutdown（drive_session 收到就退出循环，栈展开时
        // KillProcessGroupOnDrop 对整个进程组 SIGKILL——跟 acp_kill 同一条路），
        // 不等它真收尾就立刻起新的：新旧进程组不同，互不干扰。
        if let Some(h) = sess.handle.lock().unwrap().take() {
            let _ = h
                .cmd_tx
                .try_send(smelt_core::acp_conn::AcpCommand::Shutdown);
        }
        acp_relaunch(
            &slot,
            id,
            launch,
            resume_id,
            acp_sessions.spawn_gate(),
            subscribers,
        );
        Ok(())
    })();
    let response = match result {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    };
    let mut c = conn;
    let _ = writeln!(c, "{}", response);
}

fn collect_acp_handoff(acp_sessions: &AcpSessions) -> (Vec<serde_json::Value>, Vec<RawFd>) {
    let acp_session_list = acp_sessions.snapshot();
    let mut acp_items = Vec::new();
    let mut acp_fds = Vec::new();
    for (id, slot) in &acp_session_list {
        let sess = &slot.value;
        let stdio = sess
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| *h.stdio.lock().unwrap());
        let Some(stdio) = stdio else { continue };
        let cmd = sess
            .state
            .lock()
            .unwrap()
            .launch
            .clone()
            .unwrap_or_default();
        let (snapshot, pending_raw_line) = {
            let reduced = sess.reduced.lock().unwrap();
            (
                reduced.to_snapshot(false),
                reduced.pending_raw_request_line().map(str::to_string),
            )
        };
        acp_items.push(serde_json::json!({
            "id": id,
            "stdin_fd": stdio.stdin_fd,
            "stdout_fd": stdio.stdout_fd,
            "pid": stdio.pid,
            "cwd": sess.cwd,
            "cmd": cmd,
            "agent_needs_transcript_check": sess.agent_needs_transcript_check,
            "snapshot": snapshot,
            "pending_raw_line": pending_raw_line,
        }));
        acp_fds.push(stdio.stdin_fd);
        acp_fds.push(stdio.stdout_fd);
    }

    let handed_off_ids: std::collections::HashSet<&str> =
        acp_items.iter().filter_map(|v| v["id"].as_str()).collect();
    for (id, slot) in &acp_session_list {
        if handed_off_ids.contains(id.as_str()) {
            continue;
        }
        let sess = &slot.value;
        if let Some(h) = sess.handle.lock().unwrap().take() {
            let _ = h
                .cmd_tx
                .try_send(smelt_core::acp_conn::AcpCommand::Shutdown);
        }
    }

    (acp_items, acp_fds)
}

fn acp_upgrade_blockers(acp_sessions: &AcpSessions) -> Vec<String> {
    use smelt_core::acp_session::AcpPhase;

    acp_sessions
        .snapshot()
        .into_iter()
        .filter_map(|(id, slot)| {
            let phase_is_quiescent = matches!(
                slot.value.reduced.lock().unwrap().phase,
                AcpPhase::Idle | AcpPhase::Ended(_)
            );
            let in_flight = slot
                .value
                .handle
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|handle| handle.in_flight_rpc.load(Ordering::SeqCst) != 0);
            (!phase_is_quiescent || in_flight).then_some(id)
        })
        .collect()
}

/// 无缝升级：快照会话表 → 写交接文件 → exec 磁盘上的新二进制（流程见文件头注释）。
///
/// 锁策略：只短暂持 sessions 锁拿一份 Arc 列表就放掉——不像早期版本那样一直攥到
/// exec，那样会让 open/list/kill/version 在升级期间全部卡在 sessions 锁上。逐会话
/// 再去拿 out 锁时，靠 handle_open 里给客户端 socket 设的 CLIENT_WRITE_TIMEOUT
/// 兜底：就算某个客户端冻结导致泵线程握着 out 锁在 write_all 里卡住，最多卡
/// CLIENT_WRITE_TIMEOUT 那么久也会因写超时放手，不会无限期挂死。
/// （极小残余窗口：某泵线程恰好已 read 出 ≤8KB 还没拿到锁，这部分随 exec 丢失。
/// 丢的只是"显示字节"不是输入；重连后的 jolt 全屏重绘会盖掉，可接受。）
fn handle_upgrade(
    conn: UnixStream,
    req: &serde_json::Value,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    listen_fd: RawFd,
) {
    let mut c = conn;
    // 可选 `"exe":"/path/to/smeltd"`：装 DMG 时先 exec 暂存目录里的新二进制，
    // 再替换 .app，避免「整包覆盖把旧 smeltd SIGKILL、会话全灭再新建」。
    // 未传则 exec current_exe（同路径更新）。
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
        match std::env::current_exe() {
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
    // 发布到 AcpHandle.stdio 的 ACP 子进程。
    let _spawn_gate = acquire_upgrade_spawn_gate(&SPAWN_GATE);

    // SDK 的 outstanding request callback 是进程内状态，不能随 stdin/stdout fd
    // 穿过 exec。只在所有 ACP 会话都处于协议静默边界时升级；调用方可在当前
    // 回合结束后重试，绝不能把 Running 快照和一条已丢 callback 的连接交出去。
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

    // ACP 会话的 fd 裸传：跟 PTY master fd 同一招（dup + 清 CLOEXEC 活过
    // exec()），另外带上 entries/phase/model 等纯数据快照。上面的静默屏障
    // 保证这里不会交接任何 outstanding callback；raw request 字段仅用于读取
    // 旧版本留下的 handoff 文件。
    //
    // 只有还活着（有 handle 且已经拿到 pid/fd——刚发起 spawn、还没跑到那一步
    // 的极窄窗口除外）的会话才能参与；已经 Ended 的没有 fd 可传，交接后就是
    // "这个 id 在新进程里不存在了"，GUI 侧本来就有 AcpSaved 兜底（按
    // resume_session_id 重新走 session/load），不算回归。
    let (acp_items, acp_fds) = collect_acp_handoff(acp_sessions);

    let session_list: Vec<(String, Arc<Session>)> = sessions
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), Arc::clone(v)))
        .collect();

    let mut out_guards = Vec::new(); // 持有到 exec，挡住泵线程
    let mut items = Vec::new();
    let mut fds = vec![listen_fd];
    fds.extend(&acp_fds);
    for (id, sess) in &session_list {
        // 锁序 term → out，与泵线程一致，避免死锁。
        let ctl = sess.ctl.lock().unwrap();
        let term = sess.term.lock().unwrap();
        let out = sess.out.lock().unwrap();
        let fd = ctl.master.as_raw_fd();
        let launch = sess.state.lock().unwrap().launch.clone();
        let alt_screen = term.mode().contains(TermMode::ALT_SCREEN);
        // 全会话同一套：可视区 keyframe（可再 feed 进同尺寸 Term，round-trip 安全）。
        // 不写 ring：resume 侧永不 feed ring，写进去只会误导后人再加特判。
        let grid = snapshot_ansi_for_handoff(&term, launch.as_deref());
        items.push(serde_json::json!({
            "id": id,
            "fd": fd,
            "pid": ctl.pid,
            "cols": ctl.cols,
            "rows": ctl.rows,
            "cwd": ctl.cwd,
            "launch": launch,
            "alt_screen": alt_screen,
            "grid": hex_encode(&grid),
        }));
        fds.push(fd);
        drop(term);
        drop(ctl);
        out_guards.push(out);
    }

    // 交接的 fd 全部清 CLOEXEC，让它们活过 exec。
    for &fd in &fds {
        set_cloexec(fd, false);
    }
    let payload = serde_json::json!({
        "listen_fd": listen_fd,
        "sessions": items,
        "acp_sessions": acp_items,
    })
    .to_string();
    let hp = handoff_path();
    if std::fs::write(&hp, payload).is_err() {
        for &fd in &fds {
            set_cloexec(fd, true);
        }
        let _ = writeln!(
            c,
            "{}",
            serde_json::json!({ "ok": false, "err": "写交接文件失败" })
        );
        return;
    }
    // 含会话 keyframe（屏幕内容），仅本用户可读写；resume 读到即删。
    let _ = std::fs::set_permissions(&hp, std::os::unix::fs::PermissionsExt::from_mode(0o600));

    // 先回执再 exec：客户端连接是 CLOEXEC 的，exec 后立即断开，回执必须赶在前面。
    // exec 失败的情况客户端会看到 ok:true 但轮询版本发现没变，按"升级未生效"处理。
    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));

    // iroh 隧道/远程网关不参与交接：exec 后新进程状态是空的，必须先停掉，
    // 否则转发的本机端口已随旧线程消失，手机侧却还以为连着。
    cleanup_sidecar_services();

    // 死前留痕：exec 可能不返回也不失败——被 macOS 内核 SIGKILL（新二进制以
    // cp 覆盖方式安装、同 inode 改写破坏签名时）。日志停在这一行而没有后续的
    // 「交接完成」，就是这种死法。
    dlog(&format!(
        "upgrade: 即将 exec {}（{} 个会话交接）",
        exe.display(),
        fds.len()
    ));

    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&exe)
        .env("SMELTD_HANDOFF", &hp)
        .exec();

    // 走到这里说明 exec 失败（新二进制没法执行）：回滚，守护继续用旧版本服务。
    // 注意：sidecar 已停，调用方若仍需要远程/隧道得再 remote_start/tunnel_start。
    let _ = std::fs::remove_file(&hp);
    for &fd in &fds {
        set_cloexec(fd, true);
    }
    dlog(&format!("upgrade: exec 失败已回滚，继续用旧版服务：{err}"));
    eprintln!("smeltd 无缝升级 exec 失败: {err}");
}

/// 开 PTY + 起 shell（环境设置与 GUI 内嵌版完全一致，见 workspace/terminal.rs 的注释）。
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

fn spawn_session(
    id: &str,
    rows: u16,
    cols: u16,
    cwd: Option<&str>,
    launch: Option<&str>,
    subscribers: &Subscribers,
) -> anyhow::Result<(Session, Box<dyn Read + Send>)> {
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
    for arg in shell_launch_args(&shell, launch) {
        cmd.arg(arg);
    }
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }
    cmd.env("TERM", "xterm-256color");
    // 少数 CLI 只认 COLORTERM 才开 24-bit 真彩（Zed 也会设）。
    cmd.env("COLORTERM", "truecolor");
    // 伪装 iTerm2：让 Claude Code 自动发 OSC 9 通知（GUI 侧捕获），见 terminal.rs 注释。
    cmd.env("TERM_PROGRAM", "iTerm.app");
    cmd.env("TERM_PROGRAM_VERSION", "3.5.0");
    // UTF-8 locale 兜底（无 LANG 时 zsh 落 C locale 会把 UTF-8 续字节转成乱码）。
    if std::env::var("LANG").is_err() {
        cmd.env("LANG", "en_US.UTF-8");
    }
    // 整条 hook 链路的地基：没有它，smelt-notify 没法知道自己在哪个会话里，
    // 后面的 state op 全是空中楼阁（见 docs/state-channel-plan.md）。
    cmd.env("SMELT_SESSION_ID", id);
    cmd.env("SMELT_SOCK", sock_path());
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

    // 把 master fd dup 成自己持有的 File（写端 + 读端各一份），portable_pty 的 pair
    // 在函数结尾 drop、关掉它自己那份 fd——PTY 只要还有 fd 开着就活着。child 句柄
    // 一并丢弃：kill/收尸都用 pid 直接做（portable_pty 的 Child drop 不杀进程）。
    let raw = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| anyhow::anyhow!("拿不到 PTY master fd"))?;
    let master = dup_file(raw)?;
    let pty_reader = master.try_clone()?;
    let state = Arc::new(Mutex::new(SessionState {
        id: id.to_string(),
        cwd: cwd.map(String::from),
        launch: launch.map(String::from),
        ..Default::default()
    }));
    let sess = Session {
        geometry_token: uuid::Uuid::new_v4().simple().to_string(),
        ctl: Mutex::new(Ctl {
            master,
            pid,
            jolt: false,
            cols,
            rows,
            cell_w: 0,
            cell_h: 0,
            remote_viewports: 0,
            cwd: cwd.map(String::from),
        }),
        out: Mutex::new(Out {
            clients: Vec::new(),
            watchers: Vec::new(),
        }),
        term: Mutex::new(new_daemon_term(
            rows,
            cols,
            StateListener {
                state: Arc::clone(&state),
                subscribers: Arc::clone(subscribers),
            },
        )),
        state,
    };
    Ok((sess, Box::new(pty_reader)))
}

/// PTY 输出泵：读 PTY → advance 常驻 Term → 转发 client / watchers。
/// shell 退出（EOF）：移除会话、断开客户端、收割子进程。
fn start_pty_pump(
    sess: Arc<Session>,
    mut pty_reader: Box<dyn Read + Send>,
    id: String,
    sessions: Sessions,
    subscribers: Subscribers,
) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut parser: Processor = Processor::new();
        let mut osc = OscScan::default();
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    // Keep the grid guard until this chunk claims the output
                    // lock. Resize uses the same term -> out order, so a
                    // geometry marker cannot overtake bytes already parsed at
                    // the previous grid size. Once `out` is held, release the
                    // grid before potentially slow socket writes.
                    let mut term_guard = sess.term.lock().ok();
                    if let Some(term) = term_guard.as_mut() {
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            parser.advance(&mut **term, chunk);
                        }));
                    }
                    // 标题事件先归约，再消费同一批字节里的 OSC 完成信号。否则标题
                    // spinner 可能在 OSC 已标完成后又把 phase 推回 Thinking。
                    let snapshot = if let Ok(mut state) = sess.state.lock() {
                        apply_osc_bytes(&mut state, &mut osc, chunk).then(|| state.clone())
                    } else {
                        // 状态镜像即使损坏也不能阻断 PTY 主数据流；继续推进扫描器，
                        // 保持跨 read 边界同步，然后照常把 chunk 转发给客户端。
                        for &byte in chunk {
                            let _ = osc.feed_notification(byte);
                        }
                        None
                    };
                    if let Some(snapshot) = snapshot {
                        broadcast_state(&subscribers, &snapshot);
                    }
                    let mut out = sess.out.lock().unwrap();
                    drop(term_guard);
                    // 每个渲染层独立转发；一路写失败只摘掉该 attachment。必须 shutdown
                    // 整条 socket，而不是只 drop 这里的写端 clone：handle_open 还持有
                    // 同一 socket 的读端，不 shutdown 的话客户端可能永远等不到 EOF，
                    // 画面就会无提示地冻结在最后一帧。
                    out.clients.retain_mut(|c| match c.write_all(chunk) {
                        Ok(()) => true,
                        Err(error) => {
                            dlog(&format!(
                                "terminal attachment write failed id={id} fd={} error={error}",
                                c.as_raw_fd()
                            ));
                            let _ = c.shutdown(Shutdown::Both);
                            false
                        }
                    });
                    out.watchers.retain_mut(|w| match w.write_all(chunk) {
                        Ok(()) => true,
                        Err(error) => {
                            dlog(&format!(
                                "terminal watcher write failed id={id} fd={} error={error}",
                                w.as_raw_fd()
                            ));
                            let _ = w.shutdown(Shutdown::Both);
                            false
                        }
                    });
                    drop(out);
                }
            }
        }
        sessions.lock().unwrap().remove(&id);
        smelt_core::app_log::info("session", &format!("会话 {id} 已结束（shell 退出）"));
        let mut out = sess.out.lock().unwrap();
        for c in out.clients.drain(..) {
            let _ = c.shutdown(Shutdown::Both); // GUI 读到 EOF 即知 shell 退出
        }
        for w in out.watchers.drain(..) {
            let _ = w.shutdown(Shutdown::Both); // 旁观者同样该收到 EOF
        }
        drop(out);
        // 收尸避免僵尸进程。shell 是本进程的子进程，且 exec 交接不改变父子关系
        // （同 PID），所以交接后 waitpid 照常有效。
        let pid = sess.ctl.lock().unwrap().pid;
        unsafe {
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    });
}

// ===================== 网格 → ANSI 快照（完整：history + 可视区 + 模式）=====================

use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Cell;

/// 快捷启动 / 命令行是否像 agent TUI（未必进 1049 备用屏，但灌网格会花屏）。
fn is_agent_tui_launch(launch: Option<&str>) -> bool {
    let Some(l) = launch.map(|s| s.to_ascii_lowercase()) else {
        return false;
    };
    [
        "claude",
        "grok",
        "codex",
        "gemini",
        "copilot",
        "aider",
        "opencode",
        "cursor agent",
    ]
    .iter()
    .any(|k| l.contains(k))
}

/// 是否按 TUI 处理（备用屏或 agent 启动命令）。
fn is_tui_session<T: EventListener>(term: &Term<T>, launch: Option<&str>) -> bool {
    term.mode().contains(TermMode::ALT_SCREEN) || is_agent_tui_launch(launch)
}

/// GUI 客户端 reattach 用：TUI 只画可视区；主屏 shell 带 scrollback history。
fn snapshot_ansi<T: EventListener>(term: &Term<T>, launch: Option<&str>) -> Vec<u8> {
    if is_tui_session(term, launch) {
        snapshot_viewport(term)
    } else {
        snapshot_with_history(term)
    }
}

/// Read-only renderers need actual scrollback whenever the PTY is on its main
/// screen. Agent identity alone is not enough to discard main-screen history.
fn snapshot_ansi_for_watch<T: EventListener>(term: &Term<T>, _launch: Option<&str>) -> Vec<u8> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        snapshot_viewport(term)
    } else {
        snapshot_with_history(term)
    }
}

/// 写入 handoff.json 的 grid：全会话统一**仅可视区**，可再 feed 进同尺寸空 Term。
fn snapshot_ansi_for_handoff<T: EventListener>(term: &Term<T>, _launch: Option<&str>) -> Vec<u8> {
    snapshot_viewport(term)
}

fn snapshot_viewport<T: EventListener>(term: &Term<T>) -> Vec<u8> {
    let mut out = snapshot_mode_prefix(term, /*clear_scrollback=*/ false);
    paint_viewport_keyframe(&mut out, term);
    snapshot_cursor_suffix(term, &mut out);
    out
}

fn snapshot_with_history<T: EventListener>(term: &Term<T>) -> Vec<u8> {
    let mut out = snapshot_mode_prefix(term, /*clear_scrollback=*/ true);
    // Disable autowrap while serializing full-width rows. Explicit CRLFs then
    // build real scrollback instead of CUP row numbers clamping to the screen.
    out.extend_from_slice(b"\x1b[?7l");
    paint_history_keyframe(&mut out, term);
    if term.mode().contains(TermMode::LINE_WRAP) {
        out.extend_from_slice(b"\x1b[?7h");
    }
    snapshot_cursor_suffix(term, &mut out);
    out
}

fn snapshot_mode_prefix<T: EventListener>(term: &Term<T>, clear_scrollback: bool) -> Vec<u8> {
    let mode = *term.mode();
    let cols = term.columns().max(1);
    let screen_lines = term.screen_lines().max(1);
    let mut out = Vec::with_capacity(cols.saturating_mul(screen_lines).saturating_mul(8));
    out.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l");
    if mode.contains(TermMode::ALT_SCREEN) {
        out.extend_from_slice(b"\x1b[?1049h");
    } else {
        out.extend_from_slice(b"\x1b[?1049l");
    }
    if mode.contains(TermMode::LINE_WRAP) {
        out.extend_from_slice(b"\x1b[?7h");
    } else {
        out.extend_from_slice(b"\x1b[?7l");
    }
    append_mode_restores(&mut out, mode);
    out.extend_from_slice(b"\x1b[?25l\x1b[0m\x1b[H\x1b[2J");
    if clear_scrollback {
        out.extend_from_slice(b"\x1b[3J");
    }
    out
}

fn snapshot_cursor_suffix<T: EventListener>(term: &Term<T>, out: &mut Vec<u8>) {
    let cols = term.columns().max(1);
    let screen_lines = term.screen_lines().max(1);
    let content = term.renderable_content();
    let cursor = content.cursor.point;
    let display_offset = term.grid().display_offset();
    let cursor_row = cursor.line.0 + display_offset as i32;
    if cursor_row >= 0 && (cursor_row as usize) < screen_lines {
        let col = cursor.column.0.min(cols.saturating_sub(1));
        let _ = write!(out, "\x1b[{};{}H", cursor_row as usize + 1, col + 1);
        match content.cursor.shape {
            CursorShape::Hidden => out.extend_from_slice(b"\x1b[?25l"),
            CursorShape::Underline => out.extend_from_slice(b"\x1b[4 q\x1b[?25h"),
            CursorShape::Beam => out.extend_from_slice(b"\x1b[6 q\x1b[?25h"),
            CursorShape::HollowBlock => out.extend_from_slice(b"\x1b[0 q\x1b[?25h"),
            CursorShape::Block => out.extend_from_slice(b"\x1b[2 q\x1b[?25h"),
        }
    }

    // The next PTY bytes are a diff against the terminal's current rendition.
    // Restore it after painting the keyframe so live output starts from the same state.
    let style = CellStyle::from_cell(&term.grid().cursor.template);
    if style.link.is_some() {
        emit_link_osc(out, style.link.as_deref());
    }
    emit_absolute_sgr(out, &style);
}

/// TUI 可视区 keyframe：按行 CUP + 绝对 SGR（Codux `terminal_snapshot_data` 同构）。
fn paint_viewport_keyframe<T: EventListener>(out: &mut Vec<u8>, term: &Term<T>) {
    let cols = term.columns().max(1);
    let rows = term.screen_lines().max(1);
    let display_offset = term.grid().display_offset();

    // row → (col → cell 引用通过复制字符+样式)
    let mut grid: Vec<Vec<Option<KeyframeCell>>> = vec![vec![None; cols]; rows];
    for indexed in term.renderable_content().display_iter {
        let row = indexed.point.line.0 + display_offset as i32;
        if row < 0 || row as usize >= rows {
            continue;
        }
        let col = indexed.point.column.0;
        if col >= cols {
            continue;
        }
        let cell = indexed.cell;
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let mut text = String::new();
        if cell.c != '\0' && !cell.c.is_control() {
            text.push(cell.c);
        }
        if let Some(zw) = cell.zerowidth() {
            for &ch in zw {
                if !ch.is_control() {
                    text.push(ch);
                }
            }
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        // 空白且默认样式：跳过，让主题底透出（Codux 同策略）
        if text.trim().is_empty()
            && is_default_fg(cell.fg)
            && is_default_bg(cell.bg)
            && !cell_has_visuals(cell)
        {
            continue;
        }
        grid[row as usize][col] = Some(KeyframeCell {
            text,
            width,
            style: CellStyle::from_cell(cell),
        });
    }

    emit_keyframe_rows(out, &grid);
}

/// Shell：history + 可视区，按缓冲行顺序硬换行推进（绝对 SGR）。
fn paint_history_keyframe<T: EventListener>(out: &mut Vec<u8>, term: &Term<T>) {
    let cols = term.columns().max(1);
    let top = term.topmost_line();
    let bottom = term.bottommost_line();
    let span = (bottom.0 - top.0 + 1).max(0) as usize;
    let start = if span > SNAPSHOT_MAX_LINES {
        Line(bottom.0 - SNAPSHOT_MAX_LINES as i32 + 1)
    } else {
        top
    };

    let mut rows: Vec<Vec<Option<KeyframeCell>>> = Vec::new();
    let mut line = start;
    while line <= bottom {
        let row = &term.grid()[line];
        let mut cells = vec![None; cols];
        for col in 0..cols {
            let cell = &row[Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut text = String::new();
            if cell.c != '\0' && !cell.c.is_control() {
                text.push(cell.c);
            }
            if let Some(zw) = cell.zerowidth() {
                for &ch in zw {
                    if !ch.is_control() {
                        text.push(ch);
                    }
                }
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            if text.trim().is_empty()
                && is_default_fg(cell.fg)
                && is_default_bg(cell.bg)
                && !cell_has_visuals(cell)
            {
                continue;
            }
            cells[col] = Some(KeyframeCell {
                text,
                width,
                style: CellStyle::from_cell(cell),
            });
        }
        rows.push(cells);
        line += 1;
    }
    emit_history_rows(out, &rows);
}

fn cell_has_visuals(cell: &Cell) -> bool {
    let f = cell.flags;
    f.intersects(
        Flags::BOLD
            | Flags::DIM
            | Flags::ITALIC
            | Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::UNDERCURL
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE
            | Flags::INVERSE
            | Flags::HIDDEN
            | Flags::STRIKEOUT
            | Flags::BOLD_ITALIC
            | Flags::DIM_BOLD,
    ) || cell.hyperlink().is_some()
}

#[derive(Clone, Debug, PartialEq)]
struct CellStyle {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: u8,
    inverse: bool,
    hidden: bool,
    strike: bool,
    link: Option<String>,
}

impl CellStyle {
    fn default_style() -> Self {
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            bold: false,
            dim: false,
            italic: false,
            underline: 0,
            inverse: false,
            hidden: false,
            strike: false,
            link: None,
        }
    }

    fn from_cell(cell: &Cell) -> Self {
        let f = cell.flags;
        Self {
            fg: cell.fg,
            bg: cell.bg,
            bold: f.contains(Flags::BOLD) || f.contains(Flags::BOLD_ITALIC),
            dim: f.contains(Flags::DIM) || f.contains(Flags::DIM_BOLD),
            italic: f.contains(Flags::ITALIC) || f.contains(Flags::BOLD_ITALIC),
            underline: underline_kind(f),
            inverse: f.contains(Flags::INVERSE),
            hidden: f.contains(Flags::HIDDEN),
            strike: f.contains(Flags::STRIKEOUT),
            link: cell.hyperlink().map(|h| h.uri().to_string()),
        }
    }
}

#[derive(Clone)]
struct KeyframeCell {
    text: String,
    width: usize,
    style: CellStyle,
}

/// 按行 `\x1b[row;1H` + 绝对 SGR 吐出（Codux `terminal_snapshot_data`）。
/// 每行画完后 `\x1b[K`（EL）清掉行尾残留，避免长输出软换行后 prompt 盖不干净。
fn emit_keyframe_rows(out: &mut Vec<u8>, rows: &[Vec<Option<KeyframeCell>>]) {
    let mut current = CellStyle::default_style();
    for (row_index, row_cells) in rows.iter().enumerate() {
        let Some(last_col) = row_cells.iter().rposition(|c| {
            c.as_ref().is_some_and(|cell| {
                !cell.text.trim().is_empty() || cell.style != CellStyle::default_style()
            })
        }) else {
            // 空行也 CUP + EL，清掉可能残留的旧内容
            let _ = write!(out, "\x1b[{};1H\x1b[K", row_index + 1);
            continue;
        };
        let _ = write!(out, "\x1b[{};1H", row_index + 1);
        let mut col = 0;
        while col <= last_col {
            match &row_cells[col] {
                Some(cell) => {
                    if cell.style != current {
                        if cell.style.link != current.link {
                            emit_link_osc(out, cell.style.link.as_deref());
                        }
                        emit_absolute_sgr(out, &cell.style);
                        current = cell.style.clone();
                    }
                    if cell.text.is_empty() {
                        for _ in 0..cell.width.max(1) {
                            out.push(b' ');
                        }
                    } else {
                        for ch in cell.text.chars() {
                            push_char(out, ch);
                        }
                    }
                    col += cell.width.max(1);
                }
                None => {
                    if current != CellStyle::default_style() {
                        if current.link.is_some() {
                            emit_link_osc(out, None);
                        }
                        out.extend_from_slice(b"\x1b[0m");
                        current = CellStyle::default_style();
                    }
                    out.push(b' ');
                    col += 1;
                }
            }
        }
        // 行尾 EL：抹掉该行 last_col 之后的旧字符（长 cargo 行糊进 prompt 的主因）
        if current != CellStyle::default_style() {
            if current.link.is_some() {
                emit_link_osc(out, None);
            }
            out.extend_from_slice(b"\x1b[0m");
            current = CellStyle::default_style();
        }
        out.extend_from_slice(b"\x1b[K");
    }
    if current != CellStyle::default_style() {
        if current.link.is_some() {
            emit_link_osc(out, None);
        }
        out.extend_from_slice(b"\x1b[0m");
    }
}

/// Emit buffered lines sequentially so lines above the viewport become real
/// terminal history. CUP cannot address rows outside the visible screen.
fn emit_history_rows(out: &mut Vec<u8>, rows: &[Vec<Option<KeyframeCell>>]) {
    let mut current = CellStyle::default_style();
    for (row_index, row_cells) in rows.iter().enumerate() {
        out.push(b'\r');
        let last_col = row_cells.iter().rposition(|c| {
            c.as_ref().is_some_and(|cell| {
                !cell.text.trim().is_empty() || cell.style != CellStyle::default_style()
            })
        });
        if let Some(last_col) = last_col {
            let mut col = 0;
            while col <= last_col {
                match &row_cells[col] {
                    Some(cell) => {
                        if cell.style != current {
                            if cell.style.link != current.link {
                                emit_link_osc(out, cell.style.link.as_deref());
                            }
                            emit_absolute_sgr(out, &cell.style);
                            current = cell.style.clone();
                        }
                        if cell.text.is_empty() {
                            for _ in 0..cell.width.max(1) {
                                out.push(b' ');
                            }
                        } else {
                            for ch in cell.text.chars() {
                                push_char(out, ch);
                            }
                        }
                        col += cell.width.max(1);
                    }
                    None => {
                        if current != CellStyle::default_style() {
                            if current.link.is_some() {
                                emit_link_osc(out, None);
                            }
                            out.extend_from_slice(b"\x1b[0m");
                            current = CellStyle::default_style();
                        }
                        out.push(b' ');
                        col += 1;
                    }
                }
            }
        }
        if current != CellStyle::default_style() {
            if current.link.is_some() {
                emit_link_osc(out, None);
            }
            out.extend_from_slice(b"\x1b[0m");
            current = CellStyle::default_style();
        }
        out.extend_from_slice(b"\x1b[K");
        if row_index + 1 < rows.len() {
            out.extend_from_slice(b"\r\n");
        }
    }
}

fn emit_link_osc(out: &mut Vec<u8>, uri: Option<&str>) {
    out.extend_from_slice(b"\x1b]8;;");
    if let Some(u) = uri {
        out.extend_from_slice(u.as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
}

/// 绝对 SGR：始终以 `0` 开头（Codux `snapshot_style_sgr`），杜绝差分状态机半截泄漏。
fn emit_absolute_sgr(out: &mut Vec<u8>, style: &CellStyle) {
    let mut params = Vec::with_capacity(32);
    params.push(b'0');
    let push = |params: &mut Vec<u8>, code: u8| {
        params.push(b';');
        push_u8(params, code);
    };
    if style.bold {
        push(&mut params, 1);
    }
    if style.dim {
        push(&mut params, 2);
    }
    if style.italic {
        push(&mut params, 3);
    }
    if style.underline != 0 {
        params.push(b';');
        match style.underline {
            1 => params.extend_from_slice(b"4"),
            2 => params.extend_from_slice(b"4:2"),
            3 => params.extend_from_slice(b"4:3"),
            4 => params.extend_from_slice(b"4:4"),
            5 => params.extend_from_slice(b"4:5"),
            _ => params.extend_from_slice(b"4"),
        }
    }
    if style.inverse {
        push(&mut params, 7);
    }
    if style.hidden {
        push(&mut params, 8);
    }
    if style.strike {
        push(&mut params, 9);
    }
    // 颜色：绝对模式下总是写上（含默认 39/49），与 Codux 一致
    append_color_params_abs(&mut params, true, style.fg);
    append_color_params_abs(&mut params, false, style.bg);

    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(&params);
    out.push(b'm');
}

fn append_color_params_abs(params: &mut Vec<u8>, is_fg: bool, color: Color) {
    params.push(b';');
    match color {
        Color::Named(n) => {
            push_u8(params, named_sgr_code(n, is_fg));
        }
        Color::Indexed(i) => {
            push_u8(params, if is_fg { 38 } else { 48 });
            params.extend_from_slice(b";5;");
            push_u8(params, i);
        }
        Color::Spec(rgb) => {
            push_u8(params, if is_fg { 38 } else { 48 });
            params.extend_from_slice(b";2;");
            push_u8(params, rgb.r);
            params.push(b';');
            push_u8(params, rgb.g);
            params.push(b';');
            push_u8(params, rgb.b);
        }
    }
}

fn push_char(out: &mut Vec<u8>, ch: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
}

fn append_mode_restores(out: &mut Vec<u8>, mode: TermMode) {
    if mode.contains(TermMode::APP_CURSOR) {
        out.extend_from_slice(b"\x1b[?1h");
    }
    if mode.contains(TermMode::BRACKETED_PASTE) {
        out.extend_from_slice(b"\x1b[?2004h");
    }
    // 鼠标：按实际打开的子模式恢复（SGR 优先）
    if mode.intersects(TermMode::MOUSE_MODE) {
        if mode.contains(TermMode::SGR_MOUSE) {
            out.extend_from_slice(b"\x1b[?1006h");
        }
        if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            out.extend_from_slice(b"\x1b[?1000h");
        }
        if mode.contains(TermMode::MOUSE_DRAG) {
            out.extend_from_slice(b"\x1b[?1002h");
        }
        if mode.contains(TermMode::MOUSE_MOTION) {
            out.extend_from_slice(b"\x1b[?1003h");
        }
    }
    if mode.contains(TermMode::FOCUS_IN_OUT) {
        out.extend_from_slice(b"\x1b[?1004h");
    }
}

fn underline_kind(flags: Flags) -> u8 {
    if flags.contains(Flags::UNDERCURL) {
        3
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        2
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        4
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        5
    } else if flags.contains(Flags::UNDERLINE) {
        1
    } else {
        0
    }
}

fn push_u8(params: &mut Vec<u8>, n: u8) {
    if n >= 100 {
        params.push(b'0' + n / 100);
        params.push(b'0' + (n / 10) % 10);
        params.push(b'0' + n % 10);
    } else if n >= 10 {
        params.push(b'0' + n / 10);
        params.push(b'0' + n % 10);
    } else {
        params.push(b'0' + n);
    }
}

fn is_default_fg(c: Color) -> bool {
    matches!(c, Color::Named(NamedColor::Foreground))
}
fn is_default_bg(c: Color) -> bool {
    matches!(c, Color::Named(NamedColor::Background))
}

fn named_sgr_code(n: NamedColor, is_fg: bool) -> u8 {
    use NamedColor::*;
    match (n, is_fg) {
        (Black, true) => 30,
        (Red, true) => 31,
        (Green, true) => 32,
        (Yellow, true) => 33,
        (Blue, true) => 34,
        (Magenta, true) => 35,
        (Cyan, true) => 36,
        (White, true) => 37,
        (Foreground, true) => 39,
        (BrightBlack, true) => 90,
        (BrightRed, true) => 91,
        (BrightGreen, true) => 92,
        (BrightYellow, true) => 93,
        (BrightBlue, true) => 94,
        (BrightMagenta, true) => 95,
        (BrightCyan, true) => 96,
        (BrightWhite, true) => 97,
        (Black, false) => 40,
        (Red, false) => 41,
        (Green, false) => 42,
        (Yellow, false) => 43,
        (Blue, false) => 44,
        (Magenta, false) => 45,
        (Cyan, false) => 46,
        (White, false) => 47,
        (Background, false) => 49,
        (BrightBlack, false) => 100,
        (BrightRed, false) => 101,
        (BrightGreen, false) => 102,
        (BrightYellow, false) => 103,
        (BrightBlue, false) => 104,
        (BrightMagenta, false) => 105,
        (BrightCyan, false) => 106,
        (BrightWhite, false) => 107,
        (_, true) => 39,
        (_, false) => 49,
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Processor;

    fn visible_text(term: &Term<VoidListener>) -> String {
        term.renderable_content()
            .display_iter
            .map(|i| i.cell.c)
            .filter(|c| *c != '\0')
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 把整个网格（含 history）逐格 dump 成文本行——`\0`（宽字符占位格）画成 `·`，
    /// 好让 assert 失败时能一眼看出「哪一列开始错位」。
    fn grid_dump(term: &Term<VoidListener>) -> Vec<String> {
        let mut rows = Vec::new();
        let mut line = term.topmost_line();
        let bottom = term.bottommost_line();
        while line <= bottom {
            let mut s = String::new();
            for col in 0..term.columns() {
                let c = term.grid()[line][Column(col)].c;
                s.push(if c == '\0' { '·' } else { c });
            }
            rows.push(s);
            line += 1;
        }
        rows
    }

    /// 逐格 dump **颜色与属性**——`grid_dump` 只比字符，颜色错了它一无所知（真实的
    /// reattach bug 正是「字符都在、前景色被恢复成不可见」，字符级对比全绿）。
    /// 只 dump 非空格单元，输出紧凑，assert 失败时能直接看出哪个格子的 fg/bg 变了。
    fn attr_dump(term: &Term<VoidListener>) -> Vec<String> {
        let mut out = Vec::new();
        let mut line = term.topmost_line();
        let bottom = term.bottommost_line();
        while line <= bottom {
            for col in 0..term.columns() {
                let cell = &term.grid()[line][Column(col)];
                if cell.c == ' ' || cell.c == '\0' {
                    continue; // 空白格的前景色无所谓
                }
                out.push(format!(
                    "({},{}) {:?} fg={:?} bg={:?} flags={:?}",
                    line.0, col, cell.c, cell.fg, cell.bg, cell.flags
                ));
            }
            line += 1;
        }
        out
    }

    /// 快照的根本契约：**重放后的网格必须和原网格逐格相同**。
    /// 比「快照里含某段文本」强得多——丢格、列错位、行粘连都能抓到。
    fn assert_roundtrip(rows: usize, cols: usize, input: &str, what: &str) {
        let size = DaemonTermSize { rows, cols };
        let mut a = Term::new(daemon_term_config(), &size, VoidListener);
        let mut pa: Processor = Processor::new();
        pa.advance(&mut a, input.as_bytes());

        let snap = snapshot_ansi(&a, None);

        let mut b = Term::new(daemon_term_config(), &size, VoidListener);
        let mut pb: Processor = Processor::new();
        pb.advance(&mut b, &snap);

        // 颜色/属性必须也一致——真实 bug 就藏在这里，字符级对比看不见。
        let (want_attr, got_attr) = (attr_dump(&a), attr_dump(&b));
        assert_eq!(
            want_attr,
            got_attr,
            "\n{what}：快照重放后**颜色/属性**错了（字符可能都还在）\n快照字节: {:?}\n",
            String::from_utf8_lossy(&snap)
        );

        let (want, got) = (grid_dump(&a), grid_dump(&b));
        assert_eq!(
            want,
            got,
            "\n{what}：快照重放后网格错位\n原始:\n{}\n重放:\n{}\n快照字节: {:?}",
            want.join("\n"),
            got.join("\n"),
            String::from_utf8_lossy(&snap)
        );
    }

    /// 行尾放不下宽字符：alacritty 在最后一列填 LEADING_WIDE_CHAR_SPACER，宽字符挪到下一行。
    /// 快照 `continue` 跳过这个占位格 → 该行只吐 cols-1 个字符 → 不触发自动折行。
    #[test]
    fn roundtrip_wide_char_at_line_end() {
        assert_roundtrip(4, 8, "abcdefg中x", "行尾宽字符占位格");
    }

    /// 类 Claude Code 底部状态栏：整行背景色铺满 + 中文 + 边框字形（重启后错位的就是这片）。
    #[test]
    fn roundtrip_status_bar_like() {
        assert_roundtrip(
            6,
            40,
            "\x1b[44m current  6%  5:30am │ weekly  48% \x1b[0m\r\n\
             \x1b[2m ctx:18% │ cache:100% │ 检查当前模型 \x1b[0m\r\n> ",
            "状态栏（背景色 + 中文 + 竖线）",
        );
    }

    /// 满行（写满最后一列）后跟硬换行：pending-wrap 状态处理错就会多吞/多吐一行。
    #[test]
    fn roundtrip_full_width_row_then_newline() {
        assert_roundtrip(4, 6, "abcdef\r\nxy", "满行 + 硬换行");
    }

    /// 中文占满整行（每字 2 列，正好铺满）。
    #[test]
    fn roundtrip_cjk_fills_row() {
        assert_roundtrip(4, 6, "中文字\r\nab", "中文铺满行");
    }

    /// SGR 2（DIM）——Claude Code 状态栏的灰字大量用它。怀疑对象 #1。
    #[test]
    fn roundtrip_sgr_dim() {
        assert_roundtrip(3, 20, "\x1b[2mdim gray\x1b[0m ok", "DIM 灰字");
    }

    /// DIM + 前景色组合（暗绿等）。
    #[test]
    fn roundtrip_sgr_dim_with_color() {
        assert_roundtrip(3, 20, "\x1b[2;32mdimgreen\x1b[0m ok", "DIM + 绿");
    }

    /// bright black（90）——另一种常见灰。
    #[test]
    fn roundtrip_sgr_bright_black() {
        assert_roundtrip(3, 20, "\x1b[90mgray\x1b[0m ok", "bright black 灰");
    }

    /// 256 色前景（38;5;244 = 中灰）。
    #[test]
    fn roundtrip_sgr_256color() {
        assert_roundtrip(3, 20, "\x1b[38;5;244mgray\x1b[0m ok", "256 色灰");
    }

    /// 24-bit 真彩前景。
    #[test]
    fn roundtrip_sgr_truecolor() {
        assert_roundtrip(3, 20, "\x1b[38;2;136;136;136mgray\x1b[0m ok", "真彩灰");
    }

    /// 状态栏全家桶：灰边框 + DIM + 绿数字 + 中文，一行内多次切色。
    #[test]
    fn roundtrip_sgr_status_bar_mix() {
        assert_roundtrip(
            4,
            60,
            "\x1b[2m────\x1b[0m\r\n\
             \x1b[2m ctx:\x1b[0m\x1b[32m18%\x1b[0m \x1b[2m│ cache:\x1b[0m\x1b[32m100%\x1b[0m\r\n\
             \x1b[90m current \x1b[0m\x1b[92m11%\x1b[0m \x1b[2m检查模型\x1b[0m",
            "状态栏多色混排",
        );
    }

    #[test]
    fn snapshot_roundtrip_preserves_visible_text() {
        let size = DaemonTermSize { rows: 5, cols: 20 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[31mhello\x1b[0m\r\nworld");

        let snap = snapshot_ansi(&term, None);
        assert!(snap.windows(5).any(|w| w == b"hello"));
        assert!(snap.windows(5).any(|w| w == b"world"));

        let mut term2 = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser2: Processor = Processor::new();
        parser2.advance(&mut term2, &snap);
        let text = visible_text(&term2);
        assert!(text.contains("hello"), "got {text:?}");
        assert!(text.contains("world"), "got {text:?}");
    }

    #[test]
    fn snapshot_restores_current_sgr_for_following_live_output() {
        let size = DaemonTermSize { rows: 4, cols: 30 };
        let mut original = Term::new(daemon_term_config(), &size, VoidListener);
        let mut original_parser: Processor = Processor::new();
        original_parser.advance(&mut original, b"plain \x1b[1;4;31mstyled");

        let snapshot = snapshot_ansi(&original, None);
        let mut restored = Term::new(daemon_term_config(), &size, VoidListener);
        let mut restored_parser: Processor = Processor::new();
        restored_parser.advance(&mut restored, &snapshot);

        assert_eq!(
            CellStyle::from_cell(&original.grid().cursor.template),
            CellStyle::from_cell(&restored.grid().cursor.template),
            "snapshot must restore the SGR state expected by subsequent PTY diffs"
        );

        original_parser.advance(&mut original, b" live");
        restored_parser.advance(&mut restored, b" live");
        assert_eq!(attr_dump(&original), attr_dump(&restored));
    }

    #[test]
    fn snapshot_enters_alt_screen_when_active() {
        let size = DaemonTermSize { rows: 4, cols: 10 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[?1049hTUI");
        let snap = snapshot_ansi(&term, None);
        assert!(snap.windows(8).any(|w| w == b"\x1b[?1049h"));
        // Codux 风格 keyframe：备用屏也画可视区内容
        assert!(
            snap.windows(3).any(|w| w == b"TUI"),
            "TUI keyframe 应含可视区文字, got {}",
            String::from_utf8_lossy(&snap)
        );
        assert!(snap.windows(4).any(|w| w == b"\x1b[2J"), "应清屏");
        // 绝对 SGR：每个样式序列以 ESC[0 开头
        assert!(
            snap.windows(4).any(|w| w == b"\x1b[0m") || snap.windows(4).any(|w| w == b"\x1b[0;"),
            "应有绝对 SGR"
        );
    }

    /// agent launch 走 TUI keyframe（可视区），不是空骨架。
    #[test]
    fn snapshot_agent_launch_paints_viewport_keyframe() {
        let size = DaemonTermSize { rows: 4, cols: 20 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"hello-grok-grid");
        let snap = snapshot_ansi(&term, Some("grok"));
        assert!(
            snap.windows(10).any(|w| w == b"hello-grok"),
            "agent keyframe 应含可视区: {}",
            String::from_utf8_lossy(&snap)
        );
        // 按行 CUP
        assert!(snap.windows(4).any(|w| w == b"\x1b[1;"), "应按行 CUP 定位");
    }

    #[test]
    fn watch_snapshot_keeps_main_screen_history_for_agent_launch() {
        let size = DaemonTermSize { rows: 3, cols: 40 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..10 {
            parser.advance(&mut term, format!("agent-line-{i:02}\r\n").as_bytes());
        }

        let snap = snapshot_ansi_for_watch(&term, Some("codex"));
        assert!(snap.windows(13).any(|w| w == b"agent-line-00"));
        assert!(snap.windows(13).any(|w| w == b"agent-line-09"));
    }

    /// 真彩 SGR 必须以完整 `\x1b[0;…48;2;…m` 形式出现（Codux 绝对 SGR）。
    #[test]
    fn snapshot_truecolor_sgr_always_has_esc_prefix() {
        let size = DaemonTermSize { rows: 3, cols: 20 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[48;2;20;20;20mX\x1b[0m");
        let snap = snapshot_ansi(&term, None);
        let s = String::from_utf8_lossy(&snap);
        // 实际形如 \x1b[0;39;48;2;20;20;20m
        assert!(
            s.contains("\u{1b}[0;39;48;2;20;20;20m")
                || s.contains("\u{1b}[0;") && s.contains("48;2;20;20;20m"),
            "绝对 SGR 应含完整真彩序列: {s}"
        );
        // 重放后字符仍在
        let mut term2 = Term::new(daemon_term_config(), &size, VoidListener);
        let mut p2: Processor = Processor::new();
        p2.advance(&mut term2, &snap);
        assert!(visible_text(&term2).contains('X'));
    }

    #[test]
    fn is_agent_tui_launch_matches_common_agents() {
        assert!(is_agent_tui_launch(Some("grok")));
        assert!(is_agent_tui_launch(Some(
            "claude --dangerously-skip-permissions"
        )));
        assert!(is_agent_tui_launch(Some("codex")));
        assert!(!is_agent_tui_launch(Some("zsh")));
        assert!(!is_agent_tui_launch(None));
    }

    #[test]
    fn agent_launch_reads_interactive_shell_config() {
        assert_eq!(
            shell_launch_args("/bin/zsh", Some("claude --dangerously-skip-permissions")),
            vec![
                "-ilc".to_string(),
                "claude --dangerously-skip-permissions; exec /bin/zsh -l".to_string(),
            ]
        );
        assert_eq!(shell_launch_args("/bin/zsh", None), vec!["-l".to_string()]);
    }

    #[test]
    fn snapshot_includes_scrollback_history() {
        // 3 行屏高，灌 10 行 → 前几行进 history
        let size = DaemonTermSize { rows: 3, cols: 40 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        for i in 0..10 {
            parser.advance(&mut term, format!("line-{i:02}\r\n").as_bytes());
        }
        // 可视区只有最后几行；快照必须仍带上更早的 line-00
        let snap = snapshot_ansi(&term, None);
        assert!(
            snap.windows(7).any(|w| w == b"line-00"),
            "完整快照应含 scrollback 里的 line-00，实际: {}",
            String::from_utf8_lossy(&snap)
        );
        assert!(snap.windows(7).any(|w| w == b"line-09"));

        // 重放到同尺寸终端，早期行必须进入真实 scrollback，而不是用越界 CUP
        // 全部夹在可视区底部。
        let mut term2 = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser2: Processor = Processor::new();
        parser2.advance(&mut term2, &snap);
        assert!(
            term2.topmost_line().0 < 0,
            "同尺寸重放后应产生 scrollback，topmost={:?}",
            term2.topmost_line()
        );
        // 扫整个 grid（含 history）
        let mut all = String::new();
        let top = term2.topmost_line();
        let bottom = term2.bottommost_line();
        let mut line = top;
        while line <= bottom {
            for col in 0..term2.columns() {
                all.push(term2.grid()[line][Column(col)].c);
            }
            all.push('\n');
            line += 1;
        }
        assert!(
            all.contains("line-00"),
            "重放后 grid 应含 line-00，got {all:?}"
        );
        assert!(all.contains("line-09"), "重放后 grid 应含 line-09");
    }

    #[test]
    fn snapshot_restores_bracketed_paste_mode() {
        let size = DaemonTermSize { rows: 3, cols: 10 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[?2004hhi");
        let snap = snapshot_ansi(&term, None);
        assert!(
            snap.windows(8).any(|w| w == b"\x1b[?2004h"),
            "开了 bracketed paste 的会话快照应恢复该模式"
        );
    }

    #[test]
    fn snapshot_preserves_osc8_hyperlink() {
        let size = DaemonTermSize { rows: 3, cols: 40 };
        let mut term = Term::new(daemon_term_config(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(
            &mut term,
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        );
        let snap = snapshot_ansi(&term, None);
        let s = String::from_utf8_lossy(&snap);
        assert!(
            s.contains("https://example.com"),
            "快照应含 OSC 8 URI，got {s}"
        );
        assert!(snap.windows(4).any(|w| w == b"link"));
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use std::time::Instant;

    /// 造一个不依赖真实 shell 的会话：`Ctl.master` 指向 `/dev/null`（测试不发输入帧，
    /// 用不上真正的 PTY 写端），`pid` 用一个已退出、还没被 reap 的真实子进程——
    /// 给 `start_pty_pump` 结束时的 `waitpid` 一个安全、真实存在的目标，不借用 -1
    /// 或随便一个不相关的 pid。
    pub(crate) fn make_dummy_session(rows: u16, cols: u16) -> Arc<Session> {
        let master = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .unwrap();
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        drop(child); // Child::drop 不 wait()，留成 zombie，交给 pump 收尾时的 waitpid

        let state = Arc::new(Mutex::new(SessionState::default()));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let listener = StateListener {
            state: Arc::clone(&state),
            subscribers,
        };
        Arc::new(Session {
            geometry_token: uuid::Uuid::new_v4().simple().to_string(),
            ctl: Mutex::new(Ctl {
                master,
                pid,
                jolt: false,
                cols,
                rows,
                cell_w: 0,
                cell_h: 0,
                remote_viewports: 0,
                cwd: None,
            }),
            out: Mutex::new(Out {
                clients: Vec::new(),
                watchers: Vec::new(),
            }),
            term: Mutex::new(new_daemon_term(rows, cols, listener)),
            state,
        })
    }

    /// 读一行 JSON 尺寸头 + `replay_len` 字节快照——跟真实客户端的 attach 协议一致。
    fn read_header_and_snapshot(br: &mut BufReader<UnixStream>) -> serde_json::Value {
        let mut line = String::new();
        br.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let replay_len = v["replay_len"].as_u64().unwrap() as usize;
        let mut snap = vec![0u8; replay_len];
        br.read_exact(&mut snap).unwrap();
        v
    }

    #[test]
    fn remote_watch_owns_geometry_until_its_connection_closes() {
        let sess = make_dummy_session(59, 181);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::from([(
            "t".to_string(),
            Arc::clone(&sess),
        )])));
        let (server, mut client) = UnixStream::pair().unwrap();
        let sessions_for_watch = Arc::clone(&sessions);
        let watch = thread::spawn(move || {
            let reader = BufReader::new(server.try_clone().unwrap());
            handle_watch(
                server,
                reader,
                &serde_json::json!({
                    "id": "t",
                    "controls_geometry": true,
                    "cols": 49,
                    "rows": 47,
                    "cell_w": 8,
                    "cell_h": 15,
                }),
                sessions_for_watch,
            );
        });

        let mut reader = BufReader::new(client.try_clone().unwrap());
        let header = read_header_and_snapshot(&mut reader);
        assert_eq!(header["cols"], 49);
        assert_eq!(header["rows"], 47);
        assert_eq!(sess.ctl.lock().unwrap().remote_viewports, 1);

        // A focused desktop may try to reassert its large viewport. The
        // daemon must keep the mobile canonical grid while the lease lives.
        resize_session(&sess, 181, 59, 9, 18);
        {
            let ctl = sess.ctl.lock().unwrap();
            assert_eq!((ctl.cols, ctl.rows), (49, 47));
        }

        let geometry = TerminalGeometryParamsForTest {
            cols: 55,
            rows: 40,
            cell_w: 8,
            cell_h: 15,
        };
        client.write_all(&geometry.frame()).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        watch.join().unwrap();

        {
            let ctl = sess.ctl.lock().unwrap();
            assert_eq!((ctl.cols, ctl.rows), (55, 40));
            assert_eq!(ctl.remote_viewports, 0);
        }
        resize_session(&sess, 181, 59, 9, 18);
        let ctl = sess.ctl.lock().unwrap();
        assert_eq!((ctl.cols, ctl.rows), (181, 59));
    }

    #[test]
    fn mobile_watch_rejolts_the_tui_after_the_watcher_is_attached() {
        // attach 时的那次 SIGWINCH 发生在 watcher 挂载之前、快照抓取之后没有任何补抖，
        // TUI 若只重绘半屏（Claude 等），移动端首次进入就会停在旧画面上。守护必须在
        // watcher 挂上之后继续补抖，让重绘字节真正流到移动端。
        let sess = make_dummy_session(59, 181);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::from([(
            "t".to_string(),
            Arc::clone(&sess),
        )])));

        // 每次 resize_session_* 都会给桌面客户端写一条 geometry OSC 标记——用它数抖动次数。
        let (desktop_server, desktop_client) = UnixStream::pair().unwrap();
        sess.out.lock().unwrap().clients.push(desktop_server);

        let (server, client) = UnixStream::pair().unwrap();
        let sessions_for_watch = Arc::clone(&sessions);
        let watch = thread::spawn(move || {
            let reader = BufReader::new(server.try_clone().unwrap());
            handle_watch(
                server,
                reader,
                &serde_json::json!({
                    "id": "t",
                    "controls_geometry": true,
                    "cols": 49,
                    "rows": 47,
                    "cell_w": 8,
                    "cell_h": 15,
                }),
                sessions_for_watch,
            );
        });

        let mut reader = BufReader::new(client.try_clone().unwrap());
        read_header_and_snapshot(&mut reader);

        desktop_client
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let token = sess.geometry_token.as_bytes().to_vec();
        let mut seen = Vec::new();
        let mut desktop_client = desktop_client;
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            let mut buffer = [0u8; 4096];
            match desktop_client.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => seen.extend_from_slice(&buffer[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
            let markers = seen.windows(token.len()).filter(|w| *w == token).count();
            if markers >= 3 {
                break;
            }
        }

        let markers = seen.windows(token.len()).filter(|w| *w == token).count();
        assert!(
            markers >= 3,
            "attach 后必须再补抖两次（共 ≥3 条 geometry 标记），实际 {markers} 条"
        );

        client.shutdown(Shutdown::Write).unwrap();
        watch.join().unwrap();
    }

    struct TerminalGeometryParamsForTest {
        cols: u16,
        rows: u16,
        cell_w: u16,
        cell_h: u16,
    }

    impl TerminalGeometryParamsForTest {
        fn frame(&self) -> Vec<u8> {
            let mut payload = Vec::with_capacity(16);
            for value in [self.cols, self.rows, self.cell_w, self.cell_h] {
                payload.extend_from_slice(&u32::from(value).to_be_bytes());
            }
            let mut frame = vec![1];
            frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            frame.extend_from_slice(&payload);
            frame
        }
    }

    #[test]
    fn attach_only_open_does_not_create_a_missing_session() {
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let (server, client) = UnixStream::pair().unwrap();
        let reader = BufReader::new(server.try_clone().unwrap());

        handle_open(
            server,
            reader,
            &serde_json::json!({
                "id": "missing",
                "cols": 80,
                "rows": 24,
                "create_if_missing": false,
            }),
            Arc::clone(&sessions),
            subscribers,
        );

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["ok"], false);
        assert!(response["err"].as_str().unwrap().contains("不存在"));
        assert!(sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn multiple_opens_and_watch_receive_the_same_output_independently() {
        let sess = make_dummy_session(24, 80);
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions
            .lock()
            .unwrap()
            .insert("t".to_string(), Arc::clone(&sess));

        // 模拟 PTY：pump 从一端读，测试从另一端写，模拟"shell 产生了输出"。
        let (pty_reader_end, mut pty_writer_end) = UnixStream::pair().unwrap();
        start_pty_pump(
            Arc::clone(&sess),
            Box::new(pty_reader_end),
            "t".to_string(),
            Arc::clone(&sessions),
            Arc::new(Mutex::new(Vec::new())),
        );

        // 第一路：桌面 attachment。
        let (open_server, open_client) = UnixStream::pair().unwrap();
        let sessions_a = Arc::clone(&sessions);
        let subscribers_a: Subscribers = Arc::new(Mutex::new(Vec::new()));
        thread::spawn(move || {
            let reader = BufReader::new(open_server.try_clone().unwrap());
            handle_open(
                open_server,
                reader,
                &serde_json::json!({"id":"t","cols":80,"rows":24}),
                sessions_a,
                subscribers_a,
            );
        });
        let mut open_br = BufReader::new(open_client.try_clone().unwrap());
        let open_header = read_header_and_snapshot(&mut open_br);
        assert_eq!(open_header["geometry_token"], sess.geometry_token.as_str());

        // 第二路：另一个交互 attachment。同 id 并行 open 不该顶掉第一路。
        let (open2_server, open2_client) = UnixStream::pair().unwrap();
        let sessions_b = Arc::clone(&sessions);
        let subscribers_b: Subscribers = Arc::new(Mutex::new(Vec::new()));
        thread::spawn(move || {
            let reader = BufReader::new(open2_server.try_clone().unwrap());
            handle_open(
                open2_server,
                reader,
                &serde_json::json!({
                    "id": "t",
                    "cols": 80,
                    "rows": 24,
                    "create_if_missing": false,
                }),
                sessions_b,
                subscribers_b,
            );
        });
        let mut open2_br = BufReader::new(open2_client.try_clone().unwrap());
        read_header_and_snapshot(&mut open2_br);

        // 第三路：watch（只读旁观）。同样不影响两个 open attachment。
        let (watch_server, watch_client) = UnixStream::pair().unwrap();
        let sessions_c = Arc::clone(&sessions);
        thread::spawn(move || {
            let reader = BufReader::new(watch_server.try_clone().unwrap());
            handle_watch(
                watch_server,
                reader,
                &serde_json::json!({"id":"t"}),
                sessions_c,
            );
        });
        let mut watch_br = BufReader::new(watch_client.try_clone().unwrap());
        read_header_and_snapshot(&mut watch_br);

        let out = sess.out.lock().unwrap();
        assert_eq!(out.clients.len(), 2);
        assert_eq!(out.watchers.len(), 1);
        drop(out);

        // 模拟 shell 输出一行字节，两个 open 和 watch 都该收到同一份转发。
        pty_writer_end.write_all(b"hello\r\n").unwrap();

        let mut open_buf = [0u8; 7];
        open_br.read_exact(&mut open_buf).unwrap();
        assert_eq!(
            &open_buf, b"hello\r\n",
            "open 没收到转发——watch 的接入可能把它顶掉了"
        );

        let mut open2_buf = [0u8; 7];
        open2_br.read_exact(&mut open2_buf).unwrap();
        assert_eq!(
            &open2_buf, b"hello\r\n",
            "第二个 open 没收到转发——可能仍在执行单 client 顶替"
        );

        let mut watch_buf = [0u8; 7];
        watch_br.read_exact(&mut watch_buf).unwrap();
        assert_eq!(&watch_buf, b"hello\r\n", "watch 没收到转发");

        // watcher 断开，不该影响两路 open 继续收转发（惰性清理：写失败即摘除，
        // 不依赖 handle_watch 自己那个线程的清理时序）。
        drop(watch_br);
        drop(watch_client);

        pty_writer_end.write_all(b"world!\n").unwrap();
        let mut open_buf2 = [0u8; 7];
        open_br.read_exact(&mut open_buf2).unwrap();
        assert_eq!(
            &open_buf2, b"world!\n",
            "watcher 断线后不该影响 open 那一路的转发"
        );
        let mut open2_buf2 = [0u8; 7];
        open2_br.read_exact(&mut open2_buf2).unwrap();
        assert_eq!(
            &open2_buf2, b"world!\n",
            "watcher 断线后不该影响第二路 open 的转发"
        );

        // 收尾：关掉模拟 PTY 的写端，触发 pump 的退出清理（移除会话表项 + waitpid）。
        drop(pty_writer_end);
        let mut removed = false;
        for _ in 0..50 {
            if !sessions.lock().unwrap().contains_key("t") {
                removed = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(removed, "pump 应在 PTY EOF 后把会话从表里摘掉");

        drop(open_br);
        drop(open_client);
        drop(open2_br);
        drop(open2_client);
    }

    /// subscribe：首帧全量快照，之后 state 变化推一行——跟真实 `state` op 走的是
    /// 同一条 broadcast_state 路径。
    #[test]
    fn subscribe_gets_snapshot_then_broadcast_on_state_change() {
        let sess = make_dummy_session(24, 80);
        sess.state.lock().unwrap().id = "sub-test".to_string();

        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions
            .lock()
            .unwrap()
            .insert("sub-test".to_string(), Arc::clone(&sess));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));

        let (sub_server, sub_client) = UnixStream::pair().unwrap();
        let sessions_b = Arc::clone(&sessions);
        let acp_sessions_b = new_acp_sessions();
        let subscribers_b = Arc::clone(&subscribers);
        thread::spawn(move || {
            handle_subscribe(sub_server, &sessions_b, &acp_sessions_b, &subscribers_b);
        });

        let mut br = BufReader::new(sub_client.try_clone().unwrap());
        let mut line = String::new();
        br.read_line(&mut line).unwrap();
        let first: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(first["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(first["sessions"][0]["id"], "sub-test");

        let snapshot = {
            let mut st = sess.state.lock().unwrap();
            st.phase = Phase::AwaitingApproval;
            st.pending_question = Some("要不要继续".to_string());
            st.clone()
        };
        broadcast_state(&subscribers, &snapshot);

        let mut line2 = String::new();
        br.read_line(&mut line2).unwrap();
        let second: serde_json::Value = serde_json::from_str(&line2).unwrap();
        assert_eq!(second["session"]["phase"], "awaiting_approval");
        assert_eq!(second["session"]["pending_question"], "要不要继续");
    }

    /// `state` op 与 `subscribe` 并发时不得 ABBA 死锁——两边的锁序必须一致。
    ///
    /// 这两条路径在真实环境里天天并发：`state` 是 Claude hooks 每次状态变化都在打的，
    /// `subscribe` 是 GUI 状态通道常驻的。一旦成环，`sessions` 会被永久锁死，
    /// `open`/`list`/`kill`/`version`/`upgrade` 全部卡住——PTY 还活着，但守护废了，
    /// 用户只能 pkill，**正在跑的 agent 会话全灭**。CLIENT_WRITE_TIMEOUT 救不了：
    /// 卡在锁获取上，不是卡在 write。
    ///
    /// 易错点（本 bug 的成因）：`if let Some(x) = m.lock().unwrap().get(..).cloned() { .. }`
    /// 里那把 guard **活到整个 body 结束**（两个 edition 都如此，Rust 2024 的
    /// if-let rescope 只改 else 分支）。旁边的 action/input/resize 用 let-else，
    /// guard 在语句末即释放，所以只有 state 这一条路径会成环。
    #[test]
    fn state_op_and_subscribe_do_not_deadlock() {
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let sess = make_dummy_session(24, 80);
        sess.state.lock().unwrap().id = "dl".to_string();
        sessions
            .lock()
            .unwrap()
            .insert("dl".to_string(), Arc::clone(&sess));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));

        const ROUNDS: usize = 300;
        let (done_tx, done_rx) = std::sync::mpsc::channel::<&'static str>();

        // A：反复走 state op（真 handle_conn 分发）
        {
            let sessions = Arc::clone(&sessions);
            let subscribers = Arc::clone(&subscribers);
            let tx = done_tx.clone();
            thread::spawn(move || {
                for _ in 0..ROUNDS {
                    let (server, client) = UnixStream::pair().unwrap();
                    let mut client = client;
                    writeln!(
                        client,
                        "{}",
                        serde_json::json!({ "op": "state", "id": "dl", "phase": "thinking" })
                    )
                    .unwrap();
                    handle_conn(
                        server,
                        Arc::clone(&sessions),
                        new_acp_sessions(),
                        tasks::new_task_state(),
                        0,
                        -1,
                        new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string())),
                        Arc::new(Mutex::new(None)),
                        Arc::new(Mutex::new(HashMap::new())),
                        Arc::clone(&subscribers),
                    );
                }
                let _ = tx.send("state");
            });
        }

        // B：反复走 subscribe（持 subscribers 求 sessions，与 A 反向）。
        // handle_subscribe 注册完会阻塞在 read 上等客户端断开（长连接，同 handle_watch），
        // 所以必须另起线程跑它、由本线程 drop 掉 client 放它走——直接调用会把本线程
        // 当场焊死在 read 上（这里踩过一次，卡住的是测试自己，不是产品）。
        {
            let sessions = Arc::clone(&sessions);
            let subscribers = Arc::clone(&subscribers);
            let tx = done_tx.clone();
            thread::spawn(move || {
                for _ in 0..ROUNDS {
                    let (server, client) = UnixStream::pair().unwrap();
                    let s = Arc::clone(&sessions);
                    let acp_s = new_acp_sessions();
                    let sub = Arc::clone(&subscribers);
                    let h = thread::spawn(move || handle_subscribe(server, &s, &acp_s, &sub));
                    // 立刻断开：read 拿到 EOF 就收尾退出。要抓的锁序（持 subscribers
                    // → 求 sessions）在注册阶段、早于 read，此时已经跑过了。
                    drop(client);
                    let _ = h.join();
                }
                let _ = tx.send("subscribe");
            });
        }
        drop(done_tx);

        for _ in 0..2 {
            if done_rx
                .recv_timeout(std::time::Duration::from_secs(20))
                .is_err()
            {
                panic!(
                    "state op 与 subscribe 并发死锁：20 秒内未跑完 {ROUNDS} 轮。\
                     state 持 sessions 求 subscribers，subscribe 持 subscribers 求 sessions"
                );
            }
        }
        assert!(
            sess.state.lock().unwrap().structured_events,
            "收到 state hook 后必须锁定结构化事件链路"
        );
    }
}

/// ACP 会话托管：不 spawn 真实 agent 子进程（测试环境里也没有已登录的
/// claude/codex 可用），直接构造 `AcpSession`/`AcpSessionState` 驱动被测函数，
/// 只测「smeltd 这一层的管子接对了没」——归约本身（entries 合并/phase 机/
/// 回声去重等）已经在 smelt_core::acp_session 的单测里覆盖过，这里不重复。
#[cfg(test)]
mod acp_tests {
    use super::*;
    use smelt_core::acp_chat::{AcpEntry, ToolCallStatus, ToolKind};
    use smelt_core::acp_session::{AcpPhase, AcpSessionState};

    fn make_acp_session_value(id: &str, reduced: AcpSessionState) -> AcpSession {
        AcpSession {
            reduced: Mutex::new(reduced),
            snapshot_revision: AtomicU64::new(0),
            handle: Mutex::new(None),
            cwd: None,
            agent_needs_transcript_check: true,
            state: Arc::new(Mutex::new(SessionState {
                id: id.to_string(),
                ..Default::default()
            })),
            out: Mutex::new(AcpOut {
                client: None,
                watchers: Vec::new(),
            }),
            launch_spec: Mutex::new(None),
        }
    }

    fn make_acp_session(id: &str, reduced: AcpSessionState) -> Arc<AcpSession> {
        Arc::new(make_acp_session_value(id, reduced))
    }

    #[test]
    fn hot_attach_never_relaunches_or_replays_agent_history() {
        assert!(!acp_open_needs_relaunch(false, true, true));
        assert!(!acp_open_needs_relaunch(false, true, false));
        assert!(acp_open_needs_relaunch(true, false, true));
        assert!(acp_open_needs_relaunch(false, false, true));
    }

    #[test]
    fn reopening_live_session_returns_requested_tail_without_relaunch() {
        let mut reduced = AcpSessionState::default();
        for index in 0..5 {
            reduced
                .entries
                .push(AcpEntry::User(format!("message-{index}")));
        }
        reduced.phase = AcpPhase::Idle;

        let acp_sessions = new_acp_sessions();
        let (slot, _) =
            acp_sessions.reserve_with("acp-live", || make_acp_session_value("acp-live", reduced));
        let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::AcpHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(None)),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });

        let (server, client) = UnixStream::pair().unwrap();
        let reader = BufReader::new(server.try_clone().unwrap());
        let sessions_for_open = Arc::clone(&acp_sessions);
        let worker = thread::spawn(move || {
            handle_acp_open(
                server,
                reader,
                &serde_json::json!({
                    "id": "acp-live",
                    "cmd": "/must/not/be/launched",
                    "tail_limit": 2
                }),
                sessions_for_open,
                Arc::new(Mutex::new(Vec::new())),
            );
        });

        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["snapshot"]["entries_offset"], 3);
        assert_eq!(response["snapshot"]["entries_total"], 5);
        assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 2);
        assert!(slot.value.handle.lock().unwrap().is_some());

        drop(reader);
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn one_shot_action_keeps_existing_control_client_attached() {
        let acp_sessions = new_acp_sessions();
        let (slot, _) = acp_sessions.reserve_with("acp-action", || {
            make_acp_session_value("acp-action", AcpSessionState::default())
        });
        let (cmd_tx, cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::AcpHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(None)),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });

        let (control_server, _control_client) = UnixStream::pair().unwrap();
        let control_fd = control_server.as_raw_fd();
        slot.value.out.lock().unwrap().client = Some(control_server);

        let (action_server, action_client) = UnixStream::pair().unwrap();
        handle_acp_action(
            action_server,
            &serde_json::json!({
                "id": "acp-action",
                "action": {"Prompt": {"text": "from mobile", "images": []}},
            }),
            &acp_sessions,
            &Arc::new(Mutex::new(Vec::new())),
        );

        let mut response = String::new();
        BufReader::new(action_client)
            .read_line(&mut response)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["ok"],
            true
        );
        assert_eq!(
            slot.value
                .out
                .lock()
                .unwrap()
                .client
                .as_ref()
                .map(|client| client.as_raw_fd()),
            Some(control_fd),
            "one-shot action must not replace the PC control client"
        );
        match cmd_rx.try_recv().unwrap() {
            smelt_core::acp_conn::AcpCommand::Prompt { text, images } => {
                assert_eq!(text, "from mobile");
                assert!(images.is_empty());
            }
            _ => panic!("expected prompt command"),
        }
        assert!(matches!(
            slot.value.reduced.lock().unwrap().entries.last(),
            Some(AcpEntry::User(text)) if text == "from mobile"
        ));
    }

    #[test]
    fn one_shot_action_rejects_invalid_prompt_shape() {
        let acp_sessions = new_acp_sessions();
        acp_sessions.reserve_with("acp-action", || {
            make_acp_session_value("acp-action", AcpSessionState::default())
        });
        let (server, client) = UnixStream::pair().unwrap();

        handle_acp_action(
            server,
            &serde_json::json!({
                "id": "acp-action",
                "action": {"Prompt": {"content": "wrong field", "images": []}},
            }),
            &acp_sessions,
            &Arc::new(Mutex::new(Vec::new())),
        );

        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"], "invalid ACP action");
    }

    #[test]
    fn watch_on_unknown_session_just_disconnects() {
        let acp_sessions = new_acp_sessions();
        let (server, client) = UnixStream::pair().unwrap();
        let reader = BufReader::new(server.try_clone().unwrap());
        handle_acp_watch(
            server,
            reader,
            &serde_json::json!({"id": "acp-nope"}),
            acp_sessions,
        );
        // 没有会话可接：函数直接 return，客户端读到 EOF（不是某行 JSON）。
        let mut buf = Vec::new();
        BufReader::new(client).read_to_end(&mut buf).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn watch_delivers_initial_snapshot_matching_to_snapshot() {
        let mut reduced = AcpSessionState::default();
        reduced.entries.push(AcpEntry::User("hi".into()));
        reduced.phase = AcpPhase::Idle;
        let expected = reduced.to_snapshot(false);

        let acp_sessions = new_acp_sessions();
        acp_sessions.reserve_with("acp-1", || make_acp_session_value("acp-1", reduced));

        let (server, client) = UnixStream::pair().unwrap();
        let reader = BufReader::new(server.try_clone().unwrap());
        let h = thread::spawn(move || {
            handle_acp_watch(
                server,
                reader,
                &serde_json::json!({"id": "acp-1"}),
                acp_sessions,
            );
        });

        let mut br = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        br.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["snapshot"]["entries"].as_array().unwrap().len(), 1);
        assert_eq!(
            serde_json::to_value(&expected).unwrap()["entries"],
            v["snapshot"]["entries"]
        );

        // 全部克隆（`br` 内部那份 + 这个原始 `client`）都要丢，socket 才会真正
        // 关闭产生 EOF——只 drop 一份，另一份还开着，对端读不到 EOF 会一直卡住
        // （这个坑踩过一次，见 watch_tests 里同款收尾写法）。
        drop(br);
        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn bounded_acp_snapshot_reads_only_the_requested_older_page() {
        let mut reduced = AcpSessionState::default();
        for index in 0..5 {
            reduced
                .entries
                .push(AcpEntry::User(format!("message-{index}")));
        }
        let acp_sessions = new_acp_sessions();
        acp_sessions.reserve_with("acp-history", || {
            make_acp_session_value("acp-history", reduced)
        });

        let (server, client) = UnixStream::pair().unwrap();
        handle_acp_snapshot(
            server,
            &serde_json::json!({
                "id": "acp-history",
                "before": 3,
                "limit": 2,
            }),
            &acp_sessions,
        );

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["snapshot"]["entries_offset"], 1);
        assert_eq!(response["snapshot"]["entries_total"], 5);
        assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn push_snapshot_reaches_control_client_and_watchers_and_drops_dead_ones() {
        let sess = make_acp_session("acp-2", AcpSessionState::default());

        let (c_server, c_client) = UnixStream::pair().unwrap();
        let (w_server, w_client) = UnixStream::pair().unwrap();
        {
            let mut out = sess.out.lock().unwrap();
            out.client = Some(c_server);
            out.watchers.push(w_server);
        }
        drop(c_client); // 控制连接对端已经断了：推送应该发现写失败并自己摘掉

        push_acp_snapshot(&sess, false);

        assert!(
            sess.out.lock().unwrap().client.is_none(),
            "写失败的 client 该被摘掉"
        );
        assert_eq!(
            sess.out.lock().unwrap().watchers.len(),
            1,
            "还活着的 watcher 不该被牵连摘掉"
        );

        let mut line = String::new();
        BufReader::new(w_client).read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("snapshot").is_some());
    }

    #[test]
    fn parallel_tool_completion_replaces_snapshot_from_the_changed_card() {
        let mut reduced = AcpSessionState::default();
        for id in ["tool-a", "tool-b"] {
            reduced.entries.push(AcpEntry::ToolCall {
                id: id.into(),
                title: id.into(),
                kind: ToolKind::Execute,
                status: ToolCallStatus::InProgress,
                output: Vec::new(),
            });
        }
        let sess = make_acp_session("acp-parallel", reduced);
        let (server, client) = UnixStream::pair().unwrap();
        sess.out.lock().unwrap().client = Some(server);

        let outcome = {
            let mut state = sess.reduced.lock().unwrap();
            smelt_core::acp_session::apply_event(
                &mut state,
                smelt_core::acp_conn::AcpEvent::ToolFinished {
                    id: "tool-a".into(),
                    status: ToolCallStatus::Completed,
                    output: Vec::new(),
                },
            )
        };
        push_acp_snapshot_since(&sess, false, outcome.entries_offset);

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["snapshot"]["entries_offset"], 0);
        assert_eq!(response["snapshot"]["entries"].as_array().unwrap().len(), 2);
        assert_eq!(
            response["snapshot"]["entries"][0]["ToolCall"]["status"],
            "completed"
        );
        assert_eq!(
            response["snapshot"]["entries"][1]["ToolCall"]["status"],
            "in_progress"
        );
    }

    #[test]
    fn kill_removes_session_and_closes_connections() {
        let acp_sessions = new_acp_sessions();
        let (slot, _) = acp_sessions.reserve_with("acp-3", || {
            make_acp_session_value("acp-3", AcpSessionState::default())
        });

        let (c_server, c_client) = UnixStream::pair().unwrap();
        slot.value.out.lock().unwrap().client = Some(c_server);

        let (server, client) = UnixStream::pair().unwrap();
        handle_acp_kill(server, &serde_json::json!({"id": "acp-3"}), &acp_sessions);

        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["ok"], true);
        assert!(acp_sessions.get("acp-3").is_none());

        // 控制连接该被强制关掉：对端读到 EOF。
        let mut buf = Vec::new();
        BufReader::new(c_client).read_to_end(&mut buf).unwrap();
        assert!(buf.is_empty());
    }

    /// kill 一个不存在的 id：跟终端 `kill` 一样静默回 ok，不报错。
    #[test]
    fn kill_unknown_session_is_a_harmless_no_op() {
        let acp_sessions = new_acp_sessions();
        let (server, client) = UnixStream::pair().unwrap();
        handle_acp_kill(
            server,
            &serde_json::json!({"id": "acp-ghost"}),
            &acp_sessions,
        );
        let mut resp = String::new();
        BufReader::new(client).read_line(&mut resp).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&resp).unwrap()["ok"],
            true
        );
    }

    /// 帮手：走一遍 `handle_acp_open` 的完整流程（跟真实客户端一样连接→读首行
    /// 快照→断开），cmd 用一个必然不存在的路径——`spawn_acp` 保证不阻塞调用方
    /// （见文件头职责边界），子进程起不来只会异步产出 `AcpEvent::Fatal`，不
    /// 影响这里要测的「登记进表」这件事。
    fn open_acp_session_once(
        id: &str,
        acp_sessions: &AcpSessions,
        subscribers: &Subscribers,
    ) -> Arc<acp_registry::AcpSlot<AcpSession>> {
        let (server, client) = UnixStream::pair().unwrap();
        let reader = BufReader::new(server.try_clone().unwrap());
        let acp_sessions2 = Arc::clone(acp_sessions);
        let subscribers2 = Arc::clone(subscribers);
        let id_owned = id.to_string();
        let h = thread::spawn(move || {
            handle_acp_open(
                server,
                reader,
                &serde_json::json!({"id": id_owned, "cmd": "/definitely/not/a/real/binary-xyz"}),
                acp_sessions2,
                subscribers2,
            );
        });
        let mut br = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        br.read_line(&mut line).unwrap(); // 读到首行快照，说明 acp_spawn 已经跑完
        drop(br);
        drop(client); // 两份 clone 都要丢，读循环那头才会真正见到 EOF 退出
        h.join().unwrap();
        acp_sessions
            .get(id)
            .expect("open 后 registry 中应保留该 slot")
    }

    #[test]
    fn concurrent_open_same_id_keeps_one_registry_slot() {
        let acp_sessions: AcpSessions =
            Arc::new(acp_registry::AcpRegistry::new(Arc::new(RwLock::new(()))));
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let barrier = Arc::new(std::sync::Barrier::new(8));

        let slots = thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..8 {
                let acp_sessions = Arc::clone(&acp_sessions);
                let subscribers = Arc::clone(&subscribers);
                let barrier = Arc::clone(&barrier);
                workers.push(scope.spawn(move || {
                    barrier.wait();
                    open_acp_session_once("acp-race", &acp_sessions, &subscribers)
                }));
            }
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert_eq!(acp_sessions.snapshot().len(), 1);
        assert!(
            slots.windows(2).all(|pair| Arc::ptr_eq(&pair[0], &pair[1])),
            "并发 open 必须拿到同一个稳定 slot"
        );
    }

    #[test]
    fn kill_does_not_remove_a_concurrently_installed_replacement() {
        let acp_sessions: AcpSessions =
            Arc::new(acp_registry::AcpRegistry::new(Arc::new(RwLock::new(()))));
        let (old, _) = acp_sessions.reserve_with("acp-race", || {
            make_acp_session_value("acp-race", AcpSessionState::default())
        });
        let lifecycle = old.lifecycle.lock().unwrap();
        let registry_for_kill = Arc::clone(&acp_sessions);
        let (server, client) = UnixStream::pair().unwrap();
        let killer = thread::spawn(move || {
            handle_acp_kill(
                server,
                &serde_json::json!({"id": "acp-race"}),
                &registry_for_kill,
            );
        });

        for _ in 0..100 {
            if acp_sessions.get("acp-race").is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            acp_sessions.get("acp-race").is_none(),
            "kill 应先摘掉它实际取得的 slot，再等待 lifecycle 收尾"
        );
        let (replacement, created) = acp_sessions.reserve_with("acp-race", || {
            make_acp_session_value("acp-race", AcpSessionState::default())
        });
        assert!(created);

        drop(lifecycle);
        killer.join().unwrap();
        let mut response = String::new();
        BufReader::new(client)
            .read_line(&mut response)
            .expect("kill response");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["ok"],
            true
        );
        assert!(Arc::ptr_eq(
            &acp_sessions.get("acp-race").unwrap(),
            &replacement
        ));
    }

    #[test]
    fn open_retries_when_kill_removes_its_reserved_slot() {
        let acp_sessions = new_acp_sessions();
        let (old, _) = acp_sessions.reserve_with("acp-open-kill", || {
            make_acp_session_value("acp-open-kill", AcpSessionState::default())
        });
        let lifecycle = old.lifecycle.lock().unwrap();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));

        let (open_server, open_client) = UnixStream::pair().unwrap();
        let open_reader = BufReader::new(open_server.try_clone().unwrap());
        let registry_for_open = Arc::clone(&acp_sessions);
        let open_worker = thread::spawn(move || {
            handle_acp_open(
                open_server,
                open_reader,
                &serde_json::json!({
                    "id": "acp-open-kill",
                    "cmd": "/definitely/not/a/real/binary-open-kill"
                }),
                registry_for_open,
                subscribers,
            );
        });

        for _ in 0..100 {
            if Arc::strong_count(&old) >= 3 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Arc::strong_count(&old) >= 3,
            "open 应已 reserve 旧 slot 并等待 lifecycle"
        );

        let (kill_server, kill_client) = UnixStream::pair().unwrap();
        let registry_for_kill = Arc::clone(&acp_sessions);
        let kill_worker = thread::spawn(move || {
            handle_acp_kill(
                kill_server,
                &serde_json::json!({"id": "acp-open-kill"}),
                &registry_for_kill,
            );
        });
        for _ in 0..100 {
            if acp_sessions.get("acp-open-kill").is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(acp_sessions.get("acp-open-kill").is_none());

        drop(lifecycle);

        let mut open_line = String::new();
        let mut open_reader = BufReader::new(open_client.try_clone().unwrap());
        open_reader.read_line(&mut open_line).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&open_line)
                .unwrap()
                .get("snapshot")
                .is_some()
        );
        let current = acp_sessions
            .get("acp-open-kill")
            .expect("open 必须改用 kill 之后的 replacement slot");
        assert!(!Arc::ptr_eq(&current, &old));

        drop(open_reader);
        drop(open_client);
        open_worker.join().unwrap();
        kill_worker.join().unwrap();
        let mut kill_response = String::new();
        BufReader::new(kill_client)
            .read_line(&mut kill_response)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&kill_response).unwrap()["ok"],
            true
        );
    }

    /// 回归 code review 发现的高严重度 bug：`acp_spawn` 建了新会话却从没插进
    /// `acp_sessions` 表，导致 watch/list/kill 都找不到它，`handle_upgrade`
    /// 收集 fd 时也会漏掉它——无缝升级直接把这条会话弄丢。
    #[test]
    fn open_new_session_registers_it_in_acp_sessions_table() {
        let acp_sessions = new_acp_sessions();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));

        open_acp_session_once("acp-new", &acp_sessions, &subscribers);

        assert!(
            acp_sessions.get("acp-new").is_some(),
            "新建会话必须登记进 acp_sessions，不然 watch/list/kill 和无缝升级的 fd 收集都找不到它"
        );
    }

    /// 同一个 bug 的另一面：表里没有它，`handle_acp_open` 的「已存在就复用」
    /// 分支永远命中不了 —— 同一个 id 重开一次就会再走一遍 `acp_spawn`，多起
    /// 一个 agent 子进程，旧的那个泄漏在后台再也够不着。
    #[test]
    fn reopening_same_id_reuses_existing_session_instead_of_spawning_a_duplicate() {
        let acp_sessions = new_acp_sessions();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));

        open_acp_session_once("acp-dup", &acp_sessions, &subscribers);
        let first = acp_sessions.get("acp-dup").expect("首次打开该已登记");

        open_acp_session_once("acp-dup", &acp_sessions, &subscribers);
        let second = acp_sessions.get("acp-dup").expect("重开该还在表里");

        assert_eq!(
            acp_sessions.snapshot().len(),
            1,
            "同一个 id 重开不该在表里多出一条"
        );
        assert!(
            Arc::ptr_eq(&first, &second),
            "重开应该复用已登记的会话，不能是 acp_spawn 又建了一个新对象（否则旧 agent 进程/线程直接泄漏）"
        );
    }

    #[test]
    fn open_then_handoff_keeps_one_stable_registry_slot() {
        let acp_sessions = new_acp_sessions();
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let (stdin_fd_owner, _stdin_peer) = UnixStream::pair().unwrap();
        let (stdout_fd_owner, _stdout_peer) = UnixStream::pair().unwrap();
        let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        let (slot, created) = acp_sessions.reserve_with("acp-handoff", || {
            let sess = make_acp_session_value("acp-handoff", AcpSessionState::default());
            *sess.handle.lock().unwrap() = Some(smelt_core::acp_conn::AcpHandle {
                cmd_tx,
                event_rx,
                stdio: Arc::new(Mutex::new(Some(smelt_core::acp_conn::AcpStdio {
                    pid: std::process::id() as i32,
                    stdin_fd: stdin_fd_owner.as_raw_fd(),
                    stdout_fd: stdout_fd_owner.as_raw_fd(),
                }))),
                in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            });
            sess
        });
        assert!(created);

        let opened = open_acp_session_once("acp-handoff", &acp_sessions, &subscribers);
        assert!(Arc::ptr_eq(&opened, &slot));

        let (items, fds) = collect_acp_handoff(&acp_sessions);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "acp-handoff");
        assert_eq!(
            fds,
            vec![stdin_fd_owner.as_raw_fd(), stdout_fd_owner.as_raw_fd()]
        );
        assert_eq!(acp_sessions.snapshot().len(), 1);
        assert!(Arc::ptr_eq(
            &acp_sessions.get("acp-handoff").unwrap(),
            &slot
        ));
    }

    #[test]
    fn upgrade_barrier_requires_quiescent_phase_and_no_outstanding_rpc() {
        let acp_sessions = new_acp_sessions();

        let mut idle = AcpSessionState::default();
        idle.phase = AcpPhase::Idle;
        let (idle_slot, _) = acp_sessions.reserve_with("acp-idle", || {
            make_acp_session_value("acp-idle", idle)
        });
        let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *idle_slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::AcpHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(None)),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        assert!(acp_upgrade_blockers(&acp_sessions).is_empty());

        idle_slot.value.reduced.lock().unwrap().phase = AcpPhase::Running;
        assert_eq!(acp_upgrade_blockers(&acp_sessions), vec!["acp-idle"]);

        idle_slot.value.reduced.lock().unwrap().phase = AcpPhase::Idle;
        idle_slot
            .value
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .in_flight_rpc
            .store(1, Ordering::SeqCst);
        assert_eq!(acp_upgrade_blockers(&acp_sessions), vec!["acp-idle"]);
    }

    #[test]
    fn upgrade_returns_busy_before_touching_handoff_for_active_acp_turn() {
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let acp_sessions = new_acp_sessions();
        let mut running = AcpSessionState::default();
        running.phase = AcpPhase::Running;
        acp_sessions.reserve_with("acp-running", || {
            make_acp_session_value("acp-running", running)
        });

        let (server, client) = UnixStream::pair().unwrap();
        handle_upgrade(
            server,
            &serde_json::json!({"op": "upgrade"}),
            &sessions,
            &acp_sessions,
            -1,
        );

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["ok"], false);
        assert_eq!(response["busy"], true);
        assert_eq!(response["sessions"], serde_json::json!(["acp-running"]));
    }

    #[test]
    fn subscribe_snapshot_merges_terminal_and_acp_sessions() {
        let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
        let term_sess = watch_tests::make_dummy_session(24, 80);
        term_sess.state.lock().unwrap().id = "term-1".to_string();
        sessions
            .lock()
            .unwrap()
            .insert("term-1".to_string(), term_sess);

        let acp_sessions = new_acp_sessions();
        acp_sessions.reserve_with("acp-1", || {
            make_acp_session_value("acp-1", AcpSessionState::default())
        });

        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let (server, client) = UnixStream::pair().unwrap();
        let acp_sessions2 = Arc::clone(&acp_sessions);
        let h = thread::spawn(move || {
            handle_subscribe(server, &sessions, &acp_sessions2, &subscribers)
        });

        let mut line = String::new();
        BufReader::new(client.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let ids: Vec<&str> = v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"term-1"));
        assert!(ids.contains(&"acp-1"));

        drop(client);
        h.join().unwrap();
    }

    #[test]
    fn daemon_phase_distinguishes_executing_tool_from_thinking() {
        let mut running_with_tool = AcpSessionState::default();
        running_with_tool.phase = AcpPhase::Running;
        running_with_tool.entries.push(AcpEntry::ToolCall {
            id: "t1".into(),
            title: "Read".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
        });
        assert_eq!(
            compute_acp_daemon_phase(&running_with_tool),
            Phase::ExecutingTool
        );

        let mut running_no_tool = AcpSessionState::default();
        running_no_tool.phase = AcpPhase::Running;
        assert_eq!(compute_acp_daemon_phase(&running_no_tool), Phase::Thinking);

        let mut ended = AcpSessionState::default();
        ended.phase = AcpPhase::Ended("boom".into());
        assert_eq!(compute_acp_daemon_phase(&ended), Phase::Dead);
    }

    #[test]
    fn pending_question_prefers_permission_over_elicitation() {
        use smelt_core::acp_session::LivePermission;

        let mut s = AcpSessionState::default();
        assert_eq!(acp_pending_question(&s), None);

        s.permissions.push(LivePermission {
            question: "要不要覆盖这个文件？".into(),
            tool_call_id: "t1".into(),
            options: Vec::new(),
            details: smelt_core::acp_session::ApprovalDetailsView::Generic,
            responder: None,
            raw_request_line: None,
        });
        assert_eq!(
            acp_pending_question(&s).as_deref(),
            Some("要不要覆盖这个文件？")
        );
    }

    #[test]
    fn acp_open_request_prefers_structured_launch() {
        let req = parse_acp_open_request(&serde_json::json!({
            "id": "acp-1",
            "cwd": "/repo",
            "launch": {
                "command": "claude --print",
                "env": {
                    "CLAUDE_CONFIG_DIR": "~/Claude Workspaces/quant"
                }
            },
            "agent": "claude",
            "resume_id": "resume-1"
        }))
        .unwrap();

        assert_eq!(req.id, "acp-1");
        assert_eq!(req.launch.command, "claude --print");
        assert_eq!(
            req.launch.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Claude Workspaces/quant")
        );
        assert_eq!(req.resume_id.as_deref(), Some("resume-1"));
    }

    #[test]
    fn acp_open_request_legacy_cmd_becomes_launch_spec() {
        let req = parse_acp_open_request(&serde_json::json!({
            "id": "acp-legacy",
            "cmd": "claude --dangerously-skip-permissions",
            "agent": "claude"
        }))
        .unwrap();

        assert_eq!(req.launch.command, "claude --dangerously-skip-permissions");
        assert!(req.launch.env.is_empty());
    }

    #[test]
    fn blank_acp_session_does_not_reuse_runtime_id_as_history() {
        let mut reduced = smelt_core::acp_session::AcpSessionState::default();
        reduced.acp_session_id = Some("runtime-only".into());

        assert_eq!(known_acp_resume_id(&reduced), None);
    }

    #[test]
    fn acp_session_with_entries_can_fallback_to_runtime_history_id() {
        let mut reduced = smelt_core::acp_session::AcpSessionState::default();
        reduced.acp_session_id = Some("runtime-history".into());
        reduced
            .entries
            .push(smelt_core::acp_chat::AcpEntry::User("hello".into()));

        assert_eq!(
            known_acp_resume_id(&reduced).as_deref(),
            Some("runtime-history")
        );
    }

    #[test]
    fn requested_history_id_wins_when_dead_session_is_relaunched() {
        assert_eq!(
            select_resume_id(
                Some("saved-history".to_string()),
                Some("daemon-runtime".to_string())
            ),
            Some("saved-history".to_string())
        );
        assert_eq!(
            select_resume_id(None, Some("known-history".to_string())),
            Some("known-history".to_string())
        );
    }
}
