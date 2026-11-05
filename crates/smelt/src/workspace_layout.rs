//! 工作台布局：把布局文件搭成外层 Dock，切换、恢复默认，以及插件面板。
//!
//! 格式和校验在 `smelt_client::layout`。这里只管窗口里的事：组件名对到面板实体，
//! 声明树翻成 `DockLayout` 装进 `workspace_shell` 建好的那个 `DockArea`。
//! 面板实体按组件名缓存，切换布局时复用，会话列表、舞台里的终端和插件脚本都不重建。
//!
//! 插件面板（`plugin:<id>/<面板>`）由插件自己注册的类建出来，外面包一层 smelt 的
//! `PluginPanel`：标题、底色和「没装 / 没声明 / 没注册 / 加载失败」的说明归宿主，内容归插件。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, WeakEntity,
    Window, div, prelude::*, px, rgb,
};
use gpui_component::ActiveTheme;
use gpui_component::dock::{
    BasePanel, BasePanelView, DockArea, DockLayout, DockPlacement, Panel, PanelBuildContext,
    PanelEvent, PanelInfo, PanelRegistry, PanelState, panel_handle,
};
use gpui_shell::dock::ScriptPanel;
use smelt_client::layout::{
    self, Axis, Component, Layout, LayoutEntry, Node, PluginPanelRef, Region, Tab,
};

use crate::tool_panel::ToolPanelTab;
use crate::workspace_shell::{ShellContent, ShellPanel};
use crate::{Workspace, ui_theme, workspace_frame};

/// 没写 `size` 的停靠区多宽。会话列表和工具面板所在的停靠区不看这个，用各自存的宽度。
const DEFAULT_DOCK_SIZE: f32 = 320.;

/// 当前装着的布局，和从它算出来、每帧要用的几样东西。
pub(crate) struct ShellLayout {
    pub(crate) id: String,
    pub(crate) layout: Layout,
    /// 会话列表在哪个停靠区。只认左右：开合和宽度由 `sidebar_open` / `sidebar_w` 管。
    pub(crate) sessions_dock: Option<DockPlacement>,
    /// 工具面板在哪个停靠区。只认左右，和会话列表不在同一个。
    pub(crate) tools_dock: Option<DockPlacement>,
    /// 中间区域左上角是不是舞台。左停靠区收起或没有时，靠它判断舞台要不要让红黄绿按钮。
    pub(crate) stage_top_left_of_center: bool,
    /// 组件 → 面板，正好是这套布局里的全部组件。切换布局时复用。
    panels: HashMap<Component, Arc<dyn BasePanelView>>,
    /// 插件面板：摆进布局的（也在 `panels` 里），和没摆、落在工具面板里的。
    plugins: HashMap<Component, Entity<PluginPanel>>,
}

impl ShellLayout {
    fn new(
        id: String,
        layout: Layout,
        panels: HashMap<Component, Arc<dyn BasePanelView>>,
        plugins: HashMap<Component, Entity<PluginPanel>>,
    ) -> Self {
        let side_dock = |component: &Component| match layout.region_of(component) {
            Some(Region::Left) => Some(DockPlacement::Left),
            Some(Region::Right) => Some(DockPlacement::Right),
            _ => None,
        };
        let sessions_dock = side_dock(&Component::Sessions);
        let tools_dock = side_dock(&Component::Tools).filter(|dock| Some(*dock) != sessions_dock);
        let stage_top_left_of_center = layout.center_top_left() == Some(&Component::Stage);
        Self {
            id,
            layout,
            sessions_dock,
            tools_dock,
            stage_top_left_of_center,
            panels,
            plugins,
        }
    }

    /// 工具面板里一个插件页签的内容。只读内存。
    pub(crate) fn plugin_content(&self, target: &PluginPanelRef, cx: &App) -> AnyElement {
        match self.plugins.get(&Component::Plugin(target.clone())) {
            Some(panel) => panel.read(cx).content(cx),
            // 插件删掉以后，别的会话里还记着它的页签，切回去时到这里。
            None => {
                crate::placeholder_view("这个插件面板已经没有了。", cx.theme().muted_foreground)
                    .into_any_element()
            }
        }
    }

    /// 布局里有没有摆这个组件。摆出来的插件和工具页签不再出现在工具面板里。
    pub(crate) fn places(&self, component: &Component) -> bool {
        self.panels.contains_key(component)
    }

    /// 这个停靠区的开合、宽度归工作台的字段管，不按布局文件里的初值。
    fn managed(&self, placement: DockPlacement) -> bool {
        self.sessions_dock == Some(placement) || self.tools_dock == Some(placement)
    }
}

/// 第一次装：默认布局。`area` 刚建好，还是空的。插件必须已经挂载（`plugins::mount_all`）：
/// 插件面板按注册过的类建。
pub(crate) fn install_default(
    workspace: WeakEntity<Workspace>,
    area: &Entity<DockArea>,
    window: &mut Window,
    cx: &mut App,
) -> ShellLayout {
    let builder = Builder::new(workspace, area, HashMap::new(), HashMap::new(), window);
    install(
        builder,
        area,
        layout::DEFAULT_ID.to_string(),
        Layout::default_layout(),
        cx,
    )
}

fn install(
    mut builder: Builder<'_>,
    area: &Entity<DockArea>,
    id: String,
    layout: Layout,
    cx: &mut App,
) -> ShellLayout {
    // 这套布局用不到的面板不留：留着的实体还订阅着工作台，每次通知都会跟着重画。
    let wanted: HashSet<Component> = layout.components().into_iter().cloned().collect();
    builder
        .panels
        .retain(|component, _| wanted.contains(component));
    builder.sync_plugins(&layout, cx);

    let center = builder.build_node(&layout.center, cx);
    let docks: Vec<_> = [
        (DockPlacement::Left, Region::Left),
        (DockPlacement::Right, Region::Right),
        (DockPlacement::Bottom, Region::Bottom),
    ]
    .into_iter()
    .map(|(placement, region)| {
        let dock = layout.dock(region).map(|spec| {
            (
                builder.build_node(&spec.node, cx),
                spec.size.unwrap_or(DEFAULT_DOCK_SIZE),
                spec.open,
            )
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
    let shell = ShellLayout::new(id, layout, panels, plugins);

    area.update(cx, |area, cx| {
        area.set_center(center, window, cx);
        for (placement, dock) in docks {
            let Some((node, size, open)) = dock else {
                area.remove_dock(placement, window, cx);
                continue;
            };
            area.set_dock(placement, node, window, cx);
            // 放会话列表、工具面板的停靠区由 `align_shell_dock` 每帧对齐，这里不动。
            if !shell.managed(placement) {
                area.set_dock_size(placement, px(size), window, cx);
                if area.is_dock_open(placement) != open {
                    area.toggle_dock(placement, window, cx);
                }
            }
        }
        // 锁住只禁拖动和放下，开合与拖宽照常。
        area.set_locked(true, window, cx);
    });
    shell
}

/// 搭一套布局时手里的东西：上一套留下、可以复用的面板，和建新面板要用的窗口、Dock。
struct Builder<'a> {
    workspace: WeakEntity<Workspace>,
    area: WeakEntity<DockArea>,
    panels: HashMap<Component, Arc<dyn BasePanelView>>,
    plugins: HashMap<Component, Entity<PluginPanel>>,
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
            window,
        }
    }

    /// 插件面板对齐到这套布局和眼下挂着的插件：用不到的释放，插件重新加载过的重建，
    /// 没摆进布局的声明面板各建一份给工具面板。摆进布局的由 `build_node` 建。
    fn sync_plugins(&mut self, layout: &Layout, cx: &mut App) {
        let mut wanted: HashSet<Component> = layout
            .plugin_panels()
            .into_iter()
            .map(|target| Component::Plugin(target.clone()))
            .collect();
        let unplaced: Vec<PluginPanelRef> = crate::plugins::slots()
            .into_iter()
            .filter_map(crate::plugins::panel_of)
            .filter(|(plugin, panel)| !layout.mentions_plugin_panel(plugin, panel))
            .map(|(plugin, panel)| PluginPanelRef::new(plugin, panel))
            .collect();
        wanted.extend(unplaced.iter().cloned().map(Component::Plugin));

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
        for target in unplaced {
            self.plugin(&target, None, cx);
        }
    }

    /// 取一个插件面板，没有就建。`title` 是布局给的标题，复用的面板也换上它。
    fn plugin(
        &mut self,
        target: &PluginPanelRef,
        title: Option<String>,
        cx: &mut App,
    ) -> Entity<PluginPanel> {
        let component = Component::Plugin(target.clone());
        if let Some(panel) = self.plugins.get(&component) {
            panel.update(cx, |panel, cx| panel.set_title(title, cx));
            return panel.clone();
        }
        let panel = PluginPanel::new(
            self.workspace.clone(),
            target.clone(),
            title,
            &self.area,
            self.window,
            cx,
        );
        self.plugins.insert(component, panel.clone());
        panel
    }

    fn build_node(&mut self, node: &Node, cx: &mut App) -> DockLayout {
        match node {
            Node::Split { axis, children } => {
                let split = match axis {
                    Axis::Row => DockLayout::h_split(),
                    Axis::Column => DockLayout::v_split(),
                };
                children.iter().fold(split, |split, (child, size)| {
                    split.child(self.build_node(child, cx), size.map(px))
                })
            }
            Node::Tabs { tabs, active } => {
                let mut group = DockLayout::tabs();
                for Tab { component, title } in tabs {
                    let panel = match component {
                        Component::Plugin(target) => {
                            let plugin = self.plugin(target, title.clone(), cx);
                            self.panels
                                .entry(component.clone())
                                .or_insert_with(|| panel_handle(plugin))
                                .clone()
                        }
                        builtin => {
                            let workspace = &self.workspace;
                            self.panels
                                .entry(builtin.clone())
                                .or_insert_with(|| builtin_panel(builtin, workspace, cx))
                                .clone()
                        }
                    };
                    group = group.panel_view(panel, cx);
                }
                group.active_index(*active)
            }
        }
    }
}

fn builtin_panel(
    component: &Component,
    workspace: &WeakEntity<Workspace>,
    cx: &mut App,
) -> Arc<dyn BasePanelView> {
    let content = match ToolPanelTab::from_builtin_component(component) {
        Some(tab) => ShellContent::Tool(tab),
        None => {
            let name = component.name();
            ShellContent::Region(
                crate::workspace_shell::builtin(&name)
                    .unwrap_or_else(|| panic!("内置组件 `{name}` 没有登记面板")),
            )
        }
    };
    let component = component.clone();
    panel_handle(cx.new(|cx| ShellPanel::new(workspace.clone(), component, content, cx)))
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
        // 停靠区可能换了内容。上次写进去的宽度以现在 Dock 上的为准，
        // 不然下一帧对齐会把差值当成用户拖了分隔条。
        let dock = self.shell_dock.read(cx);
        let width = |placement: Option<DockPlacement>, fallback: f32| {
            placement
                .and_then(|placement| dock.dock_size(placement))
                .map_or(fallback, f32::from)
        };
        self.shell_applied_sidebar = width(self.shell_layout.sessions_dock, self.sidebar_w);
        self.shell_applied_tool = width(self.shell_layout.tools_dock, self.tool_panel_w);
        cx.notify();
    }

    /// 插件重新加载（`plugins::mount_all`）之后调：插件面板换成新脚本建的，插件增删了的
    /// 工具面板页签跟着增删。摆进布局的面板实体不换，Dock 不用重搭，停靠区的开合和宽度不动。
    pub(crate) fn rebuild_plugin_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let area = self.shell_dock.clone();
        let panels = std::mem::take(&mut self.shell_layout.panels);
        let plugins = std::mem::take(&mut self.shell_layout.plugins);
        let mut builder = Builder::new(cx.weak_entity(), &area, panels, plugins, window);
        builder.sync_plugins(&self.shell_layout.layout, cx);
        // 摆进布局的插件面板 `sync_plugins` 只重建、不新建：它们在装这套布局时都建过了。
        let Builder {
            panels, plugins, ..
        } = builder;
        self.shell_layout.panels = panels;
        self.shell_layout.plugins = plugins;
        cx.notify();
    }

    /// 让布局里的一个组件显示出来：所在停靠区收着就展开，同组里有别的标签就选中它。
    /// 会话列表、工具面板所在的停靠区归各自的开关字段管，改字段，下一帧对齐。
    pub(crate) fn reveal_layout_component(
        &mut self,
        component: &Component,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self
            .shell_layout
            .panels
            .get(component)
            .map(|panel| panel.panel_id(cx))
        else {
            return;
        };
        let placement = match self.shell_layout.layout.region_of(component) {
            Some(Region::Left) => Some(DockPlacement::Left),
            Some(Region::Right) => Some(DockPlacement::Right),
            Some(Region::Bottom) => Some(DockPlacement::Bottom),
            Some(Region::Center) | None => None,
        };
        if let Some(placement) = placement {
            if self.shell_layout.sessions_dock == Some(placement) {
                self.sidebar_open = true;
            } else if self.shell_layout.tools_dock == Some(placement) {
                self.set_tool_panel_open(true, cx);
            } else {
                self.shell_dock.update(cx, |area, cx| {
                    if !area.is_dock_open(placement) {
                        area.toggle_dock(placement, window, cx);
                    }
                });
            }
        }
        self.shell_dock
            .update(cx, |area, cx| area.select_panel(id, window, cx));
        cx.notify();
    }
}

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
            let changed = if displayed {
                workspace.shell_displayed.insert(component)
            } else {
                workspace.shell_displayed.remove(&component)
            };
            if changed && displayed {
                workspace.schedule_visible_refresh(cx);
            }
        });
    });
}

/// 一个插件面板：插件注册的类建出来的面板，外面包一层宿主的标题和底色。
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

    /// 面板内容，不带标题条：工具面板里的页签用它，布局里的面板再套一层标题。
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
        false
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

    fn title_bar(&self, _: &App) -> bool {
        false
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for PluginPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 当前项目在不在本机、能不能用，插件经 `context()` 自己看（见 workspace_plugins）。
        titled_panel(self.title_text(), self.content(cx), cx)
    }
}

/// 布局里单独占一块的面板：上面一条标题，下面是内容。插件和单独摆出来的文件、变更、历史都用它。
/// 它们不管窗口外框：标题条能拖窗口，红黄绿按钮压在它上面也不挡内容。
pub(crate) fn titled_panel(title: impl IntoElement, body: AnyElement, cx: &App) -> gpui::Div {
    let muted = cx.theme().muted_foreground;
    let surface: Hsla = rgb(ui_theme::bg_column()).into();
    let title_band = workspace_frame::with_window_drag(
        div()
            .flex_none()
            .h(workspace_frame::TOP_BAR_HEIGHT)
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(muted)
            .child(title),
    );
    div().size_full().px(ui_theme::chrome_gap() / 2.).child(
        workspace_frame::card(surface)
            .flex_col()
            .child(title_band)
            .child(div().flex_1().min_h_0().flex().child(body)),
    )
}
