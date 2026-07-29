//! 配对码格式：桌面生成、手机解析。
//!
//! 放在 smelt-core 而不是 `smelt-iroh`，是因为 GUI 只需要拼这一个字符串，
//! 不该为此把整个 iroh 依赖（实测让二进制大 ~10MB）拖进来。生成方（桌面设置页）
//! 和解析方（Flutter app）分属两套代码，格式必须有唯一一份权威定义可引用。

/// iroh 配对码：`smelt+iroh://<endpoint_id>/?token=<token>`。
///
/// 刻意沿用 query string 带 token 的写法，跟现有 `http(s)://host/?token=` 的配对
/// 链接保持一致——手机侧解析器只要多认一个 scheme，不用改结构。
///
/// `endpoint_id` 放在 host 位而不是 path，因为它就是「连谁」，语义等价于主机名。
///
/// 注意两半缺一不可：`endpoint_id` 只让人连得上网关，能不能操作由 token 决定。
pub fn iroh_pairing_uri(endpoint_id: &str, token: &str) -> String {
    format!("smelt+iroh://{endpoint_id}/?token={token}")
}

/// iroh 配对码的 scheme，解析方用来分流。
pub const IROH_PAIRING_SCHEME: &str = "smelt+iroh";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carries_both_halves() {
        let uri = iroh_pairing_uri("abc123", "tok456");
        assert!(uri.starts_with("smelt+iroh://"), "{uri}");
        assert!(uri.contains("abc123"), "{uri}");
        // token 必须在，否则扫出来的码连不上网关的鉴权
        assert!(uri.ends_with("?token=tok456"), "{uri}");
    }

    #[test]
    fn scheme_matches_generated_uri() {
        // 两个常量各写各的迟早对不上，这里钉住。
        let uri = iroh_pairing_uri("id", "t");
        assert!(uri.starts_with(&format!("{IROH_PAIRING_SCHEME}://")));
    }
}
