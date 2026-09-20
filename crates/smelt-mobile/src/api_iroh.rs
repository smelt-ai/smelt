//! Flutter 实际调用的 API。
//!
//! `flutter_rust_bridge.yaml` 的 `rust_input` 只指向本模块；手机端通过本地
//! WebSocket 连接 iroh 隧道，不在 Rust FFI 层重复实现网关协议。

use flutter_rust_bridge::frb;

/// 启动到指定 EndpointId 的 iroh 隧道，返回手机本地入口端口。
///
/// Dart 侧随后照常连 `ws://127.0.0.1:<port>/acp/ws?token=...`：
/// 隧道对上层是透明的，鉴权和消息格式都和直连网关时完全一样。
///
/// 幂等 —— 同一个 endpoint 重复调用返回同一个端口。
pub async fn iroh_tunnel_start(endpoint_id: String, relay_url: String) -> Result<u32, String> {
    crate::iroh_tunnel::start(&endpoint_id, &relay_url)
        .await
        .map(|p| p as u32)
        .map_err(|e| format!("{e:#}"))
}

/// 停止 iroh 隧道。没有隧道时是 no-op。
pub async fn iroh_tunnel_stop() {
    crate::iroh_tunnel::stop().await;
}

/// iroh 当前选中的实际传输路径和 QUIC RTT。
#[derive(Clone, Debug)]
pub struct IrohPathStatus {
    /// `lan`、`p2p` 或 `relay`。
    pub kind: String,
    pub rtt_ms: u32,
}

pub async fn iroh_tunnel_path_status() -> Option<IrohPathStatus> {
    crate::iroh_tunnel::path_status()
        .await
        .map(|status| IrohPathStatus {
            kind: status.kind,
            rtt_ms: status.rtt_ms,
        })
}

/// App 启动时调用一次。
#[frb(init)]
pub fn init_app() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Debug),
    );
    log::info!("smelt-mobile initialized");
}
