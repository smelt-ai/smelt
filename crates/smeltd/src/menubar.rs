//! macOS 菜单栏图标：打开 GUI / 退出守护。
//!
//! 从 `main.rs` 整块搬出。点「退出」会清 sidecar 再 `process::exit`，结束所有会话。

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use std::collections::HashSet;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

/// 应用图标母图，编进二进制当菜单栏图标（跟 workspace 用的是同一张）。
const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon-1024.png");

struct GuiReaper {
    sender: std::sync::mpsc::Sender<GuiChild>,
    pids: Arc<Mutex<HashSet<i32>>>,
}

enum GuiChild {
    Spawned(Child),
    Inherited(i32),
}

impl GuiChild {
    fn pid(&self) -> i32 {
        match self {
            Self::Spawned(child) => child.id() as i32,
            Self::Inherited(pid) => *pid,
        }
    }

    fn try_reap(&mut self) -> std::io::Result<bool> {
        try_reap_pid_with(self.pid(), |pid| {
            let waited = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
            if waited < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(waited)
            }
        })
    }
}

/// 对一个确切 PID 做非阻塞 wait。Spawned/Inherited 统一走这条路径，避免
/// `Child::try_wait` 在 Unix 上把 EINTR 原样抛出后，管理线程误以为 owner 已失效。
fn try_reap_pid_with(
    pid: i32,
    mut waitpid: impl FnMut(i32) -> std::io::Result<i32>,
) -> std::io::Result<bool> {
    loop {
        match waitpid(pid) {
            Ok(waited) if waited == pid => return Ok(true),
            Ok(0) => return Ok(false),
            Ok(waited) => {
                return Err(std::io::Error::other(format!(
                    "waitpid({pid}) 返回意外 PID {waited}"
                )));
            }
            Err(error) if error.raw_os_error() == Some(libc::EINTR) => continue,
            // 交接启动边界可能已经先收掉这个退出状态。
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(true),
            Err(error) => return Err(error),
        }
    }
}

static GUI_REAPER: OnceLock<GuiReaper> = OnceLock::new();

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    width: f64,
    height: f64,
}

/// 点「打开 smelt」：拉起同目录的 GUI（dev 的 target 目录和 app 包内都叫 smelt）。
/// 已在跑的话，由 GUI 自己的单实例逻辑负责前置窗口，这里只管发起。
fn gui_reaper() -> std::io::Result<&'static GuiReaper> {
    if let Some(reaper) = GUI_REAPER.get() {
        return Ok(reaper);
    }

    let (sender, receiver) = std::sync::mpsc::channel::<GuiChild>();
    let pids = Arc::new(Mutex::new(HashSet::new()));
    let reaper_pids = Arc::clone(&pids);
    thread::Builder::new()
        .name("smelt-gui-reaper".into())
        .spawn(move || {
            let mut children = Vec::<GuiChild>::new();
            loop {
                if children.is_empty() {
                    let Ok(child) = receiver.recv() else {
                        break;
                    };
                    children.push(child);
                } else {
                    match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(child) => children.push(child),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                while let Ok(child) = receiver.try_recv() {
                    children.push(child);
                }
                // 不能对第一个 Child 阻塞 wait：首个 GUI 通常会长期存活，那会让后续
                // 单实例启动器即使已经退出也永远排在它后面。后台线程逐个做精确 PID
                // 的非阻塞 wait，既不抢别的模块的退出状态，也能并发管理多个 launcher。
                children.retain_mut(|child| match child.try_reap() {
                    Ok(false) => true,
                    Ok(true) => {
                        reaper_pids.lock().unwrap().remove(&child.pid());
                        false
                    }
                    Err(error) => {
                        super::dlog(&format!(
                            "menubar: 回收 GUI launcher pid={} 失败：{error}",
                            child.pid()
                        ));
                        // 未确认 ECHILD/已回收前不能放弃唯一 owner。保留 Child 并在
                        // 下一轮重试，避免一次瞬态系统调用失败重新制造永久僵尸。
                        true
                    }
                });
            }
        })?;
    // 正常只有 AppKit 主线程初始化。即便测试并发撞上，输掉 set 的 sender 一 drop，
    // 它对应的空 reaper 会自然退出；所有调用方都返回唯一的静态 sender。
    let _ = GUI_REAPER.set(GuiReaper { sender, pids });
    GUI_REAPER
        .get()
        .ok_or_else(|| std::io::Error::other("GUI child reaper 初始化失败"))
}

fn spawn_gui(gui: &Path) -> std::io::Result<i32> {
    use std::process::Stdio;

    let reaper = gui_reaper()?;
    let child = std::process::Command::new(gui)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = child.id() as i32;
    reaper.pids.lock().unwrap().insert(pid);
    if let Err(error) = reaper.sender.send(GuiChild::Spawned(child)) {
        reaper.pids.lock().unwrap().remove(&pid);
        // 静态 sender 存在时 receiver 理论上不会消失。若线程异常退出，宁可在这个
        // 极端失败路径同步 wait 一次，也不能重新制造永久僵尸。
        if let GuiChild::Spawned(mut child) = error.0 {
            let _ = child.wait();
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "GUI child reaper 已退出",
        ));
    }
    Ok(pid)
}

/// exec 交接快照：只返回本模块确实持有回收权的直接子进程。
pub(crate) fn child_pids_for_handoff() -> Vec<i32> {
    let Some(reaper) = GUI_REAPER.get() else {
        return Vec::new();
    };
    let mut pids: Vec<_> = reaper.pids.lock().unwrap().iter().copied().collect();
    pids.sort_unstable();
    pids
}

/// exec 后为仍存活的菜单 GUI launcher 重建精确 PID owner。已经在启动边界被 sweep
/// 掉的 PID 会得到 ECHILD，按“已回收”处理；绝不使用运行期 waitpid(-1)。
pub(crate) fn restore_child_pids(pids: &[i32]) -> std::io::Result<()> {
    let pids: HashSet<_> = pids.iter().copied().filter(|pid| *pid > 1).collect();
    if pids.is_empty() {
        return Ok(());
    }
    let reaper = gui_reaper()?;
    for pid in pids {
        if !reaper.pids.lock().unwrap().insert(pid) {
            continue;
        }
        if reaper.sender.send(GuiChild::Inherited(pid)).is_err() {
            reaper.pids.lock().unwrap().remove(&pid);
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("GUI child reaper 无法接管 handoff pid={pid}"),
            ));
        }
    }
    Ok(())
}

extern "C" fn on_open(_this: &Object, _cmd: Sel, _sender: *mut Object) {
    // 与终端/ACP spawn 共用升级门闩：handoff 快照 PID 集合后到 exec 之间不能再冒出
    // 一个未被记录的新 GUI 直接子进程。
    let _spawn_gate = super::SPAWN_GATE
        .read()
        .unwrap_or_else(|error| error.into_inner());
    if let Ok(exe) = super::daemon_executable_path() {
        let gui = exe.with_file_name("smelt");
        let _ = spawn_gui(&gui);
    }
}

/// 点「退出 smelt」：整个守护进程退出。注意这会关掉所有 PTY——所有会话（含正在
/// 跑的 agent）随之结束。后果已写进菜单项文案里。先清 iroh 隧道与远程网关，
/// 避免端口残留。
extern "C" fn on_quit(_this: &Object, _cmd: Sel, _sender: *mut Object) {
    crate::plugin_runtime::stop();
    crate::cleanup_sidecar_services();
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
pub(crate) fn run_event_loop() -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn nonblocking_exact_wait_retries_eintr_without_dropping_ownership() {
        let pid = 42;
        let mut calls = 0;
        let reaped = try_reap_pid_with(pid, |waited_pid| {
            assert_eq!(waited_pid, pid);
            calls += 1;
            if calls == 1 {
                Err(std::io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(pid)
            }
        })
        .unwrap();

        assert!(reaped);
        assert_eq!(calls, 2, "EINTR 后必须继续等待同一个精确 PID");
    }

    #[test]
    fn unexpected_nonblocking_wait_error_is_not_treated_as_reaped() {
        let error = try_reap_pid_with(42, |_| Err(std::io::Error::from_raw_os_error(libc::EIO)))
            .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EIO));
    }

    fn wait_until_reaped_without_stealing_status(pid: i32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn short_lived_gui_launcher_is_reaped_without_a_daemon_wide_wait() {
        let pid = spawn_gui(Path::new("/usr/bin/true")).unwrap();
        let reaped = wait_until_reaped_without_stealing_status(pid, Duration::from_secs(1));
        if !reaped {
            unsafe {
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
        }
        assert!(
            reaped,
            "菜单栏丢弃 Child 会把已经退出的短命 GUI 启动器留成 smeltd 的僵尸子进程"
        );
    }

    #[test]
    fn long_lived_gui_does_not_block_reaping_later_launchers() {
        use std::process::{Command, Stdio};

        let blocker = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let blocker_pid = blocker.id() as i32;
        let reaper = gui_reaper().unwrap();
        reaper.pids.lock().unwrap().insert(blocker_pid);
        reaper.sender.send(GuiChild::Spawned(blocker)).unwrap();
        let handoff_contains_blocker = child_pids_for_handoff().contains(&blocker_pid);

        let short_pid = spawn_gui(Path::new("/usr/bin/true")).unwrap();
        let short_reaped =
            wait_until_reaped_without_stealing_status(short_pid, Duration::from_secs(1));

        // 无论断言结果如何，都先让后台 owner 收掉长命子进程，避免失败用例污染进程表。
        unsafe {
            libc::kill(blocker_pid, libc::SIGKILL);
        }
        let _ = wait_until_reaped_without_stealing_status(blocker_pid, Duration::from_secs(1));
        let _ = wait_until_reaped_without_stealing_status(short_pid, Duration::from_secs(1));

        assert!(
            handoff_contains_blocker,
            "仍存活且由菜单 reaper 持有的 PID 必须进入 exec handoff"
        );
        assert!(
            short_reaped,
            "一个长生命周期 GUI 不能阻塞后续已退出启动器的回收"
        );
    }
}
