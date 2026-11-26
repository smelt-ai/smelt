//! 布局注册表：一页（见 [`crate::page`]）怎么摆。
//!
//! 布局是 [`DockAreaState`] 原文，面板名就是视图 id（见 [`crate::view`]）。每份
//! 布局说明自己给哪一页，一页可以有好几份，别的插件也能给不是自己的页带布局。插件按来源
//! 整组登记，id 是 `<来源>/<名字>`，所以一个插件替换不了别人的布局。首页用哪份由用户在
//! 设置里选，别的页用给它登记的第一份；选的那份没登记（插件还没加载、被卸了）时宿主先用
//! 别的，选择不改。
//!
//! 除了视图，叶子还可以是一摞（[`STACK_PANEL`]）：几个视图按顺序排在一格里，见 [`Stack`]。
//!
//! 布局还说各块地方（左、右、下停靠区和中间）垫什么材质（[`LayoutMaterials`]），没说的铺实色。
//! 材质跟着地方走，不跟视图：把视图拖到别处，透不透看它落在哪。

use gpui::{App, Axis, Global, Pixels, SharedString};
use serde::{Deserialize, Serialize};

use crate::{HostEvent, Text};

/// 布局与存档的序列化结构。与 NSH `<DockArea>` 及既有落盘 JSON 格式完全同构，字段名和
/// serde tag 为持久化契约，不可擅改。
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DockAreaState {
    #[serde(default)]
    pub version: Option<usize>,
    pub center: PanelState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_dock: Option<DockState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_dock: Option<DockState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottom_dock: Option<DockState>,
}

/// 单个停靠区的持久化状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DockState {
    panel: PanelState,
    placement: DockPlacement,
    size: Pixels,
    open: bool,
}

impl DockState {
    pub fn new(panel: PanelState, placement: DockPlacement, size: Pixels, open: bool) -> Self {
        Self {
            panel,
            placement,
            size,
            open,
        }
    }

    pub fn panel(&self) -> &PanelState {
        &self.panel
    }

    pub fn placement(&self) -> DockPlacement {
        self.placement
    }

    pub fn size(&self) -> Pixels {
        self.size
    }

    pub fn open(&self) -> bool {
        self.open
    }
}

/// 布局树节点（框架或叶子面板）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PanelState {
    pub panel_name: String,
    pub children: Vec<PanelState>,
    pub info: PanelInfo,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            panel_name: String::new(),
            children: Vec::new(),
            info: PanelInfo::Panel(serde_json::Value::Null),
        }
    }
}

impl PanelState {
    pub fn new(panel_name: impl Into<String>) -> Self {
        Self {
            panel_name: panel_name.into(),
            ..Default::default()
        }
    }

    pub fn add_child(&mut self, panel: PanelState) {
        self.children.push(panel);
    }
}

/// 节点附加信息：`stack`（分栏）、`tabs`（标签页组）或 `panel`（叶子数据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PanelInfo {
    #[serde(rename = "stack")]
    Stack { sizes: Vec<Pixels>, axis: usize },
    #[serde(rename = "tabs")]
    Tabs { active_index: usize },
    #[serde(rename = "panel")]
    Panel(serde_json::Value),
}

impl PanelInfo {
    pub fn stack(sizes: Vec<Pixels>, axis: Axis) -> Self {
        Self::Stack {
            sizes,
            axis: if axis == Axis::Horizontal { 0 } else { 1 },
        }
    }

    pub fn tabs(active_index: usize) -> Self {
        Self::Tabs { active_index }
    }

    pub fn panel(info: serde_json::Value) -> Self {
        Self::Panel(info)
    }

    pub fn axis(&self) -> Option<Axis> {
        match self {
            Self::Stack { axis, .. } => Some(if *axis == 0 {
                Axis::Horizontal
            } else {
                Axis::Vertical
            }),
            _ => None,
        }
    }

    pub fn sizes(&self) -> Option<&Vec<Pixels>> {
        match self {
            Self::Stack { sizes, .. } => Some(sizes),
            _ => None,
        }
    }

    pub fn active_index(&self) -> Option<usize> {
        match self {
            Self::Tabs { active_index } => Some(*active_index),
            _ => None,
        }
    }
}

/// 停靠区相对主区域的位置。serde tag 为持久化契约，不可擅改。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DockPlacement {
    #[serde(rename = "center")]
    Center,
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "bottom")]
    Bottom,
    #[serde(rename = "right")]
    Right,
}

impl DockPlacement {
    pub fn axis(&self) -> Axis {
        match self {
            Self::Left | Self::Right | Self::Center => Axis::Horizontal,
            Self::Bottom => Axis::Vertical,
        }
    }

    pub fn is_left(&self) -> bool {
        matches!(self, Self::Left)
    }

    pub fn is_bottom(&self) -> bool {
        matches!(self, Self::Bottom)
    }

    pub fn is_right(&self) -> bool {
        matches!(self, Self::Right)
    }
}

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

/// 一块地方垫的系统材质，按这块地方在窗口里的用途说，宿主按平台换成具体的材质：macOS 上
/// 各块各垫各的，Windows 上整扇窗口只有一种系统材质（Mica），声明了的地方都透它。主题底色按
/// 外观里的不透明度铺在材质上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Material {
    /// 导航侧栏：透得最明显，带桌面的颜色（macOS 的侧栏材质）。
    Sidebar,
    /// 窗口底：几乎不透，只隐约带一点桌面的颜色（macOS 的窗口底材质）。检查器、辅助栏这类
    /// 次要但常看的地方。
    Window,
}

impl Material {
    pub const ALL: [Self; 2] = [Self::Sidebar, Self::Window];

    /// JS 和布局说明里的名字。
    pub const fn key(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Window => "window",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|material| material.key() == key)
    }
}

/// 布局里各块地方垫什么材质。没写的（`None`）不透，铺实的主题底色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutMaterials {
    pub left: Option<Material>,
    pub right: Option<Material>,
    pub bottom: Option<Material>,
    pub center: Option<Material>,
}

impl LayoutMaterials {
    /// 这块地方垫什么。
    pub fn get(&self, placement: DockPlacement) -> Option<Material> {
        match placement {
            DockPlacement::Left => self.left,
            DockPlacement::Right => self.right,
            DockPlacement::Bottom => self.bottom,
            DockPlacement::Center => self.center,
        }
    }
}

/// 登记一份布局要给的东西。
#[derive(Clone, Debug)]
pub struct LayoutSource<'a> {
    /// 给哪一页，页 id `<来源>/<名字>`。页不必已经登记：登记上了就用得上。
    pub page: &'a str,
    /// 来源内的名字，不含 `/`。
    pub name: &'a str,
    /// 设置里给人看的名字，跟着界面语言变（见 [`crate::Text`]）。
    pub title: Text,
    /// `DockAreaState` 的 JSON 原文。
    pub state: &'a str,
    /// 各块地方垫什么材质。
    pub materials: LayoutMaterials,
}

/// 登记过的一份布局。标题跟着界面语言变，显示时 [`Text::get`]，存着它的不用在换语言时
/// 重取。
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutInfo {
    /// `<来源>/<名字>`。
    pub id: SharedString,
    /// 给哪一页。
    pub page: SharedString,
    pub title: Text,
    pub materials: LayoutMaterials,
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    owner: String,
    info: LayoutInfo,
    state: DockAreaState,
}

impl Entry {
    fn info(&self) -> LayoutInfo {
        self.info.clone()
    }
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
                title: source.title.clone(),
                materials: source.materials,
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
        .map(|registry| registry.layouts.iter().map(Entry::info).collect())
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
            title: name.to_string().into(),
            state,
            materials: LayoutMaterials::default(),
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
            // 材质跟着登记信息走，不进布局原文。
            let glass = LayoutMaterials {
                left: Some(Material::Sidebar),
                ..LayoutMaterials::default()
            };
            register_layouts(
                "smelt",
                &[LayoutSource {
                    materials: glass,
                    ..source("wide", TWO)
                }],
                cx,
            )
            .unwrap();
            assert_eq!(layouts(cx)[0].materials, glass);
            assert_eq!(glass.get(DockPlacement::Left), Some(Material::Sidebar));
            assert_eq!(glass.get(DockPlacement::Center), None);
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

    #[test]
    fn dock_state_serde_contract_is_preserved() {
        use gpui::px;

        let stack = serde_json::to_value(PanelInfo::stack(vec![px(1.)], Axis::Vertical)).unwrap();
        assert_eq!(
            stack,
            serde_json::json!({ "stack": { "sizes": [1.0], "axis": 1 } })
        );
        let tabs = serde_json::to_value(PanelInfo::tabs(2)).unwrap();
        assert_eq!(tabs, serde_json::json!({ "tabs": { "active_index": 2 } }));
        assert_eq!(
            serde_json::to_value(DockPlacement::Bottom).unwrap(),
            serde_json::json!("bottom")
        );

        let empty = serde_json::to_value(DockAreaState::default()).unwrap();
        assert!(empty.get("left_dock").is_none());
        assert!(empty.get("right_dock").is_none());
        assert!(empty.get("bottom_dock").is_none());

        let mut dock_area = parse_layout(ONE).unwrap();
        dock_area.left_dock = Some(DockState::new(
            PanelState::new("nav"),
            DockPlacement::Left,
            px(240.),
            true,
        ));
        let json = serde_json::to_string(&dock_area).unwrap();
        let again = parse_layout(&json).unwrap();
        assert_eq!(again, dock_area);
        let left = again.left_dock.unwrap();
        assert_eq!(left.placement(), DockPlacement::Left);
        assert_eq!(left.size(), px(240.));
        assert!(left.open());
        assert_eq!(left.panel().panel_name, "nav");
    }
}
