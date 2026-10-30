//! 设置页：Agent。本机 Agent（CLI 和 Hooks）、新建菜单（「+」菜单项和额外 workspace）、
//! 高级（对话适配器命令，默认折叠）。

use super::*;
use smelt_ui::IconAction as _;

pub(super) fn agents_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let items = agent_groups(entity.clone(), snapshot, cx);
    SettingPage::new("Agent")
        .group(
            SettingGroup::new()
                .title("本机 Agent")
                .item(items.runtime)
                .item(items.hooks),
        )
        .group(
            SettingGroup::new()
                .title("新建菜单")
                .item(items.launch)
                .item(items.profiles),
        )
        .group(advanced_group(entity, snapshot.acp_commands_open, cx))
}

/// 对话适配器命令：几乎没人改，默认折起来只露一行摘要。每家一项的说明都一样，写在标题行
/// 一次就够。
fn advanced_group(entity: Entity<Workspace>, open: bool, cx: &App) -> SettingGroup {
    let changed = ConversationAgentKind::ALL
        .into_iter()
        .filter(|agent| acp_cmd_for(&crate::RuntimeKey::Local, *agent, cx) != agent.default_cmd())
        .count();
    let summary = match changed {
        0 => format!("{} 家都用内置", ConversationAgentKind::ALL.len()),
        n => format!("{n} 家改过"),
    };
    let header = SettingItem::new(
        "对话适配器命令",
        SettingField::render(move |_, _, cx: &mut App| {
            let entity = entity.clone();
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(summary.clone()),
                )
                .child(
                    Button::new("toggle-acp-commands")
                        .quiet()
                        .compact()
                        .icon(if open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .label(if open { "收起" } else { "展开" })
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |ws, cx| {
                                ws.settings_acp_commands_open = !ws.settings_acp_commands_open;
                                cx.notify();
                            });
                        }),
                )
        }),
    )
    .description("替换适配器或追加参数时才需要改，留空用内置；只影响之后新建的对话。")
    .keywords(["acp", "适配器", "自定义启动命令", "对话命令"]);
    let group = SettingGroup::new().title("高级").item(header);
    if open {
        group.items(
            ConversationAgentKind::ALL
                .into_iter()
                .map(acp_cmd_setting_item),
        )
    } else {
        group
    }
}

/// Agent 页的四块内容，由 `agents_page` 按「本机 Agent / 新建菜单」组装。
struct AgentSettingItems {
    runtime: SettingItem,
    hooks: SettingItem,
    launch: SettingItem,
    profiles: SettingItem,
}

fn agent_groups(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    _cx: &App,
) -> AgentSettingItems {
    // —— 启动：项目「+」下拉菜单的可配置启动项 ——
    // Settings 的 list 测量项高度时，百分比宽度（w_full）经常解析不到确定父宽，
    // 卡片会缩成「内容宽」——输入框只露出几个字。这里用窗口视口算绝对像素宽。
    let launch_editor_entity = entity.clone();
    let launch_rows = snapshot.launch_rows.clone();
    let launch_item = SettingItem::render(move |_, window, cx: &mut App| {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let fg = cx.theme().foreground;
        let secondary = cx.theme().secondary;
        // 侧栏默认 250 + 左右 padding/滚动条余量；再夹到合理区间。
        let field_w = {
            let vw = f32::from(window.viewport_size().width);
            let w = (vw - 250. - 80.).clamp(360., 720.);
            px(w)
        };
        let mut col = v_flex().w(field_w).gap_3().child(
            v_flex()
                .w(field_w)
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(fg)
                        .child("项目「+」菜单"),
                )
                .child(
                    div()
                        .w(field_w)
                        .text_sm()
                        .text_color(muted)
                        .child("命令在项目目录下执行，可带参数；内置项不能删。"),
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
            let row_key = label.entity_id();
            let delete_slot = if *removable {
                Button::new(("del-launch", row_key))
                    .quiet()
                    .compact()
                    .icon(IconName::Close)
                    .icon_action("删除启动项")
                    .w(del_w)
                    .flex_none()
                    .on_click(move |_, _, cx: &mut App| {
                        del_entity.update(cx, |ws, cx| {
                            ws.remove_launch_entry(row_ix, cx);
                        });
                    })
                    .into_any_element()
            } else {
                // 内置启动项固定存在；保留空槽以维持两列对齐。
                div().size(del_w).flex_none().into_any_element()
            };
            list = list.child(
                h_flex()
                                        .id(("launch-row", row_key))
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
            Button::new("add-launch")
                .outline()
                .icon(IconName::Plus)
                .label("添加启动项")
                .w(field_w)
                .on_click(move |_, _, cx: &mut App| {
                    add_entity.update(cx, |ws, cx| ws.add_launch_entry(cx));
                }),
        )
        .into_any_element()
    })
    .keywords(["快捷启动", "launch", "命令", "claude", "codex", "copilot"]);

    AgentSettingItems {
        runtime: SettingItem::render({
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
                    .quiet()
                    .compact()
                    .icon(gpui_component::Icon::new(
                        gpui_kit_assets::IconName::RefreshCw,
                    ))
                    .loading_icon(IconName::LoaderCircle)
                    .loading(runtime.refreshing)
                    .disabled(runtime.refreshing || runtime.installing.is_some())
                    .icon_action("刷新运行环境检测")
                    .on_click(move |_, _, cx| {
                        refresh_entity.update(cx, |workspace, cx| {
                            workspace.refresh_acp_runtime(cx);
                        });
                    });

                let mut content = v_flex().w_full().gap_3().child(
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

                let runtime_row =
                    |agent: ConversationAgentKind,
                     executable: &smelt_model::acp_conn::RuntimeExecutable| {
                        let (status, color): (String, Hsla) = if !executable.is_available() {
                            ("未安装".to_string(), rgb(crate::ui_theme::red()).into())
                        } else if let Some(error) = &executable.error {
                            (
                                format!("检测失败：{error}"),
                                rgb(crate::ui_theme::yellow()).into(),
                            )
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
                                .normal()
                                .compact()
                                .icon(IconName::Plus)
                                .label(if installing_this {
                                    "安装中…"
                                } else {
                                    "安装"
                                })
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
                                        div().truncate().text_xs().text_color(color).child(status),
                                    )
                                    .child(
                                        div().truncate().text_xs().text_color(muted).child(path),
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
                for agent in ConversationAgentKind::ALL {
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
        ]),
        launch: launch_item,
        profiles: SettingItem::render({
            let profile_editor_entity = entity;
            let profile_rows = snapshot.profile_rows.clone();
            move |_, window, cx: &mut App| {
                let muted = cx.theme().muted_foreground;
                let border = cx.theme().border;
                let fg = cx.theme().foreground;
                let secondary = cx.theme().secondary;
                let field_w = {
                    let vw = f32::from(window.viewport_size().width);
                    let w = (vw - 250. - 80.).clamp(360., 720.);
                    px(w)
                };
                let mut col = v_flex().w(field_w).gap_3().child(
                    v_flex()
                        .w(field_w)
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .text_color(fg)
                                .child("「新建对话」菜单里的额外 workspace"),
                        )
                        .child(div().w(field_w).text_sm().text_color(muted).child(
                            "同一家 Agent 再开一个配置目录（如 ~/.claude-quant），\
                                                     「新建对话」菜单和历史会话页都会多一项。",
                        )),
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

                let profiles = agent_host_settings(&crate::RuntimeKey::Local, cx)
                    .profiles
                    .clone();
                for (ix, ((label, dir), p)) in profile_rows.iter().zip(profiles.iter()).enumerate()
                {
                    let row_ix = ix;
                    let row_key = label.entity_id();
                    let kind_entity = profile_editor_entity.clone();
                    let del_entity = profile_editor_entity.clone();
                    let current_kind_label = p
                        .kind()
                        .map(|kind| kind.short_label().to_string())
                        .unwrap_or_else(|| format!("未知 ({})", p.kind_id));
                    list = list.child(
                        h_flex()
                            .id(("profile-row", row_key))
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new(("profile-kind", row_key))
                                    .quiet()
                                    .compact()
                                    .w(kind_w)
                                    .label(current_kind_label)
                                    .dropdown_menu(move |mut menu, _window, _cx| {
                                        for kind in ConversationAgentKind::ALL {
                                            let kind_entity = kind_entity.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(kind.label()).on_click(
                                                    move |_ev, _window, cx| {
                                                        kind_entity.update(cx, |ws, cx| {
                                                            ws.set_profile_kind(row_ix, kind, cx);
                                                        });
                                                    },
                                                ),
                                            );
                                        }
                                        menu
                                    }),
                            )
                            .child(Input::new(label).w(name_w))
                            .child(Input::new(dir).w(dir_w).font_family(mono.clone()))
                            .child(
                                Button::new(("del-profile", row_key))
                                    .quiet()
                                    .compact()
                                    .icon(IconName::Close)
                                    .icon_action("删除 workspace")
                                    .w(del_w)
                                    .flex_none()
                                    .on_click(move |_, _, cx: &mut App| {
                                        del_entity.update(cx, |ws, cx| {
                                            ws.remove_profile(row_ix, cx);
                                        });
                                    }),
                            ),
                    );
                }
                col = col.child(list);
                let add_entity = profile_editor_entity.clone();
                col.child(
                    Button::new("add-profile")
                        .outline()
                        .icon(IconName::Plus)
                        .label("添加 workspace")
                        .w(field_w)
                        .on_click(move |_, _, cx: &mut App| {
                            add_entity.update(cx, |ws, cx| ws.add_profile(cx));
                        }),
                )
                .into_any_element()
            }
        })
        .keywords([
            "workspace",
            "claude-quant",
            "config dir",
            "多工作区",
            "agent",
        ]),
        hooks: SettingItem::render(move |_, _, cx: &mut App| {
            // 状态由运行时检测，这里只读设置窗口打开时取回的缓存。
            let (hook_status, notify_path, busy) = cx
                .try_global::<AgentHooksCache>()
                .map(|cache| {
                    (
                        cache.status.as_ref().map(|s| s.providers.clone()),
                        cache.status.as_ref().map(|s| s.notify_path.clone()),
                        cache.busy,
                    )
                })
                .unwrap_or_default();
            let installed = hook_status
                .as_ref()
                .is_some_and(|providers| providers.iter().all(|provider| provider.installed));
            let muted = cx.theme().muted_foreground;
            let success: Hsla = rgb(crate::ui_theme::green()).into();
            // 接入项照运行时报的原样列：它们不是一家 Agent 一项（「Claude / Grok」共用
            // 一份配置，Pi 没有 hooks），硬对到上面的 CLI 表反而对不齐。
            let providers =
                match hook_status {
                    Some(providers) => h_flex()
                        .gap_3()
                        .flex_wrap()
                        .children(providers.into_iter().map(|provider| {
                            let color = if provider.installed { success } else { muted };
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(div().size(px(6.)).rounded_full().bg(color))
                                .child(div().text_xs().child(provider.label))
                                .child(div().text_xs().text_color(muted).child(
                                    if provider.installed {
                                        "已接入"
                                    } else {
                                        "未接入"
                                    },
                                ))
                        }))
                        .into_any_element(),
                    None => div()
                        .text_xs()
                        .text_color(muted)
                        .child("正在读取接入状态…")
                        .into_any_element(),
                };
            v_flex()
                .w_full()
                .gap_2()
                .pt_3()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_start()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Hooks"))
                                .child(div().text_xs().text_color(muted).child(
                                    "把 Smelt 的通知接进各家 Agent 的用户级配置，只增删 Smelt \
                                             自己的条目；不介入工具权限决策，重开会话后生效。",
                                )),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .child(
                                    Button::new("uninstall-agent-hooks")
                                        .quiet()
                                        .compact()
                                        .label("移除")
                                        .disabled(busy)
                                        .on_click(move |_, _window, cx: &mut App| {
                                            // 先关闭自动安装，避免部分移除失败后重启又被装回。
                                            apply_agent_host(
                                                &crate::RuntimeKey::Local,
                                                |c| c.hooks_enabled = false,
                                                cx,
                                            );
                                            uninstall_agent_hooks(cx);
                                        }),
                                )
                                .child(
                                    Button::new("install-agent-hooks")
                                        .compact()
                                        .label(if installed {
                                            "重新安装"
                                        } else {
                                            "安装 hooks"
                                        })
                                        .disabled(busy)
                                        .on_click(move |_, _window, cx: &mut App| {
                                            // 先记用户意图：即使某个 provider 本次安装失败，
                                            // 下次启动也会继续校正，而不是悄悄退回关闭。
                                            apply_agent_host(
                                                &crate::RuntimeKey::Local,
                                                |c| c.hooks_enabled = true,
                                                cx,
                                            );
                                            install_agent_hooks(cx);
                                        }),
                                ),
                        ),
                )
                .child(providers)
                .when_some(notify_path, |this, path| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("smelt-notify：{path}")),
                    )
                })
                .into_any_element()
        })
        .keywords(["hook", "hooks", "通知接入", "配置文件"]),
    }
}
