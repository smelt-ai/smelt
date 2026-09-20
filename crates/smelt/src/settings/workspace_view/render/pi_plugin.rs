//! 设置页：智能体插件（Pi 的技能与扩展）。
//!
//! 这里只管「装了什么、还在不在、谁在用」，勾选在智能体编辑器里做。
//! 只列全局目录：智能体对话的工作目录是一次性空目录，项目级插件永远扫不到。

use super::*;
use smelt_core::pi_plugin_catalog::{
    PiPlugin, PiPluginKind, global_extension_root, global_skill_roots,
};

pub(super) fn pi_plugin_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    _cx: &App,
) -> SettingPage {
    let plugins = snapshot.pi_plugins.clone();
    let usable = plugins.iter().filter(|plugin| plugin.is_usable()).count();
    let broken = plugins.len() - usable;

    let mut groups = vec![overview_group(entity.clone(), &plugins)];
    if let Some(error) = snapshot.pi_plugin_error.clone() {
        groups.push(
            SettingGroup::new()
                .title("上一次操作失败")
                .item(SettingItem::render(move |_, _, cx: &mut App| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error.clone())
                        .into_any_element()
                })),
        );
    }
    groups.push(list_group(
        entity,
        &plugins,
        snapshot.pi_plugin_pending_delete.clone(),
        snapshot,
    ));

    SettingPage::new("智能体插件")
        .description(if broken == 0 {
            format!("已装 {usable} 个可用插件。创建智能体时勾选需要的插件，对话只加载勾选项。")
        } else {
            format!(
                "已装 {usable} 个可用插件，另有 {broken} 个失效条目（多半是断掉的软链），建议删掉。"
            )
        })
        .groups(groups)
}

fn overview_group(entity: Entity<Workspace>, plugins: &[PiPlugin]) -> SettingGroup {
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

    SettingGroup::new()
        .title("插件来源")
        .description("技能是 markdown 能力包，扩展是注册工具/命令的 TypeScript 模块。")
        .item(
            SettingItem::new(
                "扫描目录",
                SettingField::render(move |_, _, cx: &mut App| {
                    let muted = cx.theme().muted_foreground;
                    v_flex()
                        .gap_1()
                        .children(roots.iter().map(|path| {
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(path.clone())
                                .into_any_element()
                        }))
                        .into_any_element()
                }),
            )
            .description(format!(
                "当前扫到 {skill_count} 个技能、{extension_count} 个扩展。"
            )),
        )
        .item(
            SettingItem::new(
                "导入",
                SettingField::render(move |_, _, _| {
                    let skill_entity = entity.clone();
                    let extension_entity = entity.clone();
                    let refresh_entity = entity.clone();
                    h_flex()
                        .gap_2()
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
                                .ghost()
                                .small()
                                .label("重新扫描")
                                .on_click(move |_, _, cx| {
                                    refresh_entity.update(cx, |workspace, cx| {
                                        workspace.refresh_pi_plugins(cx);
                                    });
                                }),
                        )
                        .into_any_element()
                }),
            )
            .description("把本地的技能目录（含 SKILL.md）或扩展文件拷进全局插件目录，原目录不动。"),
        )
}

fn list_group(
    entity: Entity<Workspace>,
    plugins: &[PiPlugin],
    pending_delete: Option<String>,
    snapshot: &SettingsRenderSnapshot,
) -> SettingGroup {
    if plugins.is_empty() {
        return SettingGroup::new()
            .title("已安装")
            .item(SettingItem::render(move |_, _, cx: &mut App| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("还没有装任何插件。")
                    .into_any_element()
            }));
    }

    let items = plugins
        .iter()
        .map(|plugin| {
            plugin_item(
                entity.clone(),
                plugin,
                pending_delete.as_deref() == Some(plugin.id.as_str()),
                snapshot,
            )
        })
        .collect::<Vec<_>>();

    SettingGroup::new()
        .title("已安装")
        .description("删除会直接删掉磁盘上的文件，并把所有智能体里对它的勾选一起清掉。")
        .items(items)
}

fn plugin_item(
    entity: Entity<Workspace>,
    plugin: &PiPlugin,
    pending: bool,
    snapshot: &SettingsRenderSnapshot,
) -> SettingItem {
    let users = snapshot
        .agent_names_using_plugin
        .get(&plugin.id)
        .cloned()
        .unwrap_or_default();
    let id = plugin.id.clone();
    let name = plugin.name.clone();
    let kind_label = plugin.kind.label();
    let origin = plugin.origin.clone();
    let path = plugin.path.display().to_string();
    let broken = plugin.broken.clone();
    let description = plugin.description.clone();

    let title = format!("{name}（{kind_label}）");
    let mut detail = Vec::new();
    if !description.is_empty() {
        detail.push(description);
    }
    detail.push(format!("来源：{origin} · {path}"));
    if let Some(reason) = &broken {
        detail.push(format!("失效：{reason}"));
    }
    if users.is_empty() {
        detail.push("暂时没有智能体勾选它。".to_string());
    } else {
        detail.push(format!("正在被 {} 使用。", users.join("、")));
    }

    SettingItem::new(
        title,
        SettingField::render(move |_, _, _| {
            let id_for_request = id.clone();
            let id_for_confirm = id.clone();
            let request_entity = entity.clone();
            let confirm_entity = entity.clone();
            let cancel_entity = entity.clone();
            if pending {
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(format!("confirm-delete-pi-plugin-{id}"))
                            .danger()
                            .small()
                            .label("确认删除")
                            .on_click(move |_, _, cx| {
                                let id = id_for_confirm.clone();
                                confirm_entity.update(cx, |workspace, cx| {
                                    workspace.confirm_pi_plugin_delete(id, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new(format!("cancel-delete-pi-plugin-{id}"))
                            .ghost()
                            .small()
                            .label("取消")
                            .on_click(move |_, _, cx| {
                                cancel_entity.update(cx, |workspace, cx| {
                                    workspace.cancel_pi_plugin_delete(cx);
                                });
                            }),
                    )
                    .into_any_element()
            } else {
                Button::new(format!("delete-pi-plugin-{id}"))
                    .ghost()
                    .small()
                    .label("删除")
                    .on_click(move |_, _, cx| {
                        let id = id_for_request.clone();
                        request_entity.update(cx, |workspace, cx| {
                            workspace.request_pi_plugin_delete(id, cx);
                        });
                    })
                    .into_any_element()
            }
        }),
    )
    .description(detail.join("\n"))
}
