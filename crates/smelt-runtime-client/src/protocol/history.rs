//! 各家 agent 历史会话的业务方法。历史存档在运行时那台机器上（`~/.claude` 等），
//! 扫描、解析、删除都由运行时执行；客户端只拿列表和对话内容。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod, EmptyParams};

macro_rules! history_calls {
    ($( $(#[$doc:meta])* $call:ident => $method:ident ($params:ty) -> $result:ty; )+) => {
        $(
            $(#[$doc])*
            pub enum $call {}

            impl ControlCall for $call {
                type Params = $params;
                type Result = $result;

                const METHOD: ControlMethod = ControlMethod::$method;
            }
        )+
    };
}

history_calls! {
    /// 某家 agent（及 workspace profile）在某个项目下的历史会话，最近活跃的在前。
    /// 用户改过的名字已经贴好。
    HistoryListCall => HistoryList(HistoryListParams) -> HistoryListResult;
    /// 一份历史会话的对话内容。存档已被删或格式认不出时 `detail` 为 `None`。
    HistoryDetailCall => HistoryDetail(HistoryDetailParams) -> HistoryDetailResult;
    /// 删除一份历史存档，连同它的自定义标题。只删这个项目的历史列表里确实有的那一份。
    HistoryDeleteCall => HistoryDelete(HistoryDeleteParams) -> EmptyParams;
    /// 记下一段历史会话属于哪个产品智能体，以后「继续」时按原来的智能体启动。
    HistoryRememberAgentCall => HistoryRememberAgent(HistoryRememberAgentParams) -> EmptyParams;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryListParams {
    /// 历史来源的稳定标识（`HistorySourceKind::id`），如 `claude`、`antigravity`。
    pub agent: String,
    pub profile_id: Option<String>,
    /// 项目目录。
    pub cwd: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySession {
    /// 存档在运行时那台机器上的路径；查看和删除时原样带回来。
    pub path: String,
    /// agent 认得的 session id，续接时用它。
    pub resume_id: String,
    /// agent 自己生成的标题。
    pub title: String,
    /// 用户在 Smelt 里改的名字。
    pub custom_title: Option<String>,
    pub started_at_ms: Option<i64>,
    pub last_active_at_ms: Option<i64>,
    pub message_count: usize,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryListResult {
    pub sessions: Vec<HistorySession>,
    /// resume_id → 这段会话所属的智能体定义 id。只有用产品智能体开的会话才有。
    pub agent_definition_ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDetailParams {
    pub agent: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryTurn {
    pub is_user: bool,
    pub timestamp_ms: Option<i64>,
    pub text: String,
    /// 这轮 assistant 调用的工具名。
    pub tools: Vec<String>,
    /// 这轮工具入参里出现的文件路径，会话迁移用。
    pub tool_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDetail {
    pub turns: Vec<HistoryTurn>,
    /// 原会话用的模型（读得到才有）。
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDetailResult {
    pub detail: Option<HistoryDetail>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDeleteParams {
    pub agent: String,
    pub profile_id: Option<String>,
    /// 列表所在的项目目录。运行时据此重新列一遍，确认要删的存档确实在里面。
    pub cwd: String,
    pub resume_id: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRememberAgentParams {
    pub agent: String,
    pub profile_id: Option<String>,
    pub resume_id: String,
    pub agent_definition_id: String,
}
