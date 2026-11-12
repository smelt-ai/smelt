//! 快捷键：命令的出厂键位、用户改过的覆盖，以及把它们装进 GPUI 键表。
//!
//! 能改键的是登记过的命令（`smelt_plugin_api::register_commands`），键都绑到
//! [`RunCommand`]，由主窗口根节点按 id 执行。用户改过的按命令 id 存在客户端库的
//! `key_binding` 表；还没登记的命令（插件没装、还没加载）的覆盖先留着，登记了就生效。
//! 存的是 `Keystroke::unparse` 按本机系统写出的显式修饰键（macOS `cmd-`、Linux `super-`），
//! 动态的按键串都用 `Keystroke::parse_with_os` 解析。
//!
//! 改键不能把整张键表清空重来：gpui-shell 插件第一次用到时才往键表里加自己的键，清掉就
//! 回不来了。smelt 自己的键都打上 [`APP_KEYMAP`] 标记，改键时只把这一段原地换掉，前后别人
//! 的键和先后顺序都不动。命令登记一变就重装这一段。
//!
//! 工作区组合键绑在 `Workspace` 或更具体、但仍浅于 Terminal / Input 的 context 上。
//! `None` 与最内层同深度，后注册会抢走输入框的键，不用。

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    App, Global, KeyBinding, KeyBindingContextPredicate, KeyBindingMetaIndex, Keystroke, Menu,
    MenuItem, NoAction, OperatingSystem, SharedString, Subscription, Window,
};
use gpui_component::input::{
    DeleteToBeginningOfLine, DeleteToEndOfLine, DeleteToPreviousWordStart,
};
use smelt_client_store::KeyBindingRecord;
use smelt_plugin_api::{self as api, CommandInfo, HostEvent, RunCommand};

use super::*;

/// smelt 自己注册的键都带这个标记，改键时据此认出要换掉的那一段。
/// 值只在 `NoAction` 判优先级时有用，和不带标记的键一样按用户键位算。
const APP_KEYMAP: KeyBindingMetaIndex = KeyBindingMetaIndex(0);

/// 命令 id → 用户改的按键。`None` 是用户清掉了，不绑任何键。
pub(crate) type Overrides = BTreeMap<String, Option<String>>;

/// 一条登记过的命令和它现在生效的键。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bound {
    pub(crate) info: CommandInfo,
    /// 生效的按键：改过用改过的，没改过用出厂的；清掉了、出厂键不合规或撞了是 `None`。
    pub(crate) key: Option<String>,
    /// 用户改过（含清掉）。
    pub(crate) overridden: bool,
}

/// 不让绑的键：终端在自己的按键监听里处理、不在键表里的（应用键先于终端派发，绑上去终端
/// 就收不到了），通用的编辑键，以及 macOS 自己用的。⌘Space、⌘Tab 这类系统直接截走的键
/// 根本到不了应用，录不到，不用列。
const RESERVED_KEYS: &[(&str, &str)] = &[
    ("cmd-c", "复制"),
    ("cmd-v", "粘贴"),
    ("cmd-x", "剪切"),
    ("cmd-a", "全选"),
    ("cmd-z", "撤销"),
    ("cmd-shift-z", "重做"),
    ("cmd-f", "终端内搜索"),
    ("cmd-h", "系统的「隐藏应用」"),
    ("cmd-alt-h", "系统的「隐藏其他应用」"),
    ("cmd-m", "系统的「最小化」"),
];

/// 出厂键要带 ⌘（或者是 F 键），不能是保留键：应用键先于终端和输入框派发。
fn default_allowed(keystroke: &Keystroke, os: OperatingSystem) -> bool {
    (keystroke.modifiers.platform || is_function_key(&keystroke.key))
        && !RESERVED_KEYS
            .iter()
            .any(|(reserved, _)| same_key(reserved, keystroke, os))
}

/// 每条命令现在生效的键，按登记顺序。用户改过的优先，然后出厂键按登记顺序排：撞上已经
/// 用掉的键、或者不合规的出厂键都不绑，记日志。冲突不看 context，一个键只给一条命令。
pub(crate) fn bind_commands(
    commands: Vec<CommandInfo>,
    overrides: &Overrides,
    os: OperatingSystem,
) -> Vec<Bound> {
    let mut used: Vec<Keystroke> = Vec::new();
    let mut take = |id: &str, spec: &str, is_default: bool| -> bool {
        let Ok(stroke) = Keystroke::parse_with_os(spec, os) else {
            eprintln!("[keymap] {id} 的按键 {spec} 解析不了，不绑");
            return false;
        };
        if is_default && !default_allowed(&stroke, os) {
            eprintln!("[keymap] {id} 的出厂键 {spec} 要带 ⌘ 或者是 F 键，也不能是保留键，不绑");
            return false;
        }
        if used
            .iter()
            .any(|other| other.modifiers == stroke.modifiers && other.key == stroke.key)
        {
            eprintln!("[keymap] {id} 的按键 {spec} 已经给了别的命令，不绑");
            return false;
        }
        used.push(stroke);
        true
    };
    let mut keys: Vec<Option<String>> = vec![None; commands.len()];
    for (slot, command) in keys.iter_mut().zip(&commands) {
        if let Some(Some(spec)) = overrides.get(command.id.as_ref())
            && take(&command.id, spec, false)
        {
            *slot = Some(spec.clone());
        }
    }
    for (slot, command) in keys.iter_mut().zip(&commands) {
        if !overrides.contains_key(command.id.as_ref())
            && let Some(spec) = &command.key
            && take(&command.id, spec, true)
        {
            *slot = Some(spec.to_string());
        }
    }
    commands
        .into_iter()
        .zip(keys)
        .map(|(info, key)| Bound {
            overridden: overrides.contains_key(info.id.as_ref()),
            info,
            key,
        })
        .collect()
}

/// 应用键位：改不了的固定键，加上每条命令生效的键。
/// 键是运行时的字符串，只能走 `KeyBinding::load`，它要 App 给键盘映射。
pub(crate) fn app_key_bindings(bound: &[Bound], cx: &App) -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("escape", DismissWorkspace, Some("Workspace")),
        // 设置窗口：Cmd+W（Windows / Linux 是 Ctrl+W）关窗。它是普通窗口，不是模态层，
        // Esc 不关它，留给下拉框、弹出层和输入框。`KeyBinding::new` 不解析 `secondary-`，
        // 按目标平台写成显式修饰键。
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-w", CloseSettingsWindow, Some("SettingsWindow")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-w", CloseSettingsWindow, Some("SettingsWindow")),
        // 文件树只在自己的宿主上挂 FileTree。树没挂载（文件页不可见、树收起、
        // 搜索框有内容）时这条 context 不在焦点路径上。
        KeyBinding::new("up", FileTreeMoveUp, Some("FileTree")),
        KeyBinding::new("down", FileTreeMoveDown, Some("FileTree")),
        KeyBinding::new("left", FileTreeMoveLeft, Some("FileTree")),
        KeyBinding::new("right", FileTreeMoveRight, Some("FileTree")),
        KeyBinding::new("enter", FileTreeActivate, Some("FileTree")),
        KeyBinding::new("up", SidebarUp, Some("Sidebar")),
        KeyBinding::new("down", SidebarDown, Some("Sidebar")),
        KeyBinding::new("left", SidebarCollapse, Some("Sidebar")),
        KeyBinding::new("right", SidebarExpand, Some("Sidebar")),
        KeyBinding::new("enter", SidebarOpen, Some("Sidebar")),
        KeyBinding::new("escape", SidebarExit, Some("Sidebar")),
        // readline / Grok TUI 行编辑：Ctrl+U 删到行首、Ctrl+K 删到行尾、
        // Ctrl+W 删词。绑在 Input context，所有输入框（ACP composer 等）生效。
        KeyBinding::new("ctrl-u", DeleteToBeginningOfLine, Some("Input")),
        KeyBinding::new("ctrl-k", DeleteToEndOfLine, Some("Input")),
        KeyBinding::new("ctrl-w", DeleteToPreviousWordStart, Some("Input")),
        // 把 Tab/Shift-Tab 从 gpui-component Root 的全局焦点跳转手里要回来，
        // 终端聚焦时改发给 shell（见 terminal_view.rs 里 TerminalTab 的注释）。
        KeyBinding::new("tab", terminal_view::TerminalTab, Some("Terminal")),
        KeyBinding::new(
            "shift-tab",
            terminal_view::TerminalBackTab,
            Some("Terminal"),
        ),
    ];
    // cmux：Ctrl+Backspace / Ctrl+Delete 也是删行（^U / ^K），不是删词。
    #[cfg(target_os = "macos")]
    {
        bindings.push(KeyBinding::new(
            "ctrl-backspace",
            DeleteToBeginningOfLine,
            Some("Input"),
        ));
        bindings.push(KeyBinding::new(
            "ctrl-delete",
            DeleteToEndOfLine,
            Some("Input"),
        ));
    }
    for command in bound {
        let Some(keystroke) = &command.key else {
            continue;
        };
        // 登记时验过 context 能解析，按键在 `bind_commands` 里验过。
        let context = KeyBindingContextPredicate::parse(&command.info.context)
            .ok()
            .map(Rc::new);
        match KeyBinding::load(
            keystroke,
            Box::new(RunCommand::new(command.info.id.clone())),
            context,
            false,
            None,
            cx,
        ) {
            Ok(binding) => bindings.push(binding),
            Err(error) => {
                eprintln!(
                    "[keymap] {} 的按键 {keystroke} 解析不了：{error}",
                    command.info.id
                )
            }
        }
    }
    bindings
        .into_iter()
        .map(|binding| binding.with_meta(APP_KEYMAP))
        .collect()
}

/// 应用菜单栏：macOS 顶部「Smelt」菜单，含当前版本、「设置… ⌘,」+「退出 Smelt ⌘Q」
/// （跟侧栏「设置」一样开独立设置窗口，符合 mac 惯例）；帮助文档和反馈问题按 mac 惯例
/// 放「帮助」菜单。设置「关于」页也有这两项，没有菜单栏的平台从那里进。
///
/// 菜单项上的快捷键是设菜单那一刻从键表里取的，改键后要重设一次。
fn app_menus() -> Vec<Menu> {
    let command =
        |title: &'static str, id: &'static str| MenuItem::action(title, RunCommand::new(id));
    vec![
        Menu::new("Smelt").items([
            MenuItem::action(format!("v{}", smelt_version::VERSION), NoAction).disabled(true),
            #[cfg(target_os = "macos")]
            MenuItem::Separator,
            #[cfg(target_os = "macos")]
            command("检查更新…", "smelt.app/check-update"),
            MenuItem::Separator,
            command("设置…", "smelt.app/settings"),
            MenuItem::Separator,
            command("退出 Smelt", "smelt.app/quit"),
        ]),
        Menu::new("帮助").items([
            command("帮助文档", "smelt.app/docs"),
            command("反馈问题", "smelt.app/report-issue"),
        ]),
    ]
}

/// 快捷键的全局状态：用户的覆盖、每条命令现在的键，以及设置页正在录哪一条。
#[derive(Default)]
pub(crate) struct KeymapState {
    overrides: Overrides,
    /// 登记过的命令和生效的键，装键表时算好，设置页直接读。
    bound: Vec<Bound>,
    /// 设置页正在录新按键的那一条。
    pub(crate) recording: Option<SharedString>,
    /// 最近一次改键没成：哪一条、为什么。
    pub(crate) error: Option<(SharedString, String)>,
    /// 录到的键已经是另一条的：等用户确认改绑还是算了。
    pub(crate) reassign: Option<Reassign>,
    /// 录键时抢在动作派发前拿到按键，录完就丢掉。
    interceptor: Option<Subscription>,
}

impl Global for KeymapState {}

/// 改绑：`keystroke` 给 `id`，`other` 原来用着它，确认后清掉。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reassign {
    pub(crate) id: SharedString,
    pub(crate) other: SharedString,
    pub(crate) keystroke: String,
}

impl KeymapState {
    pub(crate) fn bound(&self) -> &[Bound] {
        &self.bound
    }

    pub(crate) fn find(&self, id: &str) -> Option<&Bound> {
        self.bound.iter().find(|bound| bound.info.id.as_ref() == id)
    }
}

/// 启动时：读用户的覆盖，装进键表，设菜单；之后命令登记一变就重装。必须在 `set_menus`
/// 之前装键，菜单项才带快捷键，这里两件一起做。要在插件宿主装好之后调。
pub(crate) fn init(cx: &mut App) {
    cx.set_global(KeymapState {
        overrides: load_overrides(cx.platform_info().os),
        ..KeymapState::default()
    });
    install(cx);
    let events = api::events(cx);
    cx.subscribe(&events, |_, event, cx| {
        if matches!(event, HostEvent::CommandsChanged) {
            install(cx);
        }
    })
    .detach();
}

fn load_overrides(os: OperatingSystem) -> Overrides {
    let records = match smelt_client_store::client_store().and_then(|store| store.key_bindings()) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("[keymap] 读快捷键设置失败，用出厂键位：{error}");
            return Overrides::new();
        }
    };
    overrides_from_records(records, os)
}

/// 解析不了的按键丢掉，那一条回到出厂键位。认不出的命令留着：它的插件可能还没加载。
fn overrides_from_records(records: Vec<KeyBindingRecord>, os: OperatingSystem) -> Overrides {
    records
        .into_iter()
        .filter_map(|record| {
            if let Some(keystroke) = &record.keystroke
                && Keystroke::parse_with_os(keystroke, os).is_err()
            {
                eprintln!(
                    "[keymap] {} 的按键 {keystroke} 解析不了，用出厂键位",
                    record.command
                );
                return None;
            }
            Some((record.command, record.keystroke))
        })
        .collect()
}

/// 按现在的命令和覆盖重算每条命令的键，把键表里 smelt 那一段换掉，再重设菜单。
fn install(cx: &mut App) {
    let bound = bind_commands(
        api::commands(cx),
        &cx.global::<KeymapState>().overrides,
        cx.platform_info().os,
    );
    let app = app_key_bindings(&bound, cx);
    cx.global_mut::<KeymapState>().bound = bound;
    let rebuilt = {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        splice_app_bindings(keymap.bindings().cloned().collect(), app)
    };
    cx.clear_key_bindings();
    cx.bind_keys(rebuilt);
    cx.set_menus(app_menus());
}

/// smelt 那一段换成 `app`，放在原来第一条 smelt 键的位置；原来没有就接在最后。
fn splice_app_bindings(existing: Vec<KeyBinding>, app: Vec<KeyBinding>) -> Vec<KeyBinding> {
    let at = existing
        .iter()
        .position(is_app_binding)
        .unwrap_or(existing.len());
    let mut others = existing
        .into_iter()
        .filter(|binding| !is_app_binding(binding));
    let mut rebuilt: Vec<KeyBinding> = others.by_ref().take(at).collect();
    rebuilt.extend(app);
    rebuilt.extend(others);
    rebuilt
}

fn is_app_binding(binding: &KeyBinding) -> bool {
    binding.meta() == Some(APP_KEYMAP)
}

/// 改几条快捷键：`Some(按键)` 换成这个键，`None` 清掉不绑。先一起写库，写成了才换上；
/// 写不成就记在 `failed` 那一条上，键位不动。
fn save_overrides(
    failed: SharedString,
    changes: Vec<(SharedString, Option<String>)>,
    cx: &mut App,
) {
    let records: Vec<KeyBindingRecord> = changes
        .iter()
        .map(|(id, keystroke)| KeyBindingRecord {
            command: id.to_string(),
            keystroke: keystroke.clone(),
        })
        .collect();
    let saved =
        smelt_client_store::client_store().and_then(|store| store.put_key_bindings(&records));
    apply_saved(failed, saved, cx, |overrides| {
        for (id, keystroke) in changes {
            overrides.insert(id.to_string(), keystroke);
        }
    });
}

/// 回到出厂键位。
pub(crate) fn reset(id: SharedString, cx: &mut App) {
    let saved = smelt_client_store::client_store().and_then(|store| store.delete_key_binding(&id));
    let key = id.to_string();
    apply_saved(id, saved, cx, move |overrides| {
        overrides.remove(&key);
    });
}

fn apply_saved(
    failed: SharedString,
    saved: Result<(), smelt_client_store::ClientStoreError>,
    cx: &mut App,
    change: impl FnOnce(&mut Overrides),
) {
    match saved {
        Ok(()) => {
            cx.update_global::<KeymapState, _>(|state, _| {
                change(&mut state.overrides);
                state.error = None;
                state.reassign = None;
            });
            install(cx);
        }
        Err(error) => {
            eprintln!("[keymap] 保存快捷键 {failed} 失败：{error}");
            cx.update_global::<KeymapState, _>(|state, _| {
                state.error = Some((failed, format!("保存失败：{error}")));
            });
        }
    }
}

/// 确认改绑：对方清掉，键给这一条。
pub(crate) fn confirm_reassign(cx: &mut App) {
    let Some(Reassign {
        id,
        other,
        keystroke,
    }) = cx.global::<KeymapState>().reassign.clone()
    else {
        return;
    };
    save_overrides(id.clone(), vec![(other, None), (id, Some(keystroke))], cx);
}

pub(crate) fn cancel_reassign(cx: &mut App) {
    cx.update_global::<KeymapState, _>(|state, _| state.reassign = None);
}

/// 设置页点了某条的键帽：开始录。设置窗口里的下一次按键先到这里，不派发成动作，所以
/// ⌘W、⌘Q 这种已经有用处的键也录得到。按键落在别的窗口（设置窗口关了、切走了）就不录了，
/// 那个键照常交给那个窗口。
pub(crate) fn start_recording(id: SharedString, window: &Window, cx: &mut App) {
    let recording_window = window.window_handle().window_id();
    let recording = id.clone();
    let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
        // 单独按下又松开的修饰键也会报上来，不算。
        if is_modifier_key(&event.keystroke.key) {
            return;
        }
        if window.window_handle().window_id() != recording_window {
            cx.defer(stop_recording);
            return;
        }
        cx.stop_propagation();
        let keystroke = event.keystroke.clone();
        let id = recording.clone();
        cx.defer(move |cx| finish_recording(id, keystroke, cx));
    });
    cx.update_global::<KeymapState, _>(|state, _| {
        state.recording = Some(id);
        state.error = None;
        state.reassign = None;
        state.interceptor = Some(interceptor);
    });
}

pub(crate) fn stop_recording(cx: &mut App) {
    cx.update_global::<KeymapState, _>(|state, _| {
        state.recording = None;
        state.interceptor = None;
    });
}

fn finish_recording(id: SharedString, keystroke: Keystroke, cx: &mut App) {
    if cx.global::<KeymapState>().recording.as_ref() != Some(&id) {
        return;
    }
    stop_recording(cx);
    let outcome = {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        recorded(
            &id,
            &keystroke,
            &cx.global::<KeymapState>().bound,
            keymap.bindings(),
            cx.platform_info().os,
        )
    };
    match outcome {
        Recorded::Cancel => {}
        Recorded::Reset => reset(id, cx),
        Recorded::Set(keystroke) => save_overrides(id.clone(), vec![(id, keystroke)], cx),
        Recorded::Taken { other, keystroke } => cx.update_global::<KeymapState, _>(|state, _| {
            state.reassign = Some(Reassign {
                id,
                other,
                keystroke,
            });
        }),
        Recorded::Rejected(reason) => cx.update_global::<KeymapState, _>(|state, _| {
            state.error = Some((id, reason));
        }),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    Cancel,
    /// 录回了出厂键位。
    Reset,
    /// `None` 是清掉。
    Set(Option<String>),
    /// 另一条命令正用着它，问用户要不要改绑。
    Taken {
        other: SharedString,
        keystroke: String,
    },
    Rejected(String),
}

/// 录到一个按键之后怎么办。Esc 取消，⌫ 清掉；别的键要带 ⌘（或者是 F 键），不能是保留键。
/// 撞上另一条命令现在的键，交给用户决定要不要改绑；录回出厂键就是恢复默认；撞上组件
/// 自带或改不了的键直接拒。
fn recorded<'a>(
    id: &str,
    keystroke: &Keystroke,
    bound: &[Bound],
    keymap: impl IntoIterator<Item = &'a KeyBinding>,
    os: OperatingSystem,
) -> Recorded {
    let modifiers = &keystroke.modifiers;
    let bare = !(modifiers.platform || modifiers.control || modifiers.alt || modifiers.shift);
    if bare && keystroke.key == "escape" {
        return Recorded::Cancel;
    }
    if bare && matches!(keystroke.key.as_str(), "backspace" | "delete") {
        return Recorded::Set(None);
    }
    // 应用键先于终端和输入框派发：⌃ / ⌥ 组合会抢走 shell 和 readline 的键。
    if !modifiers.platform && !is_function_key(&keystroke.key) {
        return Recorded::Rejected("要带 ⌘：⌃ 和 ⌥ 组合留给终端和输入框".to_string());
    }
    let keystroke = Keystroke {
        modifiers: *modifiers,
        key: keystroke.key.clone(),
        key_char: None,
    };
    if let Some((_, what)) = RESERVED_KEYS
        .iter()
        .find(|(reserved, _)| same_key(reserved, &keystroke, os))
    {
        return Recorded::Rejected(format!("{} 留给{what}", display(&keystroke)));
    }
    if let Some(other) = bound.iter().find(|other| {
        other.info.id.as_ref() != id
            && other
                .key
                .as_deref()
                .is_some_and(|key| same_key(key, &keystroke, os))
    }) {
        return Recorded::Taken {
            other: other.info.id.clone(),
            keystroke: keystroke.unparse(os),
        };
    }
    let this = bound.iter().find(|command| command.info.id.as_ref() == id);
    if this
        .and_then(|command| command.info.key.as_deref())
        .is_some_and(|default| same_key(default, &keystroke, os))
    {
        // 录回出厂键位就是没改，不留一条和出厂一样的覆盖。
        return Recorded::Reset;
    }
    let taken = keymap.into_iter().find(|binding| {
        is_fixed(binding)
            && matches!(binding.keystrokes(), [only] if only.inner().modifiers == keystroke.modifiers
                && only.inner().key == keystroke.key)
    });
    if let Some(binding) = taken {
        return Recorded::Rejected(format!(
            "{} 已被别的功能占用（{}）",
            display(&keystroke),
            binding.action().name()
        ));
    }
    Recorded::Set(Some(keystroke.unparse(os)))
}

/// 键表里改不了、录键时要让开的键。命令的键按 [`Bound`] 算冲突，不看键表里的旧键；设置
/// 窗口自己的关窗键只在设置窗口里生效，不和工作台的键冲突。
fn is_fixed(binding: &KeyBinding) -> bool {
    let action = binding.action().as_any();
    !action.is::<RunCommand>() && !action.is::<NoAction>() && !action.is::<CloseSettingsWindow>()
}

fn same_key(spec: &str, keystroke: &Keystroke, os: OperatingSystem) -> bool {
    Keystroke::parse_with_os(spec, os)
        .is_ok_and(|parsed| parsed.modifiers == keystroke.modifiers && parsed.key == keystroke.key)
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=24).contains(&number))
}

fn is_modifier_key(key: &str) -> bool {
    matches!(key, "shift" | "control" | "alt" | "platform" | "function")
}

pub(crate) fn display(keystroke: &Keystroke) -> String {
    gpui_component::kbd::Kbd::format(keystroke)
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
