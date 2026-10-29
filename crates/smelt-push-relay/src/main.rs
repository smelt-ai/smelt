//! Smelt 推送中继。
//!
//! - `POST /v1/register`：手机交来 APNs 令牌，换一个句柄（见 `handle`）。
//! - `POST /v1/send`：smeltd 带着句柄发一条推送，中继解开句柄、签名、转给 APNs。
//! - `GET /healthz`：探活。
//!
//! 只监听本机，HTTPS 由前面的反向代理做（部署见 docs/push-relay-deployment.md）。日志里
//! 不出现令牌、句柄和提供方令牌。

mod apns;
mod handle;
mod limit;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use smelt_push::{
    RELAY_REGISTER_PATH, RELAY_SEND_PATH, RegisterRequest, RegisterResponse, SendRequest,
};

use crate::apns::{Apns, Outcome};
use crate::handle::{Device, HandleKey};
use crate::limit::RateLimiter;

#[derive(Parser)]
#[command(about = "Smelt 推送中继：替 smeltd 把推送转给 APNs")]
struct Args {
    /// 监听地址。HTTPS 交给前面的反向代理。
    #[arg(long, env = "SMELT_PUSH_LISTEN", default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    /// Apple 后台下载的 APNs 私钥（AuthKey_XXXX.p8）。
    #[arg(long, env = "SMELT_PUSH_APNS_KEY")]
    apns_key: PathBuf,
    /// 这把私钥的 Key ID。
    #[arg(long, env = "SMELT_PUSH_APNS_KEY_ID")]
    apns_key_id: String,
    /// Apple Developer Team ID。
    #[arg(long, env = "SMELT_PUSH_TEAM_ID")]
    team_id: String,
    /// 允许推送的 App Bundle ID，逗号分隔。必须属于上面这个 Team。
    #[arg(long, env = "SMELT_PUSH_TOPICS", value_delimiter = ',', required = true)]
    topics: Vec<String>,
    /// 句柄密钥文件：32 字节随机数的 base64（`openssl rand -base64 32`）。换掉它所有句柄作废。
    #[arg(long, env = "SMELT_PUSH_HANDLE_KEY")]
    handle_key: PathBuf,
}

struct AppState {
    apns: Apns,
    handles: HandleKey,
    topics: HashSet<String>,
    limiter: RateLimiter,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        eprintln!("smelt-push-relay: {error}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    let read = |path: &PathBuf| {
        std::fs::read_to_string(path).map_err(|error| format!("读不了 {}：{error}", path.display()))
    };
    let state = AppState {
        apns: Apns::new(&read(&args.apns_key)?, args.apns_key_id, args.team_id)?,
        handles: HandleKey::from_base64(&read(&args.handle_key)?)?,
        topics: args
            .topics
            .into_iter()
            .map(|topic| topic.trim().to_string())
            .filter(|topic| !topic.is_empty())
            .collect(),
        limiter: RateLimiter::default(),
    };
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .map_err(|error| format!("监听 {} 失败：{error}", args.listen))?;
    eprintln!(
        "smelt-push-relay 监听 {}，允许的 topic：{:?}",
        args.listen, state.topics
    );
    axum::serve(listener, router(Arc::new(state)))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("服务退出：{error}"))
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(RELAY_REGISTER_PATH, post(register))
        .route(RELAY_SEND_PATH, post(send))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}

async fn register(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, &'static str)> {
    if !smelt_push::valid_device_token(&request.device_token) {
        return Err((StatusCode::BAD_REQUEST, "device_token 不是十六进制令牌"));
    }
    if !state.topics.contains(&request.topic) {
        return Err((StatusCode::BAD_REQUEST, "这个中继不给该 App 推送"));
    }
    let handle = state.handles.seal(&Device {
        token: request.device_token.to_ascii_lowercase(),
        environment: request.environment,
        topic: request.topic,
    });
    Ok(Json(RegisterResponse { handle }))
}

async fn send(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SendRequest>,
) -> (StatusCode, &'static str) {
    let Some(device) = state.handles.open(&request.handle) else {
        // 不是这个中继发的，或中继换过句柄密钥：手机要重新登记。
        return (StatusCode::GONE, "句柄已失效");
    };
    if !state.topics.contains(&device.topic) {
        return (StatusCode::GONE, "这个中继已不再给该 App 推送");
    }
    if request.collapse_id.is_empty() || request.collapse_id.len() > 64 || !is_uuid(&request.apns_id)
    {
        return (StatusCode::BAD_REQUEST, "collapse_id 或 apns_id 格式不对");
    }
    if !state.limiter.allow(&device.token, Instant::now()) {
        return (StatusCode::TOO_MANY_REQUESTS, "这台手机的推送太频繁");
    }
    match state.apns.send(&device, &request).await {
        Outcome::Delivered => (StatusCode::OK, "ok"),
        Outcome::Unregistered => (StatusCode::GONE, "令牌已失效"),
        Outcome::BadRequest(reason) => {
            eprintln!("APNs 拒收：{reason}");
            (StatusCode::BAD_REQUEST, "APNs 拒收")
        }
        Outcome::Retry(reason) => {
            eprintln!("APNs 暂时不可用：{reason}");
            (StatusCode::BAD_GATEWAY, "APNs 暂时不可用")
        }
    }
}

/// APNs 的 `apns-id` 要求 8-4-4-4-12 的 UUID 写法。
fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use base64::Engine as _;
    use tower::ServiceExt as _;

    const TOPIC: &str = "ai.smelt.mobile";

    fn state() -> Arc<AppState> {
        Arc::new(AppState {
            apns: Apns::new(&apns::test_p8(), "K".into(), "T".into()).unwrap(),
            handles: HandleKey::from_base64(
                &base64::engine::general_purpose::STANDARD.encode([3; 32]),
            )
            .unwrap(),
            topics: HashSet::from([TOPIC.to_string()]),
            limiter: RateLimiter::default(),
        })
    }

    async fn post_json(
        state: Arc<AppState>,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, Vec<u8>) {
        let response = router(state)
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap()
            .to_vec();
        (status, body)
    }

    fn send_body(handle: &str) -> serde_json::Value {
        serde_json::json!({
            "handle": handle,
            "kind": "approval",
            "runtime_id": "r",
            "session_id": "s",
            "collapse_id": "c",
            "apns_id": "123e4567-e89b-42d3-a456-426614174000",
        })
    }

    #[tokio::test]
    async fn register_returns_a_handle_that_opens_to_the_device() {
        let state = state();
        let token = "AB".repeat(32);
        let (status, body) = post_json(
            Arc::clone(&state),
            RELAY_REGISTER_PATH,
            serde_json::json!({ "device_token": token, "environment": "sandbox", "topic": TOPIC }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let response: RegisterResponse = serde_json::from_slice(&body).unwrap();
        let device = state.handles.open(&response.handle).unwrap();
        assert_eq!(device.token, "ab".repeat(32), "令牌统一成小写");
        assert_eq!(device.topic, TOPIC);
    }

    #[tokio::test]
    async fn register_rejects_unknown_topics_and_bad_tokens() {
        let (status, _) = post_json(
            state(),
            RELAY_REGISTER_PATH,
            serde_json::json!({ "device_token": "ab", "environment": "sandbox", "topic": "x.y" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = post_json(
            state(),
            RELAY_REGISTER_PATH,
            serde_json::json!({ "device_token": "xyz", "environment": "sandbox", "topic": TOPIC }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// 别的中继发的、或换过句柄密钥之前发的句柄：回 410，运行时删掉登记。
    #[tokio::test]
    async fn send_with_a_foreign_handle_is_gone() {
        let (status, _) = post_json(state(), RELAY_SEND_PATH, send_body("bogus")).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn send_rejects_malformed_ids_before_calling_apns() {
        let state = state();
        let handle = state.handles.seal(&Device {
            token: "ab".repeat(32),
            environment: smelt_push::ApnsEnvironment::Sandbox,
            topic: TOPIC.into(),
        });
        let mut body = send_body(&handle);
        body["apns_id"] = "not-a-uuid".into();
        let (status, _) = post_json(state, RELAY_SEND_PATH, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn uuid_format() {
        assert!(is_uuid("123e4567-e89b-42d3-a456-426614174000"));
        assert!(!is_uuid("123e4567e89b42d3a456426614174000"));
        assert!(!is_uuid("123e4567-e89b-42d3-a456-42661417400g"));
    }
}
