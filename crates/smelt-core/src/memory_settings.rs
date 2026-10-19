//! 记忆召回的配置，存在运行时库的 `memory_settings` 表里，设计见 `docs/agent-memory.md` 5.2：
//! - 向量检索：用户手动填一个 OpenAI 兼容的 `/embeddings` 服务（含 Ollama 等本地服务）。
//!   没配或调用失败时，召回直接退回关键词 + 模型重排。
//! - 重排模型：从 Pi 已登录的模型里选；不选就自动挑。选了但不可用时退回检索结果。

pub use smelt_model::memory_settings::*;

use std::collections::BTreeMap;

use serde_json::Value;

/// `settings get` 不回显密钥本身，只说明设没设。
const API_KEY_SET: &str = "已设置";

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

impl EmbeddingSettings {
    /// 地址和模型都填了才算配置；密钥可空（本地服务不需要）。
    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}

/// 设置项 key 对应表里的哪一列。
fn field<'a>(
    record: &'a mut smelt_store::MemorySettingsRecord,
    key: &str,
) -> Option<&'a mut String> {
    match key {
        EMBEDDING_BASE_URL => Some(&mut record.embedding_base_url),
        EMBEDDING_MODEL => Some(&mut record.embedding_model),
        EMBEDDING_API_KEY => Some(&mut record.embedding_api_key),
        RERANK_MODEL_PROVIDER => Some(&mut record.rerank_model_provider),
        RERANK_MODEL_ID => Some(&mut record.rerank_model_id),
        _ => None,
    }
}

pub fn load_rerank_model_on(
    store: &smelt_store::Store,
) -> Result<Option<RerankModel>, smelt_store::StoreError> {
    let record = store.memory_settings()?;
    let (provider, id) = (record.rerank_model_provider, record.rerank_model_id);
    Ok((!provider.is_empty() && !id.is_empty()).then_some(RerankModel { provider, id }))
}

pub fn load_on(store: &smelt_store::Store) -> Result<EmbeddingSettings, smelt_store::StoreError> {
    let record = store.memory_settings()?;
    Ok(EmbeddingSettings {
        base_url: record.embedding_base_url,
        model: record.embedding_model,
        api_key: record.embedding_api_key,
    })
}

/// 校验并写入一项。空字符串表示清除。
pub fn apply_on(store: &smelt_store::Store, key: &str, value: &Value) -> Result<(), String> {
    let text = match value {
        Value::String(text) => text.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    if key == EMBEDDING_BASE_URL
        && !text.is_empty()
        && !(text.starts_with("http://") || text.starts_with("https://"))
    {
        return Err(format!("{key} 必须是 http:// 或 https:// 开头的地址"));
    }
    store
        .modify_memory_settings(|record| {
            let slot = field(record, key).ok_or_else(|| format!("未知的记忆设置：{key}"))?;
            *slot = text;
            Ok::<_, String>(())
        })
        .map(|_| ())
}

/// 给 `settings get` 和设置页看的形状：密钥只显示设没设。
pub fn to_map(
    settings: &EmbeddingSettings,
    rerank: Option<&RerankModel>,
) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            RERANK_MODEL_PROVIDER.to_string(),
            Value::from(
                rerank
                    .map(|model| model.provider.clone())
                    .unwrap_or_default(),
            ),
        ),
        (
            RERANK_MODEL_ID.to_string(),
            Value::from(rerank.map(|model| model.id.clone()).unwrap_or_default()),
        ),
        (
            EMBEDDING_BASE_URL.to_string(),
            Value::from(settings.base_url.clone()),
        ),
        (
            EMBEDDING_MODEL.to_string(),
            Value::from(settings.model.clone()),
        ),
        (
            EMBEDDING_API_KEY.to_string(),
            Value::from(if settings.api_key.is_empty() {
                ""
            } else {
                API_KEY_SET
            }),
        ),
    ])
}

/// 连接测试的结果：服务可用时返回向量维度和耗时。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingProbe {
    pub dimensions: usize,
    pub elapsed_ms: u128,
}

const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const PROBE_INPUT: &str = "Smelt 记忆向量检索连接测试";

/// 用给定配置真发一次 `/embeddings` 请求，确认服务可用。错误说成人话：HTTP 状态和响应开头、
/// 连不上、超时、返回格式不对。设置页的「测试连接」用它，测的是表单里还没保存的值。
pub async fn probe(settings: &EmbeddingSettings) -> Result<EmbeddingProbe, String> {
    if !(settings.base_url.starts_with("http://") || settings.base_url.starts_with("https://")) {
        return Err("服务地址必须是 http:// 或 https:// 开头".into());
    }
    if settings.model.is_empty() {
        return Err("还没填模型".into());
    }
    let url = format!("{}/embeddings", settings.base_url.trim_end_matches('/'));
    let mut builder = reqwest::Client::builder().timeout(PROBE_TIMEOUT);
    // 本机服务（Ollama 等）直连：reqwest 会用 macOS 的系统代理，代理连不上本机时回 502，
    // 用户会以为服务坏了。Pi 那边的 fetch 只认环境变量代理，不受影响。
    if is_loopback(&url) {
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|error| error.to_string())?;
    let mut request = client
        .post(&url)
        .json(&serde_json::json!({ "model": settings.model, "input": [PROBE_INPUT] }));
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(&settings.api_key);
    }
    let started = std::time::Instant::now();
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            format!("请求超时（{} 秒）：{url}", PROBE_TIMEOUT.as_secs())
        } else if error.is_connect() {
            format!("连不上 {url}：服务没启动或地址不对")
        } else {
            format!("请求失败：{error}")
        }
    })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("读取响应失败：{error}"))?;
    let elapsed_ms = started.elapsed().as_millis();
    if !status.is_success() {
        let snippet: String = body.chars().take(200).collect();
        let hint = match status.as_u16() {
            401 | 403 => "（API Key 不对或没填）",
            404 => "（地址不对：应填到 /v1 这一级，不含 /embeddings）",
            502..=504 => "（网关或代理报错：服务可能没启动，或代理连不上它）",
            _ => "",
        };
        return Err(format!("服务返回 {status}{hint}：{snippet}"));
    }
    let value: Value = serde_json::from_str(&body)
        .map_err(|_| "返回的不是 JSON，可能不是 OpenAI 兼容的 /embeddings 接口".to_string())?;
    let dimensions = value["data"][0]["embedding"]
        .as_array()
        .map(Vec::len)
        .filter(|len| *len > 0)
        .ok_or("返回里没有向量（data[0].embedding），可能不是向量模型")?;
    Ok(EmbeddingProbe {
        dimensions,
        elapsed_ms,
    })
}

fn is_loopback(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, smelt_store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        (dir, store)
    }

    #[test]
    fn round_trips_clears_and_never_echoes_the_key() {
        let (_dir, store) = store();
        assert!(!load_on(&store).unwrap().is_configured());

        apply_on(
            &store,
            EMBEDDING_BASE_URL,
            &Value::from(" http://localhost:11434/v1 "),
        )
        .unwrap();
        apply_on(&store, EMBEDDING_MODEL, &Value::from("bge-m3")).unwrap();
        apply_on(&store, EMBEDDING_API_KEY, &Value::from("sk-secret")).unwrap();
        let settings = load_on(&store).unwrap();
        assert!(settings.is_configured());
        assert_eq!(settings.base_url, "http://localhost:11434/v1");
        assert_eq!(settings.api_key, "sk-secret");
        let shown = to_map(&settings, None);
        assert_eq!(shown[EMBEDDING_API_KEY], "已设置");
        assert!(!serde_json::to_string(&shown).unwrap().contains("sk-secret"));

        apply_on(&store, EMBEDDING_MODEL, &Value::from("")).unwrap();
        assert!(!load_on(&store).unwrap().is_configured());
    }

    /// 一次性的假 HTTP 服务：收一个请求、回一段固定响应。
    fn serve_once(status: &str, body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{address}/v1")
    }

    fn settings(base_url: String) -> EmbeddingSettings {
        EmbeddingSettings {
            base_url,
            model: "bge-m3".into(),
            api_key: String::new(),
        }
    }

    #[test]
    fn probe_reports_the_vector_dimensions() {
        let url = serve_once(
            "200 OK",
            r#"{"data":[{"index":0,"embedding":[0.1,0.2,0.3]}]}"#,
        );
        let probe = crate::block_on::block_on_tokio(probe(&settings(url)))
            .unwrap()
            .unwrap();
        assert_eq!(probe.dimensions, 3);
    }

    #[test]
    fn probe_explains_what_went_wrong() {
        let run = |settings: EmbeddingSettings| {
            crate::block_on::block_on_tokio(probe(&settings))
                .unwrap()
                .unwrap_err()
        };
        let unauthorized = run(settings(serve_once(
            "401 Unauthorized",
            r#"{"error":"bad key"}"#,
        )));
        assert!(
            unauthorized.contains("401") && unauthorized.contains("API Key"),
            "{unauthorized}"
        );
        let not_found = run(settings(serve_once("404 Not Found", "")));
        assert!(not_found.contains("/v1"), "{not_found}");
        let chat_model = run(settings(serve_once("200 OK", r#"{"data":[]}"#)));
        assert!(chat_model.contains("没有向量"), "{chat_model}");
        let refused = {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            run(settings(format!("http://127.0.0.1:{port}/v1")))
        };
        assert!(refused.contains("连不上"), "{refused}");
        assert!(run(settings("localhost:11434".into())).contains("http://"));
    }

    #[test]
    fn local_services_are_recognised_for_direct_connection() {
        assert!(is_loopback("http://localhost:11434/v1/embeddings"));
        assert!(is_loopback("http://127.0.0.1:11434/v1/embeddings"));
        assert!(is_loopback("http://[::1]:11434/v1/embeddings"));
        assert!(!is_loopback("https://api.openai.com/v1/embeddings"));
    }

    #[test]
    fn rerank_model_counts_only_when_both_parts_are_set() {
        let (_dir, store) = store();
        assert_eq!(load_rerank_model_on(&store).unwrap(), None);
        apply_on(
            &store,
            RERANK_MODEL_PROVIDER,
            &Value::from("github-copilot"),
        )
        .unwrap();
        assert_eq!(load_rerank_model_on(&store).unwrap(), None);
        apply_on(&store, RERANK_MODEL_ID, &Value::from("gpt-6-luna")).unwrap();
        assert_eq!(
            load_rerank_model_on(&store).unwrap(),
            Some(RerankModel {
                provider: "github-copilot".into(),
                id: "gpt-6-luna".into()
            })
        );
        apply_on(&store, RERANK_MODEL_ID, &Value::from("")).unwrap();
        assert_eq!(load_rerank_model_on(&store).unwrap(), None);
    }

    #[test]
    fn rejects_a_base_url_that_is_not_http() {
        let (_dir, store) = store();
        assert!(apply_on(&store, EMBEDDING_BASE_URL, &Value::from("localhost:11434")).is_err());
    }
}
