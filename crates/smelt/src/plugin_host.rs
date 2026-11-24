//! 宿主这一侧的插件接口：实现 [`smelt_plugin_api::Host`]，登记内置插件，把打开视图、换页
//! 的命令接到主窗口的路由上（见 `workspace_layout`）。

use rust_i18n::t;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui::*;
use smelt_plugin_api::{self as api, CommandSpec, HostEvent, Machine, RuntimeKey};
use smelt_runtime_client::RuntimeClient;

use crate::dir_picker::PathPick;
use crate::{DaemonStates, MainWindow, Workspace, runtimes, status_item};

struct SmeltHost {
    current: Rc<RefCell<Option<MainWindow>>>,
}

impl api::Host for SmeltHost {
    fn machines(&self, cx: &App) -> Vec<Machine> {
        let mut keys = vec![RuntimeKey::Local];
        if let Some(hub) = cx.try_global::<runtimes::RuntimeHub>() {
            keys.extend(
                hub.remotes
                    .iter()
                    .map(|remote| remote.runtime.key().clone()),
            );
        }
        keys.into_iter()
            .map(|key| Machine {
                name: runtimes::runtime_name(&key, cx),
                unavailable: runtimes::runtime_unavailable_label(&key, cx),
                key,
            })
            .collect()
    }

    fn client(&self, machine: &RuntimeKey, cx: &App) -> Option<RuntimeClient> {
        runtimes::client(machine, cx)
    }

    fn connector(
        &self,
        machine: &RuntimeKey,
        cx: &App,
    ) -> Option<std::sync::Arc<dyn api::client::Connector>> {
        runtimes::runtime(machine, cx).map(|runtime| runtimes::connector(&runtime))
    }

    fn machine_online(&self, machine: &RuntimeKey, cx: &App) -> bool {
        runtimes::runtime_online(machine, cx)
    }

    fn connections(&self, cx: &App) -> api::Connections {
        runtimes::connections(cx)
    }

    fn view_shown(&self, id: &str, cx: &App) -> bool {
        view_shown(id, cx)
    }

    fn view_laid_out(&self, id: &str, cx: &App) -> bool {
        cx.try_global::<LaidOutViews>()
            .is_some_and(|views| views.0.contains(id))
    }

    fn window_active(&self, cx: &App) -> bool {
        // 应用在不在前台现问：全屏窗口切到别的 Space 后不再出帧，窗口也还是 key，只有应用级的
        // 状态跟得上。窗口是不是 key 随激活事件记在全局，不读 Workspace 实体，避免工作台更新
        // 期间（如 sync_shell_view -> MainShell::publish）二次借用 panic。
        status_item::is_app_active() && main_window_key(cx)
    }

    fn overlay_open(&self, cx: &App) -> bool {
        main_overlay_open(cx)
    }

    fn main_window(&self, _cx: &App) -> Option<AnyWindowHandle> {
        self.current
            .borrow()
            .as_ref()
            .map(|main| main.window.into())
    }

    fn path_style(&self, machine: &RuntimeKey, cx: &App) -> api::model::runtime_path::PathStyle {
        runtimes::path_style(machine, cx)
    }

    fn session_states(
        &self,
        machine: &RuntimeKey,
        cx: &App,
    ) -> Vec<api::model::daemon_state::DaemonSessionState> {
        DaemonStates::snapshot(machine, cx)
    }

    fn session_presence(
        &self,
        machine: &RuntimeKey,
        session_id: &str,
        cx: &App,
    ) -> Option<api::model::daemon_state::RuntimePresence> {
        DaemonStates::runtime_presence(machine, session_id, cx)
    }

    fn workspace_directory(
        &self,
        machine: &RuntimeKey,
        cx: &App,
    ) -> Option<api::model::workspace_directory::WorkspaceDirectory> {
        cx.try_global::<crate::WorkspaceDirectoriesGlobal>()?
            .received
            .get(machine)
            .cloned()
    }

    fn sessions_revision(&self, machine: &RuntimeKey, cx: &App) -> Option<(u64, u64)> {
        Some((
            crate::WorkspaceDirectoriesGlobal::revision(machine, cx),
            DaemonStates::revision(machine, cx),
        ))
    }

    fn notification_permission(&self, _cx: &App) -> api::NotificationPermission {
        status_item::notification_permission()
    }
}

/// 宿主动作命令取工作台用：工作台没建（测试里只登记命令表）时是 `None`。
pub(crate) type WorkspaceRef = Rc<dyn Fn(&App) -> Option<Entity<Workspace>>>;

/// 在工作台里做 `f`；工作台没了算失败。主窗口就是工作台所在的窗口，所以用得到窗口的
/// 命令先取窗口（[`main_window`]），再进这里。
pub(crate) fn update_workspace<R>(
    workspace: &WorkspaceRef,
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut Context<Workspace>) -> Result<R, String>,
) -> Result<R, String> {
    let workspace = workspace(cx).ok_or_else(api::no_main_window)?;
    workspace.update(cx, f)
}

/// 命令拿到的主窗口；没开时交回 [`api::no_main_window`]。
pub(crate) fn main_window(window: Option<&mut Window>) -> Result<&mut Window, String> {
    window.ok_or_else(api::no_main_window)
}

/// 主窗口的槽位，跟 `main` 里管窗口的是同一份：主窗口关着时命令要能再开一扇。
struct MainWindowSlot(Rc<RefCell<Option<MainWindow>>>);

impl Global for MainWindowSlot {}

/// 在主窗口里对工作台做 `f`，再把主窗口拿到前面来。`window` 是命令拿到的主窗口；没有就是
/// 主窗口关着，先开一扇（命令不是在主窗口的 update 里跑的，可以开）。
pub(crate) fn in_main_window<R>(
    window: Option<&mut Window>,
    workspace: &WorkspaceRef,
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
) -> Result<R, String> {
    let result = match window {
        Some(window) => {
            let workspace = workspace(cx).ok_or_else(api::no_main_window)?;
            let result = workspace.update(cx, |workspace, cx| f(workspace, window, cx));
            // 应用前置之外再激活主窗口，macOS 才会切回它所在的原生全屏 Space。
            window.activate_window();
            result
        }
        None => {
            let current = cx
                .try_global::<MainWindowSlot>()
                .map(|slot| slot.0.clone())
                .ok_or_else(api::no_main_window)?;
            let main = crate::ensure_main_window(&current, cx);
            main.window
                .update(cx, |_, window, cx| {
                    let result = main
                        .workspace
                        .update(cx, |workspace, cx| f(workspace, window, cx));
                    window.activate_window();
                    result
                })
                .map_err(|error| t!("error.main_window", error = error).into_owned())?
                .map_err(|_| api::no_main_window())?
        }
    };
    crate::status_item::activate_app();
    Ok(result)
}

/// 宿主的动作 `smelt.app/*`（参数、结果见 `smelt_plugin_api` 里的同名常量）。
pub(crate) fn app_commands(workspace: &WorkspaceRef) -> Vec<CommandSpec> {
    let picker = workspace.clone();
    let views = workspace.clone();
    let pages = workspace.clone();
    let shown = workspace.clone();
    let commands = vec![
        // 开窗口、开视图、开页面只改人眼前看到的，开给命令行：智能体写完插件能把它亮出来。
        api::SHOW_MAIN_WINDOW
            .spec(
                api::text!("command.show_main_window"),
                move |_, window, cx| in_main_window(window, &shown, cx, |_, _, _| ()),
            )
            .callable_from_cli(),
        api::QUIT_NOW.spec(api::text!("command.quit_now"), |_, _, cx| {
            crate::request_app_quit(cx);
            Ok(())
        }),
        api::SET_DOCK_BADGE.spec(api::text!("command.set_dock_badge"), |badge, _, _| {
            crate::dock::set_badge(badge.label.as_deref());
            Ok(())
        }),
        api::NOTIFY.spec(api::text!("command.notify"), |args, _, _| {
            if args.id.as_deref().is_some_and(str::is_empty) {
                return Err(t!("error.empty_notification_id").into_owned());
            }
            crate::status_item::deliver_plugin_notification(&args);
            Ok(())
        }),
        api::WITHDRAW_NOTIFICATION.spec(
            api::text!("command.withdraw_notification"),
            |args, _, _| {
                crate::status_item::withdraw_plugin_notification(&args.id);
                Ok(())
            },
        ),
        api::OPEN_NOTIFICATION_SETTINGS.spec(
            api::text!("command.open_notification_settings"),
            |_, _, cx| {
                if let Some(url) = crate::status_item::system_notification_settings_url() {
                    cx.open_url(&url);
                }
                Ok(())
            },
        ),
        api::REQUEST_NOTIFICATION_PERMISSION.spec(
            api::text!("command.request_notification_permission"),
            |_, _, _| {
                crate::status_item::request_system_notification_authorization();
                Ok(())
            },
        ),
        api::OPEN_IN_TEXT_EDITOR.spec_async(
            api::text!("command.open_in_text_editor"),
            |args, _, cx| {
                cx.spawn(async move |_| {
                    smelt_sys::blocking::run(move || {
                        crate::system_open::open_in_system_text_editor(&args.path)
                    })
                    .await
                })
            },
        ),
        api::OPEN_IN_FILE_MANAGER.spec_async(
            api::text!("command.open_in_file_manager"),
            |args, _, cx| {
                cx.spawn(async move |_| {
                    smelt_sys::blocking::run(move || {
                        crate::system_open::open_in_file_manager(&args.path)
                    })
                    .await
                })
            },
        ),
        api::PICK_PATHS.spec_async(
            api::text!("command.pick_paths"),
            move |request, window, cx| {
                let Some(runtime) = runtimes::runtime(&request.machine, cx) else {
                    return Task::ready(Err(api::machine_removed()));
                };
                if runtime.is_local() {
                    // 系统选择框不需要主窗口；从菜单栏等处调用时 app 可能在后台，先拿到前面来。
                    crate::status_item::activate_app();
                    let picked = crate::dir_picker::pick_local_paths(&request, cx);
                    return cx.spawn(async move |_| Ok(picked.await));
                }
                let (done, received) = smol::channel::bounded(1);
                let pick = PathPick { request, done };
                // 远程机器在主窗口里逐级浏览，主窗口关着就先开一扇。
                if let Err(error) = in_main_window(window, &picker, cx, |workspace, _, cx| {
                    workspace.open_dir_picker(runtime, pick, cx)
                }) {
                    return Task::ready(Err(error));
                }
                // 取消时选择框连同 `done` 一起扔掉，这边收到关闭。
                cx.spawn(async move |_| Ok(received.recv().await.ok()))
            },
        ),
        api::SET_VIEW_BADGE.spec(api::text!("command.set_view_badge"), |args, _, cx| {
            api::set_view_badge(&args.view, args.label.map(Into::into), cx);
            Ok(())
        }),
        api::OPEN_VIEW
            .spec(api::text!("command.open_view"), move |args, window, cx| {
                let window = main_window(window)?;
                update_workspace(&views, cx, |workspace, cx| {
                    if api::view(&args.view, cx).is_some_and(|view| view.instances) {
                        return workspace
                            .open_instance(&args.view, &args.args, window, cx)
                            .map(|()| true);
                    }
                    Ok(workspace.open_view(&args.view, args.args, window, cx))
                })
            })
            .callable_from_cli(),
        api::OPEN_PAGE
            .spec(api::text!("command.open_page"), move |args, window, cx| {
                let window = main_window(window)?;
                update_workspace(&pages, cx, |workspace, cx| {
                    if api::page(&args.page, cx).is_none() {
                        return Ok(false);
                    }
                    Ok(workspace.navigate(args.page, window, cx))
                })
            })
            .callable_from_cli(),
        // 远程机器的连接表归宿主（它据此拨号、订阅），插件经这几条命令改，见
        // `smelt_plugin_api::connections`。
        api::ADD_MACHINE.spec_async(api::text!("command.add_machine"), |args, _, cx| {
            let added = runtimes::add_runtime(args.pairing_uri, cx);
            cx.spawn(async move |_| {
                let (machine, name) = added.await?;
                Ok(api::AddedMachine { machine, name })
            })
        }),
        api::REMOVE_MACHINE.spec_async(api::text!("command.remove_machine"), |args, _, cx| {
            match remote_id(&args.machine, cx) {
                Ok(id) => runtimes::remove_runtime(id, cx),
                Err(error) => Task::ready(Err(error)),
            }
        }),
        api::RENAME_MACHINE.spec(api::text!("command.rename_machine"), |args, _, cx| {
            let id = remote_id(&args.machine, cx)?;
            let name = args.name.trim();
            if name.is_empty() {
                return Err(t!("error.empty_name").into_owned());
            }
            runtimes::rename_runtime(id, name.to_string(), cx);
            Ok(())
        }),
        api::SET_MACHINE_RELAY.spec_async(
            api::text!("command.set_machine_relay"),
            |args, _, cx| {
                let id = match remote_id(&args.machine, cx) {
                    Ok(id) => id,
                    Err(error) => return Task::ready(Err(error)),
                };
                runtimes::set_runtime_relay(id, args.relay, cx)
            },
        ),
        api::RECONNECT_MACHINE.spec(api::text!("command.reconnect_machine"), |args, _, cx| {
            remote_id(&args.machine, cx)?;
            crate::runtime_events::reconnect(&args.machine, cx);
            Ok(())
        }),
        api::UPGRADE_MACHINE.spec(api::text!("command.upgrade_machine"), |args, _, cx| {
            remote_id(&args.machine, cx)?;
            if !runtimes::runtime_can_upgrade(&args.machine, cx) {
                return Err(t!("error.cannot_upgrade").into_owned());
            }
            crate::runtime_events::upgrade_remote(&args.machine, cx);
            Ok(())
        }),
        api::REFRESH_CONNECTIONS.spec(api::text!("command.refresh_connections"), |(), _, cx| {
            runtimes::sample_links(cx);
            crate::runtime_events::probe_remotes(cx);
            Ok(())
        }),
    ];
    commands
}

/// 连接表里的一台远程机器的 id。本机不在表里，移除、改名这些都不适用。
fn remote_id(machine: &RuntimeKey, cx: &App) -> Result<String, String> {
    match machine {
        RuntimeKey::Local => Err(t!("error.local_not_remote").into_owned()),
        RuntimeKey::Remote(id) if runtimes::runtime(machine, cx).is_some() => Ok(id.clone()),
        RuntimeKey::Remote(_) => Err(api::machine_removed()),
    }
}

/// 启动时、开主窗口之前调一次：装好宿主服务，登记内置插件。
pub(crate) fn install(current: Rc<RefCell<Option<MainWindow>>>, cx: &mut App) {
    cx.set_global(MainWindowSlot(current.clone()));
    api::install(SmeltHost { current }, cx);
    // 机器加了、移除了、改名、连上断开都记在 RuntimeHub 上。
    cx.observe_global::<runtimes::RuntimeHub>(|cx| api::emit(HostEvent::MachinesChanged, cx))
        .detach();
    // 会话状态（守护推的）和工作区目录（项目、对话的名册）都写在全局上，主窗口关着也照常。
    cx.observe_global::<DaemonStates>(|cx| api::emit(HostEvent::SessionsChanged, cx))
        .detach();
    cx.observe_global::<crate::WorkspaceDirectoriesGlobal>(|cx| {
        api::emit(HostEvent::SessionsChanged, cx)
    })
    .detach();
}

/// 此刻看得见的视图。摆视图的地方各报各的：主窗口报开着的那页里露着的视图（含出口里
/// 放着的），设置报它停着的那一页，设置视图本身露着时那一页才算看得见。插件只看并起来的一套
/// （`Host::view_shown`），不分视图摆在哪。
#[derive(Default)]
pub(crate) struct ShownViews {
    window: HashSet<String>,
    settings: Option<SharedString>,
}

impl Global for ShownViews {}

impl ShownViews {
    fn contains(&self, id: &str) -> bool {
        self.window.contains(id)
            || (self.settings.as_deref() == Some(id)
                && self.window.contains(crate::settings::SETTINGS_VIEW))
    }
}

/// 视图此刻看得见，和插件看到的是同一套。只读内存。
pub(crate) fn view_shown(id: &str, cx: &App) -> bool {
    cx.try_global::<ShownViews>()
        .is_some_and(|shown| shown.contains(id))
}

/// 主窗口里看得见的视图；主窗口关了报空的。
pub(crate) fn set_window_shown(shown: HashSet<String>, cx: &mut App) {
    // 先只读比较：可写地取全局（`default_global`）就算它变了，读过它的视图都要重画、观察者都
    // 收到通知，没变不能碰。
    let current = cx.try_global::<ShownViews>();
    if current.map_or(shown.is_empty(), |views| views.window == shown) {
        return;
    }
    cx.default_global::<ShownViews>().window = shown;
    api::emit(HostEvent::ShownViewsChanged, cx);
}

/// 设置停着的那一页（设置页视图的 id）；设置视图放掉了报 `None`。
pub(crate) fn set_settings_shown(page: Option<SharedString>, cx: &mut App) {
    // 同 `set_window_shown`：没变不碰全局。
    let current = cx.try_global::<ShownViews>();
    if current.map_or(page.is_none(), |views| views.settings == page) {
        return;
    }
    cx.default_global::<ShownViews>().settings = page;
    api::emit(HostEvent::ShownViewsChanged, cx);
}

/// 摆在各页眼下装的布局里的视图（`workspace_layout::sync_laid_out_views` 算），插件经
/// `Host::view_laid_out` 读。
#[derive(Default)]
struct LaidOutViews(HashSet<String>);

impl Global for LaidOutViews {}

/// 换一份摆着的视图；变了发 [`HostEvent::LaidOutViewsChanged`]。
pub(crate) fn set_laid_out(views: HashSet<String>, cx: &mut App) {
    // 同 `set_window_shown`：没变不碰全局。
    let current = cx.try_global::<LaidOutViews>();
    if current.map_or(views.is_empty(), |laid_out| laid_out.0 == views) {
        return;
    }
    cx.default_global::<LaidOutViews>().0 = views;
    api::emit(HostEvent::LaidOutViewsChanged, cx);
}

/// 主窗口给 [`SmeltHost`] 读的两份状态：是不是 key window、有没有开宿主遮罩层。
///
/// 独立存全局而不是让 `SmeltHost` 去读 `Workspace` 实体：主壳（`MainShell::publish`）和插件
/// 视图在 `Workspace` 正被借用期间（如 `sync_shell_view`、`sync_outlet`、`open_view`）就会查
/// 这两个状态，再借工作台实体会触发 GPUI 的 `double_lease_panic`。按 `owner` 认当前主窗口，
/// 避免关窗重开时旧工作台滞后释放把新窗口的状态清掉。
#[derive(Default)]
struct MainWindowState {
    owner: Option<EntityId>,
    window_key: bool,
    overlay_open: bool,
}

impl Global for MainWindowState {}

fn main_window_key(cx: &App) -> bool {
    cx.try_global::<MainWindowState>()
        .is_some_and(|state| state.window_key)
}

fn main_overlay_open(cx: &App) -> bool {
    cx.try_global::<MainWindowState>()
        .is_some_and(|state| state.overlay_open)
}

/// 主窗口建好时接管状态：写入初始 key 状态并清掉上一扇窗口可能遗留的遮罩状态。
pub(crate) fn attach_main_window(owner: EntityId, window_key: bool, cx: &mut App) {
    let had_overlay = main_overlay_open(cx);
    cx.set_global(MainWindowState {
        owner: Some(owner),
        window_key,
        overlay_open: false,
    });
    if had_overlay {
        api::emit(HostEvent::OverlayChanged, cx);
    }
}

/// 同步主窗口是不是 key window；旧工作台或值未变时不动全局（`prepare_frame` 每帧都走这里）。
pub(crate) fn set_main_window_key(owner: EntityId, window_key: bool, cx: &mut App) {
    let current = cx.try_global::<MainWindowState>();
    if current.is_some_and(|state| state.owner.is_some() && state.owner != Some(owner)) {
        return;
    }
    if current.is_some_and(|state| state.owner == Some(owner) && state.window_key == window_key) {
        return;
    }
    let state = cx.default_global::<MainWindowState>();
    state.owner = Some(owner);
    state.window_key = window_key;
}

/// 同步主窗口里宿主遮罩层的开关；变了发 [`HostEvent::OverlayChanged`]。
pub(crate) fn set_overlay_open(owner: EntityId, open: bool, cx: &mut App) {
    let current = cx.try_global::<MainWindowState>();
    if current.is_some_and(|state| state.owner.is_some() && state.owner != Some(owner)) {
        return;
    }
    let changed = current.map_or(open, |state| state.overlay_open != open);
    if !changed && (current.is_some_and(|state| state.owner == Some(owner)) || !open) {
        return;
    }
    let state = cx.default_global::<MainWindowState>();
    state.owner = Some(owner);
    state.overlay_open = open;
    if changed {
        api::emit(HostEvent::OverlayChanged, cx);
    }
}

/// 主窗口释放时清理它持有的状态。若已经重开了新的主窗口，旧工作台滞后释放时不覆盖新窗口，
/// 返回 `false` 让调用方也跳过清空视图可见状态。
pub(crate) fn release_main_window(owner: EntityId, cx: &mut App) -> bool {
    let Some(current) = cx.try_global::<MainWindowState>() else {
        return true;
    };
    if current.owner != Some(owner) {
        return false;
    }
    let had_overlay = current.overlay_open;
    cx.set_global(MainWindowState::default());
    if had_overlay {
        api::emit(HostEvent::OverlayChanged, cx);
    }
    true
}

impl Workspace {
    /// 宿主事件里工作台自己要管的：视图登记变了要重装布局（新登记的补上、撤下的变
    /// 占位）、对出口；页、布局登记变了要重挑各页的布局；主题登记变了重新套用选中的。
    pub(crate) fn subscribe_host_events(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &api::events(cx),
            window,
            |this, _, event: &HostEvent, window, cx| match event {
                HostEvent::ViewsChanged => {
                    this.reload_layout(window, cx);
                    this.sync_outlet(window, cx);
                    this.sync_nsh_settings_page(cx);
                    cx.notify();
                }
                HostEvent::LocaleChanged => {
                    this.sync_nsh_chrome(cx);
                    this.sync_nsh_settings_page(cx);
                }
                HostEvent::LayoutsChanged | HostEvent::PagesChanged => this.sync_pages(window, cx),
                HostEvent::ThemesChanged => {
                    if let Some(appearance) = cx.try_global::<crate::settings::Appearance>() {
                        let selected = appearance.theme_name.clone();
                        crate::settings::apply_named_theme(&selected, cx);
                        cx.refresh_windows();
                    }
                    cx.notify();
                }
                _ => {}
            },
        )
    }

    /// 遮罩层开了或关了就告诉插件：画着原生网页视图的插件要让开。插件视图在面板里是缓存着
    /// 画的，工作台重画不会带着它重画，只能靠事件。
    pub(crate) fn note_overlay(&mut self, cx: &mut Context<Self>) {
        set_overlay_open(cx.entity_id(), self.main_overlay_open(), cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;
    use std::rc::Rc;

    use gpui::AppContext as _;
    use smelt_plugin_api::{self as api, HostEvent};

    // 不 `use super::*`：上面 `use gpui::*` 带进来的 `gpui::test` 会把 `#[test]` 解析成它自己，
    // 无限展开。
    use super::{
        LaidOutViews, MainWindowState, ShownViews, SmeltHost, attach_main_window, main_window_key,
        release_main_window, set_laid_out, set_main_window_key, set_overlay_open,
        set_settings_shown, set_window_shown, view_shown,
    };

    /// 没变不碰全局：工作台每次同步布局都会把摆着、露着的视图重报一遍，可写地取一下就算
    /// 它变了，读过它的视图、观察者都会被惊动。
    #[gpui::test]
    fn reporting_the_same_views_leaves_the_globals_untouched(cx: &mut gpui::TestAppContext) {
        let shown = Rc::new(Cell::new(0));
        let laid_out = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let shown = shown.clone();
            cx.observe_global::<ShownViews>(move |_| shown.set(shown.get() + 1))
                .detach();
            let laid_out = laid_out.clone();
            cx.observe_global::<LaidOutViews>(move |_| laid_out.set(laid_out.get() + 1))
                .detach();
        });
        let views = HashSet::from(["a/list".to_string()]);
        for _ in 0..2 {
            cx.update(|cx| {
                set_window_shown(views.clone(), cx);
                set_settings_shown(Some("a/settings".into()), cx);
                set_laid_out(views.clone(), cx);
            });
        }
        assert_eq!(shown.get(), 1, "露着的视图第二次报的和第一次一样");
        assert_eq!(laid_out.get(), 1, "摆着的视图第二次报的和第一次一样");
        assert!(cx.update(|cx| view_shown("a/list", cx)));
    }

    /// 宿主状态从全局读，工作台实体正被借用更新期间查 `window_active` / `overlay_open`
    /// 不会二次借用；重复报相同值不动全局；重开主窗口后旧工作台滞后释放不覆盖新窗口。
    #[gpui::test]
    fn main_window_state_is_readable_during_entity_update_and_guarded_by_owner(
        cx: &mut gpui::TestAppContext,
    ) {
        let state_writes = Rc::new(Cell::new(0));
        let overlay_events = Rc::new(Cell::new(0));
        let (owner_a, owner_b) = cx.update(|cx| {
            api::install(
                SmeltHost {
                    current: Rc::new(RefCell::new(None)),
                },
                cx,
            );
            let writes = state_writes.clone();
            cx.observe_global::<MainWindowState>(move |_| writes.set(writes.get() + 1))
                .detach();
            let events = overlay_events.clone();
            cx.subscribe(&api::events(cx), move |_, event: &HostEvent, _| {
                if matches!(event, HostEvent::OverlayChanged) {
                    events.set(events.get() + 1);
                }
            })
            .detach();
            (cx.new(|_| ()), cx.new(|_| ()))
        });

        owner_a.update(cx, |_, cx| {
            let id = cx.entity_id();
            attach_main_window(id, true, cx);
            assert!(main_window_key(cx));
            assert!(!api::host(cx).overlay_open(cx));
            // 即使实体正被可变借用，调 window_active 也不二次借用。
            let _ = api::host(cx).window_active(cx);

            set_main_window_key(id, true, cx);
            set_overlay_open(id, false, cx);
        });
        assert_eq!(
            state_writes.get(),
            1,
            "重复报相同的 key/overlay 不应再触全局"
        );
        assert_eq!(
            overlay_events.get(),
            0,
            "遮罩层始终关闭，不应发 OverlayChanged"
        );

        owner_a.update(cx, |_, cx| {
            let id = cx.entity_id();
            set_overlay_open(id, true, cx);
            assert!(api::host(cx).overlay_open(cx));
            set_overlay_open(id, true, cx);
        });
        assert_eq!(state_writes.get(), 2);
        assert_eq!(overlay_events.get(), 1);

        // 重开新主窗口：接管所有权，清掉旧窗口遗留的遮罩状态并通知插件。
        owner_b.update(cx, |_, cx| {
            attach_main_window(cx.entity_id(), true, cx);
            assert!(main_window_key(cx));
            assert!(!api::host(cx).overlay_open(cx));
        });
        assert_eq!(overlay_events.get(), 2);

        // 旧工作台滞后更新和释放不得覆盖新主窗口。
        cx.update(|cx| {
            let old = owner_a.entity_id();
            set_main_window_key(old, false, cx);
            set_overlay_open(old, true, cx);
            assert!(
                !release_main_window(old, cx),
                "旧工作台释放不应返回 true 清掉新主窗口的视图状态"
            );
            assert!(main_window_key(cx));
            assert!(!api::host(cx).overlay_open(cx));
        });

        // 当前主窗口在遮罩开着时释放：清空状态并补发一次 OverlayChanged。
        cx.update(|cx| {
            let active = owner_b.entity_id();
            set_overlay_open(active, true, cx);
            assert!(api::host(cx).overlay_open(cx));
            assert!(release_main_window(active, cx));
            assert!(!main_window_key(cx));
            assert!(!api::host(cx).overlay_open(cx));
        });
        assert_eq!(overlay_events.get(), 4);
    }

    #[test]
    fn smelt_host_never_reads_workspace_entity() {
        let src = include_str!("plugin_host.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("plugin_host.rs 含测试模块");
        assert!(
            !prod.contains("workspace.read("),
            "SmeltHost 必须只读全局状态，禁止 workspace.read(cx) 以免更新期间二次借用 panic"
        );
    }
}
