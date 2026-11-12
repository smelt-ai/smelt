//! 命令注册表：工作台级的动作。
//!
//! 快捷键、应用菜单、命令面板和插件之间互调，认的都是命令。插件按来源整组登记，id 是
//! `<来源>/<名字>`（和布局一样，一个插件替换不了别人的命令）。宿主把快捷键和菜单项都绑到
//! 同一个 gpui action [`RunCommand`]，主窗口根节点收到后按 id 查这里执行；用户改的键按
//! 命令 id 存。
//!
//! 视图里面的按键（列表上下、输入框编辑、终端 Tab）不是命令：它们的意思随焦点所在的
//! 视图变，由视图自己的 gpui action 管（JS 插件用 gpui-shell 的 `ShellAction`），不能
//! 改键，也不进命令面板。

use std::rc::Rc;

use gpui::{App, Global, KeyBindingContextPredicate, Keystroke, SharedString, Window};

use crate::HostEvent;

/// 快捷键和菜单项绑的 action：执行一条命令。键位和菜单只带 id，`args` 是 `null`；
/// [`run`] 可以带参数。
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = smelt, no_json)]
pub struct RunCommand {
    pub command: SharedString,
    pub args: serde_json::Value,
}

impl RunCommand {
    /// 不带参数执行 `command`。
    pub fn new(command: impl Into<SharedString>) -> Self {
        Self {
            command: command.into(),
            args: serde_json::Value::Null,
        }
    }
}

/// 命令处理了没有。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    Done,
    /// 眼下不适用（比如焦点在终端网格上，F7 该给终端）：按键交给下一条绑定，最后落到
    /// 视图自己的按键处理。
    Pass,
}

/// 命令的处理函数。`window` 是主窗口；参数认不出的字段忽略。出错由命令自己报给用户。
pub type CommandHandler = Rc<dyn Fn(&serde_json::Value, &mut Window, &mut App) -> Handled>;

/// 登记一条命令要给的东西。
pub struct CommandSpec {
    /// 来源内的名字，不含 `/`，如 `save`。
    pub name: String,
    /// 给人看的名字，命令面板和快捷键设置里显示。
    pub title: String,
    /// 出厂快捷键，gpui 写法的单个按键（如 `cmd-shift-p`）。要带 ⌘ 或者是 F 键，保留键
    /// 不能用；不合规的宿主装键表时不装，记日志。
    pub key: Option<String>,
    /// 快捷键在哪个 key context 里生效，gpui 谓词写法。主窗口任何地方是 `Workspace`；
    /// 视图自己挂的 context 只在焦点在它里面时生效。
    pub context: String,
    /// 出现在命令面板里。
    pub palette: bool,
    pub handler: CommandHandler,
}

/// 登记过的一条命令。
#[derive(Clone, Debug, PartialEq)]
pub struct CommandInfo {
    /// `<来源>/<名字>`。
    pub id: SharedString,
    /// 来源给人看的名字，快捷键设置按它分组。
    pub group: SharedString,
    pub title: SharedString,
    pub key: Option<SharedString>,
    pub context: SharedString,
    pub palette: bool,
}

struct Entry {
    owner: String,
    info: CommandInfo,
    handler: CommandHandler,
}

#[derive(Default)]
struct CommandRegistry {
    commands: Vec<Entry>,
}

impl Global for CommandRegistry {}

/// 按来源整组登记命令：这个来源原来的命令全换成这一组，空的就是全撤下。一条不合法整组
/// 不登记，原来的不动。来源第一次登记排在最后，再登记留在原来的位置。`group` 是来源
/// 给人看的名字。
pub fn register_commands(
    owner: &str,
    group: &str,
    specs: Vec<CommandSpec>,
    cx: &mut App,
) -> Result<(), String> {
    if owner.is_empty() || owner.contains('/') {
        return Err(format!("命令来源「{owner}」不能为空，也不能含 `/`"));
    }
    let os = cx.platform_info().os;
    let mut entries: Vec<Entry> = Vec::with_capacity(specs.len());
    for spec in specs {
        if spec.name.is_empty() || spec.name.contains('/') {
            return Err(format!("命令名「{}」不能为空，也不能含 `/`", spec.name));
        }
        let id: SharedString = format!("{owner}/{}", spec.name).into();
        if entries.iter().any(|entry| entry.info.id == id) {
            return Err(format!("命令「{id}」登记了两次"));
        }
        if spec.title.trim().is_empty() {
            return Err(format!("命令「{id}」没有标题"));
        }
        if let Some(key) = &spec.key {
            if key.split_whitespace().count() != 1 {
                return Err(format!("命令「{id}」的快捷键「{key}」只能是一个按键"));
            }
            if let Err(error) = Keystroke::parse_with_os(key, os) {
                return Err(format!("命令「{id}」的快捷键 {key} 解析不了：{error}"));
            }
        }
        if let Err(error) = KeyBindingContextPredicate::parse(&spec.context) {
            return Err(format!(
                "命令「{id}」的 context「{}」写错了：{error}",
                spec.context
            ));
        }
        entries.push(Entry {
            owner: owner.to_string(),
            info: CommandInfo {
                id,
                group: group.to_string().into(),
                title: spec.title.into(),
                key: spec.key.map(Into::into),
                context: spec.context.into(),
                palette: spec.palette,
            },
            handler: spec.handler,
        });
    }
    let commands = &mut cx.default_global::<CommandRegistry>().commands;
    let had_any = commands.iter().any(|entry| entry.owner == owner);
    if !had_any && entries.is_empty() {
        return Ok(());
    }
    let at = commands
        .iter()
        .position(|entry| entry.owner == owner)
        .unwrap_or(commands.len());
    commands.retain(|entry| entry.owner != owner);
    let at = at.min(commands.len());
    commands.splice(at..at, entries);
    // 处理函数比不了，换了就当变了。
    crate::emit(HostEvent::CommandsChanged, cx);
    Ok(())
}

/// 撤下一个来源的全部命令。
pub fn unregister_commands(owner: &str, cx: &mut App) {
    if !cx.has_global::<CommandRegistry>() {
        return;
    }
    let commands = &mut cx.global_mut::<CommandRegistry>().commands;
    let before = commands.len();
    commands.retain(|entry| entry.owner != owner);
    if commands.len() != before {
        crate::emit(HostEvent::CommandsChanged, cx);
    }
}

/// 登记过的命令，按登记顺序。只读内存，render 里可以调。
pub fn commands(cx: &App) -> Vec<CommandInfo> {
    cx.try_global::<CommandRegistry>()
        .map(|registry| {
            registry
                .commands
                .iter()
                .map(|entry| entry.info.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 一条登记过的命令；没登记是 `None`。
pub fn command(id: &str, cx: &App) -> Option<CommandInfo> {
    cx.try_global::<CommandRegistry>()?
        .commands
        .iter()
        .find(|entry| entry.info.id.as_ref() == id)
        .map(|entry| entry.info.clone())
}

/// 在主窗口里执行一条命令。宿主收到 [`RunCommand`] 时调；别处要执行命令用 [`run`]。
/// 没登记的命令（键表还没跟上注册表）算 [`Handled::Pass`]。
pub fn invoke(id: &str, args: &serde_json::Value, window: &mut Window, cx: &mut App) -> Handled {
    // 先把处理函数拿出来再调：处理函数里可能再登记命令。
    let handler = cx.try_global::<CommandRegistry>().and_then(|registry| {
        registry
            .commands
            .iter()
            .find(|entry| entry.info.id.as_ref() == id)
            .map(|entry| entry.handler.clone())
    });
    match handler {
        Some(handler) => handler(args, window, cx),
        None => {
            eprintln!("[commands] 没有登记的命令 {id}");
            Handled::Pass
        }
    }
}

/// 执行一条命令，排到这一轮更新之后、在主窗口里跑：调用方可能正嵌在宿主别的更新里。
/// 命令没登记、主窗口没开时报错。
pub fn run(id: &str, args: serde_json::Value, cx: &mut App) -> Result<(), String> {
    if command(id, cx).is_none() {
        return Err(format!("没有命令「{id}」"));
    }
    let window = crate::host(cx)
        .main_window(cx)
        .ok_or_else(|| "主窗口没有打开".to_string())?;
    let id = id.to_string();
    cx.defer(move |cx| {
        if let Err(error) = window.update(cx, |_, window, cx| invoke(&id, &args, window, cx)) {
            eprintln!("[commands] {id} 没执行：主窗口已关闭（{error}）");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, key: Option<&str>) -> CommandSpec {
        CommandSpec {
            name: name.to_string(),
            title: name.to_string(),
            key: key.map(str::to_string),
            context: "Workspace".to_string(),
            palette: true,
            handler: Rc::new(|_, _, _| Handled::Done),
        }
    }

    fn ids(cx: &App) -> Vec<String> {
        commands(cx)
            .into_iter()
            .map(|info| info.id.into())
            .collect()
    }

    #[gpui::test]
    fn commands_are_registered_per_owner_and_keep_their_place(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_commands("smelt.app", "应用", vec![spec("quit", Some("cmd-q"))], cx).unwrap();
            register_commands("trader", "交易", vec![spec("a", None), spec("b", None)], cx)
                .unwrap();
            assert_eq!(ids(cx), ["smelt.app/quit", "trader/a", "trader/b"]);
            // 再登记是整组替换，位置不变。
            register_commands("smelt.app", "应用", vec![spec("docs", None)], cx).unwrap();
            assert_eq!(ids(cx), ["smelt.app/docs", "trader/a", "trader/b"]);
            let info = command("trader/b", cx).unwrap();
            assert_eq!(info.group.as_ref(), "交易");
            assert_eq!(command("smelt.app/quit", cx), None);
            unregister_commands("trader", cx);
            assert_eq!(ids(cx), ["smelt.app/docs"]);
            register_commands("smelt.app", "应用", vec![], cx).unwrap();
            assert!(commands(cx).is_empty());
        });
    }

    #[gpui::test]
    fn an_invalid_command_rejects_the_whole_set(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_commands("trader", "交易", vec![spec("a", None)], cx).unwrap();
            let bad_sets: Vec<Vec<CommandSpec>> = vec![
                vec![spec("b", None), spec("b", None)],
                vec![spec("", None)],
                vec![spec("x/y", None)],
                vec![spec("b", Some("cmd-k cmd-s"))],
                vec![spec("b", Some("hyper-k"))],
                vec![CommandSpec {
                    context: "Workspace &&".to_string(),
                    ..spec("b", None)
                }],
                vec![CommandSpec {
                    title: " ".to_string(),
                    ..spec("b", None)
                }],
            ];
            for bad in bad_sets {
                assert!(register_commands("trader", "交易", bad, cx).is_err());
            }
            assert!(register_commands("a/b", "x", vec![spec("a", None)], cx).is_err());
            assert!(register_commands("", "x", vec![spec("a", None)], cx).is_err());
            assert_eq!(ids(cx), ["trader/a"]);
        });
    }

    #[gpui::test]
    fn invoking_runs_the_handler_and_unknown_commands_pass(cx: &mut gpui::TestAppContext) {
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
            let seen = calls.clone();
            register_commands(
                "trader",
                "交易",
                vec![CommandSpec {
                    handler: Rc::new(move |args, _, _| {
                        seen.borrow_mut().push(args.clone());
                        Handled::Done
                    }),
                    ..spec("buy", None)
                }],
                cx,
            )
            .unwrap();
            let args = serde_json::json!({ "qty": 2 });
            assert_eq!(invoke("trader/buy", &args, window, cx), Handled::Done);
            assert_eq!(invoke("trader/sell", &args, window, cx), Handled::Pass);
            assert_eq!(*calls.borrow(), [args]);
        });
    }

    #[test]
    fn key_bindings_match_commands_by_id() {
        assert_eq!(RunCommand::new("a/b"), RunCommand::new("a/b"));
        assert_ne!(RunCommand::new("a/b"), RunCommand::new("a/c"));
    }
}
