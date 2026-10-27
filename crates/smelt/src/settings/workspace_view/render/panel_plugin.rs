//! 设置页：面板插件（Tool Panel 里的 gpui-shell 插件）。
//!
//! 列出眼下加载的插件，导入、删除自己装的插件，重新加载全部插件。App 自带的插件跟着
//! smelt 更新，不能删；用户装一个同 id 的副本就能盖过它。

use super::*;
use crate::plugins::{InstalledPlugin, PluginSource};

/// 示例插件仓库，照着它写自己的插件。
const DEMO_URL: &str = "https://github.com/smelt-ai/smelt-plugin-demo";

pub(super) fn panel_plugin_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
) -> SettingPage {
    let state = &snapshot.panel_plugins;
    let mut page = SettingPage::new("面板插件")
        .description("每个插件是右侧 Tool Panel 里的一个页签。")
        .resettable(false);
    if let Some(error) = state.error.clone() {
        page = page.group(
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
    page.group(list_group(entity, state))
        .group(authoring_group())
}

/// 页面的主体：已装的插件。导入、打开目录、重新加载是对这张列表的操作，放在它的第一行。
fn list_group(entity: Entity<Workspace>, state: &PanelPluginsSnapshot) -> SettingGroup {
    let title = match state.plugins.len() {
        0 => "已装插件".to_string(),
        count => format!("已装 {count} 个"),
    };
    let mut group = SettingGroup::new()
        .title(title)
        .item(toolbar_item(entity.clone(), state))
        // 页脚本身是小号字，这里只把颜色换成警告色：导入别人的插件等于给它整台机器的权限。
        .footer(|_, cx| {
            div().text_color(cx.theme().warning).child(
                "插件和 Smelt 一样能读写任意文件、运行任意命令，只导入你信任的。\
                 删除的插件挪进 ~/.smelt/trash/plugins，需要时能找回。",
            )
        });
    if state.plugins.is_empty() {
        return group.item(SettingItem::render(|_, _, cx: &mut App| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("还没有插件。")
                .into_any_element()
        }));
    }
    for plugin in &state.plugins {
        group = group.item(plugin_item(
            entity.clone(),
            plugin,
            state.pending_delete.as_deref() == Some(plugin.id.as_str()),
            state.busy,
        ));
    }
    group
}

/// 列表第一行：左边说明导入会做什么，右边三个操作，导入是唯一的主按钮。
fn toolbar_item(entity: Entity<Workspace>, state: &PanelPluginsSnapshot) -> SettingItem {
    let user_dir = state
        .user_dir
        .as_ref()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let busy = state.busy;
    SettingItem::new(
        "导入",
        SettingField::render(move |_, _, _| {
            let import_entity = entity.clone();
            let open_entity = entity.clone();
            let reload_entity = entity.clone();
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    Button::new("reload-panel-plugins")
                        .quiet()
                        .compact()
                        .label("重新加载")
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            reload_entity.update(cx, |workspace, cx| {
                                workspace.reload_panel_plugins(cx);
                            });
                        }),
                )
                .child(
                    Button::new("open-panel-plugin-dir")
                        .quiet()
                        .compact()
                        .label("打开插件目录")
                        .on_click(move |_, _, cx| {
                            open_entity.update(cx, |workspace, cx| {
                                workspace.open_panel_plugin_dir(cx);
                            });
                        }),
                )
                .child(
                    Button::new("import-panel-plugin")
                        .compact()
                        .label("导入插件…")
                        .loading(busy)
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            import_entity.update(cx, |workspace, cx| {
                                workspace.import_panel_plugin(cx);
                            });
                        }),
                )
                .into_any_element()
        }),
    )
    .description(format!(
        "选一个含 gpui-shell.json 的目录，复制进 {user_dir}；改了脚本点「重新加载」，不用重启。"
    ))
}

/// 给插件作者看的：示例和日志。放在最后，不跟装插件的人抢位置。
fn authoring_group() -> SettingGroup {
    SettingGroup::new().title("自己写插件").item(
        SettingItem::new(
            "示例和教程",
            SettingField::render(|_, _, _| {
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        Button::new("open-panel-plugin-log")
                            .quiet()
                            .compact()
                            .label("打开日志")
                            .on_click(|_, _, _| {
                                let opened = smelt_sys::app_log::log_path()
                                    .ok_or_else(|| "找不到日志文件".to_string())
                                    .and_then(|path| crate::ide::open_in_system_text_editor(&path));
                                if let Err(error) = opened {
                                    crate::status_item::notify_error(error);
                                }
                            }),
                    )
                    .child(
                        Button::new("open-panel-plugin-demo")
                            .link()
                            .icon_size()
                            .label("示例和教程 ↗")
                            .on_click(move |_, _, cx| cx.open_url(DEMO_URL)),
                    )
                    .into_any_element()
            }),
        )
        .description(
            "gpui-shell 脚本：JavaScript 描述界面，GPUI 原生绘制。console.log 和脚本错误写进日志，\
             带 [plugin] 标记。",
        ),
    )
}

fn plugin_item(
    entity: Entity<Workspace>,
    plugin: &InstalledPlugin,
    pending: bool,
    busy: bool,
) -> SettingItem {
    let id = plugin.id.clone();
    let removable = plugin.source == PluginSource::User;

    // 清单里的 version 可以不写。
    let mut detail = vec![if plugin.version.is_empty() {
        plugin.id.clone()
    } else {
        format!("{} · 版本 {}", plugin.id, plugin.version)
    }];
    detail.push(match plugin.source {
        PluginSource::User => format!("自己装的 · {}", plugin.root.display()),
        PluginSource::Bundled => "smelt 自带，跟着 smelt 更新".to_string(),
        #[cfg(debug_assertions)]
        PluginSource::Repo => format!("开发构建从仓库加载 · {}", plugin.root.display()),
    });
    if let Some(error) = &plugin.error {
        detail.push(format!("加载失败：{error}"));
    }

    SettingItem::new(
        plugin.name.clone(),
        SettingField::render(move |_, _, _| {
            if !removable {
                return div().into_any_element();
            }
            let request_entity = entity.clone();
            let confirm_entity = entity.clone();
            let cancel_entity = entity.clone();
            let id_for_request = id.clone();
            let id_for_confirm = id.clone();
            if pending {
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(format!("confirm-delete-panel-plugin-{id}"))
                            .danger()
                            .compact()
                            .label("确认删除")
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let id = id_for_confirm.clone();
                                confirm_entity.update(cx, |workspace, cx| {
                                    workspace.confirm_panel_plugin_delete(id, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new(format!("cancel-delete-panel-plugin-{id}"))
                            .quiet()
                            .compact()
                            .label("取消")
                            .on_click(move |_, _, cx| {
                                cancel_entity.update(cx, |workspace, cx| {
                                    workspace.cancel_panel_plugin_delete(cx);
                                });
                            }),
                    )
                    .into_any_element()
            } else {
                Button::new(format!("delete-panel-plugin-{id}"))
                    .quiet()
                    .compact()
                    .label("删除…")
                    .disabled(busy)
                    .on_click(move |_, _, cx| {
                        let id = id_for_request.clone();
                        request_entity.update(cx, |workspace, cx| {
                            workspace.request_panel_plugin_delete(id, cx);
                        });
                    })
                    .into_any_element()
            }
        }),
    )
    .description(detail.join("\n"))
}
