//! NSH 主壳的布局模型：在 `smelt_plugin_api::DockAreaState`（持久化格式）与 `<DockArea>`
//! 受控布局 JSON 之间无损互转，并计算各组和各视图的 `SafeArea`、可见集合与面板选项。
//!
//! 规则与 `crates/smelt/src/workspace_shell.rs` 保持一致：
//! - 布局存档沿用同一份 `DockAreaState` JSON（版本、面板树、停靠区开合和尺寸完全兼容）；
//! - 未登记或重复摆放的视图转为不可见占位项（`visible: false`），写回存档时原样还原；
//! - `smelt/stack` 摞保留其 `Stack` 树与原始 `info`，写回存档时原样还原，并把 `SafeArea` 按边分给摞内各项；
//! - 出口（`smelt/outlet`）的标题、角标、是否自己画顶栏跟随当前显示在出口里的视图。

use std::collections::{BTreeMap, HashSet};

use gpui::{App, Edges, Pixels, px, size};
use nsh_base::dock::{
    DockAreaLayout, DockAreaState as NshDockAreaState, DockNode, DockPlacement as NshPlacement,
    DockRegion, DockState as NshDockState, PanelBuilder, PanelId, PanelInfo as NshPanelInfo,
    PanelState as NshPanelState,
};
use nsh_core::geometry::{Axis as NshAxis, Points};
use serde_json::{Value, json};
use smelt_plugin_api as api;
use smelt_ui::window_bar::{BAR_MARGIN, SafeArea, TOP_BAR_HEIGHT, WINDOW_EDGE_MARGIN};

/// 左右停靠区开合按钮宽度（与 `workspace_shell.rs` 一致）。
pub const DOCK_TOGGLE_WIDTH: f32 = 20.;

/// 窗口自身占掉的角（红黄绿灯宽度、底角圆角）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowCorners {
    pub lights: Pixels,
    pub rounded: Pixels,
}

/// 与 `nsh_base::dock::DockAreaLayout` 同构的纯数据树，直接保留 `u64` 节点 id 以便跨 JS 无损往返。
#[derive(Clone, Debug, PartialEq)]
pub struct DockTreeLayout {
    pub center: DockTreeNode,
    pub left: Option<DockTreeRegion>,
    pub right: Option<DockTreeRegion>,
    pub bottom: Option<DockTreeRegion>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DockTreeRegion {
    pub layout: DockTreeNode,
    pub size: Option<f32>,
    pub open: Option<bool>,
    pub collapsible: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DockTreeNode {
    Split {
        id: Option<u64>,
        axis: NshAxis,
        children: Vec<DockTreeNode>,
        sizes: Vec<Option<f32>>,
    },
    Tabs {
        id: Option<u64>,
        panels: Vec<String>,
        active_index: usize,
    },
}

impl DockTreeLayout {
    fn from_nsh(layout: &DockAreaLayout) -> Self {
        Self {
            center: DockTreeNode::from_nsh(&layout.center),
            left: layout.left.as_ref().map(DockTreeRegion::from_nsh),
            right: layout.right.as_ref().map(DockTreeRegion::from_nsh),
            bottom: layout.bottom.as_ref().map(DockTreeRegion::from_nsh),
        }
    }

    fn dock(&self, placement: NshPlacement) -> Option<&DockTreeRegion> {
        match placement {
            NshPlacement::Left => self.left.as_ref(),
            NshPlacement::Right => self.right.as_ref(),
            NshPlacement::Bottom => self.bottom.as_ref(),
            NshPlacement::Center => None,
        }
    }
}

impl DockTreeRegion {
    fn from_nsh(region: &DockRegion) -> Self {
        Self {
            layout: DockTreeNode::from_nsh(&region.layout),
            size: region.size.map(|p| p.0),
            open: region.open,
            collapsible: region.collapsible,
        }
    }
}

impl DockTreeNode {
    fn from_nsh(node: &DockNode) -> Self {
        match node {
            DockNode::Split {
                id,
                axis,
                children,
                sizes,
            } => Self::Split {
                id: id.map(|id| id.as_u64()),
                axis: *axis,
                children: children.iter().map(Self::from_nsh).collect(),
                sizes: sizes.iter().map(|s| s.map(|p| p.0)).collect(),
            },
            DockNode::Tabs {
                id,
                panels,
                active_index,
            } => Self::Tabs {
                id: id.map(|id| id.as_u64()),
                panels: panels.iter().map(|p| p.as_str().to_owned()).collect(),
                active_index: *active_index,
            },
        }
    }
}

/// 一块面板叶子对应的原始语义，用于渲染和无损写回存档。
#[derive(Clone, Debug)]
pub enum LeafMeta {
    /// 正常登记的视图。
    View { id: String, info: Value },
    /// `smelt/stack` 一摞（只包含已登记且不重复的子视图，存档保留原始 `info`）。
    Stack { stack: api::Stack, info: Value },
    /// 未登记或重复摆放的占位（隐藏不画，存档原样写回）。
    Placeholder { original: NshPanelState },
}

/// 一页的受控 Dock 状态。
#[derive(Clone, Debug)]
pub struct PageLayout {
    pub layout: DockTreeLayout,
    pub leaves: BTreeMap<String, LeafMeta>,
    pub zoomed: Option<u64>,
}

/// 由 [`PageLayout`] 算出的当前页快照（渲染与安全区来源）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComputedShellView {
    pub laid_out: HashSet<String>,
    pub shown: HashSet<String>,
    pub areas: BTreeMap<String, SafeArea>,
    pub groups: BTreeMap<u64, SafeArea>,
    pub left_dock: Option<bool>,
    pub right_dock: Option<bool>,
    pub outlet: Option<String>,
    pub corners: WindowCorners,
}

impl ComputedShellView {
    pub fn panel_of<'a>(&self, id: &'a str) -> &'a str {
        if self.outlet.as_deref() == Some(id) {
            api::OUTLET_VIEW
        } else {
            id
        }
    }

    pub fn safe_area_of(&self, id: &str) -> SafeArea {
        self.areas
            .get(self.panel_of(id))
            .copied()
            .unwrap_or_default()
    }

    pub fn left_toggle_x(&self) -> f32 {
        f32::from(self.corners.lights.max(WINDOW_EDGE_MARGIN))
    }

    pub fn leading_inset(&self) -> Pixels {
        match self.left_dock {
            Some(_) => px(self.left_toggle_x()) + px(DOCK_TOGGLE_WIDTH) + BAR_MARGIN,
            None => self.corners.lights,
        }
    }

    pub fn trailing_inset(&self) -> Pixels {
        match self.right_dock {
            Some(_) => WINDOW_EDGE_MARGIN + px(DOCK_TOGGLE_WIDTH) + BAR_MARGIN,
            None => px(0.),
        }
    }
}

/// 将 `smelt_plugin_api::DockAreaState` 转为 NSH 受控布局，提取所有 `smelt/stack` 与占位元数据。
pub fn from_api_state(state: &api::DockAreaState, cx: &App) -> Result<PageLayout, String> {
    let raw = serde_json::to_value(state).map_err(|e| format!("序列化 DockAreaState 失败：{e}"))?;
    let nsh_state: NshDockAreaState =
        serde_json::from_value(raw).map_err(|e| format!("解析 NSH DockAreaState 失败：{e}"))?;

    struct Builder<'a> {
        cx: &'a App,
        placed: HashSet<String>,
        leaves: BTreeMap<String, LeafMeta>,
        stack_seq: usize,
        placeholder_seq: usize,
    }

    impl<'a> Builder<'a> {
        fn place_view(&mut self, id: &str) -> bool {
            if !self.placed.insert(id.to_owned()) {
                return false;
            }
            api::view(id, self.cx).is_some_and(|spec| spec.settings.is_none() && !spec.instances)
        }

        fn filter_stack(&mut self, stack: &api::Stack) -> Option<api::Stack> {
            let mut items = Vec::new();
            for item in &stack.items {
                let content = match &item.content {
                    api::StackContent::View(id) => {
                        if !self.place_view(id) {
                            continue;
                        }
                        api::StackContent::View(id.clone())
                    }
                    api::StackContent::Stack(child) => {
                        let Some(filtered) = self.filter_stack(child) else {
                            continue;
                        };
                        api::StackContent::Stack(filtered)
                    }
                };
                items.push(api::StackItem {
                    content,
                    fit: item.fit,
                });
            }
            (!items.is_empty()).then_some(api::Stack {
                axis: stack.axis,
                items,
            })
        }
    }

    impl PanelBuilder for Builder<'_> {
        fn build(&mut self, panel: &NshPanelState) -> PanelId {
            if panel.panel_name == api::STACK_PANEL {
                if let NshPanelInfo::Panel(info) = &panel.info
                    && let Ok(parsed) = serde_json::from_value::<api::Stack>(info.clone())
                {
                    let key = format!("{}#{}", api::STACK_PANEL, self.stack_seq);
                    self.stack_seq += 1;
                    if let Some(filtered) = self.filter_stack(&parsed) {
                        self.leaves.insert(
                            key.clone(),
                            LeafMeta::Stack {
                                stack: filtered,
                                info: info.clone(),
                            },
                        );
                    } else {
                        self.leaves.insert(
                            key.clone(),
                            LeafMeta::Placeholder {
                                original: panel.clone(),
                            },
                        );
                    }
                    return PanelId::new(key);
                }
            } else if self.place_view(&panel.panel_name) {
                let info = match &panel.info {
                    NshPanelInfo::Panel(value) => value.clone(),
                    _ => Value::Null,
                };
                let key = panel.panel_name.clone();
                self.leaves.insert(
                    key.clone(),
                    LeafMeta::View {
                        id: key.clone(),
                        info,
                    },
                );
                return PanelId::new(key);
            }
            let key = format!("smelt.placeholder#{}", self.placeholder_seq);
            self.placeholder_seq += 1;
            self.leaves.insert(
                key.clone(),
                LeafMeta::Placeholder {
                    original: panel.clone(),
                },
            );
            PanelId::new(key)
        }
    }

    let mut builder = Builder {
        cx,
        placed: HashSet::new(),
        leaves: BTreeMap::new(),
        stack_seq: 0,
        placeholder_seq: 0,
    };
    let nsh_layout = DockAreaLayout::from_state(&nsh_state, &mut builder);
    Ok(PageLayout {
        layout: DockTreeLayout::from_nsh(&nsh_layout),
        leaves: builder.leaves,
        zoomed: None,
    })
}

fn dump_leaf(panel: &str, leaves: &BTreeMap<String, LeafMeta>) -> NshPanelState {
    match leaves.get(panel) {
        Some(LeafMeta::View { id, info }) => {
            let mut state = NshPanelState::new(id.clone());
            state.info = NshPanelInfo::Panel(info.clone());
            state
        }
        Some(LeafMeta::Stack { info, .. }) => {
            let mut state = NshPanelState::new(api::STACK_PANEL);
            state.info = NshPanelInfo::Panel(info.clone());
            state
        }
        Some(LeafMeta::Placeholder { original }) => original.clone(),
        None => NshPanelState::new(panel),
    }
}

fn node_to_panel_state(node: &DockTreeNode, leaves: &BTreeMap<String, LeafMeta>) -> NshPanelState {
    match node {
        DockTreeNode::Split {
            axis,
            children,
            sizes,
            ..
        } => {
            let mut state = NshPanelState::new("StackPanel");
            state.children = children
                .iter()
                .map(|child| node_to_panel_state(child, leaves))
                .collect();
            let points: Vec<Points> = (0..children.len())
                .map(|ix| {
                    sizes
                        .get(ix)
                        .copied()
                        .flatten()
                        .filter(|s| s.is_finite() && *s > 0.)
                        .map(Points)
                        .unwrap_or_default()
                })
                .collect();
            state.info = NshPanelInfo::stack(points, *axis);
            state
        }
        DockTreeNode::Tabs {
            panels,
            active_index,
            ..
        } => {
            let mut state = NshPanelState::new("TabPanel");
            state.children = panels.iter().map(|p| dump_leaf(p, leaves)).collect();
            state.info = NshPanelInfo::tabs(*active_index);
            state
        }
    }
}

fn region_to_dock_state(
    region: &DockTreeRegion,
    placement: NshPlacement,
    leaves: &BTreeMap<String, LeafMeta>,
) -> NshDockState {
    NshDockState::new(
        node_to_panel_state(&region.layout, leaves),
        placement,
        Points(region.size.unwrap_or(200.)),
        region.open.unwrap_or(true),
    )
}

/// 将受控 [`PageLayout`] 无损还原回 `smelt_plugin_api::DockAreaState`（写盘存档用）。
pub fn to_api_state(page: &PageLayout) -> Result<api::DockAreaState, String> {
    let mut nsh_state = NshDockAreaState::default();
    nsh_state.version = Some(api::LAYOUT_VERSION);
    nsh_state.center = node_to_panel_state(&page.layout.center, &page.leaves);
    nsh_state.left_dock = page
        .layout
        .left
        .as_ref()
        .map(|r| region_to_dock_state(r, NshPlacement::Left, &page.leaves));
    nsh_state.right_dock = page
        .layout
        .right
        .as_ref()
        .map(|r| region_to_dock_state(r, NshPlacement::Right, &page.leaves));
    nsh_state.bottom_dock = page
        .layout
        .bottom
        .as_ref()
        .map(|r| region_to_dock_state(r, NshPlacement::Bottom, &page.leaves));
    let value = serde_json::to_value(&nsh_state)
        .map_err(|e| format!("序列化 NSH DockAreaState 失败：{e}"))?;
    serde_json::from_value(value).map_err(|e| format!("转换 DockAreaState 失败：{e}"))
}

// ---- DockTreeLayout <-> JSON（与 nsh-base::dock::value 的 JS 协议同构） ----

pub fn layout_to_json(layout: &DockTreeLayout) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("center".into(), node_to_json(&layout.center));
    if let Some(region) = &layout.left {
        obj.insert("left".into(), region_to_json(region));
    }
    if let Some(region) = &layout.right {
        obj.insert("right".into(), region_to_json(region));
    }
    if let Some(region) = &layout.bottom {
        obj.insert("bottom".into(), region_to_json(region));
    }
    Value::Object(obj)
}

fn region_to_json(region: &DockTreeRegion) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("layout".into(), node_to_json(&region.layout));
    if let Some(size) = region.size {
        obj.insert("size".into(), json!(size));
    }
    if let Some(open) = region.open {
        obj.insert("open".into(), json!(open));
    }
    if let Some(collapsible) = region.collapsible {
        obj.insert("collapsible".into(), json!(collapsible));
    }
    Value::Object(obj)
}

fn node_to_json(node: &DockTreeNode) -> Value {
    match node {
        DockTreeNode::Split {
            id,
            axis,
            children,
            sizes,
        } => {
            let mut obj = serde_json::Map::new();
            obj.insert("type".into(), json!("split"));
            if let Some(id) = id {
                obj.insert("id".into(), json!(id));
            }
            obj.insert(
                "axis".into(),
                json!(match axis {
                    NshAxis::Horizontal => "horizontal",
                    NshAxis::Vertical => "vertical",
                }),
            );
            obj.insert(
                "children".into(),
                Value::Array(children.iter().map(node_to_json).collect()),
            );
            obj.insert(
                "sizes".into(),
                Value::Array(
                    sizes
                        .iter()
                        .map(|s| s.map_or(Value::Null, |p| json!(p)))
                        .collect(),
                ),
            );
            Value::Object(obj)
        }
        DockTreeNode::Tabs {
            id,
            panels,
            active_index,
        } => {
            let mut obj = serde_json::Map::new();
            obj.insert("type".into(), json!("tabs"));
            if let Some(id) = id {
                obj.insert("id".into(), json!(id));
            }
            obj.insert(
                "panels".into(),
                Value::Array(panels.iter().map(|p| json!(p)).collect()),
            );
            obj.insert("activeIndex".into(), json!(active_index));
            Value::Object(obj)
        }
    }
}

pub fn layout_from_json(value: &Value) -> Result<DockTreeLayout, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "layout 不是对象".to_owned())?;
    let center = node_from_json(
        obj.get("center")
            .ok_or_else(|| "layout 缺 center".to_owned())?,
    )?;
    let read_region = |field: &str| -> Result<Option<DockTreeRegion>, String> {
        match obj.get(field) {
            Some(v) if !v.is_null() => region_from_json(v).map(Some),
            _ => Ok(None),
        }
    };
    Ok(DockTreeLayout {
        center,
        left: read_region("left")?,
        right: read_region("right")?,
        bottom: read_region("bottom")?,
    })
}

fn region_from_json(value: &Value) -> Result<DockTreeRegion, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "dock region 不是对象".to_owned())?;
    let layout = node_from_json(
        obj.get("layout")
            .ok_or_else(|| "dock region 缺 layout".to_owned())?,
    )?;
    Ok(DockTreeRegion {
        layout,
        size: obj
            .get("size")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .map(|v| v as f32),
        open: obj.get("open").and_then(Value::as_bool),
        collapsible: obj.get("collapsible").and_then(Value::as_bool),
    })
}

fn node_from_json(value: &Value) -> Result<DockTreeNode, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "dock node 不是对象".to_owned())?;
    let id = obj.get("id").and_then(Value::as_u64);
    match obj.get("type").and_then(Value::as_str) {
        Some("split") => {
            let axis = match obj.get("axis").and_then(Value::as_str) {
                Some("vertical") => NshAxis::Vertical,
                _ => NshAxis::Horizontal,
            };
            let children = obj
                .get("children")
                .and_then(Value::as_array)
                .ok_or_else(|| "split 缺 children".to_owned())?
                .iter()
                .map(node_from_json)
                .collect::<Result<Vec<_>, _>>()?;
            let sizes = obj
                .get("sizes")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|v| v.as_f64().filter(|f| f.is_finite()).map(|f| f as f32))
                        .collect()
                })
                .unwrap_or_default();
            Ok(DockTreeNode::Split {
                id,
                axis,
                children,
                sizes,
            })
        }
        Some("tabs") => {
            let panels = obj
                .get("panels")
                .and_then(Value::as_array)
                .ok_or_else(|| "tabs 缺 panels".to_owned())?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let active_index = obj.get("activeIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
            Ok(DockTreeNode::Tabs {
                id,
                panels,
                active_index,
            })
        }
        other => Err(format!("未知 dock node type：{other:?}")),
    }
}

// ---- 安全区、可见视图、面板选项计算 ----

fn is_leaf_visible(page: &PageLayout, panel: &str) -> bool {
    !matches!(page.leaves.get(panel), Some(LeafMeta::Placeholder { .. }))
}

fn leaf_draws_top_bar(page: &PageLayout, panel: &str, outlet: Option<&str>, cx: &App) -> bool {
    match page.leaves.get(panel) {
        Some(LeafMeta::Stack { .. }) => true,
        Some(LeafMeta::Placeholder { .. }) => true,
        Some(LeafMeta::View { id, .. }) => {
            if id == api::OUTLET_VIEW {
                match outlet.and_then(|o| api::view(o, cx)) {
                    Some(info) => info.draws_top_bar,
                    None => true,
                }
            } else {
                api::view(id, cx).is_some_and(|info| info.draws_top_bar)
            }
        }
        None => false,
    }
}

/// 传给 `<DockArea panels={...}>` 的面板规则列表。
pub fn panel_options_json(page: &PageLayout, outlet: Option<&str>, cx: &App) -> Value {
    let mut out = Vec::with_capacity(page.leaves.len());
    for (key, leaf) in &page.leaves {
        let visible = !matches!(leaf, LeafMeta::Placeholder { .. });
        let draws_top = leaf_draws_top_bar(page, key, outlet, cx);
        let zoomable = matches!(leaf, LeafMeta::View { .. }) && !draws_top;
        out.push(json!({
            "id": key,
            "closable": false,
            "zoomable": zoomable,
            "visible": visible,
            "hideTabBar": draws_top,
        }));
    }
    Value::Array(out)
}

/// 传给 `MainShell.tsx` 渲染每个面板叶子的元数据（标题、角标、`smelt/stack` 结构）。
pub fn panel_meta_json(page: &PageLayout, outlet: Option<&str>, cx: &App) -> Value {
    let mut map = serde_json::Map::new();
    for (key, leaf) in &page.leaves {
        match leaf {
            LeafMeta::View { id, .. } => {
                let effective_id = if id == api::OUTLET_VIEW {
                    outlet.unwrap_or(id.as_str())
                } else {
                    id.as_str()
                };
                let info = api::view(effective_id, cx);
                let title = info
                    .as_ref()
                    .map(|v| v.title.get().to_string())
                    .unwrap_or_else(|| effective_id.to_owned());
                let badge = api::view_badge(effective_id, cx).map(|b| b.to_string());
                let draws_top_bar = leaf_draws_top_bar(page, key, outlet, cx);
                map.insert(
                    key.clone(),
                    json!({
                        "kind": "view",
                        "view": id,
                        "title": title,
                        "badge": badge,
                        "draws_top_bar": draws_top_bar,
                    }),
                );
            }
            LeafMeta::Stack { stack, .. } => {
                let titles: Vec<String> = stack
                    .views()
                    .into_iter()
                    .map(|id| {
                        api::view(id, cx)
                            .map(|v| v.title.get().to_string())
                            .unwrap_or_else(|| id.to_owned())
                    })
                    .collect();
                map.insert(
                    key.clone(),
                    json!({
                        "kind": "stack",
                        "title": titles.join(" · "),
                        "stack": stack_to_json(stack),
                        "draws_top_bar": true,
                    }),
                );
            }
            LeafMeta::Placeholder { .. } => {
                map.insert(key.clone(), json!({ "kind": "placeholder" }));
            }
        }
    }
    Value::Object(map)
}

fn stack_to_json(stack: &api::Stack) -> Value {
    json!({
        "axis": match stack.axis {
            api::StackAxis::Horizontal => "horizontal",
            api::StackAxis::Vertical => "vertical",
        },
        "items": stack.items.iter().map(|item| {
            match &item.content {
                api::StackContent::View(id) => json!({ "view": id, "fit": item.fit }),
                api::StackContent::Stack(child) => json!({ "stack": stack_to_json(child), "fit": item.fit }),
            }
        }).collect::<Vec<_>>(),
    })
}

/// 按 [`PageLayout`] 算出当前页的可见视图与各组/各视图的 `SafeArea`（与 `ShellView::of` 等价）。
pub fn compute_shell_view(
    page: &PageLayout,
    corners: WindowCorners,
    outlet: Option<String>,
    cx: &App,
) -> ComputedShellView {
    let zoomed = page.zoomed;
    let toggle = |region: Option<&DockTreeRegion>| {
        let region = region?;
        if zoomed.is_some() || !region.collapsible.unwrap_or(true) {
            return None;
        }
        Some(region.open.unwrap_or(true))
    };

    let mut view = ComputedShellView {
        corners,
        left_dock: toggle(page.layout.left.as_ref()),
        right_dock: toggle(page.layout.right.as_ref()),
        outlet: outlet.clone(),
        ..Default::default()
    };

    let (leading, trailing) = (view.leading_inset(), view.trailing_inset());
    let rounded = size(corners.rounded, corners.rounded);
    let top_left = top_left_group(page);
    let top_right = top_right_group(page);
    let bottom_left = bottom_corner_group(page, NshPlacement::Left);
    let bottom_right = bottom_corner_group(page, NshPlacement::Right);

    let placements: [(NshPlacement, Option<(&DockTreeNode, bool)>); 4] = [
        (NshPlacement::Center, Some((&page.layout.center, true))),
        (
            NshPlacement::Left,
            page.layout
                .left
                .as_ref()
                .map(|r| (&r.layout, r.open.unwrap_or(true))),
        ),
        (
            NshPlacement::Right,
            page.layout
                .right
                .as_ref()
                .map(|r| (&r.layout, r.open.unwrap_or(true))),
        ),
        (
            NshPlacement::Bottom,
            page.layout
                .bottom
                .as_ref()
                .map(|r| (&r.layout, r.open.unwrap_or(true))),
        ),
    ];

    let mut top_groups = HashSet::new();
    for (placement, root) in &placements {
        if let Some((node, true)) = root
            && *placement != NshPlacement::Bottom
        {
            collect_top_edge_groups(node, &mut top_groups);
        }
    }

    for (_, root) in placements {
        let Some((root, open)) = root else {
            continue;
        };
        walk_tabs(root, &mut |node_id, panels, active_ix| {
            let visible: Vec<(usize, &str)> = panels
                .iter()
                .enumerate()
                .map(|(ix, p)| (ix, p.as_str()))
                .filter(|(_, p)| is_leaf_visible(page, p))
                .collect();
            for (_, key) in &visible {
                match page.leaves.get(*key) {
                    Some(LeafMeta::Stack { stack, .. }) => {
                        view.laid_out
                            .extend(stack.views().into_iter().map(str::to_owned));
                    }
                    Some(LeafMeta::View { id, .. }) => {
                        view.laid_out.insert(id.clone());
                    }
                    _ => {}
                }
            }
            if !open || zoomed.is_some_and(|z| z != node_id) {
                return;
            }
            let Some((_, displayed)) = visible
                .iter()
                .find(|(ix, _)| *ix == active_ix)
                .or(visible.first())
            else {
                return;
            };
            let mut group = SafeArea::default();
            if zoomed.is_some() || top_groups.contains(&node_id) {
                group.title_bar = true;
                if top_left == Some(node_id) {
                    group.corners.top_left = size(leading, TOP_BAR_HEIGHT);
                }
                if top_right == Some(node_id) {
                    group.corners.top_right = size(trailing, TOP_BAR_HEIGHT);
                }
                view.groups.insert(node_id, group);
            }
            let draws_top = leaf_draws_top_bar(page, displayed, outlet.as_deref(), cx);
            let bare = visible.len() == 1 && draws_top;
            let mut shown = if bare { group } else { SafeArea::default() };
            if bottom_left == Some(node_id) {
                shown.corners.bottom_left = rounded;
            }
            if bottom_right == Some(node_id) {
                shown.corners.bottom_right = rounded;
            }
            let mut record = |id: &str, area: SafeArea| {
                if area != SafeArea::default() {
                    view.areas.insert(id.to_owned(), area);
                }
                view.shown.insert(id.to_owned());
            };
            match page.leaves.get(*displayed) {
                Some(LeafMeta::Stack { stack, .. }) => {
                    divide_stack_areas(stack, shown, &mut record)
                }
                Some(LeafMeta::View { id, .. }) => record(id, shown),
                _ => {}
            }
        });
    }

    if let Some(content) = &view.outlet
        && view.shown.contains(api::OUTLET_VIEW)
    {
        view.shown.insert(content.clone());
    }

    view
}

fn divide_stack_areas(stack: &api::Stack, area: SafeArea, f: &mut impl FnMut(&str, SafeArea)) {
    let last = stack.items.len().saturating_sub(1);
    for (ix, item) in stack.items.iter().enumerate() {
        let (first, end) = (ix == 0, ix == last);
        let touches = match stack.axis {
            api::StackAxis::Vertical => Edges {
                top: first,
                bottom: end,
                left: true,
                right: true,
            },
            api::StackAxis::Horizontal => Edges {
                top: true,
                bottom: true,
                left: first,
                right: end,
            },
        };
        let sub = area.child(touches);
        match &item.content {
            api::StackContent::View(id) => f(id, sub),
            api::StackContent::Stack(child) => divide_stack_areas(child, sub, f),
        }
    }
}

fn walk_tabs(node: &DockTreeNode, f: &mut impl FnMut(u64, &[String], usize)) {
    match node {
        DockTreeNode::Tabs {
            id,
            panels,
            active_index,
        } => {
            if let Some(id) = id {
                f(*id, panels, *active_index);
            }
        }
        DockTreeNode::Split { children, .. } => {
            for child in children {
                walk_tabs(child, f);
            }
        }
    }
}

fn collect_top_edge_groups(node: &DockTreeNode, out: &mut HashSet<u64>) {
    match node {
        DockTreeNode::Tabs { id, .. } => {
            if let Some(id) = id {
                out.insert(*id);
            }
        }
        DockTreeNode::Split { axis, children, .. } => match axis {
            NshAxis::Horizontal => {
                for child in children {
                    collect_top_edge_groups(child, out);
                }
            }
            NshAxis::Vertical => {
                if let Some(first) = children.first() {
                    collect_top_edge_groups(first, out);
                }
            }
        },
    }
}

fn open_root(layout: &DockTreeLayout, placement: NshPlacement) -> Option<&DockTreeNode> {
    match placement {
        NshPlacement::Center => Some(&layout.center),
        _ => layout
            .dock(placement)
            .filter(|r| r.open.unwrap_or(true))
            .map(|r| &r.layout),
    }
}

fn corner_group(node: &DockTreeNode, right: bool, bottom: bool) -> Option<u64> {
    match node {
        DockTreeNode::Tabs { id, .. } => *id,
        DockTreeNode::Split { axis, children, .. } => {
            let last = match axis {
                NshAxis::Horizontal => right,
                NshAxis::Vertical => bottom,
            };
            let child = if last {
                children.last()
            } else {
                children.first()
            };
            child.and_then(|c| corner_group(c, right, bottom))
        }
    }
}

fn top_left_group(page: &PageLayout) -> Option<u64> {
    if let Some(z) = page.zoomed {
        return Some(z);
    }
    open_root(&page.layout, NshPlacement::Left)
        .or_else(|| open_root(&page.layout, NshPlacement::Center))
        .and_then(|root| corner_group(root, false, false))
}

fn top_right_group(page: &PageLayout) -> Option<u64> {
    if let Some(z) = page.zoomed {
        return Some(z);
    }
    open_root(&page.layout, NshPlacement::Right)
        .or_else(|| open_root(&page.layout, NshPlacement::Center))
        .and_then(|root| corner_group(root, true, false))
}

fn bottom_corner_group(page: &PageLayout, side: NshPlacement) -> Option<u64> {
    if let Some(z) = page.zoomed {
        return Some(z);
    }
    let root = match open_root(&page.layout, side) {
        Some(root) => Some(root),
        None if page.layout.bottom.is_some() => open_root(&page.layout, NshPlacement::Bottom),
        None => open_root(&page.layout, NshPlacement::Center),
    };
    root.and_then(|root| corner_group(root, side == NshPlacement::Right, true))
}

/// 查找某视图在 `PageLayout` 里的面板 key、所在组 id 与所在区域（`smelt/stack` 内的视图返回摞 key）。
pub fn find_view_in_page(page: &PageLayout, view_id: &str) -> Option<(String, u64, NshPlacement)> {
    let target_key = page.leaves.iter().find_map(|(key, leaf)| match leaf {
        LeafMeta::View { id, .. } if id == view_id => Some(key.clone()),
        LeafMeta::Stack { stack, .. } if stack.views().contains(&view_id) => Some(key.clone()),
        _ => None,
    })?;
    let regions: [(NshPlacement, Option<&DockTreeNode>); 4] = [
        (NshPlacement::Center, Some(&page.layout.center)),
        (
            NshPlacement::Left,
            page.layout.left.as_ref().map(|r| &r.layout),
        ),
        (
            NshPlacement::Right,
            page.layout.right.as_ref().map(|r| &r.layout),
        ),
        (
            NshPlacement::Bottom,
            page.layout.bottom.as_ref().map(|r| &r.layout),
        ),
    ];
    for (placement, root) in regions {
        let Some(root) = root else {
            continue;
        };
        let mut found = None;
        walk_tabs(root, &mut |node_id, panels, _| {
            if panels.iter().any(|p| p == &target_key) {
                found = Some(node_id);
            }
        });
        if let Some(node_id) = found {
            return Some((target_key, node_id, placement));
        }
    }
    None
}

/// 在 `PageLayout` 里展开停靠区并选中包含 `panel_key` 的标签（用于 `show_view`）。
pub fn reveal_panel_in_layout(page: &mut PageLayout, panel_key: &str) -> bool {
    let mut changed = false;
    fn select_in_node(node: &mut DockTreeNode, panel_key: &str) -> bool {
        match node {
            DockTreeNode::Tabs {
                panels,
                active_index,
                ..
            } => {
                if let Some(ix) = panels.iter().position(|p| p == panel_key) {
                    *active_index = ix;
                    return true;
                }
                false
            }
            DockTreeNode::Split { children, .. } => children
                .iter_mut()
                .any(|child| select_in_node(child, panel_key)),
        }
    }
    if select_in_node(&mut page.layout.center, panel_key) {
        changed = true;
    } else {
        for region in [
            page.layout.left.as_mut(),
            page.layout.right.as_mut(),
            page.layout.bottom.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            if select_in_node(&mut region.layout, panel_key) {
                region.open = Some(true);
                changed = true;
                break;
            }
        }
    }
    if changed {
        // 如果别的组正放大，退出放大让它露出来。
        if let Some(zoomed) = page.zoomed {
            let mut in_zoomed = false;
            for root in [
                Some(&page.layout.center),
                page.layout.left.as_ref().map(|r| &r.layout),
                page.layout.right.as_ref().map(|r| &r.layout),
                page.layout.bottom.as_ref().map(|r| &r.layout),
            ]
            .into_iter()
            .flatten()
            {
                walk_tabs(root, &mut |id, panels, _| {
                    if id == zoomed && panels.iter().any(|p| p == panel_key) {
                        in_zoomed = true;
                    }
                });
            }
            if !in_zoomed {
                page.zoomed = None;
            }
        }
    }
    changed
}

/// 将 `SafeArea` 序列化给 JS 插件与主壳使用。
pub fn safe_area_to_json(area: SafeArea) -> Value {
    let bar = area.bar_insets();
    let top = area.top_bar_insets();
    json!({
        "title_bar": area.title_bar,
        "top_left": f32::from(area.corners.top_left.width),
        "top_right": f32::from(area.corners.top_right.width),
        "bottom_left": f32::from(area.corners.bottom_left.width),
        "bottom_right": f32::from(area.corners.bottom_right.width),
        "top_bar_insets": { "left": f32::from(top.left), "right": f32::from(top.right) },
        "bar_insets": { "left": f32::from(bar.left), "right": f32::from(bar.right) },
    })
}

/// 把一页的 `areas` 与 `groups` 转成供 `smelt.state` 与 `MainShell.tsx` 消费的 JSON。
pub fn safe_areas_json(view: &ComputedShellView) -> Value {
    let mut map = serde_json::Map::new();
    for (id, area) in &view.areas {
        map.insert(id.clone(), safe_area_to_json(*area));
    }
    if let Some(outlet) = &view.outlet
        && let Some(area) = view.areas.get(api::OUTLET_VIEW)
    {
        map.insert(outlet.clone(), safe_area_to_json(*area));
    }
    Value::Object(map)
}

pub fn group_areas_json(view: &ComputedShellView) -> Value {
    let mut map = serde_json::Map::new();
    for (node_id, area) in &view.groups {
        map.insert(node_id.to_string(), safe_area_to_json(*area));
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_area_layout_json_round_trips() {
        let raw = json!({
            "center": {
                "type": "split",
                "id": 1,
                "axis": "horizontal",
                "children": [
                    { "type": "tabs", "id": 2, "panels": ["smelt/outlet"], "activeIndex": 0 }
                ],
                "sizes": [820.0]
            },
            "left": {
                "layout": {
                    "type": "tabs",
                    "id": 3,
                    "panels": ["smelt/stack#0"],
                    "activeIndex": 0
                },
                "size": 260.0,
                "open": true,
                "collapsible": true
            }
        });
        let parsed = layout_from_json(&raw).unwrap();
        let back = layout_to_json(&parsed);
        assert_eq!(back["center"]["id"], json!(1));
        assert_eq!(back["center"]["children"][0]["id"], json!(2));
        assert_eq!(back["left"]["layout"]["id"], json!(3));
        assert_eq!(back["left"]["size"], json!(260.0));
        assert_eq!(back["left"]["open"], json!(true));
        assert_eq!(
            back["center"]["children"][0]["panels"],
            json!(["smelt/outlet"])
        );
    }
}
