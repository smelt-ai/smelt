//! smelt 工作台 —— 基于 gpui-component 的桌面窗口。
//!
//! Workspace 管理多个终端标签（TerminalView）：顶部标签栏切换 / 新建 / 关闭，
//! 下方渲染当前活动终端。每个终端各自独立（PTY、IME、滚动、resize）。
//!
//! 运行： cargo run --bin workspace

mod terminal;
mod terminal_view;

use gpui::*;
use gpui_component::*;
use terminal_view::TerminalView;

/// 命令面板里的一个可执行动作。
#[derive(Clone)]
enum Cmd {
    NewTab,
    OpenProject,
    CloseTab,
    NextTab,
    PrevTab,
    SwitchTab(usize),
}

/// 命令面板状态。
struct Palette {
    query: String,
    selected: usize,
}

/// 工作台根视图：多标签终端管理器。
struct Workspace {
    tabs: Vec<Entity<TerminalView>>,
    active: usize,
    /// 网格列数：1=单终端，2=两列，3=三列。
    layout_cols: usize,
    /// 左侧会话侧栏是否展开（Cmd+B 切换）。
    sidebar_open: bool,
    /// 命令面板（Cmd+K）；None 表示未打开。
    palette: Option<Palette>,
    palette_focus: FocusHandle,
}

impl Workspace {
    fn new(cx: &mut Context<Self>) -> Self {
        let first = cx.new(|cx| TerminalView::new(cx, current_dir()));
        Self {
            tabs: vec![first],
            active: 0,
            layout_cols: 1,
            sidebar_open: true,
            palette: None,
            palette_focus: cx.focus_handle(),
        }
    }

    /// 在指定目录新建标签并激活。
    fn add_tab(&mut self, cwd: Option<String>, cx: &mut Context<Self>) {
        let view = cx.new(|cx| TerminalView::new(cx, cwd));
        self.tabs.push(view);
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    /// 「+」新建标签：继承当前活动标签的目录。
    fn new_tab(&mut self, cx: &mut Context<Self>) {
        let cwd = self
            .tabs
            .get(self.active)
            .and_then(|t| t.read(cx).cwd())
            .or_else(current_dir);
        self.add_tab(cwd, cx);
    }

    /// 「打开项目」：弹原生选择框选一个目录，在其中开新标签。
    fn open_project(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择项目目录".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(dir) = paths.into_iter().next() {
                    let dir = dir.to_str().map(String::from);
                    this.update(cx, |this, cx| this.add_tab(dir, cx)).ok();
                }
            }
        })
        .detach();
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 || ix >= self.tabs.len() {
            return; // 至少保留一个终端
        }
        self.tabs.remove(ix);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > ix {
            self.active -= 1;
        }
        cx.notify();
    }

    /// 聚焦当前活动终端。
    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(t) = self.tabs.get(self.active) {
            let h = t.read(cx).focus_handle();
            window.focus(&h, cx);
        }
    }

    /// 切换到第 ix 个标签并聚焦。
    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() {
            self.active = ix;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    /// 循环切换网格布局：1 → 2 → 3 → 1 列。
    fn cycle_layout(&mut self, cx: &mut Context<Self>) {
        self.layout_cols = match self.layout_cols {
            1 => 2,
            2 => 3,
            _ => 1,
        };
        cx.notify();
    }

    fn next_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        if n > 0 {
            self.activate((self.active + 1) % n, window, cx);
        }
    }

    fn prev_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        if n > 0 {
            self.activate((self.active + n - 1) % n, window, cx);
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = Some(Palette {
            query: String::new(),
            selected: 0,
        });
        window.focus(&self.palette_focus, cx);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 全部命令（含逐标签切换）。
    fn all_commands(&self, cx: &App) -> Vec<(String, Cmd)> {
        let mut v = vec![
            ("新建标签".to_string(), Cmd::NewTab),
            ("打开项目…".to_string(), Cmd::OpenProject),
            ("关闭当前标签".to_string(), Cmd::CloseTab),
            ("下一个标签".to_string(), Cmd::NextTab),
            ("上一个标签".to_string(), Cmd::PrevTab),
        ];
        for (i, t) in self.tabs.iter().enumerate() {
            v.push((format!("切换到: {}", t.read(cx).title()), Cmd::SwitchTab(i)));
        }
        v
    }

    /// 按查询过滤后的命令。
    fn filtered(&self, cx: &App) -> Vec<(String, Cmd)> {
        let q = self
            .palette
            .as_ref()
            .map(|p| p.query.to_lowercase())
            .unwrap_or_default();
        self.all_commands(cx)
            .into_iter()
            .filter(|(label, _)| q.is_empty() || label.to_lowercase().contains(&q))
            .collect()
    }

    fn exec_cmd(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        match cmd {
            Cmd::NewTab => self.new_tab(cx),
            Cmd::OpenProject => self.open_project(cx),
            Cmd::CloseTab => self.close_tab(self.active, cx),
            Cmd::NextTab => {
                let n = self.tabs.len();
                if n > 0 {
                    self.activate((self.active + 1) % n, window, cx);
                }
            }
            Cmd::PrevTab => {
                let n = self.tabs.len();
                if n > 0 {
                    self.activate((self.active + n - 1) % n, window, cx);
                }
            }
            Cmd::SwitchTab(i) => self.activate(i, window, cx),
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;
        let can_close = self.tabs.len() > 1;

        // 先收集标签标题，释放对 self.tabs 的借用
        let titles: Vec<(usize, String)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, v)| (ix, v.read(cx).title().to_string()))
            .collect();

        // 左侧会话侧栏
        let sidebar = if self.sidebar_open {
            let rows: Vec<Stateful<Div>> = titles
                .iter()
                .map(|(ix, title)| sidebar_row(*ix, title.clone(), *ix == active, can_close, cx))
                .collect();
            Some(
                div()
                    .flex()
                    .flex_col()
                    .w(px(200.))
                    .h_full()
                    .bg(rgb(0x16161e))
                    .border_r_1()
                    .border_color(rgb(0x2a2b3d))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(rgb(0x565f89))
                            .child("会话"),
                    )
                    .child(div().flex().flex_col().flex_1().children(rows))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .border_t_1()
                            .border_color(rgb(0x2a2b3d))
                            .child(new_tab_button(cx))
                            .child(open_project_button(cx))
                            .child(div().flex_1())
                            .child(layout_button(self.layout_cols, cx)),
                    ),
            )
        } else {
            None
        };

        // 主内容：单终端 或 网格（多列）
        let cols = self.layout_cols;
        let n = self.tabs.len();
        let content = if cols <= 1 {
            div().flex_1().child(self.tabs[active].clone())
        } else {
            let rows: Vec<Div> = (0..n)
                .step_by(cols)
                .map(|start| {
                    let end = (start + cols).min(n);
                    let cards: Vec<Div> = (start..end)
                        .map(|ix| {
                            let is_active = ix == active;
                            let view = self.tabs[ix].clone();
                            let title = titles.get(ix).map(|(_, t)| t.clone()).unwrap_or_default();
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .border_1()
                                .border_color(if is_active {
                                    rgb(0x7aa2f7)
                                } else {
                                    rgb(0x2a2b3d)
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _ev, window, cx| {
                                        this.activate(ix, window, cx)
                                    }),
                                )
                                // 卡片标题头
                                .child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .text_sm()
                                        .bg(if is_active { rgb(0x2a2b3d) } else { rgb(0x16161e) })
                                        .text_color(if is_active {
                                            rgb(0xc0caf5)
                                        } else {
                                            rgb(0x565f89)
                                        })
                                        .child(title),
                                )
                                .child(div().flex_1().child(view))
                        })
                        .collect();
                    div().flex_1().flex().gap_1().children(cards)
                })
                .collect();
            div().flex_1().flex().flex_col().gap_1().p_1().children(rows)
        };

        // 命令面板弹层
        let palette_overlay = self.palette.as_ref().map(|p| {
            let cmds = self.filtered(cx);
            let sel = if cmds.is_empty() {
                0
            } else {
                p.selected.min(cmds.len() - 1)
            };
            let query = p.query.clone();
            let items: Vec<Stateful<Div>> = cmds
                .iter()
                .enumerate()
                .map(|(i, (label, cmd))| {
                    let is_sel = i == sel;
                    let cmd = cmd.clone();
                    let mut d = div()
                        .id(("cmd", i))
                        .px_3()
                        .py_1()
                        .text_color(if is_sel { rgb(0xc0caf5) } else { rgb(0x9aa5ce) })
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            this.exec_cmd(cmd.clone(), window, cx)
                        }))
                        .child(label.clone());
                    if is_sel {
                        d = d.bg(rgb(0x2a2b3d));
                    }
                    d
                })
                .collect();

            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(80.))
                .child(
                    div()
                        .track_focus(&self.palette_focus)
                        .on_key_down(cx.listener(palette_key))
                        .w(px(480.))
                        .flex()
                        .flex_col()
                        .bg(rgb(0x16161e))
                        .border_1()
                        .border_color(rgb(0x2a2b3d))
                        .rounded_lg()
                        .shadow_lg()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .text_color(rgb(0xc0caf5))
                                .child(if query.is_empty() {
                                    "› 输入命令…".to_string()
                                } else {
                                    format!("› {}", query)
                                }),
                        )
                        .children(items),
                )
        });

        div()
            .relative()
            .flex()
            .size_full()
            .bg(rgb(0x1a1b26))
            .font_family(terminal_view::FONT_FAMILY)
            // 全局快捷键：Cmd+K 面板 / Cmd+B 侧栏 / Cmd+\ 布局 / Cmd+[ ] 切换
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                if !ks.modifiers.platform {
                    return;
                }
                match ks.key.as_str() {
                    "k" => {
                        if this.palette.is_some() {
                            this.close_palette(window, cx);
                        } else {
                            this.open_palette(window, cx);
                        }
                    }
                    "b" => {
                        this.sidebar_open = !this.sidebar_open;
                        cx.notify();
                    }
                    "\\" => this.cycle_layout(cx),
                    "[" => this.prev_active(window, cx),
                    "]" => this.next_active(window, cx),
                    _ => {}
                }
            }))
            // 左侧会话侧栏
            .children(sidebar)
            // 主区（单终端 / 网格）
            .child(div().flex_1().flex().flex_col().child(content))
            // 命令面板（最上层）
            .children(palette_overlay)
    }
}

/// 命令面板的键盘处理：字符过滤、上下选择、回车执行、Esc 关闭。
fn palette_key(
    this: &mut Workspace,
    ev: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if this.palette.is_none() {
        return;
    }
    let ks = &ev.keystroke;
    match ks.key.as_str() {
        "escape" => this.close_palette(window, cx),
        "up" => {
            if let Some(p) = this.palette.as_mut() {
                p.selected = p.selected.saturating_sub(1);
            }
            cx.notify();
        }
        "down" => {
            let len = this.filtered(cx).len();
            if let Some(p) = this.palette.as_mut() {
                if len > 0 && p.selected + 1 < len {
                    p.selected += 1;
                }
            }
            cx.notify();
        }
        "backspace" => {
            if let Some(p) = this.palette.as_mut() {
                p.query.pop();
                p.selected = 0;
            }
            cx.notify();
        }
        "enter" => {
            let sel = this.palette.as_ref().map(|p| p.selected).unwrap_or(0);
            let cmds = this.filtered(cx);
            if let Some((_, cmd)) = cmds.into_iter().nth(sel) {
                this.exec_cmd(cmd, window, cx);
            }
        }
        _ => {
            if !ks.modifiers.platform && !ks.modifiers.control && !ks.modifiers.function {
                if let Some(kc) = ks.key_char.clone() {
                    if !kc.is_empty() {
                        if let Some(p) = this.palette.as_mut() {
                            p.query.push_str(&kc);
                            p.selected = 0;
                        }
                        cx.notify();
                    }
                }
            }
        }
    }
}

/// 侧栏里的一个会话行：点击切换；活动态高亮；可关闭时带「×」。
fn sidebar_row(
    ix: usize,
    title: String,
    active: bool,
    can_close: bool,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    let (bg, fg) = if active {
        (rgb(0x2a2b3d), rgb(0xc0caf5))
    } else {
        (rgb(0x16161e), rgb(0x9aa5ce))
    };

    let mut row = div()
        .id(("sess", ix))
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .on_click(cx.listener(move |this, _ev, window, cx| {
            this.activate(ix, window, cx);
        }))
        .child(div().flex_1().child(title));

    if can_close {
        row = row.child(
            div()
                .id(("sess-close", ix))
                .px_1()
                .rounded_sm()
                .text_color(rgb(0x565f89))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.close_tab(ix, cx);
                }))
                .child("×"),
        );
    }

    row
}

/// 「+」新建标签按钮（继承当前项目目录）。
fn new_tab_button(cx: &mut Context<Workspace>) -> Stateful<Div> {
    div()
        .id("new-tab")
        .px_2()
        .py_1()
        .rounded_md()
        .text_color(rgb(0x7aa2f7))
        .on_click(cx.listener(|this, _ev, _window, cx| {
            this.new_tab(cx);
        }))
        .child("+")
}

/// 布局切换按钮：显示当前列数图标，点击循环 1/2/3 列。
fn layout_button(cols: usize, cx: &mut Context<Workspace>) -> Stateful<Div> {
    let icon = match cols {
        1 => "▢",
        2 => "▥",
        _ => "▦",
    };
    div()
        .id("layout")
        .px_2()
        .py_1()
        .rounded_md()
        .text_color(rgb(0x565f89))
        .on_click(cx.listener(|this, _ev, _window, cx| this.cycle_layout(cx)))
        .child(icon)
}

/// 「打开项目」按钮：弹选择框选目录，在其中开新标签。
fn open_project_button(cx: &mut Context<Workspace>) -> Stateful<Div> {
    div()
        .id("open-project")
        .px_2()
        .py_1()
        .rounded_md()
        .text_color(rgb(0x565f89))
        .on_click(cx.listener(|this, _ev, _window, cx| {
            this.open_project(cx);
        }))
        .child("📂")
}

/// 当前工作目录字符串。
fn current_dir() -> Option<String> {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
}

fn main() {
    gpui_platform::application().run(move |cx| {
        // 用任何 gpui-component 功能前必须先初始化。
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| Workspace::new(cx));
                // 顶层视图必须包一层 Root（组件库的主题/遮罩系统要求）。
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("打开窗口失败");
        })
        .detach();
    });
}
