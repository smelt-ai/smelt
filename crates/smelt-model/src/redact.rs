//! 给人看的文本打码。
//!
//! MCP 服务器的地址和命令行、登录失败的输出、Pi 的通知里常夹着令牌：`?api_key=…`、
//! `--token xxx`、`-H "Authorization: Bearer …"`、`GITHUB_TOKEN=…`。这些文本只用来展示，
//! 显示前统一过一遍这里；真正的配置原样留在文件里。
//!
//! 键名规则只有一份（[`is_sensitive_key`]），运行时调试载荷的打码也用它。

/// 打码后的占位。
pub const MASK: &str = "***";

/// 去掉非字母数字再转小写：`X-Api-Key`、`api_key`、`--api-key` 都归成 `apikey`。
pub fn normalized_sensitive_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 键名像凭据：常见名精确匹配，或以 token、secret 这类词结尾（`github_token`、`clientSecret`）。
pub fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalized_sensitive_key(key);
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "auth"
            | "headers"
            | "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "token"
            | "clientsecret"
            | "secret"
            | "password"
            | "passwd"
            | "cookie"
            | "setcookie"
            | "credential"
            | "credentials"
    ) || [
        "authorization",
        "auth",
        "apikey",
        "token",
        "secret",
        "password",
        "passwd",
        "cookie",
        "credential",
    ]
    .iter()
    .any(|suffix| normalized.ends_with(suffix))
}

/// 一段自由文本打码：URL 里的用户名密码和敏感查询参数、敏感命令行参数后面的值、
/// 敏感键的 `KEY=VALUE` / `"key":"value"`、`Authorization:` 头的值、`Bearer` 后面的令牌，
/// 以及 `sk-…`、`ghp_…` 这类一看就是令牌的词。
pub fn redact_secrets(text: &str) -> String {
    Redactor::default().text(text)
}

/// 命令行参数逐个打码。`--token` 和它的值是相邻两个参数，所以要连着看，不能各打各的。
pub fn redact_args(args: &[String]) -> Vec<String> {
    let mut redactor = Redactor::default();
    args.iter().map(|arg| redactor.text(arg)).collect()
}

#[derive(Default)]
struct Redactor {
    /// 上一个词是 `--token`、`Authorization:`、`Bearer` 这类，下一个词就是值。
    mask_next: bool,
}

impl Redactor {
    fn text(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while !rest.is_empty() {
            let word_start = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            out.push_str(&rest[..word_start]);
            rest = &rest[word_start..];
            let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            if word_end > 0 {
                out.push_str(&self.word(&rest[..word_end]));
            }
            rest = &rest[word_end..];
        }
        out
    }

    fn word(&mut self, word: &str) -> String {
        let core_start = word
            .find(|c: char| !is_wrapping_lead(c))
            .unwrap_or(word.len());
        let core_end = word
            .rfind(|c: char| !is_wrapping_trail(c))
            .map_or(core_start, |index| {
                index + word[index..].chars().next().map_or(0, char::len_utf8)
            })
            .max(core_start);
        let (lead, core, trail) = (
            &word[..core_start],
            &word[core_start..core_end],
            &word[core_end..],
        );
        if core.is_empty() {
            return word.to_string();
        }
        if std::mem::take(&mut self.mask_next) && !core.starts_with('-') {
            // `Authorization: Bearer xxx`：打码的是 scheme 后面那个词。
            if is_auth_scheme(core) {
                self.mask_next = true;
                return word.to_string();
            }
            return format!("{lead}{MASK}{trail}");
        }
        // `--token xxx`：值在下一个词。带 `=` 的交给下面的键值规则。
        if core.starts_with('-') && !core.contains('=') {
            self.mask_next = is_sensitive_key(core);
            return word.to_string();
        }
        // `Authorization:`、`X-Api-Key:`、`"token":` 这样的头名或键名，值在下一个词。
        if let Some(name) = core.strip_suffix(':')
            && is_sensitive_key(name)
        {
            self.mask_next = true;
            return word.to_string();
        }
        if core.eq_ignore_ascii_case("bearer") {
            self.mask_next = true;
            return word.to_string();
        }
        let core = mask_url_userinfo(core);
        let core = self.mask_pairs(&core);
        let core = mask_token_like(&core);
        format!("{lead}{core}{trail}")
    }

    /// `key=value`、`key:value`、`"key":"value"`，键名敏感就把值换成占位。
    /// 查询串 `?a=1&api_key=x`、环境变量 `GITHUB_TOKEN=x`、`--token=x` 都走这里。
    fn mask_pairs(&mut self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut copied = 0;
        let mut index = 0;
        while index < bytes.len() {
            let separator = bytes[index];
            if separator != b'=' && separator != b':' {
                index += 1;
                continue;
            }
            // `https://` 不是键值。
            if separator == b':' && text[index + 1..].starts_with("//") {
                index += 1;
                continue;
            }
            let mut key_end = index;
            if key_end > 0 && bytes[key_end - 1] == b'"' {
                key_end -= 1;
            }
            let key_start = text[..key_end]
                .char_indices()
                .rev()
                .find(|(_, c)| !is_key_char(*c))
                .map_or(0, |(at, c)| at + c.len_utf8());
            let key = &text[key_start..key_end];
            let mut value_start = index + 1;
            if bytes.get(value_start) == Some(&b'"') {
                value_start += 1;
            }
            let value_end = text[value_start..]
                .find(is_value_end)
                .map_or(text.len(), |at| value_start + at);
            let value = &text[value_start..value_end];
            if key.is_empty() || value.is_empty() || !is_sensitive_key(key) {
                index += 1;
                continue;
            }
            if is_auth_scheme(value) && value_end == text.len() {
                // `Authorization:Bearer xxx`：令牌在下一个词。
                self.mask_next = true;
            } else {
                out.push_str(&text[copied..value_start]);
                out.push_str(MASK);
                copied = value_end;
            }
            index = value_end;
        }
        out.push_str(&text[copied..]);
        out
    }
}

/// `scheme://user:pass@host/…` 里的 `user:pass` 换成占位。
fn mask_url_userinfo(text: &str) -> String {
    let Some(scheme_end) = text.find("://") else {
        return text.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = text[authority_start..]
        .find(['/', '?', '#'])
        .map_or(text.len(), |at| authority_start + at);
    match text[authority_start..authority_end].rfind('@') {
        Some(at) if at > 0 => format!(
            "{}{MASK}{}",
            &text[..authority_start],
            &text[authority_start + at..]
        ),
        _ => text.to_string(),
    }
}

/// 常见服务的令牌前缀。后面至少跟这么多个令牌字符才算，免得把 `sk-learn` 这种词也打掉。
const TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xapp-",
    "figd_",
    "eyJ",
];
const TOKEN_MIN_BODY: usize = 16;

/// `sk-abc…` 保留前缀，后面的换成占位。
fn mask_token_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while index < text.len() {
        let at_boundary = text[..index]
            .chars()
            .next_back()
            .is_none_or(|c| !is_token_char(c));
        let prefix = if at_boundary {
            TOKEN_PREFIXES
                .iter()
                .find(|prefix| text[index..].starts_with(**prefix))
        } else {
            None
        };
        if let Some(prefix) = prefix {
            let body_start = index + prefix.len();
            let body_end = text[body_start..]
                .find(|c: char| !is_token_char(c))
                .map_or(text.len(), |at| body_start + at);
            if body_end - body_start >= TOKEN_MIN_BODY {
                out.push_str(&text[copied..body_start]);
                out.push_str(MASK);
                copied = body_end;
                index = body_end;
                continue;
            }
        }
        index += text[index..].chars().next().map_or(1, char::len_utf8);
    }
    out.push_str(&text[copied..]);
    out
}

fn is_auth_scheme(word: &str) -> bool {
    ["bearer", "basic", "token", "digest"]
        .iter()
        .any(|scheme| word.eq_ignore_ascii_case(scheme))
}

fn is_wrapping_lead(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | '(' | '[' | '{' | '<')
}

fn is_wrapping_trail(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | ')' | ']' | '}' | '>' | ',' | ';')
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn is_value_end(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '&' | '#' | ';' | ',' | '"' | '\'' | ')' | ']' | '}' | '>'
        )
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn sensitive_key_rules_cover_common_spellings() {
        for key in [
            "Authorization",
            "X-Api-Key",
            "api_key",
            "--api-key",
            "GITHUB_TOKEN",
            "clientSecret",
            "access_token",
            "password",
        ] {
            assert!(is_sensitive_key(key), "{key} 应算敏感");
        }
        for key in ["url", "--port", "name", "--token-file", "timeout"] {
            assert!(!is_sensitive_key(key), "{key} 不该算敏感");
        }
    }

    #[test]
    fn url_userinfo_and_sensitive_query_values_are_masked() {
        assert_eq!(
            redact_secrets("https://alice:hunter2@mcp.example.com/v1?api_key=abc123&region=eu"),
            "https://***@mcp.example.com/v1?api_key=***&region=eu"
        );
        assert_eq!(
            redact_secrets("https://mcp.example.com/mcp?token=t0k#access_token=frag"),
            "https://mcp.example.com/mcp?token=***#access_token=***"
        );
        assert_eq!(
            redact_secrets("https://mcp.figma.com/mcp"),
            "https://mcp.figma.com/mcp",
            "没有凭据的地址原样显示"
        );
        assert_eq!(
            redact_secrets("http://localhost:3000/mcp"),
            "http://localhost:3000/mcp"
        );
    }

    #[test]
    fn values_after_sensitive_flags_are_masked() {
        assert_eq!(
            redact_args(&args(&["-y", "server", "--token", "abc", "--port", "8080"])),
            args(&["-y", "server", "--token", "***", "--port", "8080"])
        );
        assert_eq!(
            redact_args(&args(&["--api-key=abc", "--password", "p@ss"])),
            args(&["--api-key=***", "--password", "***"])
        );
        assert_eq!(
            redact_args(&args(&["--auth", "--verbose"])),
            args(&["--auth", "--verbose"]),
            "敏感开关后面跟的是另一个开关，不是值"
        );
        assert_eq!(
            redact_secrets("npx server --secret s3cr3t --name demo"),
            "npx server --secret *** --name demo"
        );
    }

    #[test]
    fn header_values_are_masked_keeping_the_scheme() {
        assert_eq!(
            redact_args(&args(&[
                "-H",
                "Authorization: Bearer abc.def",
                "-H",
                "X-Trace: 1"
            ])),
            args(&["-H", "Authorization: Bearer ***", "-H", "X-Trace: 1"])
        );
        assert_eq!(
            redact_args(&args(&["--header", "X-Api-Key: k123"])),
            args(&["--header", "X-Api-Key: ***"])
        );
        assert_eq!(
            redact_secrets(r#"curl -H "Authorization: token ghx" https://x"#),
            r#"curl -H "Authorization: token ***" https://x"#
        );
        assert_eq!(
            redact_secrets("Authorization:Bearer abc"),
            "Authorization:Bearer ***"
        );
    }

    #[test]
    fn sensitive_key_value_pairs_are_masked() {
        assert_eq!(
            redact_secrets("GITHUB_TOKEN=ghx OTHER=1 env API_KEY=k"),
            "GITHUB_TOKEN=*** OTHER=1 env API_KEY=***"
        );
        assert_eq!(
            redact_secrets(r#"HTTP 401: {"error":"bad","access_token":"abc","hint":"x"}"#),
            r#"HTTP 401: {"error":"bad","access_token":"***","hint":"x"}"#
        );
        assert_eq!(
            redact_secrets(r#""password": "hunter2""#),
            r#""password": "***""#
        );
        assert_eq!(redact_secrets("password: hunter2"), "password: ***");
    }

    #[test]
    fn bearer_and_known_token_shapes_are_masked_in_free_text() {
        assert_eq!(
            redact_secrets("request failed with Bearer abc123 attached"),
            "request failed with Bearer *** attached"
        );
        assert_eq!(
            redact_secrets("key sk-proj-ABCDEFGHIJKLMNOPQRST rejected"),
            "key sk-*** rejected"
        );
        assert_eq!(redact_secrets("(ghp_0123456789abcdefghij)"), "(ghp_***)");
        assert_eq!(
            redact_secrets("installed sk-learn and ask-question"),
            "installed sk-learn and ask-question",
            "短的、或者不在词首的不算令牌"
        );
    }

    #[test]
    fn plain_text_and_whitespace_survive() {
        let text = "MCP servers need attention:\n  figma: needs sign-in\n  fs: failed: spawn npx ENOENT\nRun /mcp to fix.";
        assert_eq!(redact_secrets(text), text);
        assert_eq!(
            redact_secrets("invalid token provided"),
            "invalid token provided"
        );
        assert_eq!(
            redact_secrets("登录失败：浏览器超时"),
            "登录失败：浏览器超时"
        );
    }
}
