//! 用其他应用打开：探测这台电脑上装了哪些编辑器 / IDE，连同系统文件管理器一起登记进会话
//! 列表项目行的右键菜单（[`PROJECT_MENU`]，只列本机项目），点了用它打开项目目录。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。菜单怎么画、开着的菜单怎么跟着变归
//! 画菜单的一方；有哪些应用、图标、怎么启动归这里。文件管理器那一项交给宿主的
//! `smelt.app/open-in-file-manager`，不另写一份。
//!
//! 探测要问 Launch Services、翻 PATH、读应用包里的图标，放后台低优先级跑：启动时一次，
//! 之后主窗口每回到前台再探一次（装了、卸了编辑器都是在别的应用里做的，回来时菜单就对
//! 了）。没探完之前菜单里只有文件管理器。没有定时器。

mod detect;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::ui::IconName;
use api::{ContextMenuAction, ContextMenuItem, HostEvent, RuntimeKey};
use gpui::{App, Global, Priority, Task};
use rust_i18n::t;
use serde::Deserialize;
use smelt_plugin_api::{self as api, text};

use detect::InstalledIde;

rust_i18n::i18n!("locales", fallback = "en");

/// 本插件的来源 id。
const OWNER: &str = "smelt.ide";

// 约定：smelt.session-list 的 PROJECT_MENU，上下文是 `{machine, path}`；权威定义在
// crates/plugins/session-list/src/contract.rs。
const PROJECT_MENU: &str = "smelt.session-list/project";

/// 菜单项点了执行的命令。
const OPEN_WITH: api::Command<OpenWith, ()> = api::Command::new("smelt.ide/open-with");

/// [`OPEN_WITH`] 的参数：登记时给的 `app` 并上 [`PROJECT_MENU`] 的上下文（这里只用 `path`）。
#[derive(Deserialize)]
struct OpenWith {
    /// [`FILE_MANAGER`] 或 IDE 的名字。
    app: String,
    /// 本机项目根目录。
    path: PathBuf,
}

/// 文件管理器那一项的 [`OpenWith::app`]。IDE 用自己的名字，不会和它撞。
const FILE_MANAGER: &str = "file-manager";

/// 最近一次探测的结果。
#[derive(Default)]
struct Catalog {
    ides: Vec<InstalledIde>,
    file_manager_icon: Option<Arc<gpui::Image>>,
    /// 后台正在探：这期间再来的探测请求不另起一份。
    scanning: bool,
}

impl Global for Catalog {}

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    cx.set_global(Catalog::default());
    api::register_commands(
        OWNER,
        text!("open_with"),
        vec![OPEN_WITH.spec_async(text!("open_with"), |args, _, cx| open_with(args, cx))],
        cx,
    )
    .expect("用其他应用打开的命令表写错了");
    // 先把文件管理器登记上：它一定在，不用等探测。
    publish(cx);
    scan(cx);
    let events = api::events(cx);
    cx.subscribe(&events, |_, event: &HostEvent, cx| {
        if matches!(event, HostEvent::WindowActivated) {
            scan(cx);
        }
    })
    .detach();
}

fn open_with(args: OpenWith, cx: &mut App) -> Task<Result<(), String>> {
    if args.app == FILE_MANAGER {
        return api::OPEN_IN_FILE_MANAGER.run(&api::OpenInFileManager { path: args.path }, cx);
    }
    let catalog = cx.global::<Catalog>();
    let Some(ide) = catalog.ides.iter().find(|ide| ide.label == args.app) else {
        return Task::ready(Err(t!("error.app_missing", app = args.app).into_owned()));
    };
    Task::ready(detect::open_in(ide, Path::new(&args.path)))
}

/// 在后台重新探一次，探完把菜单换成新的。正在探就不另起。
fn scan(cx: &mut App) {
    let catalog = cx.global_mut::<Catalog>();
    if catalog.scanning {
        return;
    }
    catalog.scanning = true;
    let found = cx
        .background_executor()
        .spawn_with_priority(Priority::Low, async {
            (detect::detect_installed(), detect::file_manager_icon())
        });
    cx.spawn(async move |cx| {
        let (ides, file_manager_icon) = found.await;
        cx.update(|cx| {
            let catalog = cx.global_mut::<Catalog>();
            catalog.scanning = false;
            catalog.ides = ides;
            catalog.file_manager_icon = file_manager_icon;
            publish(cx);
        });
    })
    .detach();
}

/// 把目录登记成项目菜单里的一组子菜单：IDE 在前，文件管理器在最后。和上次一样时什么都
/// 不惊动。
fn publish(cx: &mut App) {
    let catalog = cx.global::<Catalog>();
    let mut apps: Vec<ContextMenuItem> = catalog
        .ides
        .iter()
        .map(|ide| app_item(ide.label, ide.icon.clone(), IconName::Terminal, ide.label))
        .collect();
    apps.push(app_item(
        detect::file_manager_label(),
        catalog.file_manager_icon.clone(),
        IconName::FolderOpen,
        FILE_MANAGER,
    ));
    let group = ContextMenuItem {
        label: text!("open_with"),
        image: None,
        icon: Some(IconName::ExternalLink.path()),
        // 应用都装在这台电脑上，远程项目的目录不在这里。
        machine: Some(RuntimeKey::Local),
        action: ContextMenuAction::Submenu(apps),
    };
    api::register_context_menu_items(OWNER, PROJECT_MENU, vec![group], cx)
        .expect("用其他应用打开的菜单项写错了");
}

/// 子菜单里的一个应用：有自己的彩色图标就画它，没有画 `fallback`。
fn app_item(
    label: impl Into<api::Text>,
    image: Option<Arc<gpui::Image>>,
    fallback: IconName,
    app: &str,
) -> ContextMenuItem {
    ContextMenuItem {
        label: label.into(),
        image: image.map(api::MenuImage::Decoded),
        icon: Some(fallback.path()),
        machine: None,
        action: ContextMenuAction::Run {
            command: OPEN_WITH.id().to_string(),
            args: api::serde_json::json!({ "app": app }),
        },
    }
}
