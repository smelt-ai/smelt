//! 布局注册表：一页（见 [`crate::page`]）怎么摆。
//!
//! 布局是 gpui-base 的 `DockAreaState` 原文，面板名就是视图 id（见 [`crate::view`]）。每份
//! 布局说明自己给哪一页，一页可以有好几份，别的插件也能给不是自己的页带布局。插件按来源
//! 整组登记，id 是 `<来源>/<名字>`，所以一个插件替换不了别人的布局。首页用哪份由用户在
//! 设置里选，别的页用给它登记的第一份；选的那份没登记（插件还没加载、被卸了）时宿主先用
//! 别的，选择不改。
//!
//! 除了视图，叶子还可以是一摞（[`STACK_PANEL`]）：几个视图按顺序排在一格里，见 [`Stack`]。

use gpui::{App, Global, SharedString};
use serde::{Deserialize, Serialize};

use crate::HostEvent;

pub use gpui_base::dock::{DockAreaState, PanelInfo, PanelState};

/// 一摞：Dock 里的一块面板，`info.panel` 是一份 [`Stack`]，把几个视图按顺序排在一格里。
/// 各项之间没有分隔条、拖不动，按内容定高（`fit`）的那几项之外平分剩下的地方；不分标签，
/// 不能单独放大，也不能把标签拖进拖出。适合侧栏这种上中下固定几块的地方。
///
/// 里面的视图不画外壳的标题带（顶上那条归它们自己，见 [`crate::ViewSpec::draws_top_bar`]），
/// 宿主按各自碰到这一格的哪几条边分 safe area（[`crate::safe_area`]）。出口、设置页、
/// 按实例建的视图不能放进来。
pub const STACK_PANEL: &str = "smelt/stack";

/// 一摞的内容。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    pub axis: StackAxis,
    pub items: Vec<StackItem>,
}

/// 一摞朝哪个方向排。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StackAxis {
    Horizontal,
    Vertical,
}

/// 一摞里的一项：一个视图（`{"view": "<视图 id>"}`），或者换个方向再排一摞
/// （`{"stack": {...}}`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StackItem {
    #[serde(flatten)]
    pub content: StackContent,
    /// 沿排的方向按内容定大小，不分剩下的地方。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fit: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StackContent {
    View(String),
    Stack(Stack),
}

impl Stack {
    /// 面板是一摞就读出来；不是一摞是 `None`，是一摞但写得不对是错误。
    pub fn of(panel: &PanelState) -> Option<Result<Stack, String>> {
        if panel.panel_name != STACK_PANEL {
            return None;
        }
        Some(match &panel.info {
            PanelInfo::Panel(info) if panel.children.is_empty() => {
                serde_json::from_value::<Stack>(info.clone())
                    .map_err(|error| format!("`{STACK_PANEL}` 写得不对：{error}"))
                    .and_then(|stack| stack.check().map(|()| stack))
            }
            _ => Err(format!("`{STACK_PANEL}` 是叶子，内容写在 `info.panel` 里")),
        })
    }

    /// 里面的视图，按先后。
    pub fn views(&self) -> Vec<&str> {
        let mut views = Vec::new();
        self.walk(&mut |id| views.push(id));
        views
    }

    fn walk<'a>(&'a self, f: &mut impl FnMut(&'a str)) {
        for item in &self.items {
            match &item.content {
                StackContent::View(id) => f(id),
                StackContent::Stack(stack) => stack.walk(f),
            }
        }
    }

    fn check(&self) -> Result<(), String> {
        if self.items.is_empty() {
            return Err(format!("`{STACK_PANEL}` 里至少要有一项"));
        }
        for item in &self.items {
            match &item.content {
                StackContent::View(id) if id == crate::OUTLET_VIEW => {
                    return Err(format!("出口不能放进 `{STACK_PANEL}`"));
                }
                StackContent::View(_) => {}
                StackContent::Stack(stack) => stack.check()?,
            }
        }
        Ok(())
    }
}

/// 布局里的每一块面板（框架和叶子），先序。
fn panels(state: &DockAreaState) -> Vec<&PanelState> {
    fn walk<'a>(panel: &'a PanelState, out: &mut Vec<&'a PanelState>) {
        out.push(panel);
        panel.children.iter().for_each(|child| walk(child, out));
    }
    let mut out = Vec::new();
    walk(&state.center, &mut out);
    for dock in [&state.left_dock, &state.right_dock, &state.bottom_dock]
        .into_iter()
        .flatten()
    {
        walk(dock.panel(), &mut out);
    }
    out
}

/// 一份布局里的摞（[`STACK_PANEL`]），按先后。写得不对的跳过（[`parse_layout`] 不收这种
/// 布局，存档里有就当它是别的面板）。
pub fn layout_stacks(state: &DockAreaState) -> Vec<Stack> {
    panels(state)
        .into_iter()
        .filter_map(|panel| Stack::of(panel)?.ok())
        .collect()
}

/// 布局和存档的 `version`。面板的数据结构不兼容地变了才加。
pub const LAYOUT_VERSION: usize = 1;

/// 按官方格式解析一份布局，`version` 要是 [`LAYOUT_VERSION`]。插件给的布局和宿主的存档共用。
pub fn parse_layout(text: &str) -> Result<DockAreaState, String> {
    let state: DockAreaState =
        serde_json::from_str(text).map_err(|error| format!("不是合法的布局：{error}"))?;
    match state.version {
        Some(LAYOUT_VERSION) => {}
        Some(version) => {
            return Err(format!(
                "`version` 是 {version}，这个版本只认 {LAYOUT_VERSION}"
            ));
        }
        None => return Err(format!("没写 `version`，要写 {LAYOUT_VERSION}")),
    }
    for panel in panels(&state) {
        if let Some(Err(error)) = Stack::of(panel) {
            return Err(error);
        }
    }
    Ok(state)
}

/// 登记一份布局要给的东西。
#[derive(Clone, Debug)]
pub struct LayoutSource<'a> {
    /// 给哪一页，页 id `<来源>/<名字>`。页不必已经登记：登记上了就用得上。
    pub page: &'a str,
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
    /// 给哪一页。
    pub page: SharedString,
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
        if source
            .page
            .split_once('/')
            .is_none_or(|(owner, name)| owner.is_empty() || name.is_empty() || name.contains('/'))
        {
            return Err(format!(
                "布局「{id}」给的页「{}」不是 `<来源>/<名字>`",
                source.page
            ));
        }
        let state = parse_layout(source.state).map_err(|error| format!("布局「{id}」{error}"))?;
        entries.push(Entry {
            owner: owner.to_string(),
            info: LayoutInfo {
                id,
                page: source.page.to_string().into(),
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
            page: "smelt/work",
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
                vec![LayoutSource {
                    page: "work",
                    ..source("b", TWO)
                }],
            ] {
                assert!(register_layouts("trader", &bad, cx).is_err());
            }
            assert!(register_layouts("a/b", &[source("a", ONE)], cx).is_err());
            assert_eq!(ids(cx), ["trader/a"]);
        });
    }

    fn with_center_leaf(leaf: &str) -> String {
        format!(
            r#"{{"version":1,"center":{{"panel_name":"TabPanel","info":{{"tabs":{{"active_index":0}}}},"children":[{leaf}]}}}}"#
        )
    }

    #[test]
    fn a_stack_lists_its_views_in_order() {
        let text = with_center_leaf(
            r#"{"panel_name":"smelt/stack","children":[],"info":{"panel":{"axis":"vertical","items":[
                {"view":"a/nav","fit":true},
                {"view":"b/list"},
                {"stack":{"axis":"horizontal","items":[{"view":"a/settings"},{"view":"c/quota","fit":true}]},"fit":true}
            ]}}}"#,
        );
        let state = parse_layout(&text).unwrap();
        let stacks = layout_stacks(&state);
        assert_eq!(stacks.len(), 1);
        assert_eq!(
            stacks[0].views(),
            ["a/nav", "b/list", "a/settings", "c/quota"]
        );
        assert_eq!(stacks[0].axis, StackAxis::Vertical);
        assert!(stacks[0].items[0].fit && !stacks[0].items[1].fit);
        // 原样写回：`fit` 为假的不写。
        let again = serde_json::to_value(&stacks[0]).unwrap();
        assert_eq!(again["items"][1], serde_json::json!({ "view": "b/list" }));
    }

    #[test]
    fn a_bad_stack_rejects_the_layout() {
        for info in [
            r#"{"axis":"vertical","items":[]}"#,
            r#"{"axis":"diagonal","items":[{"view":"a/b"}]}"#,
            r#"{"axis":"vertical","items":[{"view":"smelt/outlet"}]}"#,
            r#"{"axis":"vertical","items":[{"stack":{"axis":"horizontal","items":[]}}]}"#,
            r#"{"axis":"vertical","items":[{"view":"a/b"}],"sizes":[1]}"#,
            "null",
        ] {
            let text = with_center_leaf(&format!(
                r#"{{"panel_name":"smelt/stack","children":[],"info":{{"panel":{info}}}}}"#
            ));
            assert!(parse_layout(&text).is_err(), "{info}");
        }
    }
}
