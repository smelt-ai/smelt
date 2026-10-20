//! 设备身份与配对。
//!
//! 设备身份就是这台手机 iroh 密钥对应的 EndpointId，运行时按它认设备，所以密钥必须
//! 持久化在 App 数据目录里。

use flutter_rust_bridge::frb;

use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::RuntimeRecord;
use crate::connection::Target;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// 这台设备的 iroh EndpointId，运行时的设备列表里显示的就是它。
    pub endpoint_id: String,
}

/// 配对串里给界面预览的部分。一次性配对码不暴露。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingTarget {
    pub endpoint_id: String,
    pub relay_url: Option<String>,
}

/// 读取或生成设备密钥（`<data_dir>/client-iroh-secret`，0600），打开本机库
/// （`smelt-mobile.sqlite3`）和离线缓存，为每台登记的机器建节点并在后台开始连。返回设备身份。
///
/// 幂等：同一目录重复调用返回同一身份；换目录调用报错（身份不能在进程里悄悄换掉）。
/// 机器、草稿相关的调用都要在它之后；各事件流可以先订阅。
pub fn init_client(data_dir: String) -> Result<DeviceIdentity, SmeltError> {
    let endpoint_id = crate::fleet::fleet().init_client(data_dir.into())?;
    Ok(DeviceIdentity { endpoint_id })
}

/// 只解析并校验配对串，不联网。
#[frb(sync)]
pub fn parse_pairing(uri: String) -> Result<PairingTarget, SmeltError> {
    let pairing = parse(&uri)?;
    Ok(PairingTarget {
        endpoint_id: pairing.endpoint_id,
        relay_url: pairing.relay_url,
    })
}

/// 用配对串配对：`device.pair` 后 `system.describe`，成功后写进登记并保持连接，返回登记。
///
/// 不影响其它机器的连接（只有 endpoint 形态因此改变时，其它机器会自动重连一次）。这台
/// endpoint 已登记（被撤销后重新配对）时更新它的登记，名字、标签色、顺序不变；新机器名字
/// 默认取主机名，标签色取色板里还没用过的第一个，排在最后。失败时不留下任何登记，不自动
/// 重试（配对码一次性）。
pub fn pair(uri: String, device_name: String) -> Result<RuntimeRecord, SmeltError> {
    let pairing = parse(&uri)?;
    crate::fleet::fleet().pair(
        Target::new(pairing.endpoint_id, pairing.relay_url),
        pairing.code,
        device_name,
    )
}

fn parse(uri: &str) -> Result<smelt_pairing::Pairing, SmeltError> {
    let pairing = smelt_pairing::parse_pairing_uri(uri)
        .map_err(|message| error(SmeltErrorKind::InvalidPairing, message))?;
    crate::iroh_dialer::validate(&pairing.endpoint_id, pairing.relay_url.as_deref())?;
    Ok(pairing)
}
