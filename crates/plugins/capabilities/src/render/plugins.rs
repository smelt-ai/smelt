//! 设置页：智能体插件（Pi 的技能与扩展）。
//!
//! 这里只管「装了什么、还在不在、谁在用」，勾选在智能体编辑器里做。
//! 只列全局目录和随 smelt-agent 发布的内置技能：智能体对话的工作目录是一次性空目录，
//! 项目级插件永远扫不到。内置技能跟着运行时更新，不能删。

use std::collections::HashMap;

use super::*;
use api::model::pi_plugin_catalog::{
    PiPlugin, PiPluginKind, global_extension_root, global_skill_roots,
};
use smelt_plugin_api::ui::kit::TYPE;

pub(super) fn pi_plugin_page(
    entity: Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    cx: &App,
) -> SettingsPage {
    let plugins = page.pi_plugins();
    let users = agent_names_using_plugins(&page.machine, cx);
    let usable = plugins.iter().filter(|plugin| plugin.is_usable()).count();
    let broken = plugins.len() - usable;
    let summary = if broken == 0 {
        t!("plugins.summary", usable = usable).into_owned()
    } else {
        t!("plugins.summary_broken", usable = usable, broken = broken).into_owned()
    };

    SettingsPage::new("capabilities-pane:plugins")
        .child(page_intro(summary))
        .child(overview_section(entity.clone(), &plugins))
        .children(page.pi_plugin_error.clone().map(|error| {
            SettingsSection::new()
                .title(t!("plugins.last_failed"))
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::bad()))
                        .child(error),
                )
        }))
        .child(list_section(
            entity,
            &plugins,
            page.pi_plugin_pending_delete.clone(),
            &users,
        ))
}

fn overview_section(entity: Entity<CapabilitiesPage>, plugins: &[PiPlugin]) -> SettingsSection {
    let skill_count = plugins
        .iter()
        .filter(|plugin| plugin.kind == PiPluginKind::Skill)
        .count();
    let extension_count = plugins
        .iter()
        .filter(|plugin| plugin.kind == PiPluginKind::Extension)
        .count();

    let roots = global_skill_roots()
        .into_iter()
        .chain(std::iter::once(global_extension_root()))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();

    let skill_entity = entity.clone();
    let extension_entity = entity.clone();
    let refresh_entity = entity;
    // 目录路径长，列在说明里，不挤到行右边。
    let scan = SettingsRow::new(t!("plugins.scan")).description(format!(
        "{}\n{}",
        t!(
            "plugins.scan_summary",
            skills = skill_count,
            extensions = extension_count
        ),
        roots.join("\n")
    ));
    let import = SettingsRow::new(t!("plugins.import_confirm"))
        .description(t!("plugins.import_description"))
        .child(
            Button::new("import-pi-skill")
                .small()
                .label(t!("plugins.import_skill"))
                .on_click(move |_, _, cx| {
                    skill_entity.update(cx, |workspace, cx| {
                        workspace.import_pi_plugin(PiPluginKind::Skill, cx);
                    });
                }),
        )
        .child(
            Button::new("import-pi-extension")
                .small()
                .label(t!("plugins.import_extension"))
                .on_click(move |_, _, cx| {
                    extension_entity.update(cx, |workspace, cx| {
                        workspace.import_pi_plugin(PiPluginKind::Extension, cx);
                    });
                }),
        )
        .child(
            Button::new("refresh-pi-plugins")
                .quiet()
                .small()
                .label(t!("plugins.rescan"))
                .on_click(move |_, _, cx| {
                    refresh_entity.update(cx, |workspace, cx| {
                        workspace.refresh_pi_plugins(cx);
                    });
                }),
        );
    SettingsSection::new()
        .title(t!("plugins.sources"))
        .description(t!("plugins.sources_description"))
        .child(scan)
        .child(import)
}

/// 插件 id → 那台机器上勾了它的智能体名，从智能体插件的配置里数；还没收到时是空的。
fn agent_names_using_plugins(machine: &RuntimeKey, cx: &App) -> HashMap<String, Vec<String>> {
    let mut users: HashMap<String, Vec<String>> = HashMap::new();
    let Some(settings) = crate::agents::settings_on(machine, cx) else {
        return users;
    };
    for agent in &settings.agents {
        for plugin in &agent.plugins {
            users
                .entry(plugin.clone())
                .or_default()
                .push(agent.name.clone());
        }
    }
    users
}

fn list_section(
    entity: Entity<CapabilitiesPage>,
    plugins: &[PiPlugin],
    pending_delete: Option<String>,
    users: &HashMap<String, Vec<String>>,
) -> SettingsSection {
    if plugins.is_empty() {
        return SettingsSection::new().title(t!("plugins.installed")).child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::muted()))
                .child(t!("plugins.none").into_owned()),
        );
    }

    SettingsSection::new()
        .title(t!("plugins.installed"))
        .description(t!("plugins.installed_description"))
        .children(plugins.iter().map(|plugin| {
            plugin_row(
                entity.clone(),
                plugin,
                pending_delete.as_deref() == Some(plugin.id.as_str()),
                users.get(&plugin.id).cloned().unwrap_or_default(),
            )
        }))
}

fn plugin_row(
    entity: Entity<CapabilitiesPage>,
    plugin: &PiPlugin,
    pending: bool,
    users: Vec<String>,
) -> SettingsRow {
    let id = plugin.id.clone();
    let name = plugin.name.clone();
    let kind_label = match plugin.kind {
        PiPluginKind::Skill => t!("plugins.kind.skill"),
        PiPluginKind::Extension => t!("plugins.kind.extension"),
    };
    let origin = plugin.origin.clone();
    let path = plugin.path.display().to_string();
    let broken = plugin.broken.clone();
    let description = plugin.description.clone();
    let builtin = plugin.builtin;

    let title = t!("plugins.row_title", name = name, kind = kind_label).into_owned();
    let mut detail = Vec::new();
    if !description.is_empty() {
        detail.push(description);
    }
    if builtin {
        detail.push(t!("plugins.origin_builtin", origin = origin).into_owned());
    } else {
        detail.push(t!("plugins.origin", origin = origin, path = path).into_owned());
    }
    if let Some(reason) = &broken {
        detail.push(t!("plugins.broken", reason = reason).into_owned());
    }
    if users.is_empty() {
        detail.push(t!("plugins.unused").into_owned());
    } else {
        let separator = t!("plugins.user_separator");
        detail.push(t!("plugins.used_by", names = users.join(separator.as_ref())).into_owned());
    }

    let row = SettingsRow::new(title).description(detail.join("\n"));
    // 内置的跟着运行时更新，不能删。
    if builtin {
        return row;
    }
    if pending {
        let confirm_entity = entity.clone();
        let cancel_entity = entity;
        row.child(
            Button::new(format!("confirm-delete-pi-plugin-{id}"))
                .danger()
                .small()
                .label(t!("plugins.confirm_delete"))
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    confirm_entity.update(cx, |workspace, cx| {
                        workspace.confirm_pi_plugin_delete(id, cx);
                    });
                }),
        )
        .child(
            Button::new(format!("cancel-delete-pi-plugin-{}", plugin.id))
                .quiet()
                .small()
                .label(t!("common.cancel"))
                .on_click(move |_, _, cx| {
                    cancel_entity.update(cx, |workspace, cx| {
                        workspace.cancel_pi_plugin_delete(cx);
                    });
                }),
        )
    } else {
        row.child(
            Button::new(format!("delete-pi-plugin-{id}"))
                .quiet()
                .small()
                .label(t!("common.delete"))
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.request_pi_plugin_delete(id, cx);
                    });
                }),
        )
    }
}
