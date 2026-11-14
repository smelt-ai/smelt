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
use gpui::{App, Entity, SharedString, Subscription};
use gpui_shell::{
    HostArguments, HostError, HostModule, HostObject, HostResult, HostValue, ScriptView,
};
use serde_json::Value;

use crate::{
    CommandSpec, Handled, HostEvent, RuntimeKey, SettingsGroup, SettingsPageBuilder,
    SettingsPageSpec, TopicMessage, rusqlite,
};

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
/** 舞台上露着的一段会话。 */
export interface ViewedSession {
  machine: Machine;
  /** 守护会话 id（终端是那一格），同 session_states 里的。 */
  id: string;
  /** 最新的内容在眼前；对话往上翻着看历史时为 false，终端总是 true。 */
  at_latest: boolean;
}
/** 用户眼下在看什么。 */
export interface ActiveContext {
  /** 当前项目，可能在远程机器上；没有项目时为 null。 */
  project: Project | null;
  /** 舞台上露着的会话；舞台被挡住、收起、换成别的页，或没有会话时为 null。 */
  session: ViewedSession | null;
}
/** 用户眼下在看什么。变了发 active_context_changed。 */
export function active_context(): ActiveContext;
/** 那台机器的运行时报的会话状态，每格终端、每段对话一条（形状同运行时 subscribe 推的会话状态）。主窗口关着也照常更新；变了发 sessions_changed。 */
export function session_states(machine: Machine): unknown[];
/** 那台机器的工作区目录：开着的项目和对话，是会话的名册（形状同运行时的 workspace 目录）；还没收到时为 null。变了发 sessions_changed。 */
export function workspace_directory(machine: Machine): unknown | null;
export interface Attention {
  /** 守护会话 id。 */
  session_id: string;
  /** 还等人处理（批准、输入、失败），看过也留着；false 是看过就消的提醒。 */
  requires_action: boolean;
}
/** 要人看一眼的会话，所有机器的。变了发 attention_changed。 */
export function attention(): Attention[];
/** 菜单栏菜单里的一项。icon 只取 PNG（字节数组）的透明度当形状，涂成 color（0xRRGGBB）。 */
export type StatusMenuItem =
  | { type: "action"; label: string; icon?: { png: number[]; color: number } | null; command: string; args?: unknown }
  | { type: "separator" };
/** 那台机器的智能体配置（profiles、agents、env 等，形状同运行时的 agent_host.get）；还没读到时为 null。变了发 agent_settings_changed。 */
export function agent_settings(machine: Machine): unknown | null;
/** 那个视图此刻看得见。变了发 shown_views_changed。 */
export function view_shown(view: string): boolean;
/** 主窗口此刻在前台。回到前台发 window_activated，去后台不发：定时做事的醒来时问这里，不在前台就停。 */
export function window_active(): boolean;
/** 主窗口里宿主的遮罩层（确认框、命令面板、大图预览）开着。开关时发 overlay_changed。 */
export function overlay_open(): boolean;
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
/**
 * 执行一条命令，任何来源的都行（id 是 "<来源>/<名字>"），兑现为命令交回的结果。排到这一轮之后跑；
 * 主窗口没开也照样执行，要主窗口的命令这时 reject。命令不存在、眼下不适用、执行失败时 reject，带可以直接
 * 给人看的原因。不 await 命令也照样跑完。
 * 脚本登记的命令接到就算成功，结果是 null。
 *
 * 宿主的动作也是命令：
 * - "smelt.app/notify" { id?: string, subtitle: string, body: string, on_click?: { command: string, args?: unknown } }：
 *   发系统通知，显示受系统通知授权和设置影响。给了 id（以插件 id 开头，"<插件 id>/…"）的留在通知中心里由插件管：
 *   同一个 id 再发换掉上一条，可以撤回，应用重启后也认得；不给是一次性的提示。on_click 是点它时执行的命令，
 *   不给就只是把应用拿到前面来。
 * - "smelt.app/withdraw-notification" { id: string }：撤回给了 id 的通知；没有这条也不算错。
 * - "smelt.app/open-in-text-editor" { path: string }：用系统文本编辑器打开这台电脑上的文件。
 * - "smelt.app/pick-paths" { machine: Machine, title: string, confirm: string, files?: boolean, multiple?: boolean }
 *   → string[] | null：在那台机器上选路径，title 是选择框标题，confirm 是确认按钮上的动词；取消时为 null。
 * - "smelt.app/open-view" { view: string, args?: unknown } → boolean：打开一个视图，布局里摆了它就让它露出来，
 *   没摆就在舞台上开成一页，再把 args 交给它；没有这个视图时为 false。
 * - "smelt.app/show-main-window"：把主窗口拿到前面来，已经关了就新开一扇。
 * - "smelt.app/set-status-menu" { title?: string, tooltip?: string, items?: StatusMenuItem[] }：设置菜单栏图标旁的字、
 *   提示和点开的菜单，后设的为准；只有 macOS 有。
 * - "smelt.app/set-dock-badge" { label: string | null }：设置 Dock 图标的角标，null 清掉；只有 macOS 有。
 * - "smelt.sessions/activate" { session_id: string }：在主窗口里露出这个会话并拿到前面来，主窗口关着就先开。
 * - "smelt.sessions/send-text" { text: string }：把文字交给当前会话（终端打进输入、对话填进输入框，不替用户发出）；
 *   没有能接的会话时 reject。
 * - "smelt.sessions/resume" / "smelt.sessions/resume-in-terminal" / "smelt.sessions/handoff"：续接、交接历史会话。
 */
export function run(command: string, args?: unknown): Promise<unknown>;
/** 那台机器上的路径怎么切。 */
export function path_style(machine: Machine): "posix" | "windows";
/** 打开文件，交给登记的文件打开者；没有打开者时本机文件用系统文本编辑器打开，远程的 reject。line 从 1 数。 */
export function open_file(machine: Machine, path: string, line?: number): Promise<null>;
export interface SettingsPage {
  /** 本插件内的名字，不含 "/"；设置页 id 是 "<插件 id>/<name>"。 */
  name: string;
  /** 设置侧栏上的名字。 */
  title: string;
  /**
   * 页面的入口脚本，插件目录里的相对路径，如 "settings.tsx"。设置窗口里点开这一页时单独加载、挂载它，
   * 和主入口是两份脚本实例，不共享内存：值存在插件自己的库里（store_query / store_execute），
   * 要主入口跟着变就 run 一条它登记的命令。设置窗口关了这一份就放掉。
   * 入口脚本里不能调 register_* 登记东西，登记都在主入口里做。
   */
  entry: string;
  /** 放进设置侧栏的哪一组，默认 "plugins"。 */
  group?: "general" | "agents" | "system" | "plugins";
  /** 搜索时除了标题还能按这些词找到这一页。 */
  keywords?: string[];
}
/** 替换本插件登记的设置页。空数组撤下全部；一页不合法整组不登记。 */
export function register_settings_pages(pages: SettingsPage[]): null;
/** 让本插件的命令 name 当文件打开者（它要另外登记，requires_args）；执行时参数是 { machine, path, line }。只认最后登记的打开者。 */
export function register_file_opener(name: string): null;
/** SQL 参数。true / false 存成 1 / 0。 */
export type SqlValue = string | number | boolean | null;
/** 在本插件自己的 SQLite 库（~/.smelt/plugin-data/<插件 id>.sqlite3）里查询，每行是列名到值的对象。表由插件自己建；表结构的版本可以记在 PRAGMA user_version。BLOB 列取不出来。 */
export function store_query(sql: string, params?: SqlValue[]): Promise<Record<string, string | number | null>[]>;
/** 在本插件自己的库里执行一条语句，返回改动的行数。 */
export function store_execute(sql: string, params?: SqlValue[]): Promise<number>;
export type HostEvent =
  | { type: "runtime_topic"; machine: Machine; topic: string; message: TopicMessage }
  | { type: "reconnected"; machine: Machine }
  | { type: "project_changed"; machine: Machine; root: string }
  | { type: "window_activated" }
  | { type: "overlay_changed" }
  | { type: "machines_changed" }
  | { type: "active_context_changed" }
  | { type: "sessions_changed" }
  | { type: "attention_changed" }
  | { type: "agent_settings_changed" }
  | { type: "shown_views_changed" }
  | { type: "views_changed" }
  | { type: "themes_changed" }
  | { type: "layouts_changed" }
  | { type: "commands_changed" }
  | { type: "settings_pages_changed" };
/**
 * 运行时话题推来的：一条事件；带快照的话题（如 attention.changed）订上和积压后的整份状态，收到就整份换掉
 * 自己留的；断开或积压可能错过了事件（没有快照的话题自己重读）。载荷形状见协议里那个话题的类型。
 */
export type TopicMessage =
  | { kind: "event"; payload: unknown }
  | { kind: "snapshot"; payload: unknown }
  | { kind: "lagged" };
/**
 * 等宿主的下一条事件。从插件挂载完起开始记，没取走的同样一条只留一份；一台机器一个话题的快照或积压到了，
 * 这个话题之前没取走的就不留了。同时只能有一个在等。
 */
export function next_event(): Promise<HostEvent>;
"#;

/// 宿主怎么把插件目录里的一个入口建成设置页（见 `register_settings_pages`）。
#[derive(Clone)]
pub struct SettingsPageLoader {
    /// JS 设置页在侧栏上一律用这个图标。
    pub icon: SharedString,
    /// 校验入口（插件目录里的相对路径），给出建页的函数。
    pub load: Rc<dyn Fn(&str) -> Result<SettingsPageBuilder, String>>,
}

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
    /// 设置页那份脚本：和主入口同一个身份，但不能登记，登记会顶掉主入口登记的那组。
    settings_page: bool,
    settings_loader: Option<SettingsPageLoader>,
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

    /// 主入口登记设置页时，入口怎么建成页。没给就不能登记设置页。
    pub fn with_settings_loader(self, loader: SettingsPageLoader) -> Self {
        self.0.borrow_mut().settings_loader = Some(loader);
        self
    }

    /// 设置页那份脚本的身份：读写的是同一个插件的库、执行的是同样的命令，但 `register_*`
    /// 一律报错。
    pub fn for_settings_page(id: &str, name: &str) -> Self {
        let plugin = Self::for_plugin(id, name);
        plugin.0.borrow_mut().settings_page = true;
        plugin
    }

    /// 登记前先过这一关：设置页那份脚本不能登记。
    fn may_register(&self, name: &str) -> Result<(), HostError> {
        if self.0.borrow().settings_page {
            return Err(HostError::new(format!(
                "{name}：设置页入口里不能登记，登记放在插件主入口里"
            )));
        }
        Ok(())
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
        let themes = self.clone();
        let layouts = self.clone();
        let openers = self.clone();
        let settings = self.clone();
        HostModule::new(MODULE)
            .declarations(DECLARATIONS)
            .function("active_context", |_| {
                let context = in_app("active_context", |cx| crate::active_context(cx))?;
                let project = context.project.map_or(HostValue::Null, |project| {
                    HostObject::new()
                        .field("machine", machine_value(&project.machine))
                        .field("root", project.root.as_str())
                        .into()
                });
                let session = context.session.map_or(HostValue::Null, |session| {
                    HostObject::new()
                        .field("machine", machine_value(&session.machine))
                        .field("id", session.id.as_str())
                        .field("at_latest", session.at_latest)
                        .into()
                });
                Ok(HostObject::new()
                    .field("project", project)
                    .field("session", session)
                    .into())
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
            .function("session_states", |arguments| {
                let machine = machine_argument("session_states", arguments, 0)?;
                let states = in_app("session_states", |cx| {
                    crate::host(cx).session_states(&machine, cx)
                })?;
                serialized("session_states", &states)
            })
            .function("workspace_directory", |arguments| {
                let machine = machine_argument("workspace_directory", arguments, 0)?;
                let directory = in_app("workspace_directory", |cx| {
                    crate::host(cx).workspace_directory(&machine, cx)
                })?;
                serialized("workspace_directory", &directory)
            })
            .function("attention", |_| {
                let attention = in_app("attention", |cx| crate::host(cx).attention(cx))?;
                serialized("attention", &attention)
            })
            .function("view_shown", |arguments| {
                let id = arguments.string(0)?;
                Ok(in_app("view_shown", |cx| crate::host(cx).view_shown(id, cx))?.into())
            })
            .function("window_active", |_arguments| {
                Ok(in_app("window_active", |cx| crate::host(cx).window_active(cx))?.into())
            })
            .function("overlay_open", |_arguments| {
                Ok(in_app("overlay_open", |cx| crate::host(cx).overlay_open(cx))?.into())
            })
            .function("register_themes", move |arguments| {
                themes.may_register("register_themes")?;
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
                layouts.may_register("register_layouts")?;
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
                commands.may_register("register_commands")?;
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
            .async_function("run", |arguments| {
                let command = arguments.string(0)?.to_string();
                let args = arguments.get(1).map(json_value).unwrap_or(Value::Null);
                in_task(
                    "run",
                    move |cx| crate::run(&command, args, cx),
                    |value| host_value(&value),
                )
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
            .async_function("open_file", |arguments| {
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
                in_task(
                    "open_file",
                    move |cx| crate::open_file(location, cx),
                    |()| HostValue::Null,
                )
            })
            .function("register_file_opener", move |arguments| {
                openers.may_register("register_file_opener")?;
                let name = arguments.string(0)?;
                if opener_owner.is_empty() {
                    return Err(HostError::new("插件没有命令来源身份"));
                }
                in_app("register_file_opener", |cx| {
                    crate::register_file_opener(&opener_owner, name, cx)
                })?;
                Ok(HostValue::Null)
            })
            .function("register_settings_pages", move |arguments| {
                settings.may_register("register_settings_pages")?;
                let (owner, loader) = {
                    let state = settings.0.borrow();
                    (state.id.clone(), state.settings_loader.clone())
                };
                if owner.is_empty() {
                    return Err(HostError::new("插件没有设置页来源身份"));
                }
                let loader = loader.ok_or_else(|| {
                    HostError::new("register_settings_pages：这个插件不能登记设置页")
                })?;
                let specs = settings_page_arguments(arguments, &loader)?;
                in_app("register_settings_pages", |cx| {
                    crate::register_settings_pages(&owner, specs, cx)
                })?
                .map_err(HostError::new)?;
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
            .async_function("next_command", move |_| Ok(calls.next_command()))
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
        if state.queue.contains(&event) {
            return;
        }
        // 快照、积压之前同一台机器同一个话题没取走的，被它取代了。
        if let HostEvent::RuntimeTopic {
            machine,
            topic,
            message: TopicMessage::Snapshot(_) | TopicMessage::Lagged,
        } = &event
        {
            state.queue.retain(|queued| {
                !matches!(queued, HostEvent::RuntimeTopic { machine: m, topic: t, .. }
                    if m == machine && t == topic)
            });
        }
        state.queue.push_back(event);
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
                Ok(()) => return Handled::ok(),
                // 等的那个 Promise 已经没人要了，照常排队。
                Err(call) => call,
            },
            None => call,
        };
        // 每次执行都要到，不去重。
        state.calls.push_back(call);
        Handled::ok()
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

/// 起一个宿主任务，结果经 `convert` 用 Promise 交回。gpui 的任务只能在主线程上等，脚本
/// 那边要的是能跨线程的 future，中间用一个 oneshot 接。
fn in_task<T: Send + 'static>(
    name: &'static str,
    start: impl FnOnce(&mut App) -> gpui::Task<Result<T, String>>,
    convert: impl FnOnce(T) -> HostValue + Send + 'static,
) -> Result<impl Future<Output = Result<HostValue, HostError>> + Send, HostError> {
    let (sender, receiver) = oneshot::channel::<Result<T, String>>();
    in_app(name, |cx| {
        let task = start(cx);
        cx.spawn(async move |_| {
            let _ = sender.send(task.await);
        })
        .detach();
    })?;
    Ok(async move {
        receiver
            .await
            .unwrap_or_else(|_| Err(format!("{name}：没有执行完")))
            .map(convert)
            .map_err(HostError::new)
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

/// `register_settings_pages` 的参数。入口交给宿主的加载器校验、变成建页函数。
fn settings_page_arguments(
    arguments: &HostArguments,
    loader: &SettingsPageLoader,
) -> Result<Vec<SettingsPageSpec>, HostError> {
    let invalid = |what: &str| HostError::new(format!("register_settings_pages：{what}"));
    arguments
        .value(0)?
        .as_array()
        .ok_or_else(|| invalid("参数要是设置页对象的数组"))?
        .iter()
        .map(|page| {
            let text = |name: &str| match page.get(name) {
                None | Some(HostValue::Null) => Ok(None),
                Some(HostValue::Str(text)) => Ok(Some(text.clone())),
                Some(other) => Err(invalid(&format!(
                    "{name} 要是字符串，收到 {}",
                    other.describe()
                ))),
            };
            let name = text("name")?.ok_or_else(|| invalid("每一页都要有 name"))?;
            let title = text("title")?.ok_or_else(|| invalid("每一页都要有 title"))?;
            let entry = text("entry")?.ok_or_else(|| invalid("每一页都要有 entry"))?;
            let group = match text("group")?.as_deref() {
                None | Some("plugins") => SettingsGroup::Plugins,
                Some("general") => SettingsGroup::General,
                Some("agents") => SettingsGroup::Agents,
                Some("system") => SettingsGroup::System,
                Some(other) => {
                    return Err(invalid(&format!(
                        "group 只能是 general、agents、system、plugins，收到「{other}」"
                    )));
                }
            };
            let keywords = match page.get("keywords") {
                None | Some(HostValue::Null) => Vec::new(),
                Some(HostValue::Array(words)) => words
                    .iter()
                    .map(|word| {
                        word.as_str()
                            .map(|word| SharedString::from(word.to_string()))
                            .ok_or_else(|| invalid("keywords 要是字符串数组"))
                    })
                    .collect::<Result<_, _>>()?,
                Some(other) => {
                    return Err(invalid(&format!(
                        "keywords 要是字符串数组，收到 {}",
                        other.describe()
                    )));
                }
            };
            let build = (loader.load)(&entry)
                .map_err(|error| invalid(&format!("「{name}」的 entry：{error}")))?;
            Ok(SettingsPageSpec {
                name: name.into(),
                title: title.into(),
                icon: loader.icon.clone(),
                group,
                keywords,
                build,
            })
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

/// 宿主的数据转成给脚本的值。
fn serialized(name: &str, value: &impl serde::Serialize) -> Result<HostValue, HostError> {
    serde_json::to_value(value)
        .map(|value| host_value(&value))
        .map_err(|error| HostError::new(format!("{name}：转不成 JSON：{error}")))
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

// ---------------------------------------------------------------- 事件

fn event_value(event: &HostEvent) -> HostValue {
    let machine = machine_value;
    let object = match event {
        HostEvent::RuntimeTopic {
            machine: key,
            topic,
            message,
        } => {
            let message = match message {
                TopicMessage::Event(payload) => HostObject::new()
                    .field("kind", "event")
                    .field("payload", host_value(payload)),
                TopicMessage::Snapshot(payload) => HostObject::new()
                    .field("kind", "snapshot")
                    .field("payload", host_value(payload)),
                TopicMessage::Lagged => HostObject::new().field("kind", "lagged"),
            };
            HostObject::new()
                .field("type", "runtime_topic")
                .field("machine", machine(key))
                .field("topic", topic.as_str())
                .field("message", message)
        }
        HostEvent::Reconnected(key) => HostObject::new()
            .field("type", "reconnected")
            .field("machine", machine(key)),
        HostEvent::ProjectChanged { machine: key, root } => HostObject::new()
            .field("type", "project_changed")
            .field("machine", machine(key))
            .field("root", root.as_str()),
        HostEvent::WindowActivated => HostObject::new().field("type", "window_activated"),
        HostEvent::OverlayChanged => HostObject::new().field("type", "overlay_changed"),
        HostEvent::MachinesChanged => HostObject::new().field("type", "machines_changed"),
        HostEvent::ActiveContextChanged => {
            HostObject::new().field("type", "active_context_changed")
        }
        HostEvent::SessionsChanged => HostObject::new().field("type", "sessions_changed"),
        HostEvent::AttentionChanged => HostObject::new().field("type", "attention_changed"),
        HostEvent::AgentSettingsChanged => {
            HostObject::new().field("type", "agent_settings_changed")
        }
        HostEvent::ShownViewsChanged => HostObject::new().field("type", "shown_views_changed"),
        HostEvent::ViewsChanged => HostObject::new().field("type", "views_changed"),
        HostEvent::ThemesChanged => HostObject::new().field("type", "themes_changed"),
        HostEvent::LayoutsChanged => HostObject::new().field("type", "layouts_changed"),
        HostEvent::CommandsChanged => HostObject::new().field("type", "commands_changed"),
        HostEvent::SettingsPagesChanged => {
            HostObject::new().field("type", "settings_pages_changed")
        }
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
    fn run_requires_a_command_id_and_a_script_context() {
        let module = JsPlugin::new().module();
        for arguments in [
            HostArguments::default(),
            HostArguments::new([HostValue::Null]),
            HostArguments::new([HostValue::Number(1.0)]),
        ] {
            assert!(module.begin("run", &arguments).is_err());
        }
        let outside_script = module
            .begin(
                "run",
                &HostArguments::new([HostValue::from("smelt.app/notify")]),
            )
            .err()
            .expect("测试里没有脚本上下文");
        assert!(outside_script.message().contains("只能在视图"));
    }

    #[test]
    fn host_queries_check_their_arguments_before_entering_app() {
        let module = JsPlugin::new().module();
        for query in ["agent_settings", "session_states", "workspace_directory"] {
            for machine in [HostValue::Bool(true), HostValue::Number(1.0)] {
                let error = module
                    .call(query, &HostArguments::new([machine]))
                    .unwrap_err();
                assert!(error.message().contains("机器要是 null 或字符串"));
            }
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

    fn object(fields: &[(&str, HostValue)]) -> HostValue {
        let mut object = HostObject::new();
        for (name, value) in fields {
            object = object.field(*name, value.clone());
        }
        object.into()
    }

    /// 只认 `settings.tsx`，记下问过哪些入口。
    fn loader(asked: Rc<RefCell<Vec<String>>>) -> SettingsPageLoader {
        SettingsPageLoader {
            icon: "icons/puzzle.svg".into(),
            load: Rc::new(move |entry| {
                asked.borrow_mut().push(entry.to_string());
                if entry == "settings.tsx" {
                    Ok(Rc::new(|_, _| unreachable!("登记时不建页")))
                } else {
                    Err(format!("插件目录里没有 {entry}"))
                }
            }),
        }
    }

    #[test]
    fn settings_page_registration_rejects_invalid_arguments_before_entering_app() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let module = JsPlugin::for_plugin("example", "示例")
            .with_settings_loader(loader(asked.clone()))
            .module();
        let page = |fields: &[(&str, HostValue)]| HostValue::Array(vec![object(fields)]);
        let valid = [
            ("name", HostValue::from("prefs")),
            ("title", "偏好".into()),
            ("entry", "settings.tsx".into()),
        ];
        let with = |extra: (&str, HostValue)| {
            let mut fields = valid.to_vec();
            fields.push(extra);
            page(&fields)
        };
        for arguments in [
            HostArguments::default(),
            HostArguments::new([HostValue::Null]),
            HostArguments::new([HostValue::Array(vec![HostValue::Null])]),
            HostArguments::new([page(&valid[..2])]),
            HostArguments::new([page(&[valid[0].clone(), valid[2].clone()])]),
            HostArguments::new([with(("group", "sidebar".into()))]),
            HostArguments::new([with(("keywords", "通知".into()))]),
            HostArguments::new([with((
                "keywords",
                HostValue::Array(vec![HostValue::Number(1.0)]),
            ))]),
            HostArguments::new([page(&[
                valid[0].clone(),
                valid[1].clone(),
                ("entry", "missing.tsx".into()),
            ])]),
        ] {
            let error = module
                .call("register_settings_pages", &arguments)
                .unwrap_err();
            assert!(
                !error.message().contains("只能在视图"),
                "{}",
                error.message()
            );
        }
        // 参数对了才进 App；测试里没有 App。入口在进 App 前就交给加载器校验过。
        let error = module
            .call(
                "register_settings_pages",
                &HostArguments::new([with(("group", "general".into()))]),
            )
            .unwrap_err();
        assert!(
            error.message().contains("只能在视图"),
            "{}",
            error.message()
        );
        assert_eq!(
            asked.borrow().last().map(String::as_str),
            Some("settings.tsx")
        );
    }

    #[test]
    fn settings_pages_need_a_loader() {
        let module = JsPlugin::for_plugin("example", "示例").module();
        let error = module
            .call(
                "register_settings_pages",
                &HostArguments::new([HostValue::Array(Vec::new())]),
            )
            .unwrap_err();
        assert!(
            error.message().contains("不能登记设置页"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_settings_page_script_cannot_register_anything() {
        let module = JsPlugin::for_settings_page("example", "示例")
            .with_settings_loader(loader(Rc::default()))
            .module();
        for name in [
            "register_themes",
            "register_layouts",
            "register_commands",
            "register_settings_pages",
        ] {
            let error = module
                .call(name, &HostArguments::new([HostValue::Array(Vec::new())]))
                .unwrap_err();
            assert!(error.message().contains("设置页入口里不能登记"), "{name}");
        }
        let error = module
            .call("register_file_opener", &HostArguments::new(["open".into()]))
            .unwrap_err();
        assert!(error.message().contains("设置页入口里不能登记"));
    }

    #[test]
    fn command_calls_are_queued_in_order_and_stop_after_release() {
        let plugin = JsPlugin::for_plugin("example", "示例");
        let args = serde_json::json!({ "n": 1 });
        assert!(!plugin.deliver_command("a", &args).is_pass());
        assert!(!plugin.deliver_command("a", &args).is_pass());
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
        assert!(plugin.deliver_command("a", &args).is_pass());
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
        plugin.deliver(&HostEvent::ActiveContextChanged);
        plugin.deliver(&HostEvent::ActiveContextChanged);
        plugin.deliver(&HostEvent::MachinesChanged);
        assert_eq!(
            plugin.0.borrow().queue,
            [HostEvent::ActiveContextChanged, HostEvent::MachinesChanged]
        );
    }

    /// 话题事件按顺序排着；快照到了，同一台机器同一个话题之前排着的就不要了。
    #[test]
    fn a_topic_snapshot_supersedes_queued_messages_of_that_topic() {
        let topic =
            |machine: RuntimeKey, topic: &str, message: TopicMessage| HostEvent::RuntimeTopic {
                machine,
                topic: topic.to_string(),
                message,
            };
        let plugin = JsPlugin::new();
        let first = topic(
            RuntimeKey::Local,
            "attention.changed",
            TopicMessage::Event(serde_json::json!({ "revision": 1 })),
        );
        let other_machine = topic(
            RuntimeKey::Remote("box".into()),
            "attention.changed",
            TopicMessage::Event(serde_json::json!({ "revision": 1 })),
        );
        let other_topic = topic(RuntimeKey::Local, "history.changed", TopicMessage::Lagged);
        let snapshot = topic(
            RuntimeKey::Local,
            "attention.changed",
            TopicMessage::Snapshot(serde_json::json!({ "revision": 2, "sessions": [] })),
        );
        for event in [&first, &other_machine, &other_topic, &snapshot] {
            plugin.deliver(event);
        }
        assert_eq!(
            plugin.0.borrow().queue,
            [other_machine, other_topic, snapshot.clone()]
        );
        let value = event_value(&snapshot);
        let message = value.get("message").expect("带着消息");
        assert_eq!(
            message.get("kind").and_then(HostValue::as_str),
            Some("snapshot")
        );
        assert!(message.get("payload").is_some());
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
