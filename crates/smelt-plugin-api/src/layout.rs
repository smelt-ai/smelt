//! 布局注册表：插件能给的工作台摆法。
//!
//! 布局是 gpui-base 的 `DockAreaState` 原文，面板名就是视图 id（见 [`crate::view`]）。插件
//! 按来源整组登记，id 是 `<来源>/<名字>`，所以一个插件替换不了别人的布局。用哪份由用户在
//! 设置里选，宿主按选择装；选的那份没登记（插件还没加载、被卸了）时宿主先用别的，选择
//! 不改。

use gpui::{App, Global, SharedString};

use crate::HostEvent;

pub use gpui_component::dock::DockAreaState;

/// 布局和存档的 `version`。面板的数据结构不兼容地变了才加。
pub const LAYOUT_VERSION: usize = 1;

/// 按官方格式解析一份布局，`version` 要是 [`LAYOUT_VERSION`]。插件给的布局和宿主的存档共用。
pub fn parse_layout(text: &str) -> Result<DockAreaState, String> {
    let state: DockAreaState =
        serde_json::from_str(text).map_err(|error| format!("不是合法的布局：{error}"))?;
    match state.version {
        Some(LAYOUT_VERSION) => Ok(state),
        Some(version) => Err(format!(
            "`version` 是 {version}，这个版本只认 {LAYOUT_VERSION}"
        )),
        None => Err(format!("没写 `version`，要写 {LAYOUT_VERSION}")),
    }
}

/// 登记一份布局要给的东西。
#[derive(Clone, Debug)]
pub struct LayoutSource<'a> {
    /// 来源内的名字，不含 `/`。
    pub name: &'a str,
    /// 设置里给人看的名字。
    pub title: &'a str,
    /// `DockAreaState` 的 JSON 原文。
    pub state: &'a str,
}

/// 登记过的一份布局。
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutInfo {
    /// `<来源>/<名字>`。
    pub id: SharedString,
    pub title: SharedString,
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    owner: String,
    info: LayoutInfo,
    state: DockAreaState,
}

#[derive(Default)]
struct LayoutRegistry {
    layouts: Vec<Entry>,
}

impl Global for LayoutRegistry {}

/// 按来源整组登记布局：这个来源原来的布局全换成这一组，空的就是全撤下。一份不合法整组
/// 不登记，原来的不动。来源第一次登记排在最后，再登记留在原来的位置。
pub fn register_layouts(
    owner: &str,
    sources: &[LayoutSource<'_>],
    cx: &mut App,
) -> Result<(), String> {
    if owner.is_empty() || owner.contains('/') {
        return Err(format!("布局来源「{owner}」不能为空，也不能含 `/`"));
    }
    let mut entries = Vec::with_capacity(sources.len());
    for source in sources {
        if source.name.is_empty() || source.name.contains('/') {
            return Err(format!("布局名「{}」不能为空，也不能含 `/`", source.name));
        }
        let id: SharedString = format!("{owner}/{}", source.name).into();
        if entries.iter().any(|entry: &Entry| entry.info.id == id) {
            return Err(format!("布局「{id}」登记了两次"));
        }
        let state = parse_layout(source.state).map_err(|error| format!("布局「{id}」{error}"))?;
        entries.push(Entry {
            owner: owner.to_string(),
            info: LayoutInfo {
                id,
                title: source.title.to_string().into(),
            },
            state,
        });
    }
    let layouts = &mut cx.default_global::<LayoutRegistry>().layouts;
    let at = layouts
        .iter()
        .position(|entry| entry.owner == owner)
        .unwrap_or(layouts.len());
    let old: Vec<Entry> = layouts
        .iter()
        .filter(|entry| entry.owner == owner)
        .cloned()
        .collect();
    if old == entries {
        return Ok(());
    }
    layouts.retain(|entry| entry.owner != owner);
    let at = at.min(layouts.len());
    layouts.splice(at..at, entries);
    crate::emit(HostEvent::LayoutsChanged, cx);
    Ok(())
}

/// 撤下一个来源的全部布局。
pub fn unregister_layouts(owner: &str, cx: &mut App) {
    if !cx.has_global::<LayoutRegistry>() {
        return;
    }
    let layouts = &mut cx.global_mut::<LayoutRegistry>().layouts;
    let before = layouts.len();
    layouts.retain(|entry| entry.owner != owner);
    if layouts.len() != before {
        crate::emit(HostEvent::LayoutsChanged, cx);
    }
}

/// 登记过的布局，按登记顺序。只读内存，render 里可以调。
pub fn layouts(cx: &App) -> Vec<LayoutInfo> {
    cx.try_global::<LayoutRegistry>()
        .map(|registry| {
            registry
                .layouts
                .iter()
                .map(|entry| entry.info.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 一份登记过的布局本来的样子；没登记是 `None`。
pub fn layout(id: &str, cx: &App) -> Option<DockAreaState> {
    cx.try_global::<LayoutRegistry>()?
        .layouts
        .iter()
        .find(|entry| entry.info.id.as_ref() == id)
        .map(|entry| entry.state.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = r#"{"version":1,"center":{"panel_name":"TabPanel","info":{"tabs":{"active_index":0}},"children":[{"panel_name":"stage","children":[],"info":{"panel":null}}]}}"#;
    const TWO: &str = r#"{"version":1,"center":{"panel_name":"TabPanel","info":{"tabs":{"active_index":0}},"children":[{"panel_name":"files","children":[],"info":{"panel":null}}]}}"#;

    fn source<'a>(name: &'a str, state: &'a str) -> LayoutSource<'a> {
        LayoutSource {
            name,
            title: name,
            state,
        }
    }

    fn ids(cx: &App) -> Vec<String> {
        layouts(cx).into_iter().map(|info| info.id.into()).collect()
    }

    #[gpui::test]
    fn layouts_are_registered_per_owner_and_keep_their_place(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_layouts("smelt", &[source("default", ONE)], cx).unwrap();
            register_layouts("trader", &[source("a", ONE), source("b", TWO)], cx).unwrap();
            assert_eq!(ids(cx), ["smelt/default", "trader/a", "trader/b"]);
            // 再登记是整组替换，位置不变。
            register_layouts("smelt", &[source("wide", TWO)], cx).unwrap();
            assert_eq!(ids(cx), ["smelt/wide", "trader/a", "trader/b"]);
            assert_eq!(layout("smelt/wide", cx), Some(parse_layout(TWO).unwrap()));
            assert_eq!(layout("smelt/default", cx), None);
            unregister_layouts("trader", cx);
            assert_eq!(ids(cx), ["smelt/wide"]);
            register_layouts("smelt", &[], cx).unwrap();
            assert!(layouts(cx).is_empty());
        });
    }

    #[gpui::test]
    fn an_invalid_layout_rejects_the_whole_set(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_layouts("trader", &[source("a", ONE)], cx).unwrap();
            let newer = ONE.replace(r#""version":1"#, r#""version":2"#);
            for bad in [
                vec![source("b", TWO), source("c", "{}")],
                vec![source("b", &newer)],
                vec![source("b", TWO), source("b", ONE)],
                vec![source("", ONE)],
                vec![source("x/y", ONE)],
            ] {
                assert!(register_layouts("trader", &bad, cx).is_err());
            }
            assert!(register_layouts("a/b", &[source("a", ONE)], cx).is_err());
            assert_eq!(ids(cx), ["trader/a"]);
        });
    }
}
