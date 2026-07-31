//! Flutter 实际调用的 API。
//!
//! 刻意与 [`crate::api`] 分开：那个模块是一套**废弃方向**的实现（自造 E2EE
//! 握手、`rpc()` 直接返回未实现），和手机端真正在跑的协议对不上。把它桥到
//! Dart 会生成一堆看着能用、其实是空壳的绑定，比没有还糟。
//!
//! 所以 `flutter_rust_bridge.yaml` 的 `rust_input` 只指向本模块。

use flutter_rust_bridge::frb;

/// 启动到指定 EndpointId 的 iroh 隧道，返回手机本地入口端口。
///
/// Dart 侧随后照常连 `ws://127.0.0.1:<port>/acp/ws?token=...`：
/// 隧道对上层是透明的，鉴权和消息格式都和直连网关时完全一样。
///
/// 幂等 —— 同一个 endpoint 重复调用返回同一个端口。
pub async fn iroh_tunnel_start(
    endpoint_id: String,
    relay_url: String,
    relay_token: String,
) -> Result<u32, String> {
    crate::iroh_tunnel::start(&endpoint_id, &relay_url, &relay_token)
        .await
        .map(|p| p as u32)
        .map_err(|e| format!("{e:#}"))
}

/// 停止 iroh 隧道。没有隧道时是 no-op。
pub async fn iroh_tunnel_stop() {
    crate::iroh_tunnel::stop().await;
}

/// 当前隧道的本地端口，没有则返回 `None`。
pub async fn iroh_tunnel_port() -> Option<u32> {
    crate::iroh_tunnel::port().await.map(|p| p as u32)
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

/// `smelt+iroh://` 配对码的两半。缺一不可：EndpointId 决定连得上谁，
/// token 决定连上之后能不能操作。
#[derive(Clone, Debug)]
pub struct IrohPairing {
    pub endpoint_id: String,
    pub token: String,
    pub relay_url: String,
    pub relay_token: String,
}

/// 解析 `smelt+iroh://` 配对码。
///
/// Dart 侧自己也有一份解析（`pairing_config.dart`，扫码即时校验用，不能等
/// FFI 起来）。这个函数留给需要在 Rust 侧核对同一个码的场景，两边都以
/// `smelt-core::pairing` 为准。
pub fn parse_iroh_pairing_uri(uri: String) -> Result<IrohPairing, String> {
    let parsed = smelt_core::pairing::parse_iroh_pairing_uri(&uri)?;
    Ok(IrohPairing {
        endpoint_id: parsed.endpoint_id,
        token: parsed.token,
        relay_url: parsed.relay_url,
        relay_token: parsed.relay_token,
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
