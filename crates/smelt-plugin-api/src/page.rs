//! 页面注册表：工作台舞台上能打开的整页。
//!
//! 页面按字符串 id 登记，宿主的路由只记 id，宿主里不出现页面的类型。同一个 id 只留最后
//! 登记的那份，用户插件可以借此替换内置页。

use std::rc::Rc;

use gpui::{AnyView, App, Context, Entity, Global, Render, SharedString, Window};
use gpui_component::IconName;

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
    pub icon: IconName,
    /// 进侧栏「智能体」那一组，排在宿主自己的入口后面。
    pub sidebar: bool,
    /// 进侧栏每台机器的菜单。从那里打开时参数带 `{"machine": …}`
    /// （见 [`crate::machine_to_json`]）。
    pub machine_menu: bool,
    /// 第一次打开时建视图；之后宿主一直留着它。
    pub build: PageBuilder,
}

/// 建页面视图的函数。
pub type PageBuilder = Rc<dyn Fn(&mut Window, &mut App) -> Rc<dyn PageHandle>>;

#[derive(Default)]
struct PageRegistry {
    pages: Vec<PageSpec>,
}

impl Global for PageRegistry {}

/// 登记一页。同 id 的旧登记被替换，位置不变。
pub fn register_page(spec: PageSpec, cx: &mut App) {
    let registry = cx.default_global::<PageRegistry>();
    match registry.pages.iter_mut().find(|page| page.id == spec.id) {
        Some(page) => *page = spec,
        None => registry.pages.push(spec),
    }
}

/// 登记过的页面，按登记顺序。
pub fn pages(cx: &App) -> Vec<PageSpec> {
    cx.try_global::<PageRegistry>()
        .map(|registry| registry.pages.clone())
        .unwrap_or_default()
}

/// 按 id 找一页。
pub fn page(id: &str, cx: &App) -> Option<PageSpec> {
    cx.try_global::<PageRegistry>()?
        .pages
        .iter()
        .find(|page| page.id.as_ref() == id)
        .cloned()
}
