//! 自定义 provider 的对话协议，以及「问端点要模型列表」这件事的共享类型。
//!
//! **为什么放在这里而不是各自的设置模块。** dsh 和 Pi 的自定义 provider 都由
//! pi-ai 承载，认的是同一个 `Api` 联合类型。两边原先各写死一份候选列表，结果
//! 是加了 `openai-responses` 只进了 dsh、Pi 那边选不到——协议本身没有 harness
//! 之分，列表就不该有两份。
//!
//! 这里刻意只收录 pi-ai 的 `KnownApi`。用户仍可通过直接编辑配置文件用别的值，
//! 界面不拦；但界面**推荐**的必须是我们知道对端认识的。

/// pi-ai 的 `KnownApi`，顺序即界面里的排列顺序：常用的在前。
pub const PROVIDER_API_PROTOCOLS: &[&str] = &[
    "openai-completions",
    "openai-responses",
    "anthropic-messages",
    "google-generative-ai",
    "azure-openai-responses",
    "openai-codex-responses",
    "google-vertex",
    "bedrock-converse-stream",
    "mistral-conversations",
    "pi-messages",
];

/// 界面默认值。绝大多数中转网关都是 OpenAI 兼容的。
pub const DEFAULT_PROVIDER_API: &str = "openai-completions";

/// 规整一个协议值：认识的原样返回，不认识（含空）的退回默认值。
///
/// 用于读配置——文件里可能是手写的、拼错的，或来自更新过的 Pi。让界面显示一个
/// 它自己都选不中的值，比退回默认更让人困惑。
pub fn normalize_provider_api(api: &str) -> &'static str {
    let api = api.trim();
    PROVIDER_API_PROTOCOLS
        .iter()
        .copied()
        .find(|candidate| *candidate == api)
        .unwrap_or(DEFAULT_PROVIDER_API)
}

/// 端点报告的一个模型。
///
/// dsh 的 Node 发现桥和 Pi 的直连发现都产出它：字段是两边的交集，也是配置里
/// 真正要落盘的那几项。
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

/// 自动目录落盘时写入的默认输入能力声明。
///
/// OpenAI 兼容端点不报告模型接受哪些输入类型，Pi 对未声明 `input` 的模型默认
/// `"text"` 并在客户端丢弃图片。统一写 `text` + `image` 把能力判断交给服务端。
pub const INPUT_TEXT_IMAGE: [&str; 2] = ["text", "image"];

impl DiscoveredModel {
    /// 展示用的名字。端点没给就退回 ID。
    pub fn label(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_new_response_protocol_is_offered() {
        assert!(PROVIDER_API_PROTOCOLS.contains(&"openai-responses"));
    }

    #[test]
    fn unknown_protocols_fall_back_to_the_default() {
        assert_eq!(
            normalize_provider_api("openai-responses"),
            "openai-responses"
        );
        assert_eq!(
            normalize_provider_api("  anthropic-messages "),
            "anthropic-messages"
        );
        assert_eq!(normalize_provider_api(""), DEFAULT_PROVIDER_API);
        assert_eq!(
            normalize_provider_api("openai-response"),
            DEFAULT_PROVIDER_API
        );
    }

    #[test]
    fn discovered_model_falls_back_to_its_id() {
        let bare = DiscoveredModel {
            id: "gpt-5".into(),
            name: None,
            context_window: None,
            max_tokens: None,
        };
        assert_eq!(bare.label(), "gpt-5");
        let blank = DiscoveredModel {
            name: Some("   ".into()),
            ..bare
        };
        assert_eq!(blank.label(), "gpt-5");
    }
}
