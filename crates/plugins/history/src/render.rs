//! 历史页的画法：顶上来源标签，左边对话内容，右边会话列表。只读视图上缓存好的状态。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use smelt_plugin_api::model::acp_chat::is_interrupt_marker;
use smelt_plugin_api::model::agent_kind::HistorySourceKind;
use smelt_plugin_api::model::session_history::{SessionSummary, Turn};
use smelt_plugin_api::ui::button::Button;
use smelt_plugin_api::ui::markdown_links::FileLinks;
use smelt_plugin_api::ui::markdown_mermaid::chat_markdown_view;
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::ui_theme;
use smelt_plugin_api::{self as api, RuntimeKey};

use crate::HistoryPage;
use crate::model::*;

/// 会话列表的宽度上下限，和文件、Git 的列表一致。
const LIST_MIN_WIDTH: f32 = 190.;
const LIST_MAX_WIDTH: f32 = 320.;

/// 历史详情各轮正文的解析状态，按轮次下标留着。
///
/// 详情是虚拟列表。解析结果挂在元素上的话，行一滚出渲染范围就丢：超过 4KB 的正文
/// 滚回来要先以空内容等后台重新解析，行高跟着跳（对话页同一个坑，见
/// `chat_markdown_view`）。解析落地时请列表重新量这一行，还在视口外的行也能在滚到之前量对。
#[derive(Default)]
pub(crate) struct TurnMarkdown {
    turns: RefCell<HashMap<usize, (Entity<TextViewState>, Subscription)>>,
}

impl TurnMarkdown {
    fn view(
        &self,
        index: usize,
        text: &str,
        links: FileLinks<'_>,
        list_state: &ListState,
        cx: &mut App,
    ) -> TextView {
        let state = self
            .turns
            .borrow_mut()
            .entry(index)
            .or_insert_with(|| {
                let state = cx.new(|cx| TextViewState::markdown(text, cx));
                let list_state = list_state.clone();
                let remeasure = cx.observe(&state, move |_, _| {
                    list_state.remeasure_items(index..index + 1);
                });
                (state, remeasure)
            })
            .0
            .clone();
        chat_markdown_view(&state, links)
    }
}

/// 一行会话右键菜单要用的：这一行和它在这个来源下能做什么。
#[derive(Clone)]
struct RowActions {
    page: Entity<HistoryPage>,
    session: SessionSummary,
    agent: HistorySourceKind,
    profile_id: Option<String>,
    /// 按智能体续接：不给裸引擎的入口（CLI/TUI 续接、迁移会绕开智能体）。
    is_agent: bool,
    machine: RuntimeKey,
}

impl Render for HistoryPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, fg, border, accent) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.foreground,
                theme.border,
                theme.primary,
            )
        };
        let Some(project) = self.project.clone() else {
            return v_flex()
                .size_full()
                .child(placeholder("当前没有活动项目", muted));
        };
        let (agent, profile_id) = self.source();
        let settings = api::host(cx).agent_settings(&project.machine, cx);
        let current_key = list_key(
            &project.machine,
            agent,
            profile_id.as_deref(),
            &project.root,
        );
        let current = self.lists.get(&current_key);
        // 智能体配置借着 cx，先把要用的都算出来，后面还要可变地用 cx。
        let bindings = current
            .map(|list| known_bindings(&list.bindings, settings))
            .unwrap_or_default();

        let tabs = tabs_with_sessions(
            source_tabs(
                self.engine,
                settings.into_iter().flat_map(|settings| &settings.profiles),
            ),
            (agent, profile_id.as_deref()),
            |tab| {
                let key = list_key(
                    &project.machine,
                    tab.kind,
                    tab.profile_id.as_deref(),
                    &project.root,
                );
                self.lists
                    .get(&key)
                    .is_some_and(|list| !list.snapshot.sessions.is_empty())
            },
        );
        let switcher = self.render_switcher(tabs, (agent, profile_id.clone()), border, cx);

        // 打开着的那份不在当前列表里（被删了、换了来源）就不画。
        let sessions = current.map(|list| list.snapshot.sessions.clone());
        let opened = self.detail.as_ref().filter(|opened| {
            sessions
                .as_ref()
                .is_some_and(|list| list.iter().any(|session| session.path == opened.path))
        });
        let selected = opened.and_then(|opened| {
            sessions
                .as_ref()?
                .iter()
                .find(|session| session.path == opened.path)
                .cloned()
        });

        let list_body = match current {
            None => placeholder("加载中…", muted).into_any_element(),
            Some(list) if list.snapshot.sessions.is_empty() => {
                placeholder("这个项目还没有本地保存的历史会话", muted).into_any_element()
            }
            Some(list) => {
                let query = self.filter.read(cx).value().trim().to_lowercase();
                let visible: Rc<Vec<usize>> = Rc::new(
                    list.snapshot
                        .sessions
                        .iter()
                        .enumerate()
                        .filter(|(_, session)| {
                            query.is_empty()
                                || session.title.to_lowercase().contains(&query)
                                || session.agent_title.to_lowercase().contains(&query)
                        })
                        .map(|(ix, _)| ix)
                        .collect(),
                );
                if visible.is_empty() {
                    placeholder("没有匹配的历史会话", muted).into_any_element()
                } else {
                    let in_agent_space = self.engine.is_some();
                    let page = cx.entity();
                    let sessions = list.snapshot.sessions.clone();
                    let row_text = list.snapshot.row_text.clone();
                    let row_keys = list.snapshot.row_keys.clone();
                    let selected_path = selected.as_ref().map(|session| session.path.clone());
                    let machine = project.machine.clone();
                    uniform_list("history-sessions", visible.len(), move |range, _, _| {
                        range
                            .map(|row| {
                                let ix = visible[row];
                                let session = &sessions[ix];
                                let actions = RowActions {
                                    page: page.clone(),
                                    session: session.clone(),
                                    agent,
                                    profile_id: profile_id.clone(),
                                    is_agent: is_agent_session(
                                        in_agent_space,
                                        bindings.get(&session.resume_id).map(String::as_str),
                                    ),
                                    machine: machine.clone(),
                                };
                                let (title, secondary) = row_text[ix].clone();
                                let is_selected =
                                    selected_path.as_deref() == Some(session.path.as_str());
                                session_row(
                                    row_keys[ix].clone(),
                                    title,
                                    secondary,
                                    is_selected,
                                    actions,
                                    (muted, fg, border, accent),
                                )
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

        let detail_header = selected.as_ref().map(|session| {
            h_flex()
                .flex_none()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .font_semibold()
                        .text_color(fg)
                        .overflow_hidden()
                        .child(truncated_line(session.title.clone())),
                )
                .child(meta(session_when(session), muted))
                .child(meta(format!("{} 条消息", session.message_count), muted))
                .when(session.total_tokens > 0, |row| {
                    row.child(meta(format_count(session.total_tokens), muted))
                })
        });

        let turns_body = match opened {
            None => placeholder("← 选择一个历史会话查看内容", muted).into_any_element(),
            Some(opened) if opened.detail.turns.is_empty() => {
                placeholder("这份会话没有可展示的对话内容", muted).into_any_element()
            }
            Some(opened) => {
                let detail = opened.detail.clone();
                let turn_markdown = opened.turn_markdown.clone();
                let remeasure = self.detail_list.clone();
                // 文件链接按项目目录补全；远程项目的链接带上机器，点开时去那台机器读。
                let cwd = project.root.clone();
                let remote = match &project.machine {
                    RuntimeKey::Local => None,
                    RuntimeKey::Remote(id) => Some(id.clone()),
                };
                // 和对话页同一套排版：居中限宽的阅读栏，间距算在虚拟项自己身上。
                list(self.detail_list.clone(), move |i, _, cx| {
                    let turn = &detail.turns[i];
                    let links = FileLinks::new(Some(&cwd), remote.as_deref());
                    let body = turn_markdown.view(i, &turn.text, links, &remeasure, cx);
                    h_flex()
                        .w_full()
                        .justify_center()
                        .px_4()
                        .pb(px(16.))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .max_w(ui_theme::conversation_max_width())
                                .child(history_turn(turn, body, muted, fg, border)),
                        )
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

        // 和文件、Git 一样是「详情 + 列表」：列表用相对宽度配上下限，窄面板不被它吃满，
        // 全屏时也不至于窄得不能用。
        let list_panel = v_flex()
            .h_full()
            .min_h_0()
            .min_w_0()
            .w(relative(0.38))
            .min_w(px(LIST_MIN_WIDTH))
            .max_w(px(LIST_MAX_WIDTH))
            .flex_none()
            .border_l_1()
            .border_color(border)
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .px_2()
                    .pt_2()
                    .child(Input::new(&self.filter).small().cleanable(true)),
            )
            .child(list_body);

        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(switcher)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .items_stretch()
                    .overflow_hidden()
                    .child(detail_body)
                    .child(list_panel),
            )
    }
}

impl HistoryPage {
    /// 顶栏：左边手动刷新，右边来源标签。放不下时占位收成零，标签条自己横向滚。
    fn render_switcher(
        &self,
        tabs: Vec<SourceTab>,
        current: (HistorySourceKind, Option<String>),
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = tabs
            .iter()
            .position(|tab| (tab.kind, tab.profile_id.as_ref()) == (current.0, current.1.as_ref()));
        let targets: Vec<_> = tabs
            .iter()
            .map(|tab| (tab.kind, tab.profile_id.clone()))
            .collect();
        let page = cx.entity();
        h_flex()
            .w_full()
            .h(px(38.))
            .min_w_0()
            .flex_none()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .child(
                Button::new("history-refresh")
                    .quiet()
                    .icon_size()
                    .icon(Icon::new(IconName::RefreshCw))
                    .tooltip("刷新")
                    .on_click(move |_, _, cx| {
                        page.update(cx, |page, cx| page.refresh_now(cx));
                    }),
            )
            .child(div().flex_1().min_w_0())
            // 智能体的 space 只有一家，不画标签。
            .when(self.engine.is_none(), |bar| {
                bar.child(
                    TabBar::new("history-agent-switcher")
                        .segmented()
                        .min_w_0()
                        .when_some(selected, |bar, ix| bar.selected_index(ix))
                        .on_click(cx.listener(move |page, ix: &usize, _, cx| {
                            if let Some((kind, profile)) = targets.get(*ix).cloned() {
                                page.select_source(kind, profile, cx);
                            }
                        }))
                        .children(tabs.into_iter().map(|tab| Tab::new().label(tab.label))),
                )
            })
    }
}

fn session_row(
    key: SharedString,
    title: SharedString,
    secondary: SharedString,
    is_selected: bool,
    actions: RowActions,
    (muted, fg, border, accent): (Hsla, Hsla, Hsla, Hsla),
) -> AnyElement {
    let open = actions.clone();
    div()
        .w_full()
        .px_2()
        .pb_1()
        .child(
            v_flex()
                .id((ElementId::from("history-session"), key))
                .w_full()
                .h(px(54.))
                .justify_center()
                .gap_0p5()
                .px_2()
                .rounded_md()
                .text_color(fg)
                .when(is_selected, |row| row.bg(accent.opacity(0.18)))
                .when(!is_selected, |row| {
                    row.hover(|style| style.bg(border.opacity(0.5)))
                })
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .overflow_hidden()
                        .child(truncated_line(title)),
                )
                .child(
                    div()
                        .w_full()
                        .text_xs()
                        .text_color(muted)
                        .overflow_hidden()
                        .child(truncated_line(secondary)),
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    let path = open.session.path.clone();
                    open.page.update(cx, |page, cx| page.open_detail(path, cx));
                })
                .context_menu(move |menu, _, cx| row_menu(menu, &actions, cx)),
        )
        .into_any_element()
}

/// 一行的右键菜单。这个来源能干什么由它自己的能力决定：
/// - ACP 续接、迁移要 ACP 身份，纯终端来源（Antigravity）没有，只能起它的 CLI。
/// - 删除：Antigravity 的历史同时在总索引库和正文库两处，`agy` 可能正开着；只删一半会
///   把索引弄成死链，所以不给删，而不是给一个半对的删除。
fn row_menu(mut menu: PopupMenu, row: &RowActions, cx: &App) -> PopupMenu {
    if row.agent.acp().is_some() {
        let it = row.clone();
        menu = menu.item(PopupMenuItem::new(continue_label(row.is_agent)).on_click(
            move |_, _, cx| {
                it.page
                    .update(cx, |page, cx| page.resume(&it.session, false, cx));
            },
        ));
    }
    // 智能体会话不给裸引擎的入口：CLI 续接、迁移都会绕开智能体，插件、工作方式、绑定的
    // 上下文全都带不上。
    if !row.is_agent {
        let it = row.clone();
        menu = menu.item(
            PopupMenuItem::new("CLI/TUI 继续").on_click(move |_, _, cx| {
                it.page
                    .update(cx, |page, cx| page.resume(&it.session, true, cx));
            }),
        );
        // 「继续」是同一家接着聊；迁移是把内容重述给另一家，分开一组。
        if let Some(source) = row.agent.acp() {
            let settings = api::host(cx).agent_settings(&row.machine, cx);
            menu = menu.separator().item(PopupMenuItem::label("迁移到"));
            for target in handoff_targets(source, row.profile_id.as_deref(), settings) {
                let it = row.clone();
                menu = menu.item(
                    PopupMenuItem::new(target.label()).on_click(move |_, _, cx| {
                        let target = target.clone();
                        it.page
                            .update(cx, |page, cx| page.hand_off(&it.session, target, cx));
                    }),
                );
            }
        }
    }
    let it = row.clone();
    menu = menu
        .separator()
        .item(PopupMenuItem::new("重命名").on_click(move |_, window, cx| {
            it.page
                .update(cx, |page, cx| page.start_rename(&it.session, window, cx));
        }));
    if row.session.custom_title.is_some() {
        let it = row.clone();
        menu = menu.item(
            PopupMenuItem::new("恢复默认名称").on_click(move |_, _, cx| {
                it.page.read(cx).reset_title(&it.session, cx);
            }),
        );
    }
    if row.agent.acp().is_some() {
        let it = row.clone();
        menu = menu
            .separator()
            .item(PopupMenuItem::new("删除").on_click(move |_, window, cx| {
                it.page
                    .update(cx, |page, cx| page.confirm_delete(&it.session, window, cx));
            }));
    }
    let path = row.session.path.clone();
    menu.item(
        PopupMenuItem::new("复制文件路径").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
        }),
    )
}

/// 一轮对话，排版对齐对话页：用户消息是靠右的气泡，agent 回答是不套卡片的正文。
fn history_turn(turn: &Turn, body: TextView, muted: Hsla, fg: Hsla, border: Hsla) -> AnyElement {
    // agent 回显的中断标记不是用户说的话，和对话页一样画成分隔线。
    if turn.is_user && is_interrupt_marker(&turn.text) {
        return h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(div().flex_1().h(px(1.)).bg(border))
            .child(div().text_xs().text_color(muted).child("已中断"))
            .child(div().flex_1().h(px(1.)).bg(border))
            .into_any_element();
    }
    if turn.is_user {
        // 最小宽度的原因见对话页 `user_message_bubble`：markdown 列表在收缩的气泡里会被裁掉。
        return h_flex()
            .w_full()
            .justify_end()
            .child(
                div()
                    .max_w(relative(0.72))
                    .min_w(px(160.))
                    .px_4()
                    .py_2p5()
                    .rounded(ui_theme::bubble_radius())
                    .bg(ui_theme::overlay(0x14))
                    .text_sm()
                    .text_color(fg)
                    .child(body),
            )
            .into_any_element();
    }
    v_flex()
        .w_full()
        .min_w_0()
        .gap_1()
        .text_sm()
        .text_color(fg)
        .when(!turn.text.trim().is_empty(), |col| col.child(body))
        .children(tool_summary(&turn.tools).map(|summary| {
            div()
                .text_xs()
                .text_color(muted)
                .child(format!("🔧 {summary}"))
        }))
        .into_any_element()
}

/// 这一轮用过的工具，按首次出现排，重复的记次数：`Read ×2 · Edit`。
fn tool_summary(tools: &[String]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let mut order: Vec<&String> = Vec::new();
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for tool in tools {
        *counts.entry(tool).or_insert_with(|| {
            order.push(tool);
            0
        }) += 1;
    }
    Some(
        order
            .into_iter()
            .map(|name| match counts[name] {
                1 => name.clone(),
                count => format!("{name} ×{count}"),
            })
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

fn meta(text: impl Into<SharedString>, muted: Hsla) -> Div {
    div()
        .flex_none()
        .text_xs()
        .text_color(muted)
        .child(text.into())
}

fn placeholder(text: &'static str, muted: Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .px_4()
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_center()
                .text_color(muted)
                .child(text),
        )
}
