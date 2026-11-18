//! 配对串格式：运行时生成，桌面和手机解析。
//!
//! 刻意不依赖 `smelt-iroh`：生成方（运行时）和解析方（桌面、Flutter 手机端）分属几套
//! 代码，格式要有唯一一份轻量的权威定义。
//!
//! `smelt+iroh://<endpoint_id>/?code=<一次性码>[&relay=<自建 relay>]`
//!
//! `endpoint_id` 放在 host 位：它就是「连谁」。默认用公共 relay 和地址发现，只凭
//! endpoint id 就能找到运行时；只有运行时用了自建 relay 时才带 `relay`。

/// 配对串的 scheme，解析方用来分流。
pub const PAIRING_SCHEME: &str = "smelt+iroh";

pub fn pairing_uri(endpoint_id: &str, code: &str, relay_url: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("code", code);
    if let Some(relay) = relay_url {
        query.append_pair("relay", relay);
    }
    format!("{PAIRING_SCHEME}://{endpoint_id}/?{}", query.finish())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pairing {
    pub endpoint_id: String,
    pub code: String,
    pub relay_url: Option<String>,
}

/// 配对串解析不了的原因。按种类给，界面按自己的语言说；`Display` 是英文，给命令行和日志。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingError {
    /// 不是 `smelt+iroh://` 开头。
    NotPairing,
    /// 没有 endpoint id。
    MissingEndpoint,
    /// 没有配对码。
    MissingCode,
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPairing => write!(f, "not a {PAIRING_SCHEME} pairing link"),
            Self::MissingEndpoint => f.write_str("the pairing link has no endpoint id"),
            Self::MissingCode => f.write_str("the pairing link has no pairing code"),
        }
    }
}

impl std::error::Error for PairingError {}

/// 解析配对串。
///
/// 不用 `url::Url` 整体解析：`smelt+iroh` 是自定义 scheme，通用 URL 解析器对非标准
/// scheme 的 authority 处理各家不一。这里手动剥 scheme 和 host，query 用标准解析。
pub fn parse_pairing_uri(uri: &str) -> Result<Pairing, PairingError> {
    let prefix = format!("{PAIRING_SCHEME}://");
    let rest = uri
        .trim()
        .strip_prefix(&prefix)
        .ok_or(PairingError::NotPairing)?;
    let (host, query) = rest.split_once('?').unwrap_or((rest, ""));
    let endpoint_id = host.trim_end_matches('/');
    if endpoint_id.is_empty() {
        return Err(PairingError::MissingEndpoint);
    }
    let mut code = None;
    let mut relay_url = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" if !value.is_empty() => code = Some(value.into_owned()),
            "relay" if !value.is_empty() => relay_url = Some(value.into_owned()),
            _ => {}
        }
    }
    let code = code.ok_or(PairingError::MissingCode)?;
    Ok(Pairing {
        endpoint_id: endpoint_id.to_string(),
        code,
        relay_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_a_relay() {
        let uri = pairing_uri(
            "abc123",
            "K7QX-M2PD",
            Some("https://relay.example.test:8443"),
        );
        assert!(uri.starts_with("smelt+iroh://abc123/?"), "{uri}");
        let parsed = parse_pairing_uri(&uri).unwrap();
        assert_eq!(parsed.endpoint_id, "abc123");
        assert_eq!(parsed.code, "K7QX-M2PD");
        assert_eq!(
            parsed.relay_url.as_deref(),
            Some("https://relay.example.test:8443")
        );

        let uri = pairing_uri("abc123", "K7QX-M2PD", None);
        assert!(!uri.contains("relay"), "{uri}");
        assert_eq!(parse_pairing_uri(&uri).unwrap().relay_url, None);
    }

    #[test]
    fn rejects_other_schemes_and_missing_parts() {
        assert_eq!(
            parse_pairing_uri("https://example.com/?code=c"),
            Err(PairingError::NotPairing)
        );
        assert_eq!(
            parse_pairing_uri("smelt+iroh://abc123/"),
            Err(PairingError::MissingCode)
        );
        assert_eq!(
            parse_pairing_uri("smelt+iroh:///?code=c"),
            Err(PairingError::MissingEndpoint)
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let parsed = parse_pairing_uri("  smelt+iroh://abc/?code=x \n").unwrap();
        assert_eq!(parsed.endpoint_id, "abc");
        assert_eq!(parsed.code, "x");
    }
}
