//! 交接 v2 predecessor（老进程）：快照→typed manifest→spawn successor→
//! 传 manifest/fd/grid→READY→COMMIT。COMMIT 发出前任何失败都回滚、原地继续服务。
//!
//! 快照锁序与旧版逐行一致（state 克隆→output_gate→input_gate→ctl→term→out→
//! child），guards 由 [`with_snapshot`] 的栈帧持有至事务结束——生周期跨不过
//! 函数边界是借用规则决定的，不绕（自引用结构是 UB 温床）。
//!
//! 相对 exec-self 删除的东西：CLOEXEC 手术（`SCM_RIGHTS` 直接 dup 描述符，
//! 发送方原件保持 CLOEXEC 全程不动）、交接文件、二次 exec。
//!
//! 保留的拒绝项（Bus you can't drain）：peer_messaging 在途投递与
//! `acp_upgrade_blockers`——hosted 会话永不进 blockers，只有 direct-fd
//! 遗留连接 mid-turn 才拦（SDK future 穿不过进程边界）；它们经
//! legacy_rehome 自迁移到 hosted，有界收敛。Busy 只拦升级、不拦安装
//! （Phase 1 已解耦），故此处保留拒绝是安全的。

use super::manifest::{
    FdRole, GridRef, HandoffManifest, ProducerInfo, TerminalChildHandoff, TerminalHandoff,
    sha256_hex, validate,
};
use super::transport::{self, ReadyInfo};
use alacritty_terminal::term::TermMode;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::MutexGuard;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

/// 已 stage 的交接载荷：fd_roles/fds/grids 三组同序一一对应。
/// fds 是发送方原件号（保持 CLOEXEC），`SCM_RIGHTS` 发 dup 份。
pub(crate) struct StagedHandoff {
    pub manifest: HandoffManifest,
    pub fds: Vec<RawFd>,
    pub grid_blobs: Vec<Vec<u8>>,
}

/// [`with_snapshot`] 交给闭包的快照：载荷 + 跨事务持有的 guards。
/// guards 字段只为持有而存在，drop 即恢复服务（回滚）/随进程退出（提交）。
pub(crate) struct StagedSnapshot<'a> {
    pub staged: StagedHandoff,
    _output_guards: Vec<MutexGuard<'a, ()>>,
    _input_guards: Vec<MutexGuard<'a, ()>>,
    _out_guards: Vec<MutexGuard<'a, crate::Out>>,
    _child_guards: Vec<MutexGuard<'a, crate::TerminalChildState>>,
}

pub(crate) enum TransactionOutcome {
    /// 已 COMMIT：调用方回 {ok:true} 后 exit(0)，successor 正在服务。
    /// sidecars 已停，EventHub 已刷。
    Committed,
    /// 已回滚：老进程原地继续服务。reason 进回执 + dlog。
    RolledBack { reason: String },
}

/// 快照侧子进程映射（纯函数，可单测）：Running→Live（收养监控）；
/// Finished（含 WaitFailed）→Exited（僵尸重定父后 launchd 会收，pump 读
/// EOF 收尾，都不需要 successor 观察）；pid<=1→跳过（与旧版"关 fd 跳过"同结局）。
pub(crate) fn terminal_child_handoff(
    state: &crate::TerminalChildState,
    pid: i32,
) -> Option<TerminalChildHandoff> {
    if pid <= 1 {
        return None;
    }
    match state {
        crate::TerminalChildState::Running => Some(TerminalChildHandoff::Live { pid }),
        crate::TerminalChildState::Finished(_) => {
            Some(TerminalChildHandoff::ExitedDuringHandoff { pid })
        }
    }
}

/// 快照（ACP 先、终端后，与旧版同序、无新锁嵌套）→ 组装 → 跑事务闭包。
/// legacy_rehome 语义沿用旧版（direct-fd 遗留迁移），与传输无关。
pub(crate) fn with_snapshot<R>(
    sessions: &crate::Sessions,
    acp_sessions: &crate::acp_host::AcpSessions,
    listen_fd: RawFd,
    f: impl FnOnce(StagedSnapshot<'_>) -> R,
) -> R {
    // roles[0]/fds[0] 恒为 listen（validate 强制首位）。
    let mut fd_roles = vec![FdRole::Listen];
    let mut fds: Vec<RawFd> = vec![listen_fd];
    let mut grid_blobs = Vec::new();

    // ACP 先（只收集、不持有锁），与旧版同序。
    let (acp_items, acp_fds) = crate::acp_host::collect_acp_handoff_typed(acp_sessions);

    // 终端：与旧版同一把锁序，guards 持有至事务结束（挡住泵/attach/resize/输入）。
    let session_list = sessions.snapshot();
    let mut output_guards = Vec::new();
    let mut input_guards = Vec::new();
    let mut out_guards = Vec::new();
    let mut child_guards = Vec::new();
    let mut items = Vec::new();
    let mut grids = Vec::new();
    for (id, sess) in &session_list {
        let state = sess.state.lock().unwrap().clone();
        let output_gate = sess.output_gate.lock().unwrap();
        let input_gate = sess.input_gate.lock().unwrap();
        let ctl = sess.ctl.lock().unwrap();
        let term = sess.term.lock().unwrap();
        let out = sess.out.lock().unwrap();
        // 必须排在 output/input/ctl/term/out 之后，和 kill/EOF 清理的锁序一致。
        let child_state = sess.child.lock_for_handoff();

        let pid = sess.child.pid();
        let Some(child) = terminal_child_handoff(&child_state, pid) else {
            // spawn 未成/无子进程：与旧版"关 fd 跳过"同结局（旧版在恢复侧
            // 跳，这里在快照侧跳——fd 根本不进交接，更干净）。
            crate::dlog(&format!("handoff: 跳过无子进程会话 id={id}"));
            continue;
        };
        // 空 grid 直接省略（恢复侧走"无 grid"路径），manifest 禁 len 0。
        let grid =
            crate::terminal_snapshot::snapshot_ansi_for_handoff(&term, state.launch.as_deref());
        if !grid.is_empty() {
            grids.push(GridRef {
                session_id: id.clone(),
                len: grid.len() as u64,
                sha256: sha256_hex(&grid),
            });
            grid_blobs.push(grid);
        }
        fd_roles.push(FdRole::TerminalMaster {
            session_id: id.clone(),
        });
        fds.push(ctl.master.as_raw_fd());
        items.push(TerminalHandoff {
            id: id.clone(),
            child,
            cols: ctl.cols,
            rows: ctl.rows,
            cwd: ctl.cwd.clone(),
            launch: state.launch.clone(),
            agent_mcp: state.agent_mcp,
            agent_token: state.agent_token.clone(),
            alt_screen: term.mode().contains(TermMode::ALT_SCREEN),
        });
        drop(term);
        drop(ctl);
        output_guards.push(output_gate);
        input_guards.push(input_gate);
        out_guards.push(out);
        child_guards.push(child_state);
    }

    for (role, fd) in &acp_fds {
        fd_roles.push(role.clone());
        fds.push(*fd);
    }

    #[cfg(target_os = "macos")]
    let menu_gui_pids = crate::menubar::child_pids_for_handoff();
    #[cfg(not(target_os = "macos"))]
    let menu_gui_pids = Vec::<i32>::new();

    let manifest = HandoffManifest {
        producer: ProducerInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
        },
        // 此时刻之后启动的同号 pid 必是复用——successor 清理时凭此验明正身。
        snapshot_wall_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        fd_roles,
        sessions: items,
        acp: acp_items,
        menu_gui_pids,
        grids,
    };
    f(StagedSnapshot {
        staged: StagedHandoff {
            manifest,
            fds,
            grid_blobs,
        },
        _output_guards: output_guards,
        _input_guards: input_guards,
        _out_guards: out_guards,
        _child_guards: child_guards,
    })
}

/// 跑一次交接事务。调用方须已持有 SPAWN_GATE 写锁（挡 spawn）并通过
/// peer/ACP 屏障。listen_fd 是 `fds[0]`（`roles[0]=Listen`，validate 保证）。
pub(crate) fn run_transaction(
    exe: &std::path::Path,
    staged: &StagedHandoff,
    event_hub: &crate::event_hub::EventHubHandle,
) -> TransactionOutcome {
    let rollback = |reason: String| {
        crate::dlog(&format!("handoff: 事务回滚，原地继续服务：{reason}"));
        TransactionOutcome::RolledBack { reason }
    };

    if let Err(error) = validate(&staged.manifest) {
        return rollback(format!("manifest 非法（构造 bug）：{error}"));
    }
    if staged.fds.len() != staged.manifest.fd_roles.len() {
        return rollback(format!(
            "fd 数量 {} 与角色数 {} 不一致（构造 bug）",
            staged.fds.len(),
            staged.manifest.fd_roles.len()
        ));
    }

    // 提交前 spawn 调用方给的映像路径。启动名先不动：拨早了，回滚之后下一次冷启动
    // 会跑到这次没提交成功的映像上。
    let target = exe.to_path_buf();
    let fingerprint = match smelt_core::daemon_image::content_fingerprint(&target) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return rollback(format!("计算候选守护指纹失败：{error}")),
    };

    let (ours, theirs) = match transport::socketpair() {
        Ok(pair) => pair,
        Err(error) => return rollback(format!("建 socketpair 失败：{error}")),
    };
    let mut child = match spawn_successor(&target, &fingerprint, &theirs) {
        Ok(child) => child,
        Err(error) => return rollback(format!("spawn successor 失败：{error}")),
    };
    drop(theirs); // predecessor 只留自己一端；不关会破坏 successor 的 EOF 判定。
    let abort_and_kill = |ours: &UnixStream, child: &mut std::process::Child, reason: String| {
        let _ = transport::send_abandon(ours);
        kill_successor(child);
        rollback(reason)
    };

    if let Err(error) = transport::send_manifest(&ours, &staged.manifest) {
        return abort_and_kill(&ours, &mut child, format!("发 manifest 失败：{error}"));
    }
    if let Err(error) = transport::send_fds(&ours, &staged.fds) {
        return abort_and_kill(&ours, &mut child, format!("发 fd 失败：{error}"));
    }
    if let Err(error) = transport::send_grids(&ours, &staged.manifest.grids, &staged.grid_blobs) {
        return abort_and_kill(&ours, &mut child, format!("发 grid 失败：{error}"));
    }
    let ready: ReadyInfo = match transport::await_ready(&ours) {
        Ok(ready) => ready,
        Err(error) => {
            return abort_and_kill(&ours, &mut child, format!("等 READY 失败：{error}"));
        }
    };
    crate::dlog(&format!(
        "handoff: successor READY（终端 {} + ACP {}，丢 grid {:?}），准备提交",
        ready.restored_terminals, ready.restored_acp, ready.dropped_grids
    ));

    // 提交前最后的可回滚点：EventHub 落盘。失败则 ABANDON（successor 退出），
    // sidecar 全程没动过，回滚即恢复。
    if let Err(error) = event_hub.flush_for_shutdown() {
        return abort_and_kill(&ours, &mut child, format!("EventHub flush 失败：{error}"));
    }
    if transport::send_commit(&ours).is_err() {
        // successor 死在 READY~COMMIT 的毫秒窗口：它没开始服务，回滚安全。
        // sidecars 还没停，无需重启——这就是 stop 放在 COMMIT 之后的原因。
        return abort_and_kill(
            &ours,
            &mut child,
            "发 COMMIT 失败（successor 已死）".to_string(),
        );
    }

    // ---- 不可逆段开始：successor 已在服务 ----
    // 现在才把启动名拨到 successor 正在映射的那份映像。旧进程钉住的是自己的文件，
    // 不靠这条链接拉会话宿主。拨失败只记日志：successor 已经在服务，不能回滚。
    if let Err(error) = smelt_core::daemon_image::retarget_launch_after_commit(exe) {
        crate::dlog(&format!("handoff: COMMIT 后改写启动名失败：{error}"));
    }

    // 先停 sidecars（让出端口），调用方随后回 ok + exit。自启若发现端口仍被占用且前任还是父进程，会等前任退出
    // 后再绑一次。此处失败也不回滚（已无可回滚之处），只留痕。
    //
    // 双 accept 窗口（COMMIT→exit 间两边同时 accept，新连接可能落到 dying
    // 进程）是刻意接受的：关 listen 叫不醒阻塞中的 accept（close 不唤醒、
    // shutdown 会伤及 successor 的 dup），而窗口只有毫秒、落错的连接吃 EOF
    // 后走客户端既有重连（终端 schedule_auto_reconnect + 升级后批量重连），
    // 不丢数据。为此改 accept 循环是过度设计。
    crate::remote::cleanup_sidecar_services();
    drop(ours);
    // successor 独立运行：我们 exit 后它重定父到 launchd。不 wait（它不会退出）。
    drop(child);
    TransactionOutcome::Committed
}

/// 回滚 tripwire：事务开始前读一次库版本。
pub(crate) fn store_schema_before() -> Option<u32> {
    let root = smelt_paths::smelt_home()?;
    smelt_store::Store::schema_version(&root.join(smelt_store::DATABASE_FILE_NAME))
}

/// successor 若已迁移 store（库版本变了），老进程不可再服务：
/// 旧二进制 + 新 schema 的组合只在 open 时被拒，运行中连接是静默错乱。
/// 此时唯一安全动作是 exit（结局=旧版最坏情况，会话丢但守护新；显式可观测）。
/// 判定逐臂解释：
/// - Some(a)→Some(b), a≠b：迁移了 → exit。
/// - 其余（含 None）：无信息/新建/被删 → resume。新建库对 store:None 运行的老
///   进程无害；被删是用户手贱，老连接握着旧 inode 照跑，语义≈崩溃恢复。
pub(crate) fn store_migrated_since(before: Option<u32>) -> bool {
    let root = match smelt_paths::smelt_home() {
        Some(root) => root,
        None => return false,
    };
    migrated(
        before,
        smelt_store::Store::schema_version(&root.join(smelt_store::DATABASE_FILE_NAME)),
    )
}

/// tripwire 纯规则（可单测），臂解释见 [`store_migrated_since`]。
fn migrated(before: Option<u32>, after: Option<u32>) -> bool {
    matches!((before, after), (Some(a), Some(b)) if a != b)
}

/// successor 可观测的 spawn 环境（纯函数，可单测）：交接 socket fd 号 +
/// 守护映像指纹。fd 号即 `--import-handoff` 模式信号（argv 保持无参，
/// 沿用 SMELTD_MENUBAR 的 env 风格）。
pub(crate) fn successor_env(sock_fd: RawFd, fingerprint: &str) -> Vec<(String, String)> {
    vec![
        ("SMELTD_HANDOFF_SOCK".to_string(), sock_fd.to_string()),
        (
            "SMELTD_DAEMON_FINGERPRINT".to_string(),
            fingerprint.to_string(),
        ),
    ]
}

fn spawn_successor(
    exe: &std::path::Path,
    fingerprint: &str,
    handoff_sock: &UnixStream,
) -> std::io::Result<std::process::Child> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    // Rust 建的 socket 自带 CLOEXEC；为 spawn 放行这一只。调用方持有
    // SPAWN_GATE 写锁，此窗口无其它 fork，不会误继承。
    crate::set_cloexec(handoff_sock.as_raw_fd(), false)?;
    let mut command = std::process::Command::new(exe);
    for (key, value) in successor_env(handoff_sock.as_raw_fd(), fingerprint) {
        command.env(key, value);
    }
    let result = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    // 无论 spawn 成败，恢复 CLOEXEC（失败时本端稍后随回滚 drop，但显式恢复
    // 更稳；成功时 predecessor 手里这份必须不再可继承）。
    let _ = crate::set_cloexec(handoff_sock.as_raw_fd(), true);
    result
}

/// 回滚杀 successor：先让唯一的 `waitpid` 就位，再 TERM，两秒后 KILL。
/// 已经退出的不再发信号，避免 pid 被系统拿去复用后打到别人。
/// 第二次截止仍未收回时，等的那条线程继续是这个 pid 唯一的 `waitpid`，
/// 这里不能再 `child.wait()`。
fn kill_successor(child: &mut std::process::Child) {
    match child.try_wait() {
        Ok(Some(_)) | Err(_) => return,
        Ok(None) => {}
    }
    let pid = child.id() as i32;
    let killed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&killed);
    let _ = smelt_core::process_wait::wait_for_pid(
        pid,
        Duration::from_secs(2),
        move || unsafe {
            libc::kill(pid, libc::SIGTERM);
        },
        move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        },
        Duration::from_secs(2),
    );
    if killed.load(std::sync::atomic::Ordering::SeqCst) {
        crate::dlog("handoff: successor TERM 超时，已 SIGKILL");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn successor_env_carries_sock_and_fingerprint() {
        let env: HashMap<String, String> = successor_env(17, "fp-abc").into_iter().collect();
        assert_eq!(env.get("SMELTD_HANDOFF_SOCK"), Some(&"17".to_string()));
        assert_eq!(
            env.get("SMELTD_DAEMON_FINGERPRINT"),
            Some(&"fp-abc".to_string())
        );
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn kill_successor_reaps_a_child_that_ignores_term() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap \"\" TERM; exec sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;
        let started = Instant::now();
        kill_successor(&mut child);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "TERM 两秒后应杀掉并收回，实际 {:?}",
            started.elapsed()
        );
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn commit_flips_the_launch_symlink_and_keeps_both_images() {
        use std::os::unix::fs::MetadataExt;
        let root = std::env::temp_dir().join(format!(
            "smeltd-promote-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let old_src = root.join("old-src");
        let new_src = root.join("new-src");
        std::fs::write(&old_src, b"running").unwrap();
        std::fs::write(&new_src, b"candidate").unwrap();
        let old_image = smelt_core::daemon_image::publish_image(&root, &old_src).unwrap();
        let new_image = smelt_core::daemon_image::publish_image(&root, &new_src).unwrap();
        let old_inode = std::fs::metadata(&old_image).unwrap().ino();
        smelt_core::daemon_image::retarget_launch_symlink(&root, &old_image).unwrap();

        let linked = smelt_core::daemon_image::retarget_launch_after_commit(&new_image).unwrap();

        assert_eq!(linked, new_image);
        assert_eq!(
            std::fs::read_link(root.join("smeltd")).unwrap(),
            new_image.file_name().unwrap()
        );
        assert_eq!(std::fs::read(&old_image).unwrap(), b"running");
        assert_eq!(std::fs::read(&new_image).unwrap(), b"candidate");
        assert_eq!(std::fs::metadata(&old_image).unwrap().ino(), old_inode);
        assert!(new_image.is_file());
        assert!(smelt_core::daemon_image::pending_image(&root).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn child_state_maps_to_handoff() {
        use super::super::manifest::TerminalChildHandoff;
        use crate::{TerminalChildExit, TerminalChildState};
        assert!(matches!(
            terminal_child_handoff(&TerminalChildState::Running, 100),
            Some(TerminalChildHandoff::Live { pid: 100 })
        ));
        for state in [
            TerminalChildState::Finished(TerminalChildExit::Reaped),
            TerminalChildState::Finished(TerminalChildExit::AlreadyReaped),
            TerminalChildState::Finished(TerminalChildExit::WaitFailed(None)),
            TerminalChildState::Finished(TerminalChildExit::AdoptedExited),
        ] {
            assert!(
                matches!(
                    terminal_child_handoff(&state, 100),
                    Some(TerminalChildHandoff::ExitedDuringHandoff { pid: 100 })
                ),
                "Finished 一律记 Exited（含 WaitFailed）"
            );
        }
        assert_eq!(
            terminal_child_handoff(&TerminalChildState::Running, 0),
            None
        );
        assert_eq!(
            terminal_child_handoff(&TerminalChildState::Running, -3),
            None
        );
    }

    #[test]
    fn migration_tripwire_only_fires_on_version_change() {
        assert!(migrated(Some(7), Some(8)));
        assert!(!migrated(Some(7), Some(7)));
        // 无信息/新建/被删：一律 resume（臂解释见 store_migrated_since）。
        assert!(!migrated(None, None));
        assert!(!migrated(None, Some(8)));
        assert!(!migrated(Some(7), None));
    }
}
