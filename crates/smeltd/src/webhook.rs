//! 本机 Webhook：外部程序 POST 到 127.0.0.1 即可触发自动化。
//!
//! 不进公网。每条 Webhook 时机的自动化有独立令牌，路径即鉴权。

use std::sync::Mutex;
use std::thread;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use smelt_core::automation::DEFAULT_WEBHOOK_PORT;
use smelt_core::automation_store::AutomationStore;
use tokio::net::TcpListener;

use super::event_hub::EventHubHandle;
use super::recover_locked_automations;

static BASE_URL: Mutex<Option<String>> = Mutex::new(None);

#[derive(Clone)]
struct WebhookState {
    automations: AutomationStore,
    event_hub: EventHubHandle,
}

pub(crate) fn current_base_url() -> Option<String> {
    BASE_URL.lock().unwrap().clone()
}

pub(crate) fn annotate_snapshot(
    mut snapshot: smelt_core::automation::AutomationFile,
) -> smelt_core::automation::AutomationFile {
    snapshot.webhook_base_url = current_base_url();
    snapshot
}

pub(crate) fn spawn(automations: AutomationStore, event_hub: EventHubHandle) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("[webhook] 无法创建 Tokio runtime: {error}");
                return;
            }
        };
        runtime.block_on(async move {
            let Some(listener) = bind_loopback().await else {
                return;
            };
            let port = match listener.local_addr() {
                Ok(addr) => addr.port(),
                Err(error) => {
                    eprintln!("[webhook] 读取监听地址失败: {error}");
                    return;
                }
            };
            let url = format!("http://127.0.0.1:{port}");
            *BASE_URL.lock().unwrap() = Some(url.clone());
            {
                let snapshot = annotate_snapshot(automations.lock().unwrap().snapshot());
                if let Err(error) = event_hub.publish_automations(&snapshot) {
                    eprintln!("[webhook] 发布自动化投影失败: {error}");
                }
            }
            eprintln!("[webhook] 本机接收 {url}/hooks/<token>");
            let app = router(automations, event_hub);
            if let Err(error) = axum::serve(listener, app).await {
                eprintln!("[webhook] 服务退出: {error}");
            }
        });
    });
}

pub(crate) fn router(automations: AutomationStore, event_hub: EventHubHandle) -> Router {
    Router::new()
        .route("/hooks/{token}", post(post_hook).options(preflight))
        .with_state(WebhookState {
            automations,
            event_hub,
        })
}

async fn bind_loopback() -> Option<TcpListener> {
    for port in DEFAULT_WEBHOOK_PORT..DEFAULT_WEBHOOK_PORT.saturating_add(16) {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => {
                eprintln!("[webhook] 绑定 127.0.0.1:{port} 失败: {error}");
                return None;
            }
        }
    }
    eprintln!("[webhook] 127.0.0.1:{DEFAULT_WEBHOOK_PORT} 起连续 16 个端口都被占用");
    None
}

async fn preflight() -> Response {
    cors(StatusCode::NO_CONTENT, Json(json!({})))
}

async fn post_hook(
    Path(token): Path<String>,
    State(state): State<WebhookState>,
    body: axum::body::Bytes,
) -> Response {
    let payload = parse_payload(&body);
    recover_locked_automations(&state.automations, &state.event_hub);
    let result =
        state
            .automations
            .lock()
            .unwrap()
            .trigger_webhook(&token, payload, chrono::Local::now());
    match result {
        Ok(applied) => {
            if applied.changed {
                let snapshot = annotate_snapshot(applied.snapshot);
                if let Err(error) = state.event_hub.publish_automations(&snapshot) {
                    eprintln!("[webhook] 发布自动化投影失败: {error}");
                }
            }
            let run = applied.result.run();
            cors(
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "run_id": run.as_ref().map(|run| run.id.clone()),
                    "automation_id": run.as_ref().map(|run| run.automation_id.clone()),
                    "status": run.as_ref().map(|run| run.status),
                })),
            )
        }
        Err(error) if error.contains("不存在") => cors(
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": error })),
        ),
        Err(error) if error.contains("已暂停") => cors(
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": error })),
        ),
        Err(error) if error.contains("已有运行中") => cors(
            StatusCode::CONFLICT,
            Json(json!({ "ok": false, "error": error })),
        ),
        Err(error) => cors(
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": error })),
        ),
    }
}

fn parse_payload(body: &[u8]) -> Option<Value> {
    if body.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        return Some(value);
    }
    String::from_utf8(body.to_vec()).ok().map(Value::String)
}

fn cors(status: StatusCode, Json(body): Json<Value>) -> Response {
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use smelt_core::automation::{
        Automation, AutomationAction, AutomationCommand, AutomationTrigger,
    };
    use smelt_core::automation_store::AutomationOwner;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn store_with_hook(token: &str) -> AutomationStore {
        let store = Arc::new(Mutex::new(AutomationOwner::in_memory()));
        let automation = Automation {
            id: "hook-1".into(),
            name: "Hook".into(),
            enabled: true,
            workspace_dir: Some("/tmp".into()),
            trigger: AutomationTrigger::webhook_with_secret(token),
            action: AutomationAction::shell("true", Vec::new()),
            sinks: Vec::new(),
        };
        store
            .lock()
            .unwrap()
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation),
                },
                chrono::Utc::now(),
            )
            .unwrap();
        store
    }

    #[tokio::test]
    async fn post_to_local_hook_starts_a_run() {
        let automations = store_with_hook("tok_abc");
        let event_hub = crate::new_event_hub();
        let app = router(Arc::clone(&automations), event_hub);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/tok_abc")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"你好"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot = automations.lock().unwrap().snapshot();
        assert_eq!(snapshot.runs.len(), 1);
        assert_eq!(
            snapshot.runs[0].context.trigger_payload,
            Some(json!({"text":"你好"}))
        );
    }

    #[tokio::test]
    async fn unknown_token_is_not_found() {
        let automations = store_with_hook("tok_abc");
        let event_hub = crate::new_event_hub();
        let app = router(automations, event_hub);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn paused_hook_is_forbidden() {
        let automations = store_with_hook("tok_abc");
        automations
            .lock()
            .unwrap()
            .apply(
                AutomationCommand::SetEnabled {
                    automation_id: "hook-1".into(),
                    enabled: false,
                },
                chrono::Utc::now(),
            )
            .unwrap();
        let event_hub = crate::new_event_hub();
        let app = router(automations, event_hub);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/tok_abc")
                    .body(Body::from(r#"{"text":"你好"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
