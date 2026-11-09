//! 视图注册表：插件能画的东西。
//!
//! 插件只说自己能画什么（id、标题、图标、怎么建），不说摆在哪：摆在侧栏入口、舞台还是
//! 面板里由宿主定。视图按字符串 id 登记，宿主只记 id，宿主里不出现视图的类型。同一个
//! id 只留最后登记的那份，用户插件可以借此替换内置的。建好的视图也记在这里：登记被替换
//! 或撤下，旧视图跟着放掉。

use std::rc::Rc;

use gpui::{AnyView, App, Context, Entity, Global, Render, SharedString, Window};

use crate::HostEvent;

/// 宿主自己的路由，视图不能用这几个 id。
pub const RESERVED_VIEW_IDS: [&str; 4] = ["agents", "automations", "desktop", "session"];

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

/// 登记一个视图要给的东西。没有位置：摆在哪归宿主。
#[derive(Clone)]
pub struct ViewSpec {
    pub id: SharedString,
    pub title: SharedString,
    /// 图标的资源路径，如 `IconName::LayoutDashboard.path()`。
    pub icon: SharedString,
    /// 能针对某台机器打开：宿主给每台机器都留一个入口，从那里打开时参数带
    /// `{"machine": …}`（见 [`crate::machine_to_json`]）。
    pub per_machine: bool,
    /// 第一次打开时建视图；之后一直用这一份，直到登记被替换或撤下。
    pub build: ViewBuilder,
}

/// 建视图的函数。
pub type ViewBuilder = Rc<dyn Fn(&mut Window, &mut App) -> Rc<dyn ViewHandle>>;

struct Entry {
    spec: ViewSpec,
    view: Option<Rc<dyn ViewHandle>>,
}

#[derive(Default)]
struct ViewRegistry {
    views: Vec<Entry>,
}

impl Global for ViewRegistry {}

/// 登记一个视图。同 id 的旧登记被替换，次序不变，旧视图放掉。
pub fn register_view(spec: ViewSpec, cx: &mut App) -> Result<(), String> {
    if RESERVED_VIEW_IDS.contains(&spec.id.as_ref()) {
        return Err(format!("视图 id「{}」是宿主自己的路由，不能用", spec.id));
    }
    let registry = cx.default_global::<ViewRegistry>();
    let entry = Entry { spec, view: None };
    match registry
        .views
        .iter_mut()
        .find(|view| view.spec.id == entry.spec.id)
    {
        Some(view) => *view = entry,
        None => registry.views.push(entry),
    }
    crate::emit(HostEvent::ViewsChanged, cx);
    Ok(())
}

/// 撤下一个视图。没登记过就什么都不做。
pub fn unregister_view(id: &str, cx: &mut App) {
    if !cx.has_global::<ViewRegistry>() {
        return;
    }
    let views = &mut cx.global_mut::<ViewRegistry>().views;
    let Some(index) = views.iter().position(|view| view.spec.id.as_ref() == id) else {
        return;
    };
    views.remove(index);
    crate::emit(HostEvent::ViewsChanged, cx);
}

/// 登记过的视图，按登记顺序。
pub fn views(cx: &App) -> Vec<ViewSpec> {
    cx.try_global::<ViewRegistry>()
        .map(|registry| {
            registry
                .views
                .iter()
                .map(|view| view.spec.clone())
                .collect()
        })
        .unwrap_or_default()
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
    let build = entry.spec.build.clone();
    let view = build(window, cx);
    // 建的时候插件可能刚好替换了这个视图：只记到还是这份登记的那一项上。
    if let Some(entry) = cx
        .global_mut::<ViewRegistry>()
        .views
        .iter_mut()
        .find(|entry| entry.spec.id.as_ref() == id && Rc::ptr_eq(&entry.spec.build, &build))
    {
        entry.view = Some(view.clone());
    }
    Some((view, true))
}

fn entry<'a>(id: &str, cx: &'a App) -> Option<&'a Entry> {
    cx.try_global::<ViewRegistry>()?
        .views
        .iter()
        .find(|view| view.spec.id.as_ref() == id)
}
