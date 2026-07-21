//! 托管 remote-web SPA（与信令同域，跨网手机只打开 signal 域名）。

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../remote-web/dist/"]
struct EmbeddedSpa;

fn spa_read(rel: &str) -> Option<Vec<u8>> {
    let rel = rel.trim_start_matches('/');
    // 开发：环境变量可指向磁盘 dist
    if let Ok(dir) = std::env::var("SMELT_REMOTE_WEB") {
        let p = std::path::Path::new(&dir).join(rel);
        if p.is_file() {
            if let Ok(b) = std::fs::read(p) {
                return Some(b);
            }
        }
    }
    EmbeddedSpa::get(rel).map(|f| f.data.into_owned())
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// SPA 入口：/ 与客户端路由 /s/... 都回 index.html  
/// 跨网页默认 write meta=true（真实写权限由 bridge hello_ok 再约束）
pub async fn spa_index() -> Response {
    let Some(bytes) = spa_read("index.html") else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "remote-web 未嵌入：构建前请 npm run build",
        )
            .into_response();
    };
    let mut raw = String::from_utf8_lossy(&bytes).into_owned();
    let meta = r#"<meta name="smelt-write" content="true" />"#;
    if raw.contains("</head>") {
        raw = raw.replacen("</head>", &format!("{meta}\n</head>"), 1);
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store, max-age=0"),
        ],
        raw,
    )
        .into_response()
}

pub async fn spa_asset(Path(path): Path<String>) -> Response {
    let rel = format!("assets/{path}");
    let Some(bytes) = spa_read(&rel) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let ct = content_type(&rel);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ct),
            // 带 hash 的 vite 资源可长缓存
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

pub fn spa_ready() -> bool {
    spa_read("index.html")
        .map(|b| {
            let s = String::from_utf8_lossy(&b);
            s.contains("/assets/") || s.contains("assets/")
        })
        .unwrap_or(false)
}
