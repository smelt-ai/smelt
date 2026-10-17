//! 能查额度的 Pi 订阅 provider 白名单。读凭据、查询在运行时。


/// `auth.json` 里哪个字段才是额度接口要的令牌。
///
/// 这个区分不是风格问题：Pi 的 `github-copilot` 把 GitHub OAuth 令牌存在
/// `refresh`，`access` 放的是 `copilot_internal/v2/token` 换来的短期推理令牌
/// （`tid=…`），拿后者打额度接口固定 401。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiTokenField {
    /// 访问令牌，受 `expires` 约束。
    Access,
    /// 刷新令牌，长期有效，不看 `expires`。
    Refresh,
}

/// Pi 下一家能查额度的 OAuth provider。
pub struct PiQuotaProvider {
    /// `auth.json` 的键，也是 Pi 的 provider id。展示名由菜单那边的协议分组提供，
    /// 这里不再重复维护一份。
    pub id: &'static str,
    pub token_field: PiTokenField,
}

/// Pi 的 OAuth provider 里，有已知只读额度接口的那些。顺序即侧栏展示顺序。
///
/// 没列进来的（`kimi-coding`、`openrouter`、`meta`、`radius`、`openai-chatgpt`）
/// 不是不支持 OAuth，而是我们没有可复用的额度接口；以后要接只需加 parser 和
/// 这里的一行。
pub static PI_QUOTA_PROVIDERS: &[PiQuotaProvider] = &[
    PiQuotaProvider {
        id: "anthropic",
        // Pi 用的是 Claude Code 的 client_id 和 scope，令牌能直接查 claude.ai 额度。
        token_field: PiTokenField::Access,
    },
    PiQuotaProvider {
        id: "github-copilot",
        token_field: PiTokenField::Refresh,
    },
    PiQuotaProvider {
        id: "openai-codex",
        token_field: PiTokenField::Access,
    },
    PiQuotaProvider {
        id: "xai",
        // Pi 的 xAI 登录带 `grok-cli:access` scope，和 Grok CLI 的会话令牌同源。
        token_field: PiTokenField::Access,
    },
];

pub fn pi_quota_provider(id: &str) -> Option<&'static PiQuotaProvider> {
    PI_QUOTA_PROVIDERS.iter().find(|provider| provider.id == id)
}
