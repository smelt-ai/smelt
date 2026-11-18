//! 退出前的守卫：用户要退出（⌘Q、菜单「退出 Smelt」）时，宿主先问登记过的守卫，有一个
//! 接手了，宿主就不再弹自己的退出确认、也不退出；接手的插件做完自己的事（比如先拉起更新
//! 安装程序）再用 [`QUIT_NOW`] 退出，或者干脆不退了（用户在它的确认框里点了取消）。
//!
//! 只管「用户要退出」这一步。系统关机、Windows 关掉最后一扇窗之类不经过这里，插件要在
//! 进程收尾时做事用 gpui 的 `on_app_quit`。

use std::rc::Rc;

use gpui::{App, Global};

use crate::Command;

/// 守卫：返回 `true` 表示这次退出由它接手。它在宿主处理退出的当中被调，不要在里面直接
/// 更新主窗口（会重入）；要开确认框先 `cx.defer`。
pub type QuitGuard = Rc<dyn Fn(&mut App) -> bool>;

/// 直接退出：不问守卫，不弹宿主的退出确认。接手了退出的守卫做完自己的事用它。
pub const QUIT_NOW: Command<(), ()> = Command::new("smelt.app/quit-now");

#[derive(Default)]
struct QuitGuards(Vec<(String, QuitGuard)>);

impl Global for QuitGuards {}

/// 登记来源 `owner` 的退出守卫。同一来源再登记就换掉原来那个。
pub fn register_quit_guard(owner: &str, guard: QuitGuard, cx: &mut App) {
    let guards = &mut cx.default_global::<QuitGuards>().0;
    guards.retain(|(entry, _)| entry != owner);
    guards.push((owner.to_string(), guard));
}

/// 撤下来源 `owner` 的退出守卫。
pub fn unregister_quit_guard(owner: &str, cx: &mut App) {
    if cx.has_global::<QuitGuards>() {
        cx.global_mut::<QuitGuards>()
            .0
            .retain(|(entry, _)| entry != owner);
    }
}

/// 宿主用：按登记的次序问守卫，有一个接手就停下、返回 `true`，宿主这次不退出。
pub fn quit_guarded(cx: &mut App) -> bool {
    let guards: Vec<QuitGuard> = match cx.try_global::<QuitGuards>() {
        Some(guards) => guards.0.iter().map(|(_, guard)| guard.clone()).collect(),
        None => return false,
    };
    guards.into_iter().any(|guard| guard(cx))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[gpui::test]
    fn first_guard_that_takes_over_stops_the_quit(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(!quit_guarded(cx));
            let asked = Rc::new(Cell::new(0));
            let counter = asked.clone();
            register_quit_guard(
                "pass",
                Rc::new(move |_| {
                    counter.set(counter.get() + 1);
                    false
                }),
                cx,
            );
            assert!(!quit_guarded(cx));
            register_quit_guard("take", Rc::new(|_| true), cx);
            assert!(quit_guarded(cx));
            assert_eq!(asked.get(), 2);
            unregister_quit_guard("take", cx);
            assert!(!quit_guarded(cx));
        });
    }
}
