//! 会话列表的命令：[`crate::contract`] 里命令的登记和处理，一组「会话列表」。上一段 / 下一段
//! 按 `SessionList::sidebar` 的 `order()` 打开那段。

use rust_i18n::t;
use std::rc::Rc;

use gpui::{App, Task, Window};
use smelt_plugin_api::{self as api, Command, CommandSpec, Handled, RuntimeKey};

use crate::contract::{self, ListArgs, RevealInList};
use crate::model::{self, ConversationKey, SessionList};
use crate::{dialogs, ops};

/// 登记本插件的命令。`register` 里调一次。
pub(crate) fn register(cx: &mut App) {
    let specs = vec![
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
    ];
    if let Err(error) = api::register_commands(crate::OWNER, api::text!("view.list"), specs, cx) {
        eprintln!("[session-list] 命令登记失败：{error}");
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

/// 在列表里找出这段会话：展开它所在的组（搜索滤掉了就清掉搜索），列表视图滚过去、闪一下；
/// 侧栏收着时 `OPEN_VIEW` 把它打开。
fn reveal_in_list(args: RevealInList, cx: &mut App) -> Task<Result<(), String>> {
    let host = api::host(cx);
    let style = host.path_style(&args.machine, cx);
    let directory = host.workspace_directory(&args.machine, cx);
    let Some(key) =
        model::listed_session(&args.machine, &args.session_id, style, directory.as_ref())
    else {
        return Task::ready(Err(t!("error.not_in_list").into_owned()));
    };
    SessionList::global(cx).update(cx, |list, cx| {
        list.reveal(&key, cx);
        list.located = Some(key);
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
                .map(|root| ops::open_project(RuntimeKey::Local, root, cx))
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
        let list = SessionList::global(cx).read(cx);
        step(list.sidebar.order(), list.selection.session(), forward)
    };
    let Some(key) = next else {
        return Handled::ok();
    };
    let opened = ops::open_conversation(key, cx);
    Handled::Done(cx.spawn(async move |_| opened.await.map(|()| serde_json::Value::Null)))
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
