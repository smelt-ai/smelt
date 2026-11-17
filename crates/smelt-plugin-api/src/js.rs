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

pub mod ui;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use futures_channel::oneshot;
use gpui::{App, Entity, SharedString, Subscription};
use gpui_shell::{
    HostArguments, HostError, HostModule, HostObject, HostResult, HostValue, ScriptView,
};
use serde_json::Value;

use crate::client::protocol::control::ControlMethod;
use crate::model::daemon_state::RuntimePresence;
use crate::{
    CommandSpec, ContextMenuAction, ContextMenuItem, Handled, HostEvent, MenuEntry, MenuSlot,
    NotificationPermission, RuntimeKey, SettingsEntry, SettingsGroup, TopicMessage, ViewBuilder,
    ViewSpec, rusqlite,
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
/** 主窗口里露着的一段会话。 */
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
  /** 露着的会话；会话视图被挡住、收起、换成别的页，或没有会话时为 null。 */
  session: ViewedSession | null;
}
/** 用户眼下在看什么。变了发 active_context_changed。 */
export function active_context(): ActiveContext;
/**
 * 发布用户眼下在看什么：画项目和会话的插件在用户选中、切换时调，别的插件靠 active_context 跟着。和上次一样就什么都
 * 不做，变了发 active_context_changed。session 不给 at_latest 算 true。
 */
export function set_active_context(context: ActiveContext): null;
/** 那台机器的运行时报的会话状态，每格终端、每段对话一条（形状同运行时 subscribe 推的会话状态）。主窗口关着也照常更新；变了发 sessions_changed。 */
export function session_states(machine: Machine): unknown[];
/** 那台机器的工作区目录：开着的项目和对话，是会话的名册（形状同运行时的 workspace 目录）；还没收到时为 null。变了发 sessions_changed。 */
export function workspace_directory(machine: Machine): unknown | null;
/**
 * 系统通知的授权：checking 正在读系统设置，not_requested 还没问过用户（第一次发通知时系统会问，也可以用
 * "smelt.app/request-notification-permission" 先问），granted 允许了（banners 弹横幅、sound 声音、badge Dock
 * 角标是用户给这个应用开着的几项，关掉的系统不做），denied 用户在系统设置里关了（用
 * "smelt.app/open-notification-settings" 带他去开），unavailable 系统报错（error 是原因），unsupported 这个系统上
 * 没有系统通知。应用回到前台时重查；变了发 notification_permission_changed。
 */
export function notification_permission():
  | { state: "checking" | "not_requested" | "denied" | "unsupported" }
  | { state: "granted"; banners: boolean; sound: boolean; badge: boolean }
  | { state: "unavailable"; error: string };
/** 菜单栏菜单里的一项。icon 只取 PNG（字节数组）的透明度当形状，涂成 color（0xRRGGBB）。 */
export type StatusMenuItem =
  | { type: "action"; label: string; icon?: { png: number[]; color: number } | null; command: string; args?: unknown }
  | { type: "separator" };
/**
 * 那个视图此刻看得见；设置页是设置开着、停在这一页。变了发 shown_views_changed。
 * 视图 id 是 "<来源>/<名字>"：插件主入口是 "<插件 id>/main"，设置页是 "<插件 id>/<name>"。
 */
export function view_shown(view: string): boolean;
/** 那个视图摆在某一页眼下装的布局里（不管露没露着）。没摆的视图打开时放进出口。变了发 laid_out_views_changed。 */
export function view_laid_out(view: string): boolean;
/** 主窗口此刻在前台。回到前台发 window_activated，去后台不发：定时做事的醒来时问这里，不在前台就停。 */
export function window_active(): boolean;
/** 主窗口里宿主的遮罩层（确认框、命令面板、大图预览）开着。开关时发 overlay_changed。 */
export function overlay_open(): boolean;
/** 替换本插件由脚本登记的主题，每项是一份 smelt 主题文件 JSON（带 version，格式见 docs/ui-design-language.md 第 2 节）。有一份不合法就整批不装。空数组撤下全部。 */
export function register_themes(themeSets: string[]): null;
export interface Page {
  /** 本插件内的名字，不含 "/"；页 id 是 "<插件 id>/<name>"。 */
  name: string;
  /** 给人看的名字。 */
  title: string;
  /** 启动时进这一页；好几个插件都说是首页时登记在前的算。默认 false。 */
  home?: boolean;
  /** 出口（布局里摆的 "smelt/outlet"）没放东西时显示的视图 id。布局里没摆出口的页不用给。 */
  index?: string;
}
/**
 * 替换本插件登记的页。主窗口一次显示一页，每页按给它登记的布局摆。布局里可以摆一个出口 "smelt/outlet"：
 * 打开哪页都没摆的视图时放进出口，布局的其余部分不动。打开摆在别的页里的视图就去那一页，关掉回来的那页。
 * 空数组撤下全部；一页不合法整组不登记。
 */
export function register_pages(pages: Page[]): null;
export interface Layout {
  /** 给哪一页，页 id "<来源>/<名字>"，可以是别的插件的页。 */
  page: string;
  /** 本插件内的名字，不含 "/"；布局 id 是 "<插件 id>/<name>"。 */
  name: string;
  /** 设置里显示的名字。 */
  title: string;
  /**
   * gpui-base DockAreaState JSON，面板名是视图 id，version 为 1。叶子也可以是一摞 "smelt/stack"：
   * info.panel 写 { axis: "vertical" | "horizontal", items: [{ view: "<视图 id>", fit?: true } | { stack: {...}, fit?: true }] }，
   * 几个视图按顺序排在一格里，没有分隔条，fit 的按内容定大小，其余平分；不分标签、不能单独放大。
   */
  layout: string;
}
/** 替换本插件登记的布局。空数组撤下全部；一份不合法整组不登记。首页用哪份由用户在设置里选，别的页用第一份。 */
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
  /** 这一次执行，交回结果时用（resolve_command / reject_command）。 */
  id: number;
  /** 本插件内的名字。 */
  name: string;
  /** 执行时带的参数；快捷键、菜单、命令面板执行时为 null。 */
  args: unknown;
}
/**
 * 等本插件命令的下一次执行。从登记起开始记，按顺序一次不落。同时只能有一个在等。每次执行都要用 resolve_command 或
 * reject_command 交回，执行的一方（run 的 Promise）等着它；插件卸下时还没交回的都算失败。
 */
export function next_command(): Promise<CommandCall>;
/** 交回 id 那次执行的结果，run 它的一方兑现为 result（不给是 null）。已经交回过、没有这一次时报错。 */
export function resolve_command(id: number, result?: unknown): null;
/** 那次执行失败，run 它的一方 reject，reason 是给人看的原因。 */
export function reject_command(id: number, reason: string): null;
/**
 * 执行一条命令，任何来源的都行（id 是 "<来源>/<名字>"），兑现为命令交回的结果。排到这一轮之后跑；
 * 主窗口没开也照样执行，要主窗口的命令这时 reject。命令不存在、眼下不适用、执行失败时 reject，带可以直接
 * 给人看的原因。不 await 命令也照样跑完。
 * 脚本登记的命令兑现为脚本交回的结果（见 resolve_command）。
 *
 * 宿主的动作也是命令：
 * - "smelt.app/notify" { id?: string, subtitle: string, body: string, on_click?: { command: string, args?: unknown } }：
 *   发系统通知，显示受系统通知授权和设置影响。给了 id（以插件 id 开头，"<插件 id>/…"）的留在通知中心里由插件管：
 *   同一个 id 再发换掉上一条，可以撤回，应用重启后也认得；不给是一次性的提示。on_click 是点它时执行的命令，
 *   不给就只是把应用拿到前面来。
 * - "smelt.app/withdraw-notification" { id: string }：撤回给了 id 的通知；没有这条也不算错。
 * - "smelt.app/open-notification-settings"：打开系统设置里本应用的通知设置；只有 macOS 有。
 * - "smelt.app/request-notification-permission"：还没问过用户时请系统问一次通知授权；问过了什么都不做，结果看
 *   notification_permission_changed。
 * - "smelt.app/open-in-text-editor" { path: string }：用系统文本编辑器打开这台电脑上的文件。
 * - "smelt.app/pick-paths" { machine: Machine, title: string, confirm: string, files?: boolean, multiple?: boolean }
 *   → string[] | null：在那台机器上选路径，title 是选择框标题，confirm 是确认按钮上的动词；取消时为 null。
 * - "smelt.app/open-view" { view: string, args?: unknown } → boolean：打开一个视图，哪页的布局里摆了它就去那页
 *   让它露出来，哪页都没摆就放进出口，再把 args 交给它；没有这个视图时为 false。按实例建的视图（args 是提供它的
 *   插件的约定里的形状）每次按 args 新建一份放进出口，出口换了内容就释放；没有能放的出口时 reject。
 * - "smelt.app/quit-now"：直接退出，不问退出守卫、不弹退出确认。接手了退出（set_quit_guard）的插件做完自己的事用它。
 * - "smelt.app/open-page" { page: string } → boolean：进一页，来路上有它就退回去；没有这页、这页没有布局时为 false。
 * - "smelt.app/show-main-window"：把主窗口拿到前面来，已经关了就新开一扇。
 * - "smelt.app/set-status-menu" { title?: string, tooltip?: string, items?: StatusMenuItem[] }：设置菜单栏图标旁的字、
 *   提示和点开的菜单，后设的为准；只有 macOS 有。
 * - "smelt.app/set-dock-badge" { label: string | null }：设置 Dock 图标的角标，null 清掉；只有 macOS 有。
 * - "smelt.app/set-view-badge" { view: string, label: string | null }：设视图入口（面板标签、侧栏那一行）上的角标，
 *   null 清掉；视图建没建都画，变了发 view_badges_changed。
 * - "smelt.app/open-in-file-manager" { path: string }：用系统文件管理器打开这台电脑上的一个目录。
 * - "smelt.app/add-machine" { pairing_uri: string } → { machine: Machine, name: string }：用配对串添加一台远程机器，
 *   要走公网拨号，最长等半分钟；同一台机器重新配对就更新原来那条。
 * - "smelt.app/remove-machine" { machine: Machine }：移除一台远程机器，它的项目和会话从这边移走，它上面的会话照常运行。
 * - "smelt.app/rename-machine" { machine: Machine, name: string }：改远程机器在这台电脑上的显示名。
 * - "smelt.app/set-machine-relay" { machine: Machine, relay: string }：改连它用的中继，空的是 n0 的公共 relay；
 *   在线时先请它自己改用新中继，离线时只改这边。
 * - "smelt.app/reconnect-machine" { machine: Machine }：马上重连一台离线或版本对不上的机器。
 * - "smelt.app/upgrade-machine" { machine: Machine }：请它把运行时升到这个桌面的版本，进度看 connections。
 * - "smelt.app/refresh-connections"：有人在看连接状态了，现在重读连接路径和延迟、重探离线的；露出来时调一次。
 */
export function run(command: string, args?: unknown): Promise<unknown>;
/** 那台机器上的路径怎么切。 */
export function path_style(machine: Machine): "posix" | "windows";
/**
 * 调那台机器的运行时控制接口：method 是线上方法名（如 "session.list"），params 是它的参数（不给是 {}），兑现为
 * 运行时交回的结果。参数、结果的形状见 smelt-runtime-client 里那个方法的定义。运行时报的错、连不上、超时（timeout_ms，
 * 默认 60 秒）都 reject，带可以直接给人看的原因；没有这个方法当场报错。
 */
export function call(machine: Machine, method: string, params?: unknown, timeout_ms?: number): Promise<unknown>;
/** 桌面挂着的机器，本机在最前。unavailable 是眼下用不了的原因；smelt_home 是那台机器的 SMELT_HOME，没连上过是 null。变了发 machines_changed。 */
export function machines(): { machine: Machine; name: string; unavailable: string | null; smelt_home: string | null }[];
/** 登记了的一个视图（见 views）。 */
export interface ViewInfo {
  /** "<来源>/<名字>"。 */
  id: string;
  title: string;
  /** 图标资源路径，如 "icons/terminal.svg"。 */
  icon: string;
  /** 视图自己画顶栏，宿主不给它加标题栏。 */
  draws_top_bar: boolean;
  /** 按实例建：每次打开按参数新建一份（参数形状见提供方的约定）。 */
  instances: boolean;
  /** 是一页设置时：放在哪一组、按哪些词能搜到；不是为 null。 */
  settings: { group: "general" | "agents" | "system" | "plugins"; keywords: string[] } | null;
}
/** 所有来源登记的视图，按登记先后。变了发 views_changed。 */
export function views(): ViewInfo[];
/** 视图入口上的角标（"smelt.app/set-view-badge" 设的）；没有为 null。变了发 view_badges_changed。 */
export function view_badge(view: string): string | null;
/** 一个角被宿主控件占掉的宽高。 */
export interface CornerSize { width: number; height: number }
/**
 * 视图的 safe area：title_bar 是顶上一行是窗口标题栏（34 高，空白处拖窗口），corners 是四角被宿主控件
 * （红黄绿按钮、停靠区开合按钮）占的宽高。外壳替它画标题带的视图顶上不是标题栏，JS 视图都是这种，
 * 自己的总是全 0。视图还没建、没摆着时也全为 0。变了发 safe_area_changed。
 */
export function safe_area(view: string): {
  title_bar: boolean;
  corners: { top_left: CornerSize; top_right: CornerSize; bottom_right: CornerSize; bottom_left: CornerSize };
};
/** 所有来源登记的页（形状同 register_pages，id 是 "<来源>/<名字>"），按登记先后。变了发 pages_changed。 */
export function pages(): { id: string; title: string; home: boolean; index: string | null }[];
/** 登记了的一条命令（见 commands）。key 是登记时给的出厂快捷键（用户可能在设置里改了），没有为 null；group 是来源给人看的名字。 */
export interface CommandInfo {
  id: string;
  group: string;
  title: string;
  key: string | null;
  context: string;
  palette: boolean;
  requires_args: boolean;
}
/** 所有来源登记的命令，按登记先后。变了发 commands_changed。 */
export function commands(): CommandInfo[];
/** 一条命令；没登记为 null。 */
export function command(id: string): CommandInfo | null;
/** 画右键菜单时取的一项：args 已经并上 context，点了直接 run(command, args)。 */
export type ContextMenuEntry = {
  label: string;
  png: number[] | null;
  icon: string | null;
} & ({ command: string; args: Record<string, unknown> } | { submenu: ContextMenuEntry[] });
/**
 * 画自己的右键菜单 menu（"<本插件 id>/<名字>"）时取别的插件放进来的项（register_context_menu_items），只列在 machine
 * 上的东西该有的；context 是这个菜单带的上下文，形状由本插件在约定里写明。打开菜单时取，不要缓存。
 */
export function context_menu_items(menu: string, machine: Machine, context?: Record<string, unknown> | null): ContextMenuEntry[];
/** 那台机器此刻在线：最近一次探测连得上、版本对得上；本机守护重启、升级的那一会儿也不在线。变了发 machines_changed。 */
export function machine_online(machine: Machine): boolean;
/** 一台远程机器的连接（见 connections）。 */
export interface Connection {
  machine: Machine;
  /** 在这台电脑上的显示名。 */
  name: string;
  /** 这台电脑经哪个中继连它；null 是 n0 的公共 relay。 */
  relay: string | null;
  /** 它跑的系统（"linux"、"macos"、"windows"），最近一次连上时报的，离线也在；从没连上过是 null。 */
  os: string | null;
  state:
    | { state: "connecting" }
    | { state: "online"; version: string; platform: string | null }
    | { state: "offline"; error: string }
    | { state: "incompatible"; version: string; detail: string }
    | { state: "upgrading"; progress: string };
  /** 连着时走哪条路、多慢；slow 是慢到值得提醒。只在有人看时采样（"smelt.app/refresh-connections"）。 */
  link: {
    route: { kind: "direct" } | { kind: "relay"; relay: string } | { kind: "other" };
    rtt_ms: number;
    slow: boolean;
  } | null;
  /** 最近一次手动升级没升成的原因。 */
  upgrade_note: string | null;
  can_upgrade: boolean;
  can_reconnect: boolean;
}
/**
 * 远程机器的连接表：配对了哪些、各自连得怎样；本机不在里面。load_error 是启动时没读出登记过的机器的原因。
 * 改用 "smelt.app/add-machine" 等命令。变了发 machines_changed。
 */
export function connections(): { remotes: Connection[]; load_error: string | null };
/**
 * 等那台机器在线，兑现为 true；机器被移除了为 false。等的时候不拨号，跟着宿主探到的状态醒。failures 是在线以后又连
 * 失败的次数，大于 0 时先按它退避（250ms 起翻倍，上限 8s）再看，免得对着在线却出错的机器连环重试。
 */
export function wait_online(machine: Machine, failures?: number): Promise<boolean>;
/**
 * 那台机器当前这一轮会话快照里，这个会话的运行时还在不在：live 活着；left_over 守护停过，只剩身份，按原 id 重新打开
 * 能续接；gone 没了。订阅还没就绪、判断不了为 null。变了发 sessions_changed。
 */
export function session_presence(machine: Machine, session_id: string): "live" | "left_over" | "gone" | null;
export interface MenuItem {
  /** app 放进应用菜单（「Smelt」，版本号下面、「设置…」上面），help 放进「帮助」菜单。 */
  slot: "app" | "help";
  title: string;
  /** 点了执行的命令 id（"<来源>/<名字>"），不带参数。 */
  command: string;
}
/** 替换本插件放进菜单栏菜单的项。空数组撤下全部；一项不合法整组不登记。 */
export function register_menu_items(items: MenuItem[]): null;
/** 右键菜单里的一项（见 register_context_menu_items）：点了执行 command，或者展开 submenu。 */
export type ContextMenuItem = {
  label: string;
  /** 彩色图标（如应用自己的图标），PNG 字节；有它就不画 icon。 */
  png?: number[] | null;
  /** 线性图标的资源路径，如 "icons/external-link.svg"；png、icon 都没有就不画图标。 */
  icon?: string | null;
  /** 只在这台机器的东西上列（{ machine: null } 是只在本机）；不给是哪台都列。 */
  only_on?: { machine: Machine } | null;
} & ({ command: string; args?: Record<string, unknown> | null } | { submenu: ContextMenuItem[] });
/**
 * 往别的插件画的右键菜单 menu 里放本插件的项。menu 是 "<来源>/<名字>"，菜单名和它带的上下文见画它的插件的约定。
 * 整组替换本插件在这个菜单里原来的项，空数组撤下。点了执行 command，参数是 args 并上菜单的上下文，同名字段以上下文
 * 为准；命令撤下了那一项就不列。一项不合法整组不登记。
 */
export function register_context_menu_items(menu: string, items: ContextMenuItem[]): null;
/**
 * 接手用户要退出（⌘Q、菜单「退出 Smelt」）：用户要退出时先执行本插件的命令 name（要另外登记，不带参数），宿主不弹
 * 自己的退出确认、也不退出。插件做完自己的事用 run("smelt.app/quit-now") 退出，或者不退了。脚本没法当场回答这次
 * 接不接，所以挂着就是每次都接，用不着时传 null 撤下。插件卸下、这条命令撤下了就不再接。系统关机等不经过这里。
 */
export function set_quit_guard(name: string | null): null;
/** 打开文件，交给登记的文件打开者；没有打开者时本机文件用系统文本编辑器打开，远程的 reject。line 从 1 数。 */
export function open_file(machine: Machine, path: string, line?: number): Promise<null>;
export interface SettingsPage {
  /**
   * 本插件内的名字，不含 "/"；设置页 id 是 "<插件 id>/<name>"。"main" 不能用：那是插件主入口的视图。
   */
  name: string;
  /** 设置侧栏上的名字。 */
  title: string;
  /**
   * 页面的入口脚本，插件目录里的相对路径，如 "settings.tsx"。第一次打开这一页时单独加载、挂载它，
   * 和主入口是两份脚本实例，不共享内存：值存在插件自己的库里（store_query / store_execute），
   * 要主入口跟着变就 run 一条它登记的命令。
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
  | { type: "notification_permission_changed" }
  | { type: "shown_views_changed" }
  | { type: "laid_out_views_changed" }
  | { type: "views_changed" }
  | { type: "view_badges_changed" }
  | { type: "safe_area_changed" }
  | { type: "themes_changed" }
  | { type: "layouts_changed" }
  | { type: "pages_changed" }
  | { type: "commands_changed" }
  /** 插件发的事件（见 emit_event）：name 是 "<来源>/<名字>"，载荷形状由发的插件定。 */
  | { type: "plugin"; name: string; payload: unknown }
  /** 要在那台机器上打开这些路径（拖到 Dock 图标上的、Finder 里「用 Smelt 打开」的），绝对路径，可能是目录也可能是文件。 */
  | { type: "open_paths"; machine: Machine; paths: string[] };
/**
 * 运行时话题推来的：一条事件；带快照的话题（如 attention.changed）订上和积压后的整份状态，收到就整份换掉
 * 自己留的；断开或积压可能错过了事件（没有快照的话题自己重读）；那台机器这一轮连接上不会再推这个话题
 * （运行时太旧拒掉了订阅等，reason 给人看，重新连上时再订）。载荷形状见协议里那个话题的类型。
 */
export type TopicMessage =
  | { kind: "event"; payload: unknown }
  | { kind: "snapshot"; payload: unknown }
  | { kind: "lagged" }
  | { kind: "unavailable"; reason: string };
/**
 * 等宿主的下一条事件。从插件挂载完起开始记，没取走的同样一条只留一份（plugin、open_paths 每条都留）；一台
 * 机器一个话题的快照或积压到了，这个话题之前没取走的就不留了。同时只能有一个在等。
 */
export function next_event(): Promise<HostEvent>;
/**
 * 广播一条插件事件，各插件（Rust 内置插件也算）在 next_event 里收到 { type: "plugin", name, payload }，本插件
 * 自己也收得到。name 是 "<插件 id>/<名字>"，只能发本插件来源下的。事件只报此刻的变化，后订的收不到以前的。
 */
export function emit_event(name: string, payload?: unknown): null;
"#;

/// 执行了一次插件的命令，等脚本取走、交回结果。
#[derive(Debug)]
struct CommandCall {
    /// 交回结果时认这一次（`resolve_command` / `reject_command`）。
    id: u64,
    name: String,
    args: Value,
}

/// 宿主怎么把插件目录里的一个入口建成设置页（见 `register_settings_pages`）。
#[derive(Clone)]
pub struct SettingsPageLoader {
    /// JS 设置页在侧栏上一律用这个图标。
    pub icon: SharedString,
    /// 校验入口（插件目录里的相对路径），给出建页的函数。
    pub load: LoadSettingsPage,
}

/// 校验 JS 设置页入口、给出建页函数的宿主函数。
pub type LoadSettingsPage = Rc<dyn Fn(&str) -> Result<ViewBuilder, String>>;

/// JS 插件主入口挂成的视图，名字固定：id 是 `<插件 id>/main`。
pub const MAIN_VIEW: &str = "main";

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
    /// 执行过、脚本还没取走的命令。
    calls: VecDeque<CommandCall>,
    call_waiter: Option<oneshot::Sender<CommandCall>>,
    /// 交给脚本、还没交回结果的执行：执行的一方在等这些。
    replies: HashMap<u64, oneshot::Sender<crate::CommandResult>>,
    /// 下一次执行的编号。
    next_call: u64,
    /// 视图已经释放：它登记的命令不再接。
    released: bool,
    /// 设置页那份脚本：和主入口同一个身份，但不能登记，登记会顶掉主入口登记的那组。
    settings_page: bool,
    settings_loader: Option<SettingsPageLoader>,
    /// 脚本登记的设置页。这个来源的视图是一组：宿主挂上的主入口视图加上这些，哪边变了
    /// 都整组再登记（见 [`JsPlugin::publish_views`]）。
    settings_views: Vec<ViewSpec>,
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

    /// 主入口挂成的视图（名字是 [`MAIN_VIEW`]），和脚本登记的设置页并成一组登记。
    pub fn set_main_view(&self, spec: ViewSpec, cx: &mut App) -> Result<(), String> {
        let settings = self.0.borrow().settings_views.clone();
        self.publish_views(Some(spec), settings, cx)
    }

    /// 脚本登记的设置页换成这一组，和主入口的视图并起来整组登记。
    fn set_settings_views(&self, specs: Vec<ViewSpec>, cx: &mut App) -> Result<(), String> {
        let owner = self.0.borrow().id.clone();
        // 主入口的视图从注册表里取回来，不在这里另存一份：它的建视图函数拿着脚本视图，
        // 存在这里就成了脚本视图 → 宿主模块 → 这里 → 脚本视图的环，插件卸下也放不掉。
        let main = crate::view(&format!("{owner}/{MAIN_VIEW}"), cx).map(|info| ViewSpec {
            name: MAIN_VIEW.into(),
            title: info.title,
            icon: info.icon,
            draws_top_bar: info.draws_top_bar,
            instances: info.instances,
            settings: info.settings,
            build: info.build,
        });
        self.publish_views(main, specs.clone(), cx)?;
        self.0.borrow_mut().settings_views = specs;
        Ok(())
    }

    fn publish_views(
        &self,
        main: Option<ViewSpec>,
        settings: Vec<ViewSpec>,
        cx: &mut App,
    ) -> Result<(), String> {
        let owner = self.0.borrow().id.clone();
        if owner.is_empty() {
            return Err("插件没有视图来源身份".to_string());
        }
        crate::register_views(&owner, main.into_iter().chain(settings).collect(), cx)
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
        let page_owner = layout_owner.clone();
        let commands = self.clone();
        let calls = self.clone();
        let opener_owner = self.0.borrow().id.clone();
        let query_owner = self.0.borrow().id.clone();
        let execute_owner = query_owner.clone();
        let event_owner = query_owner.clone();
        let themes = self.clone();
        let layouts = self.clone();
        let pages = self.clone();
        let openers = self.clone();
        let settings = self.clone();
        let menus = self.clone();
        let context_menus = self.clone();
        let guards = self.clone();
        let resolved = self.clone();
        let rejected = self.clone();
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
            .function("notification_permission", |_| {
                let permission = in_app("notification_permission", |cx| {
                    crate::host(cx).notification_permission(cx)
                })?;
                Ok(permission_value(&permission))
            })
            .function("view_shown", |arguments| {
                let id = arguments.string(0)?;
                Ok(in_app("view_shown", |cx| crate::host(cx).view_shown(id, cx))?.into())
            })
            .function("view_laid_out", |arguments| {
                let id = arguments.string(0)?;
                Ok(in_app("view_laid_out", |cx| crate::host(cx).view_laid_out(id, cx))?.into())
            })
            .function("window_active", |_arguments| {
                Ok(in_app("window_active", |cx| crate::host(cx).window_active(cx))?.into())
            })
            .function("overlay_open", |_arguments| {
                Ok(in_app("overlay_open", |cx| crate::host(cx).overlay_open(cx))?.into())
            })
            .function("set_active_context", |arguments| {
                let context = active_context_argument(arguments.value(0)?)?;
                in_app("set_active_context", |cx| {
                    crate::set_active_context(context, cx)
                })?;
                Ok(HostValue::Null)
            })
            .function("machines", |_| {
                let machines = in_app("machines", |cx| crate::host(cx).machines(cx))?;
                Ok(HostValue::Array(
                    machines
                        .iter()
                        .map(|machine| {
                            HostObject::new()
                                .field("machine", machine_value(&machine.key))
                                .field("name", machine.name.as_str())
                                .field("unavailable", optional_text(machine.unavailable.as_deref()))
                                .field("smelt_home", optional_text(machine.smelt_home.as_deref()))
                                .into()
                        })
                        .collect(),
                ))
            })
            .function("views", |_| {
                let views = in_app("views", |cx| crate::views(cx))?;
                Ok(HostValue::Array(views.iter().map(view_value).collect()))
            })
            .function("view_badge", |arguments| {
                let id = arguments.string(0)?;
                let badge = in_app("view_badge", |cx| crate::view_badge(id, cx))?;
                Ok(optional_text(badge.as_deref()))
            })
            .function("safe_area", |arguments| {
                let id = arguments.string(0)?;
                let area = in_app("safe_area", |cx| {
                    crate::built_view(id, cx)
                        .map(|view| crate::safe_area(view.view().entity_id(), cx))
                        .unwrap_or_default()
                })?;
                Ok(safe_area_value(&area))
            })
            .function("pages", |_| {
                let pages = in_app("pages", |cx| crate::pages(cx))?;
                Ok(HostValue::Array(
                    pages
                        .iter()
                        .map(|page| {
                            HostObject::new()
                                .field("id", page.id.as_ref())
                                .field("title", page.title.as_ref())
                                .field("home", page.home)
                                .field("index", optional_text(page.index.as_deref()))
                                .into()
                        })
                        .collect(),
                ))
            })
            .function("commands", |_| {
                let commands = in_app("commands", |cx| crate::commands(cx))?;
                Ok(HostValue::Array(
                    commands.iter().map(command_value).collect(),
                ))
            })
            .function("command", |arguments| {
                let id = arguments.string(0)?;
                let command = in_app("command", |cx| crate::command(id, cx))?;
                Ok(command.as_ref().map_or(HostValue::Null, command_value))
            })
            .function("context_menu_items", |arguments| {
                let menu = arguments.string(0)?.to_string();
                let machine = machine_argument("context_menu_items", arguments, 1)?;
                let context = arguments.get(2).map(json_value).unwrap_or(Value::Null);
                let items = in_app("context_menu_items", |cx| {
                    crate::context_menu_items(&menu, &machine, cx)
                })?;
                Ok(HostValue::Array(
                    items
                        .iter()
                        .map(|item| context_menu_value(item, &context))
                        .collect(),
                ))
            })
            .async_function("call", |arguments| {
                let machine = machine_argument("call", arguments, 0)?;
                let name = arguments.string(1)?;
                let method = ControlMethod::from_wire_name(name)
                    .ok_or_else(|| HostError::new(format!("call：运行时没有「{name}」这个方法")))?;
                let params = match arguments.get(2) {
                    None | Some(HostValue::Null) => Value::Object(Default::default()),
                    Some(params) => json_value(params),
                };
                let timeout = match arguments.get(3) {
                    None | Some(HostValue::Null) => crate::CALL_TIMEOUT,
                    Some(HostValue::Number(ms)) if *ms > 0.0 && ms.is_finite() => {
                        std::time::Duration::from_millis(*ms as u64)
                    }
                    Some(other) => {
                        return Err(HostError::new(format!(
                            "call：timeout_ms 要是正数，收到 {}",
                            other.describe()
                        )));
                    }
                };
                in_task(
                    "call",
                    move |cx| {
                        let client = crate::host(cx).client(&machine, cx);
                        cx.background_executor().spawn(async move {
                            let client =
                                client.ok_or_else(|| crate::MACHINE_REMOVED.to_string())?;
                            client
                                .call_method(method, params, timeout)
                                .map_err(crate::call_error_text)
                        })
                    },
                    |value| host_value(&value),
                )
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
                    .map(|(page, name, title, state)| crate::LayoutSource {
                        page,
                        name,
                        title,
                        state,
                    })
                    .collect::<Vec<_>>();
                in_app("register_layouts", |cx| {
                    crate::register_layouts(&layout_owner, &sources, cx)
                })?
                .map_err(HostError::new)?;
                Ok(HostValue::Null)
            })
            .function("register_pages", move |arguments| {
                pages.may_register("register_pages")?;
                let pages = page_arguments(arguments)?;
                if page_owner.is_empty() {
                    return Err(HostError::new("插件没有页的来源身份"));
                }
                let sources = pages
                    .iter()
                    .map(|(name, title, home, index)| crate::PageSource {
                        name,
                        title,
                        home: *home,
                        index: index.as_deref(),
                    })
                    .collect::<Vec<_>>();
                in_app("register_pages", |cx| {
                    crate::register_pages(&page_owner, &sources, cx)
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
            .function("connections", |_| {
                let connections = in_app("connections", |cx| crate::host(cx).connections(cx))?;
                serialized("connections", &connections)
            })
            .function("machine_online", |arguments| {
                let machine = machine_argument("machine_online", arguments, 0)?;
                let online = in_app("machine_online", |cx| {
                    crate::host(cx).machine_online(&machine, cx)
                })?;
                Ok(online.into())
            })
            .async_function("wait_online", |arguments| {
                let machine = machine_argument("wait_online", arguments, 0)?;
                let failures = match arguments.get(1) {
                    None | Some(HostValue::Null) => 0,
                    Some(HostValue::Number(failures))
                        if *failures >= 0.0
                            && failures.fract() == 0.0
                            && *failures <= f64::from(u32::MAX) =>
                    {
                        *failures as u32
                    }
                    Some(other) => {
                        return Err(HostError::new(format!(
                            "wait_online：failures 要是不小于 0 的整数，收到 {}",
                            other.describe()
                        )));
                    }
                };
                in_task(
                    "wait_online",
                    move |cx| {
                        cx.spawn(async move |cx| {
                            Ok::<_, String>(crate::wait_online(&machine, failures, cx).await)
                        })
                    },
                    HostValue::Bool,
                )
            })
            .function("session_presence", |arguments| {
                let machine = machine_argument("session_presence", arguments, 0)?;
                let session = arguments.string(1)?.to_string();
                let presence = in_app("session_presence", |cx| {
                    crate::host(cx).session_presence(&machine, &session, cx)
                })?;
                Ok(match presence {
                    None => HostValue::Null,
                    Some(RuntimePresence::Live) => "live".into(),
                    Some(RuntimePresence::LeftOver) => "left_over".into(),
                    Some(RuntimePresence::Gone) => "gone".into(),
                })
            })
            .function("register_menu_items", move |arguments| {
                menus.may_register("register_menu_items")?;
                let entries = menu_arguments(arguments)?;
                let owner = menus.0.borrow().id.clone();
                if owner.is_empty() {
                    return Err(HostError::new("插件没有命令来源身份"));
                }
                in_app("register_menu_items", |cx| {
                    crate::register_menu_items(&owner, entries, cx)
                })?;
                Ok(HostValue::Null)
            })
            .function("register_context_menu_items", move |arguments| {
                context_menus.may_register("register_context_menu_items")?;
                let menu = arguments.string(0)?.to_string();
                let items = arguments
                    .value(1)?
                    .as_array()
                    .ok_or_else(|| {
                        HostError::new("register_context_menu_items：菜单项要是对象的数组")
                    })?
                    .iter()
                    .map(context_menu_item)
                    .collect::<Result<Vec<_>, _>>()?;
                let owner = context_menus.0.borrow().id.clone();
                if owner.is_empty() {
                    return Err(HostError::new("插件没有命令来源身份"));
                }
                in_app("register_context_menu_items", |cx| {
                    crate::register_context_menu_items(&owner, &menu, items, cx)
                })?
                .map_err(|error| HostError::new(format!("register_context_menu_items：{error}")))?;
                Ok(HostValue::Null)
            })
            .function("set_quit_guard", move |arguments| {
                guards.may_register("set_quit_guard")?;
                let name = match arguments.value(0)? {
                    HostValue::Null => None,
                    HostValue::Str(name) if !name.is_empty() && !name.contains('/') => {
                        Some(name.clone())
                    }
                    other => {
                        return Err(HostError::new(format!(
                            "set_quit_guard：要是本插件的命令名（不含 \"/\"）或 null，收到 {}",
                            other.describe()
                        )));
                    }
                };
                let owner = guards.0.borrow().id.clone();
                if owner.is_empty() {
                    return Err(HostError::new("插件没有命令来源身份"));
                }
                let guard = name.map(|name| quit_guard(&guards, format!("{owner}/{name}")));
                in_app("set_quit_guard", |cx| match guard {
                    Some(guard) => crate::register_quit_guard(&owner, guard, cx),
                    None => crate::unregister_quit_guard(&owner, cx),
                })?;
                Ok(HostValue::Null)
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
                let loader = settings.0.borrow().settings_loader.clone();
                let loader = loader.ok_or_else(|| {
                    HostError::new("register_settings_pages：这个插件不能登记设置页")
                })?;
                let specs = settings_page_arguments(arguments, &loader)?;
                in_app("register_settings_pages", |cx| {
                    settings.set_settings_views(specs, cx)
                })?
                .map_err(|error| HostError::new(format!("register_settings_pages：{error}")))?;
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
            .function("resolve_command", move |arguments| {
                let id = call_id("resolve_command", arguments)?;
                let result = arguments.get(1).map(json_value).unwrap_or(Value::Null);
                resolved.reply("resolve_command", id, Ok(result))
            })
            .function("reject_command", move |arguments| {
                let id = call_id("reject_command", arguments)?;
                let reason = arguments.string(1)?.to_string();
                rejected.reply("reject_command", id, Err(reason))
            })
            .async_function("next_event", move |_| Ok(events.next_event()))
            .function("emit_event", move |arguments| {
                let name = arguments.string(0)?;
                let payload = arguments.get(1).map(json_value).unwrap_or(Value::Null);
                let event = plugin_event(&event_owner, name, payload)?;
                in_app("emit_event", |cx| crate::emit(event, cx))?;
                Ok(HostValue::Null)
            })
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
        // 插件事件、要打开的路径每条都是一回事，不能合并：同一条排两次中间可能夹着别的。
        let each_counts = matches!(
            event,
            HostEvent::Plugin { .. } | HostEvent::OpenPaths { .. }
        );
        if !each_counts && state.queue.contains(&event) {
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
            let call = receiver
                .await
                .map_err(|_| HostError::new("next_command：被后一次 next_command 顶掉了"))?;
            Ok(HostObject::new()
                .field("id", HostValue::Number(call.id as f64))
                .field("name", call.name)
                .field("args", host_value(&call.args))
                .into())
        }
    }

    /// 宿主执行了这个插件的一条命令：排给脚本，交回等结果的那一头。视图已经释放就不接
    /// （`None`），按键交给下一条绑定。
    fn deliver_command(
        &self,
        name: &str,
        args: &Value,
    ) -> Option<oneshot::Receiver<crate::CommandResult>> {
        let mut state = self.0.borrow_mut();
        if state.released {
            return None;
        }
        let id = state.next_call;
        state.next_call += 1;
        let (reply, result) = oneshot::channel();
        state.replies.insert(id, reply);
        let call = CommandCall {
            id,
            name: name.to_string(),
            args: args.clone(),
        };
        let call = match state.call_waiter.take() {
            Some(waiter) => match waiter.send(call) {
                Ok(()) => return Some(result),
                // 等的那个 Promise 已经没人要了，照常排队。
                Err(call) => call,
            },
            None => call,
        };
        // 每次执行都要到，不去重。
        state.calls.push_back(call);
        Some(result)
    }

    /// 脚本交回一次执行的结果。
    fn reply(&self, function: &str, id: u64, result: crate::CommandResult) -> HostResult {
        let reply =
            self.0.borrow_mut().replies.remove(&id).ok_or_else(|| {
                HostError::new(format!("{function}：没有在等结果的第 {id} 次执行"))
            })?;
        // 执行的一方不等了（任务被扔掉）不算错。
        let _ = reply.send(result);
        Ok(HostValue::Null)
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
        // 等结果的那一头收到「没执行完」。
        state.replies.clear();
        state.released = true;
    }
}

/// 脚本没交回结果插件就卸下了。
const UNFINISHED_COMMAND: &str = "插件已卸下，命令没有执行完";

/// 脚本发的插件事件：名字要在本插件来源下（`<插件 id>/<名字>`）。
fn plugin_event(owner: &str, name: &str, payload: Value) -> Result<HostEvent, HostError> {
    if owner.is_empty() {
        return Err(HostError::new("emit_event：插件没有事件来源身份"));
    }
    let own = name
        .strip_prefix(owner)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|rest| !rest.is_empty());
    if !own {
        return Err(HostError::new(format!(
            "emit_event：事件名要是「{owner}/<名字>」，收到「{name}」"
        )));
    }
    Ok(HostEvent::Plugin {
        name: name.to_string().into(),
        payload,
    })
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

/// `register_pages` 的参数：每项的名字、标题、是不是首页、出口的默认内容。
type PageArgument = (String, String, bool, Option<String>);

fn page_arguments(arguments: &HostArguments) -> Result<Vec<PageArgument>, HostError> {
    let invalid = || {
        HostError::new(
            "页必须是 { name: string, title: string, home?: boolean, index?: string } 对象的数组",
        )
    };
    arguments
        .value(0)?
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|page| {
            let text = |name: &str| {
                page.get(name)
                    .and_then(HostValue::as_str)
                    .map(str::to_owned)
                    .ok_or_else(invalid)
            };
            let home = match page.get("home") {
                None | Some(HostValue::Null) => false,
                Some(HostValue::Bool(home)) => *home,
                Some(_) => return Err(invalid()),
            };
            let index = match page.get("index") {
                None | Some(HostValue::Null) => None,
                Some(HostValue::Str(index)) => Some(index.clone()),
                Some(_) => return Err(invalid()),
            };
            Ok((text("name")?, text("title")?, home, index))
        })
        .collect()
}

/// `register_layouts` 的参数：每项给的页、名字、标题、布局原文。
fn layout_arguments(
    arguments: &HostArguments,
) -> Result<Vec<(String, String, String, String)>, HostError> {
    let invalid = || HostError::new("布局必须是 { page, name, title, layout } 字符串对象的数组");
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
            Ok((
                field("page")?,
                field("name")?,
                field("title")?,
                field("layout")?,
            ))
        })
        .collect()
}

/// `register_settings_pages` 的参数。入口交给宿主的加载器校验、变成建页函数。
fn settings_page_arguments(
    arguments: &HostArguments,
    loader: &SettingsPageLoader,
) -> Result<Vec<ViewSpec>, HostError> {
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
            if name == MAIN_VIEW {
                return Err(invalid(&format!(
                    "设置页不能叫「{MAIN_VIEW}」，那是插件主入口的视图"
                )));
            }
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
            Ok(ViewSpec {
                name: name.into(),
                title: title.into(),
                icon: loader.icon.clone(),
                draws_top_bar: false,
                instances: false,
                settings: Some(SettingsEntry { group, keywords }),
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
                handler: Rc::new(move |args, _, cx| {
                    let Some(result) = state
                        .upgrade()
                        .and_then(|state| JsPlugin(state).deliver_command(&handler_name, args))
                    else {
                        return Handled::Pass;
                    };
                    Handled::Done(cx.background_executor().spawn(async move {
                        result
                            .await
                            .unwrap_or_else(|_| Err(UNFINISHED_COMMAND.to_string()))
                    }))
                }),
                name,
                title,
            })
        })
        .collect()
}

/// `register_menu_items` 的参数。
fn menu_arguments(arguments: &HostArguments) -> Result<Vec<MenuEntry>, HostError> {
    let invalid = |what: &str| HostError::new(format!("register_menu_items：{what}"));
    arguments
        .value(0)?
        .as_array()
        .ok_or_else(|| invalid("参数要是菜单项对象的数组"))?
        .iter()
        .map(|item| {
            let text = |name: &str| {
                item.get(name)
                    .and_then(HostValue::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(&format!("每一项都要有字符串 {name}")))
            };
            let slot = match text("slot")?.as_str() {
                "app" => MenuSlot::App,
                "help" => MenuSlot::Help,
                other => return Err(invalid(&format!("slot 只能是 app、help，收到「{other}」"))),
            };
            let title = text("title")?;
            let command = text("command")?;
            if !command.contains('/') {
                return Err(invalid(&format!(
                    "命令「{command}」要写成 \"<来源>/<名字>\""
                )));
            }
            Ok(MenuEntry {
                slot,
                title: title.into(),
                command: command.into(),
            })
        })
        .collect()
}

/// `register_context_menu_items` 的一项。字没字、命令和参数的形状由
/// [`crate::register_context_menu_items`] 再查。
fn context_menu_item(item: &HostValue) -> Result<ContextMenuItem, HostError> {
    let invalid = |what: &str| HostError::new(format!("register_context_menu_items：{what}"));
    let text = |name: &str| match item.get(name) {
        None | Some(HostValue::Null) => Ok(None),
        Some(HostValue::Str(text)) => Ok(Some(text.clone())),
        Some(other) => Err(invalid(&format!(
            "{name} 要是字符串，收到 {}",
            other.describe()
        ))),
    };
    let label = text("label")?.ok_or_else(|| invalid("每一项都要有 label"))?;
    let image = match item.get("png") {
        None | Some(HostValue::Null) => None,
        Some(HostValue::Array(bytes)) => {
            let bytes = bytes
                .iter()
                .map(|byte| match byte {
                    HostValue::Number(byte)
                        if (0.0..=255.0).contains(byte) && byte.fract() == 0.0 =>
                    {
                        Ok(*byte as u8)
                    }
                    _ => Err(invalid(&format!("「{label}」的 png 要是字节（0–255）数组"))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Some(std::sync::Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                bytes,
            )))
        }
        Some(other) => {
            return Err(invalid(&format!(
                "「{label}」的 png 要是字节数组，收到 {}",
                other.describe()
            )));
        }
    };
    let machine = match item.get("only_on") {
        None | Some(HostValue::Null) => None,
        Some(on @ HostValue::Object(_)) => match on.get("machine") {
            Some(HostValue::Null) => Some(RuntimeKey::Local),
            Some(HostValue::Str(id)) => Some(RuntimeKey::Remote(id.clone())),
            _ => {
                return Err(invalid(&format!(
                    "「{label}」的 only_on 要是 {{ machine: null 或字符串 }}"
                )));
            }
        },
        Some(other) => {
            return Err(invalid(&format!(
                "「{label}」的 only_on 要是对象，收到 {}",
                other.describe()
            )));
        }
    };
    let command = text("command")?;
    let action = match (command, item.get("submenu")) {
        (Some(command), None | Some(HostValue::Null)) => ContextMenuAction::Run {
            command,
            args: item.get("args").map(json_value).unwrap_or(Value::Null),
        },
        (None, Some(HostValue::Array(children))) => ContextMenuAction::Submenu(
            children
                .iter()
                .map(context_menu_item)
                .collect::<Result<_, _>>()?,
        ),
        _ => {
            return Err(invalid(&format!(
                "「{label}」要有 command 或 submenu（数组）之一"
            )));
        }
    };
    Ok(ContextMenuItem {
        label: label.into(),
        image,
        icon: text("icon")?.map(SharedString::from),
        machine,
        action,
    })
}

/// `set_quit_guard` 登记的守卫：用户要退出时执行插件的命令 `command`，算它接手。插件视图
/// 释放了、命令撤下了就不接，免得没人处理、谁也退不出去。
fn quit_guard(plugin: &JsPlugin, command: String) -> crate::QuitGuard {
    let state = Rc::downgrade(&plugin.0);
    Rc::new(move |cx: &mut App| {
        let alive = state
            .upgrade()
            .is_some_and(|state| !state.borrow().released);
        if !alive || crate::command(&command, cx).is_none() {
            return false;
        }
        // 排到这一轮更新之后跑，不在宿主处理退出的当中重入。
        crate::run(&command, Value::Null, cx).detach();
        true
    })
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

/// `resolve_command` / `reject_command` 的第一个参数：`next_command` 给的那次执行的 id。
fn call_id(name: &str, arguments: &HostArguments) -> Result<u64, HostError> {
    match arguments.value(0)? {
        HostValue::Number(id) if *id >= 0.0 && id.fract() == 0.0 && *id <= MAX_SAFE_INTEGER => {
            Ok(*id as u64)
        }
        other => Err(HostError::new(format!(
            "{name}：id 要是 next_command 给的整数，收到 {}",
            other.describe()
        ))),
    }
}

/// `set_active_context` 的参数：`{ project, session }`，各自可以是 null。
fn active_context_argument(value: &HostValue) -> Result<crate::ActiveContext, HostError> {
    let invalid = |what: &str| HostError::new(format!("set_active_context：{what}"));
    if !matches!(value, HostValue::Object(_)) {
        return Err(invalid(&format!(
            "参数要是 {{ project, session }}，收到 {}",
            value.describe()
        )));
    }
    let machine = |object: &HostValue, of: &str| match object.get("machine") {
        Some(HostValue::Null) => Ok(RuntimeKey::Local),
        Some(HostValue::Str(id)) => Ok(RuntimeKey::Remote(id.clone())),
        _ => Err(invalid(&format!("{of}.machine 要是 null 或字符串"))),
    };
    let text = |object: &HostValue, of: &str, name: &str| match object.get(name) {
        Some(HostValue::Str(text)) if !text.is_empty() => Ok(text.clone()),
        _ => Err(invalid(&format!("{of}.{name} 要是非空字符串"))),
    };
    let project = match value.get("project") {
        None | Some(HostValue::Null) => None,
        Some(project @ HostValue::Object(_)) => Some(crate::Project {
            machine: machine(project, "project")?,
            root: text(project, "project", "root")?,
        }),
        Some(_) => return Err(invalid("project 要是对象或 null")),
    };
    let session = match value.get("session") {
        None | Some(HostValue::Null) => None,
        Some(session @ HostValue::Object(_)) => Some(crate::ViewedSession {
            machine: machine(session, "session")?,
            id: text(session, "session", "id")?,
            at_latest: match session.get("at_latest") {
                None | Some(HostValue::Null) => true,
                Some(HostValue::Bool(at_latest)) => *at_latest,
                Some(_) => return Err(invalid("session.at_latest 要是布尔值")),
            },
        }),
        Some(_) => return Err(invalid("session 要是对象或 null")),
    };
    Ok(crate::ActiveContext { project, session })
}

fn optional_text(text: Option<&str>) -> HostValue {
    text.map_or(HostValue::Null, HostValue::from)
}

fn safe_area_value(area: &crate::SafeArea) -> HostValue {
    let corner = |size: gpui::Size<gpui::Pixels>| -> HostValue {
        HostObject::new()
            .field("width", HostValue::Number(f64::from(f32::from(size.width))))
            .field(
                "height",
                HostValue::Number(f64::from(f32::from(size.height))),
            )
            .into()
    };
    HostObject::new()
        .field("title_bar", area.title_bar)
        .field(
            "corners",
            HostObject::new()
                .field("top_left", corner(area.corners.top_left))
                .field("top_right", corner(area.corners.top_right))
                .field("bottom_right", corner(area.corners.bottom_right))
                .field("bottom_left", corner(area.corners.bottom_left)),
        )
        .into()
}

fn view_value(view: &crate::ViewInfo) -> HostValue {
    let settings = view.settings.as_ref().map_or(HostValue::Null, |settings| {
        HostObject::new()
            .field(
                "group",
                match settings.group {
                    SettingsGroup::General => "general",
                    SettingsGroup::Agents => "agents",
                    SettingsGroup::System => "system",
                    SettingsGroup::Plugins => "plugins",
                },
            )
            .field(
                "keywords",
                HostValue::Array(
                    settings
                        .keywords
                        .iter()
                        .map(|keyword| HostValue::from(keyword.as_ref()))
                        .collect(),
                ),
            )
            .into()
    });
    HostObject::new()
        .field("id", view.id.as_ref())
        .field("title", view.title.as_ref())
        .field("icon", view.icon.as_ref())
        .field("draws_top_bar", view.draws_top_bar)
        .field("instances", view.instances)
        .field("settings", settings)
        .into()
}

fn command_value(command: &crate::CommandInfo) -> HostValue {
    HostObject::new()
        .field("id", command.id.as_ref())
        .field("group", command.group.as_ref())
        .field("title", command.title.as_ref())
        .field("key", optional_text(command.key.as_deref()))
        .field("context", command.context.as_ref())
        .field("palette", command.palette)
        .field("requires_args", command.requires_args)
        .into()
}

/// 给脚本画的一项右键菜单：命令的参数已经并上菜单的上下文，点了直接 `run`。
fn context_menu_value(item: &ContextMenuItem, context: &Value) -> HostValue {
    let png = item.image.as_ref().map_or(HostValue::Null, |image| {
        HostValue::Array(
            image
                .bytes
                .iter()
                .map(|byte| HostValue::Number(f64::from(*byte)))
                .collect(),
        )
    });
    let object = HostObject::new()
        .field("label", item.label.as_ref())
        .field("icon", optional_text(item.icon.as_deref()))
        .field("png", png);
    match &item.action {
        ContextMenuAction::Run { command, args } => object
            .field("command", command.as_str())
            .field("args", host_value(&crate::context_menu_args(args, context)))
            .into(),
        ContextMenuAction::Submenu(items) => object
            .field(
                "submenu",
                HostValue::Array(
                    items
                        .iter()
                        .map(|item| context_menu_value(item, context))
                        .collect(),
                ),
            )
            .into(),
    }
}

fn machine_value(key: &RuntimeKey) -> HostValue {
    match key {
        RuntimeKey::Local => HostValue::Null,
        RuntimeKey::Remote(id) => HostValue::Str(id.clone()),
    }
}

fn permission_value(permission: &NotificationPermission) -> HostValue {
    let state = match permission {
        NotificationPermission::Checking => "checking",
        NotificationPermission::NotRequested => "not_requested",
        NotificationPermission::Granted {
            banners,
            sound,
            badge,
        } => {
            return HostObject::new()
                .field("state", "granted")
                .field("banners", *banners)
                .field("sound", *sound)
                .field("badge", *badge)
                .into();
        }
        NotificationPermission::Denied => "denied",
        NotificationPermission::Unsupported => "unsupported",
        NotificationPermission::Unavailable(error) => {
            return HostObject::new()
                .field("state", "unavailable")
                .field("error", error.as_str())
                .into();
        }
    };
    HostObject::new().field("state", state).into()
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
                TopicMessage::Unavailable(reason) => HostObject::new()
                    .field("kind", "unavailable")
                    .field("reason", reason.as_str()),
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
        HostEvent::NotificationPermissionChanged => {
            HostObject::new().field("type", "notification_permission_changed")
        }
        HostEvent::ShownViewsChanged => HostObject::new().field("type", "shown_views_changed"),
        HostEvent::LaidOutViewsChanged => HostObject::new().field("type", "laid_out_views_changed"),
        HostEvent::ViewsChanged => HostObject::new().field("type", "views_changed"),
        HostEvent::ViewBadgesChanged => HostObject::new().field("type", "view_badges_changed"),
        HostEvent::SafeAreaChanged => HostObject::new().field("type", "safe_area_changed"),
        HostEvent::ThemesChanged => HostObject::new().field("type", "themes_changed"),
        HostEvent::LayoutsChanged => HostObject::new().field("type", "layouts_changed"),
        HostEvent::PagesChanged => HostObject::new().field("type", "pages_changed"),
        HostEvent::CommandsChanged => HostObject::new().field("type", "commands_changed"),
        HostEvent::Plugin { name, payload } => HostObject::new()
            .field("type", "plugin")
            .field("name", name.as_str())
            .field("payload", host_value(payload)),
        HostEvent::OpenPaths {
            machine: key,
            paths,
        } => HostObject::new()
            .field("type", "open_paths")
            .field("machine", machine(key))
            .field(
                "paths",
                HostValue::Array(paths.iter().map(|path| path.as_str().into()).collect()),
            ),
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
        for query in ["session_states", "workspace_directory"] {
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
        assert!(
            module
                .call("view_laid_out", &HostArguments::default())
                .is_err()
        );
        assert!(module.call("safe_area", &HostArguments::default()).is_err());
        // 参数对了才去找宿主，测试里没有脚本上下文。
        let outside_script = module
            .call("session_states", &HostArguments::new([HostValue::Null]))
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
            // 没说给哪一页。
            HostArguments::new([layout(&[
                ("name", "a".into()),
                ("title", "A".into()),
                ("layout", "{}".into()),
            ])]),
            HostArguments::new([layout(&[
                ("page", "smelt.layouts/workbench".into()),
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
                    ("page", "smelt.layouts/workbench".into()),
                    ("name", "a".into()),
                    ("title", "A".into()),
                    ("layout", "{}".into()),
                ])]),
            )
            .unwrap_err();
        assert!(error.message().contains("只能在视图"));
    }

    #[test]
    fn page_registration_rejects_invalid_arguments_before_entering_app() {
        let module = JsPlugin::for_plugin("example", "示例").module();
        let page = |fields: &[(&str, HostValue)]| {
            let mut object = HostObject::new();
            for (name, value) in fields {
                object = object.field(*name, value.clone());
            }
            HostValue::Array(vec![object.into()])
        };
        for arguments in [
            HostArguments::default(),
            HostArguments::new([HostValue::Null]),
            HostArguments::new([page(&[("name", "a".into())])]),
            HostArguments::new([page(&[
                ("name", "a".into()),
                ("title", "A".into()),
                ("home", "yes".into()),
            ])]),
            HostArguments::new([page(&[
                ("name", "a".into()),
                ("title", "A".into()),
                ("index", HostValue::Bool(true)),
            ])]),
        ] {
            let error = module.call("register_pages", &arguments).unwrap_err();
            assert!(
                !error.message().contains("只能在视图"),
                "{}",
                error.message()
            );
        }
        // 参数对了才进 App；测试里没有 App。
        for home in [HostValue::Null, HostValue::Bool(true)] {
            let error = module
                .call(
                    "register_pages",
                    &HostArguments::new([page(&[
                        ("name", "a".into()),
                        ("title", "A".into()),
                        ("home", home),
                    ])]),
                )
                .unwrap_err();
            assert!(error.message().contains("只能在视图"));
        }
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
    fn runtime_queries_check_their_arguments_before_entering_app() {
        let module = JsPlugin::new().module();
        for query in ["machine_online", "session_presence"] {
            let error = module
                .call(query, &HostArguments::new([HostValue::Bool(true)]))
                .unwrap_err();
            assert!(error.message().contains("机器要是 null 或字符串"));
        }
        assert!(
            module
                .call("session_presence", &HostArguments::new([HostValue::Null]))
                .is_err()
        );
        for failures in [HostValue::Number(-1.0), HostValue::Number(1.5), "1".into()] {
            let error = module
                .begin(
                    "wait_online",
                    &HostArguments::new([HostValue::Null, failures]),
                )
                .err()
                .expect("failures 不对");
            assert!(error.message().contains("failures"), "{}", error.message());
        }
        let outside_script = module
            .begin("wait_online", &HostArguments::new([HostValue::Null]))
            .err()
            .expect("测试里没有脚本上下文");
        assert!(outside_script.message().contains("只能在视图"));
    }

    #[test]
    fn menu_registrations_reject_invalid_arguments_before_entering_app() {
        let module = JsPlugin::for_plugin("example", "示例").module();
        let menu_item = |slot: &str, command: &str| {
            HostValue::Array(vec![object(&[
                ("slot", slot.into()),
                ("title", "检查更新".into()),
                ("command", command.into()),
            ])])
        };
        for items in [
            HostValue::Null,
            menu_item("window", "example/check"),
            menu_item("app", "check"),
            HostValue::Array(vec![object(&[("slot", "app".into())])]),
        ] {
            let error = module
                .call("register_menu_items", &HostArguments::new([items]))
                .unwrap_err();
            assert!(
                !error.message().contains("只能在视图"),
                "{}",
                error.message()
            );
        }
        let error = module
            .call(
                "register_menu_items",
                &HostArguments::new([menu_item("help", "example/docs")]),
            )
            .unwrap_err();
        assert!(error.message().contains("只能在视图"));

        let run = object(&[("label", "Zed".into()), ("command", "example/open".into())]);
        for item in [
            object(&[("command", "example/open".into())]),
            object(&[("label", "Zed".into())]),
            object(&[
                ("label", "Zed".into()),
                ("command", "example/open".into()),
                ("submenu", HostValue::Array(Vec::new())),
            ]),
            object(&[
                ("label", "Zed".into()),
                ("command", "example/open".into()),
                ("png", HostValue::Array(vec![HostValue::Number(256.0)])),
            ]),
            object(&[
                ("label", "Zed".into()),
                ("command", "example/open".into()),
                ("only_on", object(&[("machine", HostValue::Number(1.0))])),
            ]),
            object(&[
                ("label", "更多".into()),
                ("submenu", HostValue::Array(vec![HostValue::Null])),
            ]),
        ] {
            let error = module
                .call(
                    "register_context_menu_items",
                    &HostArguments::new(["example/menu".into(), HostValue::Array(vec![item])]),
                )
                .unwrap_err();
            assert!(
                !error.message().contains("只能在视图"),
                "{}",
                error.message()
            );
        }
        let nested = object(&[
            ("label", "用其他应用打开".into()),
            ("only_on", object(&[("machine", HostValue::Null)])),
            ("submenu", HostValue::Array(vec![run])),
        ]);
        let error = module
            .call(
                "register_context_menu_items",
                &HostArguments::new(["example/menu".into(), HostValue::Array(vec![nested])]),
            )
            .unwrap_err();
        assert!(error.message().contains("只能在视图"));

        for name in [HostValue::Number(1.0), "".into(), "other/quit".into()] {
            let error = module
                .call("set_quit_guard", &HostArguments::new([name]))
                .unwrap_err();
            assert!(
                !error.message().contains("只能在视图"),
                "{}",
                error.message()
            );
        }
        let error = module
            .call("set_quit_guard", &HostArguments::new([HostValue::Null]))
            .unwrap_err();
        assert!(error.message().contains("只能在视图"));
    }

    #[test]
    fn a_context_menu_item_maps_to_the_rust_shape() {
        let item = context_menu_item(&object(&[
            ("label", "用其他应用打开".into()),
            ("icon", "icons/external-link.svg".into()),
            ("only_on", object(&[("machine", "box".into())])),
            (
                "submenu",
                HostValue::Array(vec![object(&[
                    ("label", "Zed".into()),
                    (
                        "png",
                        HostValue::Array(vec![HostValue::Number(137.0), HostValue::Number(80.0)]),
                    ),
                    ("command", "example/open".into()),
                    ("args", object(&[("app", "zed".into())])),
                ])]),
            ),
        ]))
        .expect("形状对");
        assert_eq!(item.label.as_ref(), "用其他应用打开");
        assert_eq!(item.icon.as_deref(), Some("icons/external-link.svg"));
        assert_eq!(item.machine, Some(RuntimeKey::Remote("box".to_string())));
        assert!(item.image.is_none());
        let ContextMenuAction::Submenu(children) = &item.action else {
            panic!("应是子菜单");
        };
        assert_eq!(children[0].machine, None);
        assert_eq!(
            children[0].image.as_ref().map(|image| image.bytes.clone()),
            Some(vec![137, 80])
        );
        assert_eq!(
            children[0].action,
            ContextMenuAction::Run {
                command: "example/open".to_string(),
                args: serde_json::json!({ "app": "zed" }),
            }
        );
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
            HostArguments::new([page(&[
                ("name", "main".into()),
                valid[1].clone(),
                valid[2].clone(),
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
            "register_pages",
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
        let first = plugin.deliver_command("a", &args).expect("视图在就接");
        let second = plugin.deliver_command("a", &args).expect("视图在就接");
        let value = smol::block_on(plugin.next_command()).unwrap();
        assert_eq!(value.get("name").and_then(HostValue::as_str), Some("a"));
        assert_eq!(value.get("id"), Some(&HostValue::Number(0.0)));
        assert_eq!(
            value.get("args").and_then(|args| args.get("n")),
            Some(&HostValue::Number(1.0))
        );
        assert_eq!(plugin.0.borrow().calls.len(), 1);
        assert!(smol::block_on(plugin.next_command()).is_ok());
        // 脚本交回第一次的结果；第二次没交回，插件卸下时执行的一方收到失败，不会一直等。
        plugin
            .reply(
                "resolve_command",
                0,
                Ok(serde_json::json!({ "done": true })),
            )
            .unwrap();
        assert_eq!(
            smol::block_on(first).unwrap(),
            Ok(serde_json::json!({ "done": true }))
        );
        assert!(plugin.reply("resolve_command", 0, Ok(Value::Null)).is_err());
        // 队列空了，这次挂起等着；视图释放后它收到错误，不会一直挂着。
        let waiting = plugin.next_command();
        assert!(plugin.0.borrow().call_waiter.is_some());
        plugin.detach();
        assert!(smol::block_on(waiting).is_err());
        assert!(smol::block_on(second).is_err());
        assert!(plugin.deliver_command("a", &args).is_none());
        assert!(plugin.0.borrow().calls.is_empty());
    }

    #[test]
    fn scripts_reply_to_command_calls_by_id() {
        let plugin = JsPlugin::for_plugin("example", "示例");
        let module = plugin.module();
        let pending = plugin.deliver_command("a", &Value::Null).unwrap();
        for id in [
            HostValue::Null,
            HostValue::Number(-1.0),
            HostValue::Number(0.5),
        ] {
            let error = module
                .call("reject_command", &HostArguments::new([id, "不行".into()]))
                .unwrap_err();
            assert!(error.message().contains("id 要是"));
        }
        let unknown = module
            .call(
                "reject_command",
                &HostArguments::new([HostValue::Number(7.0), "不行".into()]),
            )
            .unwrap_err();
        assert!(unknown.message().contains("第 7 次"));
        module
            .call(
                "reject_command",
                &HostArguments::new([HostValue::Number(0.0), "不行".into()]),
            )
            .unwrap();
        assert_eq!(smol::block_on(pending).unwrap(), Err("不行".to_string()));
    }

    #[test]
    fn runtime_calls_check_method_and_timeout_before_entering_app() {
        let module = JsPlugin::new().module();
        let call = |arguments: Vec<HostValue>| {
            module
                .begin("call", &HostArguments::new(arguments))
                .err()
                .expect("测试里没有脚本上下文，到不了运行时")
                .message()
                .to_string()
        };
        assert!(
            call(vec![HostValue::Null, "no.such-method".into()]).contains("没有「no.such-method」")
        );
        assert!(call(vec![HostValue::Bool(true), "session.list".into()]).contains("机器要是"));
        assert!(
            call(vec![
                HostValue::Null,
                "session.list".into(),
                HostValue::Null,
                HostValue::Number(0.0),
            ])
            .contains("timeout_ms")
        );
        // 参数都对，才去找宿主。
        assert!(call(vec![HostValue::Null, "session.list".into()]).contains("只能在视图"));
    }

    #[test]
    fn active_context_comes_from_script_objects() {
        let object = |fields: Vec<(&str, HostValue)>| {
            fields
                .into_iter()
                .fold(HostObject::new(), |object, (name, value)| {
                    object.field(name, value)
                })
                .into()
        };
        let empty = active_context_argument(&object(vec![])).unwrap();
        assert_eq!(empty, crate::ActiveContext::default());
        let context = active_context_argument(&object(vec![
            (
                "project",
                object(vec![("machine", "box".into()), ("root", "/w".into())]),
            ),
            (
                "session",
                object(vec![("machine", HostValue::Null), ("id", "s1".into())]),
            ),
        ]))
        .unwrap();
        assert_eq!(
            context.project,
            Some(crate::Project {
                machine: RuntimeKey::Remote("box".into()),
                root: "/w".into(),
            })
        );
        let session = context.session.unwrap();
        assert_eq!(session.machine, RuntimeKey::Local);
        assert!(session.at_latest);
        for bad in [
            HostValue::Null,
            object(vec![(
                "project",
                object(vec![("machine", HostValue::Null)]),
            )]),
            object(vec![(
                "session",
                object(vec![
                    ("machine", HostValue::Number(1.0)),
                    ("id", "s".into()),
                ]),
            )]),
        ] {
            assert!(active_context_argument(&bad).is_err());
        }
    }

    #[test]
    fn context_menu_entries_carry_merged_args() {
        let item = ContextMenuItem {
            label: "打开".into(),
            image: None,
            icon: Some("icons/external-link.svg".into()),
            machine: None,
            action: ContextMenuAction::Submenu(vec![ContextMenuItem {
                label: "用 Zed".into(),
                image: None,
                icon: None,
                machine: None,
                action: ContextMenuAction::Run {
                    command: "ide/open".into(),
                    args: serde_json::json!({ "app": "zed", "path": "旧的" }),
                },
            }]),
        };
        let value = context_menu_value(&item, &serde_json::json!({ "path": "/w/a.rs" }));
        assert_eq!(value.get("png"), Some(&HostValue::Null));
        let HostValue::Array(submenu) = value.get("submenu").unwrap() else {
            panic!("要有子菜单");
        };
        let args = json_value(submenu[0].get("args").unwrap());
        assert_eq!(args, serde_json::json!({ "app": "zed", "path": "/w/a.rs" }));
        assert_eq!(
            submenu[0].get("command").and_then(HostValue::as_str),
            Some("ide/open")
        );
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

    /// 插件事件同一条再来也要排上：中间夹着的那条变了状态，合并掉就丢了来回。
    #[test]
    fn plugin_events_are_queued_every_time() {
        let plugin = JsPlugin::new();
        let at_latest = |value: bool| HostEvent::Plugin {
            name: "probe/at-latest".into(),
            payload: serde_json::json!({ "at_latest": value }),
        };
        plugin.deliver(&at_latest(false));
        plugin.deliver(&at_latest(true));
        plugin.deliver(&at_latest(false));
        assert_eq!(
            plugin.0.borrow().queue,
            [at_latest(false), at_latest(true), at_latest(false)]
        );
    }

    #[test]
    fn scripts_emit_only_under_their_own_id() {
        let payload = serde_json::json!({ "n": 1 });
        assert_eq!(
            plugin_event("probe", "probe/moved", payload.clone()).unwrap(),
            HostEvent::Plugin {
                name: "probe/moved".into(),
                payload: payload.clone(),
            }
        );
        for name in [
            "other/moved",
            "probe",
            "probe/",
            "prober/moved",
            "probemoved",
        ] {
            assert!(
                plugin_event("probe", name, payload.clone()).is_err(),
                "{name}"
            );
        }
        assert!(plugin_event("", "/moved", payload).is_err());
    }

    #[test]
    fn plugin_events_and_paths_reach_scripts_as_declared() {
        let event = event_value(&HostEvent::Plugin {
            name: "probe/moved".into(),
            payload: serde_json::json!({ "at": [1, null] }),
        });
        assert_eq!(
            json_value(&event),
            serde_json::json!({ "type": "plugin", "name": "probe/moved", "payload": { "at": [1, null] } })
        );
        let event = event_value(&HostEvent::OpenPaths {
            machine: RuntimeKey::Remote("box".into()),
            paths: vec!["/a".into(), "/b c".into()],
        });
        assert_eq!(
            json_value(&event),
            serde_json::json!({ "type": "open_paths", "machine": "box", "paths": ["/a", "/b c"] })
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
