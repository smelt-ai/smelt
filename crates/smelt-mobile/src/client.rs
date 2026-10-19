//! 进程级单例：设备身份和当前运行时的连接状态机。
//!
//! 后续阶段（会话目录、对话、终端）从 [`current_runtime`] 拿当前的 `RuntimeClient`，
//! 用 [`subscribe`] 得知运行时切换（generation 变化）与连接状态变化。

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::cache::Cache;
use crate::connection::{ConnectionHub, Timing};
use crate::iroh_dialer::IrohDialer;

/// 设备密钥文件名，和桌面的 `~/.smelt/client-iroh-secret` 同名。
const SECRET_FILE: &str = "client-iroh-secret";

struct Identity {
    data_dir: PathBuf,
    endpoint_id: String,
    dialer: Arc<IrohDialer>,
    cache: Option<Arc<Cache>>,
}

static IDENTITY: Mutex<Option<Identity>> = Mutex::new(None);

/// 状态机不依赖身份：`connection_events` 可以先于 `init_client` 订阅。
static HUB: LazyLock<ConnectionHub> = LazyLock::new(|| ConnectionHub::new(Timing::PRODUCTION));

pub(crate) fn hub() -> &'static ConnectionHub {
    &HUB
}

/// 读取或生成设备密钥，返回设备的 endpoint id。同一目录幂等。
pub(crate) fn init(data_dir: PathBuf) -> Result<String, SmeltError> {
    let mut slot = IDENTITY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(identity) = slot.as_ref() {
        if identity.data_dir == data_dir {
            return Ok(identity.endpoint_id.clone());
        }
        return Err(error(
            SmeltErrorKind::Other,
            format!(
                "设备身份已从 {} 加载，不能换成 {}",
                identity.data_dir.display(),
                data_dir.display()
            ),
        ));
    }
    let secret = smelt_iroh::load_or_create_secret(&data_dir.join(SECRET_FILE))
        .map_err(|e| error(SmeltErrorKind::Other, format!("读取设备身份失败：{e:#}")))?;
    let endpoint_id = secret.public().to_string();
    log::info!("设备身份：{endpoint_id}");
    // 缓存打不开不影响连接，只是离线时没有旧数据可看。
    let cache = Cache::open(&data_dir.join(crate::cache::DATABASE_FILE))
        .map(Arc::new)
        .map_err(|e| log::warn!("离线缓存打不开：{e}"))
        .ok();
    *slot = Some(Identity {
        data_dir,
        endpoint_id: endpoint_id.clone(),
        dialer: Arc::new(IrohDialer::new(secret)),
        cache,
    });
    Ok(endpoint_id)
}

pub(crate) fn dialer() -> Result<Arc<IrohDialer>, SmeltError> {
    IDENTITY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|identity| Arc::clone(&identity.dialer))
        .ok_or_else(|| {
            error(
                SmeltErrorKind::Other,
                "还没有初始化设备身份（先调 init_client）",
            )
        })
}

/// 当前运行时（连接中或重连中都算），没有时为 None。给后续阶段用。
pub(crate) fn current_runtime() -> Option<crate::connection::CurrentRuntime> {
    HUB.current()
}

/// 监听运行时切换与连接状态。见 [`ConnectionHub::subscribe`]。给后续阶段用。
pub(crate) fn subscribe(listener: crate::connection::Listener) {
    HUB.subscribe(listener);
}

/// 离线缓存。还没初始化身份或库打不开时为 None。
pub(crate) fn cache() -> Option<Arc<Cache>> {
    IDENTITY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|identity| identity.cache.clone())
}
