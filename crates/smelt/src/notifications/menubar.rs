//! 菜单栏和 Dock：右上角图标旁显示要你看的会话数和运行中的会话数（`N@·M`），点开列出这些
//! 会话，点一条跳过去；Dock 图标的角标是要你看的会话数。
//!
//! 会话名册（各台机器的工作区目录）、会话状态读宿主的，要你看的会话读本模块的提醒镜像；
//! 菜单和角标经 `set_status_menu`、`smelt.app/set-dock-badge` 交给宿主画（图标用 `smelt_ui`
//! 资产中的单色 SVG 路径），点菜单项执行 `smelt.sessions/activate`。主窗口关着也照常更新。
//! 算出来和上次交给宿主的一样就不再交。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use api::model::agent_kind::agent_id_from_launch;
use api::model::agent_status::{AgentStatus, terminal_agent_status};
use api::model::daemon_state::DaemonSessionState;
use api::model::workspace_directory::WorkspaceConversation;
use gpui::{App, Task};
use rust_i18n::t;
use smelt_plugin_api::{
    self as api, DockBadge, RuntimeKey, StatusIcon, StatusImage, StatusMenu, StatusMenuItem,
};
use smelt_ui::ui_theme::agent_status_u32;

use super::OWNER;
use super::sessions::{self, Activate};

/// 已有独立菜单栏单色 SVG 的 agent id（对照 `smelt_ui::assets` 里的 `smelt-icons/agent-*.svg`）。
const AGENT_ICON_IDS: &[&str] = &[
    "claude",
    "copilot",
    "codex",
    "grok",
    "antigravity",
    "cursor",
    "opencode",
    "kiro",
    "pi",
    "omp",
    "crush",
];

/// 认不出 agent 的会话（裸终端、自定义 agent）用的方框终端图标。
const TERMINAL_MENU_ICON: &str = "icons/terminal.svg";

/// 上次交给宿主的。交失败了清掉，下次事件重交。
#[derive(Default)]
pub(super) struct Sent {
    menu: Option<Menu>,
    badge: Option<DockBadge>,
}

/// 重算菜单和角标，变了才交给宿主。`attention` 是要你看的会话（所有机器的）：会话 id 到
/// 还等不等你处理。
pub(super) fn refresh(sent: &Rc<RefCell<Sent>>, attention: &HashMap<String, bool>, cx: &mut App) {
    let host = api::host(cx);
    let mut conversations = Vec::new();
    for machine in host.machines(cx) {
        conversations.extend(machine_rows(&machine, cx));
    }
    let menu = menu(conversations, attention);
    let badge = DockBadge {
        label: (!attention.is_empty()).then(|| attention.len().to_string()),
    };

    let mut last = sent.borrow_mut();
    if last.menu.as_ref() != Some(&menu) {
        api::set_status_menu(OWNER, Some(menu.status_menu()), cx);
        last.menu = Some(menu);
    }
    // 宿主命令跟着主窗口的工作台登记，启动时还没有；登记好了会发 CommandsChanged。
    if api::command(api::SET_DOCK_BADGE.id(), cx).is_none() {
        return;
    }
    if last.badge.as_ref() != Some(&badge) {
        let task = api::SET_DOCK_BADGE.run(&badge, cx);
        last.badge = Some(badge);
        forget_on_failure(task, sent.clone(), |sent| sent.badge = None, cx);
    }
}

/// 一台机器上没归档的对话。
fn machine_rows(machine: &api::Machine, cx: &App) -> Vec<Row> {
    let host = api::host(cx);
    let Some(directory) = host.workspace_directory(&machine.key, cx) else {
        return Vec::new();
    };
    let states: HashMap<String, DaemonSessionState> = host
        .session_states(&machine.key, cx)
        .into_iter()
        .map(|state| (state.id.clone(), state))
        .collect();
    let remote = (machine.key != RuntimeKey::Local).then_some(machine.name.as_str());
    directory
        .conversations
        .iter()
        .filter(|conversation| conversation.archived_at.is_none())
        .filter_map(|conversation| conversation_row(&machine.key, conversation, &states, remote))
        .collect()
}

/// 那台机器上含这个守护会话的对话叫什么，和菜单里同一个名字（远程的带机器名）。
pub(super) fn session_title(machine: &RuntimeKey, session_id: &str, cx: &App) -> Option<String> {
    let machine = api::host(cx)
        .machines(cx)
        .into_iter()
        .find(|known| known.key == *machine)?;
    machine_rows(&machine, cx)
        .into_iter()
        .find(|row| row.ids.iter().any(|id| id == session_id))
        .map(|row| row.title)
}

fn forget_on_failure(
    task: Task<Result<(), String>>,
    sent: Rc<RefCell<Sent>>,
    forget: impl FnOnce(&mut Sent) + 'static,
    cx: &mut App,
) {
    cx.spawn(async move |_| {
        if let Err(error) = task.await {
            eprintln!("[notifications] Dock 角标交给宿主失败：{error}");
            forget(&mut sent.borrow_mut());
        }
    })
    .detach();
}

/// 一段对话在菜单里要用到的。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    /// 对话在哪台机器上。
    machine: RuntimeKey,
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

/// `machine` 上一段对话的会话 id、状态、名字和 agent。一个会话都没起过的（终端还没开）是
/// `None`。`remote` 是远程机器的名字，接在名字后面。
fn conversation_row(
    machine: &RuntimeKey,
    conversation: &WorkspaceConversation,
    states: &HashMap<String, DaemonSessionState>,
    remote: Option<&str>,
) -> Option<Row> {
    let status_of = |id: &str| terminal_agent_status(states.get(id));
    // 名字运行时算好了，同一段对话里每一格报的都一样。还没推来（旧运行时）写通用名。
    let (ids, title, agent) = if let Some(acp) = &conversation.acp {
        let title = states
            .get(&acp.sid)
            .and_then(|state| state.conversation_name.clone())
            .unwrap_or_else(|| t!("notifications.fallback.conversation").into_owned());
        (vec![acp.sid.clone()], title, Some(acp.agent.clone()))
    } else {
        let started: Vec<_> = conversation
            .terminals
            .iter()
            .filter_map(|terminal| Some((terminal.id.clone()?, terminal)))
            .collect();
        // 菜单栏不知道哪格是活动的，agent 取第一格。
        let (first_id, first) = started.first()?;
        let state = states.get(first_id);
        let title = started
            .iter()
            .find_map(|(id, _)| states.get(id)?.conversation_name.clone())
            .unwrap_or_else(|| t!("notifications.fallback.terminal").into_owned());
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
        machine: machine.clone(),
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
    /// 点了跳到哪台机器上的哪个守护会话。
    machine: RuntimeKey,
    session_id: String,
    label: String,
    agent: Option<String>,
    /// 图标颜色，`0xRRGGBB`。
    color: u32,
}

/// 列出运行中、要你处理、有没看提醒的对话，要你处理的排前面；运行数按所有对话算，要你看
/// 的数按所有提醒算（对话已经关了的提醒也算，系统通知还在）。
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
                (AgentStatus::NeedsYou, t!("notifications.status.needs_you"))
            } else if status == AgentStatus::Running {
                (AgentStatus::Running, t!("notifications.status.running"))
            } else {
                (AgentStatus::Idle, t!("notifications.status.unread"))
            };
            Some((
                shown,
                Entry {
                    machine: row.machine,
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
                    png: StatusImage::Asset(logo(entry.agent.as_deref())),
                    color: entry.color,
                }),
                command: sessions::ACTIVATE.id().to_string(),
                args: api::serde_json::to_value(Activate {
                    machine: entry.machine.clone(),
                    session_id: entry.session_id.clone(),
                })
                .unwrap_or_default(),
            })
            .collect();
        if !items.is_empty() {
            items.push(StatusMenuItem::Separator);
        }
        items.push(StatusMenuItem::Action {
            label: t!("notifications.menu.show_main_window").into_owned(),
            icon: None,
            command: api::SHOW_MAIN_WINDOW.id().to_string(),
            args: api::serde_json::Value::Null,
        });
        StatusMenu {
            // 用应用图标：这个就是 Smelt 自己的会话菜单。
            icon: None,
            title: self.title.clone(),
            tooltip: self.tooltip.clone(),
            items,
        }
    }
}

fn logo(agent: Option<&str>) -> String {
    match agent {
        Some(id) if AGENT_ICON_IDS.contains(&id) => format!("smelt-icons/agent-{id}.svg"),
        _ => TERMINAL_MENU_ICON.to_string(),
    }
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
        (attention, 0) => attention_text(attention),
        (0, running) => running_text(running),
        (attention, running) => t!(
            "notifications.tooltip.both",
            attention = attention_text(attention),
            running = running_text(running)
        )
        .into_owned(),
    }
}

fn attention_text(count: usize) -> String {
    if count == 1 {
        t!("notifications.tooltip.attention.one").into_owned()
    } else {
        t!("notifications.tooltip.attention.other", count = count).into_owned()
    }
}

fn running_text(count: usize) -> String {
    if count == 1 {
        t!("notifications.tooltip.running.one").into_owned()
    } else {
        t!("notifications.tooltip.running.other", count = count).into_owned()
    }
}

#[cfg(test)]
mod tests;
