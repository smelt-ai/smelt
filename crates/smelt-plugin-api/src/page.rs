//! 页面注册表：app 能路由到的页。
//!
//! 整个主窗口一次只显示一页，每页有自己的布局（见 [`crate::layout`]）：工作台一页三栏，
//! 设置一页两栏。页长什么样由登记给它的布局定，页这里只有名字、是不是首页、出口里默认放
//! 什么。
//!
//! 路由是嵌套的：页是外层，布局里可以摆一个出口（[`OUTLET_VIEW`]），出口是这页里换内容的
//! 地方，布局的其余部分（侧栏等）留着不动。打开一个视图（[`crate::OPEN_VIEW`]）：哪页的
//! 布局里摆了它就去那页让它露出来；哪页都没摆就放进出口（当前页有出口放这页，没有就去首页
//! 的出口）。出口没放东西时显示这页的默认内容（[`PageSource::index`]）。换页用
//! [`OPEN_PAGE`]，页头的关闭回到来的那页；同一页里换出口的内容不算来路。
//!
//! 和视图、布局一样按来源整组登记，id 是 `<来源>/<名字>`。启动时进首页：登记顺序里第一个
//! 说自己是首页的；一个都没有时宿主把所有视图摆在一页里兜底。

use gpui::{App, Global, SharedString};
use serde::{Deserialize, Serialize};

use crate::{Command, HostEvent};

/// 出口：宿主提供的视图，布局里摆它的地方显示路由到的视图。
pub const OUTLET_VIEW: &str = "smelt/outlet";

/// 进一页：已经在这页什么都不做；来路上有它就退回去，不绕圈。没有这页、这页没有布局时
/// 结果是 `false`。
pub const OPEN_PAGE: Command<OpenPage, bool> = Command::new("smelt.app/open-page");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPage {
    /// 页 id，`<来源>/<名字>`。
    pub page: String,
}

/// 登记一页要给的东西。
#[derive(Clone, Debug)]
pub struct PageSource<'a> {
    /// 来源内的名字，不含 `/`。
    pub name: &'a str,
    /// 给人看的名字。
    pub title: &'a str,
    /// 启动时进这一页。好几个来源都说是首页时，登记在前的算。
    pub home: bool,
    /// 出口没放东西时显示的视图 id，如工作台的会话舞台。布局里没摆出口的页用不上。
    pub index: Option<&'a str>,
}

/// 登记过的一页。
#[derive(Clone, Debug, PartialEq)]
pub struct PageInfo {
    /// `<来源>/<名字>`。
    pub id: SharedString,
    pub title: SharedString,
    pub home: bool,
    pub index: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    owner: String,
    info: PageInfo,
}

#[derive(Default)]
struct PageRegistry {
    pages: Vec<Entry>,
}

impl Global for PageRegistry {}

/// 按来源整组登记页：这个来源原来的页全换成这一组，空的就是全撤下。一页不合法整组不
/// 登记，原来的不动。来源第一次登记排在最后，再登记留在原来的位置。
pub fn register_pages(owner: &str, sources: &[PageSource<'_>], cx: &mut App) -> Result<(), String> {
    if owner.is_empty() || owner.contains('/') {
        return Err(format!("页的来源「{owner}」不能为空，也不能含 `/`"));
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(sources.len());
    for source in sources {
        if source.name.is_empty() || source.name.contains('/') {
            return Err(format!("页名「{}」不能为空，也不能含 `/`", source.name));
        }
        let id: SharedString = format!("{owner}/{}", source.name).into();
        if entries.iter().any(|entry| entry.info.id == id) {
            return Err(format!("页「{id}」登记了两次"));
        }
        if source.title.trim().is_empty() {
            return Err(format!("页「{id}」没有标题"));
        }
        if let Some(index) = source.index
            && !index
                .split_once('/')
                .is_some_and(|(owner, name)| !owner.is_empty() && !name.is_empty())
        {
            return Err(format!("页「{id}」的默认内容「{index}」不是视图 id"));
        }
        entries.push(Entry {
            owner: owner.to_string(),
            info: PageInfo {
                id,
                title: source.title.to_string().into(),
                home: source.home,
                index: source.index.map(|index| index.to_string().into()),
            },
        });
    }
    let pages = &mut cx.default_global::<PageRegistry>().pages;
    let at = pages
        .iter()
        .position(|entry| entry.owner == owner)
        .unwrap_or(pages.len());
    let old: Vec<Entry> = pages
        .iter()
        .filter(|entry| entry.owner == owner)
        .cloned()
        .collect();
    if old == entries {
        return Ok(());
    }
    pages.retain(|entry| entry.owner != owner);
    let at = at.min(pages.len());
    pages.splice(at..at, entries);
    crate::emit(HostEvent::PagesChanged, cx);
    Ok(())
}

/// 撤下一个来源的全部页。
pub fn unregister_pages(owner: &str, cx: &mut App) {
    if !cx.has_global::<PageRegistry>() {
        return;
    }
    let pages = &mut cx.global_mut::<PageRegistry>().pages;
    let before = pages.len();
    pages.retain(|entry| entry.owner != owner);
    if pages.len() != before {
        crate::emit(HostEvent::PagesChanged, cx);
    }
}

/// 登记过的页，按登记顺序。只读内存，render 里可以调。
pub fn pages(cx: &App) -> Vec<PageInfo> {
    cx.try_global::<PageRegistry>()
        .map(|registry| {
            registry
                .pages
                .iter()
                .map(|entry| entry.info.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 一页。只读内存。
pub fn page(id: &str, cx: &App) -> Option<PageInfo> {
    cx.try_global::<PageRegistry>()?
        .pages
        .iter()
        .find(|entry| entry.info.id.as_ref() == id)
        .map(|entry| entry.info.clone())
}

/// 首页：登记顺序里第一个说自己是首页的。
pub fn home_page(cx: &App) -> Option<PageInfo> {
    cx.try_global::<PageRegistry>()?
        .pages
        .iter()
        .find(|entry| entry.info.home)
        .map(|entry| entry.info.clone())
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn source(name: &'static str, home: bool) -> PageSource<'static> {
        PageSource {
            name,
            title: name,
            home,
            index: None,
        }
    }

    fn ids(cx: &App) -> Vec<String> {
        pages(cx)
            .into_iter()
            .map(|page| page.id.to_string())
            .collect()
    }

    /// 整组替换，位置留着；不合法的整组不登记。
    #[gpui::test]
    fn pages_are_replaced_per_owner(cx: &mut TestAppContext) {
        cx.update(|cx| {
            register_pages("a", &[source("one", false)], cx).unwrap();
            register_pages("b", &[source("two", false)], cx).unwrap();
            register_pages("a", &[source("three", false)], cx).unwrap();
            assert_eq!(ids(cx), ["a/three", "b/two"]);

            assert!(register_pages("a", &[source("x/y", false)], cx).is_err());
            assert!(
                register_pages("a", &[source("same", false), source("same", false)], cx).is_err()
            );
            let bad_index = PageSource {
                index: Some("stage"),
                ..source("four", false)
            };
            assert!(register_pages("a", &[bad_index], cx).is_err());
            assert_eq!(ids(cx), ["a/three", "b/two"], "不合法的整组不登记");

            unregister_pages("a", cx);
            assert_eq!(ids(cx), ["b/two"]);
        });
    }

    /// 首页是登记在前的那个；没有说自己是首页的就没有。
    #[gpui::test]
    fn the_first_home_wins(cx: &mut TestAppContext) {
        cx.update(|cx| {
            register_pages("host", &[source("settings", false)], cx).unwrap();
            assert_eq!(home_page(cx), None);
            register_pages("x", &[source("work", true)], cx).unwrap();
            register_pages("y", &[source("other", true)], cx).unwrap();
            assert_eq!(
                home_page(cx).map(|page| page.id.to_string()).as_deref(),
                Some("x/work")
            );
        });
    }
}
