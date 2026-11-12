//! Project repository forms. Filesystem work belongs to the selected runtime.

use std::collections::BTreeSet;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{
    input::{Input, InputEvent, InputState},
    *,
};
use smelt_model::workspace_directory::{
    WorkspaceDirectory, WorkspaceProjectRepository, WorkspaceProjectRepositoryAction as Action,
    WorkspaceProjectRepositoryActionParams, WorkspaceWorkset, WorkspaceWorksetCreateParams,
};

use crate::{Workspace, runtimes::Runtime, ui_theme};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Manage,
    Create,
    Bind,
    Clone,
}

pub(crate) struct ProjectRepositoriesState {
    id: u64,
    runtime: Runtime,
    project_root: String,
    mode: Mode,
    return_mode: Mode,
    repositories: Vec<WorkspaceProjectRepository>,
    worksets: Vec<WorkspaceWorkset>,
    selected: BTreeSet<String>,
    source: Option<String>,
    task: Entity<InputState>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    pending_remove: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ProjectRepositoriesState {
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
}

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

impl Workspace {
    pub(crate) fn open_project_repositories(
        &mut self,
        runtime: Runtime,
        project_root: String,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .project_repositories
            .as_ref()
            .is_some_and(|state| state.busy)
        {
            return;
        }
        self.begin_main_modal_focus(window, cx);
        let task = cx.new(|cx| InputState::new(window, cx).placeholder("例如 add-search，不含 /"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("留空使用下方建议名称"));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("HTTPS 或 SSH Git URL"));
        let subscriptions = [&task, &name, &url]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        let mode = if create { Mode::Create } else { Mode::Manage };
        let repositories = self
            .space(runtime.key())
            .into_iter()
            .flat_map(|space| &space.repositories)
            .filter(|repo| repo.project_root == project_root)
            .cloned()
            .collect::<Vec<_>>();
        let selected = repositories.iter().map(|repo| repo.name.clone()).collect();
        let worksets = self
            .space(runtime.key())
            .into_iter()
            .flat_map(|space| &space.worksets)
            .filter(|workset| workset.project_root == project_root)
            .cloned()
            .collect();
        self.project_repositories = Some(ProjectRepositoriesState {
            id: crate::next_session_ui_id(),
            runtime,
            project_root,
            mode,
            return_mode: mode,
            repositories,
            worksets,
            selected,
            source: None,
            task,
            name,
            url,
            busy: false,
            error: None,
            pending_remove: None,
            _subscriptions: subscriptions,
        });
        self.project_repository_action(Action::Discover, mode, cx);
    }

    pub(crate) fn close_project_repositories(&mut self, cx: &mut Context<Self>) {
        if self
            .project_repositories
            .as_ref()
            .is_none_or(|state| !state.busy)
        {
            self.project_repositories = None;
            cx.notify();
        }
    }

    pub(crate) fn project_repository_chosen(
        &mut self,
        id: u64,
        source: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self
            .project_repositories
            .as_mut()
            .filter(|state| state.id == id && !state.busy)
        {
            state.source = Some(source);
            state.error = None;
            cx.notify();
        }
    }

    fn project_repository_action(
        &mut self,
        action: Action,
        next_mode: Mode,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.project_repositories.as_mut() else {
            return;
        };
        if state.busy {
            return;
        }
        state.busy = true;
        state.error = None;
        let id = state.id;
        let first = matches!(&action, Action::Discover);
        let runtime = state.runtime.clone();
        let params = WorkspaceProjectRepositoryActionParams {
            project_root: state.project_root.clone(),
            action,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call_on::<
                        smelt_model::control_api::WorkspaceProjectRepositoryAction,
                    >(
                        runtime.client(),
                        params,
                        std::time::Duration::from_secs(30 * 60),
                    )
                    .map_err(crate::runtime_call::error_text)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this
                    .project_repositories
                    .as_mut()
                    .filter(|state| state.id == id)
                else {
                    return;
                };
                state.busy = false;
                match result {
                    Ok(directory) => {
                        update_catalog(state, directory, first);
                        state.mode = next_mode;
                        state.pending_remove = None;
                    }
                    Err(error) => state.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_project_repositories(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.project_repositories.as_ref() else {
            return;
        };
        if state.busy {
            return;
        }
        let next_mode = state.return_mode;
        let action = match state.mode {
            Mode::Manage => {
                self.close_project_repositories(cx);
                return;
            }
            Mode::Bind => {
                let Some(source) = state.source.clone() else {
                    self.project_repositories.as_mut().unwrap().error =
                        Some("请先选择已有 Git 仓库的根目录".into());
                    cx.notify();
                    return;
                };
                Action::Bind {
                    source,
                    name: state.repository_name(cx),
                }
            }
            Mode::Clone => Action::Clone {
                url: state.url.read(cx).value().to_string(),
                name: state.repository_name(cx),
            },
            Mode::Create => {
                self.submit_workset_form(cx);
                return;
            }
        };
        self.project_repository_action(action, next_mode, cx);
    }

    fn submit_workset_form(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.project_repositories.as_mut() else {
            return;
        };
        if state.busy {
            return;
        }
        let task = state.task.read(cx).value().trim().to_string();
        if task.is_empty()
            || task == "."
            || task == ".."
            || task.contains(['/', '\\'])
            || task.chars().any(char::is_control)
            || state.selected.is_empty()
        {
            state.error = Some("请填写不含路径分隔符的任务名，并至少选择一个仓库".into());
            cx.notify();
            return;
        }
        let params = WorkspaceWorksetCreateParams {
            project_root: state.project_root.clone(),
            task,
            branch_prefix: "feat".into(),
            repository_names: state.selected.iter().cloned().collect(),
        };
        let id = state.id;
        let runtime = state.runtime.clone();
        state.busy = true;
        state.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call_on::<
                            smelt_model::control_api::WorkspaceWorksetCreate,
                        >(
                            runtime.client(),
                            params,
                            std::time::Duration::from_secs(30 * 60),
                        )
                        .map_err(crate::runtime_call::error_text)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this
                    .project_repositories
                    .as_mut()
                    .filter(|state| state.id == id)
                else {
                    return;
                };
                match result {
                    Ok(_) => this.project_repositories = None,
                    Err(error) => {
                        state.busy = false;
                        state.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_project_repositories(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = self.project_repositories.as_ref() else {
            return div().into_any_element();
        };
        let busy = state.busy;
        let mode = state.mode;
        let muted = cx.theme().muted_foreground;
        let title = match mode {
            Mode::Manage => "管理项目仓库",
            Mode::Create => "新建工作树",
            Mode::Bind => "添加已有仓库",
            Mode::Clone => "从 Git URL 克隆",
        };
        let mut content = v_flex()
            .child(Self::modal_title(cx.theme().foreground, title))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(state.project_root.clone()),
            );
        if mode == Mode::Create {
            let task = state.task.read(cx).value().trim().to_string();
            content = content
                .child(div().text_sm().child("任务名"))
                .child(Input::new(&state.task).disabled(busy))
                .child(div().text_sm().text_color(muted).child(format!(
                    "分支：feat/{} · 在 worktrees/<任务>/<仓库> 创建独立工作目录",
                    if task.is_empty() {
                        "<任务名>"
                    } else {
                        &task
                    },
                )));
        }
        if matches!(mode, Mode::Manage | Mode::Create) {
            let mut list = v_flex()
                .id("project-repositories-list")
                .gap_2()
                .max_h(px(260.))
                .overflow_y_scroll();
            if state.repositories.is_empty() {
                list = list.child(div().py_4().text_sm().text_color(muted).child(if busy {
                    "正在读取项目仓库…"
                } else {
                    "还没有项目仓库。选择已有仓库，或从 Git URL 克隆。"
                }));
            }
            for repo in &state.repositories {
                let name = repo.name.clone();
                let referenced = state
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
                let mut row = h_flex()
                    .items_center()
                    .gap_3()
                    .py_2()
                    .px_2()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(ui_theme::row_radius());
                if mode == Mode::Create {
                    row = row.child(
                        checkbox::Checkbox::new(SharedString::from(format!("repo-check-{name}")))
                            .checked(state.selected.contains(&name))
                            .disabled(busy)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(state) = this
                                    .project_repositories
                                    .as_mut()
                                    .filter(|state| !state.busy)
                                {
                                    if *checked {
                                        state.selected.insert(name.clone());
                                    } else {
                                        state.selected.remove(&name);
                                    }
                                    cx.notify();
                                }
                            })),
                    );
                }
                let location = repo
                    .managed_link
                    .as_ref()
                    .map(|link| format!("{link} → {}", repo.root))
                    .unwrap_or_else(|| repo.root.clone());
                let source_label = if repo.managed_link.is_some() {
                    "外部仓库 · 软链接"
                } else if repo.root == state.project_root {
                    "项目根仓库"
                } else {
                    "已有仓库"
                };
                let mut detail = v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .child(format!("{} · {source_label}", repo.name)),
                    )
                    .child(div().text_xs().text_color(muted).child(location));
                if mode == Mode::Create {
                    let task = state.task.read(cx).value().trim().to_string();
                    detail = detail.child(div().text_xs().text_color(muted).child(format!(
                        "{}/worktrees/{}/{}",
                        state.project_root,
                        if task.is_empty() {
                            "<任务名>"
                        } else {
                            &task
                        },
                        repo.name,
                    )));
                }
                row = row.child(detail);
                if mode == Mode::Manage {
                    let name = repo.name.clone();
                    row = row.child(
                        smelt_ui::Button::new(SharedString::from(format!("repo-remove-{name}")))
                            .quiet()
                            .compact()
                            .label("移除")
                            .disabled(busy || !referenced.is_empty())
                            .tooltip(if referenced.is_empty() {
                                "仅取消关联，不删除真实仓库".to_string()
                            } else {
                                format!("工作树仍在使用：{referenced}")
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(state) = this
                                    .project_repositories
                                    .as_mut()
                                    .filter(|state| !state.busy)
                                {
                                    state.pending_remove = Some(name.clone());
                                    cx.notify();
                                }
                            })),
                    );
                }
                list = list.child(row);
            }
            content = content.child(list).child(
                h_flex()
                    .gap_2()
                    .child(
                        Self::modal_normal_button(
                            "repo-bind-entry",
                            "选择已有仓库…",
                            |this, _, window, cx| {
                                if let Some(state) = this
                                    .project_repositories
                                    .as_mut()
                                    .filter(|state| !state.busy)
                                {
                                    state.return_mode = state.mode;
                                    state.mode = Mode::Bind;
                                    state.source = None;
                                    state
                                        .name
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                    state.error = None;
                                    cx.notify();
                                }
                            },
                            cx,
                        )
                        .disabled(busy),
                    )
                    .child(
                        Self::modal_normal_button(
                            "repo-clone-entry",
                            "从 Git URL 克隆…",
                            |this, _, window, cx| {
                                if let Some(state) = this
                                    .project_repositories
                                    .as_mut()
                                    .filter(|state| !state.busy)
                                {
                                    state.return_mode = state.mode;
                                    state.mode = Mode::Clone;
                                    state
                                        .name
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                    state
                                        .url
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                    state.error = None;
                                    cx.notify();
                                }
                            },
                            cx,
                        )
                        .disabled(busy),
                    ),
            );
        } else {
            if mode == Mode::Bind {
                content = content.child(div().text_sm().text_color(muted).child(
                    "选择已有 Git 仓库的根目录。项目外的仓库只在 repos/ 下建软链接，原目录不会移动。",
                )).child(Self::modal_normal_button("repo-choose-folder", "选择文件夹…", |this, _, _, cx| {
                    let Some(state) = this.project_repositories.as_ref().filter(|state| !state.busy) else { return };
                    this.open_dir_picker(state.runtime.clone(), crate::dir_picker::DirPickerPurpose::ProjectRepository { id: state.id }, cx);
                }, cx).disabled(busy)).child(div().text_sm().child(state.source.clone().unwrap_or_else(|| "尚未选择目录".into())));
            } else {
                content = content
                    .child(div().text_sm().child("Git URL"))
                    .child(Input::new(&state.url).disabled(busy))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("使用运行时机器上已有的 Git/SSH 凭据；克隆可能需要几分钟。"),
                    );
            }
            let name = state.repository_name(cx);
            let destination_hint = if mode == Mode::Clone {
                format!("将克隆到：{}/repos/{name}", state.project_root)
            } else {
                format!(
                    "项目内直接绑定；项目外链接到：{}/repos/{name}",
                    state.project_root
                )
            };
            content = content
                .child(div().text_sm().child("仓库名称（可修改）"))
                .child(Input::new(&state.name).disabled(busy))
                .child(div().text_sm().text_color(muted).child(format!(
                    "名称：{} · {destination_hint}",
                    if name.is_empty() {
                        "<待填写>"
                    } else {
                        &name
                    },
                )));
        }
        if let Some(name) = &state.pending_remove {
            let name = name.clone();
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(format!(
                        "取消关联「{name}」？真实仓库和分支会保留；仅清理 Smelt 自建的软链接。"
                    )))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Self::modal_normal_button(
                                    "repo-remove-cancel",
                                    "保留关联",
                                    |this, _, _, cx| {
                                        if let Some(state) = this
                                            .project_repositories
                                            .as_mut()
                                            .filter(|state| !state.busy)
                                        {
                                            state.pending_remove = None;
                                            cx.notify();
                                        }
                                    },
                                    cx,
                                )
                                .disabled(busy),
                            )
                            .child(
                                Self::modal_danger_button(
                                    "repo-remove-confirm",
                                    "确认移除",
                                    move |this, _, _, cx| {
                                        this.project_repository_action(
                                            Action::Remove { name: name.clone() },
                                            Mode::Manage,
                                            cx,
                                        );
                                    },
                                    cx,
                                )
                                .disabled(busy),
                            ),
                    ),
            );
        }
        if let Some(error) = &state.error {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
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
        content = content.child(
            h_flex()
                .justify_end()
                .gap_2()
                .when(matches!(mode, Mode::Bind | Mode::Clone), |row| {
                    row.child(
                        Self::modal_normal_button(
                            "repo-form-back",
                            "返回",
                            |this, _, _, cx| {
                                if let Some(state) = this
                                    .project_repositories
                                    .as_mut()
                                    .filter(|state| !state.busy)
                                {
                                    state.mode = state.return_mode;
                                    state.error = None;
                                    cx.notify();
                                }
                            },
                            cx,
                        )
                        .disabled(busy),
                    )
                })
                .child(
                    Self::modal_normal_button(
                        "repo-modal-close",
                        "关闭",
                        |this, _, _, cx| this.close_project_repositories(cx),
                        cx,
                    )
                    .disabled(busy),
                )
                .child(
                    Self::modal_commit_button(
                        "repo-modal-submit",
                        label,
                        |this, _, _, cx| this.submit_project_repositories(cx),
                        cx,
                    )
                    .disabled(busy || (mode == Mode::Create && state.repositories.is_empty())),
                ),
        );
        self.modal_shell(
            620.,
            false,
            content,
            crate::overlay::ModalLayer::ProjectRepositories,
            cx,
        )
    }
}

fn update_catalog(
    state: &mut ProjectRepositoriesState,
    directory: WorkspaceDirectory,
    first: bool,
) {
    let old = state
        .repositories
        .iter()
        .map(|repo| repo.name.clone())
        .collect::<BTreeSet<_>>();
    state.repositories = directory
        .repositories
        .into_iter()
        .filter(|repo| repo.project_root == state.project_root)
        .collect();
    let names = state
        .repositories
        .iter()
        .map(|repo| repo.name.clone())
        .collect::<BTreeSet<_>>();
    state.selected.retain(|name| names.contains(name));
    for name in names {
        if first || !old.contains(&name) {
            state.selected.insert(name);
        }
    }
    state.worksets = directory
        .worksets
        .into_iter()
        .filter(|workset| workset.project_root == state.project_root)
        .collect();
}

#[cfg(test)]
mod tests {
    use super::suggested_name;

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
}
