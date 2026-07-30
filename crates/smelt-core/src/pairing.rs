//! 配对码格式：桌面生成、手机解析。
//!
//! 放在 smelt-core 而不是 `smelt-iroh`，是因为 GUI 只需要拼这一个字符串，
//! 不该为此把整个 iroh 依赖（实测让二进制大 ~10MB）拖进来。生成方（桌面设置页）
//! 和解析方（Flutter app）分属两套代码，格式必须有唯一一份权威定义可引用。

/// iroh 配对码：携带宿主、网关鉴权和用户选择的 relay 配置。
///
/// 刻意沿用 query string 带 token 的写法，跟现有 `http(s)://host/?token=` 的配对
/// 链接保持一致——手机侧解析器只要多认一个 scheme，不用改结构。
///
/// `endpoint_id` 放在 host 位而不是 path，因为它就是「连谁」，语义等价于主机名。
///
/// 注意两半缺一不可：`endpoint_id` 只让人连得上网关，能不能操作由 token 决定。
pub fn iroh_pairing_uri(
    endpoint_id: &str,
    token: &str,
    relay_url: &str,
    relay_token: &str,
) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("token", token);
    query.append_pair("relay", relay_url);
    if !relay_token.is_empty() {
        query.append_pair("relay_token", relay_token);
    }
    format!("smelt+iroh://{endpoint_id}/?{}", query.finish())
}

/// iroh 配对码的 scheme，解析方用来分流。
pub const IROH_PAIRING_SCHEME: &str = "smelt+iroh";

/// 解析结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrohPairing {
    pub endpoint_id: String,
    pub token: String,
    pub relay_url: String,
    pub relay_token: String,
}

/// 解析 `smelt+iroh://` 配对码。relay 是必填项；没有它就不得使用任何默认服务。
///
/// 和 [`iroh_pairing_uri`] 放在一起，是为了让「怎么生成」和「怎么解析」
/// 改的时候必然被同一次编辑覆盖到。
///
/// 不用 `url` crate：`smelt+iroh` 是自定义 scheme，通用解析器对非特殊 scheme 的
/// host 处理各家不一（有的直接当 opaque path），为这点格式引入依赖不划算。
pub fn parse_iroh_pairing_uri(uri: &str) -> Result<IrohPairing, String> {
    let prefix = format!("{IROH_PAIRING_SCHEME}://");
    let rest = uri
        .trim()
        .strip_prefix(&prefix)
        .ok_or_else(|| format!("不是 {IROH_PAIRING_SCHEME} 配对码"))?;

    let (host, query) = match rest.split_once('?') {
        Some((h, q)) => (h, q),
        None => (rest, ""),
    };
    // host 后面允许有 `/`（我们自己就生成 `/?token=`），去掉即可。
    let endpoint_id = host.trim_end_matches('/');
    if endpoint_id.is_empty() {
        return Err("配对码缺少 endpoint id".to_string());
    }

    let fields = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    let token = fields.get("token").map(String::as_str).unwrap_or("");
    if token.is_empty() {
        // 没有 token 的码连上也什么都干不了，与其让用户在连接页看到一个
        // 莫名其妙的 401，不如在扫码这一步就说清楚。
        return Err("配对码缺少 token".to_string());
    }
    let relay_url = fields.get("relay").map(String::as_str).unwrap_or("");
    if relay_url.is_empty() {
        return Err("配对码缺少 relay 地址，请在 Mac 上重新生成".to_string());
    }

    Ok(IrohPairing {
        endpoint_id: endpoint_id.to_string(),
        token: token.to_string(),
        relay_url: relay_url.to_string(),
        relay_token: fields.get("relay_token").cloned().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_both_halves() {
        let uri = iroh_pairing_uri(
            "abc123",
            "tok456",
            "https://relay.example.test",
            "relay-secret",
        );
        assert!(uri.starts_with("smelt+iroh://"), "{uri}");
        assert!(uri.contains("abc123"), "{uri}");
        assert!(uri.contains("token=tok456"), "{uri}");
        assert!(
            uri.contains("relay=https%3A%2F%2Frelay.example.test"),
            "{uri}"
        );
        assert!(uri.contains("relay_token=relay-secret"), "{uri}");
    }

    #[test]
    fn scheme_matches_generated_uri() {
        // 两个常量各写各的迟早对不上，这里钉住。
        let uri = iroh_pairing_uri("id", "t", "https://relay.test", "");
        assert!(uri.starts_with(&format!("{IROH_PAIRING_SCHEME}://")));
    }

    #[test]
    fn parse_round_trips_generated_uri() {
        // 生成与解析必须互逆，否则桌面出的码手机认不出来。
        let uri = iroh_pairing_uri(
            "abc123",
            "tok456",
            "https://relay.example.test:8443",
            "relay&secret",
        );
        let parsed = parse_iroh_pairing_uri(&uri).expect("应能解析自己生成的码");
        assert_eq!(parsed.endpoint_id, "abc123");
        assert_eq!(parsed.token, "tok456");
        assert_eq!(parsed.relay_url, "https://relay.example.test:8443");
        assert_eq!(parsed.relay_token, "relay&secret");
    }

    #[test]
    fn parse_rejects_other_schemes() {
        // http 配对码走的是另一条分支，不能被 iroh 解析器吞掉。
        assert!(parse_iroh_pairing_uri("https://example.com/?token=t").is_err());
    }

    #[test]
    fn parse_rejects_missing_token() {
        assert!(parse_iroh_pairing_uri("smelt+iroh://abc123/?relay=https%3A%2F%2Fr").is_err());
    }

    #[test]
    fn parse_rejects_missing_endpoint() {
        assert!(parse_iroh_pairing_uri("smelt+iroh:///?token=t&relay=https%3A%2F%2Fr").is_err());
    }

    #[test]
    fn parse_rejects_missing_relay_without_falling_back() {
        assert!(parse_iroh_pairing_uri("smelt+iroh://abc123/?token=t").is_err());
    }
}
