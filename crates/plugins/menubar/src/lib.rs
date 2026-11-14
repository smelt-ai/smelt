//! 菜单栏：右上角图标旁显示要你看的会话数和运行中的会话数（`N@·M`），点开列出这些会话，
//! 点一条跳过去；Dock 图标的角标是要你看的会话数。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道：会话名册（各台机器的工作区目录）、
//! 会话状态、要人看的会话都读宿主的，菜单和角标经 `smelt.app/set-status-menu`、
//! `smelt.app/set-dock-badge` 交给宿主画，点菜单项执行 `smelt.sessions/activate`。主窗口
//! 关着也照常更新。
//!
//! 什么时候重算全靠事件：会话、提醒、机器变了；宿主命令登记好了（启动时命令要等主窗口
//! 建好才有）。算出来和上次交给宿主的一样就不再交。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use api::model::agent_kind::{ConversationAgentKind, agent_id_from_launch};
use api::model::agent_status::{AgentStatus, terminal_agent_status};
use api::model::daemon_state::DaemonSessionState;
use api::model::session_title::{acp_display_title, display_title};
use api::model::workspace_directory::WorkspaceConversation;
use api::sessions::{self, Activate};
use api::ui::ui_theme::agent_status_u32;
use api::{DockBadge, HostEvent, RuntimeKey, StatusIcon, StatusMenu, StatusMenuItem};
use gpui::{App, Task};
use smelt_plugin_api as api;

/// 各家 agent 的单色母图（32px，只用透明度当形状），手机端也用这一份。
const AGENT_LOGO_PNG: &[(&str, &[u8])] = &[
    (
        "claude",
        include_bytes!("../../../../assets/agent-claude.png"),
    ),
    (
        "copilot",
        include_bytes!("../../../../assets/agent-copilot.png"),
    ),
    (
        "codex",
        include_bytes!("../../../../assets/agent-codex.png"),
    ),
    ("grok", include_bytes!("../../../../assets/agent-grok.png")),
    (
        "antigravity",
        include_bytes!("../../../../assets/agent-antigravity.png"),
    ),
    (
        "cursor",
        include_bytes!("../../../../assets/agent-cursor.png"),
    ),
    (
        "opencode",
        include_bytes!("../../../../assets/agent-opencode.png"),
    ),
    ("kiro", include_bytes!("../../../../assets/agent-kiro.png")),
    ("pi", include_bytes!("../../../../assets/agent-pi.png")),
    (
        "crush",
        include_bytes!("../../../../assets/agent-crush.png"),
    ),
];

/// 认不出 agent 的会话（裸终端、自定义 agent）用的方框终端图标。
const TERMINAL_PNG: &[u8] = include_bytes!("../assets/terminal.png");

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    let sent = Rc::new(RefCell::new(Sent::default()));
    let on_event = sent.clone();
    let events = api::events(cx);
    cx.subscribe(&events, move |_, event: &HostEvent, cx| {
        if matches!(
            event,
            HostEvent::SessionsChanged
                | HostEvent::AttentionChanged
                | HostEvent::MachinesChanged
                | HostEvent::CommandsChanged
        ) {
            refresh(&on_event, cx);
        }
    })
    .detach();
    refresh(&sent, cx);
}

/// 上次交给宿主的。交失败了清掉，下次事件重交。
#[derive(Default)]
struct Sent {
    menu: Option<Menu>,
    badge: Option<DockBadge>,
}

fn refresh(sent: &Rc<RefCell<Sent>>, cx: &mut App) {
    // 宿主命令跟着主窗口的工作台登记，启动时还没有；登记好了会发 CommandsChanged。
    if api::command(api::SET_STATUS_MENU.id(), cx).is_none() {
        return;
    }
    let host = api::host(cx);
    let mut conversations = Vec::new();
    for machine in host.machines(cx) {
        let Some(directory) = host.workspace_directory(&machine.key, cx) else {
            continue;
        };
        let states: HashMap<String, DaemonSessionState> = host
            .session_states(&machine.key, cx)
            .into_iter()
            .map(|state| (state.id.clone(), state))
            .collect();
        let remote = (machine.key != RuntimeKey::Local).then_some(machine.name.as_str());
        conversations.extend(
            directory
                .conversations
                .iter()
                .filter(|conversation| conversation.archived_at.is_none())
                .filter_map(|conversation| conversation_row(conversation, &states, remote)),
        );
    }
    let attention: HashMap<String, bool> = host
        .attention(cx)
        .into_iter()
        .map(|item| (item.session_id, item.requires_action))
        .collect();
    let menu = menu(conversations, &attention);
    let badge = DockBadge {
        label: (!attention.is_empty()).then(|| attention.len().to_string()),
    };

    let mut last = sent.borrow_mut();
    if last.menu.as_ref() != Some(&menu) {
        let task = api::SET_STATUS_MENU.run(&menu.status_menu(), cx);
        last.menu = Some(menu);
        forget_on_failure(task, sent.clone(), |sent| sent.menu = None, cx);
    }
    if last.badge.as_ref() != Some(&badge) {
        let task = api::SET_DOCK_BADGE.run(&badge, cx);
        last.badge = Some(badge);
        forget_on_failure(task, sent.clone(), |sent| sent.badge = None, cx);
    }
}

fn forget_on_failure(
    task: Task<Result<(), String>>,
    sent: Rc<RefCell<Sent>>,
    forget: impl FnOnce(&mut Sent) + 'static,
    cx: &mut App,
) {
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[menubar] 交给宿主失败：{error}");
            forget(&mut sent.borrow_mut());
        }
    })
    .detach();
}

/// 一段对话在菜单里要用到的。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    /// 组成这段对话的守护会话 id：对话一个，终端每格一个。
    ids: Vec<String>,
    /// 每个 id 的状态，跟 `ids` 一一对应。
    statuses: Vec<AgentStatus>,
    title: String,
    /// agent 的 id，认不出是 `None`。
    agent: Option<String>,
}

impl Row {
    fn status(&self) -> AgentStatus {
        AgentStatus::highest(self.statuses.iter().copied())
    }
}

/// 一段对话的会话 id、状态、名字和 agent。一个会话都没起过的（终端还没开）是 `None`。
/// `remote` 是远程机器的名字，接在名字后面。
fn conversation_row(
    conversation: &WorkspaceConversation,
    states: &HashMap<String, DaemonSessionState>,
    remote: Option<&str>,
) -> Option<Row> {
    let status_of = |id: &str| terminal_agent_status(states.get(id));
    let (ids, title, agent) = if let Some(acp) = &conversation.acp {
        let state = states.get(&acp.sid);
        let label = ConversationAgentKind::from_id(&acp.agent)
            .map(ConversationAgentKind::label)
            .unwrap_or(acp.agent.as_str());
        let fallback = acp_display_title(acp.session_title.as_deref(), label, None);
        let title = display_title(
            conversation.custom_title.as_deref(),
            state.and_then(|state| state.title.as_deref()),
            None,
            Some(&fallback),
        );
        (vec![acp.sid.clone()], title, Some(acp.agent.clone()))
    } else {
        let started: Vec<_> = conversation
            .terminals
            .iter()
            .filter_map(|terminal| Some((terminal.id.clone()?, terminal)))
            .collect();
        // 名字和 agent 取第一格：只有一格时就是它；多格时对话的名字是用户给整段起的。
        let (first_id, first) = started.first()?;
        let state = states.get(first_id);
        let custom = if conversation.terminals.len() == 1 {
            first
                .custom_title
                .as_deref()
                .or(conversation.custom_title.as_deref())
        } else {
            conversation
                .custom_title
                .as_deref()
                .or(first.custom_title.as_deref())
        };
        let title = display_title(
            custom.or(state.and_then(|state| state.custom_title.as_deref())),
            state.and_then(|state| state.title.as_deref()),
            first.launch_label.as_deref(),
            None,
        );
        // 明确的启动命令优先；裸终端里手动起的 agent 认 hook 报的。
        let agent = first
            .launch_cmd
            .as_deref()
            .or(state.and_then(|state| state.launch.as_deref()))
            .and_then(agent_id_from_launch)
            .map(str::to_string)
            .or_else(|| state.and_then(|state| state.provider.clone()));
        let ids = started.iter().map(|(id, _)| id.clone()).collect();
        (ids, title, agent)
    };
    let title = match remote {
        Some(machine) => format!("{title} · {machine}"),
        None => title,
    };
    Some(Row {
        statuses: ids.iter().map(|id: &String| status_of(id)).collect(),
        ids,
        title,
        agent,
    })
}

/// 算好的菜单，比较用；交给宿主前才换成带图的 [`StatusMenu`]。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Menu {
    title: String,
    tooltip: String,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    /// 点了跳到的守护会话 id。
    session_id: String,
    label: String,
    agent: Option<String>,
    /// 图标颜色，`0xRRGGBB`。
    color: u32,
}

/// 列出运行中、要你处理、有没看提醒的对话，要你处理的排前面；运行数按所有对话算，要你看
/// 的数按所有提醒算（对话已经关了的提醒也算，宿主的系统通知还在）。
fn menu(rows: Vec<Row>, attention: &HashMap<String, bool>) -> Menu {
    let running = rows
        .iter()
        .filter(|row| row.status() == AgentStatus::Running)
        .count();
    let mut listed: Vec<(AgentStatus, Entry)> = rows
        .into_iter()
        .filter_map(|row| {
            let status = row.status();
            let raised = row.ids.iter().find(|id| attention.contains_key(*id));
            let requires_action = status == AgentStatus::NeedsYou
                || row.ids.iter().any(|id| attention.get(id) == Some(&true));
            if raised.is_none() && status == AgentStatus::Idle {
                return None;
            }
            // 分屏的终端跳到挂着提醒的那格，其次是在跑的那格。
            let session_id = raised
                .or_else(|| {
                    row.ids
                        .iter()
                        .zip(&row.statuses)
                        .find(|(_, own)| **own == status)
                        .map(|(id, _)| id)
                })
                .or_else(|| row.ids.first())?
                .clone();
            let (shown, text) = if requires_action {
                (AgentStatus::NeedsYou, "需要你")
            } else if status == AgentStatus::Running {
                (AgentStatus::Running, "运行中")
            } else {
                (AgentStatus::Idle, "未读")
            };
            Some((
                shown,
                Entry {
                    session_id,
                    label: format!("{} — {text}", row.title),
                    agent: row.agent,
                    color: agent_status_u32(shown),
                },
            ))
        })
        .collect();
    listed.sort_by_key(|(status, _)| status.rank());
    Menu {
        title: status_title(attention.len(), running),
        tooltip: status_tooltip(attention.len(), running),
        entries: listed.into_iter().map(|(_, entry)| entry).collect(),
    }
}

impl Menu {
    fn status_menu(&self) -> StatusMenu {
        let mut items: Vec<StatusMenuItem> = self
            .entries
            .iter()
            .map(|entry| StatusMenuItem::Action {
                label: entry.label.clone(),
                icon: Some(StatusIcon {
                    png: logo(entry.agent.as_deref()).to_vec(),
                    color: entry.color,
                }),
                command: sessions::ACTIVATE.id().to_string(),
                args: api::serde_json::to_value(Activate {
                    session_id: entry.session_id.clone(),
                })
                .unwrap_or_default(),
            })
            .collect();
        if !items.is_empty() {
            items.push(StatusMenuItem::Separator);
        }
        items.push(StatusMenuItem::Action {
            label: "打开 smelt 主窗口".to_string(),
            icon: None,
            command: api::SHOW_MAIN_WINDOW.id().to_string(),
            args: api::serde_json::Value::Null,
        });
        StatusMenu {
            title: self.title.clone(),
            tooltip: self.tooltip.clone(),
            items,
        }
    }
}

fn logo(agent: Option<&str>) -> &'static [u8] {
    agent
        .and_then(|agent| AGENT_LOGO_PNG.iter().find(|(id, _)| *id == agent))
        .map_or(TERMINAL_PNG, |(_, png)| png)
}

/// `N@·M`：N 是要你看的会话数，M 是运行中的会话数。哪项是 0 就连同分隔点一起省掉，都是 0
/// 只剩图标。
fn status_title(attention: usize, running: usize) -> String {
    match (attention, running) {
        (0, 0) => String::new(),
        (attention, 0) => format!("{attention}@"),
        (0, running) => running.to_string(),
        (attention, running) => format!("{attention}@·{running}"),
    }
}

fn status_tooltip(attention: usize, running: usize) -> String {
    match (attention, running) {
        (0, 0) => "smelt".to_string(),
        (attention, 0) => format!("{attention} 个会话需要关注"),
        (0, running) => format!("{running} 个会话运行中"),
        (attention, running) => format!("{attention} 个会话需要关注，{running} 个会话运行中"),
    }
}

#[cfg(test)]
mod tests;
