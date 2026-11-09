//! 页面注册表：工作台舞台上能打开的整页。
//!
//! 页面按字符串 id 登记，宿主的路由只记 id，宿主里不出现页面的类型。同一个 id 只留最后
//! 登记的那份，用户插件可以借此替换内置页。建好的视图也记在这里：登记被替换或撤下，
//! 旧视图跟着放掉。

use std::rc::Rc;

use gpui::{AnyView, App, Context, Entity, Global, Render, SharedString, Window};

use crate::HostEvent;

/// 宿主自己的路由，页面不能用这几个 id。
pub const RESERVED_PAGE_IDS: [&str; 4] = ["agents", "automations", "desktop", "session"];

/// 一个页面视图。
pub trait Page: Render {
    /// 被打开：每次打开这一页都会调，参数由打开的地方给（如 `{"tab": "plugins"}`），
    /// 认不出的字段忽略。
    fn open(&mut self, args: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>);
}

/// 宿主拿着的页面：画出来，或再打开一次。
pub trait PageHandle {
    fn view(&self) -> AnyView;
    fn open(&self, args: &serde_json::Value, window: &mut Window, cx: &mut App);
}

impl<P: Page> PageHandle for Entity<P> {
    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn open(&self, args: &serde_json::Value, window: &mut Window, cx: &mut App) {
        self.update(cx, |page, cx| page.open(args, window, cx));
    }
}

/// 登记一页要给的东西。
#[derive(Clone)]
pub struct PageSpec {
    pub id: SharedString,
    pub title: SharedString,
    /// 图标的资源路径，如 `IconName::LayoutDashboard.path()`。
    pub icon: SharedString,
    /// 进侧栏「智能体」那一组，排在宿主自己的入口后面。
    pub sidebar: bool,
    /// 进侧栏每台机器的菜单。从那里打开时参数带 `{"machine": …}`
    /// （见 [`crate::machine_to_json`]）。
    pub machine_menu: bool,
    /// 第一次打开时建视图；之后一直用这一份，直到登记被替换或撤下。
    pub build: PageBuilder,
}

/// 建页面视图的函数。
pub type PageBuilder = Rc<dyn Fn(&mut Window, &mut App) -> Rc<dyn PageHandle>>;

struct Entry {
    spec: PageSpec,
    view: Option<Rc<dyn PageHandle>>,
}

#[derive(Default)]
struct PageRegistry {
    pages: Vec<Entry>,
}

impl Global for PageRegistry {}

/// 登记一页。同 id 的旧登记被替换，位置不变，旧视图放掉。
pub fn register_page(spec: PageSpec, cx: &mut App) -> Result<(), String> {
    if RESERVED_PAGE_IDS.contains(&spec.id.as_ref()) {
        return Err(format!("页面 id「{}」是宿主自己的路由，不能用", spec.id));
    }
    let registry = cx.default_global::<PageRegistry>();
    let entry = Entry { spec, view: None };
    match registry
        .pages
        .iter_mut()
        .find(|page| page.spec.id == entry.spec.id)
    {
        Some(page) => *page = entry,
        None => registry.pages.push(entry),
    }
    crate::emit(HostEvent::PagesChanged, cx);
    Ok(())
}

/// 撤下一页。没登记过就什么都不做。
pub fn unregister_page(id: &str, cx: &mut App) {
    if !cx.has_global::<PageRegistry>() {
        return;
    }
    let pages = &mut cx.global_mut::<PageRegistry>().pages;
    let Some(index) = pages.iter().position(|page| page.spec.id.as_ref() == id) else {
        return;
    };
    pages.remove(index);
    crate::emit(HostEvent::PagesChanged, cx);
}

/// 登记过的页面，按登记顺序。
pub fn pages(cx: &App) -> Vec<PageSpec> {
    cx.try_global::<PageRegistry>()
        .map(|registry| {
            registry
                .pages
                .iter()
                .map(|page| page.spec.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 按 id 找一页。
pub fn page(id: &str, cx: &App) -> Option<PageSpec> {
    entry(id, cx).map(|page| page.spec.clone())
}

/// 已经建好的页面视图。只读内存，render 里取它；没建过或没登记是 `None`。
pub fn page_view(id: &str, cx: &App) -> Option<Rc<dyn PageHandle>> {
    entry(id, cx)?.view.clone()
}

/// 取页面视图，没建过就现在建。第二项为 `true` 表示这次新建的，宿主随后要 `open` 它一次。
/// 没登记过是 `None`。在事件里调，不在 render 里。
pub fn ensure_page_view(
    id: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<(Rc<dyn PageHandle>, bool)> {
    let page = entry(id, cx)?;
    if let Some(view) = &page.view {
        return Some((view.clone(), false));
    }
    let build = page.spec.build.clone();
    let view = build(window, cx);
    // 建的时候插件可能刚好替换了这一页：只记到还是这份登记的那一项上。
    if let Some(page) = cx
        .global_mut::<PageRegistry>()
        .pages
        .iter_mut()
        .find(|page| page.spec.id.as_ref() == id && Rc::ptr_eq(&page.spec.build, &build))
    {
        page.view = Some(view.clone());
    }
    Some((view, true))
}

fn entry<'a>(id: &str, cx: &'a App) -> Option<&'a Entry> {
    cx.try_global::<PageRegistry>()?
        .pages
        .iter()
        .find(|page| page.spec.id.as_ref() == id)
}
