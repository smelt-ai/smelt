//! 工作台布局：把布局文件搭成外层 Dock，切换、恢复默认，按布局存取点出来的状态，以及插件面板。
//!
//! 格式和校验在 `smelt_client::layout`。这里只管窗口里的事：组件名对到面板实体，
//! 声明树翻成 `DockLayout` 装进 `workspace_shell` 建好的那个 `DockArea`。
//! 面板实体按组件名缓存，切换布局时复用，会话列表、舞台里的终端和插件脚本都不重建。
//!
//! 停靠区的开合和宽度、拆分比例、各组选中的标签只在 Dock 上。Dock 发 `LayoutChanged`
//! 时按布局存进客户端库（`workspace_layout_state`），装布局时在搭树的同时套回去。
//!
//! 插件面板（`plugin:<id>/<面板>`）由插件自己注册的类建出来，外面包一层 smelt 的
//! `PluginPanel`：底色和「没装 / 没声明 / 没注册 / 加载失败」的说明归宿主，内容归插件。
//! 布局里的 `plugin:*` 展开成没被单独摆出来的那些插件面板。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, WeakEntity, Window,
    div, prelude::*, px, rgb,
};
use gpui_component::ActiveTheme;
use gpui_component::dock::{
    BasePanel, BasePanelView, DockArea, DockLayout, DockPlacement, PaneNode, PaneRef, Panel,
    PanelBuildContext, PanelControl, PanelEvent, PanelId, PanelInfo, PanelRegistry, PanelState,
    panel_handle,
};
use gpui_shell::dock::ScriptPanel;
use smelt_client::layout::{
    self, Axis, Component, DockState, DockStates, Layout, LayoutEntry, LayoutState, Node,
    PluginPanelRef, Region, SplitState, Tab,
};
use smelt_client_store::LayoutStateRecord;

use crate::workspace_shell::{Builtin, ShellPanel};
use crate::{Workspace, ui_theme};

/// 布局里没写 `size`、也没存过的停靠区多宽。
const DEFAULT_DOCK_SIZE: f32 = 320.;

const SIDE_DOCKS: [(DockPlacement, Region); 3] = [
    (DockPlacement::Left, Region::Left),
    (DockPlacement::Right, Region::Right),
    (DockPlacement::Bottom, Region::Bottom),
];

/// 当前装着的布局，和它摆出来的面板。
pub(crate) struct ShellLayout {
    pub(crate) id: String,
    pub(crate) layout: Layout,
    /// 组件 → 面板，正好是 Dock 里的全部面板（含 `plugin:*` 展开出来的）。切换布局时复用。
    panels: HashMap<Component, Arc<dyn BasePanelView>>,
    /// 插件面板的宿主壳，键和 `panels` 里的插件面板一样。
    plugins: HashMap<Component, Entity<PluginPanel>>,
}

impl ShellLayout {
    /// 布局里有没有这个组件（`plugin:*` 收进来的也算）。
    pub(crate) fn places(&self, component: &Component) -> bool {
        self.panels.contains_key(component)
    }

    pub(crate) fn panel_id(&self, component: &Component, cx: &App) -> Option<PanelId> {
        self.panels.get(component).map(|panel| panel.panel_id(cx))
    }

    /// 从 Dock 现在的样子取出要存的状态。
    fn capture_state(&self, area: &DockArea, cx: &App) -> LayoutState {
        let keys: HashMap<PanelId, String> = self
            .panels
            .iter()
            .map(|(component, panel)| (panel.panel_id(cx), component.key()))
            .collect();
        let docks = DockStates {
            left: dock_state(area, DockPlacement::Left),
            right: dock_state(area, DockPlacement::Right),
            bottom: dock_state(area, DockPlacement::Bottom),
        };
        let mut splits = Vec::new();
        let mut active = Vec::new();
        for placement in [
            DockPlacement::Center,
            DockPlacement::Left,
            DockPlacement::Right,
            DockPlacement::Bottom,
        ] {
            if let Some(tree) = area.layout(placement) {
                capture_node(tree.root(), &keys, &mut splits, &mut active);
            }
        }
        LayoutState::new(docks, splits, active)
    }
}

fn dock_state(area: &DockArea, placement: DockPlacement) -> Option<DockState> {
    area.has_dock(placement).then(|| DockState {
        open: area.is_dock_open(placement),
        size: area
            .dock_size(placement)
            .map_or(DEFAULT_DOCK_SIZE, f32::from),
    })
}

/// 记下拖过的拆分（按每个子节点最左上那块面板认）和多标签组里选中的那块。
fn capture_node(
    node: &PaneNode,
    keys: &HashMap<PanelId, String>,
    splits: &mut Vec<SplitState>,
    active: &mut Vec<String>,
) {
    match node.kind() {
        PaneRef::Tabs { panels, active_ix } => {
            if panels.len() > 1
                && let Some(key) = panels.get(active_ix).and_then(|panel| keys.get(panel))
            {
                active.push(key.clone());
            }
        }
        PaneRef::Split {
            children, sizes, ..
        } => {
            let children_keys: Option<Vec<String>> = children
                .iter()
                .map(|child| first_panel(child).and_then(|panel| keys.get(&panel).cloned()))
                .collect();
            if let Some(children_keys) = children_keys
                && sizes.iter().any(Option::is_some)
            {
                splits.push(SplitState {
                    children: children_keys,
                    sizes: sizes.iter().map(|size| size.map(f32::from)).collect(),
                });
            }
            for child in children {
                capture_node(child, keys, splits, active);
            }
        }
    }
}

fn first_panel(node: &PaneNode) -> Option<PanelId> {
    match node.kind() {
        PaneRef::Tabs { panels, .. } => panels.first().copied(),
        PaneRef::Split { children, .. } => children.first().and_then(first_panel),
    }
}

/// 启动时装的布局，和装它时顺带读出来的东西。
pub(crate) struct InitialShell {
    pub(crate) area: Entity<DockArea>,
    pub(crate) layout: ShellLayout,
    pub(crate) catalog: Vec<LayoutEntry>,
    /// 刚装好时 Dock 的状态，等于库里存着的（或即将等于），用来跳过一样的写入。
    pub(crate) written: String,
}

/// 启动时装上次用的布局。没有、读不出来或不合法就装默认布局并提示，库里记着的选择不改：
/// 布局文件修好了，下次启动还是它。插件必须已经挂载（`plugins::mount_all`）。
pub(crate) fn install_initial(
    workspace: WeakEntity<Workspace>,
    chosen: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) -> InitialShell {
    let area = crate::workspace_shell::new_area(workspace.clone(), window, cx);
    let catalog = layout::discover(&layout_dirs());
    let (id, layout) = match chosen {
        None => (layout::DEFAULT_ID.to_string(), Layout::default_layout()),
        Some(id) => match catalog.iter().find(|entry| entry.id == id) {
            Some(LayoutEntry {
                layout: Ok(layout), ..
            }) => (id.to_string(), layout.clone()),
            found => {
                let reason = match found {
                    Some(LayoutEntry {
                        path,
                        layout: Err(error),
                        ..
                    }) => format!("{}：{error}", path.display()),
                    _ => "布局文件不在了".to_string(),
                };
                crate::status_item::notify_error(format!(
                    "上次用的布局 `{id}` 没有加载，先用默认布局（{reason}）"
                ));
                (layout::DEFAULT_ID.to_string(), Layout::default_layout())
            }
        },
    };
    let builder = Builder::new(workspace, &area, HashMap::new(), HashMap::new(), window);
    let layout = install(builder, &area, id, layout, cx);
    let written = layout.capture_state(area.read(cx), cx).to_json();
    InitialShell {
        area,
        layout,
        catalog,
        written,
    }
}

/// 读这套布局存过的状态。布局文件改过（指纹对不上）、格式不认识都当没存过。
fn saved_state(id: &str, layout: &Layout) -> Option<LayoutState> {
    let record = match smelt_client_store::client_store().and_then(|store| store.layout_state(id)) {
        Ok(record) => record?,
        Err(error) => {
            eprintln!("[layout] 读布局 `{id}` 的状态失败：{error}");
            return None;
        }
    };
    (record.file_digest == layout.digest)
        .then(|| LayoutState::parse(&record.state))
        .flatten()
}

fn install(
    mut builder: Builder<'_>,
    area: &Entity<DockArea>,
    id: String,
    layout: Layout,
    cx: &mut App,
) -> ShellLayout {
    builder.state = saved_state(&id, &layout);
    builder.sync_plugins(&layout, cx);
    // 这套布局用不到的面板不留：留着的实体还订阅着工作台，每次通知都会跟着重画。
    let mut wanted: HashSet<Component> = layout.components().into_iter().cloned().collect();
    wanted.extend(builder.plugins.keys().cloned());
    builder
        .panels
        .retain(|component, _| wanted.contains(component));

    let center = builder
        .build_node(&layout.center, cx)
        .map_or_else(DockLayout::tabs, |(node, _)| node);
    let docks: Vec<_> = SIDE_DOCKS
        .into_iter()
        .map(|(placement, region)| {
            let dock = layout.dock(region).and_then(|spec| {
                let (node, _) = builder.build_node(&spec.node, cx)?;
                let saved = builder.state.as_ref().and_then(|state| state.dock(region));
                let size = saved.map_or(spec.size.unwrap_or(DEFAULT_DOCK_SIZE), |dock| dock.size);
                let open = saved.map_or(spec.open, |dock| dock.open);
                Some((node, size, open))
            });
            (placement, dock)
        })
        .collect();
    let Builder {
        panels,
        plugins,
        window,
        ..
    } = builder;

    area.update(cx, |area, cx| {
        area.set_zoomed_out(window, cx);
        area.set_center(center, window, cx);
        for (placement, dock) in docks {
            let Some((node, size, open)) = dock else {
                area.remove_dock(placement, window, cx);
                continue;
            };
            area.set_dock(placement, node, window, cx);
            area.set_dock_size(placement, px(size), window, cx);
            if area.is_dock_open(placement) != open {
                area.toggle_dock(placement, window, cx);
            }
        }
        // 锁住只禁拖动和放下，开合与拖宽照常。
        area.set_locked(true, window, cx);
    });
    ShellLayout {
        id,
        layout,
        panels,
        plugins,
    }
}

/// 搭一套布局时手里的东西：上一套留下、可以复用的面板，存过的状态，和建新面板要用的窗口、Dock。
struct Builder<'a> {
    workspace: WeakEntity<Workspace>,
    area: WeakEntity<DockArea>,
    panels: HashMap<Component, Arc<dyn BasePanelView>>,
    plugins: HashMap<Component, Entity<PluginPanel>>,
    /// 没摆进布局的插件面板，`plugin:*` 展开成它们。
    unplaced: Vec<PluginPanelRef>,
    state: Option<LayoutState>,
    window: &'a mut Window,
}

impl<'a> Builder<'a> {
    fn new(
        workspace: WeakEntity<Workspace>,
        area: &Entity<DockArea>,
        panels: HashMap<Component, Arc<dyn BasePanelView>>,
        plugins: HashMap<Component, Entity<PluginPanel>>,
        window: &'a mut Window,
    ) -> Self {
        Self {
            workspace,
            area: area.downgrade(),
            panels,
            plugins,
            unplaced: Vec::new(),
            state: None,
            window,
        }
    }

    /// 插件面板对齐到这套布局和眼下挂着的插件：用不到的释放，插件重新加载过的重建。
    /// 面板本身在 `build_node` 摆到哪儿就在哪儿建。
    fn sync_plugins(&mut self, layout: &Layout, cx: &mut App) {
        self.unplaced = if layout.components().contains(&&Component::OtherPlugins) {
            crate::plugins::declared()
                .into_iter()
                .filter(|target| !layout.mentions_plugin_panel(&target.plugin, &target.panel))
                .collect()
        } else {
            Vec::new()
        };
        let wanted: HashSet<Component> = layout
            .plugin_panels()
            .into_iter()
            .cloned()
            .chain(self.unplaced.iter().cloned())
            .map(Component::Plugin)
            .collect();

        let stale: Vec<Component> = self
            .plugins
            .keys()
            .filter(|component| !wanted.contains(component))
            .cloned()
            .collect();
        for component in stale {
            if let Some(panel) = self.plugins.remove(&component) {
                self.panels.remove(&component);
                panel.update(cx, |panel, cx| panel.release(self.window, cx));
            }
        }
        for panel in self.plugins.values() {
            panel.update(cx, |panel, cx| panel.refresh(&self.area, self.window, cx));
        }
    }

    /// 取一个插件面板，没有就建。`title` 是布局给的标题，复用的面板也换上它。
    fn plugin(
        &mut self,
        target: &PluginPanelRef,
        title: Option<String>,
        cx: &mut App,
    ) -> Arc<dyn BasePanelView> {
        let component = Component::Plugin(target.clone());
        let plugin = match self.plugins.get(&component) {
            Some(panel) => {
                panel.update(cx, |panel, cx| panel.set_title(title, cx));
                panel.clone()
            }
            None => {
                let panel = PluginPanel::new(
                    self.workspace.clone(),
                    target.clone(),
                    title,
                    &self.area,
                    self.window,
                    cx,
                );
                self.plugins.insert(component.clone(), panel.clone());
                panel
            }
        };
        self.panels
            .entry(component)
            .or_insert_with(|| panel_handle(plugin))
            .clone()
    }

    fn builtin(&mut self, builtin: Builtin, cx: &mut App) -> Arc<dyn BasePanelView> {
        let workspace = self.workspace.clone();
        self.panels
            .entry(builtin.component())
            .or_insert_with(|| panel_handle(cx.new(|cx| ShellPanel::new(workspace, builtin, cx))))
            .clone()
    }

    /// 搭一个节点，顺带给出它最左上那块面板的键（认拆分用）。空的标签组（`plugin:*`
    /// 一个插件都没有）和因此空掉的拆分不搭；只剩一个子节点的拆分直接换成那个子节点。
    fn build_node(&mut self, node: &Node, cx: &mut App) -> Option<(DockLayout, String)> {
        match node {
            Node::Split { axis, children } => {
                let mut built = Vec::new();
                for (child, size) in children {
                    if let Some((node, key)) = self.build_node(child, cx) {
                        built.push((node, key, *size));
                    }
                }
                if built.len() <= 1 {
                    return built.pop().map(|(node, key, _)| (node, key));
                }
                let keys: Vec<String> = built.iter().map(|(_, key, _)| key.clone()).collect();
                let saved = self.state.as_ref().and_then(|state| {
                    state
                        .splits
                        .iter()
                        .find(|split| split.children == keys)
                        .map(|split| split.sizes.clone())
                });
                let split = match axis {
                    Axis::Row => DockLayout::h_split(),
                    Axis::Column => DockLayout::v_split(),
                };
                let split =
                    built
                        .into_iter()
                        .enumerate()
                        .fold(split, |split, (ix, (node, _, size))| {
                            let size = saved.as_ref().map_or(size, |sizes| sizes[ix]);
                            split.child(node, size.map(px))
                        });
                Some((split, keys[0].clone()))
            }
            Node::Tabs { tabs, active } => {
                let mut entries: Vec<(Arc<dyn BasePanelView>, String)> = Vec::new();
                let mut declared_active = None;
                for (ix, Tab { component, title }) in tabs.iter().enumerate() {
                    if ix == *active {
                        declared_active = Some(entries.len());
                    }
                    match component {
                        Component::OtherPlugins => {
                            for target in self.unplaced.clone() {
                                let key = Component::Plugin(target.clone()).key();
                                entries.push((self.plugin(&target, None, cx), key));
                            }
                        }
                        Component::Plugin(target) => {
                            entries.push((self.plugin(target, title.clone(), cx), component.key()));
                        }
                        builtin => {
                            let builtin = Builtin::of(builtin).unwrap_or_else(|| {
                                panic!("内置组件 `{}` 没有面板", builtin.name())
                            });
                            entries.push((self.builtin(builtin, cx), component.key()));
                        }
                    }
                }
                let first_key = entries.first()?.1.clone();
                let saved_active = self.state.as_ref().and_then(|state| {
                    entries
                        .iter()
                        .position(|(_, key)| state.active.contains(key))
                });
                let active = saved_active
                    .or(declared_active)
                    .filter(|ix| *ix < entries.len())
                    .unwrap_or(0);
                let group = entries
                    .into_iter()
                    .fold(DockLayout::tabs(), |group, (panel, _)| {
                        group.panel_view(panel, cx)
                    });
                Some((group.active_index(active), first_key))
            }
        }
    }
}

/// 布局目录，和插件一样的来源和先后：用户的、App 自带的、debug 构建读仓库里的。
fn layout_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(user) = layout::user_layout_dir() {
        dirs.push(user);
    }
    if let Some(bundled) = bundled_layout_dir() {
        dirs.push(bundled);
    }
    #[cfg(debug_assertions)]
    dirs.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(layout::LAYOUT_DIR),
    );
    dirs
}

fn bundled_layout_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    if macos.file_name()? != "MacOS" || contents.file_name()? != "Contents" {
        return None;
    }
    let dir = contents.join("Resources").join(layout::LAYOUT_DIR);
    dir.is_dir().then_some(dir)
}

impl Workspace {
    /// 重读布局目录。读文件，只在打开命令面板、切换这类事件里调，不在 render 里。
    pub(crate) fn reload_layout_catalog(&mut self) {
        self.layout_catalog = layout::discover(&layout_dirs());
    }

    /// 命令面板里和布局有关的条目。只读缓存的目录。
    pub(crate) fn layout_palette_entries(&self) -> Vec<(String, crate::Cmd)> {
        let mut entries = Vec::new();
        for entry in &self.layout_catalog {
            if entry.id == self.shell_layout.id {
                continue;
            }
            let label = match &entry.layout {
                Ok(layout) => format!("切换布局：{}", layout.name),
                Err(_) => format!("切换布局：{}（有错误）", entry.id),
            };
            entries.push((label, crate::Cmd::SwitchLayout(entry.id.clone())));
        }
        if self.shell_layout.id != layout::DEFAULT_ID {
            entries.push(("恢复默认布局".to_string(), crate::Cmd::RestoreDefaultLayout));
        }
        entries
    }

    /// 切到目录里的一套布局。每次都重读文件，改过的布局再切一次就生效。
    /// 不合法就不动当前布局，提示原因。
    pub(crate) fn switch_layout(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_layout_catalog();
        let Some(entry) = self.layout_catalog.iter().find(|entry| entry.id == id) else {
            crate::status_item::notify_error(format!("找不到布局 `{id}`"));
            return;
        };
        let LayoutEntry { id, path, layout } = entry.clone();
        match layout {
            Ok(layout) => self.apply_layout(id, layout, window, cx),
            Err(error) => crate::status_item::notify_error(format!(
                "布局 `{id}` 没有加载（{}）：{error}",
                path.display()
            )),
        }
    }

    pub(crate) fn restore_default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_layout(
            layout::DEFAULT_ID.to_string(),
            Layout::default_layout(),
            window,
            cx,
        );
    }

    /// 装一套布局，记下选的是哪套（默认布局记成没选）。
    fn apply_layout(
        &mut self,
        id: String,
        layout: Layout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panels = std::mem::take(&mut self.shell_layout.panels);
        let plugins = std::mem::take(&mut self.shell_layout.plugins);
        let area = self.shell_dock.clone();
        let builder = Builder::new(cx.weak_entity(), &area, panels, plugins, window);
        self.shell_layout = install(builder, &area, id, layout, cx);
        // 装的时候已经按存档搭好，Dock 现在的样子就是库里的，不用再写一遍。
        self.layout_state_written = Some(
            self.shell_layout
                .capture_state(self.shell_dock.read(cx), cx)
                .to_json(),
        );
        self.layout_choice =
            (self.shell_layout.id != layout::DEFAULT_ID).then(|| self.shell_layout.id.clone());
        self.save_state(cx);
        cx.notify();
    }

    /// Dock 布局变了（开合、拖宽、拖拆分、换标签）：存进这套布局的状态。和上次写的一样就不写。
    pub(crate) fn save_layout_state(&mut self, cx: &mut Context<Self>) {
        let state = self
            .shell_layout
            .capture_state(self.shell_dock.read(cx), cx)
            .to_json();
        if self.layout_state_written.as_deref() == Some(state.as_str()) {
            return;
        }
        let record = LayoutStateRecord {
            layout_id: self.shell_layout.id.clone(),
            file_digest: self.shell_layout.layout.digest.clone(),
            state: state.clone(),
        };
        match smelt_client_store::client_store().and_then(|store| store.put_layout_state(&record)) {
            Ok(()) => self.layout_state_written = Some(state),
            Err(error) => eprintln!(
                "[layout] 存布局 `{}` 的状态失败：{error}",
                self.shell_layout.id
            ),
        }
    }

    /// 窗口变窄时把展开的侧停靠区收窄，给中间留够 `MIN_WORKSPACE_CONTENT_WIDTH`：
    /// Dock 不会自己按窗口收。先收右边，再收左边，最窄收到 `MIN_SIDE_DOCK_WIDTH`。
    /// 收窄会发布局变化，跟着存下来：Dock 上的宽度就是唯一的宽度。
    pub(crate) fn fit_docks_to_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = f32::from(window.viewport_size().width);
        self.shell_dock.update(cx, |area, cx| {
            let open_width = |area: &DockArea, placement| {
                if area.is_dock_open(placement) {
                    area.dock_size(placement).map_or(0., f32::from)
                } else {
                    0.
                }
            };
            let mut excess = open_width(area, DockPlacement::Left)
                + open_width(area, DockPlacement::Right)
                + crate::MIN_WORKSPACE_CONTENT_WIDTH
                - viewport;
            for placement in [DockPlacement::Right, DockPlacement::Left] {
                if excess <= 0. {
                    break;
                }
                let width = open_width(area, placement);
                let target = (width - excess).max(MIN_SIDE_DOCK_WIDTH);
                if target < width {
                    area.set_dock_size(placement, px(target), window, cx);
                    excess -= width - target;
                }
            }
        });
    }

    /// 插件重新加载（`plugins::mount_all`）之后调：插件面板换成新脚本建的。
    /// 插件增删会改变 `plugin:*` 展开出来的面板，那就整套重装；否则 Dock 不用重搭。
    pub(crate) fn rebuild_plugin_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let area = self.shell_dock.clone();
        let panels = std::mem::take(&mut self.shell_layout.panels);
        let plugins = std::mem::take(&mut self.shell_layout.plugins);
        let before: HashSet<Component> = plugins.keys().cloned().collect();
        let mut builder = Builder::new(cx.weak_entity(), &area, panels, plugins, window);
        builder.sync_plugins(&self.shell_layout.layout, cx);
        let unplaced_changed = builder
            .unplaced
            .iter()
            .any(|target| !before.contains(&Component::Plugin(target.clone())))
            || before.len() != builder.plugins.len();
        let Builder {
            panels, plugins, ..
        } = builder;
        self.shell_layout.panels = panels;
        self.shell_layout.plugins = plugins;
        if unplaced_changed {
            let id = self.shell_layout.id.clone();
            let layout = self.shell_layout.layout.clone();
            self.apply_layout(id, layout, window, cx);
        }
        cx.notify();
    }

    /// 让布局里的一个组件显示出来：放大着就先还原，所在停靠区收着就展开，再选中它。
    /// 布局里没有这个组件就什么都不做，调用方据 `ShellLayout::places` 决定给不给入口。
    pub(crate) fn reveal_layout_component(
        &mut self,
        component: &Component,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.shell_layout.panel_id(component, cx) else {
            return;
        };
        self.shell_dock.update(cx, |area, cx| {
            if area.is_zoomed() {
                area.set_zoomed_out(window, cx);
            }
            let placement =
                SIDE_DOCKS
                    .into_iter()
                    .map(|(placement, _)| placement)
                    .find(|placement| {
                        area.layout(*placement)
                            .is_some_and(|tree| tree.find_panel_node(id).is_some())
                    });
            if let Some(placement) = placement
                && !area.is_dock_open(placement)
            {
                area.toggle_dock(placement, window, cx);
            }
            area.select_panel(id, window, cx);
        });
        cx.notify();
    }

    /// 切开或收起一侧停靠区。放大着先还原：放大时停靠区看不见，开关它没有意义。
    pub(crate) fn toggle_shell_dock(
        &mut self,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell_dock.update(cx, |area, cx| {
            if area.is_zoomed() {
                area.set_zoomed_out(window, cx);
                if area.is_dock_open(placement) {
                    return;
                }
            }
            if area.has_dock(placement) {
                area.toggle_dock(placement, window, cx);
            }
        });
        cx.notify();
    }

    /// 这一侧的停靠区展开着（放大时不算）。
    pub(crate) fn shell_dock_open(&self, placement: DockPlacement, cx: &App) -> bool {
        let area = self.shell_dock.read(cx);
        !area.is_zoomed() && area.is_dock_open(placement)
    }
}

/// 窗口变窄时侧停靠区最窄收到多少。
const MIN_SIDE_DOCK_WIDTH: f32 = 200.;

/// Dock 告诉面板它开始或不再显示（停靠区收起、同组里换了标签、切布局移走）。记在工作台上，
/// 单独摆出来的工具页签据此决定刷不刷自己的缓存；刚露出来时补一次，和在工具面板里切过去一样。
///
/// 推迟到这轮更新结束再写：切布局时 `on_removed` 是在工作台自己的 update 里同步调来的，
/// 当场再 update 工作台会重入。
pub(crate) fn set_displayed(
    workspace: &WeakEntity<Workspace>,
    component: Component,
    displayed: bool,
    cx: &mut App,
) {
    let workspace = workspace.clone();
    cx.defer(move |cx| {
        let _ = workspace.update(cx, |workspace, cx| {
            let is_stage = component == Component::Stage;
            let changed = if displayed {
                workspace.shell_displayed.insert(component)
            } else {
                workspace.shell_displayed.remove(&component)
            };
            if changed && displayed {
                workspace.schedule_visible_refresh(cx);
                // 舞台被同组别的标签挡着时正在看的会话不算看过；露出来这一刻补标。
                if is_stage {
                    workspace.mark_visible_session_read(cx);
                }
            }
        });
    });
}

/// 一个插件面板：插件注册的类建出来的面板，外面包一层宿主的底色。标题条和标签由 Dock 画。
///
/// 建不出来（没装、没声明、没注册、脚本出错）也照样占着位置，把原因画出来：
/// 静默消失的面板最难排查。插件重新加载后由 `refresh` 换成新脚本建的，实体不换。
pub(crate) struct PluginPanel {
    workspace: WeakEntity<Workspace>,
    target: PluginPanelRef,
    /// 布局里写的标题；没写用插件声明的。
    title: Option<String>,
    /// 插件在 `smelt.json` 里声明的标题，建的时候取。
    declared_title: Option<String>,
    /// 按第几次 `plugins::mount_all` 建的。
    generation: u64,
    inner: Result<Arc<dyn BasePanelView>, String>,
    focus_handle: FocusHandle,
}

impl PluginPanel {
    fn new(
        workspace: WeakEntity<Workspace>,
        target: PluginPanelRef,
        title: Option<String>,
        area: &WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let (inner, declared_title) = build_plugin_panel(&target, area, window, cx);
        cx.new(|cx| Self {
            workspace,
            target,
            title,
            declared_title,
            generation: crate::plugins::generation(),
            inner,
            focus_handle: cx.focus_handle(),
        })
    }

    fn set_title(&mut self, title: Option<String>, cx: &mut Context<Self>) {
        if self.title != title {
            self.title = title;
            cx.notify();
        }
    }

    /// 插件重新加载过就换成新脚本建的面板。
    fn refresh(
        &mut self,
        area: &WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let generation = crate::plugins::generation();
        if self.generation == generation {
            return;
        }
        self.release(window, cx);
        let (inner, declared_title) = build_plugin_panel(&self.target, area, window, cx);
        self.inner = inner;
        self.declared_title = declared_title;
        self.generation = generation;
        cx.notify();
    }

    /// 不再用这个面板：让插件释放它为这份面板留着的东西（脚本侧的 `release`）。
    /// Dock 把面板移走时不调这个：换布局时同一个面板会被移走再放进新的一套。
    fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inner = std::mem::replace(&mut self.inner, Err("面板已经释放。".to_string()));
        if let Ok(inner) = inner {
            inner.on_removed(window, cx);
        }
    }

    fn title_text(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.declared_title.clone())
            .unwrap_or_else(|| self.target.panel.clone())
    }

    fn content(&self, cx: &App) -> AnyElement {
        match &self.inner {
            // 底色由宿主铺，和文件树等内置面板一样是实底的舞台色：主题原色可能带透明度，
            // 插件自己铺会透出窗口后面的东西。插件只画内容，不画页面底。
            Ok(inner) => div()
                .flex_1()
                .min_h_0()
                .size_full()
                .bg(rgb(ui_theme::bg_stage()))
                .child(inner.view())
                .into_any_element(),
            Err(reason) => {
                crate::placeholder_view(reason, cx.theme().muted_foreground).into_any_element()
            }
        }
    }
}

/// 按插件注册的类建一份面板，`props` 交给脚本的 `deserialize`。能不能建看挂着哪些插件、
/// 它们声明了什么（`plugins::panel_target`），不看 Dock 的注册表：删掉的插件注册过的类还在。
fn build_plugin_panel(
    target: &PluginPanelRef,
    area: &WeakEntity<DockArea>,
    window: &mut Window,
    cx: &mut App,
) -> (Result<Arc<dyn BasePanelView>, String>, Option<String>) {
    let found = match crate::plugins::panel_target(&target.plugin, &target.panel) {
        Ok(found) => found,
        Err(reason) => return (Err(reason), None),
    };
    let props = target
        .props
        .as_ref()
        .map_or(serde_json::Value::Null, |props| props.value());
    let state = PanelState::new(found.registered_name);
    let info = PanelInfo::panel(props);
    let context = PanelBuildContext::new(area.clone(), &state, &info);
    let inner = match PanelRegistry::build_panel(found.registered_name, context, window, cx) {
        // 注册过、脚本也建出来了。脚本 `build` 抛错时 gpui-shell 给的是一个什么都不画的
        // 占位面板，认不出 `ScriptPanel` 就是这种。
        Some(panel)
            if panel
                .as_any()
                .downcast_ref::<Entity<ScriptPanel>>()
                .is_some() =>
        {
            Ok(panel)
        }
        Some(_) => Err(format!(
            "面板 `{}` 的脚本出错了，没有建出来，原因见日志。",
            target.panel
        )),
        None => Err(format!(
            "插件 `{}` 声明了面板 `{}`，但代码里没有注册：在根视图的 `init()` 里调 \
             `DockArea.register_panel(\"{}\", 类)`。",
            target.plugin, target.panel, target.panel
        )),
    };
    if let Err(reason) = &inner {
        eprintln!("[plugins] {}/{}：{reason}", target.plugin, target.panel);
    }
    (inner, Some(found.title))
}

impl EventEmitter<PanelEvent> for PluginPanel {}

impl Focusable for PluginPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.inner {
            Ok(inner) => inner.focus_handle(cx),
            Err(_) => self.focus_handle.clone(),
        }
    }
}

impl BasePanel for PluginPanel {
    fn panel_name(&self) -> &'static str {
        "smelt.plugin"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        true
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(inner) = &self.inner {
            inner.set_active(active, window, cx);
        }
        set_displayed(
            &self.workspace,
            Component::Plugin(self.target.clone()),
            active,
            cx,
        );
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        set_displayed(
            &self.workspace,
            Component::Plugin(self.target.clone()),
            false,
            cx,
        );
    }
}

impl Panel for PluginPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title_text()
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        Some(PanelControl::Toolbar)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for PluginPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 当前项目在不在本机、能不能用，插件经 `context()` 自己看（见 workspace_plugins）。
        self.content(cx)
    }
}
