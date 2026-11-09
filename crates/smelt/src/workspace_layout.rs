//! 工作台布局：装哪套布局、存它点出来的样子、切换 / 重置 / 复制、插件的布局命令。
//!
//! 布局文件和存档都是官方的 `DockAreaState`（见 `layout_files`、`docs/workspace-layouts.md`），
//! 整个工作台是一块 `DockArea`（`Workspace::shell_dock`），怎么画见 `workspace_shell`。
//! Dock 是唯一的一份：哪组选中哪个、放大、dock 开合、拆分大小都只在它那里，这里只读它、
//! 让它改，再把 `dump()` 原样存进库。
//!
//! 两个事件：
//! - `DockEvent::LayoutChanged`（树改了：装布局、拖动、选中、开合、拖分隔条）：重算舞台、
//!   存档、同步显示；
//! - Dock 的通知（放大、还原不发 `LayoutChanged`）：同步显示。

use std::collections::HashSet;

use gpui::{App, Context, Entity, Subscription, Window};
use gpui_component::dock::{
    BasePanelView, DockArea, DockAreaState, DockEvent, DockPlacement, InsertTarget, NodeId,
    PaneNode, PaneRef, PanelBuildContext, PanelId, PanelInfo, PanelRegistry, PanelState,
};
use serde_json::Value;
use smelt_client_store::LayoutStateRecord;

use crate::Workspace;
use crate::layout_files::{self, LayoutEntry, LayoutFile};
use crate::plugins::{
    LayoutCommand, LayoutDock, LayoutDockSide, LayoutNodeInfo, LayoutPanelInfo, LayoutSnapshot,
};
use crate::workspace_shell::{self, Builtin, ChromeSlots};
use crate::workspace_stages::{Accepts, StageSpec};

/// 装着的是哪套布局、它的文件。点出来的样子不在这里，在 Dock 上。
pub(crate) struct ShellLayout {
    /// 布局 id：默认布局是 `default`，用户布局是文件名，插件带的是 `<插件 id>/<文件名>`。
    pub(crate) id: String,
    /// 装它时读的文件。存档记着它的指纹。
    pub(crate) file: LayoutFile,
}

impl Default for ShellLayout {
    fn default() -> Self {
        Self {
            id: layout_files::DEFAULT_ID.to_string(),
            file: LayoutFile::default_layout(),
        }
    }
}

/// 全部布局：用户布局目录里的，再加上已挂载插件带的。读文件，只在启动、打开命令面板这类
/// 事件里调，不在 render 里。默认布局写在代码里，不在这里面。
pub(crate) fn discover_layouts() -> Vec<LayoutEntry> {
    let mut entries = layout_files::user_layout_dir()
        .map(|dir| layout_files::discover(&dir))
        .unwrap_or_default();
    entries.extend(crate::plugins::layouts());
    entries
}

/// 这套布局存过的状态，和库里那段原文。文件改过（指纹对不上）、格式不认识都当没存过。
fn saved_state(id: &str, file: &LayoutFile) -> Option<(DockAreaState, String)> {
    let record = match smelt_client_store::client_store().and_then(|store| store.layout_state(id)) {
        Ok(record) => record?,
        Err(error) => {
            eprintln!("[layout] 读布局 `{id}` 的存档失败：{error}");
            return None;
        }
    };
    let state = layout_files::saved_state(&record, file)?;
    Some((state, record.state))
}

/// 订阅整个工作台的 Dock：树改了重算舞台、存档；放大、还原只发通知，同步显示。
pub(crate) fn watch_shell_dock(
    area: &Entity<DockArea>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Vec<Subscription> {
    vec![
        cx.subscribe_in(area, window, |this, _, event: &DockEvent, window, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.shell_layout_changed(window, cx);
            }
        }),
        cx.observe_in(area, window, |this, _, window, cx| {
            this.sync_layout_visibility(window, cx);
        }),
    ]
}

impl Workspace {
    /// 启动时装上次用的布局。没有、读不出来或不合法就装默认布局并提示，库里记着的选择不改：
    /// 布局文件修好了，下次启动还是它。插件必须已经挂载（`plugins::mount_all`），工作台必须
    /// 已经建好（内置面板要订阅它），所以由 `Workspace::new` 推迟到建完再调。
    pub(crate) fn install_initial_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (id, file) = match self.layout_choice.clone() {
            None => (
                layout_files::DEFAULT_ID.to_string(),
                LayoutFile::default_layout(),
            ),
            Some(id) => match self.layout_catalog.iter().find(|entry| entry.id == id) {
                Some(LayoutEntry {
                    layout: Ok(file), ..
                }) => (id, file.clone()),
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
                    (
                        layout_files::DEFAULT_ID.to_string(),
                        LayoutFile::default_layout(),
                    )
                }
            },
        };
        self.load_layout(id, file, true, window, cx);
    }

    /// 重读布局目录。读文件，只在打开命令面板、切换这类事件里调，不在 render 里。
    pub(crate) fn reload_layout_catalog(&mut self) {
        self.layout_catalog = discover_layouts();
    }

    /// 命令面板里和布局有关的条目。只读缓存的目录和 Dock。
    pub(crate) fn layout_palette_entries(&self, cx: &App) -> Vec<(String, crate::Cmd)> {
        let current = &self.shell_layout.id;
        let mut entries = Vec::new();
        for entry in &self.layout_catalog {
            if &entry.id == current {
                continue;
            }
            let label = match &entry.layout {
                Ok(_) => format!("切换布局：{}", entry.id),
                Err(_) => format!("切换布局：{}（有错误）", entry.id),
            };
            entries.push((
                label,
                layout_cmd(LayoutCommand::Switch {
                    layout: entry.id.clone(),
                }),
            ));
        }
        if current != layout_files::DEFAULT_ID {
            entries.push((
                "切换到默认布局".to_string(),
                layout_cmd(LayoutCommand::Switch {
                    layout: layout_files::DEFAULT_ID.to_string(),
                }),
            ));
        }
        entries.push(("重置当前布局".to_string(), layout_cmd(LayoutCommand::Reset)));
        entries.push((
            "复制当前布局".to_string(),
            layout_cmd(LayoutCommand::Copy { layout: None }),
        ));
        // 布局里没有的面板：内置的（舞台只能布局文件里摆）和插件声明的。
        let area = self.shell_dock.read(cx);
        let absent = |name: &str| workspace_shell::find_panel(area, name, cx).is_none();
        let builtins = Builtin::ALL
            .into_iter()
            .filter(|&builtin| builtin != Builtin::Stage)
            .map(|builtin| (builtin.name().to_string(), builtin.title().to_string()));
        let plugins = crate::plugins::declared()
            .into_iter()
            .map(|target| (target.registered_name.to_string(), target.title));
        for (name, title) in builtins.chain(plugins) {
            if absent(&name) {
                entries.push((
                    format!("添加面板：{title}"),
                    layout_cmd(LayoutCommand::Add {
                        panel: name,
                        to: None,
                    }),
                ));
            }
        }
        entries
    }

    /// 切到一套布局。用户和插件的每次都重读文件，改过的布局再切一次就生效；不合法就不动
    /// 当前布局，提示原因。
    pub(crate) fn switch_layout(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.find_layout_file(id) {
            Ok(file) => self.apply_layout(id.to_string(), file, true, window, cx),
            Err(error) => crate::status_item::notify_error(error),
        }
    }

    /// 重读目录，按 id 找布局文件。默认布局写在代码里。
    fn find_layout_file(&mut self, id: &str) -> Result<LayoutFile, String> {
        if id == layout_files::DEFAULT_ID {
            return Ok(LayoutFile::default_layout());
        }
        self.reload_layout_catalog();
        let entry = self
            .layout_catalog
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("找不到布局 `{id}`"))?;
        entry
            .layout
            .clone()
            .map_err(|error| format!("布局 `{id}` 没有加载（{}）：{error}", entry.path.display()))
    }

    /// 装一套布局，记下选的是哪套（默认布局记成没选）。`use_saved` 为 false 时不看存档，
    /// 按文件装：装完的第一次存档就盖掉旧的，这就是重置。
    fn apply_layout(
        &mut self,
        id: String,
        file: LayoutFile,
        use_saved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout_choice = (id != layout_files::DEFAULT_ID).then(|| id.clone());
        self.load_layout(id, file, use_saved, window, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// 把布局装进 Dock。舞台、存档、显示由装完发出的 `LayoutChanged` 跟上。
    fn load_layout(
        &mut self,
        id: String,
        file: LayoutFile,
        use_saved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = use_saved.then(|| saved_state(&id, &file)).flatten();
        let (state, written) = match saved {
            Some((state, text)) => (state, Some(text)),
            None => (file.state.clone(), None),
        };
        self.shell_layout = ShellLayout { id, file };
        // 按存档装的，装完 dump 出来和库里一样就不用再写。
        self.layout_state_written = written;
        self.load_shell_state(state, window, cx);
    }

    fn load_shell_state(
        &mut self,
        state: DockAreaState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.shell_dock.clone();
        if let Err(error) = workspace_shell::load(&area, cx.weak_entity(), state, window, cx) {
            crate::status_item::notify_error(format!(
                "布局 `{}` 没装上：{error:#}",
                self.shell_layout.id
            ));
        }
    }

    /// Dock 的树改了：舞台按新树重算，存档，同步显示，告诉插件。
    fn shell_layout_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let specs: Vec<StageSpec> = workspace_shell::shell_panels(self.shell_dock.read(cx))
            .into_iter()
            .filter_map(|(panel, entity)| {
                let shell = entity.read(cx);
                (shell.builtin() == Builtin::Stage).then(|| StageSpec {
                    id: workspace_shell::stage_id(&entity),
                    accepts: Accepts::from_info(shell.info()),
                    panel,
                })
            })
            .collect();
        self.stages.set_specs(specs);
        // 舞台换了：焦点、各块显示什么按新树的 `accepts` 重新理。
        self.settle_stages(cx);
        self.save_layout_state(cx);
        self.sync_layout_visibility(window, cx);
        self.publish_to_plugins(cx);
        cx.notify();
    }

    /// 把 Dock 现在的样子（`dump()` 原样）存进库。和上次写的一样就不写。
    pub(crate) fn save_layout_state(&mut self, cx: &App) {
        let state = layout_files::to_text(&self.shell_dock.read(cx).dump(cx));
        if self.layout_state_written.as_deref() == Some(state.as_str()) {
            return;
        }
        let record = LayoutStateRecord {
            layout_id: self.shell_layout.id.clone(),
            file_digest: self.shell_layout.file.digest.clone(),
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

    /// 哪些面板看得见变了：刚露出来的内置页刷一次缓存，有焦点的舞台露出来时把正在看的
    /// 会话标成看过；再把 `layout()` 发给插件。插件面板显示与否由 Dock 自己告诉它。
    pub(crate) fn sync_layout_visibility(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let area = self.shell_dock.read(cx);
        let shown = workspace_shell::shown_panels(area);
        let displayed: HashSet<Builtin> = workspace_shell::shell_panels(area)
            .into_iter()
            .filter(|(panel, _)| shown.contains(panel))
            .map(|(_, entity)| entity.read(cx).builtin())
            .collect();
        let changed = shown != self.shell_shown;
        let revealed = displayed.difference(&self.shell_displayed).next().is_some();
        self.shell_shown = shown;
        self.shell_displayed = displayed;
        if revealed {
            self.schedule_visible_refresh(cx);
        }
        // 舞台被挡着时正在看的会话不算看过；露出来这一刻补标。
        if changed && self.focused_stage_shown() {
            self.mark_visible_session_read(cx);
        }
        self.publish_layout(window, cx);
        if changed {
            cx.notify();
        }
    }

    /// 把 `layout()` 发给插件。内容没变不叫醒插件；红黄绿按钮全屏时会藏起来，窗口变化时也调。
    pub(crate) fn publish_layout(&self, window: &Window, cx: &App) {
        crate::plugins::set_layout(self.layout_snapshot(window, cx));
    }

    /// 给插件的 `layout()`：Dock 的树，贴角的是哪组。
    fn layout_snapshot(&self, window: &Window, cx: &App) -> LayoutSnapshot {
        let area = self.shell_dock.read(cx);
        let shown = &self.shell_shown;
        let names = |node: NodeId| {
            workspace_shell::group_panels(area, node).map(|panels| {
                panels
                    .iter()
                    .filter_map(|&panel| area.panel(panel))
                    .map(|panel| workspace_shell::layout_name(panel, cx))
                    .collect::<Vec<_>>()
            })
        };
        let dock = |placement: DockPlacement| {
            area.layout(placement).map(|tree| LayoutDock {
                open: area.is_dock_open(placement),
                root: node_info(area, tree.root(), shown, cx),
            })
        };
        let slots = ChromeSlots::of(area);
        LayoutSnapshot {
            id: self.shell_layout.id.clone(),
            center: area
                .layout(DockPlacement::Center)
                .map(|tree| node_info(area, tree.root(), shown, cx)),
            left: dock(DockPlacement::Left),
            right: dock(DockPlacement::Right),
            bottom: dock(DockPlacement::Bottom),
            zoomed: area.zoomed_group().and_then(names),
            top_left: slots.top_left.and_then(names),
            top_right: slots.top_right.and_then(names),
            lights: f32::from(workspace_shell::lights_width(window)),
        }
    }

    /// Esc：还原放大的组。没放大着返回 false。
    pub(crate) fn unzoom_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.shell_dock.read(cx).is_zoomed() {
            return false;
        }
        self.shell_dock
            .update(cx, |area, cx| area.set_zoomed_out(window, cx));
        true
    }

    /// 布局命令：插件经 `run("layout.…", args)` 发来的，和命令面板里的。
    pub(crate) fn run_layout_command(
        &mut self,
        command: LayoutCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match command {
            LayoutCommand::Select { panel } => self.panel_named(&panel, cx).map(|id| {
                self.reveal_panel(id, window, cx);
            }),
            LayoutCommand::Zoom { panel } => self.panel_named(&panel, cx).map(|id| {
                self.toggle_zoom(id, window, cx);
            }),
            LayoutCommand::Add { panel, to } => self.add_panel(&panel, to.as_deref(), window, cx),
            LayoutCommand::Focus { id } => {
                if self.stages.panel_of(&id).is_some() {
                    self.focus_stage(&id, window, cx);
                    Ok(())
                } else {
                    Err(format!("布局里没有舞台 `{id}`"))
                }
            }
            LayoutCommand::ToggleDock { dock } => self.toggle_dock(dock, window, cx),
            LayoutCommand::Switch { layout } => {
                self.switch_layout(&layout, window, cx);
                Ok(())
            }
            LayoutCommand::Reset => {
                let id = self.shell_layout.id.clone();
                self.find_layout_file(&id)
                    .map(|file| self.apply_layout(id, file, false, window, cx))
            }
            LayoutCommand::Copy { layout } => {
                let from = layout.unwrap_or_else(|| self.shell_layout.id.clone());
                self.copy_layout(&from, window, cx)
            }
        };
        if let Err(error) = result {
            crate::status_item::notify_error(error);
        }
    }

    fn panel_named(&self, name: &str, cx: &App) -> Result<PanelId, String> {
        workspace_shell::find_panel(self.shell_dock.read(cx), name, cx)
            .ok_or_else(|| format!("布局里没有面板 `{name}`"))
    }

    /// 放大这块面板所在的组；已经放大的是这组就还原。放大前先让它露出来。
    fn toggle_zoom(&mut self, panel: PanelId, window: &mut Window, cx: &mut Context<Self>) {
        let area = self.shell_dock.read(cx);
        let Some(group) = workspace_shell::group_of(area, panel) else {
            return;
        };
        if area.zoomed_group() == Some(group) {
            self.shell_dock
                .update(cx, |area, cx| area.set_zoomed_out(window, cx));
            return;
        }
        self.reveal_panel(panel, window, cx);
        self.shell_dock
            .update(cx, |area, cx| area.set_zoomed_in(group, window, cx));
    }

    /// 开合官方的一块 dock。布局没有这块 dock 时提示。
    fn toggle_dock(
        &mut self,
        dock: LayoutDockSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let placement = dock.placement();
        if !self.shell_dock.read(cx).has_dock(placement) {
            return Err(format!("这套布局没有 {} 这块 dock", dock.name()));
        }
        self.shell_dock
            .update(cx, |area, cx| area.toggle_dock(placement, window, cx));
        Ok(())
    }

    /// 把布局里没有的面板加进来：给了 `to` 进它那一组，不然在中间的树最右边另起一组。
    /// 已经有了就切过去。
    fn add_panel(
        &mut self,
        name: &str,
        to: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Ok(existing) = self.panel_named(name, cx) {
            self.reveal_panel(existing, window, cx);
            return Ok(());
        }
        let target = match to {
            Some(to) => {
                let beside = self.panel_named(to, cx)?;
                let node = workspace_shell::group_of(self.shell_dock.read(cx), beside)
                    .ok_or_else(|| format!("布局里没有面板 `{to}`"))?;
                InsertTarget::Tabs {
                    node,
                    ix: None,
                    activate: true,
                }
            }
            None => InsertTarget::Split {
                node: self
                    .shell_dock
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .map(|tree| tree.root().id())
                    .ok_or("布局还没装好")?,
                placement: gpui_component::Placement::Right,
                size: None,
            },
        };
        let panel = self.build_panel(name, window, cx)?;
        let id = panel.panel_id(cx);
        self.shell_dock.update(cx, |area, cx| {
            // Dock 只能先把面板放进某一组，再挪到要去的地方。
            area.add_panel_view(panel, DockPlacement::Center, None, window, cx);
            area.move_panel(id, target, window, cx);
        });
        self.reveal_panel(id, window, cx);
        Ok(())
    }

    /// 按面板名新建一块面板：内置的直接建，插件的经 Dock 的注册表建（和装布局时一样）。
    fn build_panel(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<std::sync::Arc<dyn BasePanelView>, String> {
        if let Some(builtin) = Builtin::from_name(name) {
            return Ok(workspace_shell::new_builtin(cx.weak_entity(), builtin, cx));
        }
        let Some((plugin, panel)) = workspace_shell::plugin_panel(name) else {
            return Err(format!("不认识的面板 `{name}`"));
        };
        let target = crate::plugins::panel_target(plugin, panel)?;
        let state = PanelState {
            panel_name: target.registered_name.to_string(),
            children: Vec::new(),
            info: PanelInfo::panel(Value::Null),
        };
        let context = PanelBuildContext::new(self.shell_dock.downgrade(), &state, &state.info);
        PanelRegistry::build_panel(target.registered_name, context, window, cx)
            .ok_or_else(|| workspace_shell::unbuilt_reason(name))
    }

    /// 把一套布局复制到用户布局目录，复制完切过去。当前这套复制的是 Dock 现在的样子，
    /// 别的复制文件。默认布局也能复制，拿它当起点改。
    fn copy_layout(
        &mut self,
        from: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let dir = layout_files::user_layout_dir().ok_or("找不到 smelt 的主目录，没法复制布局")?;
        let text = if from == self.shell_layout.id {
            layout_files::to_text(&self.shell_dock.read(cx).dump(cx))
        } else if from == layout_files::DEFAULT_ID {
            layout_files::DEFAULT_LAYOUT.to_string()
        } else {
            self.reload_layout_catalog();
            let entry = self
                .layout_catalog
                .iter()
                .find(|entry| entry.id == from)
                .ok_or_else(|| format!("找不到布局 `{from}`"))?;
            std::fs::read_to_string(&entry.path)
                .map_err(|error| format!("读不了 {}：{error}", entry.path.display()))?
        };
        let id = layout_files::copy_id(from, |id| dir.join(format!("{id}.json")).exists());
        let path = dir.join(format!("{id}.json"));
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, text))
            .map_err(|error| format!("写不了 {}：{error}", path.display()))?;
        self.switch_layout(&id, window, cx);
        crate::status_item::notify_success(format!("已复制到 {}", path.display()));
        Ok(())
    }

    /// 插件重新加载（`plugins::mount_all`）之后调。`imported` 是刚导入的插件：它带了布局就
    /// 直接换上（多套时用按 id 排第一的），装上就生效，不用再去命令面板里找。
    ///
    /// 没有要换的时候：当前布局是插件带的而插件已经删了，回到默认布局；布局文件改过就按新
    /// 文件装；否则把 Dock 现在的样子 dump 出来再装回去，插件面板换成新脚本建的，内置面板
    /// 和点出来的样子照旧。
    pub(crate) fn plugins_reloaded(
        &mut self,
        imported: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reload_layout_catalog();
        let brought = imported.and_then(|plugin| {
            self.layout_catalog
                .iter()
                .find(|entry| layout_files::layout_plugin(&entry.id) == Some(plugin))
                .cloned()
        });
        if let Some(LayoutEntry { id, path, layout }) = brought {
            match layout {
                Ok(file) => {
                    self.apply_layout(id.clone(), file, true, window, cx);
                    crate::status_item::notify_success(format!(
                        "已换上布局「{id}」。要换回去，在命令面板里选「切换到默认布局」或别的布局"
                    ));
                }
                Err(error) => crate::status_item::notify_error(format!(
                    "插件带的布局没有加载（{}）：{error}",
                    path.display()
                )),
            }
            return;
        }

        let current = self.shell_layout.id.clone();
        let entry = self
            .layout_catalog
            .iter()
            .find(|entry| entry.id == current)
            .cloned();
        match entry {
            None if layout_files::layout_plugin(&current).is_some() => {
                self.apply_layout(
                    layout_files::DEFAULT_ID.to_string(),
                    LayoutFile::default_layout(),
                    true,
                    window,
                    cx,
                );
                crate::status_item::notify_success(format!(
                    "布局「{current}」所属的插件不在了，换回了默认布局"
                ));
            }
            Some(LayoutEntry {
                layout: Ok(file), ..
            }) if file.digest != self.shell_layout.file.digest => {
                self.apply_layout(current, file, true, window, cx);
            }
            // 默认布局、文件没变，或者文件现在不合法：照现在的样子重装。
            _ => {
                let state = self.shell_dock.read(cx).dump(cx);
                self.load_shell_state(state, window, cx);
            }
        }
    }

    /// 让一块面板露出来：别的组放大着先还原，在收起的 dock 里就打开，再选中它的标签。
    pub(crate) fn reveal_panel(
        &mut self,
        panel: PanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell_dock.update(cx, |area, cx| {
            let Some(group) = workspace_shell::group_of(area, panel) else {
                return;
            };
            if area.zoomed_group().is_some_and(|zoomed| zoomed != group) {
                area.set_zoomed_out(window, cx);
            }
            if let Some(placement) = workspace_shell::placement_of(area, panel)
                && placement != DockPlacement::Center
                && !area.is_dock_open(placement)
            {
                area.toggle_dock(placement, window, cx);
            }
            area.select_panel(panel, window, cx);
        });
    }

    /// 让布局里的一块内置面板露出来（按树的先后取第一块）。布局里没有就什么都不做。
    pub(crate) fn reveal_builtin(
        &mut self,
        builtin: Builtin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = workspace_shell::shell_panels(self.shell_dock.read(cx))
            .into_iter()
            .find(|(_, entity)| entity.read(cx).builtin() == builtin)
            .map(|(panel, _)| panel);
        if let Some(panel) = panel {
            self.reveal_panel(panel, window, cx);
        }
    }
}

fn layout_cmd(command: LayoutCommand) -> crate::Cmd {
    crate::Cmd::Layout(command)
}

/// Dock 树里的一个节点给插件看的样子。
fn node_info(
    area: &DockArea,
    node: &PaneNode,
    shown: &HashSet<PanelId>,
    cx: &App,
) -> LayoutNodeInfo {
    match node.kind() {
        PaneRef::Split { axis, children, .. } => LayoutNodeInfo {
            kind: match axis {
                gpui::Axis::Horizontal => "row",
                gpui::Axis::Vertical => "column",
            },
            panels: Vec::new(),
            active: None,
            children: children
                .iter()
                .map(|child| node_info(area, child, shown, cx))
                .collect(),
        },
        PaneRef::Tabs { panels, active_ix } => LayoutNodeInfo {
            kind: "tabs",
            panels: panels
                .iter()
                .filter_map(|&id| {
                    let panel = area.panel(id)?;
                    Some(LayoutPanelInfo {
                        name: workspace_shell::layout_name(panel, cx),
                        title: workspace_shell::panel_text(panel, cx).to_string(),
                        shown: shown.contains(&id),
                    })
                })
                .collect(),
            active: Some(active_ix),
            children: Vec::new(),
        },
    }
}
