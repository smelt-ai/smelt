//! 视图注册表：插件能画的东西。
//!
//! 插件只说自己能画什么（名字、标题、图标、怎么建），不说摆在哪：摆在哪由布局定，布局归
//! 用户。设置页也是视图，只是多带一项 [`SettingsEntry`]，由设置摆。
//!
//! 和命令、布局一样按来源整组登记，id 是 `<来源>/<名字>`，一个插件换不掉别人的视图。
//! 宿主只记 id，宿主里不出现视图的类型。建好的视图也记在这里：登记被替换或撤下，旧视图
//! 跟着放掉。
//!
//! 按实例建的视图（[`ViewSpec::instances`]，如每段会话一份的终端）不归布局：别的插件用
//! [`create_view`] 按参数建一份，自己摆、自己留着；只想放进主窗口看的（JS 插件就只能这样）
//! 用 [`OPEN_VIEW`] 带参数打开，宿主建一份放进出口。

use std::rc::Rc;

use gpui::{AnyView, App, Context, Entity, FocusHandle, Global, Render, SharedString, Window};

use serde::{Deserialize, Serialize};

use crate::{Command, HostEvent, SettingsEntry};

/// 打开一个视图：哪页的布局里摆了它就去那页让它露出来，哪页都没摆就放进出口（见
/// [`crate::page`]）；设置页在设置里打开、停在这一页。再把 `args` 交给它
/// （[`PluginView::open`]）。没登记过这个 id、哪都放不下，结果是 `false`。
///
/// 按实例建的（[`ViewSpec::instances`]）每次按 `args` 新建一份放进出口，焦点交给它；出口
/// 换了内容就放掉，重启后不放回来。建不出来、没有能放的出口是失败。
pub const OPEN_VIEW: Command<OpenView, bool> = Command::new("smelt.app/open-view");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenView {
    /// 视图 id。
    pub view: String,
    /// 交给视图的参数，认不出的字段视图忽略。
    #[serde(default)]
    pub args: serde_json::Value,
}

/// 插件的一个视图。
pub trait PluginView: Render {
    /// 被打开：每次打开都会调，参数由打开的地方给（如 `{"tab": "plugins"}`），认不出的
    /// 字段忽略。
    fn open(&mut self, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>);

    /// 聚焦这个视图时键盘焦点落在哪（如终端的输入、对话的输入框）。摆它的一方切过来时
    /// 聚焦它（见 [`ViewInstance::focus`]）。默认 `None`：不接管焦点。
    fn focus_target(&self, _cx: &App) -> Option<FocusHandle> {
        None
    }
}

/// 宿主拿着的视图：画出来，或再打开一次。
pub trait ViewHandle {
    fn view(&self) -> AnyView;
    fn open(&self, args: &serde_json::Value, window: &mut Window, cx: &mut App);
    /// 见 [`PluginView::focus_target`]。
    fn focus_target(&self, cx: &App) -> Option<FocusHandle>;
}

impl<V: PluginView> ViewHandle for Entity<V> {
    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn open(&self, args: &serde_json::Value, window: &mut Window, cx: &mut App) {
        self.update(cx, |view, cx| view.open(args, window, cx));
    }

    fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        self.read(cx).focus_target(cx)
    }
}

/// 设视图的角标：名字后面的短字，如待处理的条数。宿主画在视图的入口上（面板标签、侧栏
/// 里的那一行），视图建没建都画。状态变了由插件推，宿主不来问。
pub const SET_VIEW_BADGE: Command<ViewBadge, ()> = Command::new("smelt.app/set-view-badge");

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewBadge {
    /// 视图 id。
    pub view: String,
    /// 角标上的字；`None` 清掉。
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Default)]
struct ViewBadges(std::collections::BTreeMap<String, SharedString>);

impl Global for ViewBadges {}

/// 设视图的角标，见 [`SET_VIEW_BADGE`]。变了发 [`HostEvent::ViewBadgesChanged`]。登记
/// 视图之前设也行；撤下视图不清，插件自己清。
pub fn set_view_badge(id: &str, label: Option<SharedString>, cx: &mut App) {
    let label = label.filter(|label| !label.is_empty());
    let badges = &mut cx.default_global::<ViewBadges>().0;
    let changed = match label {
        Some(label) => badges.insert(id.to_string(), label.clone()).as_ref() != Some(&label),
        None => badges.remove(id).is_some(),
    };
    if changed {
        crate::emit(HostEvent::ViewBadgesChanged, cx);
    }
}

/// 视图的角标。只读内存，render 里可以调。
pub fn view_badge(id: &str, cx: &App) -> Option<SharedString> {
    cx.try_global::<ViewBadges>()?.0.get(id).cloned()
}

/// 登记一个视图要给的东西。没有位置：摆在哪归宿主。
#[derive(Clone)]
pub struct ViewSpec {
    /// 来源内的名字，不含 `/`。视图 id 是 `<来源>/<名字>`。
    pub name: SharedString,
    pub title: SharedString,
    /// 图标的资源路径，如 `IconName::LayoutDashboard.path()`。
    pub icon: SharedString,
    /// 视图自己画顶上那一条（拖窗口、给红黄绿按钮让位、标题）：单独一组时外壳不给它画
    /// 标题带，也不让放大。画法上的事，和摆在哪无关；放进出口也一样。
    ///
    /// 不画的（一般的插件页）由外壳画一条 34 高的标题带：居中写 `title`，能拖窗口。那条
    /// 只放标题，页面的切换、新建这些控件放在视图自己的第一行（`ui::page_toolbar`）。
    pub draws_top_bar: bool,
    /// 是一页设置：放进设置侧栏的哪一组、按哪些词能搜到。设置页由设置摆，布局不摆它。
    pub settings: Option<SettingsEntry>,
    /// 按实例建：每份参数一个，由别的插件用 [`create_view`] 建、自己摆、自己留着，如每段
    /// 会话一份的终端和对话。宿主不把它摆进布局，不给它入口；[`OPEN_VIEW`] 带参数打开时
    /// 建一份放进出口。不能是设置页。
    pub instances: bool,
    /// 第一次露出来时建视图；之后一直用这一份，直到登记被替换或撤下。按实例建的每次
    /// [`create_view`] 建一份新的。
    pub build: ViewBuilder,
}

/// 建视图的函数。
pub type ViewBuilder = Rc<dyn Fn(&mut Window, &mut App) -> Rc<dyn ViewHandle>>;

/// 登记过的一个视图：登记时给的，加上 id。
#[derive(Clone)]
pub struct ViewInfo {
    /// `<来源>/<名字>`。
    pub id: SharedString,
    pub title: SharedString,
    pub icon: SharedString,
    pub draws_top_bar: bool,
    pub settings: Option<SettingsEntry>,
    pub instances: bool,
    pub build: ViewBuilder,
}

struct Entry {
    owner: String,
    info: ViewInfo,
    view: Option<Rc<dyn ViewHandle>>,
}

#[derive(Default)]
struct ViewRegistry {
    views: Vec<Entry>,
}

impl Global for ViewRegistry {}

/// 按来源整组登记视图：这个来源原来的视图全换成这一组，空的就是全撤下。一个不合法整组
/// 不登记，原来的不动。来源第一次登记排在最后，再登记留在原来的位置。
///
/// 同 id、建视图的函数也还是同一个（`Rc::ptr_eq`）的，建好的视图留着；别的旧视图放掉。
/// 所以一个来源只改了其中一个视图时，整组再登记一遍不会重建别的。
pub fn register_views(owner: &str, specs: Vec<ViewSpec>, cx: &mut App) -> Result<(), String> {
    if owner.is_empty() || owner.contains('/') {
        return Err(format!("视图来源「{owner}」不能为空，也不能含 `/`"));
    }
    let mut infos: Vec<ViewInfo> = Vec::with_capacity(specs.len());
    for spec in specs {
        if spec.name.is_empty() || spec.name.contains('/') {
            return Err(format!("视图名「{}」不能为空，也不能含 `/`", spec.name));
        }
        let id: SharedString = format!("{owner}/{}", spec.name).into();
        if infos.iter().any(|info| info.id == id) {
            return Err(format!("视图「{id}」登记了两次"));
        }
        if spec.title.trim().is_empty() {
            return Err(format!("视图「{id}」没有标题"));
        }
        if spec.instances && spec.settings.is_some() {
            return Err(format!("视图「{id}」按实例建，不能是设置页"));
        }
        infos.push(ViewInfo {
            id,
            title: spec.title,
            icon: spec.icon,
            draws_top_bar: spec.draws_top_bar,
            settings: spec.settings,
            instances: spec.instances,
            build: spec.build,
        });
    }
    let views = &mut cx.default_global::<ViewRegistry>().views;
    let at = views
        .iter()
        .position(|entry| entry.owner == owner)
        .unwrap_or(views.len());
    let (mut old, kept): (Vec<Entry>, Vec<Entry>) = std::mem::take(views)
        .into_iter()
        .partition(|entry| entry.owner == owner);
    *views = kept;
    if old.is_empty() && infos.is_empty() {
        return Ok(());
    }
    let entries: Vec<Entry> = infos
        .into_iter()
        .map(|info| {
            let view = old
                .iter_mut()
                .find(|old| old.info.id == info.id && Rc::ptr_eq(&old.info.build, &info.build))
                .and_then(|old| old.view.take());
            Entry {
                owner: owner.to_string(),
                info,
                view,
            }
        })
        .collect();
    let at = at.min(views.len());
    views.splice(at..at, entries);
    // 没接上的旧视图随 `old` 放掉。
    drop(old);
    crate::emit(HostEvent::ViewsChanged, cx);
    Ok(())
}

/// 撤下一个来源的全部视图，建好的跟着放掉。已经用 [`create_view`] 建出去的实例归拿着的
/// 一方，不在这里收回。
pub fn unregister_views(owner: &str, cx: &mut App) {
    if !cx.has_global::<ViewRegistry>() {
        return;
    }
    let views = &mut cx.global_mut::<ViewRegistry>().views;
    let (removed, kept): (Vec<Entry>, Vec<Entry>) = std::mem::take(views)
        .into_iter()
        .partition(|entry| entry.owner == owner);
    *views = kept;
    if !removed.is_empty() {
        drop(removed);
        crate::emit(HostEvent::ViewsChanged, cx);
    }
}

/// 登记过的视图，按登记顺序。只读内存，render 里可以调。
pub fn views(cx: &App) -> Vec<ViewInfo> {
    cx.try_global::<ViewRegistry>()
        .map(|registry| {
            registry
                .views
                .iter()
                .map(|entry| entry.info.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 一个登记过的视图；没登记是 `None`。只读内存。
pub fn view(id: &str, cx: &App) -> Option<ViewInfo> {
    Some(entry(id, cx)?.info.clone())
}

/// 这个 id 有没有登记。只读内存，render 里可以调：别的视图据此决定要不要给「去那里」的入口。
pub fn is_registered(id: &str, cx: &App) -> bool {
    entry(id, cx).is_some()
}

/// 已经建好的视图。只读内存，render 里取它；没建过或没登记是 `None`。按实例建的视图
/// 这里不记，也是 `None`。
pub fn built_view(id: &str, cx: &App) -> Option<Rc<dyn ViewHandle>> {
    entry(id, cx)?.view.clone()
}

/// 取视图，没建过就现在建。第二项为 `true` 表示这次新建的，宿主随后要 `open` 它一次。
/// 没登记过、或按实例建（用 [`create_view`]）是 `None`。在事件里调，不在 render 里。
pub fn ensure_view(
    id: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<(Rc<dyn ViewHandle>, bool)> {
    let entry = entry(id, cx).filter(|entry| !entry.info.instances)?;
    if let Some(view) = &entry.view {
        return Some((view.clone(), false));
    }
    let build = entry.info.build.clone();
    let view = build(window, cx);
    // 建的时候插件可能刚好替换了这个视图：只记到还是这份登记的那一项上。
    if let Some(entry) = cx
        .global_mut::<ViewRegistry>()
        .views
        .iter_mut()
        .find(|entry| entry.info.id.as_ref() == id && Rc::ptr_eq(&entry.info.build, &build))
    {
        entry.view = Some(view.clone());
    }
    Some((view, true))
}

/// 按实例建的视图（[`ViewSpec::instances`]）的一份。谁建谁留着、谁摆；扔掉就放掉，视图
/// 里的订阅、任务跟着结束。登记被替换或撤下不收回它，拿着的一方要换新的就看
/// [`HostEvent::ViewsChanged`]。
pub struct ViewInstance {
    id: SharedString,
    handle: Rc<dyn ViewHandle>,
}

impl ViewInstance {
    /// 建它的视图 id。
    pub fn id(&self) -> &SharedString {
        &self.id
    }

    /// 画出来用的视图，每次给的都是同一份。
    pub fn view(&self) -> AnyView {
        self.handle.view()
    }

    /// 键盘焦点该落在哪，见 [`PluginView::focus_target`]。只读内存。
    pub fn focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        self.handle.focus_target(cx)
    }

    /// 把键盘焦点交给它。视图不接管焦点（[`PluginView::focus_target`] 是 `None`）就什么
    /// 都不做，结果是 `false`。
    pub fn focus(&self, window: &mut Window, cx: &mut App) -> bool {
        let Some(handle) = self.focus_handle(cx) else {
            return false;
        };
        window.focus(&handle, cx);
        true
    }
}

/// 按 `args` 建一份按实例建的视图，建好先 `open(args)` 一次再交出来。没登记过、或它
/// 不是按实例建的，是 `Err`（给人看的原因）。在事件里调，不在 render 里。
pub fn create_view(
    id: &str,
    args: &serde_json::Value,
    window: &mut Window,
    cx: &mut App,
) -> Result<ViewInstance, String> {
    let Some(entry) = entry(id, cx) else {
        return Err(format!("没有视图「{id}」"));
    };
    if !entry.info.instances {
        return Err(format!("视图「{id}」不是按实例建的，用 OPEN_VIEW 打开"));
    }
    let id = entry.info.id.clone();
    let build = entry.info.build.clone();
    let handle = build(window, cx);
    handle.open(args, window, cx);
    Ok(ViewInstance { id, handle })
}

fn entry<'a>(id: &str, cx: &'a App) -> Option<&'a Entry> {
    cx.try_global::<ViewRegistry>()?
        .views
        .iter()
        .find(|entry| entry.info.id.as_ref() == id)
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, Context, IntoElement, Render, TestAppContext, div};

    use super::*;

    struct Blank;

    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    impl PluginView for Blank {
        fn open(&mut self, _: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
    }

    /// 按实例建的视图：记下打开时给的参数，有自己的焦点。
    struct Probe {
        args: Vec<serde_json::Value>,
        focus: FocusHandle,
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    impl PluginView for Probe {
        fn open(&mut self, args: &serde_json::Value, _: &mut Window, _: &mut Context<Self>) {
            self.args.push(args.clone());
        }

        fn focus_target(&self, _: &App) -> Option<FocusHandle> {
            Some(self.focus.clone())
        }
    }

    fn spec(name: &str) -> ViewSpec {
        ViewSpec {
            name: name.to_string().into(),
            title: name.to_string().into(),
            icon: "icons/bell.svg".into(),
            draws_top_bar: false,
            settings: None,
            instances: false,
            build: Rc::new(|_, cx| -> Rc<dyn ViewHandle> { Rc::new(cx.new(|_| Blank)) }),
        }
    }

    fn instance_spec(name: &str) -> ViewSpec {
        ViewSpec {
            instances: true,
            build: Rc::new(|_, cx| -> Rc<dyn ViewHandle> {
                Rc::new(cx.new(|cx| Probe {
                    args: Vec::new(),
                    focus: cx.focus_handle(),
                }))
            }),
            ..spec(name)
        }
    }

    fn ids(cx: &App) -> Vec<String> {
        views(cx).into_iter().map(|view| view.id.into()).collect()
    }

    #[gpui::test]
    fn views_are_registered_per_owner_and_keep_their_place(cx: &mut TestAppContext) {
        cx.update(|cx| {
            register_views("notify", vec![spec("main")], cx).unwrap();
            register_views("trader", vec![spec("a"), spec("b")], cx).unwrap();
            assert_eq!(ids(cx), ["notify/main", "trader/a", "trader/b"]);
            // 再登记是整组替换，位置不变。
            register_views("notify", vec![spec("main"), spec("more")], cx).unwrap();
            assert_eq!(
                ids(cx),
                ["notify/main", "notify/more", "trader/a", "trader/b"]
            );
            unregister_views("trader", cx);
            assert_eq!(ids(cx), ["notify/main", "notify/more"]);
            register_views("notify", Vec::new(), cx).unwrap();
            assert!(views(cx).is_empty());
            assert!(view("notify/main", cx).is_none());
        });
    }

    #[gpui::test]
    fn an_invalid_view_rejects_the_whole_set(cx: &mut TestAppContext) {
        cx.update(|cx| {
            register_views("trader", vec![spec("a")], cx).unwrap();
            let mut untitled = spec("c");
            untitled.title = " ".into();
            for specs in [
                vec![spec("b"), spec("b")],
                vec![spec("b"), spec("x/y")],
                vec![spec("")],
                vec![spec("b"), untitled],
            ] {
                assert!(register_views("trader", specs, cx).is_err());
                assert_eq!(ids(cx), ["trader/a"]);
            }
            for owner in ["", "a/b"] {
                assert!(register_views(owner, vec![spec("b")], cx).is_err());
            }
            // 按实例建的不能是设置页。
            let mut settings = instance_spec("b");
            settings.settings = Some(SettingsEntry::default());
            assert!(register_views("trader", vec![settings], cx).is_err());
            assert_eq!(ids(cx), ["trader/a"]);
        });
    }

    /// 按实例建的每次建一份新的，建好先按参数打开一次；登记里不记，`ensure_view` 不建它。
    /// 普通视图不能按实例建。
    #[gpui::test]
    fn instance_views_are_built_per_call_and_not_cached(cx: &mut TestAppContext) {
        cx.add_empty_window().update(|window, cx| {
            register_views("term", vec![instance_spec("pane"), spec("main")], cx).unwrap();
            assert!(is_registered("term/pane", cx));
            assert!(ensure_view("term/pane", window, cx).is_none());

            let args = serde_json::json!({ "session_id": "s1" });
            let first = create_view("term/pane", &args, window, cx).unwrap();
            let second = create_view("term/pane", &args, window, cx).unwrap();
            assert_eq!(first.id().as_ref(), "term/pane");
            assert_ne!(first.view().entity_id(), second.view().entity_id());
            let probe = first.view().downcast::<Probe>().unwrap();
            assert_eq!(probe.read(cx).args, [args.clone()]);
            assert!(built_view("term/pane", cx).is_none());

            assert!(first.focus(window, cx));
            let focus = first.focus_handle(cx).unwrap();
            assert!(focus.is_focused(window));

            assert!(create_view("term/main", &args, window, cx).is_err());
            assert!(create_view("term/missing", &args, window, cx).is_err());
        });
    }

    /// 整组再登记时，建视图的函数没换的那几个留着建好的视图，换了的放掉。
    #[gpui::test]
    fn reregistering_keeps_views_whose_builder_is_unchanged(cx: &mut TestAppContext) {
        cx.add_empty_window().update(|window, cx| {
            let kept = spec("kept");
            register_views("p", vec![kept.clone(), spec("swapped")], cx).unwrap();
            for id in ["p/kept", "p/swapped"] {
                assert!(ensure_view(id, window, cx).is_some_and(|(_, built)| built));
            }
            let before = built_view("p/kept", cx).unwrap().view().entity_id();
            register_views("p", vec![kept, spec("swapped")], cx).unwrap();
            assert_eq!(built_view("p/kept", cx).unwrap().view().entity_id(), before);
            assert!(built_view("p/swapped", cx).is_none());
        });
    }
}
