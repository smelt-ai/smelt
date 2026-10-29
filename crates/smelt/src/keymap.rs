//! 快捷键：出厂键位、用户改过的覆盖，以及把它们装进 GPUI 键表。
//!
//! 能改的动作列在 [`SHORTCUTS`]。`id` 是存库用的名字，定了就别改：改了旧覆盖就认不出。
//! 用户改过的存在客户端库的 `key_binding` 表；读到认不出的动作或解析不了的按键，当没改。
//!
//! 改键不能把整张键表清空重来：gpui-shell 插件第一次用到时才往键表里加自己的键，清掉就
//! 回不来了。smelt 自己的键都打上 [`APP_KEYMAP`] 标记，改键时只把这一段原地换掉，前后别人
//! 的键和先后顺序都不动。
//!
//! 新的工作区组合键绑在 `Workspace` 或更具体、但仍浅于 Terminal / Input 的 context 上。
//! `None` 与最内层同深度，后注册会抢走输入框的键，这里不再新增。

use std::collections::BTreeMap;

use gpui::{
    Action, App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate,
    KeyBindingMetaIndex, Keystroke, Menu, MenuItem, NoAction, Subscription, Window,
};
use gpui_component::input::{
    DeleteToBeginningOfLine, DeleteToEndOfLine, DeleteToPreviousWordStart,
};
use smelt_client_store::KeyBindingRecord;

use super::*;

/// smelt 自己注册的键都带这个标记，改键时据此认出要换掉的那一段。
/// 值只在 `NoAction` 判优先级时有用，和不带标记的键一样按用户键位算。
const APP_KEYMAP: KeyBindingMetaIndex = KeyBindingMetaIndex(0);

/// 动作名 → 用户改的按键。`None` 是用户清掉了，不绑任何键。
pub(crate) type Overrides = BTreeMap<String, Option<String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutGroup {
    Global,
    Session,
    Edit,
}

/// 一条能改的快捷键。
pub(crate) struct Shortcut {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) group: ShortcutGroup,
    pub(crate) context: Option<&'static str>,
    pub(crate) default: &'static str,
    action: fn() -> Box<dyn Action>,
}

impl Shortcut {
    pub(crate) fn action(&self) -> Box<dyn Action> {
        (self.action)()
    }

    /// 现在生效的按键：改过用改过的，没改过用出厂的，清掉了是 `None`。
    pub(crate) fn keystroke<'a>(&'a self, overrides: &'a Overrides) -> Option<&'a str> {
        match overrides.get(self.id) {
            Some(keystroke) => keystroke.as_deref(),
            None => Some(self.default),
        }
    }
}

pub(crate) const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        id: "open_settings",
        label: "打开设置",
        group: ShortcutGroup::Global,
        context: None,
        default: "cmd-,",
        action: || Box::new(OpenSettings),
    },
    Shortcut {
        id: "command_palette",
        label: "打开命令面板",
        group: ShortcutGroup::Global,
        context: Some("Workspace"),
        default: "cmd-k",
        action: || Box::new(ToggleCommandPalette),
    },
    Shortcut {
        id: "new_conversation",
        label: "新对话",
        group: ShortcutGroup::Global,
        context: Some("Workspace"),
        default: "cmd-n",
        action: || Box::new(NewConversation),
    },
    Shortcut {
        id: "toggle_sidebar",
        label: "切换左侧栏",
        group: ShortcutGroup::Session,
        context: None,
        default: "cmd-b",
        action: || Box::new(ToggleSidebar),
    },
    Shortcut {
        id: "toggle_tool_panel",
        label: "切换右侧面板",
        group: ShortcutGroup::Session,
        context: None,
        default: "alt-cmd-b",
        action: || Box::new(ToggleToolPanel),
    },
    // ⌘⇧E 跟 VS Code「聚焦资源管理器」一样，侧栏键盘操作见 `session_list::keyboard`。
    Shortcut {
        id: "focus_sidebar",
        label: "聚焦侧栏",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "cmd-shift-e",
        action: || Box::new(FocusSidebar),
    },
    // 会话上/下切换（跨项目按侧栏视觉顺序，到头循环）。绑在 Workspace 上比 Input 浅：
    // 输入框里是文首/文末，其余工作区切会话。
    Shortcut {
        id: "prev_session",
        label: "上一个会话",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "cmd-up",
        action: || Box::new(PrevSession),
    },
    Shortcut {
        id: "next_session",
        label: "下一个会话",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "cmd-down",
        action: || Box::new(NextSession),
    },
    Shortcut {
        id: "prev_pane",
        label: "上一个分屏",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "cmd-[",
        action: || Box::new(CyclePanePrev),
    },
    Shortcut {
        id: "next_pane",
        label: "下一个分屏",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "cmd-]",
        action: || Box::new(CyclePaneNext),
    },
    Shortcut {
        id: "split_beside",
        label: "左右分屏",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "cmd-d",
        action: || Box::new(SplitBeside),
    },
    Shortcut {
        id: "split_below",
        label: "上下分屏",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "shift-cmd-d",
        action: || Box::new(SplitBelow),
    },
    Shortcut {
        id: "close_pane",
        label: "关闭当前分屏或会话",
        group: ShortcutGroup::Session,
        context: Some("Workspace"),
        default: "cmd-w",
        action: || Box::new(ClosePane),
    },
    Shortcut {
        id: "save_file",
        label: "保存当前文件",
        group: ShortcutGroup::Edit,
        context: Some("Files"),
        default: "cmd-s",
        action: || Box::new(SaveFile),
    },
    Shortcut {
        id: "prev_hunk",
        label: "上一个差异",
        group: ShortcutGroup::Edit,
        context: Some("GitChanges"),
        default: "shift-f7",
        action: || Box::new(PrevHunk),
    },
    Shortcut {
        id: "next_hunk",
        label: "下一个差异",
        group: ShortcutGroup::Edit,
        context: Some("GitChanges"),
        default: "f7",
        action: || Box::new(NextHunk),
    },
];

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

/// 应用键位：改不了的固定键，加上 [`SHORTCUTS`] 按覆盖算出来的键。
pub(crate) fn app_key_bindings(overrides: &Overrides) -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-shift-f", ToggleDebugHud, Some("Workspace")),
        KeyBinding::new("escape", DismissWorkspace, Some("Workspace")),
        // 设置窗口：Cmd+W（Windows / Linux 是 Ctrl+W）关窗。它是普通窗口，不是模态层，
        // Esc 不关它，留给下拉框、弹出层和输入框。
        KeyBinding::new("secondary-w", CloseSettingsWindow, Some("SettingsWindow")),
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
    for (index, key) in [
        "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
    ]
    .into_iter()
    .enumerate()
    {
        bindings.push(KeyBinding::new(
            key,
            ActivateSession { index },
            Some("Workspace"),
        ));
    }
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
    for shortcut in SHORTCUTS {
        let Some(keystroke) = shortcut.keystroke(overrides) else {
            continue;
        };
        let context: Option<std::rc::Rc<KeyBindingContextPredicate>> =
            shortcut.context.map(|context| {
            KeyBindingContextPredicate::parse(context)
                .expect("快捷键的 context 写错了")
                .into()
        });
        // 覆盖在读库时已经验过能解析，出厂键位由测试兜着。
        match KeyBinding::load(
            keystroke,
            shortcut.action(),
            context,
            false,
            None,
            &DummyKeyboardMapper,
        ) {
            Ok(binding) => bindings.push(binding),
            Err(error) => eprintln!("[keymap] {} 的按键 {keystroke} 解析不了：{error}", shortcut.id),
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
    vec![
        Menu::new("Smelt").items([
            MenuItem::action(format!("v{}", smelt_version::VERSION), NoAction).disabled(true),
            #[cfg(target_os = "macos")]
            MenuItem::Separator,
            #[cfg(target_os = "macos")]
            MenuItem::action("检查更新…", CheckForUpdate),
            MenuItem::Separator,
            MenuItem::action("设置…", OpenSettings),
            MenuItem::Separator,
            MenuItem::action("退出 Smelt", Quit),
        ]),
        Menu::new("帮助").items([
            MenuItem::action("帮助文档", OpenDocs),
            MenuItem::action("反馈问题", ReportIssue),
        ]),
    ]
}

/// 快捷键的全局状态：用户的覆盖，以及设置页正在录哪一条。
#[derive(Default)]
pub(crate) struct KeymapState {
    overrides: Overrides,
    /// 设置页正在录新按键的那一条。
    pub(crate) recording: Option<&'static str>,
    /// 最近一次改键没成：哪一条、为什么。
    pub(crate) error: Option<(&'static str, String)>,
    /// 录到的键已经是另一条的：等用户确认改绑还是算了。
    pub(crate) reassign: Option<Reassign>,
    /// 录键时抢在动作派发前拿到按键，录完就丢掉。
    interceptor: Option<Subscription>,
}

impl Global for KeymapState {}

/// 改绑：`keystroke` 给 `id`，`other` 原来用着它，确认后清掉。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reassign {
    pub(crate) id: &'static str,
    pub(crate) other: &'static str,
    pub(crate) keystroke: String,
}

impl KeymapState {
    pub(crate) fn overrides(&self) -> &Overrides {
        &self.overrides
    }
}

/// 启动时：读用户的覆盖，装进键表，设菜单。必须在 `set_menus` 之前装键，菜单项才带快捷键，
/// 这里两件一起做。
pub(crate) fn init(cx: &mut App) {
    cx.set_global(KeymapState {
        overrides: load_overrides(),
        ..KeymapState::default()
    });
    install(cx);
}

fn load_overrides() -> Overrides {
    let records = match smelt_client_store::client_store().and_then(|store| store.key_bindings()) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("[keymap] 读快捷键设置失败，用出厂键位：{error}");
            return Overrides::new();
        }
    };
    overrides_from_records(records)
}

/// 认不出的动作、解析不了的按键都丢掉，那一条回到出厂键位。
fn overrides_from_records(records: Vec<KeyBindingRecord>) -> Overrides {
    records
        .into_iter()
        .filter_map(|record| {
            let Some(shortcut) = shortcut(&record.action_id) else {
                eprintln!("[keymap] 不认识的快捷键动作 {}，忽略", record.action_id);
                return None;
            };
            if let Some(keystroke) = &record.keystroke
                && Keystroke::parse(keystroke).is_err()
            {
                eprintln!("[keymap] {} 的按键 {keystroke} 解析不了，用出厂键位", shortcut.id);
                return None;
            }
            Some((record.action_id, record.keystroke))
        })
        .collect()
}

pub(crate) fn shortcut(id: &str) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|shortcut| shortcut.id == id)
}

/// 把键表里 smelt 那一段换成按当前覆盖算出来的，再重设菜单。
fn install(cx: &mut App) {
    let app = app_key_bindings(&cx.global::<KeymapState>().overrides);
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
    let mut others = existing.into_iter().filter(|binding| !is_app_binding(binding));
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
    failed: &'static str,
    changes: Vec<(&'static str, Option<String>)>,
    cx: &mut App,
) {
    let records: Vec<KeyBindingRecord> = changes
        .iter()
        .map(|(id, keystroke)| KeyBindingRecord {
            action_id: id.to_string(),
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
pub(crate) fn reset(id: &'static str, cx: &mut App) {
    let saved = smelt_client_store::client_store().and_then(|store| store.delete_key_binding(id));
    apply_saved(id, saved, cx, |overrides| {
        overrides.remove(id);
    });
}

fn apply_saved(
    failed: &'static str,
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
    save_overrides(id, vec![(other, None), (id, Some(keystroke))], cx);
}

pub(crate) fn cancel_reassign(cx: &mut App) {
    cx.update_global::<KeymapState, _>(|state, _| state.reassign = None);
}

/// 设置页点了某条的键帽：开始录。设置窗口里的下一次按键先到这里，不派发成动作，所以
/// ⌘W、⌘Q 这种已经有用处的键也录得到。按键落在别的窗口（设置窗口关了、切走了）就不录了，
/// 那个键照常交给那个窗口。
pub(crate) fn start_recording(id: &'static str, window: &Window, cx: &mut App) {
    let recording_window = window.window_handle().window_id();
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

fn finish_recording(id: &'static str, keystroke: Keystroke, cx: &mut App) {
    if cx.global::<KeymapState>().recording != Some(id) {
        return;
    }
    stop_recording(cx);
    let outcome = {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        recorded(id, &keystroke, &cx.global::<KeymapState>().overrides, keymap.bindings())
    };
    match outcome {
        Recorded::Cancel => {}
        Recorded::Reset => reset(id, cx),
        Recorded::Set(keystroke) => save_overrides(id, vec![(id, keystroke)], cx),
        Recorded::Taken { other, keystroke } => {
            cx.update_global::<KeymapState, _>(|state, _| {
                state.reassign = Some(Reassign {
                    id,
                    other,
                    keystroke,
                });
            })
        }
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
    /// 另一条快捷键正用着它，问用户要不要改绑。
    Taken {
        other: &'static str,
        keystroke: String,
    },
    Rejected(String),
}

/// 录到一个按键之后怎么办。Esc 取消，⌫ 清掉；别的键要带 ⌘（或者是 F 键），不能是保留键，
/// 也不能撞上组件自带或改不了的键。撞上另一条能改的快捷键，交给用户决定要不要改绑。
fn recorded<'a>(
    id: &str,
    keystroke: &Keystroke,
    overrides: &Overrides,
    keymap: impl IntoIterator<Item = &'a KeyBinding>,
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
        .find(|(reserved, _)| same_key(reserved, &keystroke))
    {
        return Recorded::Rejected(format!("{} 留给{what}", display(&keystroke)));
    }
    if let Some(other) = SHORTCUTS.iter().find(|other| {
        other.id != id
            && other
                .keystroke(overrides)
                .is_some_and(|key| same_key(key, &keystroke))
    }) {
        return Recorded::Taken {
            other: other.id,
            keystroke: keystroke.unparse(),
        };
    }
    let taken = keymap.into_iter().find(|binding| {
        !is_rebindable(binding)
            && !binding.action().as_any().is::<NoAction>()
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
    if shortcut(id).is_some_and(|shortcut| same_key(shortcut.default, &keystroke)) {
        // 录回出厂键位就是没改，不留一条和出厂一样的覆盖。
        return Recorded::Reset;
    }
    Recorded::Set(Some(keystroke.unparse()))
}

/// smelt 自己那段里、属于能改的那些。它们的冲突按 [`SHORTCUTS`] 和覆盖算，不看键表里的旧键。
fn is_rebindable(binding: &KeyBinding) -> bool {
    is_app_binding(binding)
        && SHORTCUTS
            .iter()
            .any(|shortcut| binding.action().partial_eq(shortcut.action().as_ref()))
}

fn same_key(spec: &str, keystroke: &Keystroke) -> bool {
    Keystroke::parse(spec).is_ok_and(|parsed| {
        parsed.modifiers == keystroke.modifiers && parsed.key == keystroke.key
    })
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
