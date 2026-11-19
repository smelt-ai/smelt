//! 能力库「技能」「扩展」页签：Pi 的技能或扩展，各一页。
//!
//! 管「装了什么、还在不在、谁在用」；每行能直接勾给哪些智能体用（[`uses`]），智能体编辑器
//! 里也能勾，改的是同一份配置。
//! 只列用户级目录和随 smelt-agent 发布的内置技能：智能体对话的工作目录是一次性空目录，
//! 项目级插件永远扫不到。内置技能跟着运行时更新，不能删。
//!
//! 技能以 `~/.agents/skills` 为准（运行时那边见 `smelt_core::skill_home`）：放在旧位置的
//! 一键移过去，同名几份的挑一份为准，别的进回收目录。

use super::uses::{self};
use super::*;
use crate::agents::Usable;
use api::model::pi_plugin_catalog::{
    PiPlugin, PiPluginKind, global_extension_root, global_skill_roots,
};
use smelt_plugin_api::ui::kit::TYPE;

pub(super) fn pi_plugin_page(
    entity: Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    kind: PiPluginKind,
    cx: &App,
) -> SettingsPage {
    let plugins = page
        .pi_plugins()
        .into_iter()
        .filter(|plugin| plugin.kind == kind)
        .collect::<Vec<_>>();
    let usable = plugins.iter().filter(|plugin| plugin.is_usable()).count();
    let broken = plugins.len() - usable;
    let summary = match (kind, broken) {
        (PiPluginKind::Skill, 0) => t!("plugins.summary.skill", usable = usable),
        (PiPluginKind::Extension, 0) => t!("plugins.summary.extension", usable = usable),
        (PiPluginKind::Skill, _) => {
            t!(
                "plugins.summary_broken.skill",
                usable = usable,
                broken = broken
            )
        }
        (PiPluginKind::Extension, _) => t!(
            "plugins.summary_broken.extension",
            usable = usable,
            broken = broken
        ),
    };

    SettingsPage::new(match kind {
        PiPluginKind::Skill => "capabilities-pane:skills",
        PiPluginKind::Extension => "capabilities-pane:extensions",
    })
    .child(page_intro(summary))
    .child(overview_section(entity.clone(), kind))
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
    .child(list_section(entity, page, kind, &plugins, cx))
}

fn overview_section(entity: Entity<CapabilitiesPage>, kind: PiPluginKind) -> SettingsSection {
    let roots = match kind {
        PiPluginKind::Skill => global_skill_roots(),
        PiPluginKind::Extension => vec![global_extension_root()],
    }
    .into_iter()
    .map(|path| path.display().to_string())
    .collect::<Vec<_>>();

    let import_entity = entity.clone();
    let refresh_entity = entity;
    let (import_label, import_description, sources) = match kind {
        PiPluginKind::Skill => (
            t!("plugins.import_skill"),
            t!("plugins.import_description.skill"),
            t!("plugins.sources_description.skill"),
        ),
        PiPluginKind::Extension => (
            t!("plugins.import_extension"),
            t!("plugins.import_description.extension"),
            t!("plugins.sources_description.extension"),
        ),
    };
    // 目录路径长，列在说明里，不挤到行右边。扫到几个页首已经说了。
    let scan = SettingsRow::new(t!("plugins.scan"))
        .description(roots.join("\n"))
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
    let import = SettingsRow::new(t!("plugins.import_confirm"))
        .description(import_description)
        .child(
            Button::new("import-pi-plugin")
                .small()
                .label(import_label)
                .on_click(move |_, _, cx| {
                    import_entity.update(cx, |workspace, cx| {
                        workspace.import_pi_plugin(kind, cx);
                    });
                }),
        );
    SettingsSection::new()
        .title(t!("plugins.sources"))
        .description(sources)
        .child(scan)
        .child(import)
}

fn list_section(
    entity: Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    kind: PiPluginKind,
    plugins: &[PiPlugin],
    cx: &App,
) -> SettingsSection {
    let section = SettingsSection::new().title(t!("plugins.installed"));
    if plugins.is_empty() {
        return section.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::muted()))
                .child(match kind {
                    PiPluginKind::Skill => t!("plugins.none.skill").into_owned(),
                    PiPluginKind::Extension => t!("plugins.none.extension").into_owned(),
                }),
        );
    }

    let pending_delete = page.pi_plugin_pending_delete.as_deref();
    section
        .description(t!("plugins.installed_description"))
        .children(uses::agent_edit_error(page))
        .children(plugins.iter().map(|plugin| {
            plugin_row(
                &entity,
                page,
                plugin,
                pending_delete == Some(plugin.id.as_str()),
                cx,
            )
        }))
}

/// 旧位置的「移到通用目录」，或同名几份时「以哪份为准」的菜单。都不是就没有。
fn adopt_button(entity: &Entity<CapabilitiesPage>, plugin: &PiPlugin) -> Option<AnyElement> {
    if plugin.kind != PiPluginKind::Skill || plugin.builtin {
        return None;
    }
    let id = plugin.id.clone();
    if plugin.duplicates.is_empty() {
        if !plugin.legacy_location {
            return None;
        }
        let entity = entity.clone();
        let path = plugin.path.display().to_string();
        return Some(
            Button::new(format!("adopt-pi-plugin-{id}"))
                .small()
                .label(t!("plugins.move"))
                .on_click(move |_, _, cx| {
                    let (id, path) = (id.clone(), path.clone());
                    entity.update(cx, |page, cx| page.adopt_pi_plugin(id, path, cx));
                })
                .into_any_element(),
        );
    }
    let copies: Vec<String> = std::iter::once(&plugin.path)
        .chain(plugin.duplicates.iter())
        .map(|path| path.display().to_string())
        .collect();
    let entity = entity.clone();
    let menu_id = format!("adopt-pi-plugin-{id}");
    Some(
        MenuTrigger::dropdown(
            SharedString::from(format!("{menu_id}-menu")),
            Button::new(SharedString::from(menu_id))
                .small()
                .label(t!("plugins.pick_copy"))
                .dropdown_caret(true),
            move |_, _| {
                let mut menu = Menu::new();
                for path in &copies {
                    let entity = entity.clone();
                    let (id, path) = (id.clone(), path.clone());
                    menu = menu.item(MenuItem::new(path.clone()).on_click(move |_, _, cx| {
                        let (id, path) = (id.clone(), path.clone());
                        entity.update(cx, |page, cx| page.adopt_pi_plugin(id, path, cx));
                    }));
                }
                menu
            },
        )
        .into_any_element(),
    )
}

/// 它在哪：内置的写随谁发布；别的写扫描目录下的那一项。路径取自扫描它的那台机器
/// （`origin` 已把那边的家目录缩成 `~`），不拿本机的家目录去缩。
fn location(plugin: &PiPlugin) -> String {
    if plugin.builtin {
        return t!("plugins.origin_builtin", origin = plugin.origin).into_owned();
    }
    match plugin.path.file_name() {
        Some(name) => format!("{}/{}", plugin.origin, name.to_string_lossy()),
        None => plugin.origin.clone(),
    }
}

fn plugin_row(
    entity: &Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    plugin: &PiPlugin,
    pending: bool,
    cx: &App,
) -> SettingsRow {
    let usable = Usable::Plugin(plugin.id.clone());
    let used_by = uses::used_by_line(&page.machine, &usable, cx);
    // 失效的勾上也加载不了，不给勾。
    let uses_button = plugin
        .is_usable()
        .then(|| uses::uses_button(entity, page, usable, &plugin.id, cx))
        .flatten();
    let entity = entity.clone();
    let id = plugin.id.clone();

    // 自述是写给模型看的，常常一大段：收成两行，悬停看全文。下面一行是哪来的、谁在用，
    // 再往下才是要处理的状态。
    let mut note = vec![match used_by {
        Some(used_by) => format!("{} · {used_by}", location(plugin)),
        None => location(plugin),
    }];
    if let Some(reason) = &plugin.broken {
        note.push(t!("plugins.broken", reason = reason).into_owned());
    }
    if plugin.legacy_location && plugin.duplicates.is_empty() {
        note.push(t!("plugins.legacy").into_owned());
    }
    if !plugin.duplicates.is_empty() {
        note.push(t!("plugins.duplicates", count = plugin.duplicates.len()).into_owned());
    }

    let mut row = SettingsRow::new(plugin.name.clone()).note(note.join("\n"));
    if !plugin.description.is_empty() {
        row = row
            .description(plugin.description.clone())
            .clamp_description(format!("pi-plugin-description-{id}"), 2);
    }
    let row = row
        .children(adopt_button(&entity, plugin))
        .children(uses_button);
    // 内置的跟着运行时更新，不能删。
    if plugin.builtin {
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
