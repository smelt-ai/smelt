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
            dsh_plugin_manager: self.dsh_plugin_manager.clone(),
            dsh_model_editor: self.dsh_model_editor.clone(),
            dsh_custom_provider_editor: self.dsh_custom_provider_editor.clone(),
            dsh_model_editor_error: self.dsh_model_editor_error.clone(),
            dsh_effort_view: self.native_dsh_effort_view(cx),
            dsh_model_choices_view: self.native_dsh_model_choices_view(cx),
            native_dsh_model_settings: self.dsh_native_model_settings.clone(),
            dsh_settings: smelt_core::agent_kind::dsh_settings_summary(),
            pi_model_editor: self.pi_model_editor.clone(),
            pi_custom_provider_editor: self.pi_custom_provider_editor.clone(),
            pi_auth_providers: self.pi_auth_providers.clone(),
            pi_login: self.pi_login.clone(),
            pi_auth_model_picker: self.pi_auth_model_picker.clone(),
            pi_auth_error: self.pi_auth_error.clone(),
            pi_auth_show_all: self.pi_auth_show_all,
            pi_model_editor_error: self.pi_model_editor_error.clone(),
            pi_model_settings: self.pi_model_settings_cached().clone(),
            pi_settings: smelt_core::pi_model_settings::pi_settings_summary(),
            pi_plugins: self.pi_plugins_cached().to_vec(),
            pi_plugin_pending_delete: self.pi_plugin_pending_delete.clone(),
            pi_plugin_error: self.pi_plugin_error.clone(),
            agent_names_using_plugin: {
                let mut map: std::collections::HashMap<String, Vec<String>> = Default::default();
                for agent in &cx.global::<AgentHostState>().agents {
                    for plugin_id in &agent.plugins {
                        map.entry(plugin_id.clone())
                            .or_default()
                            .push(agent.name.clone());
                    }
                }
                map
            },
            update_status: self.update_status.clone(),
            daemon_outdated: self.daemon_outdated,
            daemon_upgrading: self.daemon_upgrading,
            daemon_upgrade_msg: self.daemon_upgrade_msg.clone(),
            daemon_info: self.daemon_info.clone(),
            settings_section: self.settings_section.clone(),
            settings_page_nonce: self.settings_page_nonce,
            show_daemon_restart_confirm: self.show_daemon_restart_confirm,
            session_manager_open: self.session_manager_open,
        }
    }

    pub(super) fn render_settings_content(
        entity: Entity<Workspace>,
        scope: SettingsScope,
        snapshot: &SettingsRenderSnapshot,
        cx: &App,
    ) -> Div {
        render::render_settings_content(entity, scope, snapshot, cx)
    }

    /// 跳到指定设置子页并打开对应作用域的窗口。已打开时靠 nonce 强制切到目标子页。
    pub(crate) fn open_settings_section(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) {
        let scope = if SettingsScope::AgentSetup.contains(&section) {
            SettingsScope::AgentSetup
        } else {
            SettingsScope::All
        };
        self.open_scoped_settings_section(scope, section, cx);
    }

    /// 指定作用域打开设置窗口并定位到子页。
    pub(crate) fn open_scoped_settings_section(
        &mut self,
        scope: SettingsScope,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        self.settings_page_nonce += 1;
        self.open_settings_window_scoped(scope, cx);
    }

    /// 打开独立设置窗口：已经开着就聚焦提到前台，不重复开第二扇。窗口只持有设置
    /// 快照，颜色选择器和配置输入框等状态仍挂在 Workspace；观察器负责同步两者。
    ///
    /// 必须用 `cx.defer` 推迟到当前这轮 `Workspace::update` 彻底返回之后再开窗：
    /// 这里被点齿轮的 `cx.listener` 调用时，`Workspace` 这个 entity 正被 update
    /// 占着；若同步 `cx.open_window`，新窗口首帧 `SettingsWindow::render` 里会
    /// 马上又对同一个 `Workspace` entity 调 `update`，两层嵌套 update 撞上 GPUI
    /// 的重入保护直接 panic 崩溃（"cannot update ... while it is already being
    /// updated"）——这就是「点齿轮整个 app 崩溃」的真正原因。
    pub fn open_settings_window(&self, cx: &mut Context<Self>) {
        self.open_settings_window_scoped(SettingsScope::All, cx);
    }

    pub fn open_settings_window_scoped(&self, scope: SettingsScope, cx: &mut Context<Self>) {
        super::reload_appearance_from_store(cx);
        let workspace = cx.entity();
        cx.defer(move |cx| {
            if let Some(handle) = cx
                .try_global::<SettingsWindowHandles>()
                .and_then(|handles| handles.0.get(&scope).copied())
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
                    title: Some(scope.window_title().into()),
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
                        workspace.reload_native_dsh_model_settings();
                        let mut plugins = cx
                            .try_global::<PluginEnablementState>()
                            .cloned()
                            .unwrap_or_else(PluginEnablementState::load);
                        plugins.refresh_catalog();
                        cx.set_global(plugins);
                        if cx
                            .try_global::<AcpRuntimeState>()
                            .is_some_and(|state| state.diagnostics.is_none() && !state.refreshing)
                        {
                            workspace.refresh_acp_runtime(cx);
                        }
                        workspace.settings_render_snapshot(cx)
                    });
                    let content = cx.new(|cx| SettingsContentView {
                        workspace: workspace.clone(),
                        scope,
                        snapshot,
                        _observe_workspace: cx.observe_in(
                            &workspace,
                            window,
                            |this: &mut SettingsContentView, _, window, cx| {
                                this.request_refresh(window, cx);
                            },
                        ),
                        _observe_settings_globals: vec![
                            SettingsContentView::observe_settings_global::<Appearance>(window, cx),
                            SettingsContentView::observe_settings_global::<LaunchConfig>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<AgentHostState>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<AcpRuntimeState>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<UpdateSettings>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<CleanupState>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<CopyFlash>(window, cx),
                            SettingsContentView::observe_settings_global::<RemoteConfig>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<RemoteRuntimeState>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<IrohRuntimeState>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<IrohConnectionsState>(
                                window, cx,
                            ),
                            SettingsContentView::observe_settings_global::<
                                crate::worktree_inherit::WorktreeInheritSettings,
                            >(window, cx),
                            SettingsContentView::observe_settings_global::<PluginEnablementState>(
                                window, cx,
                            ),
                        ],
                        // 初始快照已经在上面同步完成；避免设置窗刚打开就被第一条后台
                        // 流式通知立即打穿缓存。
                        last_refresh: Some(Instant::now()),
                        refresh_pending: false,
                        refresh_generation: 0,
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
                        last_frame: None,
                        fps_ema: 0.0,
                        debug_mem_rss: None,
                        debug_mem_sampled_at: None,
                    });
                    // 设置侧栏有自己的底色，右侧设置页则依赖 Root 的主题背景。
                    // 不能将 Root 强制设为透明，否则浅色模式下右侧会透出原生毛玻璃
                    // 的深色材质，造成同一窗口左右两套明暗主题。
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("打开设置窗口失败");
            let mut handles = cx
                .try_global::<SettingsWindowHandles>()
                .map(|handles| handles.0.clone())
                .unwrap_or_default();
            handles.insert(scope, handle);
            cx.set_global(SettingsWindowHandles(handles));
        });
    }
}
