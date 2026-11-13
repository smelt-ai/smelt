//! 设置页快照、页面渲染和独立设置窗口生命周期。

use super::*;

mod render;

impl Workspace {
    pub(super) fn settings_render_snapshot(&self, cx: &Context<Self>) -> SettingsRenderSnapshot {
        let font_options = self.font_options.get_or_init(|| {
            let mut names = cx.text_system().all_font_names();
            names.sort();
            names.dedup();
            std::sync::Arc::new(
                std::iter::once((
                    SharedString::from(""),
                    SharedString::from(format!("默认（{}）", terminal_view::DEFAULT_FONT_FAMILY)),
                ))
                .chain(
                    names
                        .into_iter()
                        .map(|name| (SharedString::from(name.clone()), SharedString::from(name))),
                )
                .collect(),
            )
        });
        let ui_font_options = std::sync::Arc::new(
            std::iter::once((SharedString::from(""), SharedString::from("默认（系统）")))
                .chain(font_options.iter().skip(1).cloned())
                .collect(),
        );

        SettingsRenderSnapshot {
            bg_color_picker: self.bg_color_picker.clone(),
            opacity_slider: self.opacity_slider.clone(),
            ui_font_size_slider: self.ui_font_size_slider.clone(),
            font_size_slider: self.font_size_slider.clone(),
            bg_image_opacity_slider: self.bg_image_opacity_slider.clone(),
            font_options: font_options.clone(),
            ui_font_options,
            launch_rows: self
                .launch_inputs
                .as_ref()
                .map(|inputs| inputs.rows.clone())
                .unwrap_or_default(),
            profile_rows: self
                .profile_inputs
                .as_ref()
                .map(|inputs| inputs.rows.clone())
                .unwrap_or_default(),
            panel_plugins: self.panel_plugins_snapshot(),
            #[cfg(any(target_os = "macos", windows))]
            update_status: self.update_status.clone(),
            #[cfg(target_os = "macos")]
            moving_app: self.moving_app,
            #[cfg(target_os = "macos")]
            move_app_error: self.move_app_error.clone(),
            daemon_outdated: self.daemon_outdated,
            daemon_upgrading: self.daemon_upgrading,
            daemon_upgrade_msg: self.daemon_upgrade_msg.clone(),
            daemon_info: self.daemon_info.clone(),
            settings_section: self.settings_section.clone(),
            settings_page_nonce: self.settings_page_nonce,
            show_daemon_restart_confirm: self.show_daemon_restart_confirm,
            show_update_install_confirm: self.show_update_install_confirm,
            session_manager_open: self.session_manager_open,
            resources: (self.settings_section == SettingsSection::Resources)
                .then(|| self.resources_snapshot(cx)),
            attention: self.settings_attention(cx),
        }
    }

    pub(super) fn render_settings_content(
        entity: Entity<Workspace>,
        snapshot: &SettingsRenderSnapshot,
        cx: &App,
    ) -> Div {
        render::render_settings_content(entity, snapshot, cx)
    }

    /// 设置里有没有要你处理的事，有的话落在哪一页：有新版本、后台服务旧了 → 系统；有机器
    /// 要处理 → 远程。侧栏「设置」上的红点和设置侧栏那一页的红点都看它。
    pub(crate) fn settings_attention(&self, cx: &App) -> Option<SettingsSection> {
        #[cfg(any(target_os = "macos", windows))]
        if self.update_available() {
            return Some(SettingsSection::app_update());
        }
        #[cfg(unix)]
        if self.daemon_outdated == Some(true) {
            return Some(SettingsSection::Maintenance);
        }
        crate::runtimes::any_runtime_needs_attention(cx).then_some(SettingsSection::Remote)
    }

    /// 打开设置窗口：⌘,、菜单栏「设置…」、侧栏「设置」都走这里。窗口已开着就只提到前面，
    /// 不换页；新开时有要处理的事就停在那一页，没有就从外观开始。
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.check_daemon_outdated(cx);
        let open = cx
            .try_global::<SettingsWindowHandle>()
            .map(|handle| handle.0)
            .is_some_and(|handle| crate::window_handle_is_open(&handle, cx));
        if !open {
            match self.settings_attention(cx) {
                Some(section) => return self.open_settings_section(section, cx),
                None => self.settings_section = SettingsSection::appearance(),
            }
        }
        self.open_settings_window(cx);
    }

    /// 跳到指定设置子页并打开设置窗口。已打开时靠 nonce 强制切到目标子页。
    pub(crate) fn open_settings_section(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        self.settings_page_nonce += 1;
        self.open_settings_window(cx);
    }

    /// 打开独立设置窗口：已经开着就聚焦提到前台，不重复开第二扇。窗口只持有设置
    /// 快照，颜色选择器和配置输入框等状态仍挂在 Workspace；观察器负责同步两者。
    ///
    /// 必须用 `cx.defer` 推迟到当前这轮 `Workspace::update` 彻底返回之后再开窗：
    /// 这里被点侧栏「设置」的 `cx.listener` 调用时，`Workspace` 这个 entity 正被 update
    /// 占着；若同步 `cx.open_window`，新窗口首帧 `SettingsWindow::render` 里会
    /// 马上又对同一个 `Workspace` entity 调 `update`，两层嵌套 update 撞上 GPUI
    /// 的重入保护直接 panic 崩溃（"cannot update ... while it is already being
    /// updated"）——这就是「点设置整个 app 崩溃」的真正原因。
    pub fn open_settings_window(&mut self, cx: &mut Context<Self>) {
        // 已经开着、换到资源页时，采样循环要醒来看一眼；新开的窗口在下面开好之后再叫一次。
        self.wake_resource_monitor();
        super::refresh_agent_hooks_status(cx);
        super::refresh_remote(cx);
        super::refresh_quota_settings(cx);
        crate::runtime_events::probe_remotes(cx);
        if self.settings_section == SettingsSection::Remote {
            // 延迟平时只在连接换路径时读；停在远程页时重读一次，看到的是现在的数。
            crate::runtimes::sample_links(cx);
        }
        let workspace = cx.entity();
        cx.defer(move |cx| {
            if let Some(handle) = cx
                .try_global::<SettingsWindowHandle>()
                .map(|handle| handle.0)
                && handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                return;
            }
            // 启动项编辑需要较宽的命令输入区；侧栏约 280，内容区要能放下长命令和插件配置。
            let bounds = WindowBounds::centered(size(px(1100.), px(800.)), cx);
            let window_bg = cx.global::<Appearance>().window_bg();
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("设置".into()),
                    ..Default::default()
                }),
                window_bounds: Some(bounds),
                window_background: window_bg,
                ..Default::default()
            };
            let handle = cx
                .open_window(options, |window, cx| {
                    let ui_font_px = cx.global::<Appearance>().ui_font_px;
                    window.set_rem_size(px(ui_font_px as f32));
                    // 只在设置窗口创建或 Workspace 通知时建立快照，不能放进 render：
                    // 后者会把高频终端状态带进设置页的响应式依赖图。
                    let snapshot = workspace.update(cx, |workspace, cx| {
                        workspace.ensure_appearance_controls(window, cx);
                        workspace.ensure_launch_inputs(window, cx);
                        workspace.ensure_profile_inputs(window, cx);
                        if cx
                            .try_global::<AcpRuntimeState>()
                            .is_some_and(|state| state.diagnostics.is_none() && !state.refreshing)
                        {
                            workspace.refresh_acp_runtime(cx);
                        }
                        workspace.settings_render_snapshot(cx)
                    });
                    let content = cx.new(|cx| {
                        SettingsContentView::new(
                            workspace.clone(),
                            snapshot,
                            window,
                            cx,
                        )
                    });
                    let view = cx.new(|cx| SettingsWindow {
                        content,
                        focus_handle: cx.focus_handle(),
                        did_focus: false,
                        _observe_appearance: cx
                            .observe_global_in::<Appearance>(window, |_, _, cx| cx.notify()),
                        applied_window_bg: None,
                        applied_window_opacity: None,
                        applied_glass_style: None,
                        applied_ui_font_px: Some(ui_font_px),
                        debug_hud: false,
                    });
                    // 设置侧栏有自己的底色，右侧设置页则依赖 Root 的主题背景。
                    // 不能将 Root 强制设为透明，否则浅色模式下右侧会透出原生毛玻璃
                    // 的深色材质，造成同一窗口左右两套明暗主题。
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("打开设置窗口失败");
            cx.set_global(SettingsWindowHandle(handle));
            workspace.read(cx).wake_resource_monitor();
        });
    }
}
