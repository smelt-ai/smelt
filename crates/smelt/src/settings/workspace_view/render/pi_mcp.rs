//! 能力库「MCP」页签：内置服务器一键添加，`~/.pi/agent/mcp.json` 里的服务器启停、
//! 登录、移除。排版和模型页签同一套（居中限宽、行内按钮 xsmall）。

use super::pi_model::{Palette, card, pill, section};
use super::*;
use crate::settings::workspace::PiConfigFileKind;
use smelt_core::pi_mcp::{PI_MCP_PRESETS, PiMcpPreset, PiMcpServer, PiMcpTransport};
use smelt_ui::IconAction as _;

const PAGE_MAX_WIDTH: f32 = 760.;

pub(super) fn pi_mcp_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> AnyElement {
    let palette = Palette::new(cx);
    let servers: &[PiMcpServer] = snapshot.pi_mcp_servers.as_deref().unwrap_or_default();
    div()
        .id("pi-mcp-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            v_flex()
                .w_full()
                .max_w(px(PAGE_MAX_WIDTH))
                .mx_auto()
                .px_6()
                .py_6()
                .gap_8()
                .child(presets_section(&entity, servers, palette))
                .child(servers_section(&entity, snapshot, servers, palette))
                .child(footer(&entity, palette)),
        )
        .into_any_element()
}

fn row(index: usize, palette: Palette) -> Div {
    let row = v_flex().w_full().px_4().py_3().gap_2();
    if index > 0 {
        row.border_t_1().border_color(palette.border)
    } else {
        row
    }
}

fn row_title(text: impl Into<SharedString>, palette: Palette) -> Div {
    div()
        .truncate()
        .text_sm()
        .text_color(palette.fg)
        .child(text.into())
}

fn row_detail(text: impl Into<SharedString>, palette: Palette) -> Div {
    div()
        .truncate()
        .text_xs()
        .text_color(palette.muted)
        .child(text.into())
}

fn presets_section(entity: &Entity<Workspace>, servers: &[PiMcpServer], palette: Palette) -> Div {
    let mut list = card(palette);
    for (index, preset) in PI_MCP_PRESETS.iter().enumerate() {
        let added = servers.iter().any(|server| server.name == preset.id);
        list = list.child(row(index, palette).child(preset_row(entity, preset, added, palette)));
    }
    section("内置", None, list, palette)
}

fn preset_row(
    entity: &Entity<Workspace>,
    preset: &'static PiMcpPreset,
    added: bool,
    palette: Palette,
) -> Div {
    let action = if added {
        pill("已添加", palette.muted).into_any_element()
    } else {
        let entity = entity.clone();
        Button::new(format!("pi-mcp-add-{}", preset.id))
            .xsmall()
            .label("添加")
            .tooltip(if preset.transport.needs_sign_in() {
                "写入 ~/.pi/agent/mcp.json，之后点「登录」授权"
            } else {
                "写入 ~/.pi/agent/mcp.json，新开的对话里就能用"
            })
            .on_click(move |_, _, cx| {
                entity.update(cx, |workspace, cx| {
                    workspace.add_pi_mcp_preset(preset.id, cx)
                });
            })
            .into_any_element()
    };
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(row_title(preset.name, palette))
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(preset.summary),
                ),
        )
        .child(div().flex_none().child(action))
}

fn servers_section(
    entity: &Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    servers: &[PiMcpServer],
    palette: Palette,
) -> Div {
    let mut body = v_flex().w_full().gap_2();
    if let Some(error) = &snapshot.pi_mcp_error {
        body = body.child(
            div()
                .text_xs()
                .text_color(palette.danger)
                .child(format!("操作失败：{error}")),
        );
    }
    match &snapshot.pi_mcp_servers {
        Err(error) => {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(palette.danger)
                    .child(format!("无法读取 mcp.json：{error}")),
            );
        }
        Ok(_) if servers.is_empty() => {
            body = body.child(
                card(palette).px_4().py_3().child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child("还没有 MCP 服务器。添加上面的内置服务器，或直接编辑 mcp.json。"),
                ),
            );
        }
        Ok(_) => {
            let mut list = card(palette);
            for (index, server) in servers.iter().enumerate() {
                let mut item =
                    row(index, palette).child(server_row(entity, snapshot, server, palette));
                if snapshot.pi_mcp_pending_remove.as_deref() == Some(server.name.as_str()) {
                    item = item.child(confirm_remove(entity, &server.name, palette));
                }
                list = list.child(item);
            }
            body = body.child(list);
        }
    }
    section("已配置", None, body, palette)
}

fn server_row(
    entity: &Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    server: &PiMcpServer,
    palette: Palette,
) -> Div {
    let detail = match &server.transport {
        PiMcpTransport::Http { url } => url.clone(),
        PiMcpTransport::Stdio { command, args } => {
            format!("本地命令 · {command} {}", args.join(" "))
        }
        PiMcpTransport::Unknown => "缺少 url 或 command".to_string(),
    };
    let logging_in = snapshot.pi_mcp_login.as_deref() == Some(server.name.as_str());
    let any_login = snapshot.pi_mcp_login.is_some();
    let removing = snapshot.pi_mcp_pending_remove.is_some();

    let mut badges = h_flex()
        .gap_2()
        .items_center()
        .child(row_title(server.name.clone(), palette));
    if !server.enabled {
        badges = badges.child(pill("已停用", palette.muted));
    } else if server.signed_in {
        badges = badges.child(pill("已登录", palette.success));
    }

    let login = (server.is_http() && server.enabled).then(|| {
        let entity = entity.clone();
        let name = server.name.clone();
        Button::new(format!("pi-mcp-login-{}", server.name))
            .xsmall()
            .label(if logging_in {
                "等待浏览器授权…"
            } else if server.signed_in {
                "重新登录"
            } else {
                "登录"
            })
            .tooltip("用浏览器授权；不需要登录的服务器会直接提示已连接")
            .disabled(any_login)
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |workspace, cx| workspace.login_pi_mcp_server(name, cx));
            })
    });
    let toggle = {
        let entity = entity.clone();
        let name = server.name.clone();
        let enable = !server.enabled;
        Button::new(format!("pi-mcp-toggle-{}", server.name))
            .ghost()
            .xsmall()
            .label(if enable { "启用" } else { "停用" })
            .disabled(logging_in)
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |workspace, cx| {
                    workspace.set_pi_mcp_server_enabled(name, enable, cx);
                });
            })
    };
    let remove = {
        let entity = entity.clone();
        let name = server.name.clone();
        Button::new(format!("pi-mcp-remove-{}", server.name))
            .ghost()
            .xsmall()
            .icon(crate::trash_icon())
            .icon_action("从 mcp.json 移除")
            .disabled(logging_in || removing)
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |workspace, cx| {
                    workspace.request_pi_mcp_remove(name, cx)
                });
            })
    };

    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(badges)
                .child(row_detail(detail, palette)),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .items_center()
                .children(login)
                .child(toggle)
                .child(remove),
        )
}

fn confirm_remove(entity: &Entity<Workspace>, name: &str, palette: Palette) -> Div {
    let confirm_entity = entity.clone();
    let cancel_entity = entity.clone();
    let confirm_name = name.to_string();
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div().text_xs().text_color(palette.muted).child(
                "从 mcp.json 删掉这一项。登录凭据保留，重新添加同名同地址的服务器不用再登录。",
            ),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .child(
                    Button::new("pi-mcp-remove-cancel")
                        .ghost()
                        .xsmall()
                        .label("取消")
                        .on_click(move |_, _, cx| {
                            cancel_entity
                                .update(cx, |workspace, cx| workspace.cancel_pi_mcp_remove(cx));
                        }),
                )
                .child(
                    Button::new("pi-mcp-remove-confirm")
                        .danger()
                        .xsmall()
                        .label("移除")
                        .on_click(move |_, _, cx| {
                            let name = confirm_name.clone();
                            confirm_entity.update(cx, |workspace, cx| {
                                workspace.confirm_pi_mcp_remove(name, cx);
                            });
                        }),
                ),
        )
}

/// 原生文件入口，以及「改了要怎样才生效」的提示：Pi 只在会话启动时读 mcp.json。
fn footer(entity: &Entity<Workspace>, palette: Palette) -> Div {
    let open_entity = entity.clone();
    v_flex()
        .w_full()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(palette.border)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex_none()
                        .mr_2()
                        .text_xs()
                        .text_color(palette.muted)
                        .child("原生配置文件"),
                )
                .child(
                    Button::new("pi-edit-mcp-file")
                        .ghost()
                        .xsmall()
                        .label("mcp.json")
                        .tooltip(smelt_core::pi_mcp::pi_mcp_path().display().to_string())
                        .on_click(move |_, window, cx| {
                            open_entity.update(cx, |workspace, cx| {
                                workspace.open_pi_config(PiConfigFileKind::Mcp, window, cx);
                            });
                        }),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(palette.muted)
                .child(
                    "普通 Pi 对话连所有启用的服务器；智能体只连在它的详情页里勾选的。改动对新开的对话生效，已经开着的对话发 /reload 重新加载。",
                ),
        )
}
