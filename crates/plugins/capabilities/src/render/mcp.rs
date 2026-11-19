//! 能力库「MCP」页签：内置服务器一键添加，`~/.pi/agent/mcp.json` 里的服务器启停、
//! 登录、移除，每台写着谁在用、能直接勾给智能体用。排版和模型页签同一套（居中限宽、行内按钮 xsmall）。

use super::*;
use crate::agents::Usable;
use crate::models::PiConfigFileKind;
use api::model::pi_mcp::{PI_MCP_PRESETS, PiMcpPreset, PiMcpServer, PiMcpTransport};
use api::ui::IconAction as _;
use api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::kit::TYPE;

const PAGE_MAX_WIDTH: f32 = 760.;

#[derive(Clone, Copy)]
struct Palette {
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
    danger: Hsla,
}

impl Palette {
    fn new() -> Self {
        Self {
            fg: rgb(ui_theme::text()).into(),
            muted: rgb(ui_theme::muted()).into(),
            border: ui_theme::hair().into(),
            danger: rgb(ui_theme::bad()).into(),
        }
    }
}

fn section(
    title: impl Into<SharedString>,
    action: Option<AnyElement>,
    body: impl IntoElement,
    palette: Palette,
) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .w_full()
                .min_h(px(28.))
                .justify_between()
                .child(
                    // 分组标题跟侧栏「最近对话」「项目」一个级别：小号、灰、不加粗。
                    div()
                        .type_style(TYPE.text_xsmall_medium)
                        .text_color(palette.muted)
                        .child(title.into()),
                )
                .children(action),
        )
        .child(body)
}

fn card(palette: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded_lg()
        .border_1()
        .border_color(palette.border)
}

pub(super) fn pi_mcp_page(
    entity: Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    cx: &App,
) -> AnyElement {
    let palette = Palette::new();
    let servers: &[PiMcpServer] = page.pi_mcp_servers().map(Vec::as_slice).unwrap_or_default();
    div()
        .id("pi-mcp-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .max_w(px(PAGE_MAX_WIDTH))
                .mx_auto()
                .px_6()
                .py_6()
                .gap_8()
                .child(presets_section(
                    &entity,
                    servers,
                    page.pi_mcp_remote_desktop(),
                    palette,
                ))
                .child(servers_section(&entity, page, servers, palette, cx))
                .child(footer(&entity, &page.pi_paths().mcp, palette)),
        )
        .into_any_element()
}

fn row(index: usize, palette: Palette) -> Div {
    let row = div().flex().flex_col().w_full().px_4().py_3().gap_2();
    if index > 0 {
        row.border_t_1().border_color(palette.border)
    } else {
        row
    }
}

fn row_title(text: impl Into<SharedString>, palette: Palette) -> Div {
    div()
        .overflow_hidden()
        .type_style(TYPE.ui)
        .text_color(palette.fg)
        .child(truncated_line(text))
}

fn row_detail(text: impl Into<SharedString>, palette: Palette) -> Div {
    div()
        .overflow_hidden()
        .type_style(TYPE.text_xsmall)
        .text_color(palette.muted)
        .child(truncated_line(text))
}

fn presets_section(
    entity: &Entity<CapabilitiesPage>,
    servers: &[PiMcpServer],
    remote_desktop: bool,
    palette: Palette,
) -> Div {
    let mut list = card(palette);
    for (index, preset) in PI_MCP_PRESETS.iter().enumerate() {
        // 添加时运行时按同一个判断写入，这里显示的就是会写进去的那一套。
        let preset = preset.for_machine(remote_desktop);
        let added = servers.iter().any(|server| server.name == preset.id);
        list = list.child(row(index, palette).child(preset_row(entity, preset, added, palette)));
    }
    section(t!("mcp.builtin"), None, list, palette)
}

fn preset_row(
    entity: &Entity<CapabilitiesPage>,
    preset: &'static PiMcpPreset,
    added: bool,
    palette: Palette,
) -> Div {
    let action = if added {
        Badge::new()
            .child(t!("mcp.added").into_owned())
            .into_any_element()
    } else {
        let entity = entity.clone();
        // 点完会换成上面的「已添加」胶囊，按钮用同高的胶囊占同一个位置，各行右侧轮廓一致。
        Button::new(format!("pi-mcp-add-{}", preset.id))
            .small()
            .label(t!("mcp.add"))
            .tooltip(if preset.transport.needs_sign_in() {
                t!("mcp.add_tooltip_sign_in")
            } else {
                t!("mcp.add_tooltip")
            })
            .on_click(move |_, _, cx| {
                entity.update(cx, |workspace, cx| {
                    workspace.add_pi_mcp_preset(preset.id, cx)
                });
            })
            .into_any_element()
    };
    // 加没加都留着：先添加、后装扩展也是常见顺序。
    let install = preset.extension_url.map(|url| {
        Button::new(format!("pi-mcp-extension-{}", preset.id))
            .quiet()
            .small()
            .label(t!("mcp.install_extension"))
            .tooltip(t!("mcp.install_extension_tooltip"))
            .on_click(move |_, _, cx| cx.open_url(url))
    });
    div()
        .flex()
        .items_center()
        .w_full()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(row_title(preset.name(), palette))
                .child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(palette.muted)
                        .child(preset.summary()),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap_1()
                .children(install)
                .child(action),
        )
}

fn servers_section(
    entity: &Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    servers: &[PiMcpServer],
    palette: Palette,
    cx: &App,
) -> Div {
    let mut body = div().flex().flex_col().w_full().gap_2();
    if let Some(error) = &page.pi_mcp_error {
        body = body.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(palette.danger)
                .child(t!("common.action_failed", error = error).into_owned()),
        );
    }
    match page.pi_mcp_servers() {
        Err(error) => {
            body = body.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(palette.danger)
                    .child(t!("mcp.read_failed", error = error).into_owned()),
            );
        }
        Ok(_) if servers.is_empty() => {
            body = body.child(
                card(palette).px_4().py_3().child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(palette.muted)
                        .child(t!("mcp.empty").into_owned()),
                ),
            );
        }
        Ok(_) => {
            if let Some(error) = super::uses::agent_edit_error(page) {
                body = body.child(error);
            }
            let mut list = card(palette);
            for (index, server) in servers.iter().enumerate() {
                let mut item =
                    row(index, palette).child(server_row(entity, page, server, palette, cx));
                if page.pi_mcp_pending_remove.as_deref() == Some(server.name.as_str()) {
                    item = item.child(confirm_remove(entity, &server.name, palette));
                }
                if let Some(view) = page
                    .pi_login
                    .as_ref()
                    .filter(|view| view.mcp_server() == Some(server.name.as_str()))
                {
                    item = item.child(super::auth::login_panel(entity.clone(), view, cx));
                }
                list = list.child(item);
            }
            body = body.child(list);
        }
    }
    section(t!("mcp.configured"), None, body, palette)
}

fn server_row(
    entity: &Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    server: &PiMcpServer,
    palette: Palette,
    cx: &App,
) -> Div {
    let usable = Usable::Mcp(server.name.clone());
    let used_by = super::uses::used_by_line(&page.machine, &usable, cx);
    let uses_button = super::uses::uses_button(entity, page, usable, &server.name, cx);
    let detail = match &server.transport {
        PiMcpTransport::Http { url } => url.clone(),
        PiMcpTransport::Stdio { command, args } => t!(
            "mcp.local_command",
            command = command,
            args = args.join(" ")
        )
        .into_owned(),
        PiMcpTransport::Unknown => t!("mcp.missing_transport").into_owned(),
    };
    // 结束了（失败）的登录面板还留着看原因，不再挡按钮：点「登录」就是重试。
    let running_login = page
        .pi_login
        .as_ref()
        .filter(|view| view.error.is_none())
        .and_then(|view| view.mcp_server());
    let logging_in = running_login == Some(server.name.as_str());
    let any_login = running_login.is_some();
    let removing = page.pi_mcp_pending_remove.is_some();

    let mut badges = div()
        .flex()
        .items_center()
        .gap_2()
        .child(row_title(server.name.clone(), palette));
    if !server.enabled {
        badges = badges.child(Badge::new().child(t!("mcp.disabled").into_owned()));
    } else if server.signed_in {
        badges = badges.child(
            Badge::new()
                .ok()
                .child(t!("auth.status.oauth").into_owned()),
        );
    }

    let login = (server.is_http() && server.enabled).then(|| {
        let entity = entity.clone();
        let name = server.name.clone();
        Button::new(format!("pi-mcp-login-{}", server.name))
            .small()
            .label(if logging_in {
                t!("mcp.waiting")
            } else if server.signed_in {
                t!("auth.sign_in_again")
            } else {
                t!("auth.sign_in")
            })
            .tooltip(t!("mcp.sign_in_tooltip"))
            .disabled(any_login)
            .on_click(move |_, window, cx| {
                let name = name.clone();
                entity.update(cx, |workspace, cx| {
                    workspace.start_pi_mcp_login(name, window, cx)
                });
            })
    });
    let toggle = {
        let entity = entity.clone();
        let name = server.name.clone();
        let enable = !server.enabled;
        Button::new(format!("pi-mcp-toggle-{}", server.name))
            .quiet()
            .small()
            .label(if enable {
                t!("mcp.enable")
            } else {
                t!("mcp.disable")
            })
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
            .quiet()
            .small()
            .icon(IconName::Trash)
            .icon_action(t!("mcp.remove_tooltip"))
            .disabled(logging_in || removing)
            .on_click(move |_, _, cx| {
                let name = name.clone();
                entity.update(cx, |workspace, cx| {
                    workspace.request_pi_mcp_remove(name, cx)
                });
            })
    };

    div()
        .flex()
        .items_center()
        .w_full()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(badges)
                .child(row_detail(detail, palette))
                .children(used_by.map(|line| row_detail(line, palette))),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap_1()
                .children(uses_button)
                .children(login)
                .child(toggle)
                .child(remove),
        )
}

fn confirm_remove(entity: &Entity<CapabilitiesPage>, name: &str, palette: Palette) -> Div {
    let confirm_entity = entity.clone();
    let cancel_entity = entity.clone();
    let confirm_name = name.to_string();
    div()
        .flex()
        .items_center()
        .w_full()
        .justify_between()
        .gap_3()
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(palette.muted)
                .child(t!("mcp.remove_body").into_owned()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap_1()
                .child(
                    Button::new("pi-mcp-remove-cancel")
                        .quiet()
                        .small()
                        .label(t!("common.cancel"))
                        .on_click(move |_, _, cx| {
                            cancel_entity
                                .update(cx, |workspace, cx| workspace.cancel_pi_mcp_remove(cx));
                        }),
                )
                .child(
                    Button::new("pi-mcp-remove-confirm")
                        .danger()
                        .small()
                        .label(t!("mcp.remove"))
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
fn footer(entity: &Entity<CapabilitiesPage>, mcp_path: &str, palette: Palette) -> Div {
    let open_entity = entity.clone();
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(palette.border)
        .child(
            div()
                .flex()
                .items_center()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .flex_none()
                        .mr_2()
                        .type_style(TYPE.text_xsmall)
                        .text_color(palette.muted)
                        .child(t!("config.native_files").into_owned()),
                )
                .child(
                    Button::new("pi-edit-mcp-file")
                        .quiet()
                        .small()
                        .label("mcp.json")
                        .tooltip(mcp_path.to_string())
                        .on_click(move |_, window, cx| {
                            open_entity.update(cx, |workspace, cx| {
                                workspace.open_pi_config(PiConfigFileKind::Mcp, window, cx);
                            });
                        }),
                ),
        )
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(palette.muted)
                .child(t!("mcp.footer").into_owned()),
        )
}
