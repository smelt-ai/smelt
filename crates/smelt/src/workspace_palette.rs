//! 命令面板（Cmd+K）：打开、关闭、命令列表与执行。

use smelt_plugin_api::CommandInfo;

use super::*;

impl Workspace {
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.new(|cx| gpui_component::command::CommandState::new(window, cx));
        state.update(cx, |state, cx| state.focus(window, cx));
        self.palette = Some(state);
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 命令面板的两组条目：登记过、要进面板的命令，和切换会话。分组下标就是确认回调里的
    /// `IndexPath.section`：0 是命令，1 是会话。过滤不改变这个坐标。只读内存。
    pub(crate) fn palette_groups(&self, cx: &App) -> (Vec<CommandInfo>, Vec<(String, usize)>) {
        let commands = smelt_plugin_api::commands(cx)
            .into_iter()
            .filter(|command| command.palette)
            .collect();
        let sessions = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, session)| {
                (
                    format!("切换到: {}", Self::title_with_runtime(session, cx)),
                    i,
                )
            })
            .collect();
        (commands, sessions)
    }

    /// 面板里选了一个会话。
    pub(crate) fn palette_switch_session(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_palette(window, cx);
        if ix < self.sessions.len() {
            self.activate(ix, window, cx);
        }
    }
}

/// 面板里选了一条命令：先关面板（关面板会把焦点还给当前会话），再执行，命令自己挪的焦点
/// 才不会被关面板盖掉。不能用 `CommandItem::action`：面板确认时先派发 action 再回调，
/// 命令会跑在关面板之前。
pub(crate) fn run_palette_command(
    workspace: &Entity<Workspace>,
    command: &str,
    window: &mut Window,
    cx: &mut App,
) {
    workspace.update(cx, |this, cx| this.close_palette(window, cx));
    let handled = smelt_plugin_api::invoke(command, &serde_json::Value::Null, window, cx);
    crate::workspace_commands::finish(handled, cx);
}
