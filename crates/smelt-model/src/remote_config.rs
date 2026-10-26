//! 远程访问：配置、运行状态、已配对设备（设计文档 D2、D3）。存取和生命周期都在运行时。

use serde::{Deserialize, Serialize};

/// 远程访问配置。运行时收到新配置就照着开关 iroh endpoint，守护重启后也按它自愈。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 自建 iroh relay。留空就用 n0 的公共 relay 和 DNS 地址发现。
    #[serde(default)]
    pub iroh_relay: String,
}

/// 远程访问现在的样子。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RemoteAccessState {
    #[default]
    Off,
    /// 正在绑定 endpoint（要联网找 relay，可能十几秒）。
    Starting,
    Running {
        endpoint_id: String,
        /// 自建 relay；`None` 表示用公共 relay。
        relay: Option<String>,
    },
    /// 开着但起不来，守护会退避重试。
    Failed { error: String },
}

/// `remote.status.wait` 一次最多挂这么久。调用方的读超时要比它长。
pub const REMOTE_STATUS_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(20);

/// `remote.status.wait` 的参数：调用方手里的状态。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteStatusWaitParams {
    pub known: RemoteAccessState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    /// 设备的 iroh endpoint id，运行时按它认设备。
    pub endpoint_id: String,
    pub name: String,
    pub paired_at_ms: i64,
    pub last_seen_at_ms: Option<i64>,
    /// 现在有没有连着。
    pub connected: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceList {
    pub devices: Vec<DeviceInfo>,
}

/// 一次性配对串。用一次或过期就作废。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingOffer {
    pub pairing_uri: String,
    pub expires_at_ms: i64,
}

/// 新设备提交配对码。只有这个方法对未配对设备开放。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairParams {
    pub code: String,
    /// 设备名，显示在设备列表里。
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceIdParams {
    pub endpoint_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevokeResult {
    /// 是否真有这台设备。
    pub revoked: bool,
}
