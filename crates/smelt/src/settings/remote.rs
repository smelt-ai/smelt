//! 远程访问的设置页模型：开关、自建 relay、设备配对和撤销（设计文档 D2、D3）。
//!
//! 生命周期归运行时：这里只把配置交给它、取回它的状态。守护重启、升级之后它自己按
//! 配置拉起来，客户端不用补发什么。

use std::sync::{Arc, OnceLock};

use gpui::{App, Global, Image, ImageFormat, SharedString, Task};
use smelt_model::control_api::{
    DeviceList, DevicePairBegin, DeviceRevoke, RemoteConfigGet, RemoteConfigSet, RemoteStatus,
    RemoteStatusWait,
};
use smelt_model::remote_config::{
    DeviceIdParams, DeviceInfo, REMOTE_STATUS_WAIT_MAX, RemoteAccessState, RemoteStatusWaitParams,
};
use smelt_runtime_client::protocol::control::{ControlCall, EmptyParams};

/// 远程访问配置。孤儿规则不允许给外部类型实现 GPUI 的 `Global`，套一层 newtype。
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RemoteConfig(pub smelt_model::remote_config::RemoteConfig);

impl std::ops::Deref for RemoteConfig {
    type Target = smelt_model::remote_config::RemoteConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for RemoteConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Global for RemoteConfig {}

/// 运行时那边的远程状态、已配对设备和正在展示的配对串。设置页渲染只读它。
#[derive(Clone, Default)]
pub struct RemoteSettingsState {
    pub status: RemoteAccessState,
    pub devices: Vec<DeviceInfo>,
    pub offer: Option<PairingOfferView>,
    pub generating_offer: bool,
    /// 最近一次操作（保存、生成配对串、撤销）的失败原因。
    pub error: Option<String>,
}

impl Global for RemoteSettingsState {}

#[derive(Clone)]
pub struct PairingOfferView {
    pub uri: String,
    /// 在后台线程编好；设置页高频重绘时直接复用，不每帧重算二维码。
    pub qr_image: Option<Arc<Image>>,
}

/// 在后台调一次运行时，回到主线程处理。
fn run_call<C>(
    params: C::Params,
    cx: &mut App,
    done: impl FnOnce(Result<C::Result, String>, &mut App) + 'static,
) where
    C: ControlCall + 'static,
    C::Params: Send + 'static,
    C::Result: Send + 'static,
{
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                crate::runtime_call::call::<C>(params, crate::runtime_call::RUNTIME_CALL_TIMEOUT)
                    .map_err(crate::runtime_call::error_text)
            })
            .await;
        cx.update(|cx| done(result, cx));
    })
    .detach();
}

fn update_state(cx: &mut App, change: impl FnOnce(&mut RemoteSettingsState)) {
    let mut state = cx
        .try_global::<RemoteSettingsState>()
        .cloned()
        .unwrap_or_default();
    change(&mut state);
    cx.set_global(state);
}

/// 用户最新想要的配置。运行时的配置还没取回来之前用户已经改过，取回来的旧值不能
/// 盖掉这次修改。
fn user_edited() -> &'static std::sync::atomic::AtomicBool {
    static EDITED: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    EDITED.get_or_init(Default::default)
}

/// 启动时从本机运行时取回配置。运行时可能还在起来：等它在线再读，读失败就退避后回去等
/// （见 [`crate::runtime_events::wait_online`]）。
pub fn start_remote_config_load(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut failures = 0u32;
        let loaded = loop {
            if !crate::runtime_events::wait_online(&crate::RuntimeKey::Local, failures, cx).await {
                return;
            }
            let fetched = cx
                .background_executor()
                .spawn(async {
                    crate::runtime_call::call::<RemoteConfigGet>(
                        EmptyParams::default(),
                        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                    )
                })
                .await;
            if let Ok(config) = fetched {
                break RemoteConfig(config);
            }
            failures = failures.saturating_add(1);
        };
        cx.update(|cx| {
            if !user_edited().load(std::sync::atomic::Ordering::SeqCst) {
                cx.set_global(loaded);
            }
        });
    })
    .detach();
}

/// 取一次远程状态和设备列表。打开设置窗口时调；绑定中就等它绑完（见
/// [`watch_remote_status`]）。
pub fn refresh_remote(cx: &mut App) {
    run_call::<RemoteStatus>(EmptyParams::default(), cx, |result, cx| {
        let starting = matches!(result, Ok(RemoteAccessState::Starting));
        update_state(cx, |state| match result {
            Ok(status) => state.status = status,
            Err(error) => state.error = Some(error),
        });
        if starting {
            watch_remote_status(cx);
        }
    });
    refresh_devices(cx);
}

/// 正在跟着绑定进度的那条等待。新的顶掉旧的：丢掉旧任务就停了它。
struct RemoteStatusWatch {
    _task: Task<()>,
}

impl Global for RemoteStatusWatch {}

/// 远程访问正在绑定（要联网找 relay，可能十几秒）时跟着它，直到绑完或失败：每次请运行时
/// 等状态和手里的不一样了再回（`remote.status.wait`），不隔一会儿查一次。设置窗口关了就
/// 不再等，重开时 [`refresh_remote`] 会重新取一次。
fn watch_remote_status(cx: &mut App) {
    let task = cx.spawn(async move |cx| {
        let mut known = RemoteAccessState::Starting;
        loop {
            let params = RemoteStatusWaitParams {
                known: known.clone(),
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call::<RemoteStatusWait>(
                        params,
                        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                    )
                    .map_err(crate::runtime_call::error_text)
                })
                .await;
            let status = match result {
                Ok(status) => status,
                Err(error) => {
                    cx.update(|cx| update_state(cx, |state| state.error = Some(error)));
                    return;
                }
            };
            let keep_waiting = cx.update(|cx| {
                if status != known {
                    let status = status.clone();
                    update_state(cx, |state| state.status = status);
                }
                status == RemoteAccessState::Starting && settings_window_open(cx)
            });
            if !keep_waiting {
                return;
            }
            known = status;
        }
    });
    cx.set_global(RemoteStatusWatch { _task: task });
}

// 运行时最多挂 REMOTE_STATUS_WAIT_MAX 就回，等它的读超时要比这长，不然没变化时会报超时。
const _: () =
    assert!(crate::runtime_call::RUNTIME_CALL_TIMEOUT.as_secs() > REMOTE_STATUS_WAIT_MAX.as_secs());

/// 设置窗口还开着。远程访问的状态只在那里显示。
fn settings_window_open(cx: &mut App) -> bool {
    let Some(handle) = cx
        .try_global::<super::SettingsWindowHandle>()
        .map(|handle| handle.0)
    else {
        return false;
    };
    handle.is_active(cx).is_some()
}

fn refresh_devices(cx: &mut App) {
    run_call::<DeviceList>(EmptyParams::default(), cx, |result, cx| {
        update_state(cx, |state| match result {
            Ok(list) => state.devices = list.devices,
            Err(error) => state.error = Some(error),
        });
    });
}

/// 交给运行时保存并照着开关。保存按提交顺序串行，后一次总是覆盖前一次。
fn save_remote_config(config: RemoteConfig, cx: &mut App) {
    user_edited().store(true, std::sync::atomic::Ordering::SeqCst);
    cx.set_global(config.clone());
    update_state(cx, |state| {
        state.error = None;
        // 配置变了，旧配对串里的 endpoint 或 relay 可能已经不对。
        state.offer = None;
    });
    cx.spawn(async move |cx| {
        static GATE: OnceLock<Arc<smol::lock::Mutex<()>>> = OnceLock::new();
        let gate = Arc::clone(GATE.get_or_init(Default::default));
        let result = cx
            .background_executor()
            .spawn(async move {
                let _guard = gate.lock().await;
                crate::runtime_call::call::<RemoteConfigSet>(
                    config.0,
                    crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                )
                .map_err(crate::runtime_call::error_text)
            })
            .await;
        cx.update(|cx| {
            let starting = matches!(result, Ok(RemoteAccessState::Starting));
            update_state(cx, |state| match result {
                Ok(status) => state.status = status,
                Err(error) => state.error = Some(format!("保存远程配置失败：{error}")),
            });
            if starting {
                watch_remote_status(cx);
            }
        });
    })
    .detach();
}

pub fn apply_remote_toggle(enabled: bool, cx: &mut App) {
    let mut config = cx.global::<RemoteConfig>().clone();
    config.enabled = enabled;
    save_remote_config(config, cx);
}

pub(crate) fn apply_iroh_relay_value(value: SharedString, cx: &mut App) {
    let mut config = cx.global::<RemoteConfig>().clone();
    let relay = value.trim().to_string();
    if config.iroh_relay == relay {
        return;
    }
    config.iroh_relay = relay;
    save_remote_config(config, cx);
}

/// 生成一次性配对串，二维码在后台编好。
pub fn begin_pairing(cx: &mut App) {
    update_state(cx, |state| {
        state.generating_offer = true;
        state.error = None;
    });
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                let offer = crate::runtime_call::call::<DevicePairBegin>(
                    EmptyParams::default(),
                    crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                )
                .map_err(crate::runtime_call::error_text)?;
                let qr_image = qr_png_for_url(&offer.pairing_uri)
                    .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)));
                Ok::<_, String>(PairingOfferView {
                    uri: offer.pairing_uri,
                    qr_image,
                })
            })
            .await;
        cx.update(|cx| {
            update_state(cx, |state| {
                state.generating_offer = false;
                match result {
                    Ok(offer) => state.offer = Some(offer),
                    Err(error) => state.error = Some(format!("生成配对串失败：{error}")),
                }
            })
        });
    })
    .detach();
}

/// 撤销一台设备：运行时删掉白名单并立即断开它。
pub fn revoke_device(endpoint_id: String, cx: &mut App) {
    run_call::<DeviceRevoke>(DeviceIdParams { endpoint_id }, cx, |result, cx| {
        if let Err(error) = result {
            update_state(cx, |state| state.error = Some(format!("撤销失败：{error}")));
        }
        refresh_devices(cx);
    });
}

pub(crate) fn qr_png_for_url(url: &str) -> Option<Vec<u8>> {
    use qrcode::QrCode;
    let code = QrCode::new(url.as_bytes()).ok()?;
    let luma = code
        .render::<image::Luma<u8>>()
        .dark_color(image::Luma([0u8]))
        .light_color(image::Luma([255u8]))
        .min_dimensions(160, 160)
        .quiet_zone(true)
        .build();
    let rgb = image::DynamicImage::ImageLuma8(luma).into_rgb8();
    let mut buf = Vec::new();
    rgb.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .ok()?;
    Some(buf)
}
