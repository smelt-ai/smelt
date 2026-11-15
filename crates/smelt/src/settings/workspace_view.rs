//! 设置页快照、页面渲染和打开设置。

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
            attention: self.settings_attention(cx),
        }
    }

    pub(super) fn render_settings_content(
        entity: Entity<Workspace>,
        snapshot: &SettingsRenderSnapshot,
        plugin_page: Option<AnyView>,
        cx: &App,
    ) -> Div {
        render::render_settings_content(entity, snapshot, plugin_page, cx)
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

    /// 打开设置：⌘,、菜单栏「设置…」、侧栏「设置」都走这里。设置已经露着就不换页；
    /// 没露着时有要处理的事就停在那一页，没有就停在上次那页。
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.check_daemon_outdated(cx);
        if !crate::plugin_host::view_shown(SETTINGS_VIEW, cx)
            && let Some(section) = self.settings_attention(cx)
        {
            return self.open_settings_section(section, window, cx);
        }
        self.open_view(SETTINGS_VIEW, serde_json::Value::Null, window, cx);
    }

    /// 打开设置并停在指定的页。已经停在这页时靠 nonce 让它从头显示。
    pub(crate) fn open_settings_section(
        &mut self,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        self.settings_page_nonce += 1;
        self.open_view(SETTINGS_VIEW, serde_json::Value::Null, window, cx);
    }
}
