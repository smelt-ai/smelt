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
                    SharedString::from(format!("默认（{}）", font_config::DEFAULT_FONT_FAMILY)),
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
            std::iter::once((
                SharedString::from(""),
                SharedString::from(format!("默认（{}）", smelt_ui::kit::UI_FONT_FAMILY)),
            ))
                .chain(font_options.iter().skip(1).cloned())
                .collect(),
        );
        let generation = smelt_ui::theme_catalog::generation(cx);
        let cached = self
            .theme_options
            .borrow()
            .as_ref()
            .filter(|(cached, _)| *cached == generation)
            .map(|(_, options)| options.clone());
        let theme_options = cached.unwrap_or_else(|| {
            let options = std::rc::Rc::new(render::theme_options(cx));
            *self.theme_options.borrow_mut() = Some((generation, options.clone()));
            options
        });

        SettingsRenderSnapshot {
            ui_font_size_slider: self.ui_font_size_slider.clone(),
            bg_image_opacity_slider: self.bg_image_opacity_slider.clone(),
            font_options: font_options.clone(),
            ui_font_options,
            theme_options,
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
        search: &Entity<smelt_ui::field::InputState>,
        scroll: &ScrollHandle,
        cx: &App,
    ) -> AnyElement {
        render::render_settings_nav(entity, selected, attention, search, scroll, cx)
    }

    /// 设置里有没有要你处理的事，有的话落在哪一页：插件的设置页挂着角标（比如「关于」页
    /// 有新版本）→ 那一页。设置导航里那一页的红点看它。只读内存。
    pub(crate) fn settings_attention(&self, cx: &App) -> Option<SettingsSection> {
        smelt_plugin_api::views(cx)
            .into_iter()
            .find(|view| {
                view.settings.is_some() && smelt_plugin_api::view_badge(&view.id, cx).is_some()
            })
            .map(|view| SettingsSection::Plugin(view.id))
    }

    /// 打开设置：⌘,、菜单栏「设置…」、命令都走这里。设置已经露着就不换页；
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
