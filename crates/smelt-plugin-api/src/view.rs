//! 视图注册表：插件能画的东西。
//!
//! 插件只说自己能画什么（名字、标题、图标、怎么建），不说摆在哪：摆在哪由布局定，布局归
//! 用户。设置页也是视图，只是多带一项 [`SettingsEntry`]，由设置摆。
//!
//! 和命令、布局一样按来源整组登记，id 是 `<来源>/<名字>`，一个插件换不掉别人的视图。
//! 宿主只记 id，宿主里不出现视图的类型。建好的视图也记在这里：登记被替换或撤下，旧视图
//! 跟着放掉。

use std::rc::Rc;

use gpui::{AnyView, App, Context, Entity, Global, Render, SharedString, Window};

use serde::{Deserialize, Serialize};

use crate::{Command, HostEvent, SettingsEntry};

/// 打开一个视图：哪页的布局里摆了它就去那页让它露出来，哪页都没摆就放进出口（见
/// [`crate::page`]）；设置页在设置里打开、停在这一页。再把 `args` 交给它
/// （[`PluginView::open`]）。没登记过这个 id 结果是 `false`。
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
}

/// 宿主拿着的视图：画出来，或再打开一次。
pub trait ViewHandle {
    fn view(&self) -> AnyView;
    fn open(&self, args: &serde_json::Value, window: &mut Window, cx: &mut App);
}

impl<V: PluginView> ViewHandle for Entity<V> {
    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn open(&self, args: &serde_json::Value, window: &mut Window, cx: &mut App) {
        self.update(cx, |view, cx| view.open(args, window, cx));
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
    /// 能针对某台机器打开：宿主给每台机器都留一个入口，从那里打开时参数带
    /// `{"machine": …}`（见 [`crate::machine_to_json`]）。
    pub per_machine: bool,
    /// 视图自己画顶上那一条（拖窗口、给红黄绿按钮让位、标题）：单独一组时外壳不给它画
    /// 标题带，也不让放大。画法上的事，和摆在哪无关。
    pub draws_top_bar: bool,
    /// 是一页设置：放进设置侧栏的哪一组、按哪些词能搜到。设置页由设置摆，布局不摆它。
    pub settings: Option<SettingsEntry>,
    /// 第一次露出来时建视图；之后一直用这一份，直到登记被替换或撤下。
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
    pub per_machine: bool,
    pub draws_top_bar: bool,
    pub settings: Option<SettingsEntry>,
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
        infos.push(ViewInfo {
            id,
            title: spec.title,
            icon: spec.icon,
            per_machine: spec.per_machine,
            draws_top_bar: spec.draws_top_bar,
            settings: spec.settings,
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

/// 撤下一个来源的全部视图，建好的跟着放掉。
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

/// 已经建好的视图。只读内存，render 里取它；没建过或没登记是 `None`。
pub fn built_view(id: &str, cx: &App) -> Option<Rc<dyn ViewHandle>> {
    entry(id, cx)?.view.clone()
}

/// 取视图，没建过就现在建。第二项为 `true` 表示这次新建的，宿主随后要 `open` 它一次。
/// 没登记过是 `None`。在事件里调，不在 render 里。
pub fn ensure_view(
    id: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<(Rc<dyn ViewHandle>, bool)> {
    let entry = entry(id, cx)?;
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

    fn spec(name: &str) -> ViewSpec {
        ViewSpec {
            name: name.to_string().into(),
            title: name.to_string().into(),
            icon: "icons/bell.svg".into(),
            per_machine: false,
            draws_top_bar: false,
            settings: None,
            build: Rc::new(|_, cx| -> Rc<dyn ViewHandle> { Rc::new(cx.new(|_| Blank)) }),
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
