//! 工作台自带的命令：快捷键、应用菜单、命令面板执行的都是这里登记的命令（见
//! `smelt_plugin_api::register_commands`）。
//!
//! 按视图分来源（`smelt.sessions`、`smelt.stage`……）：哪个视图拆成插件，它那组命令跟着
//! 搬走，命令 id 不变，用户改过的键照样认。id 定了就别改，改了用户的覆盖就认不出。

use std::rc::Rc;

use smelt_plugin_api::{self as api, CommandHandler, CommandSpec, Handled};

use super::*;

/// 处理函数：工作台、执行时带的参数（快捷键、菜单、命令面板执行时是 `null`）。
type Run = fn(&mut Workspace, &serde_json::Value, &mut Window, &mut Context<Workspace>) -> Handled;

/// 一条自带命令。键位规则见 [`CommandSpec`]。
pub(crate) struct Def {
    pub(crate) owner: &'static str,
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) key: Option<&'static str>,
    pub(crate) context: &'static str,
    pub(crate) palette: bool,
    pub(crate) requires_args: bool,
    run: Run,
}

/// 来源 → 快捷键设置里的分组名，顺序就是登记顺序。
pub(crate) const OWNERS: &[(&str, &str)] = &[
    ("smelt.app", "应用"),
    ("smelt.palette", "应用"),
    ("smelt.sessions", "会话"),
    ("smelt.stage", "分屏"),
    ("smelt.files", "文件"),
    ("smelt.git", "改动"),
];

/// 设置窗口里也要能用的键：设置窗口把收到的命令转给主窗口执行。
const APP_CONTEXT: &str = "Workspace || SettingsWindow";

const fn def(
    owner: &'static str,
    name: &'static str,
    title: &'static str,
    key: Option<&'static str>,
    run: Run,
) -> Def {
    Def {
        owner,
        name,
        title,
        key,
        context: "Workspace",
        palette: true,
        requires_args: false,
        run,
    }
}

const fn hidden(def: Def) -> Def {
    Def {
        palette: false,
        ..def
    }
}

/// 要带参数才能执行的命令：只给插件用 `run` 调，不进面板、不绑键。
const fn with_args(def: Def) -> Def {
    Def {
        palette: false,
        requires_args: true,
        ..def
    }
}

const fn within(context: &'static str, def: Def) -> Def {
    Def { context, ..def }
}

pub(crate) const COMMANDS: &[Def] = &[
    within(
        APP_CONTEXT,
        def(
            "smelt.app",
            "settings",
            "打开设置",
            Some("cmd-,"),
            |this, _, _, cx| {
                this.open_settings(cx);
                Handled::Done
            },
        ),
    ),
    within(
        APP_CONTEXT,
        def(
            "smelt.app",
            "quit",
            "退出 Smelt",
            Some("cmd-q"),
            |this, _, window, cx| {
                this.request_quit(window, cx);
                Handled::Done
            },
        ),
    ),
    // 更新器只在 macOS 上有（设计文档 D4）。
    #[cfg(target_os = "macos")]
    def(
        "smelt.app",
        "check-update",
        "检查更新",
        None,
        |this, _, _, cx| {
            // 顺手发起一次检查，再把设置窗口开到「更新」页看进度。
            if this.update_status.can_check() || this.update_status.can_retry_recovery() {
                this.check_or_recover_update(false, cx);
            }
            this.open_settings_section(settings::SettingsSection::app_update(), cx);
            Handled::Done
        },
    ),
    // 链接打包时可换，见 `settings::DOCS_URL`。
    def("smelt.app", "docs", "帮助文档", None, |_, _, _, cx| {
        cx.open_url(settings::DOCS_URL);
        Handled::Done
    }),
    def(
        "smelt.app",
        "report-issue",
        "反馈问题",
        None,
        |_, _, _, cx| {
            cx.open_url(settings::FEEDBACK_URL);
            Handled::Done
        },
    ),
    // 帧耗时、GPU/CPU/内存。编辑器把同一组合键绑成 Replace，更深的 Input 优先。
    hidden(def(
        "smelt.app",
        "debug-hud",
        "切换调试面板",
        Some("cmd-shift-f"),
        |this, _, _, cx| {
            this.debug_hud = !this.debug_hud;
            cx.notify();
            Handled::Done
        },
    )),
    hidden(def(
        "smelt.palette",
        "toggle",
        "打开命令面板",
        Some("cmd-k"),
        |this, _, window, cx| {
            if this.palette.is_some() {
                this.close_palette(window, cx);
            } else {
                this.open_palette(window, cx);
            }
            Handled::Done
        },
    )),
    // 跟侧栏最顶的「新对话」同一个动作。
    def(
        "smelt.sessions",
        "new-conversation",
        "新对话",
        Some("cmd-n"),
        |this, _, window, cx| {
            this.begin_new_conversation(window, cx);
            Handled::Done
        },
    ),
    // 沿用当前会话的运行时和目录开一个终端会话。
    def(
        "smelt.sessions",
        "new-session",
        "新建会话",
        None,
        |this, _, _, cx| {
            this.new_tab(cx);
            Handled::Done
        },
    ),
    def(
        "smelt.sessions",
        "open-project",
        "打开项目…",
        None,
        |this, _, _, cx| {
            this.open_project(cx);
            Handled::Done
        },
    ),
    // ⌘⇧E 跟 VS Code「聚焦资源管理器」一样，侧栏键盘操作见 `session_list::keyboard`。
    def(
        "smelt.sessions",
        "focus-list",
        "聚焦侧栏",
        Some("cmd-shift-e"),
        |this, _, window, cx| {
            this.focus_sidebar(window, cx);
            Handled::Done
        },
    ),
    // 按侧栏视觉顺序跨项目切，到头循环。绑在 Workspace 上比 Input 浅：输入框里是
    // 文首/文末，其余地方切会话。
    def(
        "smelt.sessions",
        "prev",
        "上一个会话",
        Some("cmd-up"),
        |this, _, window, cx| {
            this.cycle_session(-1, window, cx);
            Handled::Done
        },
    ),
    def(
        "smelt.sessions",
        "next",
        "下一个会话",
        Some("cmd-down"),
        |this, _, window, cx| {
            this.cycle_session(1, window, cx);
            Handled::Done
        },
    ),
    hidden(def(
        "smelt.sessions",
        "activate-1",
        "切换到第 1 个会话",
        Some("cmd-1"),
        activate_nth::<0>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-2",
        "切换到第 2 个会话",
        Some("cmd-2"),
        activate_nth::<1>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-3",
        "切换到第 3 个会话",
        Some("cmd-3"),
        activate_nth::<2>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-4",
        "切换到第 4 个会话",
        Some("cmd-4"),
        activate_nth::<3>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-5",
        "切换到第 5 个会话",
        Some("cmd-5"),
        activate_nth::<4>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-6",
        "切换到第 6 个会话",
        Some("cmd-6"),
        activate_nth::<5>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-7",
        "切换到第 7 个会话",
        Some("cmd-7"),
        activate_nth::<6>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-8",
        "切换到第 8 个会话",
        Some("cmd-8"),
        activate_nth::<7>,
    )),
    hidden(def(
        "smelt.sessions",
        "activate-9",
        "切换到第 9 个会话",
        Some("cmd-9"),
        activate_nth::<8>,
    )),
    // 插件要开会话时调，参数见 `smelt_plugin_api::sessions`。
    with_args(def(
        "smelt.sessions",
        "resume",
        "续接历史会话",
        None,
        |this, args, window, cx| {
            this.run_resume_command(args, window, cx);
            Handled::Done
        },
    )),
    with_args(def(
        "smelt.sessions",
        "resume-in-terminal",
        "在终端里续接历史会话",
        None,
        |this, args, _, cx| {
            this.run_resume_in_terminal_command(args, cx);
            Handled::Done
        },
    )),
    with_args(def(
        "smelt.sessions",
        "handoff",
        "交接给另一家 agent",
        None,
        |this, args, window, cx| {
            this.run_handoff_command(args, window, cx);
            Handled::Done
        },
    )),
    // 当前会话的分屏树里循环切活动 pane。编辑器的 Outdent / Indent 绑在更深的 Input 上，
    // 焦点在输入框时由它们接手。
    def(
        "smelt.stage",
        "prev-pane",
        "上一个分屏",
        Some("cmd-["),
        |this, _, window, cx| {
            this.cycle_pane(-1, window, cx);
            Handled::Done
        },
    ),
    def(
        "smelt.stage",
        "next-pane",
        "下一个分屏",
        Some("cmd-]"),
        |this, _, window, cx| {
            this.cycle_pane(1, window, cx);
            Handled::Done
        },
    ),
    def(
        "smelt.stage",
        "split-right",
        "左右分屏",
        Some("cmd-d"),
        |this, _, _, cx| {
            this.split_active(Axis::Horizontal, cx);
            Handled::Done
        },
    ),
    def(
        "smelt.stage",
        "split-down",
        "上下分屏",
        Some("shift-cmd-d"),
        |this, _, _, cx| {
            this.split_active(Axis::Vertical, cx);
            Handled::Done
        },
    ),
    // 会话只剩一个 pane 时关掉整个会话（至少留一个会话）。
    def(
        "smelt.stage",
        "close-pane",
        "关闭当前分屏或会话",
        Some("cmd-w"),
        |this, _, window, cx| {
            this.close_active(window, cx);
            Handled::Done
        },
    ),
    // 键位只在 Files context 上，文件页的两种显示方式都算。
    within(
        "Files",
        def(
            "smelt.files",
            "save",
            "保存当前文件",
            Some("cmd-s"),
            |this, _, _, cx| {
                this.save_open_file(cx);
                Handled::Done
            },
        ),
    ),
    // 文件内容视图右键菜单里的那一项，见 `send_open_file_selection`。
    hidden(def(
        "smelt.files",
        "send-selection",
        "发送选中内容到会话",
        None,
        |this, _, window, cx| {
            this.send_open_file_selection(window, cx);
            Handled::Done
        },
    )),
    within(
        "GitChanges",
        def(
            "smelt.git",
            "prev-hunk",
            "上一个差异",
            Some("shift-f7"),
            |this, _, window, cx| jump_hunk(this, false, window, cx),
        ),
    ),
    within(
        "GitChanges",
        def(
            "smelt.git",
            "next-hunk",
            "下一个差异",
            Some("f7"),
            |this, _, window, cx| jump_hunk(this, true, window, cx),
        ),
    ),
];

/// 按侧栏视觉顺序（各项目分组依次铺平）跳到第 `N + 1` 个会话。
fn activate_nth<const N: usize>(
    this: &mut Workspace,
    _: &serde_json::Value,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Handled {
    if let Some(&ix) = this.sidebar_session_order(cx).get(N) {
        this.activate(ix, window, cx);
    }
    Handled::Done
}

/// 终端网格会把 F7 / Shift+F7 编码进 PTY，网格聚焦时让出，交给终端自己的 on_key_down。
fn jump_hunk(
    this: &mut Workspace,
    forward: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Handled {
    if this.terminal_grid_focused(window, cx) {
        return Handled::Pass;
    }
    this.jump_hunk(forward, cx);
    Handled::Done
}

/// 登记全部自带命令，处理函数由 `handler` 按 [`Def`] 做出来。一组登记失败是表写错了。
fn register_with(handler: impl Fn(&'static Def) -> CommandHandler, cx: &mut App) {
    for (owner, group) in OWNERS {
        let specs = COMMANDS
            .iter()
            .filter(|def| def.owner == *owner)
            .map(|def| CommandSpec {
                name: def.name.to_string(),
                title: def.title.to_string(),
                key: def.key.map(str::to_string),
                context: def.context.to_string(),
                palette: def.palette,
                requires_args: def.requires_args,
                handler: handler(def),
            })
            .collect();
        if let Err(error) = api::register_commands(owner, group, specs, cx) {
            debug_assert!(false, "自带命令 {owner} 登记失败：{error}");
            eprintln!("[commands] 自带命令 {owner} 登记失败：{error}");
        }
    }
}

/// 工作台建好时登记。工作台没了的话命令什么都不做，按键交给下一条绑定。
pub(crate) fn register(workspace: WeakEntity<Workspace>, cx: &mut App) {
    register_with(
        |def| {
            let workspace = workspace.clone();
            let run = def.run;
            Rc::new(
                move |args: &serde_json::Value, window: &mut Window, cx: &mut App| {
                    workspace
                        .update(cx, |this, cx| run(this, args, window, cx))
                        .unwrap_or(Handled::Pass)
                },
            )
        },
        cx,
    );
}

/// 测试里不建工作台，只要登记出来的命令表。
#[cfg(test)]
pub(crate) fn register_inert(cx: &mut App) {
    register_with(|_| Rc::new(|_, _, _| Handled::Done), cx);
}

impl Workspace {
    /// 退出：有进行中的会话、或者要先装更新时先确认，不能把正在输出的对话静默杀掉。
    pub(crate) fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_requested {
            return;
        }
        self.stop_prompt_names.clear();
        #[cfg(any(target_os = "macos", windows))]
        if !matches!(self.update_status, updater::UpdateStatus::Applying { .. })
            && self.update_status.ready_update().is_some()
        {
            // 有待装更新时退出会换包，macOS 还会停掉守护。没有进行中的会话、或者
            // 不停守护，就直接走；否则必须先确认。
            let names = self.busy_work_names(cx);
            if !updater::INSTALL_STOPS_LOCAL_DAEMON
                || !daemon_restart_prompt::should_confirm(&names)
            {
                if let Some(update) = self.update_status.ready_update().cloned() {
                    self.start_update_install(update, UpdateInstallTrigger::Quit, cx);
                }
                return;
            }
            self.stop_prompt_names = names;
        }
        self.begin_main_modal_focus(window, cx);
        self.show_quit_confirm = true;
        cx.notify();
    }
}
