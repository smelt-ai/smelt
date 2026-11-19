//! 智能体（来源 `smelt.agents`）对外的约定：智能体页和「Agent」设置页的视图 id、跟智能体
//! 开新对话的命令、各台机器的智能体配置（读、订、改），和账户额度（读、订、点一家刷新）。
//!
//! 这里是权威定义。插件之间不互相依赖，插件接口也不认识具体功能：调用方各自照抄用到的
//! id 和字段，注明出处（JS 插件按同样的字符串和 JSON 调）。改这里就是改线上形状，同一次
//! 改动里把照抄的各处一起改掉（`grep -rn 'smelt.agents/' crates plugins`）。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::model::agent_host::AgentHostSettings;
use smelt_plugin_api::model::quota_service::{QuotaStatus, QuotaTarget};
use smelt_plugin_api::{Command, Event, RuntimeKey};

/// 智能体页。打开时的参数都可以不给：
/// `{"machine": …, "agent": "<id>", "tab": "config" | "memory", "new_conversation": true}`。
/// `agent` 打开那个智能体的编辑页（在 `machine` 上，不给是本机），`tab` 是停在哪个页签；
/// `new_conversation` 是从「新对话」过来、还没选默认智能体。
pub(crate) const VIEW: &str = "smelt.agents/main";

/// 跟默认智能体开一段新对话；没有默认就去智能体页选，一个都没有就去建。不带参数，
/// 标题里写着默认智能体（「新对话 · 写作助手」），侧栏顶上的按钮和 ⌘N 都是它。
pub(crate) const NEW_CONVERSATION: Command<(), ()> = Command::new("smelt.agents/new-conversation");

/// 设置里的「Agent」页：本机 Agent CLI、hooks、自定义 Agent、项目「+」菜单的启动项、账户额度
/// 开关。不带参数。
pub(crate) const SETTINGS_VIEW: &str = "smelt.agents/settings";

/// 各台机器眼下的智能体配置，只含收到过推送的机器（[`AgentSettingsState::Absent`] 不会出现）。
/// 配置只有运行时写，这是桌面上的只读缓存；之后的变化订 [`AGENT_SETTINGS_CHANGED`]。
pub(crate) const AGENT_SETTINGS: Command<(), Vec<MachineAgentSettings>> =
    Command::new("smelt.agents/agent-settings");

/// 一台机器的智能体配置换了（整份），或那台机器给不了、移除了。
pub(crate) const AGENT_SETTINGS_CHANGED: Event<MachineAgentSettings> =
    Event::new("smelt.agents/agent-settings-changed");

/// 改一台机器的智能体配置：把 `before` 到 `after` 的改动交给那台机器的运行时，它合并到最新的
/// 那份上再写，交回写好的整份。之后照常推一次 [`AGENT_SETTINGS_CHANGED`]。`before` 取自
/// [`AGENT_SETTINGS`] 或事件里的那份；同一台机器上要连着改几次，后一次以前一次交回的为底。
pub(crate) const EDIT_AGENT_SETTINGS: Command<AgentSettingsEdit, AgentHostSettings> =
    Command::new("smelt.agents/edit-agent-settings");

/// 一台机器的智能体配置。线上是 `{"machine": null | "<id>", "state": "ready", "settings": …}`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct MachineAgentSettings {
    /// 本机是 `null`。
    #[serde(with = "smelt_plugin_api::machine_json")]
    pub(crate) machine: RuntimeKey,
    #[serde(flatten)]
    pub(crate) state: AgentSettingsState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum AgentSettingsState {
    /// 运行时推来的整份配置。
    Ready { settings: AgentHostSettings },
    /// 那台机器给不了（运行时太旧、推来的解不开），原因给人看。
    Unavailable { reason: String },
    /// 没有：机器移除了。调用方丢掉手里那份。
    Absent,
}

/// [`EDIT_AGENT_SETTINGS`] 的参数。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentSettingsEdit {
    /// 本机是 `null`。
    #[serde(with = "smelt_plugin_api::machine_json")]
    pub(crate) machine: RuntimeKey,
    pub(crate) before: AgentHostSettings,
    pub(crate) after: AgentHostSettings,
}

/// 账户额度那一条：每家一枚额度环，点开看全部，点哪家刷新哪家。不带参数，摆在哪归布局。
pub(crate) const QUOTA_VIEW: &str = "smelt.agents/quota";

/// 本机运行时上各账户的额度。之后的变化订 [`QUOTA_CHANGED`]。
pub(crate) const QUOTA: Command<(), Quota> = Command::new("smelt.agents/quota");

/// 额度、查询范围或「查询中」变了，带整份。
pub(crate) const QUOTA_CHANGED: Event<Quota> = Event::new("smelt.agents/quota-changed");

/// 用户点了某家的额度：单查这一家（只有这时运行时才可能弹钥匙串窗口）。不等查完就返回，
/// 结果经 [`QUOTA_CHANGED`] 来。
pub(crate) const REFRESH_QUOTA: Command<RefreshQuota, ()> =
    Command::new("smelt.agents/refresh-quota");

/// 额度现状。账户 id 是 [`QuotaTarget::id`] 那串（`codex`、`pi:openrouter`）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Quota {
    /// 开着查询的账户，也是展示顺序。
    pub(crate) targets: Vec<QuotaTarget>,
    /// 查到过的结果，按 `targets` 的顺序；还没查到的账户不在里面。
    pub(crate) statuses: Vec<(QuotaTarget, QuotaStatus)>,
    /// 正在查的账户。
    pub(crate) refreshing: Vec<QuotaTarget>,
    /// 其中是用户点了才查的（后台定时查的不在这里），界面据此给即时反馈。
    pub(crate) manual: Vec<QuotaTarget>,
}

/// [`REFRESH_QUOTA`] 的参数。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RefreshQuota {
    pub(crate) target: QuotaTarget,
}

// ---- 插槽 ----

/// 智能体详情页上多出来的页签（插槽，见 `smelt_plugin_api` 视图注册表的「插槽」）。填它的是
/// 按实例建的视图，打开一个智能体的详情时每个填的视图建一份，参数是 [`AgentTabArgs`]，第一次
/// `open` 给，之后不换智能体。每个填的视图一个页签，按登记顺序排在「设置」和「记忆」之间，
/// 页签名是视图的标题；没人填就只有这两个页签。
///
/// 画在详情页正文里，宽度跟设置页签一样（居中的一栏），自己负责滚动。
pub(crate) const AGENT_TAB: &str = "smelt.agents/agent-tab";

/// [`AGENT_TAB`] 的参数。线上是 `{"machine": null | "<id>", "agent": "<id>"}`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentTabArgs {
    /// 本机是 `null`。
    #[serde(with = "smelt_plugin_api::machine_json")]
    pub(crate) machine: RuntimeKey,
    /// 那台机器上智能体的 id。
    pub(crate) agent: String,
}
