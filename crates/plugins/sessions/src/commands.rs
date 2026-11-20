//! 会话插件的命令（S5）：[`crate::contract`] 里所有命令的登记和处理，一组「会话」（同一
//! 来源只能登记一组，分屏的几条也在里面）。旧宿主 `session_commands.rs`、
//! `workspace_commands.rs` 里会话那部分搬到这里。
//!
//! 处理只做参数到各包函数的转接：去某段会话是打开会话视图（`lifecycle::open_session`），
//! 新建 `lifecycle::create_conversation`，对话框 `dialogs::*`，分屏
//! `session_view::{split_pane, cycle_pane, close_pane}`，上一段 / 下一段按
//! `Sessions::sidebar` 的 `order()` 打开那段。新会话在目录里那条怎么拼在 [`launch`]（纯函数）。

mod launch;

use rust_i18n::t;
use std::rc::Rc;

use gpui::{App, Axis, ClipboardItem, Task, Window};
use smelt_plugin_api::client::protocol::control::EmptyParams;
use smelt_plugin_api::model::agent_host::AgentHostSettings;
use smelt_plugin_api::model::agent_kind::HistorySourceKind;
use smelt_plugin_api::model::control_api::LaunchGet;
use smelt_plugin_api::model::daemon_state::DaemonSessionState;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};
use smelt_plugin_api::{self as api, Command, CommandSpec, Handled, RuntimeKey};

use crate::contract::{
    self, Activate, Fork, Handoff, ListArgs, Resume, RevealInList, SendText, SessionArgs,
    SessionAt, StartConversation,
};
use crate::model::{self, ConversationKey, Located, Sessions};
use crate::peers::agents::AGENT_SETTINGS;
use crate::peers::conversation::{ConversationFork, ConversationStart, ConversationText};
use crate::peers::terminal::TerminalText;
use crate::session_view::PaneKind;
use crate::{dialogs, lifecycle, peers, session_view};

/// 登记本插件的命令。`register` 里调一次。
pub(crate) fn register(cx: &mut App) {
    let specs = vec![
        contract::ACTIVATE.spec_async(api::text!("command.activate"), activate),
        contract::START_CONVERSATION
            .spec_async(api::text!("command.start_conversation"), start_conversation),
        contract::FORK.spec_async(api::text!("command.fork"), fork),
        contract::RESUME.spec_async(api::text!("command.resume"), resume),
        contract::RESUME_IN_TERMINAL
            .spec_async(api::text!("command.resume_in_terminal"), resume_in_terminal),
        contract::HANDOFF.spec_async(api::text!("command.handoff"), handoff),
        contract::SEND_TEXT.spec_async(api::text!("command.send_text"), send_text),
        contract::REVEAL_IN_LIST.spec_async(api::text!("command.reveal_in_list"), |args, _, cx| {
            reveal_in_list(args, cx)
        }),
        shortcut(
            contract::NEW_SESSION,
            api::text!("menu.new_session"),
            None,
            |window, cx| {
                dialogs::open_new_session(window, cx);
                Handled::ok()
            },
        ),
        shortcut(
            contract::OPEN_PROJECT,
            api::text!("command.open_project"),
            None,
            |_, cx| open_project(cx),
        ),
        // ⌘⇧E 跟 VS Code「聚焦资源管理器」一样。
        shortcut(
            contract::FOCUS_LIST,
            api::text!("command.focus_list"),
            Some("cmd-shift-e"),
            |_, cx| focus_list(cx),
        ),
        // 绑在 Workspace 上比输入框浅：焦点在输入框里时是文首 / 文末。
        shortcut(
            contract::PREV_SESSION,
            api::text!("command.prev_session"),
            Some("cmd-up"),
            |_, cx| step_session(false, cx),
        ),
        shortcut(
            contract::NEXT_SESSION,
            api::text!("command.next_session"),
            Some("cmd-down"),
            |_, cx| step_session(true, cx),
        ),
        // 输入框的缩进绑在更深的 context 上，焦点在输入框里时由它接。
        shortcut(
            contract::PREV_PANE,
            api::text!("command.prev_pane"),
            Some("cmd-["),
            |window, cx| {
                on_pane(window, cx, |window, cx| {
                    session_view::cycle_pane(false, window, cx)
                })
            },
        ),
        shortcut(
            contract::NEXT_PANE,
            api::text!("command.next_pane"),
            Some("cmd-]"),
            |window, cx| {
                on_pane(window, cx, |window, cx| {
                    session_view::cycle_pane(true, window, cx)
                })
            },
        ),
        shortcut(
            contract::SPLIT_RIGHT,
            api::text!("command.split_right"),
            Some("cmd-d"),
            |window, cx| {
                on_pane(window, cx, |window, cx| {
                    session_view::split_pane(Axis::Horizontal, window, cx)
                })
            },
        ),
        shortcut(
            contract::SPLIT_DOWN,
            api::text!("command.split_down"),
            Some("shift-cmd-d"),
            |window, cx| {
                on_pane(window, cx, |window, cx| {
                    session_view::split_pane(Axis::Vertical, window, cx)
                })
            },
        ),
        shortcut(
            contract::CLOSE_PANE,
            api::text!("command.close_pane"),
            Some("cmd-w"),
            |window, cx| on_pane(window, cx, session_view::close_pane),
        ),
        shortcut(
            contract::COPY_SESSION_ID,
            api::text!("menu.copy_session_id"),
            None,
            |_, cx| Handled::result(copy_active_session_id(cx)),
        ),
    ];
    if let Err(error) = api::register_commands(crate::OWNER, api::text!("view.session"), specs, cx)
    {
        eprintln!("[sessions] 命令登记失败：{error}");
    }
}

/// 不带参数的命令：进命令面板，可以绑键，在主窗口任何地方生效。都要主窗口。
fn shortcut(
    command: Command<(), ()>,
    title: api::Text,
    key: Option<&str>,
    run: impl Fn(&mut Window, &mut App) -> Handled + 'static,
) -> CommandSpec {
    CommandSpec {
        name: command.name().to_string(),
        title,
        key: key.map(str::to_string),
        context: "Workspace".to_string(),
        palette: true,
        requires_args: false,
        args: Some(Vec::new()),
        cli: false,
        handler: Rc::new(
            move |_: &serde_json::Value, window: Option<&mut Window>, cx: &mut App| match window {
                Some(window) => run(window, cx),
                None => Handled::err(api::no_main_window()),
            },
        ),
    }
}

// ---- 带参数的命令 ----

fn activate(args: Activate, _: Option<&mut Window>, cx: &mut App) -> Task<Result<(), String>> {
    lifecycle::open_session(
        SessionArgs {
            machine: args.machine,
            at: SessionAt::Session {
                session_id: args.session_id,
            },
            after: args.after,
        },
        cx,
    )
}

fn start_conversation(
    args: StartConversation,
    _: Option<&mut Window>,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let StartConversation { machine, cwd, with } = args;
    let target = machine.clone();
    with_settings(&machine, cx, move |settings, cx| {
        let acp = launch::started_acp(settings, &with, cwd, new_acp_id())?;
        Ok(create_acp(
            target,
            acp,
            Some(ConversationStart::default()),
            cx,
        ))
    })
}

/// 从一段对话分叉：照它的 agent、工作目录、启动命令和选项开一段新的，历史由对话插件拉起
/// 时从 `provider_session_id` 复制（切到 `cut` 之前），`prompt` 预填进输入框。
fn fork(args: Fork, _: Option<&mut Window>, cx: &mut App) -> Task<Result<(), String>> {
    let Some(source) = api::host(cx)
        .workspace_directory(&args.machine, cx)
        .and_then(|directory| directory.conversation_of_session(&args.session_id).cloned())
    else {
        return Task::ready(Err(t!("error.fork_source_missing").into_owned()));
    };
    let Some(acp) = source.acp.as_ref().filter(|acp| acp.sid == args.session_id) else {
        return Task::ready(Err(t!("error.fork_only_conversation").into_owned()));
    };
    let conversation = WorkspaceConversation {
        acp: Some(launch::forked_acp(acp, new_acp_id())),
        ..launch::blank_conversation(new_id(), source.workset_id.clone())
    };
    let start = ConversationStart {
        fork: Some(ConversationFork {
            provider_session_id: args.provider_session_id,
            cut: args.cut,
        }),
        prefill: args.prompt,
    };
    let created = lifecycle::create_conversation(args.machine, conversation, Some(start), cx);
    done(created, cx)
}

/// 续接一段历史会话。侧栏里已经有同一段的跳过去，没有就新开一段 `session/load` 它。
fn resume(args: Resume, _: Option<&mut Window>, cx: &mut App) -> Task<Result<(), String>> {
    let Some(kind) = HistorySourceKind::from_id(&args.agent).and_then(HistorySourceKind::acp)
    else {
        return Task::ready(Err(
            t!("error.no_acp_resume", agent = args.agent).into_owned()
        ));
    };
    let host = api::host(cx);
    let style = host.path_style(&args.machine, cx);
    let open = host
        .workspace_directory(&args.machine, cx)
        .and_then(|directory| launch::resumed(style, &directory, kind, &args.cwd, &args.resume_id));
    if let Some(id) = open {
        return lifecycle::open_conversation(
            ConversationKey {
                machine: args.machine,
                id,
            },
            cx,
        );
    }
    let machine = args.machine.clone();
    with_settings(&machine, cx, move |settings, cx| {
        let acp = launch::resumed_acp(settings, &args, new_acp_id())?;
        Ok(create_acp(
            args.machine,
            acp,
            Some(ConversationStart::default()),
            cx,
        ))
    })
}

/// 在终端里续接一段历史会话：起这家的 CLI 带上续接参数。启动项、环境变量都按存档所在
/// 那台机器的。
fn resume_in_terminal(
    args: Resume,
    _: Option<&mut Window>,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let Some(agent) = HistorySourceKind::from_id(&args.agent) else {
        return Task::ready(Err(
            t!("error.unknown_history_source", source = args.agent).into_owned()
        ));
    };
    if agent.cli_resume_syntax().is_none() {
        return Task::ready(Err(
            t!("error.no_cli_resume", agent = agent.short_label()).into_owned()
        ));
    }
    let entries =
        api::call::<LaunchGet>(&args.machine, EmptyParams::default(), api::CALL_TIMEOUT, cx);
    // 自定义 Agent 的环境变量（数据目录在里面，CLI 靠它找到存档）。
    let settings = args
        .profile_id
        .is_some()
        .then(|| AGENT_SETTINGS.run(&(), cx));
    cx.spawn(async move |cx| {
        let entries = entries.await?.entries;
        let all = match settings {
            Some(settings) => Some(settings.await?),
            None => None,
        };
        let ran = cx.update(|cx| {
            let env = match (all, args.profile_id.as_deref()) {
                (Some(all), Some(profile_id)) => {
                    let settings = launch::settings_of(all, &args.machine)?;
                    let kind = agent.acp().ok_or_else(|| {
                        t!("error.no_custom_agent", agent = agent.short_label()).into_owned()
                    })?;
                    launch::engine_launch(&settings, kind, Some(profile_id))?.env
                }
                _ => Default::default(),
            };
            let style = api::host(cx).path_style(&args.machine, cx);
            let command = launch::cli_resume_line(style, &entries, agent, &args.resume_id, &env)
                .ok_or_else(|| {
                    t!("error.no_cli_resume", agent = agent.short_label()).into_owned()
                })?;
            let terminal = WorkspaceTerminal {
                id: Some(new_id()),
                cwd: Some(args.cwd.clone()),
                custom_title: None,
                launch_label: Some(format!("{} TUI", agent.short_label())),
                launch_cmd: Some(command),
            };
            let workset = workset_of(&args.machine, &args.cwd, cx);
            let conversation = WorkspaceConversation {
                terminals: vec![terminal],
                ..launch::blank_conversation(new_id(), workset)
            };
            Ok::<_, String>(lifecycle::create_conversation(
                args.machine,
                conversation,
                None,
                cx,
            ))
        })?;
        ran.await.map(|_| ())
    })
}

/// 把一段历史交接给另一个 agent：目标从零开始，第一句话是交接说明。
fn handoff(args: Handoff, _: Option<&mut Window>, cx: &mut App) -> Task<Result<(), String>> {
    let machine = args.machine.clone();
    with_settings(&machine, cx, move |settings, cx| {
        let acp = launch::handoff_acp(settings, &args, new_acp_id())?;
        Ok(create_acp(
            args.machine,
            acp,
            Some(ConversationStart::default()),
            cx,
        ))
    })
}

/// 把文字填进眼下那一格：终端里等于粘贴，对话里填进输入框，都不替用户发出。
fn send_text(args: SendText, _: Option<&mut Window>, cx: &mut App) -> Task<Result<(), String>> {
    let Some((key, pane)) = active_pane(cx) else {
        return Task::ready(Err(t!("error.send_no_session").into_owned()));
    };
    match pane.kind {
        PaneKind::Terminal => peers::terminal::SEND_TEXT.run(
            &TerminalText {
                machine: key.machine,
                session_id: pane.session_id,
                text: args.text,
            },
            cx,
        ),
        PaneKind::Conversation => peers::conversation::INSERT_TEXT.run(
            &ConversationText {
                machine: key.machine,
                session_id: pane.session_id,
                text: args.text,
            },
            cx,
        ),
    }
}

// ---- 不带参数的命令 ----

/// 在本机选一个或几个目录当项目打开。没选是撤销，不算失败。
fn open_project(cx: &mut App) -> Handled {
    let picked = api::PICK_PATHS.run(
        &api::PathRequest {
            machine: RuntimeKey::Local,
            title: t!("pick.title").into_owned(),
            confirm: t!("pick.confirm").into_owned(),
            files: false,
            multiple: true,
        },
        cx,
    );
    Handled::Done(cx.spawn(async move |cx| {
        let roots = picked.await?.unwrap_or_default();
        let opening = cx.update(|cx| {
            roots
                .into_iter()
                .map(|root| lifecycle::open_project(RuntimeKey::Local, root, cx))
                .collect::<Vec<_>>()
        });
        let mut failed = Vec::new();
        for opened in opening {
            if let Err(error) = opened.await {
                failed.push(error);
            }
        }
        if failed.is_empty() {
            Ok(serde_json::Value::Null)
        } else {
            Err(failed.join("；"))
        }
    }))
}

/// 焦点交给会话列表，列表被挡着就先让它露出来。
fn reveal_in_list(args: RevealInList, cx: &mut App) -> Task<Result<(), String>> {
    let host = api::host(cx);
    let style = host.path_style(&args.machine, cx);
    let directory = host.workspace_directory(&args.machine, cx);
    let at = SessionAt::Session {
        session_id: args.session_id,
    };
    match model::locate(&args.machine, &at, None, style, directory.as_ref()) {
        Located::Found(key) => locate_in_list(key, cx),
        Located::Wait | Located::Missing => Task::ready(Err(t!("error.not_in_list").into_owned())),
    }
}

/// 在列表里找出这段会话：展开它所在的组（搜索滤掉了就清掉搜索），列表视图滚过去、闪一下；
/// 侧栏收着时 `OPEN_VIEW` 把它打开。
pub(crate) fn locate_in_list(key: ConversationKey, cx: &mut App) -> Task<Result<(), String>> {
    Sessions::global(cx).update(cx, |sessions, cx| {
        sessions.reveal(&key, cx);
        sessions.located = Some(key);
        cx.notify();
    });
    let opened = api::OPEN_VIEW.run(
        &api::OpenView {
            view: contract::LIST.to_string(),
            args: serde_json::Value::Null,
        },
        cx,
    );
    cx.spawn(async move |_| {
        if opened.await? {
            Ok(())
        } else {
            Err(t!("error.no_room_list").into_owned())
        }
    })
}

fn focus_list(cx: &mut App) -> Handled {
    let args = match serde_json::to_value(ListArgs { focus: true }) {
        Ok(args) => args,
        Err(error) => return Handled::err(error.to_string()),
    };
    let opened = api::OPEN_VIEW.run(
        &api::OpenView {
            view: contract::LIST.to_string(),
            args,
        },
        cx,
    );
    Handled::Done(cx.spawn(async move |_| {
        if opened.await? {
            Ok(serde_json::Value::Null)
        } else {
            Err(t!("error.no_room_list").into_owned())
        }
    }))
}

/// 按列表上的顺序切到上一段 / 下一段会话，到头绕回。打开会话视图：出口里放着别的页时，
/// 会话视图先露出来再选中、给焦点。
fn step_session(forward: bool, cx: &mut App) -> Handled {
    let next = {
        let sessions = Sessions::global(cx).read(cx);
        step(
            sessions.sidebar.order(),
            sessions.selection.session(),
            forward,
        )
    };
    let Some(key) = next else {
        return Handled::ok();
    };
    let opened = lifecycle::open_conversation(key, cx);
    Handled::Done(cx.spawn(async move |_| opened.await.map(|()| serde_json::Value::Null)))
}

/// 分屏的命令只作用在露着的那段会话上：会话视图没露、没有选中的会话时不接，按键交给
/// 下一条绑定（⌘W 不能悄悄关掉看不见的会话）。
fn on_pane(
    window: &mut Window,
    cx: &mut App,
    run: impl FnOnce(&mut Window, &mut App) -> Result<(), String>,
) -> Handled {
    let selected = Sessions::global(cx).read(cx).selection.session().is_some();
    if !selected || !api::host(cx).view_shown(contract::SESSION, cx) {
        return Handled::Pass;
    }
    Handled::result(run(window, cx))
}

fn copy_active_session_id(cx: &mut App) -> Result<(), String> {
    let (key, pane) = active_pane(cx).ok_or_else(|| t!("error.no_open_session").into_owned())?;
    copy_session_id(&key.machine, &pane.session_id, cx);
    Ok(())
}

/// 复制一格的会话 id（见 [`id_to_copy`]）。会话列表行的右键菜单也用。
pub(crate) fn copy_session_id(machine: &RuntimeKey, session_id: &str, cx: &mut App) {
    let host = api::host(cx);
    let directory = host.workspace_directory(machine, cx);
    let states = host.session_states(machine, cx);
    let id = id_to_copy(directory.as_ref(), &states, session_id);
    cx.write_to_clipboard(ClipboardItem::new_string(id));
}

// ---- 共用 ----

/// 选中的会话和它眼下那一格（会话视图推来的那份，见 `Sessions::active_pane`）。
fn active_pane(cx: &App) -> Option<(ConversationKey, session_view::Pane)> {
    let sessions = Sessions::global(cx).read(cx);
    let key = sessions.selection.session()?;
    sessions.active_pane.clone().filter(|(on, _)| on == key)
}

/// 取到 `machine` 的智能体配置后接着 `build`（它交回的任务是写目录、跳过去）。
fn with_settings(
    machine: &RuntimeKey,
    cx: &mut App,
    build: impl FnOnce(&AgentHostSettings, &mut App) -> Result<Task<Result<(), String>>, String>
    + 'static,
) -> Task<Result<(), String>> {
    let all = AGENT_SETTINGS.run(&(), cx);
    let machine = machine.clone();
    cx.spawn(async move |cx| {
        let all = all.await?;
        let ran = cx.update(|cx| {
            let settings = launch::settings_of(all, &machine)?;
            build(&settings, cx)
        })?;
        ran.await
    })
}

/// 把一段对话写进那台机器的目录、跳过去。工作集按它的工作目录定。
fn create_acp(
    machine: RuntimeKey,
    acp: WorkspaceAcp,
    start: Option<ConversationStart>,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let workset = acp
        .cwd
        .as_deref()
        .and_then(|cwd| workset_of(&machine, cwd, cx));
    let conversation = WorkspaceConversation {
        acp: Some(acp),
        ..launch::blank_conversation(new_id(), workset)
    };
    let created = lifecycle::create_conversation(machine, conversation, start, cx);
    done(created, cx)
}

fn workset_of(machine: &RuntimeKey, cwd: &str, cx: &App) -> Option<String> {
    let host = api::host(cx);
    let directory = host.workspace_directory(machine, cx)?;
    launch::workset_of(host.path_style(machine, cx), &directory, cwd)
}

/// 写进目录、跳过去了就算做完，不交回会话的身份。
fn done(created: Task<Result<ConversationKey, String>>, cx: &mut App) -> Task<Result<(), String>> {
    cx.spawn(async move |_| created.await.map(|_| ()))
}

/// 目录里的对话 id、守护会话 id 都由这边先分好。
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// ACP 会话的守护 id 统一带 `acp-` 前缀，和 agent 原生会话 id 及终端 id 区分开。
fn new_acp_id() -> String {
    format!("acp-{}", uuid::Uuid::new_v4())
}

/// 复制会话 id 时给的：agent 自己的对话 id（续接身份、或 hook 报来的那段对话），拿去
/// `--resume`、找存档才用得上；agent 还没报上来时是守护会话 id。握手、TUI 里 `/resume`
/// 都会换 id，所以用时现取。
fn id_to_copy(
    directory: Option<&WorkspaceDirectory>,
    states: &[DaemonSessionState],
    session_id: &str,
) -> String {
    let history = directory
        .and_then(|directory| directory.conversation_of_session(session_id))
        .and_then(|conversation| conversation.acp.as_ref())
        .filter(|acp| acp.sid == session_id)
        .and_then(|acp| acp.history_session_id.as_deref());
    let reported = || {
        states
            .iter()
            .find(|state| state.id == session_id)
            .and_then(|state| state.conversation_id.as_deref())
    };
    history
        .or_else(reported)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(session_id)
        .to_string()
}

/// 上一段 / 下一段：按列表上从上到下的顺序，到头绕回。还没选中的（或选中的已经不在列表
/// 里），下一段是第一段，上一段是最后一段。列表是空的是 `None`。
fn step(
    order: &[ConversationKey],
    current: Option<&ConversationKey>,
    forward: bool,
) -> Option<ConversationKey> {
    let count = order.len();
    if count == 0 {
        return None;
    }
    let at = current.and_then(|current| order.iter().position(|key| key == current));
    let next = match (at, forward) {
        (Some(at), true) => (at + 1) % count,
        (Some(at), false) => (at + count - 1) % count,
        (None, true) => 0,
        (None, false) => count - 1,
    };
    Some(order[next].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copied_id_is_the_agents_own_when_known() {
        let directory = WorkspaceDirectory {
            conversations: vec![
                WorkspaceConversation {
                    id: "c".into(),
                    acp: Some(WorkspaceAcp {
                        sid: "acp-1".into(),
                        history_session_id: Some("h1".into()),
                        ..WorkspaceAcp::default()
                    }),
                    ..WorkspaceConversation::default()
                },
                WorkspaceConversation {
                    id: "t".into(),
                    terminals: vec![WorkspaceTerminal {
                        id: Some("term-1".into()),
                        ..WorkspaceTerminal::default()
                    }],
                    ..WorkspaceConversation::default()
                },
            ],
            ..WorkspaceDirectory::default()
        };
        let reported: DaemonSessionState = serde_json::from_value(serde_json::json!({
            "id": "term-1",
            "conversation_id": " tui-9 ",
        }))
        .unwrap();
        assert_eq!(id_to_copy(Some(&directory), &[], "acp-1"), "h1");
        assert_eq!(id_to_copy(Some(&directory), &[reported], "term-1"), "tui-9");
        assert_eq!(id_to_copy(Some(&directory), &[], "term-1"), "term-1");
        assert_eq!(id_to_copy(None, &[], "acp-1"), "acp-1");
        let acp_id = new_acp_id();
        assert!(acp_id.starts_with("acp-"), "{acp_id}");
        assert!(uuid::Uuid::parse_str(acp_id.trim_start_matches("acp-")).is_ok());
    }

    #[test]
    fn stepping_wraps_around_the_list() {
        let key = |id: &str| ConversationKey {
            machine: RuntimeKey::Local,
            id: id.into(),
        };
        let order = [key("a"), key("b"), key("c")];
        assert_eq!(step(&order, Some(&key("c")), true), Some(key("a")));
        assert_eq!(step(&order, Some(&key("a")), false), Some(key("c")));
        assert_eq!(step(&order, Some(&key("a")), true), Some(key("b")));
        assert_eq!(step(&order, None, true), Some(key("a")));
        assert_eq!(step(&order, Some(&key("gone")), false), Some(key("c")));
        assert_eq!(step(&[], None, true), None);
    }
}
