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
        format!("已装 {usable} 个可用插件。创建智能体时勾选需要的插件，对话只加载勾选项。")
    } else {
        format!(
            "已装 {usable} 个可用插件，另有 {broken} 个失效条目（多半是断掉的软链），建议删掉。"
        )
    };

    SettingsPage::new("capabilities-pane:plugins")
        .child(page_intro(summary))
        .child(overview_section(entity.clone(), &plugins))
        .children(page.pi_plugin_error.clone().map(|error| {
            SettingsSection::new().title("上一次操作失败").child(
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
    let scan = SettingsRow::new("扫描目录").description(format!(
        "当前扫到 {skill_count} 个技能、{extension_count} 个扩展。\n{}",
        roots.join("\n")
    ));
    let import = SettingsRow::new("导入")
        .description("把本地的技能目录（含 SKILL.md）或扩展文件拷进全局插件目录，原目录不动。")
        .child(
            Button::new("import-pi-skill")
                .small()
                .label("导入技能目录")
                .on_click(move |_, _, cx| {
                    skill_entity.update(cx, |workspace, cx| {
                        workspace.import_pi_plugin(PiPluginKind::Skill, cx);
                    });
                }),
        )
        .child(
            Button::new("import-pi-extension")
                .small()
                .label("导入扩展")
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
                .label("重新扫描")
                .on_click(move |_, _, cx| {
                    refresh_entity.update(cx, |workspace, cx| {
                        workspace.refresh_pi_plugins(cx);
                    });
                }),
        );
    SettingsSection::new()
        .title("插件来源")
        .description("技能是 markdown 能力包，扩展是注册工具/命令的 TypeScript 模块。")
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
        return SettingsSection::new().title("已安装").child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::muted()))
                .child("还没有装任何插件。"),
        );
    }

    SettingsSection::new()
        .title("已安装")
        .description("删除会直接删掉磁盘上的文件，并把所有智能体里对它的勾选一起清掉。")
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
    let kind_label = plugin.kind.label();
    let origin = plugin.origin.clone();
    let path = plugin.path.display().to_string();
    let broken = plugin.broken.clone();
    let description = plugin.description.clone();
    let builtin = plugin.builtin;

    let title = format!("{name}（{kind_label}）");
    let mut detail = Vec::new();
    if !description.is_empty() {
        detail.push(description);
    }
    if builtin {
        detail.push(format!("来源：{origin}，跟着 Smelt 更新"));
    } else {
        detail.push(format!("来源：{origin} · {path}"));
    }
    if let Some(reason) = &broken {
        detail.push(format!("失效：{reason}"));
    }
    if users.is_empty() {
        detail.push("暂时没有智能体勾选它。".to_string());
    } else {
        detail.push(format!("正在被 {} 使用。", users.join("、")));
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
                .label("确认删除")
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
                .label("取消")
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
                .label("删除")
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.request_pi_plugin_delete(id, cx);
                    });
                }),
        )
    }
}
