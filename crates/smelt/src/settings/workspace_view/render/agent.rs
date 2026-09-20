//! 设置页：模型与 Agent（运行环境 / 新建与启动 / 对话工作区 / 通知 / Hooks）。

use super::*;

pub(super) fn runtime_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    wrap_agent_page(
        "Agent 运行环境",
        Some("查看 Smelt 用于启动 Agent 对话的内置运行时与本机 CLI 状态。"),
        0,
        entity,
        snapshot,
        cx,
    )
}

pub(super) fn launch_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    wrap_agent_page(
        "新建与启动",
        Some("管理项目行“+”菜单中显示的终端与 Agent 启动命令。"),
        2,
        entity,
        snapshot,
        cx,
    )
}

pub(super) fn workspace_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    wrap_agent_page(
        "对话与工作区",
        Some(
            "管理执行引擎启动命令、额外 workspace 与跨 Agent 通讯。智能体定义请在\
             主窗口左侧「智能体」中管理；API key 与网关地址在「DeepSeek Harness」或「Pi 模型」页面。",
        ),
        3,
        entity,
        snapshot,
        cx,
    )
}

pub(super) fn notify_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    wrap_agent_page(
        "通知",
        Some("选择哪些 Agent 状态和终端事件需要提醒你。"),
        1,
        entity,
        snapshot,
        cx,
    )
}

pub(super) fn hooks_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    wrap_agent_page(
        "Agent Hooks",
        Some("将 Smelt 通知与权限处理接入本机 Agent 配置。"),
        4,
        entity,
        snapshot,
        cx,
    )
}

fn wrap_agent_page(
    title: &'static str,
    description: Option<&'static str>,
    group_ix: usize,
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    // agent_groups 顺序：0 运行环境 / 1 通知 / 2 新建与启动 / 3 对话与工作区 / 4 Hooks
    let group = agent_groups(entity, snapshot, cx)
        .into_iter()
        .nth(group_ix)
        .expect("agent 设置分组下标应对应 wrap_agent_page 的调用");
    let page = SettingPage::new(title);
    if let Some(description) = description {
        page.description(description).group(group)
    } else {
        page.group(group)
    }
}

fn agent_groups(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    _cx: &App,
) -> Vec<SettingGroup> {
    // —— 启动：项目「+」下拉菜单的可配置启动项 ——
    // Settings 的 list 测量项高度时，百分比宽度（w_full）经常解析不到确定父宽，
    // 卡片会缩成「内容宽」——输入框只露出几个字。这里用窗口视口算绝对像素宽。
    let launch_editor_entity = entity.clone();
    let launch_rows = snapshot.launch_rows.clone();
    let launch_group = SettingGroup::new()
            .item(
                    SettingItem::render(move |_, window, cx: &mut App| {
                        let muted = cx.theme().muted_foreground;
                        let border = cx.theme().border;
                        let fg = cx.theme().foreground;
                        let popover = cx.theme().popover;
                        let secondary = cx.theme().secondary;
                        let danger = cx.theme().danger;
                        let danger_fg = cx.theme().danger_foreground;
                        // 侧栏默认 250 + 左右 padding/滚动条余量；再夹到合理区间。
                        let field_w = {
                            let vw = f32::from(window.viewport_size().width);
                            let w = (vw - 250. - 80.).clamp(360., 720.);
                            px(w)
                        };
                        let mut col = v_flex()
                                .w(field_w)
                                .gap_3()
                                .child(
                                    v_flex()
                                        .w(field_w)
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(fg)
                                                .child("快捷启动项"),
                                        )
                                        .child(
                                            div().w(field_w).text_sm().text_color(muted).child(
                                                "项目行「+」菜单里除「新建终端」「新建 Worktree…」外的项。\
                                                 显示名会出现在菜单上；命令是在该项目目录下执行的 shell 命令\
                                                 （可含参数）。内置 Agent 项固定存在，自定义项可删除。",
                                            ),
                                        ),
                                );
                            // 名称和命令并排成两列，而不是上下堆叠：之前两个输入框同宽同字体，
                            // 只靠上方一行小灰字区分，扫视时根本分不出哪个是哪个。改成
                            // 「窄名称列 + 宽命令列 + 命令用等宽字体」——列位置、宽度、字体三重
                            // 区分，比标签文字有效得多，顺带把每项从 4 行压到 1 行。
                            // 名称短（"Claude Code" 这种）、命令长（带一串参数），宽度按
                            // 信息量分：名称够放就行，剩下的全给命令。
                            let name_w = px(140.);
                            let del_w = px(28.);
                            let cmd_w = field_w - name_w - del_w - px(40.);
                            let mono = terminal_view::font_family();

                            let mut list = v_flex()
                                .w(field_w)
                                .gap_2()
                                .p_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(border)
                                .bg(secondary)
                                // 列名只在表头出现一次，不必每项重复一遍「名称」「命令」。
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(div().w(name_w).child("名称"))
                                        .child(div().w(cmd_w).child("命令"))
                                        // 占位：让表头两列跟下面的行严格对齐（删除按钮那一列）。
                                        .child(div().w(del_w)),
                                );
                            for (ix, (label, command, removable)) in launch_rows.iter().enumerate() {
                                let del_entity = launch_editor_entity.clone();
                                let row_ix = ix;
                                let delete_slot = if *removable {
                                    div()
                                        .id(("del-launch", row_ix))
                                        .size(del_w)
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .justify_center()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_sm()
                                        .text_color(muted)
                                        // 删除是破坏性操作，hover 时给红底明示。
                                        .hover(|s| s.bg(danger).text_color(danger_fg))
                                        .child("×")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |_, _, cx: &mut App| {
                                                del_entity.update(cx, |ws, cx| {
                                                    ws.remove_launch_entry(row_ix, cx);
                                                });
                                            },
                                        )
                                        .into_any_element()
                                } else {
                                    // 内置启动项固定存在；保留空槽以维持两列对齐。
                                    div().size(del_w).flex_none().into_any_element()
                                };
                                list = list.child(
                                    h_flex()
                                        .id(("launch-row", row_ix))
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .child(Input::new(label).w(name_w))
                                        // 命令是 shell 代码，用终端同款等宽字体——参数里的
                                        // `-`/`_` 对齐后好读，也一眼跟左边的显示名区分开。
                                        .child(
                                            Input::new(command)
                                                .w(cmd_w)
                                                .font_family(mono.clone()),
                                        )
                                        .child(delete_slot),
                                );
                            }
                            col = col.child(list);
                            let add_entity = launch_editor_entity.clone();
                            col.child(
                                div()
                                    .id("add-launch")
                                    .h(px(36.))
                                    .w(field_w)
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .text_sm()
                                    .text_color(fg)
                                    .bg(popover)
                                    .border_1()
                                    .border_color(border)
                                    .hover(|s| s.bg(border))
                                    .child("+ 添加启动项")
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                        add_entity.update(cx, |ws, cx| ws.add_launch_entry(cx));
                                    }),
                            )
                            .into_any_element()
                    })
                    .keywords(["快捷启动", "launch", "命令", "claude", "codex", "copilot"]),
                );

    vec![
            SettingGroup::new()
                .item(SettingItem::render({
                    let runtime_entity = entity.clone();
                    move |_, _, cx: &mut App| {
                        let runtime = cx
                            .try_global::<AcpRuntimeState>()
                            .cloned()
                            .unwrap_or_default();
                        let (fg, muted, border) = {
                            let theme = cx.theme();
                            (theme.foreground, theme.muted_foreground, theme.border)
                        };
                        let refresh_entity = runtime_entity.clone();
                        let refresh = Button::new("refresh-acp-runtime")
                            .ghost()
                            .small()
                            .icon(IconName::Redo)
                            .loading_icon(IconName::LoaderCircle)
                            .loading(runtime.refreshing)
                            .disabled(runtime.refreshing || runtime.installing.is_some())
                            .tooltip("刷新运行环境检测")
                            .on_click(move |_, _, cx| {
                                refresh_entity.update(cx, |workspace, cx| {
                                    workspace.refresh_acp_runtime(cx);
                                });
                            });

                        let mut content = v_flex()
                            .w_full()
                            .gap_3()
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(runtime_checked_at_text(runtime.checked_at)),
                                    )
                                    .child(refresh),
                            );

                        let Some(diagnostics) = runtime.diagnostics else {
                            return content
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(muted)
                                        .child(if runtime.refreshing {
                                            "正在检测本机 Agent 与运行时…"
                                        } else {
                                            "尚未取得运行环境状态"
                                        }),
                                )
                                .into_any_element();
                        };

                        let runtime_row = |agent: ConversationAgentKind,
                                           executable: &smelt_core::acp_conn::RuntimeExecutable| {
                            let (status, color): (String, Hsla) = if !executable.is_available() {
                                ("未安装".to_string(), rgb(crate::ui_theme::red()).into())
                            } else if let Some(error) = &executable.error {
                                (format!("检测失败：{error}"), rgb(crate::ui_theme::yellow()).into())
                            } else {
                                (
                                    executable
                                        .version
                                        .clone()
                                        .unwrap_or_else(|| "已安装（未返回版本）".to_string()),
                                    rgb(crate::ui_theme::green()).into(),
                                )
                            };
                            let path = executable
                                .path
                                .clone()
                                .unwrap_or_else(|| "未在 Smelt 的 Agent 搜索路径中找到".to_string());
                            let install_button = (!executable.is_available()).then(|| {
                                    let install_entity = runtime_entity.clone();
                                    let installing_this = runtime.installing == Some(agent);
                                    Button::new(format!("install-acp-cli-{}", agent.id()))
                                        .secondary()
                                        .small()
                                        .icon(IconName::Plus)
                                        .label(if installing_this { "安装中…" } else { "安装" })
                                        .loading_icon(IconName::LoaderCircle)
                                        .loading(installing_this)
                                        .disabled(runtime.refreshing || runtime.installing.is_some())
                                        .tooltip(format!("安装 {}", agent.label()))
                                        .on_click(move |_, _, cx| {
                                            install_entity.update(cx, |workspace, cx| {
                                                workspace.install_acp_cli(agent, cx);
                                            });
                                        })
                                });
                            h_flex()
                                .w_full()
                                .min_h(px(46.))
                                .items_center()
                                .gap_3()
                                .border_t_1()
                                .border_color(border)
                                .child(
                                    div()
                                        .w(px(102.))
                                        .flex_none()
                                        .text_sm()
                                        .text_color(fg)
                                        .child(agent.label()),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(px(2.))
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(color)
                                                .child(status),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(path),
                                        ),
                                )
                                .children(install_button)
                        };

                        let install_message = runtime.install_message.clone().map(|message| {
                            div()
                                .text_xs()
                                .text_color(if runtime.install_failed {
                                    rgb(crate::ui_theme::red())
                                } else {
                                    rgb(crate::ui_theme::green())
                                })
                                .child(message)
                        });

                        content = content.child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(muted)
                                .child("Agent 对话运行时"),
                        );
                        for agent in ConversationAgentKind::ALL
                            .into_iter()
                            .filter(|agent| agent.is_bare_kind())
                        {
                            if let Some(executable) = diagnostics.for_agent(agent) {
                                content = content.child(runtime_row(agent, executable));
                            }
                        }
                        content = content.children(install_message);
                        content.into_any_element()
                    }
                })
                .keywords([
                    "运行环境",
                    "版本",
                    "安装",
                    "检测",
                    "cli",
                    "claude",
                    "codex",
                    "copilot",
                    "grok",
                    "cursor",
                    "opencode",
                    "kiro",
                    "pi",
                ])),
            SettingGroup::new()
                .item(
                    SettingItem::new(
                        "系统通知权限",
                        SettingField::render(|_, _, cx: &mut App| {
                            let status = crate::status_item::system_notification_status();
                            let presentation =
                                crate::status_item::system_notification_status_presentation(&status);
                            let color = if presentation.is_error {
                                cx.theme().danger
                            } else if status.authorization
                                == crate::status_item::SystemNotificationAuthorization::Authorized
                            {
                                rgb(crate::ui_theme::green()).into()
                            } else {
                                cx.theme().muted_foreground
                            };
                            h_flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .flex_wrap()
                                .child(div().size(px(6.)).rounded_full().bg(color))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(color)
                                        .child(presentation.text),
                                )
                                .children(presentation.is_error.then(|| {
                                    Button::new("refresh-system-notification-status")
                                        .small()
                                        .ghost()
                                        .label("重新检查")
                                        .on_click(|_, _, _| {
                                            crate::status_item::refresh_system_notification_authorization();
                                        })
                                }))
                                .children(presentation.can_open_settings.then(|| {
                                    Button::new("open-system-notification-settings")
                                        .small()
                                        .ghost()
                                        .icon(IconName::ExternalLink)
                                        .label("系统设置")
                                        .tooltip("打开 macOS 通知设置")
                                        .on_click(|_, _, cx| {
                                            cx.open_url(
                                                crate::status_item::SYSTEM_NOTIFICATION_SETTINGS_URL,
                                            );
                                        })
                                }))
                                .into_any_element()
                        }),
                    )
                    .description("提醒类型开关独立保存；macOS 权限决定系统是否实际显示通知。")
                    .keywords(["通知", "notification", "权限", "macOS"]),
                )
                .item(
                    SettingItem::new(
                        "等待批准通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentHostState>()
                                    .map(|c| c.notify_approval)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_host(|c| c.notify_approval = v, cx);
                            },
                        ),
                    )
                    .description("Agent 明确进入等待审批状态时提醒。")
                    .keywords(["通知", "notification", "审批"]),
                )
                .item(
                    SettingItem::new(
                        "等待输入通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentHostState>()
                                    .map(|c| c.notify_input)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_host(|c| c.notify_input = v, cx);
                            },
                        ),
                    )
                    .description("Agent 提问或等待你继续时提醒。"),
                )
                .item(
                    SettingItem::new(
                        "任务完成通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentHostState>()
                                    .map(|c| c.notify_success)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_host(|c| c.notify_success = v, cx);
                            },
                        ),
                    )
                    .description("Agent 当前回合正常完成时提醒。"),
                )
                .item(
                    SettingItem::new(
                        "任务失败通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentHostState>()
                                    .map(|c| c.notify_failure)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_host(|c| c.notify_failure = v, cx);
                            },
                        ),
                    )
                    .description("Agent 因错误中断时提醒。"),
                )
                .item(
                    SettingItem::new(
                        "终端响铃通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentHostState>()
                                    .map(|c| c.notify_terminal_bell)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_host(|c| c.notify_terminal_bell = v, cx);
                            },
                        ),
                    )
                    .description("终端输出 BEL 控制字符时显示普通信息提醒。")
                    .keywords(["通知", "notification", "响铃", "bell"]),
                ),
            launch_group,
            SettingGroup::new()
                // 按 `ConversationAgentKind::ALL` 迭代，不逐家手写：之前这里硬编码了五家，
                // dsh 加进来之后就一直没有输入框——加一家 agent 不该需要记得回到
                // 设置页再抄一遍。
                .items(
                    ConversationAgentKind::ALL
                        .into_iter()
                        .filter(|kind| kind.is_bare_kind())
                        .map(acp_cmd_setting_item),
                )
                .item(
                    SettingItem::new(
                        "跨 Agent 通讯",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentHostState>()
                                    .map(|c| c.cross_agent_enabled)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_host(|c| c.cross_agent_enabled = v, cx);
                            },
                        ),
                    )
                    .description(
                        "允许会话通过 Smelt MCP 相互发送消息。关闭后立即停止收发；关闭期间新建的会话重新开启后需重启才会获得工具。",
                    )
                    .keywords(["跨 agent", "消息", "MCP", "协作"]),
                )
                .item(
                    SettingItem::render({
                        let profile_editor_entity = entity;
                        let profile_rows = snapshot.profile_rows.clone();
                        move |_, window, cx: &mut App| {
                            let muted = cx.theme().muted_foreground;
                            let border = cx.theme().border;
                            let fg = cx.theme().foreground;
                            let popover = cx.theme().popover;
                            let secondary = cx.theme().secondary;
                            let danger = cx.theme().danger;
                            let danger_fg = cx.theme().danger_foreground;
                            let field_w = {
                                let vw = f32::from(window.viewport_size().width);
                                let w = (vw - 250. - 80.).clamp(360., 720.);
                                px(w)
                            };
                            let mut col = v_flex()
                                    .w(field_w)
                                    .gap_3()
                                    .child(
                                        v_flex()
                                            .w(field_w)
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .text_color(fg)
                                                    .child("手动添加 workspace"),
                                            )
                                            .child(
                                                div().w(field_w).text_sm().text_color(muted).child(
                                                    "同一家 agent 可以同时用好几个 workspace（比如 Claude \
                                                     默认的 ~/.claude 之外再开一个 ~/.claude-quant）。选好\
                                                     agent 类型、填上目录，启动命令自动拼好，不用自己写 \
                                                     shell 语法。「新建对话」菜单和历史会话页都会多出对应\
                                                     的入口。",
                                                ),
                                            ),
                                    );

                                let kind_w = px(120.);
                                let name_w = px(140.);
                                let del_w = px(28.);
                                let dir_w = field_w - kind_w - name_w - del_w - px(56.);
                                let mono = terminal_view::font_family();

                                let mut list = v_flex()
                                    .w(field_w)
                                    .gap_2()
                                    .p_3()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(border)
                                    .bg(secondary)
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(div().w(kind_w).child("Agent"))
                                            .child(div().w(name_w).child("名称"))
                                            .child(div().w(dir_w).child("Workspace 目录"))
                                            .child(div().w(del_w)),
                                    );

                                let profiles = cx.global::<AgentHostState>().profiles.clone();
                                for (ix, ((label, dir), p)) in
                                    profile_rows.iter().zip(profiles.iter()).enumerate()
                                {
                                    let row_ix = ix;
                                    let kind_entity = profile_editor_entity.clone();
                                    let del_entity = profile_editor_entity.clone();
                                    let current_kind_label = p
                                        .kind()
                                        .map(|kind| kind.short_label().to_string())
                                        .unwrap_or_else(|| format!("未知 ({})", p.kind_id));
                                    list = list.child(
                                        h_flex()
                                            .id(("profile-row", row_ix))
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                Button::new(("profile-kind", row_ix))
                                                    .ghost()
                                                    .small()
                                                    .w(kind_w)
                                                    .label(current_kind_label)
                                                    .dropdown_menu(move |mut menu, _window, _cx| {
                                                        for kind in ConversationAgentKind::ALL
                                                            .into_iter()
                                                            .filter(|kind| kind.is_bare_kind())
                                                        {
                                                            let kind_entity = kind_entity.clone();
                                                            menu = menu.item(
                                                                PopupMenuItem::new(kind.label())
                                                                    .on_click(move |_ev, _window, cx| {
                                                                        kind_entity.update(cx, |ws, cx| {
                                                                            ws.set_profile_kind(
                                                                                row_ix, kind, cx,
                                                                            );
                                                                        });
                                                                    }),
                                                            );
                                                        }
                                                        menu
                                                    }),
                                            )
                                            .child(Input::new(label).w(name_w))
                                            .child(
                                                Input::new(dir).w(dir_w).font_family(mono.clone()),
                                            )
                                            .child(
                                                div()
                                                    .id(("del-profile", row_ix))
                                                    .size(del_w)
                                                    .flex()
                                                    .flex_none()
                                                    .items_center()
                                                    .justify_center()
                                                    .rounded_md()
                                                    .cursor_pointer()
                                                    .text_sm()
                                                    .text_color(muted)
                                                    .hover(|s| s.bg(danger).text_color(danger_fg))
                                                    .child("×")
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        move |_, _, cx: &mut App| {
                                                            del_entity.update(cx, |ws, cx| {
                                                                ws.remove_profile(row_ix, cx);
                                                            });
                                                        },
                                                    ),
                                            ),
                                    );
                                }
                                col = col.child(list);
                                let add_entity = profile_editor_entity.clone();
                                col.child(
                                    div()
                                        .id("add-profile")
                                        .h(px(36.))
                                        .w(field_w)
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_lg()
                                        .cursor_pointer()
                                        .text_sm()
                                        .text_color(fg)
                                        .bg(popover)
                                        .border_1()
                                        .border_color(border)
                                        .hover(|s| s.bg(border))
                                        .child("+ 添加 workspace")
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                            add_entity.update(cx, |ws, cx| ws.add_profile(cx));
                                        }),
                                )
                                .into_any_element()
                        }
                    })
                    .keywords(["workspace", "claude-quant", "config dir", "多工作区", "agent"]),
                ),
            SettingGroup::new()
                .item(SettingItem::render(move |_, _, cx: &mut App| {
                    // render 路径不走实时读盘：hooks 状态 5s 缓存，装/卸后主动失效。
                    let hook_status = hooks_installed_status();
                    let installed = hook_status.iter().all(|provider| provider.installed);
                    let (fg, muted, border) = {
                        let t = cx.theme();
                        (t.foreground, t.muted_foreground, t.border)
                    };
                    let success: Hsla = rgb(crate::ui_theme::green()).into();
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex().gap_2().flex_wrap().children(
                                hook_status.into_iter().map(|provider| {
                                    div()
                                        .text_sm()
                                        .text_color(if provider.installed { success } else { muted })
                                        .child(format!(
                                            "{} {}",
                                            provider.label,
                                            if provider.installed {
                                                "已接入"
                                            } else {
                                                "未接入"
                                            }
                                        ))
                                }),
                            ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!(
                                    "路径：{}",
                                    smelt_notify_path().display()
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .id("install-agent-hooks")
                                        .px_3()
                                        .py(px(6.))
                                        .rounded_md()
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(border)
                                        .bg(crate::ui_theme::tint(crate::ui_theme::green(), 0x22))
                                        .text_sm()
                                        .text_color(rgb(crate::ui_theme::green()))
                                        .hover(|s| s.opacity(0.9))
                                        .child(if installed {
                                            "重新安装 hooks"
                                        } else {
                                            "安装 hooks"
                                        })
                                        .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                                            // 先记用户意图：即使某个 provider 本次安装失败，
                                            // 下次启动也会继续校正，而不是悄悄退回关闭。
                                            apply_agent_host(|c| {
                                                c.agent_hooks_enabled = true;
                                            }, cx);
                                            let result = install_agent_hooks();
                                            invalidate_hooks_cache();
                                            match result {
                                                Ok(()) => {
                                                    crate::status_item::notify_success("Agent hooks 已安装");
                                                    cx.refresh_windows();
                                                }
                                                Err(e) => {
                                                    eprintln!("[workspace] 安装 hooks 失败：{e}");
                                                    crate::status_item::notify_error(format!("安装失败：{e}"));
                                                    cx.refresh_windows();
                                                }
                                            }
                                        }),
                                )
                                .child(
                                    div()
                                        .id("uninstall-agent-hooks")
                                        .px_3()
                                        .py(px(6.))
                                        .rounded_md()
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(border)
                                        .text_sm()
                                        .text_color(fg)
                                        .hover(|s| s.bg(border))
                                        .child("移除 Smelt hooks")
                                        .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                                            // 先关闭自动安装，避免部分移除失败后重启又被装回。
                                            apply_agent_host(|c| {
                                                c.agent_hooks_enabled = false;
                                            }, cx);
                                            let result = uninstall_agent_hooks();
                                            invalidate_hooks_cache();
                                            match result {
                                                Ok(()) => {
                                                    crate::status_item::notify_success("Smelt hooks 已移除");
                                                    cx.refresh_windows();
                                                }
                                                Err(e) => {
                                                    eprintln!("[workspace] 移除 hooks 失败：{e}");
                                                    crate::status_item::notify_error(format!("移除失败：{e}"));
                                                    cx.refresh_windows();
                                                }
                                            }
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(
                                    "分别写入 Claude、Copilot、Codex、Antigravity、Cursor 的用户级 hooks，\
                                     OpenCode 的全局插件、~/.grok/hooks/smelt-notifications.json，以及 \
                                     ~/.kiro/hooks/smelt-notifications.json。Grok TUI 读自己的 hooks 目录，\
                                     不只读 Claude settings；Kiro 全局 hooks 需要 CLI v3 / IDE 1.0+。只增删 \
                                     Smelt 条目或 Smelt 管理的插件文件；hooks 只观察进度和完成态，不介入工具\
                                     权限决策，重开会话后生效。",
                                ),
                        )
                        .into_any_element()
                })),
            ]
}
