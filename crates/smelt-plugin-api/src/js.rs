//! 给 JS 插件的 `smelt-plugin-api` 模块：本 crate 接口的薄映射，不另造一套。
//!
//! 宿主给每个插件建一份 [`JsPlugin`]，把它的 [`JsPlugin::module`] 装进那个插件自己的
//! gpui-shell Policy：模块里的函数由此知道是哪个插件在调。挂载成功后宿主调
//! [`JsPlugin::attach`] 交回插件视图。
//!
//! gpui-shell 的宿主不能回调脚本，宿主事件做成脚本 await 的 `next_event()`：每个插件
//! 一条队列，从挂载完起开始记。插件视图释放时队列一起撤掉，所以插件卸载、重新加载不用
//! 宿主另外收拾。
//!
//! 命令同理：宿主执行插件登记的命令时不能回调脚本，执行记进插件自己的命令队列，脚本
//! 用 `next_command()` 取。
//!
//! 插件摆在哪不由插件定，这里没有登记位置的函数；插件可以登记整份布局，用不用由用户在
//! 设置里选（见 `docs/app-plugins.md`）。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use futures_channel::oneshot;
use gpui::{App, Entity, Subscription};
use gpui_shell::{
    HostArguments, HostError, HostModule, HostObject, HostResult, HostValue, ScriptView,
};
use serde_json::Value;

use crate::{CommandSpec, Handled, HostEvent, PathRequest, RuntimeKey, rusqlite};

/// 模块名，和 Rust 这边的 crate 同名。
pub const MODULE: &str = "smelt-plugin-api";

const DECLARATIONS: &str = r#"
/** 一台机器：本机是 null，远程机器是它的 runtime id。 */
export type Machine = string | null;
export interface Project {
  machine: Machine;
  /** 根目录，按那台机器的路径写。 */
  root: string;
}
/** 工作台的当前项目，可能在远程机器上；没有项目时为 null。 */
export function active_project(): Project | null;
/** 那台机器的智能体配置（profiles、agents、env 等，形状同运行时的 agent_host.get）；还没读到时为 null。变了发 agent_settings_changed。 */
export function agent_settings(machine: Machine): unknown | null;
/** 那个视图此刻看得见。变了发 shown_views_changed。 */
export function view_shown(view: string): boolean;
/** 主窗口此刻在前台。回到前台发 window_activated，去后台不发：定时做事的醒来时问这里，不在前台就停。 */
export function window_active(): boolean;
/** 发送系统通知；显示受系统通知授权和设置影响。 */
export function notify(subtitle: string, body: string): null;
/** 替换本插件由脚本登记的主题，每项是 gpui-kit ThemeSet JSON。空数组撤下全部。 */
export function register_themes(themeSets: string[]): null;
export interface Layout {
  /** 本插件内的名字，不含 "/"；布局 id 是 "<插件 id>/<name>"。 */
  name: string;
  /** 设置里显示的名字。 */
  title: string;
  /** gpui-base DockAreaState JSON，面板名是视图 id，version 为 1。 */
  layout: string;
}
/** 替换本插件登记的布局。空数组撤下全部；一份不合法整组不登记。用哪份由用户在设置里选。 */
export function register_layouts(layouts: Layout[]): null;
export interface PathRequest {
  /** 选择框标题。 */
  title: string;
  /** 确认按钮上的动词，如「导入」。 */
  confirm: string;
  /** 除了目录还能选文件。 */
  files?: boolean;
  multiple?: boolean;
}
/** 在那台机器上选路径。取消时为 null。 */
export function pick_paths(machine: Machine, request: PathRequest): Promise<string[] | null>;
export interface Command {
  /** 本插件内的名字，不含 "/"；命令 id 是 "<插件 id>/<name>"。 */
  name: string;
  /** 命令面板和快捷键设置里显示的名字。 */
  title: string;
  /** 出厂快捷键，单个按键，如 "cmd-shift-y"；要带 cmd 或者是 F 键。用户可以在设置里改。 */
  key?: string;
  /** 快捷键在哪生效，gpui key context 谓词；默认 "Workspace"，即主窗口任何地方。 */
  context?: string;
  /** 出现在命令面板里；默认 true，requires_args 时默认 false。 */
  palette?: boolean;
  /** 要带参数才能执行：只给代码用 run 调，不能有 key、不进命令面板、不在快捷键设置里出现。默认 false。 */
  requires_args?: boolean;
}
/** 替换本插件登记的命令。空数组撤下全部；一条不合法整组不登记。执行时由 next_command 取到。 */
export function register_commands(commands: Command[]): null;
export interface CommandCall {
  /** 本插件内的名字。 */
  name: string;
  /** 执行时带的参数；快捷键、菜单、命令面板执行时为 null。 */
  args: unknown;
}
/** 等本插件命令的下一次执行。从登记起开始记，按顺序一次不落。同时只能有一个在等。 */
export function next_command(): Promise<CommandCall>;
/** 执行一条命令，任何来源的都行（id 是 "<来源>/<名字>"）。排到这一轮之后在主窗口里跑；命令不存在时报错。 */
export function run(command: string, args?: unknown): null;
/** 打开一个视图：布局里摆了它就让它露出来，没摆就在舞台上开成一页，再把 args 交给它。没有这个视图时为 false。 */
export function open_view(view: string, args?: unknown): Promise<boolean>;
/** 把文字交给当前会话（终端打进输入、对话填进输入框，不替用户发出）。没有能接的会话时报错。 */
export function send_to_session(text: string): Promise<null>;
/** 那台机器上的路径怎么切。 */
export function path_style(machine: Machine): "posix" | "windows";
/** 打开文件，交给登记的文件打开者；没有打开者时本机文件用系统文本编辑器打开，远程的报错。line 从 1 数。 */
export function open_file(machine: Machine, path: string, line?: number): null;
/** 让本插件的命令 name 当文件打开者（它要另外登记，requires_args）；执行时参数是 { machine, path, line }。只认最后登记的打开者。 */
export function register_file_opener(name: string): null;
/** SQL 参数。true / false 存成 1 / 0。 */
export type SqlValue = string | number | boolean | null;
/** 在本插件自己的 SQLite 库（~/.smelt/plugin-data/<插件 id>.sqlite3）里查询，每行是列名到值的对象。表由插件自己建；表结构的版本可以记在 PRAGMA user_version。BLOB 列取不出来。 */
export function store_query(sql: string, params?: SqlValue[]): Promise<Record<string, string | number | null>[]>;
/** 在本插件自己的库里执行一条语句，返回改动的行数。 */
export function store_execute(sql: string, params?: SqlValue[]): Promise<number>;
export type HostEvent =
  | { type: "runtime_topic"; machine: Machine; topic: string }
  | { type: "reconnected"; machine: Machine }
  | { type: "project_changed"; machine: Machine; root: string }
  | { type: "window_activated" }
  | { type: "machines_changed" }
  | { type: "active_project_changed" }
  | { type: "agent_settings_changed" }
  | { type: "shown_views_changed" }
  | { type: "views_changed" }
  | { type: "themes_changed" }
  | { type: "layouts_changed" }
  | { type: "commands_changed" };
/** 等宿主的下一条事件。从插件挂载完起开始记，没取走的同样一条只留一份。同时只能有一个在等。 */
export function next_event(): Promise<HostEvent>;
"#;

/// 一个 JS 插件在宿主这边的身份。
#[derive(Clone, Default)]
pub struct JsPlugin(Rc<RefCell<PluginState>>);

#[derive(Default)]
struct PluginState {
    /// 插件 id，它登记的布局、命令按这个来源记。
    id: String,
    /// 插件的名字，快捷键设置里它的命令按这个分组。
    name: String,
    theme_owner: String,
    queue: VecDeque<HostEvent>,
    waiter: Option<oneshot::Sender<HostEvent>>,
    /// 执行过、脚本还没取走的命令：名字和参数。
    calls: VecDeque<(String, Value)>,
    call_waiter: Option<oneshot::Sender<(String, Value)>>,
    /// 视图已经释放：它登记的命令不再接。
    released: bool,
    _events: Option<Subscription>,
    _release: Option<Subscription>,
}

impl JsPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 每个插件的脚本主题有独立的来源，不能替换其他插件或内置主题。`name` 是插件给人看
    /// 的名字。
    pub fn for_plugin(id: &str, name: &str) -> Self {
        let plugin = Self::default();
        {
            let mut state = plugin.0.borrow_mut();
            state.id = id.to_string();
            state.name = name.to_string();
            state.theme_owner = format!("script:{id}");
        }
        plugin
    }

    /// 这个插件的 `smelt-plugin-api` 模块，装进它自己的 Policy。
    pub fn module(&self) -> HostModule {
        let events = self.clone();
        let theme_owner = self.0.borrow().theme_owner.clone();
        let layout_owner = self.0.borrow().id.clone();
        let commands = self.clone();
        let calls = self.clone();
        let opener_owner = self.0.borrow().id.clone();
        let query_owner = self.0.borrow().id.clone();
        let execute_owner = query_owner.clone();
        HostModule::new(MODULE)
            .declarations(DECLARATIONS)
            .function("active_project", |_| {
                let project = in_app("active_project", |cx| crate::host(cx).active_project(cx))?;
                Ok(project.map_or(HostValue::Null, |project| {
                    HostObject::new()
                        .field("machine", machine_value(&project.machine))
                        .field("root", project.root.as_str())
                        .into()
                }))
            })
            .function("agent_settings", |arguments| {
                let machine = machine_argument("agent_settings", arguments, 0)?;
                let settings = in_app("agent_settings", |cx| {
                    crate::host(cx)
                        .agent_settings(&machine, cx)
                        .map(serde_json::to_value)
                })?;
                match settings {
                    None => Ok(HostValue::Null),
                    Some(Ok(value)) => Ok(host_value(&value)),
                    Some(Err(error)) => Err(HostError::new(format!(
                        "agent_settings：配置转不成 JSON：{error}"
                    ))),
                }
            })
            .function("view_shown", |arguments| {
                let id = arguments.string(0)?;
                Ok(in_app("view_shown", |cx| crate::host(cx).view_shown(id, cx))?.into())
            })
            .function("window_active", |_arguments| {
                Ok(in_app("window_active", |cx| crate::host(cx).window_active(cx))?.into())
            })
            .function("notify", |arguments| {
                let subtitle = arguments.string(0)?;
                let body = arguments.string(1)?;
                in_app("notify", |cx| crate::host(cx).notify(subtitle, body))?;
                Ok(HostValue::Null)
            })
            .function("register_themes", move |arguments| {
                let values = arguments
                    .value(0)?
                    .as_array()
                    .ok_or_else(|| HostError::new("主题集必须是字符串数组"))?;
                let sources = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| HostError::new("主题集必须是字符串数组"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if theme_owner.is_empty() {
                    return Err(HostError::new("插件没有主题来源身份"));
                }
                in_app("register_themes", |cx| {
                    crate::register_theme_sets(&theme_owner, &sources, cx)
                })?
                .map_err(|error| HostError::new(error.to_string()))?;
                Ok(HostValue::Null)
            })
            .function("register_layouts", move |arguments| {
                let layouts = layout_arguments(arguments)?;
                if layout_owner.is_empty() {
                    return Err(HostError::new("插件没有布局来源身份"));
                }
                let sources = layouts
                    .iter()
                    .map(|(name, title, state)| crate::LayoutSource { name, title, state })
                    .collect::<Vec<_>>();
                in_app("register_layouts", |cx| {
                    crate::register_layouts(&layout_owner, &sources, cx)
                })?
                .map_err(HostError::new)?;
                Ok(HostValue::Null)
            })
            .function("register_commands", move |arguments| {
                let specs = command_arguments(arguments, &commands)?;
                let (owner, group) = {
                    let state = commands.0.borrow();
                    (state.id.clone(), state.name.clone())
                };
                if owner.is_empty() {
                    return Err(HostError::new("插件没有命令来源身份"));
                }
                in_app("register_commands", |cx| {
                    crate::register_commands(&owner, &group, specs, cx)
                })?
                .map_err(HostError::new)?;
                Ok(HostValue::Null)
            })
            .function("run", |arguments| {
                let command = arguments.string(0)?;
                let args = arguments.get(1).map(json_value).unwrap_or(Value::Null);
                in_app("run", |cx| crate::run(command, args, cx))?.map_err(HostError::new)?;
                Ok(HostValue::Null)
            })
            .function("path_style", |arguments| {
                let machine = machine_argument("path_style", arguments, 0)?;
                let style = in_app("path_style", |cx| crate::host(cx).path_style(&machine, cx))?;
                Ok(match style {
                    crate::model::runtime_path::PathStyle::Posix => "posix",
                    crate::model::runtime_path::PathStyle::Windows => "windows",
                }
                .into())
            })
            .function("open_file", |arguments| {
                let machine = machine_argument("open_file", arguments, 0)?;
                let path = arguments.string(1)?.to_string();
                let line = match arguments.get(2) {
                    None | Some(HostValue::Null) => None,
                    Some(HostValue::Number(line)) if *line >= 1.0 && line.fract() == 0.0 => {
                        Some(*line as usize)
                    }
                    Some(other) => {
                        return Err(HostError::new(format!(
                            "open_file：line 要是从 1 数的整数，收到 {}",
                            other.describe()
                        )));
                    }
                };
                let location = crate::FileLocation {
                    machine,
                    path,
                    line,
                };
                in_app("open_file", |cx| crate::open_file(location, cx))?
                    .map_err(HostError::new)?;
                Ok(HostValue::Null)
            })
            .function("register_file_opener", move |arguments| {
                let name = arguments.string(0)?;
                if opener_owner.is_empty() {
                    return Err(HostError::new("插件没有命令来源身份"));
                }
                in_app("register_file_opener", |cx| {
                    crate::register_file_opener(&opener_owner, name, cx)
                })?;
                Ok(HostValue::Null)
            })
            .async_function("store_query", move |arguments| {
                let (sql, params) = sql_arguments("store_query", arguments)?;
                let owner = query_owner.clone();
                Ok(async move { store_query(&owner, &sql, params) })
            })
            .async_function("store_execute", move |arguments| {
                let (sql, params) = sql_arguments("store_execute", arguments)?;
                let owner = execute_owner.clone();
                Ok(async move { store_execute(&owner, &sql, params) })
            })
            .async_function("open_view", |arguments| {
                let id = arguments.string(0)?.to_string();
                let args = arguments.get(1).map(json_value).unwrap_or(Value::Null);
                in_main_window("open_view", move |window, cx| {
                    Ok(crate::host(cx).open_view(&id, args, window, cx).into())
                })
            })
            .async_function("send_to_session", |arguments| {
                let text = arguments.string(0)?.to_string();
                in_main_window("send_to_session", move |window, cx| {
                    crate::host(cx)
                        .send_to_session(&text, window, cx)
                        .map(|()| HostValue::Null)
                        .map_err(HostError::new)
                })
            })
            .async_function("next_command", move |_| Ok(calls.next_command()))
            .async_function("pick_paths", pick_paths)
            .async_function("next_event", move |_| Ok(events.next_event()))
    }

    /// 挂载成功后调：开始记宿主事件，视图释放时撤掉。
    pub fn attach(&self, view: &Entity<ScriptView>, cx: &mut App) {
        let state = Rc::downgrade(&self.0);
        let release = cx.observe_release(view, move |_, _| {
            if let Some(state) = state.upgrade() {
                JsPlugin(state).detach();
            }
        });
        let state = Rc::downgrade(&self.0);
        let host_events = crate::events(cx);
        let events = cx.subscribe(&host_events, move |_, event: &HostEvent, _| {
            if let Some(state) = state.upgrade() {
                JsPlugin(state).deliver(event);
            }
        });
        let mut state = self.0.borrow_mut();
        state._release = Some(release);
        state._events = Some(events);
    }

    fn next_event(&self) -> impl Future<Output = Result<HostValue, HostError>> + Send + use<> {
        let (sender, receiver) = oneshot::channel();
        let mut state = self.0.borrow_mut();
        match state.queue.pop_front() {
            Some(event) => {
                let _ = sender.send(event);
            }
            // 上一个还在等的被顶掉，它那边收到 `Canceled`。
            None => state.waiter = Some(sender),
        }
        async move {
            receiver
                .await
                .map(|event| event_value(&event))
                .map_err(|_| HostError::new("next_event：被后一次 next_event 顶掉了"))
        }
    }

    fn deliver(&self, event: &HostEvent) {
        let mut state = self.0.borrow_mut();
        let event = match state.waiter.take() {
            Some(waiter) => match waiter.send(event.clone()) {
                Ok(()) => return,
                // 等的那个 Promise 已经没人要了，照常排队。
                Err(event) => event,
            },
            None => event.clone(),
        };
        if !state.queue.contains(&event) {
            state.queue.push_back(event);
        }
    }

    fn next_command(&self) -> impl Future<Output = Result<HostValue, HostError>> + Send + use<> {
        let (sender, receiver) = oneshot::channel();
        let mut state = self.0.borrow_mut();
        match state.calls.pop_front() {
            Some(call) => {
                let _ = sender.send(call);
            }
            // 上一个还在等的被顶掉，它那边收到 `Canceled`。
            None => state.call_waiter = Some(sender),
        }
        async move {
            let (name, args) = receiver
                .await
                .map_err(|_| HostError::new("next_command：被后一次 next_command 顶掉了"))?;
            Ok(HostObject::new()
                .field("name", name)
                .field("args", host_value(&args))
                .into())
        }
    }

    /// 宿主执行了这个插件的一条命令。视图已经释放就不接，按键交给下一条绑定。
    fn deliver_command(&self, name: &str, args: &Value) -> Handled {
        let mut state = self.0.borrow_mut();
        if state.released {
            return Handled::Pass;
        }
        let call = (name.to_string(), args.clone());
        let call = match state.call_waiter.take() {
            Some(waiter) => match waiter.send(call) {
                Ok(()) => return Handled::Done,
                // 等的那个 Promise 已经没人要了，照常排队。
                Err(call) => call,
            },
            None => call,
        };
        // 每次执行都要到，不去重。
        state.calls.push_back(call);
        Handled::Done
    }

    /// 视图释放了：扔掉队列和订阅。登记的命令由宿主随插件撤下（见 `mount_all`），
    /// 这里撤会把重新加载后新登记的一起撤掉。
    fn detach(&self) {
        let mut state = self.0.borrow_mut();
        state.queue.clear();
        state.waiter = None;
        state._events = None;
        state.calls.clear();
        state.call_waiter = None;
        state.released = true;
    }
}

fn in_app<R>(name: &str, f: impl FnOnce(&mut App) -> R) -> Result<R, HostError> {
    gpui_shell::with_current_app(f)
        .ok_or_else(|| HostError::new(format!("{name} 只能在视图的事件、任务或 init 里调用")))
}

/// 在主窗口里做 `f`，结果用 Promise 交回。脚本可能正嵌在主窗口自己的更新里调，进不了
/// 窗口，所以推到这一轮之后。
fn in_main_window<F>(
    name: &'static str,
    f: F,
) -> Result<impl Future<Output = Result<HostValue, HostError>> + Send + use<F>, HostError>
where
    F: FnOnce(&mut gpui::Window, &mut App) -> Result<HostValue, HostError> + 'static,
{
    let (sender, receiver) = oneshot::channel::<Result<HostValue, HostError>>();
    in_app(name, |cx| {
        let Some(window) = crate::host(cx).main_window(cx) else {
            let _ = sender.send(Err(HostError::new(format!("{name}：主窗口没有打开"))));
            return;
        };
        cx.defer(move |cx| {
            let result = window
                .update(cx, |_, window, cx| f(window, cx))
                .unwrap_or_else(|_| Err(HostError::new(format!("{name}：主窗口已经关了"))));
            let _ = sender.send(result);
        });
    })?;
    Ok(async move {
        receiver
            .await
            .unwrap_or_else(|_| Err(HostError::new(format!("{name}：没有执行"))))
    })
}

/// `store_query` / `store_execute` 的参数：SQL 和可选的参数数组。
fn sql_arguments(
    name: &str,
    arguments: &HostArguments,
) -> Result<(String, Vec<rusqlite::types::Value>), HostError> {
    use rusqlite::types::Value as Sql;
    let sql = arguments.string(0)?.to_string();
    let params = match arguments.get(1) {
        None | Some(HostValue::Null) => Vec::new(),
        Some(HostValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                HostValue::Null => Ok(Sql::Null),
                HostValue::Bool(value) => Ok(Sql::Integer(i64::from(*value))),
                HostValue::Number(number)
                    if number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER =>
                {
                    Ok(Sql::Integer(*number as i64))
                }
                HostValue::Number(number) => Ok(Sql::Real(*number)),
                HostValue::Str(text) => Ok(Sql::Text(text.clone())),
                other => Err(HostError::new(format!(
                    "{name}：参数只能是字符串、数字、布尔或 null，收到 {}",
                    other.describe()
                ))),
            })
            .collect::<Result<_, _>>()?,
        Some(other) => {
            return Err(HostError::new(format!(
                "{name}：params 要是数组，收到 {}",
                other.describe()
            )));
        }
    };
    Ok((sql, params))
}

/// JS 数字能精确表示的最大整数，`Number.MAX_SAFE_INTEGER`。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn plugin_store(owner: &str) -> Result<std::sync::Arc<crate::PluginStore>, HostError> {
    if owner.is_empty() {
        return Err(HostError::new("插件没有来源身份，不能用库"));
    }
    crate::plugin_store(owner).map_err(|error| HostError::new(error.to_string()))
}

fn store_query(owner: &str, sql: &str, params: Vec<rusqlite::types::Value>) -> HostResult {
    use rusqlite::types::ValueRef;
    let store = plugin_store(owner)?;
    let rows = store
        .read(|db| {
            let mut statement = db.prepare(sql)?;
            let columns: Vec<String> = statement
                .column_names()
                .into_iter()
                .map(str::to_string)
                .collect();
            let mut rows = statement.query(rusqlite::params_from_iter(params))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let mut object = HostObject::new();
                for (index, column) in columns.iter().enumerate() {
                    let value = match row.get_ref(index)? {
                        ValueRef::Null => HostValue::Null,
                        ValueRef::Integer(value) => HostValue::from(value),
                        ValueRef::Real(value) => HostValue::from(value),
                        ValueRef::Text(text) => {
                            HostValue::from(String::from_utf8_lossy(text).into_owned())
                        }
                        ValueRef::Blob(_) => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                index,
                                column.clone(),
                                rusqlite::types::Type::Blob,
                            ));
                        }
                    };
                    object = object.field(column.as_str(), value);
                }
                out.push(HostValue::from(object));
            }
            Ok(out)
        })
        .map_err(|error| HostError::new(format!("store_query：{error}")))?;
    Ok(HostValue::Array(rows))
}

fn store_execute(owner: &str, sql: &str, params: Vec<rusqlite::types::Value>) -> HostResult {
    let store = plugin_store(owner)?;
    let changed = store
        .read(|db| db.execute(sql, rusqlite::params_from_iter(params)))
        .map_err(|error| HostError::new(format!("store_execute：{error}")))?;
    Ok(HostValue::from(changed))
}

/// `register_layouts` 的参数：每项的名字、标题、布局原文。
fn layout_arguments(arguments: &HostArguments) -> Result<Vec<(String, String, String)>, HostError> {
    let invalid = || HostError::new("布局必须是 { name, title, layout } 字符串对象的数组");
    arguments
        .value(0)?
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|layout| {
            let field = |name: &str| {
                layout
                    .get(name)
                    .and_then(HostValue::as_str)
                    .map(str::to_owned)
                    .ok_or_else(invalid)
            };
            Ok((field("name")?, field("title")?, field("layout")?))
        })
        .collect()
}

/// `register_commands` 的参数。处理函数把执行记进插件的命令队列。
fn command_arguments(
    arguments: &HostArguments,
    plugin: &JsPlugin,
) -> Result<Vec<CommandSpec>, HostError> {
    let invalid = |what: &str| HostError::new(format!("register_commands：{what}"));
    arguments
        .value(0)?
        .as_array()
        .ok_or_else(|| invalid("参数要是命令对象的数组"))?
        .iter()
        .map(|command| {
            let text = |name: &str| match command.get(name) {
                None | Some(HostValue::Null) => Ok(None),
                Some(HostValue::Str(text)) => Ok(Some(text.clone())),
                Some(other) => Err(invalid(&format!(
                    "{name} 要是字符串，收到 {}",
                    other.describe()
                ))),
            };
            let name = text("name")?.ok_or_else(|| invalid("每条命令都要有 name"))?;
            let title = text("title")?.ok_or_else(|| invalid("每条命令都要有 title"))?;
            let flag = |name: &str, default: bool| match command.get(name) {
                None | Some(HostValue::Null) => Ok(default),
                Some(HostValue::Bool(value)) => Ok(*value),
                Some(other) => Err(invalid(&format!(
                    "{name} 要是布尔值，收到 {}",
                    other.describe()
                ))),
            };
            let requires_args = flag("requires_args", false)?;
            let palette = flag("palette", !requires_args)?;
            let state = Rc::downgrade(&plugin.0);
            let handler_name = name.clone();
            Ok(CommandSpec {
                key: text("key")?,
                context: text("context")?.unwrap_or_else(|| "Workspace".to_string()),
                palette,
                requires_args,
                handler: Rc::new(move |args, _, _| match state.upgrade() {
                    Some(state) => JsPlugin(state).deliver_command(&handler_name, args),
                    None => Handled::Pass,
                }),
                name,
                title,
            })
        })
        .collect()
}

/// 脚本给的值转成命令参数。整数值的数字写成整数，Rust 这边 `as_u64` 读得出来；
/// NaN、无穷写不进 JSON，成了 `null`。
fn json_value(value: &HostValue) -> Value {
    match value {
        HostValue::Null => Value::Null,
        HostValue::Bool(value) => Value::Bool(*value),
        HostValue::Number(number) => {
            // ±2^53 以内的整数 f64 和 i64 一一对应。
            const EXACT: f64 = 9_007_199_254_740_992.0;
            if number.fract() == 0.0 && number.abs() <= EXACT {
                Value::from(*number as i64)
            } else {
                serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number)
            }
        }
        HostValue::Str(text) => Value::String(text.clone()),
        HostValue::Array(values) => Value::Array(values.iter().map(json_value).collect()),
        HostValue::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), json_value(value)))
                .collect(),
        ),
    }
}

/// 命令参数转成给脚本的值。JS 的数字都是 f64。
fn host_value(value: &Value) -> HostValue {
    match value {
        Value::Null => HostValue::Null,
        Value::Bool(value) => HostValue::Bool(*value),
        Value::Number(number) => number.as_f64().map_or(HostValue::Null, HostValue::Number),
        Value::String(text) => HostValue::Str(text.clone()),
        Value::Array(values) => HostValue::Array(values.iter().map(host_value).collect()),
        Value::Object(fields) => HostValue::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), host_value(value)))
                .collect(),
        ),
    }
}

/// 脚本里的机器：本机 `null`，远程机器是 runtime id。
fn machine_argument(
    name: &str,
    arguments: &HostArguments,
    index: usize,
) -> Result<RuntimeKey, HostError> {
    match arguments.value(index)? {
        HostValue::Null => Ok(RuntimeKey::Local),
        HostValue::Str(id) => Ok(RuntimeKey::Remote(id.clone())),
        other => Err(HostError::new(format!(
            "{name}：机器要是 null 或字符串，收到 {}",
            other.describe()
        ))),
    }
}

fn machine_value(key: &RuntimeKey) -> HostValue {
    match key {
        RuntimeKey::Local => HostValue::Null,
        RuntimeKey::Remote(id) => HostValue::Str(id.clone()),
    }
}

// ---------------------------------------------------------------- 选路径

fn pick_paths(
    arguments: &HostArguments,
) -> Result<impl Future<Output = Result<HostValue, HostError>> + Send + use<>, HostError> {
    let machine = machine_argument("pick_paths", arguments, 0)?;
    let request = arguments.value(1)?;
    let text = |name: &str| {
        request
            .get(name)
            .and_then(HostValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| HostError::new(format!("pick_paths：{name} 要是字符串")))
    };
    let flag = |name: &str| {
        request
            .get(name)
            .and_then(HostValue::as_bool)
            .unwrap_or(false)
    };
    let request = PathRequest {
        title: text("title")?,
        confirm: text("confirm")?,
        files: flag("files"),
        multiple: flag("multiple"),
    };
    let (sender, receiver) = oneshot::channel::<Vec<String>>();
    in_app("pick_paths", |cx| {
        let host = crate::host(cx);
        // 脚本可能正嵌在宿主别的更新里调它；选择框要进工作台，推到这一轮之后。
        cx.defer(move |cx| {
            let done = Box::new(move |paths: Vec<String>, _: &mut App| {
                let _ = sender.send(paths);
            });
            host.pick_paths(&machine, request, done, cx);
        });
    })?;
    // 取消了宿主不调 `done`，它被扔掉，这边就收到 `Canceled`。
    Ok(async move {
        Ok(match receiver.await {
            Ok(paths) => paths.into(),
            Err(oneshot::Canceled) => HostValue::Null,
        })
    })
}

// ---------------------------------------------------------------- 事件

fn event_value(event: &HostEvent) -> HostValue {
    let machine = machine_value;
    let object = match event {
        HostEvent::RuntimeTopic {
            machine: key,
            topic,
        } => HostObject::new()
            .field("type", "runtime_topic")
            .field("machine", machine(key))
            .field("topic", topic.as_str()),
        HostEvent::Reconnected(key) => HostObject::new()
            .field("type", "reconnected")
            .field("machine", machine(key)),
        HostEvent::ProjectChanged { machine: key, root } => HostObject::new()
            .field("type", "project_changed")
            .field("machine", machine(key))
            .field("root", root.as_str()),
        HostEvent::WindowActivated => HostObject::new().field("type", "window_activated"),
        HostEvent::MachinesChanged => HostObject::new().field("type", "machines_changed"),
        HostEvent::ActiveProjectChanged => {
            HostObject::new().field("type", "active_project_changed")
        }
        HostEvent::AgentSettingsChanged => {
            HostObject::new().field("type", "agent_settings_changed")
        }
        HostEvent::ShownViewsChanged => HostObject::new().field("type", "shown_views_changed"),
        HostEvent::ViewsChanged => HostObject::new().field("type", "views_changed"),
        HostEvent::ThemesChanged => HostObject::new().field("type", "themes_changed"),
        HostEvent::LayoutsChanged => HostObject::new().field("type", "layouts_changed"),
        HostEvent::CommandsChanged => HostObject::new().field("type", "commands_changed"),
    };
    object.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_declarations_match_its_functions() {
        JsPlugin::new()
            .module()
            .validate()
            .expect("声明和函数对得上");
    }

    #[test]
    fn notification_requires_two_strings_and_a_script_context() {
        let module = JsPlugin::new().module();
        let invalid_arguments = [
            HostArguments::default(),
            HostArguments::new([HostValue::Null, HostValue::from("正文")]),
            HostArguments::new([HostValue::from("副标题")]),
            HostArguments::new([HostValue::from("副标题"), HostValue::Null]),
        ];
        for arguments in invalid_arguments {
            assert!(module.call("notify", &arguments).is_err());
        }
        let outside_script = module
            .call(
                "notify",
                &HostArguments::new([HostValue::from("副标题"), HostValue::from("正文")]),
            )
            .unwrap_err();
        assert!(outside_script.message().contains("只能在视图"));
    }

    #[test]
    fn host_queries_check_their_arguments_before_entering_app() {
        let module = JsPlugin::new().module();
        for machine in [HostValue::Bool(true), HostValue::Number(1.0)] {
            let error = module
                .call("agent_settings", &HostArguments::new([machine]))
                .unwrap_err();
            assert!(error.message().contains("机器要是 null 或字符串"));
        }
        assert!(
            module
                .call("view_shown", &HostArguments::default())
                .is_err()
        );
        // 参数对了才去找宿主，测试里没有脚本上下文。
        let outside_script = module
            .call("agent_settings", &HostArguments::new([HostValue::Null]))
            .unwrap_err();
        assert!(outside_script.message().contains("只能在视图"));
    }

    #[test]
    fn theme_registration_rejects_invalid_arguments_before_entering_app() {
        let module = JsPlugin::for_plugin("example", "示例").module();
        assert!(
            module
                .call("register_themes", &HostArguments::default())
                .is_err()
        );
        assert!(
            module
                .call("register_themes", &HostArguments::new([HostValue::Null]))
                .is_err()
        );
        assert!(
            module
                .call(
                    "register_themes",
                    &HostArguments::new([HostValue::Array(vec![HostValue::Null])])
                )
                .is_err()
        );
    }

    #[test]
    fn layout_registration_rejects_invalid_arguments_before_entering_app() {
        let module = JsPlugin::for_plugin("example", "示例").module();
        let layout = |fields: &[(&str, HostValue)]| {
            let mut object = HostObject::new();
            for (name, value) in fields {
                object = object.field(*name, value.clone());
            }
            HostValue::Array(vec![object.into()])
        };
        for arguments in [
            HostArguments::default(),
            HostArguments::new([HostValue::Null]),
            HostArguments::new([HostValue::Array(vec![HostValue::Null])]),
            HostArguments::new([layout(&[("name", "a".into()), ("title", "A".into())])]),
            HostArguments::new([layout(&[
                ("name", "a".into()),
                ("title", HostValue::Null),
                ("layout", "{}".into()),
            ])]),
        ] {
            let error = module.call("register_layouts", &arguments).unwrap_err();
            assert!(
                !error.message().contains("只能在视图"),
                "{}",
                error.message()
            );
        }
        // 参数对了才进 App；测试里没有 App。
        let error = module
            .call(
                "register_layouts",
                &HostArguments::new([layout(&[
                    ("name", "a".into()),
                    ("title", "A".into()),
                    ("layout", "{}".into()),
                ])]),
            )
            .unwrap_err();
        assert!(error.message().contains("只能在视图"));
    }

    #[test]
    fn command_registration_rejects_invalid_arguments_before_entering_app() {
        let module = JsPlugin::for_plugin("example", "示例").module();
        let command = |fields: &[(&str, HostValue)]| {
            let mut object = HostObject::new();
            for (name, value) in fields {
                object = object.field(*name, value.clone());
            }
            HostValue::Array(vec![object.into()])
        };
        for arguments in [
            HostArguments::default(),
            HostArguments::new([HostValue::Null]),
            HostArguments::new([HostValue::Array(vec![HostValue::Null])]),
            HostArguments::new([command(&[("name", "a".into())])]),
            HostArguments::new([command(&[("title", "A".into())])]),
            HostArguments::new([command(&[
                ("name", "a".into()),
                ("title", "A".into()),
                ("key", HostValue::Number(1.0)),
            ])]),
            HostArguments::new([command(&[
                ("name", "a".into()),
                ("title", "A".into()),
                ("palette", "yes".into()),
            ])]),
        ] {
            let error = module.call("register_commands", &arguments).unwrap_err();
            assert!(
                !error.message().contains("只能在视图"),
                "{}",
                error.message()
            );
        }
        let error = module
            .call(
                "register_commands",
                &HostArguments::new([command(&[("name", "a".into()), ("title", "A".into())])]),
            )
            .unwrap_err();
        assert!(error.message().contains("只能在视图"));
    }

    #[test]
    fn command_calls_are_queued_in_order_and_stop_after_release() {
        let plugin = JsPlugin::for_plugin("example", "示例");
        let args = serde_json::json!({ "n": 1 });
        assert_eq!(plugin.deliver_command("a", &args), Handled::Done);
        assert_eq!(plugin.deliver_command("a", &args), Handled::Done);
        let value = smol::block_on(plugin.next_command()).unwrap();
        assert_eq!(value.get("name").and_then(HostValue::as_str), Some("a"));
        assert_eq!(
            value.get("args").and_then(|args| args.get("n")),
            Some(&HostValue::Number(1.0))
        );
        assert_eq!(plugin.0.borrow().calls.len(), 1);
        assert!(smol::block_on(plugin.next_command()).is_ok());
        // 队列空了，这次挂起等着；视图释放后它收到错误，不会一直挂着。
        let waiting = plugin.next_command();
        assert!(plugin.0.borrow().call_waiter.is_some());
        plugin.detach();
        assert!(smol::block_on(waiting).is_err());
        assert_eq!(plugin.deliver_command("a", &args), Handled::Pass);
        assert!(plugin.0.borrow().calls.is_empty());
    }

    #[test]
    fn command_args_cross_the_script_boundary() {
        let args = serde_json::json!({ "index": 3, "ratio": 0.5, "tags": ["a", null, true] });
        assert_eq!(json_value(&host_value(&args)), args);
        assert_eq!(json_value(&HostValue::Number(f64::NAN)), Value::Null);
        assert_eq!(json_value(&HostValue::Number(2.0)).as_u64(), Some(2));
    }

    #[test]
    fn queued_events_are_not_duplicated() {
        let plugin = JsPlugin::new();
        plugin.deliver(&HostEvent::ActiveProjectChanged);
        plugin.deliver(&HostEvent::ActiveProjectChanged);
        plugin.deliver(&HostEvent::MachinesChanged);
        assert_eq!(
            plugin.0.borrow().queue,
            [HostEvent::ActiveProjectChanged, HostEvent::MachinesChanged]
        );
    }

    #[test]
    fn a_waiting_next_event_takes_the_event_directly() {
        let plugin = JsPlugin::new();
        let waiting = plugin.next_event();
        plugin.deliver(&HostEvent::ViewsChanged);
        assert!(plugin.0.borrow().queue.is_empty());
        let value = smol::block_on(waiting).unwrap();
        assert_eq!(
            value.get("type").and_then(HostValue::as_str),
            Some("views_changed")
        );
    }

    #[test]
    fn a_queued_event_is_handed_out_first() {
        let plugin = JsPlugin::new();
        plugin.deliver(&HostEvent::MachinesChanged);
        let value = smol::block_on(plugin.next_event()).unwrap();
        assert_eq!(
            value.get("type").and_then(HostValue::as_str),
            Some("machines_changed")
        );
        assert!(plugin.0.borrow().queue.is_empty());
    }

    #[test]
    fn store_functions_round_trip_in_the_plugins_own_database() {
        let owner = format!("js-store-test-{}", std::process::id());
        let module = JsPlugin::for_plugin(&owner, "库").module();
        let run = |name: &str, values: Vec<HostValue>| {
            smol::block_on(module.begin(name, &HostArguments::new(values)).unwrap())
        };
        run(
            "store_execute",
            vec!["CREATE TABLE IF NOT EXISTS note (id INTEGER, body TEXT, score REAL, done INTEGER)".into()],
        )
        .unwrap();
        let inserted = run(
            "store_execute",
            vec![
                "INSERT INTO note VALUES (?1, ?2, ?3, ?4)".into(),
                HostValue::Array(vec![
                    HostValue::from(7),
                    HostValue::from("你好"),
                    HostValue::from(1.5),
                    HostValue::Bool(true),
                ]),
            ],
        )
        .unwrap();
        assert_eq!(inserted, HostValue::from(1));
        let rows = run("store_query", vec!["SELECT * FROM note".into()]).unwrap();
        let expected: HostValue = HostObject::new()
            .field("id", 7)
            .field("body", "你好")
            .field("score", 1.5)
            .field("done", 1)
            .into();
        assert_eq!(rows, HostValue::Array(vec![expected]));

        // 参数形状不对在主线程上就报，不进库。
        let nested = HostArguments::new([
            "SELECT ?1".into(),
            HostValue::Array(vec![HostValue::Array(Vec::new())]),
        ]);
        assert!(module.begin("store_query", &nested).is_err());
        let blob = run("store_query", vec!["SELECT x'00' AS b".into()]);
        assert!(blob.is_err(), "BLOB 列取不出来");
        // 没有来源身份的不给库。
        let anonymous = JsPlugin::new().module();
        let begun = anonymous
            .begin("store_query", &HostArguments::new(["SELECT 1".into()]))
            .unwrap();
        assert!(smol::block_on(begun).is_err());
    }
}
