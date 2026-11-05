//! 自动化的新建、编辑、删除。
//!
//! 自动化在运行时那台机器上，这里一问一答，没有推送：编辑页打开时取一次，保存后用返回的
//! 目录刷新列表。运行时的保存是整条覆盖，编辑时先取完整的那条，只换表单里有的字段，手机
//! 不编辑的（事件触发、已有的 webhook 令牌）原样带回。

use crate::api::catalog::{
    AutomationActionKind, AutomationCatalog, AutomationRunSummary, AutomationSchedule,
};
use crate::api::conversation::ConversationEntry;
use crate::api::error::SmeltError;

/// 编辑页的表单。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationDraft {
    pub name: String,
    pub action: AutomationActionKind,
    /// `Agent` 时用哪个智能体。
    pub agent_definition_id: String,
    /// `Agent` 时交给它的指令；定时触发时必填。
    pub prompt: String,
    /// `Shell` 时的整行命令。
    pub command: String,
    pub schedules: Vec<AutomationSchedule>,
    /// 有没有 HTTP 触发入口。地址带密钥，只在电脑上看。
    pub webhook: bool,
    /// 跑完在应用里通知。
    pub notify: bool,
}

/// 可选的智能体。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationAgentChoice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationEditor {
    pub draft: AutomationDraft,
    /// 这台机器上的智能体。
    pub agents: Vec<AutomationAgentChoice>,
    /// 事件触发的主题。手机上只读，保存时原样带回。
    pub event_topics: Vec<String>,
}

/// 取编辑器。`automation_id` 为空是新建：给一份默认草稿（每天 09:00、第一个智能体）。
pub fn automation_editor(
    runtime_id: String,
    automation_id: Option<String>,
) -> Result<AutomationEditor, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .automation_editor(automation_id.as_deref())
}

/// 保存。`automation_id` 为空是新建。返回保存后的目录，列表直接换上。
pub fn save_automation(
    runtime_id: String,
    automation_id: Option<String>,
    draft: AutomationDraft,
) -> Result<AutomationCatalog, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .save_automation(automation_id.as_deref(), &draft)
}

/// 一次运行的记录：给它的指令、输出或错误、对话。跑完以后会话就没有了，看这里。
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationRunDetail {
    pub run: AutomationRunSummary,
    pub automation_id: String,
    pub automation_name: String,
    /// 这次运行实际交给智能体的指令（固化在运行里，不是自动化现在的定义）。
    pub instructions: Option<String>,
    /// 命令的输出，或智能体最后的回复。
    pub output: Option<String>,
    /// 智能体运行的对话，命令运行没有。
    pub transcript: Vec<ConversationEntry>,
}

/// 一条自动化最近的运行，新的在前。运行时每条最多留 50 次。
pub fn automation_runs(
    runtime_id: String,
    automation_id: String,
) -> Result<Vec<AutomationRunSummary>, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .automation_runs(&automation_id)
}

pub fn automation_run_detail(
    runtime_id: String,
    run_id: String,
) -> Result<AutomationRunDetail, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .automation_run_detail(&run_id)
}

/// 删除，连同它的运行记录。返回删除后的目录。
pub fn delete_automation(
    runtime_id: String,
    automation_id: String,
) -> Result<AutomationCatalog, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .catalog
        .delete_automation(&automation_id)
}
