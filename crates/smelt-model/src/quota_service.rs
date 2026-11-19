//! 额度的账户标识和 `quota.*` 的线上数据。查询和缓存在运行时。

use serde::{Deserialize, Serialize};

use crate::agent_kind::ConversationAgentKind;
use crate::provider_quota::ProviderQuotaData;

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

    /// 账户属于哪家服务：agent 自己的账户就是它，Pi 底下的按 provider 归到对应那家
    /// （`pi:github-copilot` 是 Copilot）。
    pub fn service(self) -> ConversationAgentKind {
        self.provider
            .and_then(crate::pi_quota::pi_quota_provider)
            .map_or(self.agent, |provider| provider.service)
    }

    /// 界面上的名字：服务名，凭据不是那家 agent 自己的再带上来源（`Copilot · Pi`）。
    pub fn display_name(self) -> String {
        let service = self.service().short_label();
        if self.service() == self.agent {
            service.to_string()
        } else {
            format!("{service} · {}", self.agent.short_label())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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

/// 线上是 [`QuotaTarget::id`] 那串：agent id，Pi 的是 `pi:<provider>`。
impl schemars::JsonSchema for QuotaTarget {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "QuotaTarget".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
}

/// 客户端启动时要的：开着查询的全部账户（决定展示顺序）和缓存里还不旧的结果。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaSnapshot {
    pub targets: Vec<QuotaTarget>,
    pub statuses: Vec<(QuotaTarget, QuotaStatus)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaRefreshParams {
    /// `None`：后台刷新全部账户；否则是用户点了这一个，可以弹钥匙串窗口。
    pub target: Option<QuotaTarget>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaRefreshResult {
    /// 开着查询的全部账户。全量刷新时客户端据此更新展示顺序（新登录的 Pi provider 由此出现）。
    pub targets: Vec<QuotaTarget>,
    /// 这一轮实际查到的账户的结果。
    pub statuses: Vec<(QuotaTarget, QuotaStatus)>,
}

/// 设置页的一行。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaProviderSetting {
    pub target: QuotaTarget,
    pub enabled: bool,
    /// 钥匙串访问被拒绝，后台不再读。手动刷新这一家后解除。
    pub keychain_denied: bool,
}

/// 全部账户的额度开关，含关掉的。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaSettings {
    pub providers: Vec<QuotaProviderSetting>,
}

impl QuotaSettings {
    /// 开着查询的账户，顺序和列表一致。
    pub fn enabled_targets(&self) -> Vec<QuotaTarget> {
        self.providers
            .iter()
            .filter(|provider| provider.enabled)
            .map(|provider| provider.target)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaSetEnabledParams {
    pub target: QuotaTarget,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pi 底下的账户按 provider 归到对应那家服务，名字带上来源。
    #[test]
    fn pi_targets_belong_to_the_service_of_their_provider() {
        let copilot = QuotaTarget::agent(ConversationAgentKind::Copilot);
        let pi_copilot = QuotaTarget::pi("github-copilot");
        assert_eq!(copilot.service(), ConversationAgentKind::Copilot);
        assert_eq!(pi_copilot.service(), ConversationAgentKind::Copilot);
        assert_eq!(copilot.display_name(), "Copilot");
        assert_eq!(pi_copilot.display_name(), "Copilot · Pi");
        for provider in crate::pi_quota::PI_QUOTA_PROVIDERS {
            assert_ne!(
                QuotaTarget::pi(provider.id).service(),
                ConversationAgentKind::Pi
            );
        }
    }
}
