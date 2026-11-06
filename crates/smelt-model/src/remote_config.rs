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

/// 这一版手机认得的推送载荷版本，随 `push.register` 登记到各台机器。
///
/// - 1（旧版 App 不带这个字段）：只认四个提醒类型的 loc-key，不认「完成」。
/// - 2：认得带摘要的 loc-key（`SMELT_PUSH_*_DETAIL`）和「完成」。
///
/// 运行时按登记的版本挑载荷：给旧 App 发它的文案文件里没有的键，iOS 会把键名原样显示出来。
pub const PUSH_PAYLOAD_VERSION: u32 = 2;

/// `push.register` 的参数。哪台设备由连接的身份决定，不在参数里。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushRegisterParams {
    /// 推送中继发的句柄（加密过的 APNs 令牌）。运行时只转交，解不开。
    pub handle: String,
    /// 推送中继的地址，`https://` 开头。手机编译时定好，运行时往这里发。
    pub relay_url: String,
    pub approval: bool,
    pub input: bool,
    pub failure: bool,
    /// 跑完一轮。旧版手机没有这个开关，不带就是不推。
    #[serde(default)]
    pub success: bool,
    /// 见 [`PUSH_PAYLOAD_VERSION`]。旧版手机不带，就是 1。
    #[serde(default = "legacy_push_payload_version")]
    pub payload_version: u32,
    /// 智能体主动发的消息。正文原样显示，哪一版 App 都认，旧版手机不带就是推。
    #[serde(default = "default_push_message")]
    pub message: bool,
}

fn legacy_push_payload_version() -> u32 {
    1
}

fn default_push_message() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧版手机的登记不带「完成」和载荷版本：不推完成，载荷按版本 1 发。
    #[test]
    fn legacy_push_registration_reads_as_version_one_without_success() {
        let params: PushRegisterParams = serde_json::from_value(serde_json::json!({
            "handle": "h",
            "relay_url": "https://push.example.test",
            "approval": true,
            "input": true,
            "failure": true,
        }))
        .unwrap();
        assert!(!params.success);
        assert_eq!(params.payload_version, 1);
        assert!(params.message, "智能体消息旧版 App 也显示得了，默认推");
    }
}
