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

use std::marker::PhantomData;
use std::rc::Rc;

use futures_channel::oneshot;
use gpui::{
    AnyWindowHandle, App, Global, KeyBindingContextPredicate, Keystroke, SharedString, Task, Window,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

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

/// 命令的结果：成功时可以带一个值交回调用方（没有就是 `null`），失败是可以直接给人看
/// 的原因。
pub type CommandResult = Result<serde_json::Value, String>;

/// 命令处理了没有。
pub enum Handled {
    /// 接了。结果可能要等一会儿才有（等人在选择框里选），所以是个任务；马上就有的用
    /// [`Handled::ok`]、[`Handled::err`]、[`Handled::result`]。任务被扔掉就是撤销，执行
    /// 的一方要等它完（[`run`] 会等，宿主执行键位、菜单、命令面板时也会等，失败由宿主统一
    /// 报给用户）。
    Done(Task<CommandResult>),
    /// 眼下不适用（比如焦点在终端网格上，F7 该给终端）：按键交给下一条绑定，最后落到
    /// 视图自己的按键处理；用 [`run`] 执行时算失败。
    Pass,
}

impl Handled {
    /// 做完了，没有值要交回。
    pub fn ok() -> Self {
        Self::Done(Task::ready(Ok(serde_json::Value::Null)))
    }

    /// 没做成，`message` 是给人看的原因。
    pub fn err(message: impl Into<String>) -> Self {
        Self::Done(Task::ready(Err(message.into())))
    }

    /// 马上就有的结果；值转成 JSON 交回。
    pub fn result<T: Serialize>(result: Result<T, String>) -> Self {
        Self::Done(Task::ready(result.and_then(to_json)))
    }

    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// 命令的处理函数。`window` 是主窗口，主窗口没开时是 `None`：用不到窗口的命令（发通知、
/// 打开文件）照样做，用得到的交回 [`NO_MAIN_WINDOW`]。参数认不出的字段忽略。出错放进结果
/// 交回，不自己报。
pub type CommandHandler = Rc<dyn Fn(&serde_json::Value, Option<&mut Window>, &mut App) -> Handled>;

/// 命令要主窗口、主窗口却没开时交回的原因。
pub const NO_MAIN_WINDOW: &str = "主窗口没有打开";

/// 一条带类型的命令：id，以及参数 `A`、结果 `R` 的形状。宿主和插件之间互调时两头都用它，
/// 参数和结果由编译器查；JS 插件按同样形状的 JSON 调。
pub struct Command<A, R> {
    id: &'static str,
    types: PhantomData<fn(A) -> R>,
}

impl<A, R> Clone for Command<A, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A, R> Copy for Command<A, R> {}

impl<A, R> Command<A, R> {
    /// `id` 是 `<来源>/<名字>`。
    pub const fn new(id: &'static str) -> Self {
        Self {
            id,
            types: PhantomData,
        }
    }

    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// 来源内的名字，登记时用。
    pub fn name(&self) -> &'static str {
        self.id.split_once('/').map_or(self.id, |(_, name)| name)
    }
}

impl<A: Serialize, R: DeserializeOwned + 'static> Command<A, R> {
    /// 执行，见 [`run`]。结果转回 `R`。
    pub fn run(&self, args: &A, cx: &mut App) -> Task<Result<R, String>> {
        let id = self.id;
        let args = match serde_json::to_value(args) {
            Ok(args) => args,
            Err(error) => {
                return Task::ready(Err(format!("命令「{id}」的参数转不成 JSON：{error}")));
            }
        };
        let result = run(id, args, cx);
        cx.spawn(async move |_| {
            let value = result.await?;
            serde_json::from_value(value)
                .map_err(|error| format!("命令「{id}」交回的结果认不出：{error}"))
        })
    }
}

impl<A: DeserializeOwned + 'static, R: Serialize + 'static> Command<A, R> {
    /// 登记这条命令用的 [`CommandSpec`]：要带参数（不进命令面板、不绑键）。参数解析不了时
    /// 把原因交回调用方，`handler` 不会被调。
    pub fn spec(
        &self,
        title: impl Into<String>,
        handler: impl Fn(A, Option<&mut Window>, &mut App) -> Result<R, String> + 'static,
    ) -> CommandSpec {
        self.spec_with(title, move |args, window, cx| {
            Handled::result(handler(args, window, cx))
        })
    }

    /// 同 [`Self::spec`]，结果要等一会儿才有（比如等人在选择框里选）。
    pub fn spec_async(
        &self,
        title: impl Into<String>,
        handler: impl Fn(A, Option<&mut Window>, &mut App) -> Task<Result<R, String>> + 'static,
    ) -> CommandSpec {
        self.spec_with(title, move |args, window, cx| {
            let result = handler(args, window, cx);
            Handled::Done(cx.spawn(async move |_| result.await.and_then(to_json)))
        })
    }

    fn spec_with(
        &self,
        title: impl Into<String>,
        handler: impl Fn(A, Option<&mut Window>, &mut App) -> Handled + 'static,
    ) -> CommandSpec {
        let id = self.id;
        CommandSpec {
            name: self.name().to_string(),
            title: title.into(),
            key: None,
            context: "Workspace".to_string(),
            palette: false,
            requires_args: true,
            handler: Rc::new(move |args, window, cx| {
                match serde_json::from_value::<A>(args.clone()) {
                    Ok(args) => handler(args, window, cx),
                    Err(error) => Handled::err(format!("命令「{id}」的参数不对：{error}")),
                }
            }),
        }
    }
}

fn to_json<T: Serialize>(value: T) -> CommandResult {
    serde_json::to_value(value).map_err(|error| format!("命令的结果转不成 JSON：{error}"))
}

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
    /// 要带参数才能执行：只给代码用 [`crate::run`] 调，不进命令面板、不绑键，也不在快捷键
    /// 设置里出现。这种命令 `key` 要空、`palette` 要关。
    pub requires_args: bool,
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
    pub requires_args: bool,
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
        if spec.requires_args && (spec.key.is_some() || spec.palette) {
            return Err(format!(
                "命令「{id}」要带参数才能执行，不能有快捷键，也不能进命令面板"
            ));
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
                requires_args: spec.requires_args,
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

/// 执行一条命令，`window` 是主窗口（没开时 `None`）。宿主收到 [`RunCommand`] 时调；别处
/// 要执行命令用 [`run`]。没登记的命令（键表还没跟上注册表）算 [`Handled::Pass`]。
pub fn invoke(
    id: &str,
    args: &serde_json::Value,
    window: Option<&mut Window>,
    cx: &mut App,
) -> Handled {
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

/// 执行一条命令，结果用任务交回。排到这一轮更新之后跑：调用方可能正嵌在宿主别的更新
/// 里。主窗口开着就在它里面跑，没开也照样执行（见 [`CommandHandler`]）。命令照样跑完，
/// 不管调用方要不要结果（返回的任务扔掉也不撤销）。命令没登记、眼下不适用
/// （[`Handled::Pass`]）都算失败。
pub fn run(id: &str, args: serde_json::Value, cx: &mut App) -> Task<CommandResult> {
    let window = crate::host(cx).main_window(cx);
    run_in(window, id, args, cx)
}

fn run_in(
    window: Option<AnyWindowHandle>,
    id: &str,
    args: serde_json::Value,
    cx: &mut App,
) -> Task<CommandResult> {
    if command(id, cx).is_none() {
        return Task::ready(Err(format!("没有命令「{id}」")));
    }
    let (sender, receiver) = oneshot::channel();
    let id = id.to_string();
    cx.spawn(async move |cx| {
        let handled = cx.update(|cx| {
            // 排队期间主窗口关了，就当没开。
            window
                .and_then(|window| {
                    window
                        .update(cx, |_, window, cx| invoke(&id, &args, Some(window), cx))
                        .ok()
                })
                .unwrap_or_else(|| invoke(&id, &args, None, cx))
        });
        let result = match handled {
            Handled::Pass => Err(format!("命令「{id}」眼下不适用")),
            Handled::Done(task) => task.await,
        };
        let _ = sender.send(result);
    })
    .detach();
    cx.spawn(async move |_| {
        receiver
            .await
            .unwrap_or_else(|_| Err("命令没有执行完".to_string()))
    })
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
            requires_args: false,
            handler: Rc::new(|_, _, _| Handled::ok()),
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
                // 要参数的命令没法从键、面板执行。
                vec![CommandSpec {
                    requires_args: true,
                    ..spec("b", Some("cmd-k"))
                }],
                vec![CommandSpec {
                    requires_args: true,
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

    /// 执行一条命令，等它的结果；`Pass` 是 `None`。
    async fn invoke_and_wait(
        window: &mut gpui::VisualTestContext,
        id: &str,
        args: serde_json::Value,
    ) -> Option<CommandResult> {
        let handled = window.update(|window, cx| invoke(id, &args, Some(window), cx));
        match handled {
            Handled::Done(task) => Some(task.await),
            Handled::Pass => None,
        }
    }

    #[gpui::test]
    async fn invoking_runs_the_handler_and_unknown_commands_pass(cx: &mut gpui::TestAppContext) {
        let window = cx.add_empty_window();
        let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = calls.clone();
        window.update(|_, cx| {
            register_commands(
                "trader",
                "交易",
                vec![CommandSpec {
                    handler: Rc::new(move |args, _, _| {
                        seen.borrow_mut().push(args.clone());
                        Handled::result(Ok(7))
                    }),
                    ..spec("buy", None)
                }],
                cx,
            )
            .unwrap();
        });
        let args = serde_json::json!({ "qty": 2 });
        assert_eq!(
            invoke_and_wait(window, "trader/buy", args.clone()).await,
            Some(Ok(serde_json::json!(7)))
        );
        assert_eq!(
            invoke_and_wait(window, "trader/sell", args.clone()).await,
            None
        );
        assert_eq!(*calls.borrow(), [args]);
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Order {
        qty: u32,
    }

    const BUY: Command<Order, u32> = Command::new("trader/buy");
    const QUOTE: Command<Order, Option<u32>> = Command::new("trader/quote");

    #[gpui::test]
    async fn typed_commands_check_args_and_hand_back_results(cx: &mut gpui::TestAppContext) {
        let window = cx.add_empty_window();
        window.update(|_, cx| {
            register_commands(
                "trader",
                "交易",
                vec![
                    BUY.spec("买", |order, _, _| match order.qty {
                        0 => Err("买不了 0 股".to_string()),
                        qty => Ok(qty * 10),
                    }),
                    QUOTE.spec_async("报价", |order, _, cx| {
                        cx.spawn(async move |_| Ok((order.qty > 1).then_some(order.qty)))
                    }),
                ],
                cx,
            )
            .unwrap();
            let info = command(BUY.id(), cx).unwrap();
            assert!(info.requires_args && !info.palette && info.key.is_none());
        });
        assert_eq!(BUY.name(), "buy");
        assert_eq!(
            invoke_and_wait(window, BUY.id(), serde_json::json!({ "qty": 3 })).await,
            Some(Ok(serde_json::json!(30)))
        );
        assert_eq!(
            invoke_and_wait(window, BUY.id(), serde_json::json!({ "qty": 0 })).await,
            Some(Err("买不了 0 股".to_string()))
        );
        // 参数形状不对：原因交回调用方，处理函数不会被调。
        let Some(Err(error)) =
            invoke_and_wait(window, BUY.id(), serde_json::json!({ "qty": "many" })).await
        else {
            panic!("参数不对应该失败");
        };
        assert!(error.contains("trader/buy"), "{error}");
        assert_eq!(
            invoke_and_wait(window, QUOTE.id(), serde_json::json!({ "qty": 1 })).await,
            Some(Ok(serde_json::Value::Null))
        );
    }

    const NOTE: Command<Order, ()> = Command::new("desk/note");
    const SHOW: Command<Order, ()> = Command::new("desk/show");

    /// 主窗口没开时命令照样执行：用不到窗口的做成，用得到的自己说缺主窗口。
    #[gpui::test]
    async fn commands_run_without_a_main_window(cx: &mut gpui::TestAppContext) {
        let noted = Rc::new(std::cell::Cell::new(0));
        let seen = noted.clone();
        let (note, show) = cx.update(|cx| {
            register_commands(
                "desk",
                "桌面",
                vec![
                    NOTE.spec("记一笔", move |order, _, _| {
                        seen.set(order.qty);
                        Ok(())
                    }),
                    SHOW.spec("显示", |_, window, _| {
                        window.ok_or_else(|| NO_MAIN_WINDOW.to_string())?;
                        Ok(())
                    }),
                ],
                cx,
            )
            .unwrap();
            let args = serde_json::json!({ "qty": 4 });
            (
                run_in(None, NOTE.id(), args.clone(), cx),
                run_in(None, SHOW.id(), args, cx),
            )
        });
        assert_eq!(note.await, Ok(serde_json::Value::Null));
        assert_eq!(noted.get(), 4);
        assert_eq!(show.await, Err(NO_MAIN_WINDOW.to_string()));
    }

    #[test]
    fn key_bindings_match_commands_by_id() {
        assert_eq!(RunCommand::new("a/b"), RunCommand::new("a/b"));
        assert_ne!(RunCommand::new("a/b"), RunCommand::new("a/c"));
    }
}
