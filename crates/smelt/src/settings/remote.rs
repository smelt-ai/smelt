//! 远程网关与 iroh 隧道的配置模型和生命周期操作。
//!
//! 这些状态既被设置页展示，也被应用启动流程驱动；把它们集中在这里可以让
//! `settings/mod.rs` 专注于设置页模型与渲染，而不改变原有的 `settings::...` API。

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui::{App, Global, Image, ImageFormat, SharedString, Window};

use crate::terminal;

// ===================== 远程操作网关（见 docs/remote-ops-roadmap.md） =====================

/// 远程操作网关的持久化配置（全局单例，存 SQLite KV）。
///
/// 结构体本体在 `smelt_core::remote_config`——守护也要读同一份文件来自愈
/// （见那边的模块注释）。这里只是给它套一层 newtype，好实现 GPUI 的 `Global`
/// （孤儿规则不允许给外部类型实现外部 trait）。`#[serde(transparent)]` 保证
/// 落盘格式与守护读到的一模一样。
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RemoteConfig(pub smelt_core::remote_config::RemoteConfig);

impl std::ops::Deref for RemoteConfig {
    type Target = smelt_core::remote_config::RemoteConfig;

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

/// 读取远程网关开关；缺失/损坏回退默认（关闭）。
pub fn load_remote_config() -> RemoteConfig {
    RemoteConfig(smelt_core::remote_config::load())
}

fn save_remote_config(c: &RemoteConfig) {
    smelt_core::remote_config::save(&c.0)
}

/// 内嵌远程网关的运行时状态（不落盘，纯展示用）。网关只作为 iroh 的本机落点，
/// 因而 UI 只需要保留启动错误；token 和绑定地址不再单独展示。
#[derive(Clone, Default)]
pub struct RemoteRuntimeState {
    pub error: Option<String>,
}

impl Global for RemoteRuntimeState {}

/// 远程网关/iroh 的控制 op 有依赖顺序（停隧道→停网关→起网关→起隧道），用异步
/// 门闩把它们在后台串行化。它绝不在 UI 线程等待；用户连续切换时，后续操作会在
/// 前一项结束后按新的配置执行，而不是被旧 stop 反向覆盖。
static REMOTE_CONTROL_GATE: OnceLock<Arc<smol::lock::Mutex<()>>> = OnceLock::new();

fn remote_control_gate() -> Arc<smol::lock::Mutex<()>> {
    Arc::clone(REMOTE_CONTROL_GATE.get_or_init(|| Arc::new(smol::lock::Mutex::new(()))))
}

/// 后台远程操作回主线程时，只能覆盖仍对应同一份用户配置的运行态。否则用户已经
/// 关掉远程、改了 relay 或切了写权限，迟到结果会把新界面反写成旧状态。
fn remote_config_matches(current: &RemoteConfig, expected: &RemoteConfig) -> bool {
    current.enabled == expected.enabled
        && current.iroh_relay == expected.iroh_relay
        && current.write_enabled == expected.write_enabled
}

fn remote_lifecycle_matches(current: &RemoteConfig, expected: &RemoteConfig) -> bool {
    current.enabled == expected.enabled && current.iroh_relay == expected.iroh_relay
}

/// 后台任务拿到控制门闩后再读一次落盘配置。UI 全局只能在主线程读取，
/// 而副作用发生在后台；这次复查必须位于门闩之内、任何 daemon op 之前，
/// 才能阻止迟到的 bootstrap 在用户已经关闭远程后重新启动网关。
fn persisted_remote_config_matches(expected: &RemoteConfig) -> bool {
    let current = load_remote_config();
    remote_config_matches(&current, expected)
}

fn persisted_remote_relay_matches(expected: &RemoteConfig) -> bool {
    let current = load_remote_config();
    current.enabled == expected.enabled && current.iroh_relay == expected.iroh_relay
}

fn persisted_remote_lifecycle(expected: &RemoteConfig) -> Option<RemoteConfig> {
    let current = load_remote_config();
    (current.enabled == expected.enabled && current.iroh_relay == expected.iroh_relay)
        .then_some(current)
}

/// 「反馈问题」入口统一跳转的链接。应用菜单 / 侧栏用户菜单 / 设置页三处共用。
pub const FEEDBACK_URL: &str = "https://github.com/smelt-ai/smelt/issues/new/choose";

// ===================== iroh P2P 隧道（见 smeltd「iroh 隧道」） =====================

/// iroh 隧道运行时（不落盘）：配对 URI + 二维码图片。
///
/// 存的是 `smelt+iroh://` 配对 URI，由持久化的 `endpoint_id` + token 组成。
/// 两者重启都不变，只有用户手动刷新 token 才需要重新扫码。
#[derive(Clone, Default)]
pub struct IrohRuntimeState {
    pub connecting: bool,
    pub pairing_uri: Option<String>,
    /// 二维码图片在隧道状态变化时只构造一次；设置页高频重绘时直接复用，避免每帧
    /// 复制 PNG 字节并重新计算图片 id。
    pub qr_image: Option<std::sync::Arc<Image>>,
    pub error: Option<String>,
    pub write: bool,
}

impl Global for IrohRuntimeState {}

/// 已连接的移动端设备列表（不落盘，定期轮询刷新）。
#[derive(Clone, Default)]
pub struct IrohConnectionsState {
    pub connections: Vec<crate::terminal::IrohConnection>,
}

impl Global for IrohConnectionsState {}

/// 异步拉起 iroh 隧道。绑定要连接用户配置的 relay，可能耗时数秒，**必须**走后台。
fn spawn_iroh_start(cx: &mut App) {
    let config = cx.global::<RemoteConfig>().clone();
    if !config.enabled {
        return;
    }
    let expected = config;
    cx.set_global(IrohRuntimeState {
        connecting: true,
        ..Default::default()
    });
    cx.spawn(async move |cx| {
        let gate = remote_control_gate();
        let expected_for_io = expected.clone();
        let Some((result, remote, qr_image)) = cx
            .background_executor()
            .spawn(async move {
                let _guard = gate.lock().await;
                let current = persisted_remote_lifecycle(&expected_for_io)?;
                if !current.enabled {
                    return None;
                }
                let result = terminal::iroh_start(current.write_enabled, &current.iroh_relay);
                // iroh_start 可能顺带把网关也开了（守护侧的 ensure_remote_gateway），
                // 所以要回读一次网关现状，否则 UI 上「本机链接」那块会一直是空的。
                let remote = terminal::remote_status();
                // 二维码在后台线程算好再交给 UI：绝不在 UI 线程现算 QR。
                let qr_png = match &result {
                    Ok(s) => match (
                        s.endpoint_id.as_deref(),
                        s.token.as_deref(),
                        s.relay.as_deref(),
                    ) {
                        (Some(id), Some(tok), Some(relay)) if !tok.is_empty() => {
                            qr_png_for_url(&smelt_pairing::iroh_pairing_uri(id, tok, relay))
                        }
                        _ => None,
                    },
                    Err(_) => None,
                };
                let qr_image =
                    qr_png.map(|png| std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, png)));
                Some((result, remote, qr_image))
            })
            .await
        else {
            return;
        };
        cx.update(|cx| {
            if !remote_lifecycle_matches(cx.global::<RemoteConfig>(), &expected) {
                return;
            }
            if remote.running {
                cx.set_global(RemoteRuntimeState::default());
            }
            let rt = match result {
                Ok(s) => {
                    let uri = match (
                        s.endpoint_id.as_deref(),
                        s.token.as_deref(),
                        s.relay.as_deref(),
                    ) {
                        (Some(id), Some(tok), Some(relay)) if !tok.is_empty() => {
                            Some(smelt_pairing::iroh_pairing_uri(id, tok, relay))
                        }
                        _ => None,
                    };
                    match uri {
                        Some(uri) => IrohRuntimeState {
                            connecting: false,
                            pairing_uri: Some(uri),
                            qr_image,
                            error: None,
                            write: s.write,
                        },
                        // 隧道起来了但 token 没拿到：配对码缺一半，给不出可用的码。
                        None => IrohRuntimeState {
                            connecting: false,
                            error: Some("P2P 通道已建立，但分享密钥还没就绪，点重试即可".into()),
                            ..Default::default()
                        },
                    }
                }
                Err(e) => IrohRuntimeState {
                    connecting: false,
                    error: Some(e),
                    ..Default::default()
                },
            };
            cx.set_global(rt);
        });
    })
    .detach();
}

pub(crate) fn apply_iroh_relay_value(value: SharedString, cx: &mut App) {
    let mut config = cx.global::<RemoteConfig>().clone();
    config.iroh_relay = value.trim().to_string();
    let was_enabled = config.enabled;
    save_remote_config(&config);
    cx.set_global(config.clone());
    if was_enabled {
        // 停隧道要等守护回执；配置输入回调在 UI 线程，不能直接做 socket I/O。
        let gate = remote_control_gate();
        let expected = config.clone();
        cx.background_executor()
            .spawn(async move {
                let _guard = gate.lock().await;
                // 只有保存时的 relay/开关仍然有效，才执行这次旧隧道的 stop；写权限
                // 的独立偏好变化不影响停旧隧道，但下一份 relay 不能被迟到的 stop 关掉。
                if !persisted_remote_relay_matches(&expected) {
                    return;
                }
                terminal::iroh_stop();
            })
            .detach();
        cx.set_global(IrohRuntimeState {
            error: Some("Relay 配置已更新，点重试应用新配置".into()),
            ..Default::default()
        });
    }
    cx.refresh_windows();
}

/// 唯一远程开关：本机网关只作为 iroh 的 loopback 落点，不单独对用户暴露。
pub fn apply_remote_toggle(enabled: bool, cx: &mut App) {
    let mut config = cx.global::<RemoteConfig>().clone();
    config.enabled = enabled;
    save_remote_config(&config);
    cx.set_global(config.clone());

    if enabled {
        // 网关与隧道必须串行，`spawn_remote_bootstrap` 已经保证前者完成才启动后者。
        // 它的所有 socket 操作都在 background executor，切换开关本身立即返回。
        spawn_remote_bootstrap(cx);
    } else {
        // iroh 要赶在网关之前停，理由同 smeltd 的 cleanup：反过来正在转发的流
        // 会撞上已死端口，手机侧看到的是连接被拒而非干净关闭。
        cx.set_global(RemoteRuntimeState::default());
        cx.set_global(IrohRuntimeState::default());
        let gate = remote_control_gate();
        let expected = config.clone();
        cx.background_executor()
            .spawn(async move {
                let _guard = gate.lock().await;
                // 只执行仍对应「关闭远程」这份配置的 stop。若用户已经重新打开，
                // 迟到的旧 stop 不能把新 bootstrap 拉起的隧道再关掉。
                if expected.enabled || load_remote_config().enabled {
                    return;
                }
                terminal::iroh_stop();
                terminal::remote_stop();
            })
            .detach();
    }
    cx.refresh_windows();
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

pub fn spawn_iroh_start_public(cx: &mut App) {
    spawn_iroh_start(cx);
}

/// 幂等地把远程访问拉到「配置里希望的状态」：先问守护现状，已经跑着就复用，
/// 没有才 start，最后刷新配对码。
///
/// 三处调用：GUI 冷启动、硬重启守护之后、无缝升级之后。后两者以前完全没做这件事
/// ——守护换了进程，网关和隧道都随旧进程死了，而 GUI 只在冷启动拉过一次，于是
/// 手机侧静默失联，只有去设置页「关掉再打开」才恢复。守护侧现在也会按配置自愈
/// （见 smeltd 的 `autostart_remote_from_config`），这里是第二道保险，同时负责把
/// UI 上那份可能已经过期的配对码刷新掉。
///
/// 网关和隧道**串在同一条后台任务**里：并行时隧道可能先回而 token 还是空的，
/// UI 会拼出 `?token=` 的死链。
pub fn spawn_remote_bootstrap(cx: &mut App) {
    let expected = cx.global::<RemoteConfig>().clone();
    if !expected.enabled {
        return;
    }
    cx.spawn(async move |cx| {
        let gate = remote_control_gate();
        let expected_for_io = expected.clone();
        let Some(remote_rt) = cx
            .background_executor()
            .spawn(async move {
                let _guard = gate.lock().await;
                let current = persisted_remote_lifecycle(&expected_for_io)?;
                if !current.enabled {
                    return None;
                }
                crate::terminal::ensure_daemon_running();
                // 本机网关：已在跑就复用 token，否则按配置 start。
                // iroh 也需要本机网关 token（配对码里带着它）。
                let existing = crate::terminal::remote_status();
                let remote_rt =
                    if existing.running && existing.token.as_ref().is_some_and(|t| !t.is_empty()) {
                        RemoteRuntimeState { error: None }
                    } else {
                        match crate::terminal::remote_start("127.0.0.1", current.write_enabled) {
                            Ok(_) => RemoteRuntimeState { error: None },
                            Err(e) => RemoteRuntimeState { error: Some(e) },
                        }
                    };
                Some(remote_rt)
            })
            .await
        else {
            return;
        };
        cx.update(|cx| {
            if !remote_lifecycle_matches(cx.global::<RemoteConfig>(), &expected) {
                return;
            }
            cx.set_global(remote_rt);
            // 网关 token 就绪后再拉 iroh：配对码要把 token 拼进去，早拉会拿到空的。
            spawn_iroh_start_public(cx);
        });
    })
    .detach();
}

/// 看门狗轮询间隔。远程掉线是低频事件，20s 足够快，也不至于让守护 socket 变忙。
const REMOTE_WATCHDOG_INTERVAL: Duration = Duration::from_secs(20);
/// 已连接设备列表刷新间隔。比看门狗更频繁，以便及时显示新设备连接。
const CONNECTIONS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// 连续失败时最多跳过多少个轮询周期再试（≈ 20s → 5min 的退避上限）。
const REMOTE_WATCHDOG_MAX_SKIP: u32 = 15;

/// 远程访问看门狗：定期核对「守护里是不是真的还开着」，掉了就自动拉回来。
///
/// 守护侧的 `autostart_remote_from_config` 已经覆盖了守护自己重启的情况，这里补的
/// 是另外两件事：
/// 1. 老版本守护（没有自愈逻辑）被拉起时，仍然有人负责把远程恢复；
/// 2. UI 不再撒谎——`IrohRuntimeState` 是一次 `iroh_start` 的结果快照，隧道早就没了
///    它还在显示二维码，用户对着一个死码扫半天。
///
/// 退避：连续失败（典型是 relay 连不上）时逐步拉长间隔，避免每 20s 砸一次 relay。
pub fn spawn_remote_watchdog(cx: &mut App) {
    // 启动时初始化已连接设备状态
    cx.set_global(IrohConnectionsState::default());
    // 启动连接列表刷新任务
    spawn_connections_refresh(cx);
    cx.spawn(async move |cx| {
        let mut failures: u32 = 0;
        let mut skip: u32 = 0;
        loop {
            cx.background_executor()
                .timer(REMOTE_WATCHDOG_INTERVAL)
                .await;
            let config = cx.update(|cx| cx.global::<RemoteConfig>().clone());
            // 没开远程、或 relay 还没配（配了才谈得上隧道）就什么都不做。
            if !config.enabled || config.iroh_relay.trim().is_empty() {
                failures = 0;
                skip = 0;
                continue;
            }
            if skip > 0 {
                skip -= 1;
                continue;
            }
            // 正在连的时候别插一脚：iroh 绑定最长 30s，比一个轮询周期还久。
            let connecting = cx.update(|cx| cx.global::<IrohRuntimeState>().connecting);
            if connecting {
                continue;
            }
            let alive = cx
                .background_executor()
                .spawn(async {
                    crate::terminal::iroh_status().is_some()
                        && crate::terminal::remote_status().running
                })
                .await;
            if alive {
                failures = 0;
                continue;
            }
            failures = failures.saturating_add(1);
            skip = (failures - 1).min(REMOTE_WATCHDOG_MAX_SKIP);
            eprintln!("[remote] 隧道已不在运行，自动重连（第 {failures} 次）");
            cx.update(spawn_remote_bootstrap);
        }
    })
    .detach();
}

/// 定期刷新已连接设备列表，以便 UI 及时显示移动端设备的连接/断开。
fn spawn_connections_refresh(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(CONNECTIONS_REFRESH_INTERVAL)
                .await;
            let config = cx.update(|cx| cx.global::<RemoteConfig>().clone());
            // 没开远程就不查
            if !config.enabled {
                cx.update(|cx| cx.set_global(IrohConnectionsState::default()));
                continue;
            }
            let connections = cx
                .background_executor()
                .spawn(async { crate::terminal::iroh_connections() })
                .await;
            cx.update(|cx| {
                cx.set_global(IrohConnectionsState { connections });
            });
        }
    })
    .detach();
}

pub fn apply_write_toggle(enabled: bool, cx: &mut App) {
    let mut c = cx.global::<RemoteConfig>().clone();
    c.write_enabled = enabled;
    save_remote_config(&c);
    cx.set_global(c.clone());

    if !c.enabled {
        // 远程没开：只记偏好，下次打开总开关时自动带上。
        return;
    }

    // 写权限是服务端策略，热更新共享门闩即可；不重启网关/隧道，已建立的 ACP
    // WebSocket 也会立刻读取新值，避免中途切换看起来无效。控制命令在后台执行，
    // 因为 daemon 卡住时切换开关也不能拖住 UI。
    let expected = c;
    cx.spawn(async move |cx| {
        let gate = remote_control_gate();
        let expected_for_io = expected.clone();
        let Some(result) = cx
            .background_executor()
            .spawn(async move {
                let _guard = gate.lock().await;
                if !expected_for_io.enabled || !persisted_remote_config_matches(&expected_for_io) {
                    return None;
                }
                Some(terminal::remote_set_write(enabled))
            })
            .await
        else {
            return;
        };
        cx.update(|cx| {
            if !remote_config_matches(cx.global::<RemoteConfig>(), &expected) {
                return;
            }
            match result {
                Ok(()) => {
                    cx.set_global(RemoteRuntimeState::default());
                    let mut iroh = cx.global::<IrohRuntimeState>().clone();
                    iroh.write = enabled;
                    cx.set_global(iroh);
                }
                Err(error) => cx.set_global(RemoteRuntimeState { error: Some(error) }),
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// 分享卡片上的「重试」：按当前配置把网关与 iroh 隧道重新拉齐。
pub fn retry_remote_setup(cx: &mut App) {
    let c = cx.global::<RemoteConfig>().clone();
    if !c.enabled {
        return;
    }
    let expected = c;
    cx.set_global(IrohRuntimeState {
        connecting: true,
        ..Default::default()
    });
    cx.spawn(async move |cx| {
        let expected_for_io = expected.clone();
        let gate = remote_control_gate();
        let Some(result) = cx
            .background_executor()
            .spawn(async move {
                let _guard = gate.lock().await;
                let current = persisted_remote_lifecycle(&expected_for_io)?;
                if !current.enabled {
                    return None;
                }
                terminal::iroh_stop();
                // 网关先停再起，让端口与 iroh 转发目标对齐；持久化 token 保持不变。
                terminal::remote_stop();
                Some(terminal::remote_start("127.0.0.1", current.write_enabled))
            })
            .await
        else {
            return;
        };
        cx.update(|cx| {
            if !remote_lifecycle_matches(cx.global::<RemoteConfig>(), &expected) {
                return;
            }
            match result {
                Ok(_) => {
                    cx.set_global(RemoteRuntimeState::default());
                    spawn_iroh_start_public(cx);
                }
                Err(error) => {
                    cx.set_global(RemoteRuntimeState {
                        error: Some(error.clone()),
                    });
                    cx.set_global(IrohRuntimeState {
                        error: Some(error),
                        ..Default::default()
                    });
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// 用户主动刷新设备凭证。普通重试、服务重启、电脑重启和写权限切换都不会调用它。
pub fn refresh_remote_token(window: &mut Window, cx: &mut App) {
    let config = cx.global::<RemoteConfig>().clone();
    if !config.enabled {
        return;
    }
    cx.set_global(IrohRuntimeState {
        connecting: true,
        ..Default::default()
    });
    let expected = config;
    window
        .spawn(cx, async move |cx| {
            let expected_for_io = expected.clone();
            let gate = remote_control_gate();
            let Some(result) =
                cx.background_executor()
                    .spawn(async move {
                        let _guard = gate.lock().await;
                        let current = persisted_remote_lifecycle(&expected_for_io)?;
                        if !current.enabled {
                            return None;
                        }
                        Some(terminal::remote_rotate_token().and_then(|()| {
                            terminal::remote_start("127.0.0.1", current.write_enabled)
                        }))
                    })
                    .await
            else {
                return;
            };
            let _ = cx.update(|_window, cx| {
                if !remote_lifecycle_matches(cx.global::<RemoteConfig>(), &expected) {
                    return;
                }
                match result {
                    Ok(_) => {
                        cx.set_global(RemoteRuntimeState::default());
                        spawn_iroh_start_public(cx);
                        crate::status_item::notify_success(
                            "配对 Token 已刷新；旧配对已失效，请重新扫码",
                        );
                    }
                    Err(error) => {
                        cx.set_global(RemoteRuntimeState {
                            error: Some(error.clone()),
                        });
                        cx.set_global(IrohRuntimeState {
                            error: Some(error.clone()),
                            ..Default::default()
                        });
                        crate::status_item::notify_error(error);
                    }
                }
                cx.refresh_windows();
            });
        })
        .detach();
    cx.refresh_windows();
}
