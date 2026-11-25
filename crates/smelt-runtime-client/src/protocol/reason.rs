//! 控制调用报错的种类和参数（[`ControlReason`]），客户端按自己的界面语言说。
//!
//! 运行时不知道看的人用什么语言，所以不替人措辞：它发种类和参数，客户端在
//! [`ControlReason::text`] 里按当前语言（rust-i18n 的全局值）取文案。同一个报错的
//! `message` 是运行时进程里按默认语言（英文）说的同一句话，给不认识 `reason` 的老客户端、
//! 日志和比客户端新的运行时发来的种类兜底。
//!
//! 只新增字段，不改控制协议版本：老运行时不发 `reason`，老客户端不读它。

use rust_i18n::t;
use serde::{Deserialize, Serialize};
use smelt_event_protocol::TurnFailure;

/// 一种报错原因。新增种类只往后加；客户端不认识的种类读成 [`ControlReason::Other`]，
/// 改说 `message`。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlReason {
    /// 运行时不认识这个智能体。
    UnknownAgent { agent: String },
    /// 建不了智能体工作区。
    AgentWorkspaceUnavailable,
    /// 智能体还不能开对话（没有已注册的产品引擎）。
    AgentNotConversationReady,
    /// 建不了记忆目录。
    MemoryDirUnavailable,
    /// 归档对话时又指定了要搬去的工作树。
    ArchiveWithTargetWorktree,
    /// 改外观要本机桌面，眼下没有。
    AppearanceNoDesktop,
    /// 本机桌面不止一个，不知道交给谁改外观。
    AppearanceManyDesktops,
    /// 外观请求没能交到桌面。
    AppearanceUndelivered,
    /// 交给桌面了，没等到回执。
    AppearanceOutcomeUnknown,
    /// 没有这个自动化。
    AutomationNotFound { id: String },
    /// 对话里已经没有这张图（回退或压缩改写过）。
    ImageGone,
    /// 图片解不开，出不了缩略图。
    ImageUndecodable { mime: String },
    /// 对话不存在或已经结束。
    ConversationGone,
    /// 远程访问没开。
    RemoteAccessOff,
    /// 配对只能由远程设备发起。
    PairFromRemoteOnly,
    /// 配对码不对或过期了。
    PairCodeInvalid,
    /// 推送只能由已配对的远程设备登记。
    PushFromPairedOnly,
    /// 这台设备已经不在配对列表里。
    DeviceUnpaired,
    /// 不是子智能体 id。
    NotSubagentId { id: String },
    /// 找不到 Smelt 主目录。
    NoSmeltHome,
    /// 子智能体的子会话不在了。
    SubagentTranscriptGone,
    /// 读不了子智能体的子会话。
    SubagentTranscriptUnreadable { error: String },
    /// 仓库名为空或重复。
    RepositoryNameInvalid { name: String },
    /// 项目里没有这个仓库。
    RepositoryNotInProject { name: String },
    /// 没有这个工作树。
    WorktreeNotFound,
    /// 工作树还绑着会话，不能改名。
    WorktreeHasSessions,
    /// 工作树还绑着没归档的会话，不能删。
    WorktreeHasOpenSessions,
    /// Git worktree 关了，记录没删掉。
    WorktreeRecordNotDeleted { error: String },
    /// 自动化运行的会话归运行时管，不能归档。
    AutomationSessionNotArchivable,
    /// 没有这个启动项。
    UnknownLaunchItem { key: String },
    /// 没有这个智能体选项。
    UnknownAgentOption { id: String },
    /// 工作区里没有这个项目。
    ProjectNotInWorkspace { project: String },
    /// 项目里没有这个工作树。
    WorktreeNotInProject { project: String, worktree: String },
    /// 目录不存在。
    DirectoryMissing { path: String },
    /// 自动化运行的会话归运行时管，不能删。
    AutomationSessionNotDeletable,
    /// 没有这个会话。
    SessionNotFound { id: String },
    /// 这台设备还没配对。
    DeviceNotPaired,
    /// 自动化的配置或这次触发哪里不对。
    Automation { problem: AutomationProblem },
    /// 要丢弃的改动在用户看过之后又变了，没有丢。
    GitChangedSinceViewed { path: String },
    /// 仓库没有远端，推不了。
    GitNoRemote,
    /// 分支还没上游，又有多个远端，不知道推到哪。`remotes` 用逗号隔开。
    GitPushTargetAmbiguous { branch: String, remotes: String },
    /// 合并没成，冲突文件留在工作区。`detail` 是 git 的原话。
    GitMergeFailed { detail: String },
    /// 新建工作树一个仓库都没选。
    WorktreeNoRepository,
    /// 工作树要放的位置已经有东西了。
    WorktreeTargetExists { path: String },
    /// 分支已在别处检出。
    WorktreeBranchCheckedOut {
        repository: String,
        branch: String,
        path: String,
    },
    /// 关闭工作树时仓库还有未提交改动或未跟踪文件。
    WorktreeRepositoryDirty { repository: String },
    /// 关闭工作树时分支还有改动没进基线。
    WorktreeBranchUnmerged {
        repository: String,
        branch: String,
        base: String,
    },
    /// 关闭工作树时确认不了分支合没合进基线（合进去有冲突）。
    WorktreeMergeUnconfirmed {
        repository: String,
        branch: String,
        base: String,
    },
    /// 重命名工作树时仓库还有未提交改动。
    WorktreeRenameDirty { repository: String },
    /// 重命名工作树时仓库当前分支变了。
    WorktreeBranchChanged { repository: String },
    /// 仓库没有远端，定不了上游默认分支。
    WorktreeNoRemote { repository: String },
    /// 仓库找不到可用的上游默认分支。
    WorktreeNoUpstream { repository: String },
    /// 指定的基线分支在仓库的远端和本地都找不到。
    WorktreeBaseNotFound { repository: String, base: String },
    /// 选的目录不是 Git 仓库。
    RepositoryNotGit,
    /// 选的是仓库里的子目录，不是根目录。
    RepositoryNotRoot { root: String },
    /// 项目 `repos` 下这个名字已经有东西了。
    RepositoryNameTaken { path: String },
    /// Git URL 不对。
    RepositoryUrlInvalid,
    /// 克隆没成（URL、网络或凭据）。删不掉的半成品目录在 `kept`。
    RepositoryCloneFailed { kept: Option<String> },
    /// 这个仓库或名字已经在项目里了。
    RepositoryAlreadyAdded,
    /// 要本机开着的 Smelt App，眼下没有。
    AppNotOpen,
    /// 本机开着不止一个 Smelt App，不知道交给谁。
    AppAmbiguous,
    /// 请求没能交到 App。
    AppUndelivered,
    /// 交给 App 了，没等到回执。
    AppOutcomeUnknown,
    /// 比这个客户端新的运行时发来的种类。
    #[serde(other)]
    Other,
}

impl ControlReason {
    /// 按当前语言说这个原因。[`ControlReason::Other`] 没有自己的话，给 `None`，调用方改用
    /// `message`。
    pub fn text(&self) -> Option<String> {
        let text = match self {
            Self::UnknownAgent { agent } => t!("reason.unknown_agent", agent = agent),
            Self::AgentWorkspaceUnavailable => t!("reason.agent_workspace_unavailable"),
            Self::AgentNotConversationReady => t!("reason.agent_not_conversation_ready"),
            Self::MemoryDirUnavailable => t!("reason.memory_dir_unavailable"),
            Self::ArchiveWithTargetWorktree => t!("reason.archive_with_target_worktree"),
            Self::AppearanceNoDesktop => t!("reason.appearance_no_desktop"),
            Self::AppearanceManyDesktops => t!("reason.appearance_many_desktops"),
            Self::AppearanceUndelivered => t!("reason.appearance_undelivered"),
            Self::AppearanceOutcomeUnknown => t!("reason.appearance_outcome_unknown"),
            Self::AutomationNotFound { id } => t!("reason.automation_not_found", id = id),
            Self::ImageGone => t!("reason.image_gone"),
            Self::ImageUndecodable { mime } => t!("reason.image_undecodable", mime = mime),
            Self::ConversationGone => t!("reason.conversation_gone"),
            Self::RemoteAccessOff => t!("reason.remote_access_off"),
            Self::PairFromRemoteOnly => t!("reason.pair_from_remote_only"),
            Self::PairCodeInvalid => t!("reason.pair_code_invalid"),
            Self::PushFromPairedOnly => t!("reason.push_from_paired_only"),
            Self::DeviceUnpaired => t!("reason.device_unpaired"),
            Self::NotSubagentId { id } => t!("reason.not_subagent_id", id = id),
            Self::NoSmeltHome => t!("reason.no_smelt_home"),
            Self::SubagentTranscriptGone => t!("reason.subagent_transcript_gone"),
            Self::SubagentTranscriptUnreadable { error } => {
                t!("reason.subagent_transcript_unreadable", error = error)
            }
            Self::RepositoryNameInvalid { name } => {
                t!("reason.repository_name_invalid", name = name)
            }
            Self::RepositoryNotInProject { name } => {
                t!("reason.repository_not_in_project", name = name)
            }
            Self::WorktreeNotFound => t!("reason.worktree_not_found"),
            Self::WorktreeHasSessions => t!("reason.worktree_has_sessions"),
            Self::WorktreeHasOpenSessions => t!("reason.worktree_has_open_sessions"),
            Self::WorktreeRecordNotDeleted { error } => {
                t!("reason.worktree_record_not_deleted", error = error)
            }
            Self::AutomationSessionNotArchivable => t!("reason.automation_session_not_archivable"),
            Self::UnknownLaunchItem { key } => t!("reason.unknown_launch_item", key = key),
            Self::UnknownAgentOption { id } => t!("reason.unknown_agent_option", id = id),
            Self::ProjectNotInWorkspace { project } => {
                t!("reason.project_not_in_workspace", project = project)
            }
            Self::WorktreeNotInProject { project, worktree } => t!(
                "reason.worktree_not_in_project",
                project = project,
                worktree = worktree
            ),
            Self::DirectoryMissing { path } => t!("reason.directory_missing", path = path),
            Self::AutomationSessionNotDeletable => t!("reason.automation_session_not_deletable"),
            Self::SessionNotFound { id } => t!("reason.session_not_found", id = id),
            Self::DeviceNotPaired => t!("reason.device_not_paired"),
            Self::Automation { problem } => return problem.text(),
            Self::GitChangedSinceViewed { path } => {
                t!("reason.git_changed_since_viewed", path = path)
            }
            Self::GitNoRemote => t!("reason.git_no_remote"),
            Self::GitPushTargetAmbiguous { branch, remotes } => t!(
                "reason.git_push_target_ambiguous",
                branch = branch,
                remotes = remotes
            ),
            Self::GitMergeFailed { detail } => t!("reason.git_merge_failed", detail = detail),
            Self::WorktreeNoRepository => t!("reason.worktree_no_repository"),
            Self::WorktreeTargetExists { path } => t!("reason.worktree_target_exists", path = path),
            Self::WorktreeBranchCheckedOut {
                repository,
                branch,
                path,
            } => t!(
                "reason.worktree_branch_checked_out",
                repository = repository,
                branch = branch,
                path = path
            ),
            Self::WorktreeRepositoryDirty { repository } => {
                t!("reason.worktree_repository_dirty", repository = repository)
            }
            Self::WorktreeBranchUnmerged {
                repository,
                branch,
                base,
            } => t!(
                "reason.worktree_branch_unmerged",
                repository = repository,
                branch = branch,
                base = base
            ),
            Self::WorktreeMergeUnconfirmed {
                repository,
                branch,
                base,
            } => t!(
                "reason.worktree_merge_unconfirmed",
                repository = repository,
                branch = branch,
                base = base
            ),
            Self::WorktreeRenameDirty { repository } => {
                t!("reason.worktree_rename_dirty", repository = repository)
            }
            Self::WorktreeBranchChanged { repository } => {
                t!("reason.worktree_branch_changed", repository = repository)
            }
            Self::WorktreeNoRemote { repository } => {
                t!("reason.worktree_no_remote", repository = repository)
            }
            Self::WorktreeNoUpstream { repository } => {
                t!("reason.worktree_no_upstream", repository = repository)
            }
            Self::WorktreeBaseNotFound { repository, base } => t!(
                "reason.worktree_base_not_found",
                repository = repository,
                base = base
            ),
            Self::RepositoryNotGit => t!("reason.repository_not_git"),
            Self::RepositoryNotRoot { root } => t!("reason.repository_not_root", root = root),
            Self::RepositoryNameTaken { path } => t!("reason.repository_name_taken", path = path),
            Self::RepositoryUrlInvalid => t!("reason.repository_url_invalid"),
            Self::RepositoryCloneFailed { kept: None } => t!("reason.repository_clone_failed"),
            Self::RepositoryCloneFailed { kept: Some(path) } => {
                t!("reason.repository_clone_failed_kept", path = path)
            }
            Self::RepositoryAlreadyAdded => t!("reason.repository_already_added"),
            Self::AppNotOpen => t!("reason.app_not_open"),
            Self::AppAmbiguous => t!("reason.app_ambiguous"),
            Self::AppUndelivered => t!("reason.app_undelivered"),
            Self::AppOutcomeUnknown => t!("reason.app_outcome_unknown"),
            Self::Other => return None,
        };
        Some(text.into_owned())
    }
}

/// 自动化的配置或这次触发哪里不对：保存前的校验（桌面、手机在本地先查一遍）和运行时拒绝
/// 保存、触发时说的是同一套。种类在线上跟着 [`ControlReason::Automation`] 走，客户端按
/// 自己的语言说。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationProblem {
    /// 智能体动作没选智能体。
    NoAgent,
    /// Shell 动作没填命令。
    NoShellCommand,
    /// 外部触发没有接收令牌。
    NoWebhookToken,
    /// 一条触发条件都没有。
    NoTrigger,
    /// 没有 id。
    NoId,
    /// 没填名称。
    NoName,
    /// 运行时还没给它分配工作区。
    WorkspaceNotReady,
    /// 工作区不存在或不是目录。
    WorkspaceMissing { path: String },
    /// 定时触发的智能体动作没填指令。
    NoInstruction,
    /// 按周重复但一天都没选。
    NoDays,
    /// 时间不在 00:00 到 23:59 之间。
    TimeOutOfRange,
    /// 间隔分钟数不在 1 到 1440 之间。
    MinutesOutOfRange,
    /// 间隔小时数不在 1 到 168 之间。
    HoursOutOfRange,
    /// 时段的结束时间不晚于开始时间。
    EndBeforeStart,
    /// 这次运行指令和触发内容都是空的。
    NothingToSend,
    /// 没有这个自动化。
    NotFound,
    /// 没有这条运行记录。
    RunNotFound,
    /// 已经有一次在跑。
    AlreadyRunning { run: String },
    /// 正在运行，不能删。
    DeleteWhileRunning,
    /// 运行现场还在清理，不能删。
    DeleteWhileCleaningUp,
    /// 事件主题没填。
    EventTopicMissing,
    /// 事件主题太长。
    EventTopicTooLong,
    /// 事件主题不是小写点分名。
    EventTopicMalformed,
    /// 事件 id 没填。
    EventIdMissing,
    /// 事件 id 太长。
    EventIdTooLong,
    /// 事件 id 里有不能用的字符。
    EventIdInvalid,
    /// 事件过滤不是 JSON 对象。
    EventFilterNotObject,
    /// 比这个客户端新的运行时发来的种类。
    #[serde(other)]
    Other,
}

impl AutomationProblem {
    /// 按当前语言说这个问题。[`AutomationProblem::Other`] 没有自己的话，给 `None`。
    pub fn text(&self) -> Option<String> {
        let text = match self {
            Self::NoAgent => t!("automation.no_agent"),
            Self::NoShellCommand => t!("automation.no_shell_command"),
            Self::NoWebhookToken => t!("automation.no_webhook_token"),
            Self::NoTrigger => t!("automation.no_trigger"),
            Self::NoId => t!("automation.no_id"),
            Self::NoName => t!("automation.no_name"),
            Self::WorkspaceNotReady => t!("automation.workspace_not_ready"),
            Self::WorkspaceMissing { path } => t!("automation.workspace_missing", path = path),
            Self::NoInstruction => t!("automation.no_instruction"),
            Self::NoDays => t!("automation.no_days"),
            Self::TimeOutOfRange => t!("automation.time_out_of_range"),
            Self::MinutesOutOfRange => t!("automation.minutes_out_of_range"),
            Self::HoursOutOfRange => t!("automation.hours_out_of_range"),
            Self::EndBeforeStart => t!("automation.end_before_start"),
            Self::NothingToSend => t!("automation.nothing_to_send"),
            Self::NotFound => t!("automation.not_found"),
            Self::RunNotFound => t!("automation.run_not_found"),
            Self::AlreadyRunning { run } => t!("automation.already_running", run = run),
            Self::DeleteWhileRunning => t!("automation.delete_while_running"),
            Self::DeleteWhileCleaningUp => t!("automation.delete_while_cleaning_up"),
            Self::EventTopicMissing => t!("automation.event_topic_missing"),
            Self::EventTopicTooLong => t!("automation.event_topic_too_long"),
            Self::EventTopicMalformed => t!("automation.event_topic_malformed"),
            Self::EventIdMissing => t!("automation.event_id_missing"),
            Self::EventIdTooLong => t!("automation.event_id_too_long"),
            Self::EventIdInvalid => t!("automation.event_id_invalid"),
            Self::EventFilterNotObject => t!("automation.event_filter_not_object"),
            Self::Other => return None,
        };
        Some(text.into_owned())
    }

    /// 控制调用里报这个问题用的错误码：找不到的是 `NotFound`，撞上正在跑的是 `Busy`，
    /// 还没分配工作区是 `FailedPrecondition`，别的是交来的配置不对（`InvalidParams`）。
    pub fn code(&self) -> super::control::ControlErrorCode {
        use super::control::ControlErrorCode;
        match self {
            Self::NotFound | Self::RunNotFound => ControlErrorCode::NotFound,
            Self::AlreadyRunning { .. }
            | Self::DeleteWhileRunning
            | Self::DeleteWhileCleaningUp => ControlErrorCode::Busy,
            Self::WorkspaceNotReady | Self::WorkspaceMissing { .. } | Self::Other => {
                ControlErrorCode::FailedPrecondition
            }
            _ => ControlErrorCode::InvalidParams,
        }
    }
}

/// 按当前语言说；[`AutomationProblem::Other`] 说成种类名。
impl std::fmt::Display for AutomationProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.text() {
            Some(text) => f.write_str(&text),
            None => f.write_str("automation problem"),
        }
    }
}

impl std::error::Error for AutomationProblem {}

/// 一次自动化运行没成的原因种类。运行记录里存这个种类（`error_code`），底层报错原样存在
/// `error` 里当细节；客户端用 [`AutomationRunFailure::describe`] 按自己的语言说。
/// 没有种类的旧记录只有 `error`，原样显示。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunFailure {
    /// 上一次还在跑，这次跳过了。
    PreviousRunActive,
    /// 指令和触发内容都是空的。
    NothingToSend,
    /// 运行时还没给自动化分配工作区。
    WorkspaceNotReady,
    /// 工作区不存在或不是目录（细节是路径）。
    WorkspaceMissing,
    /// 自动化的配置不对。
    AutomationInvalid,
    /// 智能体用不了（细节是原因）。
    AgentUnavailable,
    /// 运行没有绑定会话，结果未知。
    SessionMissing,
    /// 运行时重启后会话没了，结果未知，没有自动重放。
    SessionLostOnRestart,
    /// 会话已经属于别的运行（细节是那条运行的 id）。
    SessionConflict,
    /// 运行时重启发生在投递当中，结果未知，没有自动重放。
    RestartDuringDelivery,
    /// 智能体的执行上下文没存下来（细节是原因）。
    ContextNotSaved,
    /// 智能体的引擎和这次运行记下的不一样了。
    EngineChanged,
    /// 智能体没能启动（细节是原因）。
    StartFailed,
    /// 多次投递都没成（细节是最后一次的原因）。
    DeliveryFailed,
    /// 智能体在投递前退出了。
    ExitedBeforeDelivery,
    /// 会话丢了，结果未知，没有自动重放。
    SessionLost,
    /// 智能体退出了（细节是退出原因）。
    AgentExited,
    /// 这一轮没有正常完成，没有更具体的原因。
    TurnFailed,
    /// 这一轮的回复中途断了。
    TurnInterrupted,
    /// 用完了这一轮的最大令牌数。
    TurnMaxTokens,
    /// 用完了这一轮的最大请求数。
    TurnMaxTurnRequests,
    /// 智能体拒绝继续。
    TurnRefused,
    /// 这一轮结束了但没有结果。
    TurnWithoutOutcome,
    /// 不是能执行的 Shell 动作。
    NotShellAction,
    /// 命令没能启动（细节是原因）。
    CommandStartFailed,
    /// 等命令结束时出错（细节是原因）。
    CommandWaitFailed,
    /// 命令以非零退出码结束（细节是退出码和 stderr）。
    CommandFailed,
    /// 比这个客户端新的运行时记的种类。
    #[serde(other)]
    Other,
}

impl AutomationRunFailure {
    /// 按当前语言说这个原因；[`AutomationRunFailure::Other`] 没有自己的话，给 `None`。
    pub fn text(&self) -> Option<String> {
        let text = match self {
            Self::PreviousRunActive => t!("automation_run.previous_run_active"),
            Self::NothingToSend => t!("automation.nothing_to_send"),
            Self::WorkspaceNotReady => t!("automation.workspace_not_ready"),
            Self::WorkspaceMissing => t!("automation_run.workspace_missing"),
            Self::AutomationInvalid => t!("automation_run.automation_invalid"),
            Self::AgentUnavailable => t!("automation_run.agent_unavailable"),
            Self::SessionMissing => t!("automation_run.session_missing"),
            Self::SessionLostOnRestart => t!("automation_run.session_lost_on_restart"),
            Self::SessionConflict => t!("automation_run.session_conflict"),
            Self::RestartDuringDelivery => t!("automation_run.restart_during_delivery"),
            Self::ContextNotSaved => t!("automation_run.context_not_saved"),
            Self::EngineChanged => t!("automation_run.engine_changed"),
            Self::StartFailed => t!("automation_run.start_failed"),
            Self::DeliveryFailed => t!("automation_run.delivery_failed"),
            Self::ExitedBeforeDelivery => t!("automation_run.exited_before_delivery"),
            Self::SessionLost => t!("automation_run.session_lost"),
            Self::AgentExited => t!("automation_run.agent_exited"),
            Self::TurnFailed => t!("automation_run.turn_failed"),
            Self::TurnInterrupted => return Some(TurnFailure::Interrupted.text()),
            Self::TurnMaxTokens => return Some(TurnFailure::MaxTokens.text()),
            Self::TurnMaxTurnRequests => {
                return Some(TurnFailure::MaxTurnRequests.text());
            }
            Self::TurnRefused => return Some(TurnFailure::Refused.text()),
            Self::TurnWithoutOutcome => t!("automation_run.turn_without_outcome"),
            Self::NotShellAction => t!("automation_run.not_shell_action"),
            Self::CommandStartFailed => t!("automation_run.command_start_failed"),
            Self::CommandWaitFailed => t!("automation_run.command_wait_failed"),
            Self::CommandFailed => t!("automation_run.command_failed"),
            Self::Other => return None,
        };
        Some(text.into_owned())
    }

    /// 原因连同细节说成一句。不认识的种类只说细节，细节也没有就说「运行失败」。
    pub fn describe(&self, detail: Option<&str>) -> String {
        let detail = detail.map(str::trim).filter(|detail| !detail.is_empty());
        match (self.text(), detail) {
            (Some(text), Some(detail)) => {
                t!("automation_run.with_detail", reason = text, detail = detail).into_owned()
            }
            (Some(text), None) => text,
            (None, Some(detail)) => detail.to_string(),
            (None, None) => t!("automation_run.other").into_owned(),
        }
    }

    /// 这一轮没完成的原因记成哪种运行失败。
    pub fn for_turn(failure: TurnFailure) -> Self {
        match failure {
            TurnFailure::Interrupted => Self::TurnInterrupted,
            TurnFailure::MaxTokens => Self::TurnMaxTokens,
            TurnFailure::MaxTurnRequests => Self::TurnMaxTurnRequests,
            TurnFailure::Refused => Self::TurnRefused,
            TurnFailure::Failed | TurnFailure::Other => Self::TurnFailed,
        }
    }

    /// 触发时发现的自动化问题记成哪种运行失败，以及要存的细节。
    pub fn for_problem(problem: &AutomationProblem) -> (Self, Option<String>) {
        match problem {
            AutomationProblem::NothingToSend => (Self::NothingToSend, None),
            AutomationProblem::WorkspaceNotReady => (Self::WorkspaceNotReady, None),
            AutomationProblem::WorkspaceMissing { path } => {
                (Self::WorkspaceMissing, Some(path.clone()))
            }
            _ => (Self::AutomationInvalid, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_travels_as_kind_and_fields() {
        let reason = ControlReason::RepositoryNotInProject {
            name: "api".to_string(),
        };
        let wire = serde_json::to_value(&reason).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "kind": "repository_not_in_project", "name": "api" })
        );
        assert_eq!(
            serde_json::from_value::<ControlReason>(wire).unwrap(),
            reason
        );
    }

    #[test]
    fn automation_problem_nests_under_its_reason() {
        let reason = ControlReason::Automation {
            problem: AutomationProblem::WorkspaceMissing {
                path: "/w".to_string(),
            },
        };
        let wire = serde_json::to_value(&reason).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "kind": "automation",
                "problem": { "kind": "workspace_missing", "path": "/w" }
            })
        );
        assert_eq!(
            serde_json::from_value::<ControlReason>(wire).unwrap(),
            reason
        );
        let newer = serde_json::json!({ "kind": "automation", "problem": { "kind": "later" } });
        assert_eq!(
            serde_json::from_value::<ControlReason>(newer).unwrap(),
            ControlReason::Automation {
                problem: AutomationProblem::Other
            }
        );
    }

    #[test]
    fn run_failure_is_a_plain_code_and_unknown_codes_read_as_other() {
        assert_eq!(
            serde_json::to_value(AutomationRunFailure::SessionLost).unwrap(),
            serde_json::json!("session_lost")
        );
        assert_eq!(
            serde_json::from_value::<AutomationRunFailure>(serde_json::json!("later")).unwrap(),
            AutomationRunFailure::Other
        );
    }

    #[test]
    fn run_failure_says_reason_then_detail() {
        assert_eq!(
            AutomationRunFailure::WorkspaceMissing.describe(Some("/w")),
            "The automation's Smelt workspace doesn't exist or isn't a folder: /w"
        );
        assert_eq!(AutomationRunFailure::Other.describe(Some("boom")), "boom");
        assert_eq!(AutomationRunFailure::Other.describe(None), "The run failed");
    }

    #[test]
    fn newer_kinds_read_as_other() {
        let wire = serde_json::json!({ "kind": "from_the_future", "x": 1 });
        assert_eq!(
            serde_json::from_value::<ControlReason>(wire).unwrap(),
            ControlReason::Other
        );
    }
}
