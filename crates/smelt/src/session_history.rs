//! 历史会话的 GPUI 状态、渲染与 Workspace 集成。各家 agent 的数据模型与
//! 解析器位于 `smelt_core::session_history`，这里 re-export 保持消费者兼容。

pub use smelt_core::session_history::*;

use smelt_core::fs::{FileSystem, LocalFs};
use std::path::{Path, PathBuf};

// ===================== GPUI 面板 =====================
//
// 以上是纯逻辑（无 GPUI 依赖，好单测）；以下是从 main.rs 拆过来的面板部分——
// `impl Workspace` 方法 + 渲染函数，字段仍然声明在 main.rs 的 `Workspace` struct 里。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use crate::acp_view;
use crate::{Workspace, placeholder_view, ui_theme};

pub(crate) fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 历史会话「时间」文案：有明显跨度（>1 分钟）就顺带标一下这个会话跑了多久，
/// 纯单条消息的会话就只显示时间点，不必画蛇添足展示"0 分钟"。
fn session_when(s: &SessionSummary) -> String {
    match (s.started_at, s.last_active_at) {
        (Some(start), Some(last)) if (last - start).num_minutes() >= 1 => format!(
            "{} · 跑了 {} 分钟",
            last.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
            (last - start).num_minutes()
        ),
        (_, Some(last)) => last
            .with_timezone(&chrono::Local)
            .format("%m-%d %H:%M")
            .to_string(),
        _ => String::new(),
    }
}

/// 历史会话列表状态：未选项目 / 还没扫描完 / 扫描完但没有历史会话 / 拿到数据。
/// 右侧使用可搜索的双层标题列表（不再用 DataTable——四个数据列挤在一栏很局促）：
/// 主行优先显示用户名称，副行保留 Agent 原始标题或时间信息。
pub enum HistoryListState {
    NoProject,
    Loading,
    Empty,
    Ready(Rc<Vec<SessionSummary>>),
}

/// 历史会话右键「删除」的确认目标。
#[derive(Clone)]
pub struct DeleteHistoryTarget {
    pub agent: HistorySourceKind,
    pub profile_id: Option<String>,
    pub cwd: String,
    pub resume_id: String,
    pub path: PathBuf,
    pub title: String,
}

/// 历史会话页：列出当前项目下各家 agent 保存的历史会话，并显示选中会话的
/// 对话内容（只读浏览，支持右键「继续」恢复该对话）。
/// 历史页顶部一个来源 tab：基础 agent 槽或手动 workspace profile。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistorySourceTab {
    pub kind: HistorySourceKind,
    pub profile_id: Option<String>,
    pub label: String,
}

/// 历史页该展示哪些来源 tab。智能体上下文只留当前引擎；普通项目列出所有裸种类
/// 再加上手动 profile。纯数据，不碰 GPUI。
///
/// 遍历 [`HistorySourceKind::ALL`] 而不是 `ConversationAgentKind::ALL`：能不能读历史
/// 取决于本机有没有落盘 transcript，与有没有 ACP 无关。
pub(crate) fn history_source_tabs(
    restrict_to_kind: Option<ConversationAgentKind>,
    profiles: impl IntoIterator<Item = smelt_core::agent_kind::AcpProfile>,
) -> Vec<HistorySourceTab> {
    let mut tabs = Vec::new();
    for kind in HistorySourceKind::ALL {
        if !kind.is_bare_kind() {
            continue;
        }
        // 智能体上下文只允许当前引擎；引擎总是一个 ACP 种类，所以纯终端
        // 来源在这种上下文里天然不匹配。
        if restrict_to_kind.is_none_or(|only| kind.acp() == Some(only)) {
            tabs.push(HistorySourceTab {
                kind,
                profile_id: None,
                label: kind.short_label().to_string(),
            });
        }
    }
    if restrict_to_kind.is_none() {
        for profile in profiles {
            let Some(kind) = profile.kind() else {
                continue;
            };
            tabs.push(HistorySourceTab {
                kind: kind.into(),
                profile_id: Some(profile.id.clone()),
                label: profile.label,
            });
        }
    }
    tabs
}

pub(crate) struct HistoryViewParams<'a> {
    pub agent: HistorySourceKind,
    /// 手动添加的 workspace profile；基础 agent 槽位为 `None`。
    pub profile_id: Option<String>,
    pub cwd: Option<String>,
    pub list: HistoryListState,
    pub detail: &'a Option<(PathBuf, Rc<SessionDetail>)>,
    pub detail_list_state: ListState,
    pub filter: Option<Entity<gpui_component::input::InputState>>,
    /// 智能体上下文里只允许一个引擎。`None` 表示普通项目。
    pub restrict_to_kind: Option<ConversationAgentKind>,
    /// 顶部来源 tab，由 [`history_source_tabs`] 在 Workspace 里算好。
    pub source_tabs: Vec<HistorySourceTab>,
    /// 当前 tab 若是手动 profile，续接要用它的启动规格。
    pub launch_override: Option<smelt_core::agent_kind::ConversationLaunchSpec>,
    /// 当前 tab 的 profile 显示名；迁移文案用。
    pub profile_label: Option<String>,
    /// `resume_id` → 产品智能体定义 id。项目历史里靠它认出「这不是裸引擎会话」。
    pub history_agent_ids: Rc<HashMap<String, String>>,
}

/// 智能体 space 里的历史，或项目里绑过智能体定义的那几条，都按智能体续接。
pub(crate) fn history_row_is_agent_session(
    agent_context: bool,
    bound_agent_definition_id: Option<&str>,
) -> bool {
    agent_context || bound_agent_definition_id.is_some()
}

pub(crate) fn history_continue_label(is_agent_session: bool) -> &'static str {
    if is_agent_session {
        "继续对话"
    } else {
        "ACP 继续"
    }
}

pub fn history_view(params: HistoryViewParams<'_>, cx: &mut Context<Workspace>) -> Div {
    let HistoryViewParams {
        agent,
        profile_id,
        cwd,
        list,
        detail,
        detail_list_state,
        filter,
        restrict_to_kind,
        source_tabs,
        launch_override,
        profile_label,
        history_agent_ids,
    } = params;
    // 智能体上下文：只留「继续对话」这一条按智能体续接的路径。
    let agent_context = restrict_to_kind.is_some();
    let (muted, fg, c_border, accent, secondary) = {
        let t = cx.theme();
        (
            t.muted_foreground,
            t.foreground,
            t.border,
            t.primary,
            t.secondary,
        )
    };

    let current_profile = profile_id.clone();
    let tab_colors = HistoryTabColors { accent, fg, muted };
    let agent_switcher = h_flex()
        .id("history-agent-switcher")
        .w_full()
        .h(px(38.))
        .min_w_0()
        .flex_none()
        .overflow_x_scrollbar()
        .gap_1()
        .px_3()
        .py_1p5()
        .border_b_1()
        .border_color(c_border)
        // 空间足够时把来源 tab 推到右侧；内容超出时占位区收缩为零，保留滚动。
        .child(div().flex_1().min_w_0())
        .children(source_tabs.into_iter().map(|tab| {
            agent_tab_button(
                tab.kind,
                tab.profile_id,
                tab.label.into(),
                agent,
                current_profile.clone(),
                tab_colors,
                cx,
            )
        }));

    // 选中会话的路径：list 和 detail 各自渲染都要用它判断"这行是不是当前打开的"，
    // 先从 detail 里取出来，避免下面重复解构。
    // 只有 Ready 时才有数据可查；detail 头部那行摘要信息（时间/消息数/tokens）
    // 就是从这份列表里按路径找回对应的 SessionSummary，不用另存一份。
    let sessions: Option<Rc<Vec<SessionSummary>>> = match &list {
        HistoryListState::Ready(s) => Some(s.clone()),
        _ => None,
    };
    let detail = detail.as_ref().filter(|(path, _)| {
        sessions
            .as_ref()
            .is_some_and(|list| list.iter().any(|session| session.path == *path))
    });
    let selected_path = detail.map(|(path, _)| path.clone());
    let query = filter
        .as_ref()
        .map(|input| input.read(cx).value().trim().to_lowercase())
        .unwrap_or_default();

    let list_body: AnyElement = match (&list, &sessions) {
        (HistoryListState::NoProject, _) => {
            placeholder_view("当前没有活动项目", muted).into_any_element()
        }
        (HistoryListState::Loading, _) => placeholder_view("加载中…", muted).into_any_element(),
        (HistoryListState::Empty, _) | (_, None) => {
            placeholder_view("这个项目还没有本地保存的历史会话", muted).into_any_element()
        }
        (HistoryListState::Ready(_), Some(list)) => {
            let visible_indices = Rc::new(
                list.iter()
                    .enumerate()
                    .filter_map(|(ix, session)| {
                        (query.is_empty()
                            || session.title.to_lowercase().contains(&query)
                            || session.agent_title.to_lowercase().contains(&query))
                        .then_some(ix)
                    })
                    .collect::<Vec<_>>(),
            );
            if visible_indices.is_empty() {
                placeholder_view("没有匹配的历史会话", muted).into_any_element()
            } else {
                // 虚拟列表的渲染回调发生在 Workspace::render 已经持有实体租约时，
                // 这个来源能干什么，由它自身能力决定，不是所有来源都一样：
                // - ACP 续接 / 迁移：需要 ACP 身份，纯终端来源（Antigravity）没有。
                //   迁移虽然只要读得出 transcript，但 `HistoryMigrationSource` 要拿源
                //   agent 写进交接头部，而那块目前只认 ACP 种类。
                // - 删除：存档形态各家不同。Antigravity 的历史同时活在总索引库和
                //   正文库两处，而且 `agy` 可能正开着它们；只删一半会把别人的索引
                //   弄成死链，所以不提供删除，而不是提供一个半对的删除。
                let migrate_agent = agent.acp();
                let supports_delete = agent.acp().is_some();
                // 这里只能根据快照构造元素，不能为了拿 Context 再 update Workspace。
                // 交互回调真正发生在之后，再通过 Entity 更新工作区。
                let workspace = cx.entity();
                let list = list.clone();
                let selected_path_for_list = selected_path.clone();
                let history_agent_ids = history_agent_ids.clone();
                let row_count = visible_indices.len();
                uniform_list("session-list", row_count, move |range, _window, _app| {
                    range
                        .map(|row_ix| {
                            let ix = visible_indices[row_ix];
                            let s = &list[ix];
                            let is_sel =
                                selected_path_for_list.as_deref() == Some(s.path.as_path());
                            let path = s.path.clone();
                            let path_for_copy = path.to_string_lossy().into_owned();
                            let resume_id = s.resume_id.clone();
                            let row_is_agent = history_row_is_agent_session(
                                agent_context,
                                history_agent_ids.get(&s.resume_id).map(String::as_str),
                            );
                            let row_cwd = cwd.clone();
                            let ws_for_resume = workspace.clone();
                            let row_launch_override = launch_override.clone();
                            let row_profile_id = profile_id.clone();
                            let rename_cwd = cwd.clone();
                            let rename_resume_id = s.resume_id.clone();
                            let rename_title = s.title.clone();
                            let delete_target = cwd.clone().filter(|_| supports_delete).map(|cwd| {
                                DeleteHistoryTarget {
                                    agent,
                                    profile_id: profile_id.clone(),
                                    cwd,
                                    resume_id: s.resume_id.clone(),
                                    path: s.path.clone(),
                                    title: s.title.clone(),
                                }
                            });
                            let migrate_source =
                                cwd.clone()
                                    .zip(migrate_agent)
                                    .map(|(cwd, agent)| HistoryMigrationSource {
                                        agent,
                                        profile_label: profile_label.clone(),
                                        title: s.title.clone(),
                                        resume_id: s.resume_id.clone(),
                                        path: s.path.clone(),
                                        cwd,
                                    });
                            let has_custom_title = s.custom_title.is_some();
                            let secondary_title = if s.custom_title.is_some() {
                                let when = session_when(s);
                                if when.is_empty() {
                                    s.agent_title.clone()
                                } else {
                                    format!("{} · {when}", s.agent_title)
                                }
                            } else {
                                session_when(s)
                            };
                            let ws_for_detail = workspace.clone();
                            let detail_path = path;
                            div()
                                .w_full()
                                .px_2()
                                .pb_1()
                                .child(
                                    v_flex()
                                        .id(("session-row", ix))
                                        .w_full()
                                        .h(px(54.))
                                        .justify_center()
                                        .gap_0p5()
                                        .px_2()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_color(fg)
                                        .when(is_sel, |d| d.bg(accent.opacity(0.18)))
                                        .when(!is_sel, |d| d.hover(|s| s.bg(c_border.opacity(0.5))))
                                        .child(
                                            div()
                                                .w_full()
                                                .text_sm()
                                                .truncate()
                                                .child(s.title.clone()),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .text_xs()
                                                .text_color(muted)
                                                .truncate()
                                                .child(secondary_title),
                                        )
                                        .on_mouse_down(MouseButton::Left, move |_, _, app| {
                                            ws_for_detail.update(app, |this, cx| {
                                                this.open_session_detail(
                                                    agent,
                                                    detail_path.clone(),
                                                    cx,
                                                );
                                            });
                                        })
                                        .context_menu(move |mut menu, _window, menu_cx| {
                                            let ws = ws_for_resume.clone();
                                            let resume_id = resume_id.clone();
                                            let row_cwd = row_cwd.clone();
                                            let row_launch_override = row_launch_override.clone();
                                            let row_profile_id = row_profile_id.clone();
                                            let acp_profile_id = row_profile_id.clone();
                                            let acp_resume_id = resume_id.clone();
                                            let acp_row_cwd = row_cwd.clone();
                                            let acp_launch_override = row_launch_override.clone();
                                            let acp_agent = agent.acp();
                                            // ACP 续接要求对方有 ACP 服务。纯终端来源只能
                                            // 走下面的 CLI/TUI 续接，这里不能给一个点了没反应的菜单项。
                                            if let Some(acp_agent) = acp_agent {
                                                menu =
                                                    menu.item(PopupMenuItem::new(
                                                        history_continue_label(row_is_agent),
                                                    )
                                                    .on_click(
                                                        move |_ev, window, cx| {
                                                            // 没选中项目时历史页本来就是空的，理论到不了这里，
                                                            // 防御性地什么都不做而不是 panic。
                                                            let Some(cwd) = acp_row_cwd.clone()
                                                            else {
                                                                return;
                                                            };
                                                            let resume_id = acp_resume_id.clone();
                                                            let launch_override =
                                                                acp_launch_override.clone();
                                                            let profile_id = acp_profile_id.clone();
                                                            ws.update(cx, |this, cx| {
                                                                this.resume_acp_session(
                                                                crate::workspace_sessions::AcpResumeRequest {
                                                                    agent: acp_agent,
                                                                    launch_override,
                                                                    profile_id,
                                                                    cwd,
                                                                    resume_id,
                                                                },
                                                                window,
                                                                cx,
                                                            );
                                                            });
                                                        },
                                                    ));
                                            }
                                            let ws = ws_for_resume.clone();
                                            let cli_resume_id = resume_id;
                                            let cli_row_cwd = row_cwd;
                                            let cli_launch_override = row_launch_override;
                                            // 智能体会话不给裸引擎的入口：CLI/TUI
                                            // 续接会绕开智能体直接起一个 TUI，插件、
                                            // 工作方式、绑定上下文全都不会带上。
                                            if !row_is_agent {
                                                menu = menu.item(
                                                    PopupMenuItem::new("CLI/TUI 继续").on_click(
                                                        move |_ev, _window, cx| {
                                                            let Some(cwd) = cli_row_cwd.clone()
                                                            else {
                                                                return;
                                                            };
                                                            let resume_id = cli_resume_id.clone();
                                                            let launch_override =
                                                                cli_launch_override.clone();
                                                            ws.update(cx, |this, cx| {
                                                                this.resume_cli_session(
                                                                    agent,
                                                                    launch_override,
                                                                    cwd,
                                                                    resume_id,
                                                                    cx,
                                                                );
                                                            });
                                                        },
                                                    ),
                                                );
                                            }
                                            // 「继续」两项是同一家 agent 的续接
                                            // （靠 session/load 或 --resume）；迁移
                                            // 是把内容重述给另一家，走的是完全不同
                                            // 的路径，所以分组分开。
                                            if let Some(source) =
                                                migrate_source.clone().filter(|_| !row_is_agent)
                                            {
                                                menu = migration_menu(
                                                    menu,
                                                    &ws_for_resume,
                                                    source,
                                                    row_profile_id.as_deref(),
                                                    menu_cx,
                                                );
                                            }
                                            if let Some(rename_cwd) = rename_cwd.clone() {
                                                let ws = ws_for_resume.clone();
                                                let rename_profile_id = row_profile_id.clone();
                                                let rename_resume_id = rename_resume_id.clone();
                                                let rename_title = rename_title.clone();
                                                let reset_cwd = rename_cwd.clone();
                                                let reset_resume_id = rename_resume_id.clone();
                                                menu = menu.separator().item(
                                                    PopupMenuItem::new("重命名").on_click(
                                                        move |_ev, window, cx| {
                                                            let profile_id =
                                                                rename_profile_id.clone();
                                                            let cwd = rename_cwd.clone();
                                                            let resume_id =
                                                                rename_resume_id.clone();
                                                            let current_title =
                                                                rename_title.clone();
                                                            ws.update(cx, |this, cx| {
                                                                this.start_rename(
                                                                    crate::RenameTarget::History {
                                                                        agent,
                                                                        profile_id,
                                                                        cwd,
                                                                        resume_id,
                                                                        current_title,
                                                                    },
                                                                    window,
                                                                    cx,
                                                                );
                                                            });
                                                        },
                                                    ),
                                                );
                                                if has_custom_title {
                                                    let ws = ws_for_resume.clone();
                                                    let reset_profile_id = row_profile_id;
                                                    menu = menu.item(
                                                        PopupMenuItem::new("恢复默认名称")
                                                            .on_click(move |_ev, _window, cx| {
                                                                ws.update(cx, |this, cx| {
                                                                    this.set_history_custom_title(
                                                                        agent,
                                                                        reset_profile_id.clone(),
                                                                        reset_cwd.clone(),
                                                                        reset_resume_id.clone(),
                                                                        None,
                                                                        cx,
                                                                    );
                                                                });
                                                            }),
                                                    );
                                                }
                                            }
                                            if let Some(target) = delete_target.clone() {
                                                let ws = ws_for_resume.clone();
                                                menu = menu.separator().item(
                                                    PopupMenuItem::new("删除").on_click(
                                                        move |_ev, _window, cx| {
                                                            ws.update(cx, |this, cx| {
                                                                this.start_delete_history(
                                                                    target.clone(),
                                                                    cx,
                                                                );
                                                            });
                                                        },
                                                    ),
                                                );
                                            }
                                            let path = path_for_copy.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new("复制文件路径").on_click(
                                                    move |_ev, _window, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(path.clone()),
                                                        );
                                                    },
                                                ),
                                            );
                                            menu
                                        }),
                                )
                                .into_any_element()
                        })
                        .collect()
                })
                .flex_1()
                .min_h_0()
                .pt_2()
                .into_any_element()
            }
        }
    };

    // 详情头部：选中会话的时间/消息数/tokens，固定在对话内容上方——之前这些
    // 信息只在左边表格的列里能看到，挪掉表格之后得有地方接住。
    let detail_header = selected_path
        .as_ref()
        .and_then(|p| {
            sessions
                .as_ref()
                .and_then(|list| list.iter().find(|s| &s.path == p))
        })
        .map(|s| {
            h_flex()
                .flex_none()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(c_border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_semibold()
                        .text_color(fg)
                        .truncate()
                        .child(s.title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(session_when(s)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{} 条消息", s.message_count)),
                )
                .when(s.total_tokens > 0, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(muted)
                            .child(format_count(s.total_tokens)),
                    )
                })
        });

    let turns_body: AnyElement = match detail {
        None => placeholder_view("← 选择一个历史会话查看内容", muted).into_any_element(),
        Some((_, d)) if d.turns.is_empty() => {
            placeholder_view("这份会话没有可展示的对话内容", muted).into_any_element()
        }
        Some((_, d)) => {
            let detail = d.clone();
            let agent_label = agent.short_label();
            gpui::list(detail_list_state, move |i, _window, _app| {
                let turn = &detail.turns[i];
                div()
                    .w_full()
                    .px_3()
                    .pb_3()
                    .child(history_turn(
                        turn,
                        i,
                        agent_label,
                        muted,
                        fg,
                        accent,
                        secondary,
                    ))
                    .into_any_element()
            })
            .w_full()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .pt_3()
            .into_any_element()
        }
    };

    let detail_body = v_flex()
        .flex_1()
        .h_full()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .children(detail_header)
        .child(turns_body);

    let list_panel = div()
        .flex()
        .flex_col()
        .h_full()
        .min_h_0()
        .min_w_0()
        // 历史和 Files/Git 一样是「列表 + 详情」的左右 Tool Panel；列表宽度稳定，
        // 详情占用剩余空间，不随停靠/全屏状态改变布局方向。用相对宽度配合上下限，
        // 避免窄停靠面板被固定宽度吃满，也避免全屏时列表窄得不可用。
        .w(relative(0.38))
        .min_w(px(crate::tool_panel::MIN_FILE_TREE_WIDTH))
        .max_w(px(crate::tool_panel::MAX_FILE_TREE_WIDTH))
        .flex_none()
        .border_l_1()
        .border_color(c_border)
        .overflow_hidden()
        .children(filter.as_ref().map(|input| {
            div()
                .flex_none()
                .w_full()
                .px_2()
                .pt_2()
                .child(Input::new(input).small().cleanable(true))
        }))
        .child(list_body);

    let content = div()
        .flex()
        .flex_row()
        .flex_1()
        .h_full()
        .min_h_0()
        .min_w_0()
        .items_stretch()
        .overflow_hidden()
        .child(detail_body)
        .child(list_panel);

    v_flex()
        .flex_1()
        .h_full()
        .min_h_0()
        .min_w_0()
        .child(agent_switcher)
        .child(content)
}

fn history_turn(
    turn: &Turn,
    index: usize,
    agent_label: &'static str,
    muted: Hsla,
    fg: Hsla,
    accent: Hsla,
    secondary: Hsla,
) -> AnyElement {
    let role = if turn.is_user { "用户" } else { agent_label };
    let role_color = if turn.is_user { accent } else { fg };
    let bubble_bg = if turn.is_user {
        accent.opacity(0.12)
    } else {
        secondary
    };
    let tool_summary = (!turn.tools.is_empty()).then(|| {
        let mut order: Vec<&String> = Vec::new();
        let mut counts: HashMap<&String, usize> = HashMap::new();
        for tool in &turn.tools {
            counts
                .entry(tool)
                .and_modify(|count| *count += 1)
                .or_insert_with(|| {
                    order.push(tool);
                    1
                });
        }
        order
            .into_iter()
            .map(|name| match counts[name] {
                count if count > 1 => format!("{name} ×{count}"),
                _ => name.clone(),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    });

    v_flex()
        .w_full()
        .min_w_0()
        .gap_1()
        .px_3()
        .py_2()
        .rounded(ui_theme::card_radius())
        .bg(bubble_bg)
        .when(turn.is_user, |element| element.max_w(px(560.)))
        .child(
            h_flex()
                .gap_2()
                .items_baseline()
                .child(
                    div()
                        .font_semibold()
                        .text_sm()
                        .text_color(role_color)
                        .child(role),
                )
                .children(turn.timestamp.map(|timestamp| {
                    div().text_xs().text_color(muted).child(
                        timestamp
                            .with_timezone(&chrono::Local)
                            .format("%m-%d %H:%M")
                            .to_string(),
                    )
                })),
        )
        .child(div().w_full().min_w_0().text_sm().text_color(fg).child(
            crate::markdown_mermaid::markdown_view(("turn-md", index), turn.text.clone()),
        ))
        .children(tool_summary.map(|summary| {
            div()
                .text_xs()
                .text_color(muted)
                .child(format!("🔧 {summary}"))
        }))
        .into_any_element()
}

/// 会话来源 tab 上的一个按钮，选中态用 accent 底色标出来。
/// 视觉，但换 agent 时还要顺带清掉右侧详情——不然会显示"上一个 agent 那份会话"
/// 的残留内容，跟点开新会话前那一瞬间的空白状态不一致）。
#[derive(Clone, Copy)]
struct HistoryTabColors {
    accent: Hsla,
    fg: Hsla,
    muted: Hsla,
}

fn agent_tab_button(
    target: HistorySourceKind,
    target_profile: Option<String>,
    label: SharedString,
    current: HistorySourceKind,
    current_profile: Option<String>,
    colors: HistoryTabColors,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    let HistoryTabColors { accent, fg, muted } = colors;
    let selected = target == current && target_profile == current_profile;
    let elem_id: SharedString = target_profile
        .as_deref()
        .map(|id| format!("profile:{id}"))
        .unwrap_or_else(|| target.id().to_string())
        .into();
    div()
        .id(elem_id)
        .flex_none()
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .text_color(if selected { fg } else { muted })
        .when(selected, |d| d.bg(accent.opacity(0.18)))
        .when(!selected, |d| d.hover(|s| s.text_color(fg)))
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if this.history_agent != target || this.history_profile != target_profile {
                    this.history_agent = target;
                    this.history_profile = target_profile.clone();
                    this.session_detail_gen = this.session_detail_gen.wrapping_add(1);
                    this.session_detail = None;
                    this.history_detail_list_state.reset(0);
                    cx.notify();
                }
            }),
        )
}

use crate::settings::{ConversationAgentKind, HistorySourceKind};

/// 历史会话缓存 key：各家 agent 各存各的，同一个 cwd 换个 tab 是完全不同的数据，
/// 光用 cwd 当 key 会把不同 agent 的列表互相顶掉。手动添加的
/// workspace profile 跟同 kind 的默认 workspace 也是两份完全不同的数据，
/// `profile_id` 折进 key 里，同一个 kind 下的不同 profile 才不会互相顶掉
/// （`profile_id` 本身已经唯一决定了 override 目录，不用再单独编码目录值）。
pub(crate) fn session_list_key(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    cwd: &str,
) -> String {
    format!("{}:{}:{cwd}", agent.id(), profile_id.unwrap_or("default"))
}

pub(crate) fn normalized_profile_override_dir(
    profile: &smelt_core::agent_kind::AcpProfile,
) -> Option<String> {
    let launch = profile.launch_spec().ok()?;
    let env_var = profile.env_var().ok()?;
    smelt_core::workspace_override::env_override_from_launch(&launch, env_var)
}

fn current_profile_launch(
    config: &crate::settings::AgentHostState,
    profile: &smelt_core::agent_kind::AcpProfile,
) -> Result<smelt_core::agent_kind::ConversationLaunchSpec, String> {
    config.profile_launch_spec(profile)
}

fn list_sessions_for(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    override_dir: Option<&str>,
    cwd: &str,
) -> Vec<SessionSummary> {
    // 自定义名称的覆盖已经在 smelt-core 那层贴好（移动端读的是同一份），这里只做
    // 结构转换。
    smelt_core::session_control::list_history_for(agent, profile_id, cwd, override_dir)
        .into_iter()
        .map(|session| SessionSummary {
            title: session.display_title().to_string(),
            path: session.path,
            agent_title: session.title,
            custom_title: session.custom_title,
            resume_id: session.resume_id,
            started_at: session.started_at,
            last_active_at: session.last_active_at,
            message_count: session.message_count,
            total_tokens: session.total_tokens,
        })
        .collect()
}

/// 删除一份已经由历史列表发现的存档。拒绝符号链接和没有文件名的路径，避免右键
/// 菜单状态过期后误删到更高层目录。
fn delete_history_path(path: &Path) -> Result<(), String> {
    if path.file_name().is_none() || path.parent().is_none() {
        return Err("历史会话路径无效".into());
    }
    let metadata = LocalFs
        .symlink_metadata(path)
        .ok_or_else(|| "历史会话已不存在".to_string())?;
    if metadata.is_symlink {
        return Err("拒绝删除符号链接形式的历史会话".into());
    }
    if metadata.is_dir {
        LocalFs
            .remove_dir_all(path)
            .map_err(|error| format!("删除会话目录失败：{error}"))
    } else {
        LocalFs
            .remove_file(path)
            .map_err(|error| format!("删除会话文件失败：{error}"))
    }
}

impl Workspace {
    /// 历史会话页：确保当前 agent（+ 可能选中的 workspace profile）+ 项目的会话
    /// 列表缓存新鲜（>10s 或缺失就后台重新扫描）。总览卡片那边固定传
    /// `(ConversationAgentKind::Claude, None)`，跟历史页的 tab 切换共用同一份缓存/同一套
    /// 读写路径。
    pub fn ensure_session_list(
        &mut self,
        agent: HistorySourceKind,
        profile_id: Option<String>,
        cwd: String,
        cx: &mut Context<Self>,
    ) {
        let override_dir = profile_id.as_deref().and_then(|id| {
            cx.global::<crate::settings::AgentHostState>()
                .find_profile(id)
                .and_then(normalized_profile_override_dir)
        });
        let key = session_list_key(agent, profile_id.as_deref(), &cwd);
        let fresh = self
            .session_list
            .get(&key)
            .is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_secs(10));
        if fresh || self.session_list_inflight.contains(&key) {
            return;
        }
        self.session_list_inflight.insert(key.clone());
        cx.spawn(async move |this, cx| {
            let c = cwd.clone();
            let od = override_dir.clone();
            let pid = profile_id.clone();
            let sessions = cx
                .background_executor()
                .spawn(async move { list_sessions_for(agent, pid.as_deref(), od.as_deref(), &c) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session_list_inflight.remove(&key);
                this.session_list
                    .insert(key.clone(), (Instant::now(), Rc::new(sessions)));
                if this.session_list_invalidated.remove(&key) {
                    this.session_list.remove(&key);
                    this.ensure_session_list(agent, profile_id.clone(), cwd.clone(), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 历史会话页：点开一份会话，按当前 tab 选中的 agent 用对应的解析器后台跑成
    /// Turn 列表。用自增 gen 丢弃过期结果（解析期间又点了别的会话，或切了 tab）。
    pub fn open_session_detail(
        &mut self,
        agent: HistorySourceKind,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.session_detail_gen = self.session_detail_gen.wrapping_add(1);
        let r#gen = self.session_detail_gen;
        self.session_detail = None;
        self.history_detail_list_state.reset(0);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let detail = cx
                .background_executor()
                .spawn(async move { load_agent_session_detail(agent, &p) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.session_detail_gen != r#gen {
                    return;
                }
                if let Some(detail) = detail {
                    this.history_detail_list_state.reset(detail.turns.len());
                    this.session_detail = Some((path, Rc::new(detail)));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 历史会话右键「删除」：先记录目标，等确认弹窗中明确确认后再删盘。
    pub(crate) fn start_delete_history(
        &mut self,
        target: DeleteHistoryTarget,
        cx: &mut Context<Self>,
    ) {
        self.delete_history_target = Some(target);
        cx.notify();
    }

    pub(crate) fn cancel_delete_history(&mut self, cx: &mut Context<Self>) {
        self.delete_history_target = None;
        cx.notify();
    }

    /// 确认删除历史会话。IO 放到后台线程，避免大体积 transcript 删除时卡住窗口；
    /// 完成后失效对应列表缓存并弹出结果通知。
    pub(crate) fn confirm_delete_history(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.delete_history_target.take() else {
            return;
        };
        self.session_detail_gen = self.session_detail_gen.wrapping_add(1);
        if self
            .session_detail
            .as_ref()
            .is_some_and(|(path, _)| path == &target.path)
        {
            self.session_detail = None;
            self.history_detail_list_state.reset(0);
        }
        cx.notify();

        let agent = target.agent;
        let profile_id = target.profile_id.clone();
        let cwd = target.cwd.clone();
        let resume_id = target.resume_id.clone();
        let path = target.path.clone();
        let title = target.title;
        let delete_profile_id = profile_id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = delete_history_path(&path);
                    if result.is_ok() {
                        // 标题仓库独立于 agent 的原始存档，删除存档时同步清理，
                        // 否则同一个 resume id 被复用时会继承旧标题。
                        let _ = smelt_core::session_control::rename_history_title(
                            agent,
                            delete_profile_id.as_deref(),
                            &resume_id,
                            None,
                            None,
                        );
                    }
                    result
                })
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                let key = session_list_key(agent, profile_id.as_deref(), &cwd);
                if result.is_ok() {
                    this.session_list.remove(&key);
                    if this.session_list_inflight.contains(&key) {
                        this.session_list_invalidated.insert(key.clone());
                    } else {
                        this.ensure_session_list(agent, profile_id.clone(), cwd.clone(), cx);
                    }
                    crate::status_item::notify_success(format!("已删除历史会话「{title}」"));
                } else if let Err(error) = result {
                    crate::status_item::notify_error(format!("删除历史会话失败：{error}"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 历史会话右键「删除」的二次确认弹窗。
    pub(crate) fn render_delete_history_confirm(&self, cx: &mut Context<Self>) -> Div {
        let Some(target) = self.delete_history_target.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let content = v_flex()
            .child(Self::modal_title(fg, "确定删除这条历史会话吗？"))
            .child(div().text_sm().text_color(muted).child(format!(
                "将永久删除 {} 的「{}」及其本地对话记录，此操作不可撤销。",
                target.agent.short_label(),
                target.title
            )))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-history",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_history(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-delete-history",
                        "确定删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_history(cx),
                        cx,
                    )),
            );
        Self::modal_shell(420., true, content, cx)
    }

    /// 历史会话页的「迁移到 →」：读源 agent 落盘的 transcript，压成交接 prompt，
    /// 交给目标 agent 起一条新会话。
    ///
    /// 跟同页的「ACP 继续」是两回事——那个是 `session/load`，只能给同一家 agent
    /// 用（agent 认自己 session store 里的 id）；这个是把对话内容重述给另一家，
    /// 目标 agent 从零开始，只是知道前面发生过什么。
    ///
    /// transcript 可能有几 MB，解析放后台线程，回到主线程才建会话。
    pub fn migrate_history_session(
        &mut self,
        source: HistoryMigrationSource,
        target: acp_view::AcpHandoffTarget,
        cx: &mut Context<Self>,
    ) {
        let path = source.path.clone();
        let source_agent = source.agent;
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let detail = cx
                .background_executor()
                .spawn(async move { load_agent_session_detail(source_agent.into(), &p) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(detail) = detail else {
                    // 存档被删/被换格式：明说读不出来，别让用户对着一条空会话猜。
                    crate::status_item::notify_error(format!(
                        "读不出这条历史会话，迁移取消：{}",
                        path.display()
                    ));
                    return;
                };
                let request = build_history_handoff_request(&source, &detail, target);
                this.add_acp_handoff_session(request, window, cx);
            });
        })
        .detach();
    }
}

/// 历史会话右键菜单里的「迁移到 →」分组：各家基础 agent + 手动添加的 workspace
/// profile，跳过源自己（同 provider 继续仍走 `session/load` / CLI resume）。这是唯一
/// 保留的文本交接入口；活体 ACP 会话不再提供交接菜单。
fn migration_menu(
    menu: gpui_component::menu::PopupMenu,
    ws: &Entity<crate::Workspace>,
    source: HistoryMigrationSource,
    source_profile_id: Option<&str>,
    cx: &App,
) -> gpui_component::menu::PopupMenu {
    let config = cx.global::<crate::settings::AgentHostState>().clone();
    let mut menu = menu.separator().item(PopupMenuItem::label("迁移到"));
    for target_agent in ConversationAgentKind::ALL
        .into_iter()
        .filter(|agent| agent.is_bare_kind())
    {
        if target_agent == source.agent && source_profile_id.is_none() {
            continue;
        }
        let target = acp_view::AcpHandoffTarget {
            agent: target_agent,
            launch: smelt_core::agent_kind::ConversationLaunchSpec::from_command(
                config.acp_cmd_for(target_agent),
            ),
            profile_id: None,
            profile_label: None,
        };
        let ws = ws.clone();
        let source = source.clone();
        menu = menu.item(PopupMenuItem::new(target_agent.label()).on_click(
            move |_ev, _window, cx| {
                let (source, target) = (source.clone(), target.clone());
                ws.update(cx, |this, cx| {
                    this.migrate_history_session(source, target, cx);
                });
            },
        ));
    }
    for profile in config.all_profiles() {
        if source_profile_id == Some(profile.id.as_str()) {
            continue;
        }
        let Some(agent) = profile.kind() else {
            continue;
        };
        let Ok(launch) = current_profile_launch(&config, profile) else {
            continue;
        };
        let target = acp_view::AcpHandoffTarget {
            agent,
            launch,
            profile_id: Some(profile.id.clone()),
            profile_label: Some(profile.label.clone()),
        };
        let ws = ws.clone();
        let source = source.clone();
        menu = menu.item(PopupMenuItem::new(profile.label.clone()).on_click(
            move |_ev, _window, cx| {
                let (source, target) = (source.clone(), target.clone());
                ws.update(cx, |this, cx| {
                    this.migrate_history_session(source, target, cx);
                });
            },
        ));
    }
    menu
}

/// 发起一次历史会话迁移需要知道的源信息（都在历史页那一行上现成拿得到）。
#[derive(Clone)]
pub struct HistoryMigrationSource {
    pub agent: ConversationAgentKind,
    /// 源 workspace profile 名；默认 workspace 为 `None`。
    pub profile_label: Option<String>,
    pub title: String,
    /// 源 agent 自己认的 session id（历史页那行的 `resume_id`）。只作为溯源信息
    /// 记进新会话，不发给目标 agent——它不认别家的 id。
    pub resume_id: String,
    pub path: std::path::PathBuf,
    pub cwd: String,
}

/// 组装历史迁移的交接请求。抽出来是为了能脱离 GPUI 单测：这里决定了目标会话
/// 拿到的第一句话长什么样。
fn build_history_handoff_request(
    source: &HistoryMigrationSource,
    detail: &SessionDetail,
    target: acp_view::AcpHandoffTarget,
) -> acp_view::AcpHandoffRequest {
    use smelt_core::session_handoff::{HandoffContext, HandoffPeer, build_handoff_prompt};

    let context = HandoffContext {
        turns: handoff_turns_from_history(detail),
        source_title: &source.title,
        source: HandoffPeer::new(source.agent, source.profile_label.as_deref()),
        target: HandoffPeer::new(target.agent, target.profile_label.as_deref()),
        cwd: Some(source.cwd.as_str()),
        source_model: detail.model.clone(),
    };
    let prompt = build_handoff_prompt(&context);

    acp_view::AcpHandoffRequest {
        source: Some(acp_view::AcpForkOrigin {
            session_id: source.resume_id.clone(),
            title: source.title.clone(),
            agent: Some(source.agent.id().to_string()),
            profile_label: source.profile_label.clone(),
            // 源是磁盘上的历史存档，不是当前开着的某条 Smelt 会话——「返回原会话」
            // 按 sid 找不到东西，banner 那颗按钮要藏起来。
            from_history: true,
        }),
        cwd: Some(source.cwd.clone()),
        agent: target.agent,
        launch: target.launch,
        // 历史迁移的目标总是从菜单现选的：基础 agent 跟设置页走，profile 用自己的命令。
        refresh_launch_from_settings: target.profile_id.is_none(),
        profile_id: target.profile_id,
        // 模型/配置是各家私有取值，一律不跨会话搬（prompt 尾部已写明）。
        config_values: Vec::new(),
        ephemeral_env: Default::default(),
        prompt,
        // 历史迁移是文本交接，不搬图片。
        images: Vec::new(),
        profile_label: target.profile_label,
        resume_session_id: None,
        fork_session_id: None,
        fork_cut: None,
        conversation_binding: smelt_core::conversation::ConversationBinding::Direct,
        agent_session: None,
    }
}

#[cfg(test)]
mod tests {
    // 不用 `use super::*;`：本文件后半段引入了 gpui/gpui_component 的 glob 导入，
    // 带进这个测试模块会让 trait 解析图爆炸式增长，`cargo test` 编译期直接撞
    // rustc 的递归限制崩溃（甚至 SIGBUS）——只导入测试真正用到的几个名字就够了。
    use super::{
        HistoryMigrationSource, HistorySourceTab, SessionDetail, Turn,
        build_history_handoff_request, current_profile_launch, handoff_turns_from_history,
        history_continue_label, history_row_is_agent_session, history_source_tabs,
        list_sessions_for, normalized_profile_override_dir, project_dir,
    };
    use crate::settings::AgentHostState;
    use smelt_core::agent_kind::{AcpProfile, ConversationAgentKind};
    use smelt_core::session_handoff::HandoffTurn;
    use std::path::Path;

    fn write(dir: &Path, name: &str, lines: &[&str]) {
        std::fs::write(dir.join(name), lines.join("\n")).unwrap();
    }

    fn test_sandbox(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-artifacts/session-history")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn history_profile_override_dir_preserves_spaces() {
        let home = test_sandbox("ovh");
        let workspace_dir = home.join("Claude Workspaces").join("quant");
        let profile = AcpProfile {
            id: "quant".into(),
            kind_id: "claude".into(),
            label: "Quant".into(),
            workspace_dir: workspace_dir.display().to_string(),
        };

        let override_dir = normalized_profile_override_dir(&profile).unwrap();

        assert_eq!(override_dir, workspace_dir.display().to_string());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn history_reader_finds_workspace_override_with_spaces() {
        let home = test_sandbox("ovr");
        let workspace_dir = home.join("Claude Workspaces").join("quant");
        let profile = AcpProfile {
            id: "quant".into(),
            kind_id: "claude".into(),
            label: "Quant".into(),
            workspace_dir: workspace_dir.display().to_string(),
        };
        let project_root = workspace_dir.join("projects").join(project_dir("/x/y"));
        std::fs::create_dir_all(&project_root).unwrap();
        write(
            &project_root,
            "quant.jsonl",
            &[
                r#"{"type":"user","timestamp":"2026-07-05T00:00:00Z","message":{"content":"with spaces in override path"}}"#,
            ],
        );

        let override_dir = normalized_profile_override_dir(&profile).unwrap();
        let sessions = list_sessions_for(
            ConversationAgentKind::Claude.into(),
            None,
            Some(&override_dir),
            "/x/y",
        );

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "with spaces in override path");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn history_resume_profile_launch_uses_current_agent_command() {
        let profile = AcpProfile {
            id: "quant".into(),
            kind_id: "claude".into(),
            label: "Quant".into(),
            workspace_dir: "~/Claude Workspaces/quant".into(),
        };
        let config = AgentHostState::default()
            .with_acp_cmd(ConversationAgentKind::Claude, "claude --current");

        let launch = current_profile_launch(&config, &profile).expect("有效 profile");

        assert_eq!(launch.command, "claude --current");
        assert_eq!(
            launch.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Claude Workspaces/quant")
        );
    }

    #[test]
    fn unknown_profile_has_no_history_override() {
        let profile = AcpProfile {
            id: "future-profile".into(),
            kind_id: "future-agent".into(),
            label: "Future Agent".into(),
            workspace_dir: "~/.future-agent".into(),
        };

        assert_eq!(normalized_profile_override_dir(&profile), None);
    }

    #[test]
    fn history_turns_become_handoff_turns_without_agent_only_fields() {
        let detail = SessionDetail {
            model: Some("claude-opus-5".into()),
            turns: vec![
                Turn {
                    is_user: true,
                    timestamp: None,
                    text: "改一下滚动".into(),
                    tools: Vec::new(),
                    tool_paths: Vec::new(),
                },
                Turn {
                    is_user: false,
                    timestamp: None,
                    text: "改好了".into(),
                    tools: vec!["Edit".into(), "Read".into(), "Read".into()],
                    tool_paths: vec!["src/main.rs".into()],
                },
                // 只有工具、没有正文的轮次（Codex 常见）不该产出空的 Assistant
                Turn {
                    is_user: false,
                    timestamp: None,
                    text: String::new(),
                    tools: vec!["exec_command".into()],
                    tool_paths: Vec::new(),
                },
            ],
        };

        let turns = handoff_turns_from_history(&detail);
        assert_eq!(
            turns,
            vec![
                HandoffTurn::User {
                    text: "改一下滚动".into(),
                    images: 0,
                },
                HandoffTurn::Assistant("改好了".into()),
                // 一轮里连着调两次 Read：折成「×2」，不铺开占预算
                HandoffTurn::Tool {
                    title: "Edit、Read ×2".into(),
                    detail: None,
                    paths: vec!["src/main.rs".into()],
                },
                HandoffTurn::Tool {
                    title: "exec_command".into(),
                    detail: None,
                    paths: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn history_migration_request_carries_source_identity_and_no_inherited_config() {
        let detail = SessionDetail {
            model: Some("grok-4.5".into()),
            turns: vec![Turn {
                is_user: true,
                timestamp: None,
                text: "接着干".into(),
                tools: Vec::new(),
                tool_paths: Vec::new(),
            }],
        };
        let source = HistoryMigrationSource {
            agent: ConversationAgentKind::Grok,
            profile_label: None,
            title: "老会话".into(),
            resume_id: "grok-session-1".into(),
            path: std::path::PathBuf::from("/tmp/does-not-matter"),
            cwd: "/repo".into(),
        };
        let target = crate::acp_view::AcpHandoffTarget {
            agent: ConversationAgentKind::Claude,
            launch: smelt_core::agent_kind::ConversationLaunchSpec::from_command("claude"),
            profile_id: None,
            profile_label: None,
        };

        let request = build_history_handoff_request(&source, &detail, target);
        let origin = request.source.expect("迁移来的会话必须记得源");
        assert_eq!(origin.agent.as_deref(), Some("grok"));
        assert_eq!(origin.session_id, "grok-session-1");
        // 源在磁盘上，不是开着的 Smelt 会话——顶栏不该给「返回原会话」
        assert!(origin.from_history);
        assert!(request.config_values.is_empty());
        assert!(request.prompt.contains("原会话由 Grok 进行"));
        assert!(request.prompt.contains("现在由你（Claude Code）接手"));
        assert!(request.prompt.contains("原会话使用的模型：grok-4.5"));
        assert!(request.prompt.contains("接着干"));
    }

    #[test]
    fn ordinary_project_lists_bare_kinds_and_skips_profile_bound_agents() {
        let tabs = history_source_tabs(None, []);
        assert!(!tabs.is_empty());
        assert!(tabs.iter().all(|tab| tab.kind.is_bare_kind()));
        assert!(tabs.iter().all(|tab| tab.profile_id.is_none()));
        assert!(
            tabs.iter()
                .any(|tab| tab.kind == ConversationAgentKind::Pi.into())
        );
        assert!(
            !tabs
                .iter()
                .any(|tab| tab.kind == ConversationAgentKind::Dsh.into())
        );
        // 只有 TUI 的 agent 同样要出现在历史 tab 里：能不能读历史与有没有 ACP 无关。
        assert!(tabs.iter().any(|tab| tab.kind
            == crate::settings::HistorySourceKind::TerminalOnly(
                crate::settings::TerminalAgentKind::Antigravity
            )));
    }

    #[test]
    fn agent_context_keeps_only_that_engine() {
        let tabs = history_source_tabs(Some(ConversationAgentKind::Pi), []);
        assert_eq!(
            tabs,
            vec![HistorySourceTab {
                kind: ConversationAgentKind::Pi.into(),
                profile_id: None,
                label: ConversationAgentKind::Pi.short_label().to_string(),
            }]
        );
    }

    #[test]
    fn project_history_names_bound_agent_sessions_continue_conversation() {
        assert!(!history_row_is_agent_session(false, None));
        assert!(history_row_is_agent_session(false, Some("writer")));
        assert!(history_row_is_agent_session(true, None));
        assert_eq!(history_continue_label(false), "ACP 继续");
        assert_eq!(history_continue_label(true), "继续对话");
    }
}
