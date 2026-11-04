//! 人在不在这台 Mac 前：决定手机推送马上发、先扣着，还是不发。
//!
//! 业内做法（Slack、Discord）：桌面在用时不推手机，人一离开就推。这里「在用」要同时满足：
//!
//! 1. 本机的 Smelt 桌面 App 连着（订阅着状态通道）。没开时 Mac 上弹不出系统通知，只能靠手机。
//! 2. 屏幕没锁、显示器没睡、当前会话在前台（没有被快速切换用户切走）。
//! 3. [`AWAY_AFTER`] 之内碰过键盘或鼠标。看的是整台 Mac：在别的 App 里干活时，Smelt 的
//!    系统通知照样弹在 Mac 上。
//!
//! 读数都是来事时现读（CoreGraphics，不要权限），不常驻定时器。人离开的边沿只有锁屏有系统
//! 事件（notify(3) 的 `com.apple.sessionagent.screenIsLocked`），桌面 App 断开由订阅连接的
//! 生命周期给出；「闲置满几分钟」没有事件，由推送那边按 [`Presence::Here`] 给的剩余时间定一次
//! 闹钟，只在有扣着的提醒时存在。
//!
//! 只认本机的桌面 App：连着这台运行时的远程桌面在另一台机器上，读不到那边的人在不在，按不在
//! 处理（照推）。不是 macOS 的运行时一律按不在处理。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 键鼠闲置这么久就算人离开了。审批会卡住智能体，比 Slack 默认的 10 分钟短。
pub(crate) const AWAY_AFTER: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presence {
    /// 人在 Mac 前；`away_in` 之后若一直没碰键鼠，就算离开。
    Here {
        away_in: Duration,
    },
    Away,
}

/// 这一刻的读数。读不到的项按「人不在」算：宁可多推一条，也不漏掉要处理的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Readings {
    local_desktops: usize,
    /// `None`：读不到会话状态（没有窗口服务器）。
    screen_locked: Option<bool>,
    on_console: Option<bool>,
    display_asleep: bool,
    /// `None`：读不到闲置时间。
    idle: Option<Duration>,
}

fn judge(readings: Readings) -> Presence {
    let usable = readings.local_desktops > 0
        && readings.screen_locked == Some(false)
        && readings.on_console != Some(false)
        && !readings.display_asleep;
    match readings.idle {
        Some(idle) if usable && idle < AWAY_AFTER => Presence::Here {
            away_in: AWAY_AFTER - idle,
        },
        _ => Presence::Away,
    }
}

/// 现读一次。
pub(crate) fn current() -> Presence {
    let local_desktops = LOCAL_DESKTOPS.load(Ordering::SeqCst);
    if local_desktops == 0 {
        return Presence::Away;
    }
    #[cfg(target_os = "macos")]
    {
        judge(mac::readings(local_desktops))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Presence::Away
    }
}

static LOCAL_DESKTOPS: AtomicUsize = AtomicUsize::new(0);
static ON_AWAY: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// 人离开时叫一声（锁屏、本机桌面 App 全断开）。只设一次，之后再设不生效。macOS 上顺带
/// 起锁屏监听线程。
pub(crate) fn on_away(callback: impl Fn() + Send + Sync + 'static) {
    if ON_AWAY.set(Box::new(callback)).is_ok() {
        #[cfg(target_os = "macos")]
        mac::watch_screen_lock();
    }
}

fn fire_away() {
    if let Some(callback) = ON_AWAY.get() {
        callback();
    }
}

/// 本机桌面 App 的一条状态订阅。订阅连接活多久就拿多久；最后一条断开时算人离开。
pub(crate) struct LocalDesktopLease;

impl LocalDesktopLease {
    pub(crate) fn acquire() -> Self {
        LOCAL_DESKTOPS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for LocalDesktopLease {
    fn drop(&mut self) {
        if LOCAL_DESKTOPS.fetch_sub(1, Ordering::SeqCst) == 1 {
            fire_away();
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_int, c_void};
    use std::time::Duration;

    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use core_foundation_sys::dictionary::CFDictionaryRef;

    use super::Readings;
    use crate::dlog;

    /// `kCGEventSourceStateHIDSystemState`：整台机器的键鼠，不分 App。
    const HID_SYSTEM_STATE: i32 = 1;
    /// `kCGAnyInputEventType`。
    const ANY_INPUT_EVENT: u32 = u32::MAX;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
        fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayIsAsleep(display: u32) -> u32;
    }

    pub(super) fn readings(local_desktops: usize) -> Readings {
        let (screen_locked, on_console) = session_flags();
        let idle =
            unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) };
        Readings {
            local_desktops,
            screen_locked,
            on_console,
            display_asleep: unsafe { CGDisplayIsAsleep(CGMainDisplayID()) } != 0,
            idle: (idle.is_finite() && idle >= 0.0).then(|| Duration::from_secs_f64(idle)),
        }
    }

    /// 当前登录会话：屏幕锁没锁、是不是在前台。锁屏时字典里才有 `CGSSessionScreenIsLocked`。
    fn session_flags() -> (Option<bool>, Option<bool>) {
        let raw = unsafe { CGSessionCopyCurrentDictionary() };
        if raw.is_null() {
            return (None, None);
        }
        let session: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_create_rule(raw) };
        let flag = |key: &'static str| {
            session
                .find(CFString::from_static_string(key))
                .and_then(|value| value.downcast::<CFBoolean>())
                .map(bool::from)
        };
        let locked = flag("CGSSessionScreenIsLocked").unwrap_or(false);
        (Some(locked), flag("kCGSSessionOnConsoleKey"))
    }

    // 锁屏走 notify(3)：loginwindow 锁屏时发 `com.apple.sessionagent.screenIsLocked`（实测）。
    // 用文件描述符接收，线程阻塞在 `read` 上，不用 CFRunLoop。同 `remote.rs` 的供电方式监听。
    const NOTIFY_STATUS_OK: u32 = 0;
    const SCREEN_LOCKED_KEY: &std::ffi::CStr = c"com.apple.sessionagent.screenIsLocked";

    unsafe extern "C" {
        fn notify_register_file_descriptor(
            name: *const c_char,
            notify_fd: *mut c_int,
            flags: c_int,
            out_token: *mut c_int,
        ) -> u32;
        fn notify_cancel(token: c_int) -> u32;
    }

    /// 起锁屏监听线程，跟进程同寿。订阅不上只记日志：扣着的提醒照样在闲置满时补推。
    pub(super) fn watch_screen_lock() {
        let mut fd = -1;
        let mut token = 0;
        let status = unsafe {
            notify_register_file_descriptor(SCREEN_LOCKED_KEY.as_ptr(), &mut fd, 0, &mut token)
        };
        if status != NOTIFY_STATUS_OK {
            dlog(&format!("订阅锁屏事件失败：notify {status}"));
            return;
        }
        // 别漏进拉起的子进程。
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        let spawned = std::thread::Builder::new()
            .name("smeltd-screen-lock".into())
            .spawn(move || {
                run_screen_lock_watch(fd);
                unsafe { notify_cancel(token) };
            });
        if let Err(error) = spawned {
            unsafe { notify_cancel(token) };
            dlog(&format!("锁屏监听线程启动失败：{error}"));
        }
    }

    fn run_screen_lock_watch(fd: c_int) {
        loop {
            // 每条通知是 4 字节的订阅号，只用来叫醒，内容不看。
            let mut token = [0u8; 4];
            let read = unsafe { libc::read(fd, token.as_mut_ptr().cast::<c_void>(), token.len()) };
            if read < 0 {
                match std::io::Error::last_os_error().kind() {
                    std::io::ErrorKind::Interrupted => continue,
                    // fd 要是非阻塞的，就停在 poll 上等下一条。
                    std::io::ErrorKind::WouldBlock => {
                        let mut pollfd = libc::pollfd {
                            fd,
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        unsafe { libc::poll(&mut pollfd, 1, -1) };
                        continue;
                    }
                    _ => {}
                }
            }
            if read <= 0 {
                dlog("锁屏监听中断，扣着的提醒改在闲置满时补推");
                return;
            }
            super::fire_away();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> Readings {
        Readings {
            local_desktops: 1,
            screen_locked: Some(false),
            on_console: Some(true),
            display_asleep: false,
            idle: Some(Duration::from_secs(60)),
        }
    }

    #[test]
    fn present_only_when_every_reading_says_so() {
        assert_eq!(
            judge(here()),
            Presence::Here {
                away_in: AWAY_AFTER - Duration::from_secs(60)
            }
        );

        type Case = (&'static str, fn(&mut Readings));
        let cases: [Case; 7] = [
            ("桌面 App 没连着", |r| r.local_desktops = 0),
            ("锁屏", |r| r.screen_locked = Some(true)),
            ("读不到会话", |r| r.screen_locked = None),
            ("切到了别的用户", |r| r.on_console = Some(false)),
            ("显示器睡了", |r| r.display_asleep = true),
            ("闲置满了", |r| r.idle = Some(AWAY_AFTER)),
            ("读不到闲置时间", |r| r.idle = None),
        ];
        for (case, change) in cases {
            let mut readings = here();
            change(&mut readings);
            assert_eq!(judge(readings), Presence::Away, "{case}");
        }
    }

    /// 读不到「在不在前台」不算离开：老系统的会话字典里没有这一项。
    #[test]
    fn missing_console_flag_is_not_away() {
        let mut readings = here();
        readings.on_console = None;
        assert!(matches!(judge(readings), Presence::Here { .. }));
    }
}
