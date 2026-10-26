//! macOS 菜单栏图标：打开 GUI / 退出守护。
//!
//! 从 `main.rs` 整块搬出。点「退出」会清 sidecar 再 `process::exit`，结束所有会话。

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use std::path::Path;
use std::process::Child;
use std::sync::OnceLock;
use std::thread;

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
fn spawn_gui(gui: &Path) -> std::io::Result<i32> {
    use std::process::Stdio;

    let child = std::process::Command::new(gui)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(reap_gui_child(child))
}

/// 每个 GUI 子进程一条线程，阻塞在它自己那个 pid 的 `waitpid` 上。首个 GUI 通常
/// 长期存活，各等各的，它就挡不住后面已经退出的单实例启动器；也不抢别的模块的
/// 退出状态。`Child` 不负责收尸，丢掉它只是放下句柄，进程仍归这条线程收。
fn reap_gui_child(child: Child) -> i32 {
    let pid = child.id() as i32;
    drop(child);
    let spawned = thread::Builder::new()
        .name("smelt-gui-reaper".into())
        .spawn(move || reap_tracked_gui_child(pid));
    if spawned.is_err() {
        // 起不了线程时宁可在这个极端失败路径同步等一次，也不能制造永久僵尸。
        reap_tracked_gui_child(pid);
    }
    pid
}

fn reap_tracked_gui_child(pid: i32) {
    if !smelt_sys::process_wait::reap_direct_child(pid) {
        super::dlog(&format!(
            "menubar: 回收 GUI launcher pid={pid} 失败：{}",
            std::io::Error::last_os_error()
        ));
    }
}

extern "C" fn on_open(_this: &Object, _cmd: Sel, _sender: *mut Object) {
    // 守护在停机就不拉 GUI：这是 AppKit 主线程，在闸门上排队会一直卡到进程退出。
    let Ok(_spawn_gate) = super::spawn_permit() else {
        return;
    };
    if let Ok(exe) = super::daemon_executable_path() {
        let gui = exe.with_file_name("smelt");
        let _ = spawn_gui(&gui);
    }
}

/// 点「退出 smelt」：整个守护进程退出。注意这会关掉所有 PTY——所有会话（含正在
/// 跑的 agent）随之结束。后果已写进菜单项文案里。先关 iroh endpoint，免得已连上的
/// 远程设备挂在一个已死的运行时上。
extern "C" fn on_quit(_this: &Object, _cmd: Sel, _sender: *mut Object) {
    crate::protocol::graceful_shutdown(None, None);
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
        let blocker_pid = reap_gui_child(blocker);

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
            short_reaped,
            "一个长生命周期 GUI 不能阻塞后续已退出启动器的回收"
        );
    }
}
