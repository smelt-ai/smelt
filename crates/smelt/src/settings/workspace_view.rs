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
            settings_section: self.settings_section.clone(),
            settings_page_nonce: self.settings_page_nonce,
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

    pub(super) fn render_settings_nav(
        entity: Entity<Workspace>,
        selected: &SettingsSection,
        attention: Option<&SettingsSection>,
        search: &Entity<gpui_component::input::InputState>,
        cx: &App,
    ) -> AnyElement {
        render::render_settings_nav(entity, selected, attention, search, cx)
    }

    /// 设置里有没有要你处理的事，有的话落在哪一页：插件的设置页挂着角标（比如「关于」页
    /// 有新版本）→ 那一页；有机器要处理 → 远程。侧栏「设置」上的红点
    /// 和设置导航里那一页的红点都看它。只读内存。
    pub(crate) fn settings_attention(&self, cx: &App) -> Option<SettingsSection> {
        if let Some(view) = smelt_plugin_api::views(cx).into_iter().find(|view| {
            view.settings.is_some() && smelt_plugin_api::view_badge(&view.id, cx).is_some()
        }) {
            return Some(SettingsSection::Plugin(view.id));
        }
        crate::runtimes::any_runtime_needs_attention(cx).then_some(SettingsSection::Remote)
    }

    /// 打开设置：⌘,、菜单栏「设置…」、侧栏「设置」都走这里。设置已经露着就不换页；
    /// 没露着时有要处理的事就停在那一页，没有就停在上次那页。
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
