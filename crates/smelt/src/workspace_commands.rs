//! 宿主自带的命令：设置、退出、调试面板，和插件调的宿主动作。快捷键与应用菜单
//! 执行的都是登记过的命令（见 `smelt_plugin_api::register_commands`），功能的命令
//! 归各自的插件登记。
//!
//! id 定了就别改，改了用户的覆盖就认不出。

use std::rc::Rc;

use smelt_plugin_api::{self as api, CommandHandler, CommandSpec, Handled};

use super::*;
use crate::plugin_host::WorkspaceRef;

/// 处理函数：工作台、执行时带的参数（快捷键、菜单执行时是 `null`）。
type Run = fn(&mut Workspace, &serde_json::Value, &mut Window, &mut Context<Workspace>) -> Handled;

/// 一条自带命令。键位规则见 [`CommandSpec`]。
pub(crate) struct Def {
    pub(crate) owner: &'static str,
    pub(crate) name: &'static str,
    /// 跟着语言变的标题。常量里放不下 `Text`，放个取它的函数。
    pub(crate) title: fn() -> api::Text,
    pub(crate) key: Option<&'static str>,
    pub(crate) context: &'static str,
    run: Run,
}

/// 来源 → 快捷键设置里的分组名，顺序就是登记顺序。
pub(crate) const OWNERS: &[(&str, fn() -> api::Text)] =
    &[("smelt.app", || api::text!("command.group.app"))];

const fn def(
    owner: &'static str,
    name: &'static str,
    title: fn() -> api::Text,
    key: Option<&'static str>,
    run: Run,
) -> Def {
    Def {
        owner,
        name,
        title,
        key,
        context: "Workspace",
        run,
    }
}

pub(crate) const COMMANDS: &[Def] = &[
    def(
        "smelt.app",
        "settings",
        || api::text!("command.open_settings"),
        Some("cmd-,"),
        |this, _, window, cx| {
            this.open_settings(window, cx);
            Handled::ok()
        },
    ),
    def(
        "smelt.app",
        "quit",
        || api::text!("command.quit"),
        Some("cmd-q"),
        |this, _, _, cx| {
            this.request_quit(cx);
            Handled::ok()
        },
    ),
    // 帧耗时、GPU/CPU/内存。编辑器把同一组合键绑成 Replace，更深的 Input 优先。
    def(
        "smelt.app",
        "debug-hud",
        || api::text!("command.debug_hud"),
        Some("cmd-shift-f"),
        |this, _, _, cx| {
            this.debug_hud = !this.debug_hud;
            cx.notify();
            Handled::ok()
        },
    ),
];

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
            _ => Vec::new(),
        };
        let specs = COMMANDS
            .iter()
            .filter(|def| def.owner == *owner)
            .map(|def| CommandSpec {
                name: def.name.to_string(),
                title: (def.title)(),
                key: def.key.map(str::to_string),
                context: def.context.to_string(),
                requires_args: false,
                args: Some(Vec::new()),
                cli: false,
                handler: handler(def),
            })
            .chain(typed)
            .collect();
        if let Err(error) = api::register_commands(owner, group(), specs, cx) {
            debug_assert!(false, "自带命令 {owner} 登记失败：{error}");
            eprintln!("[commands] 自带命令 {owner} 登记失败：{error}");
        }
    }
}

/// 工作台建好时登记。工作台没了的话命令什么都不做，按键交给下一条绑定；自带命令都要
/// 主窗口，没开时交回 [`api::no_main_window`]。
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
                move |args: &serde_json::Value, window: Option<&mut Window>, cx: &mut App| {
                    let Some(window) = window else {
                        return Handled::err(api::no_main_window());
                    };
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
    /// 退出：先问插件的退出守卫（比如更新插件要在退出时装待装的包），有一个接手就交给它；
    /// 没有的话弹宿主的退出确认。
    pub(crate) fn request_quit(&mut self, cx: &mut Context<Self>) {
        if self.quit_requested {
            return;
        }
        if api::quit_guarded(cx) {
            return;
        }
        self.show_quit_confirm = true;
        cx.notify();
    }

    /// 确认退出。后台守护和里面的会话照常跑着，只是这个窗口断开。
    pub(crate) fn confirm_quit(&mut self, cx: &mut Context<Self>) {
        if self.quit_requested {
            return;
        }
        self.quit_requested = true;
        cx.notify();
        crate::request_app_quit(cx);
    }
}
