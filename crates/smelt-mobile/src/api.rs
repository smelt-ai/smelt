//! Flutter 调用的 API
//!
//! 所有 `#[flutter_rust_bridge::frb]` 标记的函数和类型会被 codegen 生成 Dart 绑定。

use flutter_rust_bridge::frb;
use serde::{Deserialize, Serialize};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 数据类型
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 主机配置（配对后存储）
#[frb]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostConfig {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub public_key_b64: String,
}

/// 会话摘要（列表展示用）
#[frb]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub status: SessionStatus,
    pub agent_kind: AgentKind,
    pub last_message: Option<String>,
    pub unread: bool,
    pub updated_at: i64,
}

/// 会话状态
#[frb]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// 空闲，等待用户输入
    Idle,
    /// Agent 正在运行
    Running,
    /// 等待用户审批（权限请求）
    WaitingApproval,
    /// 等待用户输入（agent 主动询问）
    WaitingInput,
    /// 出错
    Error,
}

/// Agent 类型
#[frb]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    Claude,
    Codex,
    Copilot,
    Grok,
    Other,
}

/// ACP 消息条目
#[frb]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AcpEntry {
    /// 用户消息
    User { text: String },
    /// Assistant 回复
    Assistant { text: String, thought: bool },
    /// 工具调用
    ToolCall {
        id: String,
        title: String,
        kind: ToolKind,
        status: ToolStatus,
        output: Vec<String>,
    },
    /// 分割线
    Divider { label: String },
}

/// 工具类型
#[frb]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    Read,
    Write,
    Edit,
    Bash,
    Other,
}

/// 工具状态
#[frb]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Completed,
    Failed,
}

/// 审批菜单
#[frb]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalMenu {
    pub key: String,
    pub prompt: String,
    pub options: Vec<ApprovalOption>,
    pub allows_text_input: bool,
}

/// 审批选项
#[frb]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub key: String,
    pub label: String,
    pub kind: ApprovalKind,
}

/// 审批类型
#[frb]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalKind {
    Approve,
    Deny,
    Custom,
}

/// 连接状态
#[frb]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Handshaking,
    Connected,
    Reconnecting,
    AuthFailed,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 核心 API
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 连接到主机
#[frb]
pub async fn connect(host: HostConfig, device_token: String) -> Result<(), String> {
    crate::transport::connect(&host, &device_token)
        .await
        .map_err(|e| e.to_string())
}

/// 断开连接
#[frb]
pub fn disconnect() {
    crate::transport::disconnect();
}

/// 获取当前连接状态
#[frb]
pub fn connection_state() -> ConnectionState {
    crate::transport::connection_state()
}

/// 获取会话列表
#[frb]
pub async fn list_sessions() -> Result<Vec<SessionSummary>, String> {
    crate::session::list_sessions()
        .await
        .map_err(|e| e.to_string())
}

/// 订阅会话更新（返回初始条目列表，后续通过轮询获取）
#[frb]
pub async fn subscribe_session(session_id: String) -> Result<Vec<AcpEntry>, String> {
    crate::session::subscribe(&session_id)
        .await
        .map_err(|e| e.to_string())
}

/// 获取会话新条目（轮询用）
#[frb]
pub async fn poll_session_entries(
    session_id: String,
    since_index: u32,
) -> Result<Vec<AcpEntry>, String> {
    crate::session::poll_entries(&session_id, since_index as usize)
        .await
        .map_err(|e| e.to_string())
}

/// 发送用户消息
#[frb]
pub async fn send_message(session_id: String, text: String) -> Result<(), String> {
    crate::session::send_message(&session_id, &text)
        .await
        .map_err(|e| e.to_string())
}

/// 响应审批
#[frb]
pub async fn respond_approval(
    session_id: String,
    option_key: String,
    custom_text: Option<String>,
) -> Result<(), String> {
    crate::session::respond_approval(&session_id, &option_key, custom_text.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// 获取当前审批菜单
#[frb]
pub async fn get_approval_menu(session_id: String) -> Result<Option<ApprovalMenu>, String> {
    crate::session::get_approval_menu(&session_id)
        .await
        .map_err(|e| e.to_string())
}

/// 标记会话已读
#[frb]
pub async fn mark_session_read(session_id: String) -> Result<(), String> {
    crate::session::mark_read(&session_id)
        .await
        .map_err(|e| e.to_string())
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 配对 & 加密
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 生成密钥对（配对时用）
#[frb]
pub fn generate_keypair() -> KeyPair {
    crate::crypto::generate_keypair()
}

/// 密钥对
#[frb]
#[derive(Clone, Debug)]
pub struct KeyPair {
    pub public_key_b64: String,
    pub secret_key_b64: String,
}

/// 解析配对二维码
#[frb]
pub fn parse_pairing_qr(qr_data: String) -> Result<HostConfig, String> {
    crate::crypto::parse_pairing_qr(&qr_data).map_err(|e| e.to_string())
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// iroh P2P 隧道
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 启动到指定 EndpointId 的 iroh 隧道，返回本地入口端口。
///
/// Dart 侧随后照常连 `ws://127.0.0.1:<port>/acp/ws?token=...`：
/// 隧道对上层是透明的，鉴权和消息格式都和直连网关时完全一样。
///
/// 幂等 —— 同一个 endpoint 重复调用返回同一个端口。
#[frb]
pub async fn iroh_tunnel_start(endpoint_id: String) -> Result<u32, String> {
    crate::iroh_tunnel::start(&endpoint_id)
        .await
        .map(|p| p as u32)
        .map_err(|e| format!("{e:#}"))
}

/// 停止 iroh 隧道。没有隧道时是 no-op。
#[frb]
pub async fn iroh_tunnel_stop() {
    crate::iroh_tunnel::stop().await;
}

/// 当前隧道的本地端口，没有则返回 `None`。
#[frb]
pub async fn iroh_tunnel_port() -> Option<u32> {
    crate::iroh_tunnel::port().await.map(|p| p as u32)
}

/// 解析 `smelt+iroh://` 配对码。
///
/// 与桌面侧生成方共用 `smelt-core::pairing` 的定义，避免格式在两端漂移。
#[frb]
pub fn parse_iroh_pairing_uri(uri: String) -> Result<IrohPairing, String> {
    let parsed = smelt_core::pairing::parse_iroh_pairing_uri(&uri).map_err(|e| e.to_string())?;
    Ok(IrohPairing {
        endpoint_id: parsed.endpoint_id,
        token: parsed.token,
    })
}

/// `smelt+iroh://` 配对码的两半。缺一不可：EndpointId 决定连得上谁，
/// token 决定连上之后能不能操作。
#[frb]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IrohPairing {
    pub endpoint_id: String,
    pub token: String,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 初始化
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 初始化 Rust 运行时（App 启动时调用一次）
#[frb(init)]
pub fn init_app() {
    // 初始化日志
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Debug),
    );

    #[cfg(target_os = "ios")]
    {
        // iOS 日志初始化（可选）
    }

    log::info!("smelt-mobile initialized");
}
