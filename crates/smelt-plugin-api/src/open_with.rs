//! 用其他应用打开：项目右键菜单里那一组本机应用（编辑器、IDE、文件管理器）。
//!
//! 有哪些应用、叫什么、长什么样、怎么启动，都归登记它们的插件；宿主只按登记的画菜单，
//! 点了执行登记者自己的一条要带参数的命令，参数是 [`OpenWith`]。和文件打开者一样用命令，
//! 执行照常进命令队列，失败由宿主报。
//!
//! 按来源整组登记，再登记是整组替换：插件探测完、图标读到了，就把整组再登记一次；变了
//! 宿主把开着的菜单原地重建（见 [`observe_open_with_apps`]）。只用于本机目录。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, Global, SharedString, Subscription};
use serde::{Deserialize, Serialize};

/// 菜单里的一个应用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenWithApp {
    /// 菜单上的名字。
    pub label: SharedString,
    /// 应用自己的彩色图标，已经解码好：菜单直接画，不读文件。没有时画 `icon`。
    pub image: Option<Arc<gpui::Image>>,
    /// 没有 `image` 时画的线性图标的资源路径，如 `IconName::SquareTerminal.path()`。
    pub icon: SharedString,
    /// 点了执行的命令（`<来源>/<名字>`），要带参数 [`OpenWith`]。
    pub command: String,
    /// 交给命令的 [`OpenWith::app`]，登记者自己认。
    pub app: String,
}

/// [`OpenWithApp::command`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenWith {
    /// 登记时给的 [`OpenWithApp::app`]。
    pub app: String,
    /// 这台电脑上目录的绝对路径。
    pub path: PathBuf,
}

/// 登记过的应用，按来源第一次登记的顺序。
#[derive(Default)]
struct OpenWithApps(Vec<(String, Vec<OpenWithApp>)>);

impl Global for OpenWithApps {}

/// 按来源整组登记：这个来源原来的全换成这一组，空的就是全撤下。来源第一次登记排在最后，
/// 再登记留在原来的位置。和原来一样就什么都不做，不惊动在看的菜单。
pub fn register_open_with_apps(owner: &str, apps: Vec<OpenWithApp>, cx: &mut App) {
    let current = cx.try_global::<OpenWithApps>().and_then(|registry| {
        registry
            .0
            .iter()
            .find(|(entry, _)| entry == owner)
            .map(|(_, current)| current)
    });
    let unchanged = match current {
        Some(current) => *current == apps,
        None => apps.is_empty(),
    };
    if unchanged {
        return;
    }
    let registry = &mut cx.default_global::<OpenWithApps>().0;
    match registry.iter().position(|(entry, _)| entry == owner) {
        Some(at) if apps.is_empty() => {
            registry.remove(at);
        }
        Some(at) => registry[at].1 = apps,
        None => registry.push((owner.to_string(), apps)),
    }
}

/// 现在能列的应用：按登记顺序，跳过命令已经撤下的。只读内存，render 里可以调。
pub fn open_with_apps(cx: &App) -> Vec<OpenWithApp> {
    let Some(registry) = cx.try_global::<OpenWithApps>() else {
        return Vec::new();
    };
    registry
        .0
        .iter()
        .flat_map(|(_, apps)| apps)
        .filter(|app| crate::command(&app.command, cx).is_some())
        .cloned()
        .collect()
}

/// 登记的应用变了就调 `f`。留着返回的订阅才一直收得到。
pub fn observe_open_with_apps(cx: &mut App, f: impl FnMut(&mut App) + 'static) -> Subscription {
    cx.observe_global::<OpenWithApps>(f)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::{CommandSpec, Handled, register_commands, unregister_commands};

    fn app(owner: &str, label: &str) -> OpenWithApp {
        OpenWithApp {
            label: label.to_string().into(),
            image: None,
            icon: "icons/folder-open.svg".into(),
            command: format!("{owner}/open-with"),
            app: label.to_string(),
        }
    }

    fn register_command(owner: &str, cx: &mut App) {
        let spec = CommandSpec {
            name: "open-with".to_string(),
            title: "用其他应用打开".to_string(),
            key: None,
            context: "Workspace".to_string(),
            palette: false,
            requires_args: true,
            handler: Rc::new(|_, _, _| Handled::ok()),
        };
        register_commands(owner, owner, vec![spec], cx).unwrap();
    }

    fn labels(cx: &App) -> Vec<String> {
        open_with_apps(cx)
            .into_iter()
            .map(|app| app.label.to_string())
            .collect()
    }

    #[gpui::test]
    fn apps_are_registered_per_owner_and_keep_their_place(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_command("ide", cx);
            register_command("mine", cx);
            register_open_with_apps("ide", vec![app("ide", "Finder")], cx);
            register_open_with_apps("mine", vec![app("mine", "Vim")], cx);
            assert_eq!(labels(cx), ["Finder", "Vim"]);

            // 再登记是整组替换，位置不变。
            register_open_with_apps("ide", vec![app("ide", "Zed"), app("ide", "Finder")], cx);
            assert_eq!(labels(cx), ["Zed", "Finder", "Vim"]);

            // 命令撤了就不列。
            unregister_commands("mine", cx);
            assert_eq!(labels(cx), ["Zed", "Finder"]);

            register_open_with_apps("ide", Vec::new(), cx);
            assert!(labels(cx).is_empty());
        });
    }

    #[gpui::test]
    fn only_real_changes_reach_observers(cx: &mut gpui::TestAppContext) {
        let changes = Rc::new(Cell::new(0));
        let _subscription = cx.update(|cx| {
            register_command("ide", cx);
            let seen = changes.clone();
            observe_open_with_apps(cx, move |_| seen.set(seen.get() + 1))
        });
        cx.update(|cx| register_open_with_apps("ide", vec![app("ide", "Zed")], cx));
        cx.run_until_parked();
        assert_eq!(changes.get(), 1);

        cx.update(|cx| {
            register_open_with_apps("ide", vec![app("ide", "Zed")], cx);
            // 没登记过的来源撤下也不算变。
            register_open_with_apps("nobody", Vec::new(), cx);
        });
        cx.run_until_parked();
        assert_eq!(changes.get(), 1);
    }
}
