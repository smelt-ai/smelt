//! 工作台和插件之间：把侧栏的事实发布成 `smelt-workspace` 的快照，执行插件发来的操作。
//!
//! 快照只比较、写内存，没变就不动版本号，所以调用方可以放心多调。调用时机：
//! - `sync_notification_surfaces`：会话状态、提醒、守护推送；
//! - `save_state`：项目列表、置顶、排序、当前项目，这些变了都会落盘；
//! - 运行时列表变化（机器上下线）。
//!
//! 新建会话的选项不进快照：它跟着设置和远程机器上装了什么变，插件要时现取。

use super::*;
use crate::plugins::{
    CurrentProject, GitSummary, MachineEntry, MachineInfo, ProjectInfo, SessionInfo,
    WorkspaceRequest,
};
use crate::workspace_attention::{attention_badge, menu_jump_target, session_attention_ids};

impl Workspace {
    /// 把当前项目、它的 git 摘要、会话和机器（带项目）的快照发给插件。只读内存。
    pub(crate) fn publish_to_plugins(&self, cx: &App) {
        let badge = attention_badge(cx);
        let active = self.active_project_ref(cx);
        crate::plugins::set_project(active.as_ref().map(|project| CurrentProject {
            machine: machine_info(&project.runtime, cx),
            root: project.root.clone(),
        }));
        self.publish_git_to_plugins(active.as_ref());
        let mut project_of: HashMap<usize, String> = HashMap::new();
        let machines = self
            .spaces
            .iter()
            .map(|space| {
                let key = space.runtime.key();
                let projects = self
                    .project_groups(key, cx)
                    .into_iter()
                    .map(|group| {
                        for &ix in &group.sessions {
                            project_of.insert(ix, group.root.clone());
                        }
                        ProjectInfo {
                            opened: space.projects.iter().any(|root| {
                                crate::sidebar_order::same_project_root(root, &group.root)
                            }),
                            pinned: crate::sidebar_order::is_pinned_project(
                                &space.pinned_projects,
                                &group.root,
                            ),
                            active: active.as_ref().is_some_and(|active| {
                                active.runtime == *key
                                    && crate::sidebar_order::same_project_root(
                                        &active.root,
                                        &group.root,
                                    )
                            }),
                            name: group.label,
                            root: group.root,
                        }
                    })
                    .collect();
                MachineEntry {
                    machine: machine_info(key, cx),
                    projects,
                }
            })
            .collect();
        crate::plugins::set_machines(machines);

        crate::plugins::set_stages(self.stage_infos(cx));
        // 全部会话按侧栏顺序，挂着要处理的提醒也算「要你」。有焦点的舞台上的看
        // `viewed_session_id`（它管被挡着、页面盖着），别的舞台上的看那块露没露出来。
        let viewed = self.viewed_session_id(cx);
        let focused = self.focused_stage();
        let parked = self.parked_sessions();
        let sessions = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(ix, session)| {
                let ids = session_attention_ids(session, cx);
                let active_id = session
                    .active_term()
                    .map(|pane| pane.read(cx).session_id().to_string());
                let id = menu_jump_target(&ids, &badge, active_id.as_deref())?;
                let requires_action = ids.iter().any(|id| badge.get(id) == Some(&true));
                let unread = ids.iter().any(|id| badge.get(id) == Some(&false));
                let acp = session.active_acp().map(|view| view.read(cx));
                let runtime = session.runtime(cx);
                let stage = if ids.iter().any(|id| Some(id.as_str()) == viewed.as_deref()) {
                    Some(focused.clone())
                } else {
                    parked
                        .iter()
                        .find(|(parked, _)| *parked == ix)
                        .map(|(_, stage)| stage.clone())
                };
                Some(SessionInfo {
                    stage,
                    id,
                    title: Self::title_with_runtime(session, cx),
                    kind: if acp.is_some() {
                        "conversation"
                    } else {
                        "terminal"
                    },
                    agent: session.agent_icon_id(cx),
                    status: if requires_action {
                        AgentStatus::NeedsYou
                    } else {
                        session.status(cx)
                    },
                    unread: unread && !requires_action,
                    cwd: session.cwd(cx),
                    project: project_of.remove(&ix),
                    machine: machine_info(runtime.key(), cx),
                    model: acp.and_then(|view| view.model_name()),
                    context: acp.and_then(|view| view.context_usage()),
                    effort: acp.and_then(|view| view.effort()),
                    updated_at: session.effective_updated_at(cx),
                    turn_started_at: session.turn_started_at(cx),
                })
            })
            .collect();
        crate::plugins::set_sessions(sessions);
    }

    /// 把当前项目根目录的 git 摘要发给插件。项目一换（[`Self::publish_to_plugins`]）、
    /// 这个目录的 git 状态读回来（`ensure_git_status`）各发一次；内容没变不叫醒插件。
    pub(crate) fn publish_git_to_plugins(&self, active: Option<&ProjectRef>) {
        let git = active.and_then(|project| {
            let (_, data) = self.git_status.get(project)?;
            data.ok.then(|| GitSummary {
                root: project.root.clone(),
                branch: data.branch.clone(),
                upstream: data.upstream.clone(),
                ahead: data.ahead,
                behind: data.behind,
                changed: data.files.len() as u32,
                stashes: data.stash_count,
            })
        });
        crate::plugins::set_git(git);
    }

    /// 插件经 `smelt-workspace` 发来的操作。会话、项目已经不在了就什么也不做：插件手里的
    /// 快照可能比工作台旧一拍。
    pub(crate) fn handle_plugin_request(
        &mut self,
        request: WorkspaceRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match request {
            WorkspaceRequest::OpenSession(id) => {
                self.request_notification_session_jump(id, window, cx);
            }
            // 和点 × 一样：正在跑的先确认。
            WorkspaceRequest::CloseSession(id) => {
                if let Some((ix, _)) = self.find_daemon_session(&id, cx) {
                    self.request_close_session(ix, window, cx);
                }
            }
            WorkspaceRequest::SelectProject { machine, root } => {
                let project = ProjectRef {
                    runtime: runtime_key_of(&machine),
                    root,
                };
                if self.find_project_group(&project, cx).is_some() {
                    self.select_project(project, window, cx);
                }
            }
            WorkspaceRequest::NewSession { machine, root, key } => {
                let runtime_key = runtime_key_of(&machine);
                let Some(runtime) = crate::runtimes::runtime(&runtime_key, cx) else {
                    return;
                };
                let Some(target) =
                    crate::session_list::find_new_session_target(&runtime_key, &key, cx)
                else {
                    return;
                };
                let cwd = (!root.is_empty()).then_some(root);
                crate::session_list::launch_new_session(self, runtime, cwd, target, window, cx);
            }
            WorkspaceRequest::OpenSettings(Some(section)) => {
                self.open_settings_section(section, cx);
            }
            WorkspaceRequest::OpenSettings(None) => self.open_settings(cx),
            // 按当前焦点派发，和按键一样：焦点不在文件里时 save_file 就什么也不做。
            WorkspaceRequest::Run(id) => {
                if let Some(shortcut) = crate::keymap::shortcut(id) {
                    window.dispatch_action(shortcut.action(), cx);
                }
            }
            WorkspaceRequest::Layout(command) => self.run_layout_command(command, window, cx),
        }
    }
}

/// 这台机器上能新建什么，和侧栏「+」同一份。远程机器要先去那台机器读一次，所以给的是
/// 等结果的接收端。没有这台机器时是 None。
pub(crate) fn new_session_options(
    machine: &str,
    cx: &mut App,
) -> Option<tokio::sync::oneshot::Receiver<smelt_model::new_session::NewSessionSections>> {
    let runtime = crate::runtimes::runtime(&runtime_key_of(machine), cx)?;
    let (reply, receiver) = tokio::sync::oneshot::channel();
    crate::session_list::new_session_sections(
        runtime,
        move |sections, _| {
            // 插件已经卸载、不等了：丢掉就好。
            let _ = reply.send(sections);
        },
        cx,
    );
    Some(receiver)
}

/// 插件看到的机器 id：本机是 `local`，远程是运行时 id。
fn machine_id(key: &RuntimeKey) -> String {
    match key {
        RuntimeKey::Local => "local".to_owned(),
        RuntimeKey::Remote(id) => id.clone(),
    }
}

fn runtime_key_of(machine: &str) -> RuntimeKey {
    if machine == "local" {
        RuntimeKey::Local
    } else {
        RuntimeKey::Remote(machine.to_owned())
    }
}

fn machine_info(key: &RuntimeKey, cx: &App) -> MachineInfo {
    MachineInfo {
        id: machine_id(key),
        name: crate::runtimes::runtime_name(key, cx),
        local: *key == RuntimeKey::Local,
        online: crate::runtimes::runtime_unavailable_label(key, cx).is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeKey, machine_id, runtime_key_of};

    #[test]
    fn machine_ids_round_trip() {
        for key in [RuntimeKey::Local, RuntimeKey::Remote("abc".to_owned())] {
            assert_eq!(runtime_key_of(&machine_id(&key)), key);
        }
    }
}
