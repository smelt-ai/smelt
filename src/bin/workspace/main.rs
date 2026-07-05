//! smelt 工作台 —— 基于 gpui-component 的桌面窗口。
//!
//! Workspace 管理多个终端标签（TerminalView）：顶部标签栏切换 / 新建 / 关闭，
//! 下方渲染当前活动终端。每个终端各自独立（PTY、IME、滚动、resize）。
//!
//! 运行： cargo run --bin workspace

mod terminal;
mod terminal_view;

use std::collections::HashSet;
use std::path::Path;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
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

/// 主区视图：终端 / 文件树 / Git（按项目切换）。
#[derive(Clone, Copy, PartialEq)]
enum MainView {
    Terminal,
    Files,
    Git,
}

/// 工作台根视图：多标签终端管理器。
struct Workspace {
    tabs: Vec<Entity<TerminalView>>,
    active: usize,
    /// 网格列数：1=单终端，2=两列，3=三列。
    layout_cols: usize,
    /// 主区当前视图：终端 / 文件树 / Git。
    view: MainView,
    /// 文件树里已展开的文件夹绝对路径。
    expanded: HashSet<String>,
    /// 当前在文件树里打开查看的文件 (路径, 内容)。
    open_file: Option<(String, String)>,
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
            view: MainView::Terminal,
            expanded: HashSet::new(),
            open_file: None,
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

    /// 文件树：展开/收起一个文件夹。
    fn toggle_expand(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        cx.notify();
    }

    /// 文件树：打开一个文件查看内容（读文本，最多 3000 行）。
    fn view_file(&mut self, path: String, cx: &mut Context<Self>) {
        let content = std::fs::read_to_string(&path)
            .map(|c| {
                let lines: Vec<&str> = c.lines().take(3000).collect();
                lines.join("\n")
            })
            .unwrap_or_else(|_| "（无法以文本方式读取：可能是二进制文件）".to_string());
        self.open_file = Some((path, content));
        cx.notify();
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

        // 主题色 token（跟随 gpui-component 主题，替代硬编码）
        let (c_bg, c_sidebar, c_sidebar_border, c_border, c_muted, c_accent, c_accent_fg, c_primary, c_popover, c_fg) = {
            let t = cx.theme();
            (
                t.background,
                t.sidebar,
                t.sidebar_border,
                t.border,
                t.muted_foreground,
                t.sidebar_accent,
                t.sidebar_accent_foreground,
                t.primary,
                t.popover,
                t.foreground,
            )
        };

        // 先收集标签标题，释放对 self.tabs 的借用
        let titles: Vec<(usize, String)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, v)| (ix, v.read(cx).title().to_string()))
            .collect();

        // 左侧会话侧栏
        // 按 cwd 把终端分组成项目（保持出现顺序）
        let mut projects: Vec<(String, Vec<usize>)> = Vec::new();
        for (ix, _title) in titles.iter() {
            let cwd = self.tabs[*ix].read(cx).cwd().unwrap_or_default();
            let name = cwd
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("项目")
                .to_string();
            match projects.iter_mut().find(|(n, _)| *n == name) {
                Some(p) => p.1.push(*ix),
                None => projects.push((name, vec![*ix])),
            }
        }

        let sidebar = if self.sidebar_open {
            // 项目头 + 其下终端行
            let mut rows: Vec<AnyElement> = Vec::new();
            for (name, ixs) in &projects {
                rows.push(project_header(name.clone(), cx).into_any_element());
                for &ix in ixs {
                    let title = titles.get(ix).map(|(_, t)| t.clone()).unwrap_or_default();
                    rows.push(
                        sidebar_row(ix, title, ix == active, can_close, cx).into_any_element(),
                    );
                }
            }
            Some(
                div()
                    .flex()
                    .flex_col()
                    .w(px(220.))
                    .h_full()
                    .bg(c_sidebar)
                    .border_r_1()
                    .border_color(c_sidebar_border)
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(c_muted)
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
                            .border_color(c_sidebar_border)
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
                                .border_color(if is_active { c_primary } else { c_border })
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
                                        .bg(if is_active { c_accent } else { c_sidebar })
                                        .text_color(if is_active { c_accent_fg } else { c_muted })
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
                        .text_color(if is_sel { c_accent_fg } else { c_muted })
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            this.exec_cmd(cmd.clone(), window, cx)
                        }))
                        .child(label.clone());
                    if is_sel {
                        d = d.bg(c_accent);
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
                        .bg(c_popover)
                        .border_1()
                        .border_color(c_border)
                        .rounded_lg()
                        .shadow_lg()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .text_color(c_fg)
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
            .bg(c_bg)
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
            // 主区：顶部视图切换 + 内容
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(c_border)
                            .child(view_tab(0, "终端", self.view == MainView::Terminal, MainView::Terminal, cx))
                            .child(view_tab(1, "文件树", self.view == MainView::Files, MainView::Files, cx))
                            .child(view_tab(2, "Git", self.view == MainView::Git, MainView::Git, cx)),
                    )
                    .child(match self.view {
                        MainView::Terminal => content,
                        MainView::Files => {
                            let cwd = self.tabs.get(active).and_then(|t| t.read(cx).cwd());
                            let tree = file_tree(cwd, &self.expanded, cx);
                            let content = file_content_pane(&self.open_file, cx);
                            div()
                                .flex_1()
                                .flex()
                                .child(
                                    div()
                                        .w(px(260.))
                                        .border_r_1()
                                        .border_color(c_border)
                                        .child(tree),
                                )
                                .child(content)
                        }
                        MainView::Git => {
                            let cwd = self.tabs.get(active).and_then(|t| t.read(cx).cwd());
                            git_view(cwd, cx)
                        }
                    }),
            )
            // 命令面板（最上层）
            .children(palette_overlay)
    }
}

/// 顶部视图切换标签。
fn view_tab(
    id: usize,
    label: &str,
    active: bool,
    view: MainView,
    cx: &mut Context<Workspace>,
) -> Stateful<Div> {
    let t = cx.theme();
    let (fg, bg, hover) = if active {
        (t.foreground, t.accent, t.accent)
    } else {
        (t.muted_foreground, t.background, t.accent)
    };
    div()
        .id(("view", id))
        .px_3()
        .py_1()
        .rounded_md()
        .text_sm()
        .bg(bg)
        .text_color(fg)
        .hover(move |s| s.bg(hover))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.view = view;
            cx.notify();
        }))
        .child(label.to_string())
}

/// 主区占位视图（文件树 / Git 尚未实现）。
fn placeholder_view(text: &str, muted: Hsla) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_color(muted)
        .child(text.to_string())
}

/// 侧栏里的项目头（分组标题）。
fn project_header(name: String, cx: &mut Context<Workspace>) -> Div {
    let muted = cx.theme().muted_foreground;
    div()
        .px_2()
        .pt_2()
        .pb_1()
        .text_sm()
        .text_color(muted)
        .child(format!("▾ {name}"))
}

/// 文件树视图：读取项目目录，已展开的文件夹递归显示，点击文件夹展开/收起。
fn file_tree(cwd: Option<String>, expanded: &HashSet<String>, cx: &mut Context<Workspace>) -> Div {
    let (muted, fg, hover) = {
        let t = cx.theme();
        (t.muted_foreground, t.foreground, t.accent)
    };
    let Some(root) = cwd else {
        return placeholder_view("无项目目录", muted);
    };
    let mut flat: Vec<(usize, String, bool, String)> = Vec::new();
    walk_dir(Path::new(&root), expanded, 0, &mut flat);

    let rows: Vec<Stateful<Div>> = flat
        .into_iter()
        .enumerate()
        .map(|(i, (depth, name, is_dir, path))| {
            let indent = px(8.0 + depth as f32 * 14.0);
            let icon = if is_dir {
                if expanded.contains(&path) {
                    "▾"
                } else {
                    "▸"
                }
            } else {
                " "
            };
            let p = path.clone();
            div()
                .id(("file", i))
                .flex()
                .items_center()
                .gap_1()
                .pl(indent)
                .pr_2()
                .py(px(1.0))
                .text_sm()
                .text_color(if is_dir { fg } else { muted })
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    if is_dir {
                        this.toggle_expand(p.clone(), cx);
                    } else {
                        this.view_file(p.clone(), cx);
                    }
                }))
                .child(icon.to_string())
                .child(name)
        })
        .collect();

    div().flex_1().flex().flex_col().py_1().children(rows)
}

/// 递归收集目录条目（仅进入已展开的文件夹），忽略常见重目录。
fn walk_dir(
    root: &Path,
    expanded: &HashSet<String>,
    depth: usize,
    out: &mut Vec<(usize, String, bool, String)>,
) {
    let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(root) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| {
        (
            !e.path().is_dir(),
            e.file_name().to_string_lossy().to_lowercase(),
        )
    });
    for e in entries {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), ".git" | "node_modules" | "target" | ".DS_Store") {
            continue;
        }
        let is_dir = path.is_dir();
        let ps = path.to_string_lossy().to_string();
        out.push((depth, name, is_dir, ps.clone()));
        if is_dir && expanded.contains(&ps) {
            walk_dir(&path, expanded, depth + 1, out);
        }
    }
}

/// Git 视图：显示当前分支 + 改动文件（git status）。
fn git_view(cwd: Option<String>, cx: &mut Context<Workspace>) -> Div {
    let (muted, fg, border) = {
        let t = cx.theme();
        (t.muted_foreground, t.foreground, t.border)
    };
    let Some(root) = cwd else {
        return placeholder_view("无项目目录", muted);
    };
    let output = std::process::Command::new("git")
        .args(["-C", &root, "status", "--porcelain=v1", "-b"])
        .output();
    let text = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return placeholder_view("不是 git 仓库，或 git 不可用", muted),
    };

    let mut branch = String::from("?");
    let mut files: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(b) = line.strip_prefix("## ") {
            branch = b.split("...").next().unwrap_or("").trim().to_string();
        } else if line.len() >= 3 {
            files.push((line[..2].to_string(), line[3..].to_string()));
        }
    }

    let body = if files.is_empty() {
        placeholder_view("工作区干净，无改动 ✓", muted)
    } else {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .p_1()
            .children(files.into_iter().enumerate().map(|(i, (st, path))| {
                let color = git_status_color(&st);
                div()
                    .id(("git", i))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(px(1.0))
                    .text_sm()
                    .child(
                        div()
                            .w(px(22.))
                            .text_color(color)
                            .child(if st.trim().is_empty() {
                                "•".to_string()
                            } else {
                                st.trim().to_string()
                            }),
                    )
                    .child(div().text_color(fg).child(path))
            }))
    };

    div()
        .flex_1()
        .flex()
        .flex_col()
        .child(
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(fg)
                .border_b_1()
                .border_color(border)
                .child(format!("⎇ {branch}")),
        )
        .child(body)
}

/// git 状态码 → 颜色（约定色）。
fn git_status_color(st: &str) -> Rgba {
    if st.contains('?') {
        rgb(0x565f89) // 未跟踪
    } else if st.contains('A') {
        rgb(0x9ece6a) // 新增
    } else if st.contains('D') {
        rgb(0xf7768e) // 删除
    } else if st.contains('M') {
        rgb(0xe0af68) // 修改
    } else {
        rgb(0x7aa2f7)
    }
}

/// 文件内容查看面板：逐行显示选中文件的文本。
fn file_content_pane(open_file: &Option<(String, String)>, cx: &mut Context<Workspace>) -> Div {
    let (muted, fg, border) = {
        let t = cx.theme();
        (t.muted_foreground, t.foreground, t.border)
    };
    match open_file {
        None => placeholder_view("← 从左侧选择文件查看内容", muted),
        Some((path, content)) => {
            let name = path.rsplit('/').next().unwrap_or(path.as_str()).to_string();
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .text_sm()
                        .text_color(muted)
                        .border_b_1()
                        .border_color(border)
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .p_2()
                        .font_family(terminal_view::FONT_FAMILY)
                        .text_sm()
                        .text_color(fg)
                        .children(
                            content
                                .lines()
                                .map(|l| div().whitespace_nowrap().child(l.to_string())),
                        ),
                )
        }
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
    let t = cx.theme();
    let (bg, fg, muted, hover) = if active {
        (
            t.sidebar_accent,
            t.sidebar_accent_foreground,
            t.muted_foreground,
            t.sidebar_accent,
        )
    } else {
        (
            t.sidebar,
            t.sidebar_foreground,
            t.muted_foreground,
            t.sidebar_accent,
        )
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
        .hover(move |s| s.bg(hover))
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
                .text_color(muted)
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.close_tab(ix, cx);
                }))
                .child("×"),
        );
    }

    row
}

/// 「+」新建终端按钮（继承当前项目目录）。
fn new_tab_button(cx: &mut Context<Workspace>) -> Button {
    Button::new("new-tab")
        .ghost()
        .label("+")
        .tooltip("新建终端")
        .on_click(cx.listener(|this, _ev, _window, cx| {
            this.new_tab(cx);
        }))
}

/// 布局切换按钮：显示当前列数图标，点击循环 1/2/3 列。
fn layout_button(cols: usize, cx: &mut Context<Workspace>) -> Button {
    let icon = match cols {
        1 => "▢",
        2 => "▥",
        _ => "▦",
    };
    Button::new("layout")
        .ghost()
        .label(icon)
        .tooltip("切换布局")
        .on_click(cx.listener(|this, _ev, _window, cx| this.cycle_layout(cx)))
}

/// 「打开项目」按钮：弹选择框选目录，在其中开新标签。
fn open_project_button(cx: &mut Context<Workspace>) -> Button {
    Button::new("open-project")
        .ghost()
        .label("📂")
        .tooltip("打开项目")
        .on_click(cx.listener(|this, _ev, _window, cx| {
            this.open_project(cx);
        }))
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
        // 深色主题（与终端配色一致）
        Theme::change(ThemeMode::Dark, None, cx);

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
