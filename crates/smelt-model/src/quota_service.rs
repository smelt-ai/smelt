//! 额度的账户标识和 `quota.*` 的线上数据。查询和缓存在运行时。

use serde::{Deserialize, Serialize};

use crate::provider_quota::ProviderQuotaData;
use crate::agent_kind::ConversationAgentKind;

/// 一份额度对应的账户。
///
/// 多数 agent 一个 CLI 对一个账户，`provider` 为空即可。Pi 不是：它底下挂着若干
/// provider，各自是独立账户，所以键必须带上 provider 身份，否则几家会互相覆盖。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuotaTarget {
    pub agent: ConversationAgentKind,
    /// 该 agent 下的 provider id（目前只有 Pi 用到）。`None` = agent 自己的账户。
    pub provider: Option<&'static str>,
}

impl QuotaTarget {
    pub fn agent(agent: ConversationAgentKind) -> Self {
        Self {
            agent,
            provider: None,
        }
    }

    pub fn pi(provider: &'static str) -> Self {
        Self {
            agent: ConversationAgentKind::Pi,
            provider: Some(provider),
        }
    }

    /// 缓存键与 UI 元素 id。单账户的形式保持和过去一致（`codex`），所以升级后
    /// 旧缓存照样认得出来。
    pub fn id(self) -> String {
        match self.provider {
            Some(provider) => format!("{}:{provider}", self.agent.id()),
            None => self.agent.id().to_string(),
        }
    }

    /// 缓存里的键反解。provider 段必须落在当前白名单里，否则这条缓存已经没有
    /// 对应的查询入口，直接丢弃。
    pub fn from_id(id: &str) -> Option<Self> {
        let Some((agent, provider)) = id.split_once(':') else {
            return ConversationAgentKind::from_id(id).map(Self::agent);
        };
        let agent = ConversationAgentKind::from_id(agent)?;
        if agent != ConversationAgentKind::Pi {
            return None;
        }
        crate::pi_quota::pi_quota_provider(provider).map(|provider| Self::pi(provider.id))
    }

    /// 侧栏文案里的名字。
    pub fn label(self) -> &'static str {
        self.agent.short_label()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuotaStatus {
    pub quota: Option<ProviderQuotaData>,
    pub checked_at_ms: u64,
    pub error: Option<String>,
}

/// 线上用 [`QuotaTarget::id`] 那串。
impl Serialize for QuotaTarget {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.id())
    }
}

impl<'de> Deserialize<'de> for QuotaTarget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::from_id(&id)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown quota target `{id}`")))
    }
}

/// 客户端启动时要的：当前全部账户（决定展示顺序）、缓存里的结果、要不要立刻查一次。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub targets: Vec<QuotaTarget>,
    pub statuses: Vec<(QuotaTarget, QuotaStatus)>,
    /// 缓存新鲜且覆盖了全部账户：不必启动首查。
    pub fresh: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaRefreshParams {
    /// `None`：全部账户；否则只查这一个。
    pub target: Option<QuotaTarget>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaRefreshResult {
    /// 当前全部账户。全量刷新时客户端据此更新展示顺序（新登录的 Pi provider 由此出现）。
    pub targets: Vec<QuotaTarget>,
    /// 这一轮实际查到的账户的结果。
    pub statuses: Vec<(QuotaTarget, QuotaStatus)>,
}
