//! 设备配对与鉴权（设计文档 D3）。
//!
//! 只认「是不是自己的设备」，不分权限：配对过的设备和本机桌面一样拥有全部能力。
//! 安全边界是配对本身，以及丢失后能立刻撤销。
//!
//! - 本机 unix socket：和以前一样，socket 文件 0600。
//! - iroh：QUIC 握手证明对端持有它 endpoint id 的私钥；id 在 `paired_device` 表里才放行。
//!   不在表里的设备只能调 `device.pair`，提交运行时发出的一次性配对码。

use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use smelt_core::control_api::{DeviceList as DeviceListCall, DevicePair, DeviceRevoke};
use smelt_core::remote_config::{
    DeviceIdParams, DeviceInfo, DeviceList, PairParams, PairingOffer, RemoteAccessState,
    RevokeResult,
};
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};

/// 这条连接是谁。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Principal {
    /// 本机 unix socket。
    Local,
    /// 已配对的远程设备。
    Device(String),
    /// 还没配对的远程设备，只能提交配对码。
    Unpaired(String),
}

impl Principal {
    pub(crate) fn is_remote(&self) -> bool {
        !matches!(self, Self::Local)
    }
}

/// 配对码多久作废。
const CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// 未作废的码累计猜错这么多次，就全部作废，要重新生成。
const MAX_FAILED_ATTEMPTS: u32 = 5;
/// 去掉了容易看混的 0/O、1/I。正好 32 个，一个字符 5 bit。
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

struct PendingCode {
    code: String,
    expires_at: Instant,
}

#[derive(Default)]
struct Codes {
    pending: Vec<PendingCode>,
    failed_attempts: u32,
}

static CODES: Mutex<Codes> = Mutex::new(Codes {
    pending: Vec::new(),
    failed_attempts: 0,
});

/// 8 位，按 `XXXX-XXXX` 显示。随机性来自 v4 UUID 的前 5 个字节（都是随机位）。
fn new_code() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut bits = u64::from_be_bytes([0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
    let mut code = String::with_capacity(9);
    for index in 0..8 {
        if index == 4 {
            code.push('-');
        }
        code.push(CODE_ALPHABET[(bits & 31) as usize] as char);
        bits >>= 5;
    }
    code
}

fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .flat_map(char::to_uppercase)
        .collect()
}

fn issue_code() -> (String, Instant) {
    let mut codes = CODES.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    codes.pending.retain(|pending| pending.expires_at > now);
    let code = new_code();
    let expires_at = now + CODE_TTL;
    codes.pending.push(PendingCode {
        code: code.clone(),
        expires_at,
    });
    (code, expires_at)
}

/// 码对且没过期就作废它并返回 true。
fn redeem_code(code: &str) -> bool {
    let wanted = normalize_code(code);
    let mut codes = CODES.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    codes.pending.retain(|pending| pending.expires_at > now);
    if let Some(index) = codes
        .pending
        .iter()
        .position(|pending| normalize_code(&pending.code) == wanted)
    {
        codes.pending.remove(index);
        codes.failed_attempts = 0;
        return true;
    }
    codes.failed_attempts += 1;
    if codes.failed_attempts >= MAX_FAILED_ATTEMPTS {
        codes.pending.clear();
        codes.failed_attempts = 0;
    }
    false
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn store() -> Result<smelt_store::Store, ControlError> {
    smelt_core::sqlite_state::default_sqlite_store()
        .map_err(|error| ControlError::new(ControlErrorCode::OperationFailed, error))
}

fn store_error(error: smelt_store::StoreError) -> ControlError {
    ControlError::new(ControlErrorCode::OperationFailed, error.to_string())
}

/// `method` 必须是 `device.*` 之一。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
    principal: &Principal,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::DevicePairBegin => {
            decode_params::<EmptyParams>(params)?;
            encode(pair_begin()?)
        }
        ControlMethod::DevicePair => {
            let params = decode_params::<<DevicePair as ControlCall>::Params>(params)?;
            pair(principal, params)?;
            encode(EmptyParams::default())
        }
        ControlMethod::DeviceList => {
            decode_params::<<DeviceListCall as ControlCall>::Params>(params)?;
            encode(list()?)
        }
        ControlMethod::DeviceRevoke => {
            let params = decode_params::<<DeviceRevoke as ControlCall>::Params>(params)?;
            encode(revoke(params)?)
        }
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not a device method"),
        )),
    }
}

fn encode(value: impl serde::Serialize) -> Result<serde_json::Value, ControlError> {
    serde_json::to_value(value)
        .map_err(|error| ControlError::new(ControlErrorCode::Internal, error.to_string()))
}

fn pair_begin() -> Result<PairingOffer, ControlError> {
    let status = crate::remote::lifecycle_remote()
        .map(crate::remote::remote_status)
        .unwrap_or_default();
    let RemoteAccessState::Running { endpoint_id, relay } = status else {
        return Err(ControlError::new(
            ControlErrorCode::FailedPrecondition,
            "远程访问没有开启",
        ));
    };
    let (code, expires_at) = issue_code();
    let expires_at_ms = now_ms()
        + expires_at
            .saturating_duration_since(Instant::now())
            .as_millis() as i64;
    Ok(PairingOffer {
        pairing_uri: smelt_pairing::pairing_uri(&endpoint_id, &code, relay.as_deref()),
        expires_at_ms,
    })
}

fn pair(principal: &Principal, params: PairParams) -> Result<(), ControlError> {
    let endpoint_id = match principal {
        Principal::Device(id) | Principal::Unpaired(id) => id,
        Principal::Local => {
            return Err(ControlError::new(
                ControlErrorCode::FailedPrecondition,
                "配对只能从远程设备发起",
            ));
        }
    };
    if !redeem_code(&params.code) {
        return Err(ControlError::new(
            ControlErrorCode::PermissionDenied,
            "配对码无效或已过期",
        ));
    }
    let name = match params.name.trim() {
        "" => "未命名设备".to_string(),
        name => name.chars().take(64).collect(),
    };
    store()?
        .put_paired_device(&smelt_store::PairedDevice {
            endpoint_id: endpoint_id.clone(),
            name: name.clone(),
            paired_at_ms: now_ms(),
            last_seen_at_ms: None,
        })
        .map_err(store_error)?;
    crate::dlog(&format!("设备已配对：{name}（{endpoint_id}）"));
    Ok(())
}

fn connected_devices() -> Vec<String> {
    crate::remote::lifecycle_remote()
        .and_then(|state| {
            state
                .lock()
                .unwrap()
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.registry.connected())
        })
        .unwrap_or_default()
}

fn list() -> Result<DeviceList, ControlError> {
    let connected = connected_devices();
    let devices = store()?
        .list_paired_devices()
        .map_err(store_error)?
        .into_iter()
        .map(|device| DeviceInfo {
            connected: connected.contains(&device.endpoint_id),
            endpoint_id: device.endpoint_id,
            name: device.name,
            paired_at_ms: device.paired_at_ms,
            last_seen_at_ms: device.last_seen_at_ms,
        })
        .collect();
    Ok(DeviceList { devices })
}

fn revoke(params: DeviceIdParams) -> Result<RevokeResult, ControlError> {
    let revoked = store()?
        .delete_paired_device(&params.endpoint_id)
        .map_err(store_error)?;
    // 先删白名单再断连：断开后设备重连时已经不在表里了。
    if let Some(state) = crate::remote::lifecycle_remote()
        && let Some(endpoint) = state.lock().unwrap().endpoint.as_ref()
    {
        endpoint.registry.close(&params.endpoint_id);
    }
    if revoked {
        crate::dlog(&format!("设备已撤销：{}", params.endpoint_id));
    }
    Ok(RevokeResult { revoked })
}

/// 最近连接时间最多这么久写一次库：每个短请求都是一条新流，逐条写太频繁。
const LAST_SEEN_INTERVAL: Duration = Duration::from_secs(60);

fn should_touch(endpoint_id: &str) -> bool {
    static TOUCHED: Mutex<Option<std::collections::HashMap<String, Instant>>> = Mutex::new(None);
    let mut touched = TOUCHED.lock().unwrap_or_else(|e| e.into_inner());
    let touched = touched.get_or_insert_with(Default::default);
    let now = Instant::now();
    match touched.get(endpoint_id) {
        Some(last) if now.duration_since(*last) < LAST_SEEN_INTERVAL => false,
        _ => {
            touched.insert(endpoint_id.to_string(), now);
            true
        }
    }
}

/// iroh 流的入口：认出是谁，交给和 unix socket 同一个分发器。
pub(crate) struct RemoteStreams;

impl smelt_iroh::runtime::StreamAcceptor for RemoteStreams {
    fn accept(&self, remote: &str, stream: UnixStream) {
        let remote = remote.to_string();
        // 查库、分发都是阻塞的，不能占着 tokio 线程。
        std::thread::spawn(move || {
            let principal = match smelt_core::sqlite_state::default_sqlite_store()
                .and_then(|store| store.paired_device(&remote).map_err(|e| e.to_string()))
            {
                Ok(Some(_)) => {
                    if should_touch(&remote)
                        && let Ok(store) = smelt_core::sqlite_state::default_sqlite_store()
                    {
                        let _ = store.touch_paired_device(&remote, now_ms());
                    }
                    Principal::Device(remote)
                }
                Ok(None) => Principal::Unpaired(remote),
                Err(error) => {
                    crate::dlog(&format!("查设备白名单失败，拒绝这条连接：{error}"));
                    return;
                }
            };
            let Some(server) = crate::protocol::SERVER.get() else {
                return;
            };
            crate::protocol::handle_conn(stream, server.context(principal));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_use_the_unambiguous_alphabet_and_ignore_case_and_dashes() {
        let code = new_code();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(
            code.chars()
                .filter(|ch| *ch != '-')
                .all(|ch| CODE_ALPHABET.contains(&(ch as u8)))
        );
        assert_eq!(normalize_code(" ab cd-efgh "), "ABCDEFGH");
    }

    /// 码只能用一次；连续猜错到上限，没用掉的码全部作废。
    #[test]
    fn codes_are_single_use_and_guessing_burns_them() {
        let (code, _) = issue_code();
        assert!(redeem_code(&code.to_lowercase()));
        assert!(!redeem_code(&code), "用过的码不能再用");

        let (code, _) = issue_code();
        for _ in 0..MAX_FAILED_ATTEMPTS {
            assert!(!redeem_code("WRONG-CODE"));
        }
        assert!(!redeem_code(&code), "猜错到上限后正确的码也要作废");
    }
}
