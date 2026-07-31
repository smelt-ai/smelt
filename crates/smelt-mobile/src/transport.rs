//! WebSocket 传输层
//!
//! 管理与 smeltd remote_gateway 的连接，处理 E2EE 和重连。

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::api::{ConnectionState, HostConfig};
use crate::crypto::{
    decrypt, derive_shared_key, encrypt, public_key_from_b64, secret_key_from_b64,
};

/// 全局连接状态
static CONNECTION_STATE: AtomicU8 = AtomicU8::new(0); // 0 = Disconnected

/// 全局连接实例
static CONNECTION: once_cell::sync::Lazy<RwLock<Option<Connection>>> =
    once_cell::sync::Lazy::new(|| RwLock::new(None));

struct Connection {
    host: HostConfig,
    device_token: String,
    shared_key: [u8; 32],
    our_secret_b64: String,
    sender: mpsc::Sender<String>,
    _handle: tokio::task::JoinHandle<()>,
}

fn set_state(state: ConnectionState) {
    let val = match state {
        ConnectionState::Disconnected => 0,
        ConnectionState::Connecting => 1,
        ConnectionState::Handshaking => 2,
        ConnectionState::Connected => 3,
        ConnectionState::Reconnecting => 4,
        ConnectionState::AuthFailed => 5,
    };
    CONNECTION_STATE.store(val, Ordering::SeqCst);
}

/// 获取当前连接状态
pub fn connection_state() -> ConnectionState {
    match CONNECTION_STATE.load(Ordering::SeqCst) {
        0 => ConnectionState::Disconnected,
        1 => ConnectionState::Connecting,
        2 => ConnectionState::Handshaking,
        3 => ConnectionState::Connected,
        4 => ConnectionState::Reconnecting,
        5 => ConnectionState::AuthFailed,
        _ => ConnectionState::Disconnected,
    }
}

/// 连接到主机
pub async fn connect(host: &HostConfig, device_token: &str) -> Result<()> {
    // 断开现有连接
    disconnect();

    set_state(ConnectionState::Connecting);

    // 生成本端密钥对
    let our_keypair = crate::crypto::generate_keypair();
    let our_secret = secret_key_from_b64(&our_keypair.secret_key_b64)?;
    let peer_public = public_key_from_b64(&host.public_key_b64)?;
    let shared_key = derive_shared_key(&our_secret, &peer_public);

    // 建立 WebSocket
    let url = format!("{}/acp/ws?token={}", host.endpoint, device_token);
    let (ws_stream, _) = connect_async(&url)
        .await
        .map_err(|e| anyhow!("WebSocket connection failed: {}", e))?;

    set_state(ConnectionState::Handshaking);

    let (mut write, mut read) = ws_stream.split();

    // 发送握手（包含我们的公钥）
    let handshake = serde_json::json!({
        "type": "handshake",
        "publicKey": our_keypair.public_key_b64,
        "deviceToken": device_token,
    });
    write
        .send(Message::Text(handshake.to_string().into()))
        .await?;

    // 等待握手响应
    if let Some(Ok(msg)) = read.next().await {
        if let Message::Text(text) = msg {
            let resp: serde_json::Value = serde_json::from_str(&text)?;
            if resp.get("ok") != Some(&serde_json::Value::Bool(true)) {
                set_state(ConnectionState::AuthFailed);
                return Err(anyhow!(
                    "Handshake failed: {}",
                    resp.get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown")
                ));
            }
        }
    }

    set_state(ConnectionState::Connected);

    // 创建发送通道
    let (sender, mut receiver) = mpsc::channel::<String>(64);

    // 启动收发循环
    let shared_key_clone = shared_key;
    let host_clone = host.clone();
    let device_token_clone = device_token.to_string();

    let handle = tokio::spawn(async move {
        let write = Arc::new(Mutex::new(write));
        let write_clone = write.clone();

        // 发送任务
        let send_task = tokio::spawn(async move {
            while let Some(msg) = receiver.recv().await {
                // 加密消息
                if let Ok(encrypted) = encrypt(&msg, &shared_key_clone) {
                    let mut w = write_clone.lock().await;
                    if w.send(Message::Text(encrypted.into())).await.is_err() {
                        break;
                    }
                }
            }
        });

        // 接收任务
        while let Some(Ok(msg)) = read.next().await {
            match msg {
                Message::Text(text) => {
                    // 解密消息
                    if let Ok(decrypted) = decrypt(&text, &shared_key_clone) {
                        // 分发到会话处理器
                        crate::session::handle_message(&decrypted);
                    }
                }
                Message::Binary(data) => {
                    // 二进制消息（终端流等）
                    if let Ok(decrypted) = crate::crypto::decrypt_bytes(&data, &shared_key_clone) {
                        crate::session::handle_binary(&decrypted);
                    }
                }
                Message::Close(_) => {
                    log::info!("WebSocket closed by server");
                    break;
                }
                _ => {}
            }
        }

        send_task.abort();
        set_state(ConnectionState::Disconnected);

        // TODO: 自动重连逻辑
    });

    // 存储连接
    let mut conn = CONNECTION.write().await;
    *conn = Some(Connection {
        host: host.clone(),
        device_token: device_token.to_string(),
        shared_key,
        our_secret_b64: our_keypair.secret_key_b64,
        sender,
        _handle: handle,
    });

    Ok(())
}

/// 断开连接
pub fn disconnect() {
    // 通过 drop sender 来关闭连接
    let rt = tokio::runtime::Handle::try_current();
    if let Ok(rt) = rt {
        rt.block_on(async {
            let mut conn = CONNECTION.write().await;
            *conn = None;
        });
    }
    set_state(ConnectionState::Disconnected);
}

/// 发送消息（内部使用）
pub async fn send(message: &str) -> Result<()> {
    let conn = CONNECTION.read().await;
    if let Some(ref c) = *conn {
        c.sender
            .send(message.to_string())
            .await
            .map_err(|_| anyhow!("Send failed: connection closed"))?;
        Ok(())
    } else {
        Err(anyhow!("Not connected"))
    }
}

/// 发送 RPC 请求（等待响应）
pub async fn rpc<T: serde::de::DeserializeOwned>(
    method: &str,
    params: serde_json::Value,
) -> Result<T> {
    use std::sync::atomic::AtomicU64;
    static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

    let id = REQUEST_ID.fetch_add(1, Ordering::SeqCst);
    let request = serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    });

    // TODO: 实现请求-响应匹配
    // 目前简化为只发不等
    send(&request.to_string()).await?;

    // 临时：返回默认值
    Err(anyhow!("RPC response handling not implemented"))
}
