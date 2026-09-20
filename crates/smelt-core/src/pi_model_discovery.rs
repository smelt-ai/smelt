//! 问一个端点「你提供哪些模型」，直接走 HTTP。
//!
//! **为什么不复用 dsh 那条发现链路。** dsh 的发现要起一个完整 dsh 运行时
//! （`dsh-discover-models.mjs`），前提是本机装了 dsh profile 和 Smelt Host
//! bundle。只用 Pi 的用户没有这些，让 Pi 的模型配置依赖 dsh 装没装，是把两个
//! 本来无关的东西绑在一起。列模型这件事本身就是一次 `GET /models`，直接发。
//!
//! **为什么按协议分派而不是统一发一个请求。** 认证头和响应形状是协议规定的：
//! OpenAI 系是 `Authorization: Bearer` + `{data:[{id}]}`，Anthropic 是
//! `x-api-key` + `anthropic-version` + `{data:[{id,display_name}]}`。用一种猜
//! 另一种，得到的是 401 而不是模型列表。
//!
//! 拉不到就是拉不到：调用方保留原有目录，绝不因为一次网络抖动把用户的配置清空。

use crate::provider_api::DiscoveredModel;

/// 端点最多等这么久。这是用户点了按钮在等的交互，不是后台任务。
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 拼出 `GET` 模型列表的完整 URL。
///
/// 用户填 base URL 时带不带 `/v1`、带不带尾斜杠都有；已经指到 `/models` 的也
/// 有（有些网关文档就是这么给的）。这里全部收敛到一个形状，避免拼出
/// `.../v1//models` 或 `.../models/models` 这种一定 404 的地址。
pub fn models_endpoint(base_url: &str) -> Result<String, String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("请先填写 API 地址".to_string());
    }
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(format!("API 地址需要以 http:// 或 https:// 开头：{base}"));
    }
    if base.ends_with("/models") {
        return Ok(base.to_string());
    }
    Ok(format!("{base}/models"))
}

/// 一次发现请求。`api_key` 为空表示端点不需要凭据（有些内网网关如此）。
#[derive(Clone, Debug, Default)]
pub struct ModelDiscoveryRequest {
    pub base_url: String,
    pub api: String,
    pub api_key: String,
}

/// 同步调用，内部自带 tokio 运行时。必须在后台线程里跑：有一次网络往返。
pub fn discover_models(request: &ModelDiscoveryRequest) -> Result<Vec<DiscoveredModel>, String> {
    let url = models_endpoint(&request.base_url)?;
    let api = request.api.trim().to_string();
    let key = request.api_key.trim().to_string();
    let body =
        crate::block_on::block_on_tokio(async move { fetch_models_body(&url, &api, &key).await })
            .map_err(|error| error.to_string())??;
    parse_models_response(&body)
}

async fn fetch_models_body(url: &str, api: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .build()
        .map_err(|error| format!("创建 HTTP 客户端失败：{error}"))?;
    let mut builder = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "Smelt");
    if !api_key.is_empty() {
        builder = if api == "anthropic-messages" {
            builder
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
        } else {
            builder.bearer_auth(api_key)
        };
    }
    let response = builder
        .send()
        .await
        .map_err(|error| format!("请求 {url} 失败：{error}"))?;
    let status = response.status();
    if !status.is_success() {
        // 不带响应体：4xx 的 body 常常把 key 的前缀原样回显出来，那会进日志。
        return Err(format!("端点返回 HTTP {status}"));
    }
    response
        .text()
        .await
        .map_err(|error| format!("读取响应失败：{error}"))
}

/// 从响应里挑出模型列表。
///
/// 三种形状都见过：OpenAI 的 `{data:[...]}`、部分网关的 `{models:[...]}`、还有
/// 直接给数组的。名字字段各家不一（`display_name`/`name`），都认。
pub fn parse_models_response(body: &str) -> Result<Vec<DiscoveredModel>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("解析响应失败：{error}"))?;
    let items = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| "响应里没有模型列表".to_string())?;

    let mut models: Vec<DiscoveredModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let id = item
            .get("id")
            .or_else(|| item.get("name"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| item.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let Some(id) = id else { continue };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let name = item
            .get("display_name")
            .or_else(|| item.get("displayName"))
            .or_else(|| item.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != id)
            .map(str::to_string);
        models.push(DiscoveredModel {
            id: id.to_string(),
            name,
            context_window: read_u64(item, &["context_window", "contextWindow", "context_length"]),
            max_tokens: read_u64(item, &["max_tokens", "maxTokens", "max_output_tokens"]),
        });
    }
    Ok(models)
}

fn read_u64(item: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| item.get(*key))
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_tolerates_how_users_actually_write_base_urls() {
        assert_eq!(
            models_endpoint("https://api.example.com/v1").unwrap(),
            "https://api.example.com/v1/models"
        );
        assert_eq!(
            models_endpoint(" https://api.example.com/v1/ ").unwrap(),
            "https://api.example.com/v1/models"
        );
        // 文档里直接给到 /models 的网关，不能再拼一层。
        assert_eq!(
            models_endpoint("https://api.example.com/v1/models").unwrap(),
            "https://api.example.com/v1/models"
        );
    }

    #[test]
    fn endpoint_rejects_input_that_cannot_be_requested() {
        assert!(models_endpoint("   ").is_err());
        assert!(models_endpoint("api.example.com/v1").is_err());
    }

    #[test]
    fn parses_the_openai_shape() {
        let models = parse_models_response(
            r#"{"object":"list","data":[{"id":"gpt-5","object":"model"},{"id":"o4"}]}"#,
        )
        .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5");
        assert_eq!(models[0].name, None);
    }

    #[test]
    fn parses_the_anthropic_shape_with_display_names() {
        let models = parse_models_response(
            r#"{"data":[{"id":"claude-sonnet-5","display_name":"Claude Sonnet 5"}]}"#,
        )
        .unwrap();
        assert_eq!(models[0].label(), "Claude Sonnet 5");
    }

    #[test]
    fn parses_gateway_shapes_and_picks_up_limits() {
        let models = parse_models_response(
            r#"{"models":[{"id":"m","contextWindow":1000000,"max_tokens":16384}]}"#,
        )
        .unwrap();
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(models[0].max_tokens, Some(16384));

        let bare = parse_models_response(r#"["a","b"]"#).unwrap();
        assert_eq!(bare.len(), 2);
        assert_eq!(bare[1].id, "b");
    }

    /// 网关重复列同一个模型时，界面不该出现两颗一模一样的按钮。
    #[test]
    fn duplicate_ids_collapse() {
        let models = parse_models_response(r#"{"data":[{"id":"m"},{"id":"m"}]}"#).unwrap();
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn a_body_without_a_list_is_an_error_not_an_empty_catalog() {
        assert!(parse_models_response(r#"{"error":"unauthorized"}"#).is_err());
        assert!(parse_models_response("not json").is_err());
    }

    /// 空列表是**答案**（端点说它一个都不提供），不是错误。
    #[test]
    fn an_empty_list_is_an_answer() {
        assert_eq!(parse_models_response(r#"{"data":[]}"#).unwrap(), Vec::new());
    }
}
