//! 设置页：面板插件（gpui-shell 插件，每个是工作台布局里的一个视图）。
//!
//! 列出眼下加载的插件，导入、删除自己装的插件，重新加载全部插件。App 自带的插件跟着
//! smelt 更新，不能删；用户装一个同 id 的副本就能盖过它。

use super::*;
use crate::plugins::{InstalledPlugin, PluginSource};

/// 示例插件仓库，照着它写自己的插件。
const DEMO_URL: &str = "https://github.com/smelt-ai/smelt-plugin-demo";

pub(super) fn panel_plugin_page(
    id: SharedString,
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
) -> SettingsPage {
    let state = &snapshot.panel_plugins;
    SettingsPage::new(id)
        .children(state.error.clone().map(|error| {
            Section::new().title("上一次操作失败").child(
                div()
                    .text_xs()
                    .text_color(rgb(crate::ui_theme::bad()))
                    .child(error),
            )
        }))
        .child(list_section(entity, state))
        .child(authoring_section())
}

/// 页面的主体：已装的插件。导入、打开目录、重新加载是对这张列表的操作，放在它的第一格。
fn list_section(entity: Entity<Workspace>, state: &PanelPluginsSnapshot) -> Section {
    let title = match state.plugins.len() {
        0 => "已装插件".to_string(),
        count => format!("已装 {count} 个"),
    };
    let section = Section::new()
        .title(title)
        .description("每个插件是一个视图：摆进工作台布局的是面板，没摆进去的打开时显示在主区域。")
        .child(toolbar_cell(entity.clone(), state));
    if state.plugins.is_empty() {
        return section.child(
            div()
                .text_sm()
                .text_color(rgb(crate::ui_theme::muted()))
                .child("还没有插件。"),
        );
    }
    section.children(state.plugins.iter().map(|plugin| {
        plugin_row(
            entity.clone(),
            plugin,
            state.pending_delete.as_deref() == Some(plugin.id.as_str()),
            state.busy,
        )
    }))
}

/// 列表第一格：左边说明导入会做什么，右边三个操作，导入是唯一的主按钮；下面一句警告。
fn toolbar_cell(entity: Entity<Workspace>, state: &PanelPluginsSnapshot) -> Div {
    let user_dir = state
        .user_dir
        .as_ref()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    let busy = state.busy;
    let import_entity = entity.clone();
    let open_entity = entity.clone();
    let reload_entity = entity;
    let row = Row::new("导入")
        .description(format!(
            "选一个含 gpui-shell.json 的目录，复制进 {user_dir}；改了脚本点「重新加载」，不用重启。"
        ))
        .child(
            Button::new("reload-panel-plugins")
                .quiet()
                .small()
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
                .small()
                .label("打开插件目录")
                .on_click(move |_, _, cx| {
                    open_entity.update(cx, |workspace, cx| {
                        workspace.open_panel_plugin_dir(cx);
                    });
                }),
        )
        .child(
            Button::new("import-panel-plugin")
                .small()
                .label("导入插件…")
                .loading(busy)
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    import_entity.update(cx, |workspace, cx| {
                        workspace.import_panel_plugin(cx);
                    });
                }),
        );
    // 用警告色：导入别人的插件等于给它整台机器的权限。
    let warning = div()
        .text_xs()
        .text_color(rgb(crate::ui_theme::need()))
        .child(
            "插件和 Smelt 一样能读写任意文件、运行任意命令，只导入你信任的。\
             删除的插件挪进 ~/.smelt/trash/plugins，需要时能找回。",
        );
    div().flex().flex_col().gap_2().child(row).child(warning)
}

/// 给插件作者看的：示例和日志。放在最后，不跟装插件的人抢位置。
fn authoring_section() -> Section {
    Section::new().title("自己写插件").child(
        Row::new("示例和教程")
            .description(
                "gpui-shell 脚本：JavaScript 描述界面，GPUI 原生绘制。console.log 和脚本错误写进日志，\
                 带 [plugin] 标记。",
            )
            .child(
                Button::new("open-panel-plugin-log")
                    .quiet()
                    .small()
                    .label("打开日志")
                    .on_click(|_, _, _| {
                        let opened = smelt_sys::app_log::log_path()
                            .ok_or_else(|| "找不到日志文件".to_string())
                            .and_then(|path| crate::system_open::open_in_system_text_editor(&path));
                        if let Err(error) = opened {
                            crate::status_item::notify_error(error);
                        }
                    }),
            )
            .child(
                smelt_ui::Link::new("open-panel-plugin-demo", DEMO_URL)
                    .text_sm()
                    .child("示例和教程 ↗"),
            ),
    )
}

fn plugin_row(
    entity: Entity<Workspace>,
    plugin: &InstalledPlugin,
    pending: bool,
    busy: bool,
) -> Row {
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

    let row = Row::new(plugin.name.clone()).description(detail.join("\n"));
    if !removable {
        return row;
    }
    if pending {
        let confirm_entity = entity.clone();
        let cancel_entity = entity;
        let confirm_id = id.clone();
        row.child(
            Button::new(format!("confirm-delete-panel-plugin-{id}"))
                .danger()
                .small()
                .label("确认删除")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let id = confirm_id.clone();
                    confirm_entity.update(cx, |workspace, cx| {
                        workspace.confirm_panel_plugin_delete(id, cx);
                    });
                }),
        )
        .child(
            Button::new(format!("cancel-delete-panel-plugin-{id}"))
                .quiet()
                .small()
                .label("取消")
                .on_click(move |_, _, cx| {
                    cancel_entity.update(cx, |workspace, cx| {
                        workspace.cancel_panel_plugin_delete(cx);
                    });
                }),
        )
    } else {
        row.child(
            Button::new(format!("delete-panel-plugin-{id}"))
                .quiet()
                .small()
                .label("删除…")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    entity.update(cx, |workspace, cx| {
                        workspace.request_panel_plugin_delete(id, cx);
                    });
                }),
        )
    }
}
