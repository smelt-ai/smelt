//! 工作台自带的命令：快捷键、应用菜单、命令面板执行的都是这里登记的命令（见
//! `smelt_plugin_api::register_commands`）。
//!
//! 按视图分来源（`smelt.sessions`、`smelt.stage`……）：哪个视图拆成插件，它那组命令跟着
//! 搬走，命令 id 不变，用户改过的键照样认。id 定了就别改，改了用户的覆盖就认不出。

use std::rc::Rc;

use smelt_plugin_api::{self as api, CommandHandler, CommandSpec, Handled};

use super::*;
use crate::plugin_host::WorkspaceRef;

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
    run: Run,
}

/// 来源 → 快捷键设置里的分组名，顺序就是登记顺序。
pub(crate) const OWNERS: &[(&str, &str)] = &[
    ("smelt.app", "应用"),
    ("smelt.palette", "应用"),
    ("smelt.sessions", "会话"),
    ("smelt.stage", "分屏"),
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
        run,
    }
}

const fn hidden(def: Def) -> Def {
    Def {
        palette: false,
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
                Handled::ok()
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
                Handled::ok()
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
            Handled::ok()
        },
    ),
    // 链接打包时可换，见 `settings::DOCS_URL`。
    def("smelt.app", "docs", "帮助文档", None, |_, _, _, cx| {
        cx.open_url(settings::DOCS_URL);
        Handled::ok()
    }),
    def(
        "smelt.app",
        "report-issue",
        "反馈问题",
        None,
        |_, _, _, cx| {
            cx.open_url(settings::FEEDBACK_URL);
            Handled::ok()
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
            Handled::ok()
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
            Handled::ok()
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
            Handled::ok()
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
            Handled::ok()
        },
    ),
    def(
        "smelt.sessions",
        "open-project",
        "打开项目…",
        None,
        |this, _, _, cx| {
            this.open_project(cx);
            Handled::ok()
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
            Handled::ok()
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
            Handled::ok()
        },
    ),
    def(
        "smelt.sessions",
        "next",
        "下一个会话",
        Some("cmd-down"),
        |this, _, window, cx| {
            this.cycle_session(1, window, cx);
            Handled::ok()
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
    // 当前会话的分屏树里循环切活动 pane。编辑器的 Outdent / Indent 绑在更深的 Input 上，
    // 焦点在输入框时由它们接手。
    def(
        "smelt.stage",
        "prev-pane",
        "上一个分屏",
        Some("cmd-["),
        |this, _, window, cx| {
            this.cycle_pane(-1, window, cx);
            Handled::ok()
        },
    ),
    def(
        "smelt.stage",
        "next-pane",
        "下一个分屏",
        Some("cmd-]"),
        |this, _, window, cx| {
            this.cycle_pane(1, window, cx);
            Handled::ok()
        },
    ),
    def(
        "smelt.stage",
        "split-right",
        "左右分屏",
        Some("cmd-d"),
        |this, _, _, cx| {
            this.split_active(Axis::Horizontal, cx);
            Handled::ok()
        },
    ),
    def(
        "smelt.stage",
        "split-down",
        "上下分屏",
        Some("shift-cmd-d"),
        |this, _, _, cx| {
            this.split_active(Axis::Vertical, cx);
            Handled::ok()
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
            Handled::ok()
        },
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
    Handled::ok()
}

/// 登记全部自带命令，处理函数由 `handler` 按 [`Def`] 做出来；带类型的宿主动作（插件用
/// `smelt_plugin_api` 里的常量调）从 `workspace` 取工作台。一组登记失败是表写错了。
fn register_with(
    handler: impl Fn(&'static Def) -> CommandHandler,
    workspace: &WorkspaceRef,
    cx: &mut App,
) {
    for (owner, group) in OWNERS {
        let typed = match *owner {
            "smelt.app" => crate::plugin_host::app_commands(workspace),
            "smelt.sessions" => crate::session_commands::commands(workspace),
            _ => Vec::new(),
        };
        let specs = COMMANDS
            .iter()
            .filter(|def| def.owner == *owner)
            .map(|def| CommandSpec {
                name: def.name.to_string(),
                title: def.title.to_string(),
                key: def.key.map(str::to_string),
                context: def.context.to_string(),
                palette: def.palette,
                requires_args: false,
                handler: handler(def),
            })
            .chain(typed)
            .collect();
        if let Err(error) = api::register_commands(owner, group, specs, cx) {
            debug_assert!(false, "自带命令 {owner} 登记失败：{error}");
            eprintln!("[commands] 自带命令 {owner} 登记失败：{error}");
        }
    }
}

/// 工作台建好时登记。工作台没了的话命令什么都不做，按键交给下一条绑定。
pub(crate) fn register(workspace: WeakEntity<Workspace>, cx: &mut App) {
    let typed: WorkspaceRef = {
        let workspace = workspace.clone();
        Rc::new(move |_: &App| workspace.upgrade())
    };
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
        &typed,
        cx,
    );
}

/// 快捷键、菜单、命令面板执行完命令的收尾：失败统一在这里报给用户，命令自己不报。返回命令
/// 是否眼下不适用（`Pass`），由调用方决定交给下一条绑定还是算了。
pub(crate) fn finish(handled: Handled, cx: &mut App) -> bool {
    match handled {
        Handled::Pass => true,
        Handled::Done(result) => {
            report_failure(result, cx);
            false
        }
    }
}

/// 等命令跑完，失败了报出来。
pub(crate) fn report_failure<T: 'static>(result: Task<Result<T, String>>, cx: &mut App) {
    cx.spawn(async move |_| {
        if let Err(error) = result.await {
            crate::status_item::notify_error(error);
        }
    })
    .detach();
}

/// 测试里不建工作台，只要登记出来的命令表。
#[cfg(test)]
pub(crate) fn register_inert(cx: &mut App) {
    let no_workspace: WorkspaceRef = Rc::new(|_: &App| None);
    register_with(|_| Rc::new(|_, _, _| Handled::ok()), &no_workspace, cx);
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
