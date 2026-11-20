//! 运行时的工作区目录：开着哪些项目、有哪些对话。
//!
//! 运行时是唯一的写入方。客户端用单条命令改它（`workspace.conversation.*`、
//! `workspace.project.*`），经 `workspace.changed` 收整份投影，订阅首帧就带着当前这份。
//! 对话归档跨客户端共享；置顶、侧栏手动序、折叠、终端分屏、当前看哪个，是客户端界面上
//! 的功能数据，归会话插件自己存，不在这里。

use serde::{Deserialize, Serialize};

use crate::agent_kind::ConversationLaunchSpec;
use crate::agent_status::AgentStatus;
use crate::daemon_state::DaemonSessionState;
use crate::runtime_path::PathStyle;

/// 整份目录。`revision` 每次有改动加一，只在同一个 `epoch` 里能比，见 [`WorkspaceRevision`]。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDirectory {
    /// 运行时这份库的随机身份。库换过（删了重建）就不同，版本号从头数。
    #[serde(default)]
    pub epoch: String,
    pub revision: u64,
    /// 打开的项目根目录，按打开的先后。
    #[serde(default)]
    pub projects: Vec<String>,
    /// 项目手动纳入工作集的 Git 仓库。
    #[serde(default)]
    pub repositories: Vec<WorkspaceProjectRepository>,
    /// 项目下已创建的工作集，按创建顺序。
    #[serde(default)]
    pub worksets: Vec<WorkspaceWorkset>,
    /// 按新建的先后。
    #[serde(default)]
    pub conversations: Vec<WorkspaceConversation>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProjectRepository {
    pub project_root: String,
    pub name: String,
    pub root: String,
    pub position: u32,
    #[serde(default)]
    pub managed_link: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProjectRepositoryActionParams {
    pub project_root: String,
    pub action: WorkspaceProjectRepositoryAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceProjectRepositoryAction {
    Discover,
    Bind { source: String, name: String },
    Clone { url: String, name: String },
    Remove { name: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorkset {
    pub id: String,
    pub project_root: String,
    pub task: String,
    pub branch: String,
    pub position: u32,
    pub repositories: Vec<WorkspaceWorksetRepository>,
}

impl WorkspaceWorkset {
    pub fn task_root(&self, style: PathStyle) -> String {
        style.join(&style.join(&self.project_root, "worktrees"), &self.task)
    }

    pub fn contains_cwd(&self, style: PathStyle, cwd: &str) -> bool {
        style.is_absolute(cwd)
            && style.starts_with(&style.clean(cwd), &style.clean(&self.task_root(style)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetRepository {
    pub name: String,
    pub root: String,
    pub worktree: String,
    pub base_ref: String,
    pub branch_created: bool,
    pub position: u32,
}

/// 一段对话：侧栏里的一条。要么是一段 ACP 对话，要么是一组终端。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversation {
    /// 新建它的客户端生成，之后不再变。
    pub id: String,
    #[serde(default)]
    pub custom_title: Option<String>,
    /// 最近一轮结束的 unix 秒。只由守护在回合结束时写，客户端带来的值不算数（新建时填 0）。
    #[serde(default)]
    pub last_updated_at: i64,
    /// 只由运行时的归档命令修改，整段更新不能覆盖。
    #[serde(default)]
    pub archived_at: Option<i64>,
    /// 新会话创建时绑定的工作集；旧会话保持未绑定。
    #[serde(default)]
    pub workset_id: Option<String>,
    /// 已关闭的工作树原目录；未绑定时恢复前必须选择新的工作树。
    #[serde(default)]
    pub closed_workset_root: Option<String>,
    /// 这段对话里的终端，按客户端给的顺序。ACP 对话没有。
    #[serde(default)]
    pub terminals: Vec<WorkspaceTerminal>,
    #[serde(default)]
    pub acp: Option<WorkspaceAcp>,
}

impl WorkspaceConversation {
    /// 这段对话里的守护会话 id：ACP 对话是它的会话，终端是每一格（还没起过、没有 id 的
    /// 不算）。
    pub fn session_ids(&self) -> impl Iterator<Item = &str> {
        self.acp
            .as_ref()
            .map(|acp| acp.sid.as_str())
            .into_iter()
            .chain(
                self.terminals
                    .iter()
                    .filter_map(|terminal| terminal.id.as_deref()),
            )
    }

    /// 跟哪个智能体的对话（智能体定义 id）。自动化运行的、不是跟智能体开的是 `None`。
    pub fn agent_definition(&self) -> Option<&str> {
        let acp = self.acp.as_ref()?;
        if acp.automation_id.is_some() {
            return None;
        }
        acp.agent_definition_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }

    /// `states` 里属于这段对话的那些。
    pub fn states_in<'a>(
        &'a self,
        states: &'a [DaemonSessionState],
    ) -> impl Iterator<Item = &'a DaemonSessionState> {
        states
            .iter()
            .filter(|state| self.session_ids().any(|id| id == state.id))
    }

    /// 对话的状态：各格里最要紧的那个（要你 > 运行中 > 空闲），只看守护推来的。机器连不上
    /// 是另一回事，不折算进这里。
    pub fn status(&self, states: &[DaemonSessionState]) -> AgentStatus {
        AgentStatus::highest(
            self.states_in(states)
                .filter_map(AgentStatus::from_daemon_state),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTerminal {
    /// 守护里的终端会话 id。没起过的终端没有，运行时用对话 id 派生一个占位。
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub launch_label: Option<String>,
    #[serde(default)]
    pub launch_cmd: Option<String>,
}

/// ACP 对话的启动参数和尚未交付的状态。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAcp {
    pub cwd: Option<String>,
    /// 守护托管用的会话 id。写入时必填。
    pub sid: String,
    /// agent 种类标识（`ConversationAgentKind::id()`）。
    pub agent: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    /// 续接用的 provider 对话 id。守护确认过之后以守护为准，客户端带来的旧值不覆盖它。
    #[serde(default)]
    pub history_session_id: Option<String>,
    pub launch: ConversationLaunchSpec,
    /// 最后生效的 provider 配置：`(config_id, value_id)`，按设置顺序。
    #[serde(default)]
    pub config_values: Vec<(String, String)>,
    #[serde(default)]
    pub pending_prompt: Option<String>,
    #[serde(default)]
    pub pending_agent_preset: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub agent_definition_id: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversationIdParams {
    pub conversation_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversationArchiveParams {
    pub conversation_id: String,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_workset_id: Option<String>,
}

/// `workspace.rename` 的参数。带 `terminal_id` 是给这段对话里的这一格终端改名，不带是给
/// 整段对话改名。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRenameParams {
    pub conversation_id: String,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProjectParams {
    pub root: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetCreateParams {
    pub project_root: String,
    pub task: String,
    #[serde(default = "default_workset_branch_prefix")]
    pub branch_prefix: String,
    #[serde(default)]
    pub branch: Option<String>,
    /// 新分支从哪个分支拉出来；不填用各仓库远端的默认分支。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub repository_names: Vec<String>,
}

impl WorkspaceWorksetCreateParams {
    pub fn branch_name(&self) -> String {
        self.branch
            .clone()
            .unwrap_or_else(|| format!("{}/{}", self.branch_prefix.trim_matches('/'), self.task))
    }
}

fn default_workset_branch_prefix() -> String {
    "feat".to_string()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetRenameParams {
    pub workset_id: String,
    pub task: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetFinishParams {
    pub workset_id: String,
    /// 判断不了分支是否已合并（合进基线有冲突）的仓库也照关。有未提交改动、确实没合并的
    /// 仍然拦下。用户在确认框里看过 [`WorkspaceWorksetInspectResult`] 再决定。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_unconfirmed_merge: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetInspectParams {
    pub workset_id: String,
}

/// 工作树分支上的改动进没进基线。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceWorksetMerge {
    Merged,
    /// 确实还有改动没进基线。
    Unmerged,
    /// 判断不了：合进基线有冲突，常见于 squash 合并之后基线又改过同一处。
    Unconfirmed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetRepositoryState {
    pub name: String,
    /// 有未提交的改动或未跟踪的文件。
    pub dirty: bool,
    /// 本地还有工作树分支时它的合并情况；分支已经不在了是空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<WorkspaceWorksetMerge>,
}

/// 关闭工作树前的检查：先 fetch 各仓库的基线，再看每个仓库干不干净、分支合没合进去。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetInspectResult {
    pub repositories: Vec<WorkspaceWorksetRepositoryState>,
    /// fetch 失败的说明；这时合并情况按本地已有的远端分支算。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refresh_failures: Vec<String>,
}

impl WorkspaceWorksetInspectResult {
    /// 每个仓库都干净、分支都已合并（或已经不在），可以直接关。
    pub fn ready_to_finish(&self) -> bool {
        self.repositories.iter().all(|repository| {
            !repository.dirty
                && repository
                    .merge
                    .is_none_or(|merge| merge == WorkspaceWorksetMerge::Merged)
        })
    }
}

/// 新建工作树的回执。目录版本平铺在顶层，和以前只回 `WorkspaceRevision` 时同一个形状。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetCreateResult {
    #[serde(flatten)]
    pub revision: WorkspaceRevision,
    /// 跑了初始化脚本（`<源仓库>/.smelt/worktree-setup`）的仓库，没有脚本的不列。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<WorkspaceWorksetSetupResult>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetSetupResult {
    pub repository: String,
    pub success: bool,
    /// 失败时输出的末尾一段（stdout 和 stderr 合在一起），成功时为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWorksetFinishResult {
    pub revision: WorkspaceRevision,
    pub retained_branches: Vec<String>,
    pub leftover_entries: Vec<String>,
}

/// 目录的版本：哪一份（`epoch`）、改到第几次。命令回执也是它，没改动时是原来的版本。
///
/// 版本号只在同一个 `epoch` 里能比。`epoch` 不同说明运行时的库换过，版本号从头数，
/// 新的那份不能因为数小就被当成旧的扔掉。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRevision {
    #[serde(default)]
    pub epoch: String,
    pub revision: u64,
}

impl WorkspaceRevision {
    /// 同一份目录里比 `other` 旧。不同份之间谈不上新旧。
    pub fn is_older_than(&self, other: &Self) -> bool {
        self.epoch == other.epoch && self.revision < other.revision
    }

    /// 收到它时该不该替换手上的 `current`：换了一份，或同一份里更新。
    pub fn supersedes(&self, current: &Self) -> bool {
        self.epoch != current.epoch || self.revision > current.revision
    }
}

impl WorkspaceDirectory {
    pub fn version(&self) -> WorkspaceRevision {
        WorkspaceRevision {
            epoch: self.epoch.clone(),
            revision: self.revision,
        }
    }

    /// 守护会话 id 属于哪段对话。
    pub fn conversation_of_session(&self, session_id: &str) -> Option<&WorkspaceConversation> {
        self.conversations
            .iter()
            .find(|conversation| conversation.session_ids().any(|id| id == session_id))
    }
}

/// cwd 归属哪个项目根：cwd 就是根、或落在根之下（必须是完整路径段，`/a/bc` 不算
/// 落在 `/a/b` 下）。多个根都匹配时取最深的那个——`~/dev/a` 和 `~/dev/a/sub` 都打开
/// 过时，子目录的会话归后者。空 cwd 或谁都不沾 → None（调用方按 cwd 自建隐式组）。
/// 路径是运行时那台机器上的，按它的系统比（`style`）。
///
/// 归属不存：桌面、手机都按这一份算法从目录里的项目和会话的 cwd 现算。
pub fn project_root_of<'a>(style: PathStyle, roots: &'a [String], cwd: &str) -> Option<&'a str> {
    if cwd.is_empty() {
        return None;
    }
    roots
        .iter()
        .map(|root| root.trim_end_matches('/'))
        .filter(|root| !root.is_empty() && style.starts_with(cwd, root))
        .max_by_key(|root| root.len())
}

/// 项目显示名：目录末段。根目录这类没有末段的按当前语言叫「项目」。
pub fn project_name(style: PathStyle, root: &str) -> String {
    match style.file_name(root) {
        Some(name) => name.to_string(),
        None => rust_i18n::t!("project.unnamed").into_owned(),
    }
}

/// 一组项目在同一个列表里的显示名：末段重名的逐步往前补父目录段（`dev · smelt`），
/// 直到互不相同或路径补到顶。项目都在同一个运行时上，路径按它的系统切（`style`）。
pub fn project_labels(style: PathStyle, roots: &[&str]) -> Vec<String> {
    let bases: Vec<String> = roots.iter().map(|root| project_name(style, root)).collect();
    let mut labels = bases.clone();
    let mut extra = vec![0usize; roots.len()];
    // 每轮给所有重名的多补一段；路径最深也就那么几段，8 轮足够收敛。
    for _ in 0..8 {
        let dup: Vec<usize> = (0..labels.len())
            .filter(|&i| (0..labels.len()).any(|j| j != i && labels[j] == labels[i]))
            .collect();
        if dup.is_empty() {
            break;
        }
        let mut changed = false;
        for i in dup {
            let next = extra[i] + 1;
            let candidate = label_with_parents(style, roots[i], &bases[i], next);
            if candidate != labels[i] {
                extra[i] = next;
                labels[i] = candidate;
                changed = true;
            }
        }
        // 全都补到路径顶了还重名（真同路径 / 只剩根）→ 认命，别空转。
        if !changed {
            break;
        }
    }
    labels
}

/// 往前补 `extra` 段父目录：`/a/b/smelt` + 1 → `b · smelt`。
fn label_with_parents(style: PathStyle, root: &str, base: &str, extra: usize) -> String {
    let (separators, joiner): (&[char], &str) = match style {
        PathStyle::Posix => (&['/'], "/"),
        PathStyle::Windows => (&['\\', '/'], "\\"),
    };
    let segs: Vec<&str> = root.split(separators).filter(|s| !s.is_empty()).collect();
    let end = segs.len().saturating_sub(1); // base 已经代表末段
    let start = end.saturating_sub(extra);
    if start >= end {
        return base.to_string();
    }
    format!("{} · {}", segs[start..end].join(joiner), base)
}

#[cfg(test)]
mod workset_tests {
    use super::{
        PathStyle, WorkspaceWorkset, WorkspaceWorksetCreateParams, WorkspaceWorksetRepository,
    };

    #[test]
    fn workset_branch_defaults_to_feat_but_supports_prefixes_and_custom_names() {
        let mut params: WorkspaceWorksetCreateParams = serde_json::from_value(serde_json::json!({
            "project_root": "/project",
            "task": "搜索",
            "repository_names": ["api"]
        }))
        .unwrap();
        assert_eq!(params.branch_name(), "feat/搜索");
        params.branch_prefix = "fix".into();
        assert_eq!(params.branch_name(), "fix/搜索");
        params.branch = Some("hotfix/custom-name".into());
        assert_eq!(params.branch_name(), "hotfix/custom-name");
    }

    #[test]
    fn workset_sessions_start_at_the_task_root_for_any_repository_count() {
        let mut workset = WorkspaceWorkset {
            project_root: "/project".into(),
            task: "task".into(),
            ..Default::default()
        };
        for count in [1, 3] {
            workset.repositories = vec![WorkspaceWorksetRepository::default(); count];
            assert_eq!(
                workset.task_root(PathStyle::Posix),
                "/project/worktrees/task"
            );
        }
        workset.project_root = r"C:\projects\app".into();
        assert_eq!(
            workset.task_root(PathStyle::Windows),
            r"C:\projects\app\worktrees\task"
        );
    }

    #[test]
    fn workset_directory_accepts_root_and_children_but_not_siblings_or_escape() {
        let workset = WorkspaceWorkset {
            project_root: "/project".into(),
            task: "task".into(),
            ..Default::default()
        };
        for cwd in [
            "/project/worktrees/task",
            "/project/worktrees/task/backend/src",
        ] {
            assert!(workset.contains_cwd(PathStyle::Posix, cwd));
        }
        for cwd in [
            "/project",
            "/project/worktrees/task-other",
            "/project/worktrees/task/../other",
            "project/worktrees/task",
        ] {
            assert!(!workset.contains_cwd(PathStyle::Posix, cwd), "{cwd}");
        }
        let windows = WorkspaceWorkset {
            project_root: r"C:\projects\app".into(),
            ..workset
        };
        assert!(windows.contains_cwd(PathStyle::Windows, r"c:\projects\app\worktrees\task\api"));
        assert!(!windows.contains_cwd(
            PathStyle::Windows,
            r"C:\projects\app\worktrees\task\..\other"
        ));
    }
}

#[cfg(test)]
mod conversation_tests {
    use super::{WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal};
    use crate::agent_status::AgentStatus;
    use crate::daemon_state::{DaemonPhase, DaemonSessionState};

    fn terminals(id: &str, terminals: &[Option<&str>]) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            terminals: terminals
                .iter()
                .map(|terminal| WorkspaceTerminal {
                    id: terminal.map(Into::into),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn state(id: &str, phase: DaemonPhase) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            phase,
            turn_events: true,
            ..Default::default()
        }
    }

    #[test]
    fn sessions_of_a_conversation_and_back() {
        let acp = WorkspaceConversation {
            id: "c2".into(),
            acp: Some(WorkspaceAcp {
                sid: "a1".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let directory = WorkspaceDirectory {
            conversations: vec![terminals("c1", &[Some("t1"), None, Some("t2")]), acp],
            ..Default::default()
        };
        assert_eq!(
            directory.conversations[0].session_ids().collect::<Vec<_>>(),
            ["t1", "t2"],
            "没起过的终端不算"
        );
        let of = |id| directory.conversation_of_session(id).map(|c| c.id.as_str());
        assert_eq!(of("t2"), Some("c1"));
        assert_eq!(of("a1"), Some("c2"));
        assert_eq!(of("nope"), None);
    }

    #[test]
    fn agent_definition_skips_automation_runs() {
        let mut conversation = WorkspaceConversation {
            acp: Some(WorkspaceAcp {
                agent_definition_id: Some(" writer ".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(conversation.agent_definition(), Some("writer"));
        conversation.acp.as_mut().unwrap().automation_id = Some("daily".into());
        assert_eq!(conversation.agent_definition(), None);
        conversation.acp.as_mut().unwrap().agent_definition_id = Some(" ".into());
        conversation.acp.as_mut().unwrap().automation_id = None;
        assert_eq!(conversation.agent_definition(), None);
        assert_eq!(terminals("t", &[]).agent_definition(), None);
    }

    #[test]
    fn status_is_the_most_urgent_pane() {
        let conversation = terminals("c", &[Some("t1"), Some("t2")]);
        let states = [
            state("t1", DaemonPhase::Thinking),
            state("t2", DaemonPhase::AwaitingApproval),
            state("other", DaemonPhase::Failed),
        ];
        assert_eq!(conversation.states_in(&states).count(), 2);
        assert_eq!(conversation.status(&states), AgentStatus::NeedsYou);
        assert_eq!(conversation.status(&[]), AgentStatus::Idle);
    }
}

#[cfg(test)]
mod revision_tests {
    use super::WorkspaceRevision;

    fn at(epoch: &str, revision: u64) -> WorkspaceRevision {
        WorkspaceRevision {
            epoch: epoch.into(),
            revision,
        }
    }

    /// 同一份里按版本号比；库换过之后版本号从头数，新的那份不是旧的。
    #[test]
    fn revisions_compare_only_within_one_epoch() {
        assert!(at("a", 3).is_older_than(&at("a", 4)));
        assert!(!at("a", 4).is_older_than(&at("a", 4)));
        assert!(!at("b", 1).is_older_than(&at("a", 40)), "换了库");

        assert!(at("a", 5).supersedes(&at("a", 4)));
        assert!(!at("a", 4).supersedes(&at("a", 4)), "同一份不重复收");
        assert!(!at("a", 3).supersedes(&at("a", 4)));
        assert!(at("b", 1).supersedes(&at("a", 40)), "换了库就收新的");
    }
}

#[cfg(test)]
mod project_tests {
    use super::{PathStyle, project_labels, project_name, project_root_of};

    fn labels(roots: &[&str]) -> Vec<String> {
        project_labels(PathStyle::Posix, roots)
    }

    fn projects(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// 项目名是目录末段。Windows 运行时报来的路径用 `\` 分隔，按 `/` 切会把整条路径当成名字。
    #[test]
    fn project_name_is_the_last_segment_on_the_runtime() {
        assert_eq!(project_name(PathStyle::Posix, "/a/smelt/"), "smelt");
        assert_eq!(
            project_name(PathStyle::Windows, r"C:\Users\w\code\xunxian-reverse"),
            "xunxian-reverse"
        );
        assert_eq!(project_name(PathStyle::Posix, "/"), "Project");
        assert_eq!(project_name(PathStyle::Windows, r"C:\"), "Project");
    }

    /// Windows 项目重名时同样往前补父目录段，段之间用 `\` 连。
    #[test]
    fn windows_duplicate_labels_get_parent_segments() {
        assert_eq!(
            project_labels(PathStyle::Windows, &[r"C:\x\dev\smelt", r"D:\y\dev\smelt"]),
            [r"x\dev · smelt", r"y\dev · smelt"]
        );
    }

    /// 不重名就别乱加前缀（大多数情况该保持干净的目录名）。
    #[test]
    fn unique_labels_are_left_alone() {
        assert_eq!(labels(&["/a/smelt", "/a/other"]), ["smelt", "other"]);
    }

    /// 末段同名的两个项目必须区分得开——否则并排两个一模一样的「smelt」，分不清哪个是哪个。
    #[test]
    fn duplicate_labels_get_parent_segments() {
        assert_eq!(
            labels(&["/x/dev/smelt", "/y/work/smelt"]),
            ["dev · smelt", "work · smelt"]
        );
    }

    /// 补一段还撞车就继续往上补，直到分开。
    #[test]
    fn keeps_climbing_until_unique() {
        assert_eq!(
            labels(&["/a/dev/smelt", "/b/dev/smelt"]),
            ["a/dev · smelt", "b/dev · smelt"]
        );
    }

    /// 路径补到顶还重名（真·同路径）→ 必须收敛退出，不能在循环里空转。
    #[test]
    fn gives_up_instead_of_looping_forever() {
        assert_eq!(labels(&["/smelt", "/smelt"]), ["smelt", "smelt"]);
    }

    /// cwd 就是项目根、或落在项目根之下，都该归这个项目。
    #[test]
    fn cwd_belongs_to_its_project_root() {
        let p = projects(&["/Users/me/dev/smelt"]);
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/Users/me/dev/smelt"),
            Some("/Users/me/dev/smelt")
        );
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/Users/me/dev/smelt/crates/smeltd"),
            Some("/Users/me/dev/smelt")
        );
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/Users/me/dev/other"),
            None
        );
        assert_eq!(project_root_of(PathStyle::Posix, &p, ""), None);
    }

    /// 前缀必须卡在完整路径段上：`/a/smelt-old` 不是 `/a/smelt` 的子目录，
    /// 否则名字相近的两个项目会互相吞会话。
    #[test]
    fn prefix_must_be_a_whole_path_segment() {
        let p = projects(&["/a/smelt"]);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/smelt-old"), None);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/smeltd"), None);
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/a/smelt/sub"),
            Some("/a/smelt")
        );
    }

    /// 父子项目都打开着时，会话归最深的那个（不然子项目永远空着）。
    #[test]
    fn deepest_matching_project_wins() {
        let p = projects(&["/a", "/a/b", "/a/b/c"]);
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/a/b/c/x"),
            Some("/a/b/c")
        );
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/a/b/x"),
            Some("/a/b")
        );
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/x"), Some("/a"));
    }

    /// Windows 运行时的会话开在项目子目录里也归这个项目：按 `\` 切、不分大小写。
    #[test]
    fn windows_cwd_belongs_to_its_project_root() {
        let p = projects(&[r"C:\Users\w\code\smelt"]);
        assert_eq!(
            project_root_of(PathStyle::Windows, &p, r"C:\Users\w\code\smelt\crates"),
            Some(r"C:\Users\w\code\smelt")
        );
        assert_eq!(
            project_root_of(PathStyle::Windows, &p, r"c:\users\w\code\SMELT"),
            Some(r"C:\Users\w\code\smelt")
        );
        assert_eq!(
            project_root_of(PathStyle::Windows, &p, r"C:\Users\w\code\smelt-old"),
            None
        );
    }

    /// 结尾斜杠是路径写法差异，不该影响归属判定。
    #[test]
    fn trailing_slashes_are_ignored() {
        let p = projects(&["/a/b/"]);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b"), Some("/a/b"));
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/"), Some("/a/b"));
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/a/b/c"),
            Some("/a/b")
        );
    }
}
