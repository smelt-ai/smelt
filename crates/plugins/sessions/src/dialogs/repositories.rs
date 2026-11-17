//! 项目的仓库对话框：看项目里纳入了哪些仓库，纳入已有的、从 Git URL 克隆、取消关联，
//! 以及挑几个仓库建一个工作树（多仓库工作集）。
//!
//! 文件系统的活都在项目所在那台机器的运行时上做；这里只发调用，拿回整份目录更新列表。
//! 写入（克隆、建工作树）可能要几分钟，跑着的时候框关不掉，也不接新的操作。

use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px, rgb,
};
use gpui_base::input::{InputEvent, InputState};
use smelt_plugin_api::model::control_api::{
    WorkspaceProjectRepositoryAction as RepositoryAction, WorkspaceWorksetCreate,
};
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceDirectory, WorkspaceProjectRepository, WorkspaceProjectRepositoryAction as Action,
    WorkspaceProjectRepositoryActionParams, WorkspaceWorkset, WorkspaceWorksetCreateParams,
};
use smelt_plugin_api::ui::dialog;
use smelt_plugin_api::ui::{Button, Checkbox, TextInput, ui_theme};
use smelt_plugin_api::{self as api, RuntimeKey};

/// 克隆、建工作树会跑 git 和初始化脚本，大仓库要很久。
const WRITE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// 卡片宽度。
const WIDTH: f32 = 620.;

/// 工作树建好了，参数是任务名。新建会话的框据此等目录推到后选上它。
pub(super) type Created = Rc<dyn Fn(String, &mut App)>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// 列出仓库，纳入、移除。
    Manage,
    /// 建工作树。
    Create,
    /// 纳入已有的仓库。
    Bind,
    /// 从 Git URL 克隆。
    Clone,
}

/// 打开对话框，叠在已开的上面（从新建会话的框里打开时，关了回到那里）。
pub(super) fn open(
    machine: RuntimeKey,
    root: String,
    mode: Mode,
    created: Option<Created>,
    window: &mut Window,
    cx: &mut App,
) {
    let window_handle = window.window_handle();
    let state =
        cx.new(|cx| Repositories::new(machine, root, mode, created, window_handle, window, cx));
    dialog::open(window, cx, move |card, _, cx| {
        let busy = state.read(cx).busy;
        let submit = state.clone();
        let closed = state.clone();
        card.w(px(WIDTH))
            .close_on_backdrop(false)
            .child(state.clone())
            .footer(footer(&state, cx))
            .on_ok(move |window, cx| submit.update(cx, |state, cx| state.submit(window, cx)))
            // 写入跑着时不能关：Esc 不理。
            .on_cancel(move |_, _| !busy)
            .on_close(move |_, cx| closed.update(cx, |state, _| state.open = false))
    });
}

struct Repositories {
    machine: RuntimeKey,
    project_root: String,
    mode: Mode,
    /// 打开时的样子：从这里进的建工作树没有「返回」。
    entry: Mode,
    /// 纳入、克隆做完（或点「返回」）回到哪。
    return_mode: Mode,
    repositories: Vec<WorkspaceProjectRepository>,
    worksets: Vec<WorkspaceWorkset>,
    /// 建工作树勾选的仓库名。
    selected: BTreeSet<String>,
    /// 纳入时选好的目录。
    source: Option<String>,
    task: Entity<InputState>,
    branch: Entity<InputState>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    /// 等人确认取消关联的仓库名。
    pending_remove: Option<String>,
    created: Option<Created>,
    /// 框所在的窗口：建好工作树后从后台回来关框。
    window: AnyWindowHandle,
    /// 框还开着。关了以后回来的结果不再去关框（那时最上面可能是别的框）。
    open: bool,
    _subscriptions: Vec<Subscription>,
}

impl Repositories {
    fn new(
        machine: RuntimeKey,
        project_root: String,
        mode: Mode,
        created: Option<Created>,
        window_handle: AnyWindowHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let task = input("例如 add-search 或 搜索功能", window, cx);
        let branch = input("留空使用默认分支，或填写完整分支名", window, cx);
        let name = input("留空使用下方建议名称", window, cx);
        let url = input("HTTPS 或 SSH Git URL", window, cx);
        let subscriptions = [&task, &branch, &name, &url]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::Change => cx.notify(),
                        // 在输入框里回车等于点主按钮。
                        InputEvent::PressEnter { .. } => {
                            this.submit(window, cx);
                        }
                        _ => {}
                    }
                })
            })
            .collect();
        // 先拿目录里现有的一份垫着，探查回来再换。
        let directory = api::host(cx).workspace_directory(&machine, cx);
        let repositories: Vec<_> = directory
            .iter()
            .flat_map(|directory| &directory.repositories)
            .filter(|repo| repo.project_root == project_root)
            .cloned()
            .collect();
        let selected = repositories.iter().map(|repo| repo.name.clone()).collect();
        let worksets = directory
            .iter()
            .flat_map(|directory| &directory.worksets)
            .filter(|workset| workset.project_root == project_root)
            .cloned()
            .collect();
        let mut this = Self {
            machine,
            project_root,
            mode,
            entry: mode,
            return_mode: mode,
            repositories,
            worksets,
            selected,
            source: None,
            task,
            branch,
            name,
            url,
            busy: false,
            error: None,
            pending_remove: None,
            created,
            window: window_handle,
            open: true,
            _subscriptions: subscriptions,
        };
        this.repository_action(Action::Discover, mode, cx);
        this
    }

    fn repository_name(&self, cx: &App) -> String {
        let name = self.name.read(cx).value().trim().to_string();
        if !name.is_empty() {
            return name;
        }
        let source = if self.mode == Mode::Clone {
            self.url.read(cx).value().to_string()
        } else {
            self.source.clone().unwrap_or_default()
        };
        suggested_name(&source)
    }

    /// 「返回」回到哪；没有就不显示。
    fn back(&self) -> Option<Mode> {
        match self.mode {
            Mode::Manage => None,
            Mode::Create => (self.entry == Mode::Manage).then_some(Mode::Manage),
            Mode::Bind | Mode::Clone => Some(self.return_mode),
        }
    }

    fn switch(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if matches!(mode, Mode::Bind | Mode::Clone) {
            self.return_mode = self.mode;
            self.source = None;
            self.name
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.url
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.mode = mode;
        self.error = None;
        self.pending_remove = None;
        cx.notify();
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(mode) = self.back() {
            self.mode = mode;
            self.error = None;
            cx.notify();
        }
    }

    /// 在那台机器上选一个已有仓库的根目录。
    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let picked = api::PICK_PATHS.run(
            &api::PathRequest {
                machine: self.machine.clone(),
                title: "选择已有 Git 仓库".into(),
                confirm: "选择".into(),
                files: false,
                multiple: false,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            let _ = this.update(cx, |this, cx| {
                if this.busy || this.mode != Mode::Bind {
                    return;
                }
                match result {
                    Ok(Some(paths)) => {
                        if let Some(source) = paths.into_iter().next() {
                            this.source = Some(source);
                            this.error = None;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn repository_action(&mut self, action: Action, next_mode: Mode, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let first = matches!(action, Action::Discover);
        let params = WorkspaceProjectRepositoryActionParams {
            project_root: self.project_root.clone(),
            action,
        };
        let ran = api::call::<RepositoryAction>(&self.machine, params, WRITE_TIMEOUT, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = ran.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(directory) => {
                        this.update_catalog(directory, first);
                        this.mode = next_mode;
                        this.pending_remove = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 主按钮、回车。返回框能不能当场关（只有「完成」能）。
    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        let action = match self.mode {
            Mode::Manage => return true,
            Mode::Create => {
                self.create_workset(cx);
                return false;
            }
            Mode::Bind => {
                let Some(source) = self.source.clone() else {
                    self.error = Some("请先选择已有 Git 仓库的根目录".into());
                    cx.notify();
                    return false;
                };
                Action::Bind {
                    source,
                    name: self.repository_name(cx),
                }
            }
            Mode::Clone => Action::Clone {
                url: self.url.read(cx).value().to_string(),
                name: self.repository_name(cx),
            },
        };
        self.repository_action(action, self.return_mode, cx);
        false
    }

    fn create_workset(&mut self, cx: &mut Context<Self>) {
        let task = self.task.read(cx).value().trim().to_string();
        if !valid_task(&task) || self.selected.is_empty() {
            self.error = Some("请填写不含路径分隔符的任务名，并至少选择一个仓库".into());
            cx.notify();
            return;
        }
        let branch = self.branch.read(cx).value().trim().to_string();
        let params = WorkspaceWorksetCreateParams {
            project_root: self.project_root.clone(),
            task: task.clone(),
            branch_prefix: "feat".into(),
            branch: (!branch.is_empty()).then_some(branch),
            repository_names: self.selected.iter().cloned().collect(),
        };
        let ran = api::call::<WorkspaceWorksetCreate>(&self.machine, params, WRITE_TIMEOUT, cx);
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = ran.await;
            // 工作树已经建好，初始化脚本失败不算新建失败：框照常关，失败单独报。
            if let Ok(created) = &result {
                for setup in created.setup.iter().filter(|setup| !setup.success) {
                    let message = format!(
                        "工作树已建好，但仓库 {} 的初始化脚本（.smelt/worktree-setup）失败：\n{}",
                        setup.repository, setup.output
                    );
                    cx.update(|cx| crate::notify_error(&message, cx));
                }
            }
            let Ok(Some((close, created))) = this.update(cx, |this, cx| {
                this.busy = false;
                cx.notify();
                match result {
                    Ok(_) => Some((this.open.then_some(this.window), this.created.clone())),
                    Err(error) => {
                        this.error = Some(error);
                        None
                    }
                }
            }) else {
                return;
            };
            cx.update(|cx| {
                if let Some(created) = created {
                    created(task, cx);
                }
                if let Some(window) = close {
                    let _ = window.update(cx, |_, window, cx| dialog::close(window, cx));
                }
            });
        })
        .detach();
    }

    fn update_catalog(&mut self, directory: WorkspaceDirectory, first: bool) {
        let old: BTreeSet<_> = self
            .repositories
            .iter()
            .map(|repo| repo.name.clone())
            .collect();
        self.repositories = directory
            .repositories
            .into_iter()
            .filter(|repo| repo.project_root == self.project_root)
            .collect();
        let names: BTreeSet<_> = self
            .repositories
            .iter()
            .map(|repo| repo.name.clone())
            .collect();
        self.selected.retain(|name| names.contains(name));
        // 第一次探查全勾上；之后只勾新纳入的，留着人取消掉的。
        for name in names {
            if first || !old.contains(&name) {
                self.selected.insert(name);
            }
        }
        self.worksets = directory
            .worksets
            .into_iter()
            .filter(|workset| workset.project_root == self.project_root)
            .collect();
    }
}

/// 任务名是工作树目录名：不能空、不能是 `.` `..`，不含路径分隔符和控制字符。
fn valid_task(task: &str) -> bool {
    !task.is_empty()
        && task != "."
        && task != ".."
        && !task.contains(['/', '\\'])
        && !task.chars().any(char::is_control)
}

/// 从 Git URL 或目录推一个仓库名：最后一段，去掉 `.git`。URL 只看路径部分，免得把带在
/// 主机名里的凭据当成名字。
fn suggested_name(source: &str) -> String {
    let source = source.trim();
    let source = if let Some((_, rest)) = source.split_once("://") {
        let Some((_, path)) = rest.split_once('/') else {
            return String::new();
        };
        path.split(['?', '#']).next().unwrap_or_default()
    } else {
        source
    };
    source
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_string()
}

impl Render for Repositories {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let mode = self.mode;
        let title = match mode {
            Mode::Manage => "管理项目仓库",
            Mode::Create => "新建工作树",
            Mode::Bind => "添加已有仓库",
            Mode::Clone => "从 Git URL 克隆",
        };
        let task = self.task.read(cx).value().trim().to_string();
        let task_or_placeholder = if task.is_empty() {
            "<任务名>".to_string()
        } else {
            task
        };
        let mut content = dialog::content(title, Some(self.project_root.clone().into()));
        if mode == Mode::Create {
            content = content
                .child(field_label("任务名"))
                .child(TextInput::new(&self.task).disabled(busy))
                .child(hint("支持中文；建议使用与分支名接近的英文名，不含路径分隔符。"))
                .child(field_label("分支名"))
                .child(TextInput::new(&self.branch).disabled(busy))
                .child(hint(format!(
                    "留空使用 feat/{task_or_placeholder}；也可输入完整分支名（如 fix/search）。分支名不能包含空格。"
                )));
        }
        if matches!(mode, Mode::Manage | Mode::Create) {
            content = content
                .child(self.render_list(&task_or_placeholder, cx))
                .child(self.render_entries(cx));
        } else {
            content = self.render_form(content, cx);
        }
        if let Some(name) = self.pending_remove.clone() {
            content = content.child(self.render_remove(name, cx));
        }
        content.when_some(self.error.clone(), |content, error| {
            content.child(
                div()
                    .text_sm()
                    .text_color(rgb(ui_theme::bad()))
                    .child(error),
            )
        })
    }
}

fn field_label(text: &'static str) -> impl IntoElement {
    div().text_sm().child(text)
}

fn hint(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(rgb(ui_theme::muted()))
        .child(text.into())
}

impl Repositories {
    fn render_list(&self, task: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let mode = self.mode;
        let mut list = div()
            .id("project-repositories-list")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(260.))
            .overflow_y_scroll();
        if self.repositories.is_empty() {
            list = list.child(
                div()
                    .py_4()
                    .text_sm()
                    .text_color(rgb(ui_theme::muted()))
                    .child(if busy {
                        "正在读取项目仓库…"
                    } else {
                        "还没有项目仓库。选择已有仓库，或从 Git URL 克隆。"
                    }),
            );
        }
        for repo in &self.repositories {
            let name = repo.name.clone();
            let referenced = self
                .worksets
                .iter()
                .filter(|workset| {
                    workset
                        .repositories
                        .iter()
                        .any(|r| r.name == repo.name || r.root == repo.root)
                })
                .map(|workset| workset.task.as_str())
                .collect::<Vec<_>>()
                .join("、");
            let mut row = div()
                .flex()
                .items_center()
                .gap_3()
                .py_2()
                .px_2()
                .border_1()
                .border_color(ui_theme::hair())
                .rounded(ui_theme::row_radius());
            if mode == Mode::Create {
                let toggle = name.clone();
                let state = cx.entity().downgrade();
                row = row.child(
                    Checkbox::new(SharedString::from(format!("repo-check-{name}")))
                        .checked(self.selected.contains(&name))
                        .disabled(busy)
                        .accessibility_label(name.clone())
                        .on_change(move |checked, _, cx| {
                            let _ = state.update(cx, |this, cx| {
                                if this.busy {
                                    return;
                                }
                                if checked {
                                    this.selected.insert(toggle.clone());
                                } else {
                                    this.selected.remove(&toggle);
                                }
                                cx.notify();
                            });
                        }),
                );
            }
            let location = repo
                .managed_link
                .as_ref()
                .map(|link| format!("{link} → {}", repo.root))
                .unwrap_or_else(|| repo.root.clone());
            let source_label = if repo.managed_link.is_some() {
                "外部仓库 · 软链接"
            } else if repo.root == self.project_root {
                "项目根仓库"
            } else {
                "已有仓库"
            };
            let mut detail = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .child(format!("{} · {source_label}", repo.name)),
                )
                .child(hint(location));
            if mode == Mode::Create {
                detail = detail.child(hint(format!(
                    "{}/worktrees/{task}/{}",
                    self.project_root, repo.name
                )));
            }
            row = row.child(detail);
            if mode == Mode::Manage {
                row = row.child(
                    Button::new(SharedString::from(format!("repo-remove-{name}")))
                        .quiet()
                        .small()
                        .label("移除")
                        .disabled(busy || !referenced.is_empty())
                        .tooltip(if referenced.is_empty() {
                            "仅取消关联，不删除真实仓库".to_string()
                        } else {
                            format!("工作树仍在使用：{referenced}")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.busy {
                                this.pending_remove = Some(name.clone());
                                cx.notify();
                            }
                        })),
                );
            }
            list = list.child(row);
        }
        list
    }

    /// 列表下面的入口：纳入已有的、克隆，管理时还有建工作树。
    fn render_entries(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        div()
            .flex()
            .gap_2()
            .child(
                Button::new("repo-bind-entry")
                    .label("选择已有仓库…")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.switch(Mode::Bind, window, cx)),
                    ),
            )
            .child(
                Button::new("repo-clone-entry")
                    .label("从 Git URL 克隆…")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.switch(Mode::Clone, window, cx)),
                    ),
            )
            .when(self.mode == Mode::Manage, |row| {
                row.child(
                    Button::new("repo-create-entry")
                        .label("新建工作树…")
                        .disabled(busy || self.repositories.is_empty())
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.switch(Mode::Create, window, cx)
                            }),
                        ),
                )
            })
    }

    fn render_form(&self, content: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let busy = self.busy;
        let content = if self.mode == Mode::Bind {
            content
                .child(div().text_sm().text_color(rgb(ui_theme::muted())).child(
                    "选择已有 Git 仓库的根目录。项目外的仓库只在 repos/ 下建软链接，原目录不会移动。",
                ))
                .child(
                    div().flex().child(
                        Button::new("repo-choose-folder")
                            .label("选择文件夹…")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                    ),
                )
                .child(
                    div()
                        .text_sm()
                        .child(self.source.clone().unwrap_or_else(|| "尚未选择目录".into())),
                )
        } else {
            content
                .child(field_label("Git URL"))
                .child(TextInput::new(&self.url).disabled(busy))
                .child(hint(
                    "使用运行时机器上已有的 Git/SSH 凭据；克隆可能需要几分钟。",
                ))
        };
        let name = self.repository_name(cx);
        let destination = if self.mode == Mode::Clone {
            format!("将克隆到：{}/repos/{name}", self.project_root)
        } else {
            format!(
                "项目内直接绑定；项目外链接到：{}/repos/{name}",
                self.project_root
            )
        };
        content
            .child(field_label("仓库名称（可修改）"))
            .child(TextInput::new(&self.name).disabled(busy))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(ui_theme::muted()))
                    .child(format!(
                        "名称：{} · {destination}",
                        if name.is_empty() {
                            "<待填写>"
                        } else {
                            name.as_str()
                        },
                    )),
            )
    }

    fn render_remove(&self, name: String, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().child(format!(
                "取消关联「{name}」？真实仓库和分支会保留；仅清理 Smelt 自建的软链接。"
            )))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("repo-remove-cancel")
                            .quiet()
                            .label("保留关联")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.busy {
                                    this.pending_remove = None;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("repo-remove-confirm")
                            .danger()
                            .label("确认移除")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.repository_action(
                                    Action::Remove { name: name.clone() },
                                    Mode::Manage,
                                    cx,
                                );
                            })),
                    ),
            )
    }
}

fn footer(state: &Entity<Repositories>, cx: &App) -> impl IntoElement {
    let this = state.read(cx);
    let busy = this.busy;
    let mode = this.mode;
    let label = if busy {
        "正在处理…"
    } else {
        match mode {
            Mode::Manage => "完成",
            Mode::Create => "创建工作树",
            Mode::Bind => "添加仓库",
            Mode::Clone => "克隆并添加",
        }
    };
    let submit_disabled = busy || (mode == Mode::Create && this.repositories.is_empty());
    let back = state.clone();
    let submit = state.clone();
    dialog::footer()
        .when(this.back().is_some(), |row| {
            row.child(
                Button::new("repo-form-back")
                    .quiet()
                    .small()
                    .label("返回")
                    .disabled(busy)
                    .on_click(move |_, _, cx| back.update(cx, |state, cx| state.go_back(cx))),
            )
        })
        .when(mode != Mode::Manage, |row| {
            row.child(
                Button::new("repo-modal-close")
                    .quiet()
                    .small()
                    .label("关闭")
                    .disabled(busy)
                    .on_click(|_, window, cx| dialog::close(window, cx)),
            )
        })
        .child(
            Button::new("repo-modal-submit")
                .primary()
                .small()
                .label(label)
                .loading(busy)
                .disabled(submit_disabled)
                .on_click(move |_, window, cx| {
                    if submit.update(cx, |state, cx| state.submit(window, cx)) {
                        dialog::close(window, cx);
                    }
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_names_for_git_urls_and_selected_directories() {
        assert_eq!(suggested_name("git@example.com:team/api.git"), "api");
        assert_eq!(suggested_name("https://example.com/team/web.git"), "web");
        assert_eq!(suggested_name("/Users/someone/my repo/"), "my repo");
        assert_eq!(suggested_name("https://token@example.com"), "");
        assert_eq!(
            suggested_name("https://token@example.com/team/api.git?token=private"),
            "api"
        );
    }

    #[test]
    fn task_names_are_single_directory_names() {
        assert!(valid_task("add-search"));
        assert!(valid_task("搜索功能"));
        for bad in ["", ".", "..", "a/b", "a\\b", "a\nb"] {
            assert!(!valid_task(bad), "{bad:?}");
        }
    }
}
