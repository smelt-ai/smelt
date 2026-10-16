//! 命令面板（Cmd+K）：打开、关闭、命令列表与执行。

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

    /// 命令面板的两组条目。分组下标就是确认回调里的 `IndexPath.section`：
    /// 0 是命令，1 是切换会话。过滤不改变这个坐标。
    ///
    /// 这里不挂 `PrevSession` / `NextSession`。确认会先派发 Action 再跑回调，
    /// 而那两个 Action 按侧栏视觉顺序切，面板这里按会话数组顺序切。
    pub(crate) fn palette_groups(&self, cx: &App) -> [Vec<(String, Cmd)>; 2] {
        let commands = vec![
            ("新建会话".to_string(), Cmd::NewTab),
            ("打开项目…".to_string(), Cmd::OpenProject),
            ("关闭当前会话/窗格".to_string(), Cmd::CloseTab),
            ("下一个会话".to_string(), Cmd::NextTab),
            ("上一个会话".to_string(), Cmd::PrevTab),
        ];
        let sessions = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, session)| (format!("切换到: {}", session.title(cx)), Cmd::SwitchTab(i)))
            .collect();
        [commands, sessions]
    }

    pub(crate) fn exec_cmd(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        match cmd {
            Cmd::NewTab => self.new_tab(cx),
            Cmd::OpenProject => self.open_project(cx),
            Cmd::CloseTab => self.close_active(window, cx),
            Cmd::NextTab => self.next_active(window, cx),
            Cmd::PrevTab => self.prev_active(window, cx),
            Cmd::SwitchTab(i) => self.activate(i, window, cx),
        }
    }
}
