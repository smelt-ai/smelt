//! ACP 会话里客户端和运行时之间流转的数据：模型和配置项的状态、上下文用量、表单
//! 字段、分叉切点、CLI 诊断。连接 agent、收发 JSON-RPC 的部分在运行时（smelt-core）。

use std::collections::{BTreeMap, HashMap};

use agent_client_protocol::schema::v1::ElicitationContentValue;

use crate::agent_kind::{ConversationAgentKind, TerminalAgentKind};

/// 「分叉副本的切点」：见 `ConversationLaunch::fork_cut`。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AcpForkCut {
    /// 切点用户消息原文。
    pub text: String,
    /// 同文本消息中的第几条（从 0 数起），与 `Rewind` 的 occurrence 同语义。
    pub occurrence: usize,
}

/// 当前窗口里各桶的 token 估数。Pi 用 chars/4，GUI 再对齐到账单 `used`。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageBreakdown {
    #[serde(default)]
    pub system_prompt: u64,
    #[serde(default)]
    pub tools_definition: u64,
    #[serde(default)]
    pub rules: u64,
    #[serde(default)]
    pub skills: u64,
    #[serde(default)]
    pub mcp_dynamic: u64,
    #[serde(default)]
    pub subagent: u64,
    #[serde(default)]
    pub summarized: u64,
    #[serde(default)]
    pub conversation: u64,
}

impl ContextUsageBreakdown {
    pub fn occupied(&self) -> u64 {
        self.system_prompt
            + self.tools_definition
            + self.rules
            + self.skills
            + self.mcp_dynamic
            + self.subagent
            + self.summarized
            + self.conversation
    }

    /// 各桶加起来对齐到账单 used：少了补 Conversation，多了按比例缩。
    pub fn aligned_to_used(mut self, used: u64) -> Self {
        let occupied = self.occupied();
        if occupied == 0 {
            self.conversation = used;
            return self;
        }
        if occupied == used {
            return self;
        }
        if occupied < used {
            self.conversation += used - occupied;
            return self;
        }
        let scale = used as f64 / occupied as f64;
        let mut next = Self {
            system_prompt: (self.system_prompt as f64 * scale).round() as u64,
            tools_definition: (self.tools_definition as f64 * scale).round() as u64,
            rules: (self.rules as f64 * scale).round() as u64,
            skills: (self.skills as f64 * scale).round() as u64,
            mcp_dynamic: (self.mcp_dynamic as f64 * scale).round() as u64,
            subagent: (self.subagent as f64 * scale).round() as u64,
            summarized: (self.summarized as f64 * scale).round() as u64,
            conversation: (self.conversation as f64 * scale).round() as u64,
        };
        let aligned = next.occupied();
        if aligned < used {
            next.conversation += used - aligned;
        } else if aligned > used {
            next.conversation = next.conversation.saturating_sub(aligned - used);
        }
        next
    }
}

/// 一组模型候选。ACP 的 `select` 允许按 provider/厂商分组，保留这个边界让
/// 客户端能分别呈现 Provider 与 Model，而不是把两者压扁成一条难找的菜单。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelProviderGroup {
    /// 协议分组 id；仅作稳定识别，切换仍写模型选项值。
    pub id: String,
    /// 用户可见的 provider/分组名。
    pub name: String,
    /// 此 provider 下的 `(值 id, 模型名)`。
    pub options: Vec<(String, String)>,
}

/// 模型选择状态：UI 拿它渲染「当前模型」胶囊和下拉候选。全是纯 String/Vec
/// 字段，没有 agent_client_protocol 的 schema 类型，直接可以序列化进
/// acp_session 的 wire 快照，不用另造一份 View 类型。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelState {
    /// ACP 配置项 id。写回时必须用它，不能假定各 adapter 都叫 `model`。
    pub config_id: String,
    /// 当前模型的协议值。部分 agent 用 `provider/model` 编码，UI 需要它区分同名模型。
    #[serde(default)]
    pub current_value: String,
    /// 当前模型的人类可读名（`Claude Sonnet 4.5`）。
    pub current_name: String,
    /// 可选模型：(值 id, 人类可读名)。空 = agent 没给候选，UI 就只显示不给切。
    pub options: Vec<(String, String)>,
    /// ACP 提供的 provider/模型分组。旧 adapter 的平铺候选为空，UI 回退单一模型入口。
    #[serde(default)]
    pub provider_groups: Vec<ModelProviderGroup>,
}

/// 一项由 ACP agent 声明的可选会话配置。模型单独显示，避免和输入栏的模型入口重复。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionConfigState {
    pub config_id: String,
    pub name: String,
    pub description: Option<String>,
    pub current_name: String,
    pub options: Vec<(String, String)>,
    /// `Some` 表示这是布尔开关；`None` 表示 select。旧快照缺这个字段按 select 读。
    #[serde(default)]
    pub boolean: Option<bool>,
}

/// 写入 `session/set_config_option` 的值。select 走值 id，开关走 bool。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigValue {
    Select(String),
    Boolean(bool),
}

/// 表单字段的 UI 无关简化模型（schema 细节收在本模块，视图只见这个）。
pub struct ElicitField {
    /// accept 回填时的 key（schema properties 的键名）。
    pub key: String,
    pub title: String,
    /// 只有 schema 的 required 字段才会阻塞整张表单提交。
    pub required: bool,
    /// 允许用户不点任何选项、自己输入答案。只对宿主「选择题」卡片开启：
    /// 题目工具的合同是「答案必须来自人类」，选项只是便利，不能把人锁死
    /// 在预设里。ACP schema 的 enum 字段受 agent 侧约束，恒为 false。
    pub allow_custom_input: bool,
    pub kind: ElicitFieldKind,
}

pub enum ElicitFieldKind {
    /// 单选：点一个按钮。布尔字段也翻译成 是/否 两个选项。
    Select(Vec<ElicitOption>),
    /// 多选：可切换多个再提交。
    MultiSelect(Vec<ElicitOption>),
    /// 自由文本。secret 只影响客户端显示，协议回填仍是字符串。
    Text { secret: bool },
    /// 需要用户在浏览器完成的外部步骤。
    ExternalUrl(String),
}

pub struct ElicitOption {
    pub value: ElicitationContentValue,
    pub label: String,
}

/// 一个 CLI 或运行时可执行文件的本机诊断结果。
///
/// `path` 为 `None` 代表 Smelt 无法提供该运行时；找到但 `--version` 失败时仍
/// 保留路径，并将原因写入 `error`，避免把损坏的 shim 误报成「未安装」。对于
/// 首次使用才物化的内置运行时，`path` 是它将落盘的目标入口。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeExecutable {
    pub program: String,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

impl RuntimeExecutable {
    pub fn is_available(&self) -> bool {
        self.path.is_some()
    }
}

/// Agent CLI 运行环境的只读诊断快照。探测在运行时那台机器上做（`agent.cli.inspect`），
/// 线上按各家的稳定 `id` 传；认不出的 id（对端版本更新）丢掉。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(into = "AcpRuntimeDiagnosticsWire", from = "AcpRuntimeDiagnosticsWire")]
pub struct AcpRuntimeDiagnostics {
    terminal: HashMap<TerminalAgentKind, RuntimeExecutable>,
    acp: HashMap<ConversationAgentKind, RuntimeExecutable>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct AcpRuntimeDiagnosticsWire {
    #[serde(default)]
    terminal: BTreeMap<String, RuntimeExecutable>,
    #[serde(default)]
    acp: BTreeMap<String, RuntimeExecutable>,
}

impl From<AcpRuntimeDiagnostics> for AcpRuntimeDiagnosticsWire {
    fn from(diagnostics: AcpRuntimeDiagnostics) -> Self {
        Self {
            terminal: diagnostics
                .terminal
                .into_iter()
                .map(|(kind, runtime)| (kind.id().to_string(), runtime))
                .collect(),
            acp: diagnostics
                .acp
                .into_iter()
                .map(|(kind, runtime)| (kind.id().to_string(), runtime))
                .collect(),
        }
    }
}

impl From<AcpRuntimeDiagnosticsWire> for AcpRuntimeDiagnostics {
    fn from(wire: AcpRuntimeDiagnosticsWire) -> Self {
        Self {
            terminal: wire
                .terminal
                .into_iter()
                .filter_map(|(id, runtime)| Some((TerminalAgentKind::from_id(&id)?, runtime)))
                .collect(),
            acp: wire
                .acp
                .into_iter()
                .filter_map(|(id, runtime)| Some((ConversationAgentKind::from_id(&id)?, runtime)))
                .collect(),
        }
    }
}

impl AcpRuntimeDiagnostics {
    /// 记录终端 CLI 的探测结果。索引直接使用注册表种类，新增 CLI 不需要再给
    /// 诊断快照增加字段和匹配分支。
    pub fn record_terminal(&mut self, agent: TerminalAgentKind, runtime: RuntimeExecutable) {
        self.terminal.insert(agent, runtime);
    }

    /// 记录 ACP agent 自己的运行时探测结果。它优先于同名终端 CLI：例如 Smelt
    /// 可以内置 Pi 对话运行时，同时仍单独探测用户是否安装了 Pi 终端 CLI。
    pub fn record_agent(&mut self, agent: ConversationAgentKind, runtime: RuntimeExecutable) {
        self.acp.insert(agent, runtime);
    }

    /// 按 ACP agent 种类索引诊断信息。
    ///
    /// 返回 None 表示这份快照没有记录该 agent。完整探测会覆盖注册表中的每一项；
    /// 调用方应把缺项视为不可用，而不是把注册遗漏伪装成已安装。
    pub fn for_agent(&self, agent: ConversationAgentKind) -> Option<&RuntimeExecutable> {
        self.acp.get(&agent).or_else(|| {
            agent
                .terminal()
                .and_then(|terminal| self.for_terminal(terminal))
        })
    }

    /// 按终端 agent 种类索引诊断信息。返回 None 表示快照没有记录该项。
    pub fn for_terminal(&self, agent: TerminalAgentKind) -> Option<&RuntimeExecutable> {
        self.terminal.get(&agent)
    }
}
