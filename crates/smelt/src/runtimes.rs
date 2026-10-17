//! 桌面挂着的运行时（设计文档 D6）。本机运行时总在；远程运行时来自客户端库里登记的
//! 连接。会话、视图各自记着自己属于哪个运行时，调用、连接都发给它。

use smelt_runtime_client::RuntimeClient;

pub(crate) use smelt_client::runtime::{Runtime, RuntimeKey};

/// 本机运行时的客户端。连本机 socket；守护的拉起和升级另由 `terminal` 管。
pub(crate) fn local() -> RuntimeClient {
    Runtime::local().client().clone()
}

/// 给终端开一条连接。本机守护没在跑就先拉起来，这只有本机做得到；远程运行时连不上
/// 就是连不上，由调用方按断线处理。
pub(crate) fn connect_terminal(
    runtime: &Runtime,
) -> std::io::Result<std::os::unix::net::UnixStream> {
    if runtime.is_local() {
        crate::terminal::connect_daemon()
    } else {
        runtime.client().connect()
    }
}

fn remote_runtime(runtime_id: &str, client: RuntimeClient) -> Runtime {
    Runtime::new(RuntimeKey::Remote(runtime_id.to_string()), client)
}

/// 远程运行时现在连不连得上。
#[derive(Clone, Debug)]
pub(crate) enum RuntimeStatus {
    Connecting,
    Online {
        version: String,
        platform: Option<String>,
    },
    Offline(String),
}

/// 一个登记过的远程运行时。
#[derive(Clone)]
pub(crate) struct RemoteRuntime {
    pub(crate) connection: smelt_client_store::RuntimeConnection,
    pub(crate) runtime: Runtime,
    pub(crate) status: RuntimeStatus,
}

/// 桌面挂着的远程运行时，以及「添加运行时」的输入和进度。设置页只读它。
#[derive(Clone, Default)]
pub(crate) struct RuntimeHub {
    pub(crate) remotes: Vec<RemoteRuntime>,
    /// 设置页里正在输入的配对串。
    pub(crate) pairing_input: String,
    pub(crate) adding: bool,
    /// 最近一次添加、移除的失败原因。
    pub(crate) error: Option<String>,
}

impl gpui::Global for RuntimeHub {}

/// 这台桌面的 iroh 身份，所有远程运行时共用。绑定要起 tokio 运行时，只能在后台调。
fn iroh_client() -> Result<std::sync::Arc<smelt_iroh::runtime::IrohClient>, String> {
    static CLIENT: std::sync::Mutex<Option<std::sync::Arc<smelt_iroh::runtime::IrohClient>>> =
        std::sync::Mutex::new(None);
    let mut slot = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(client) = slot.as_ref() {
        return Ok(std::sync::Arc::clone(client));
    }
    let secret = smelt_iroh::default_client_secret_path()
        .and_then(|path| smelt_iroh::load_or_create_secret(&path))
        .map_err(|error| format!("读取本机设备身份失败：{error:#}"))?;
    let client = smelt_iroh::runtime::IrohClient::new(secret)
        .map_err(|error| format!("起不了 iroh：{error:#}"))?;
    *slot = Some(std::sync::Arc::clone(&client));
    Ok(client)
}

fn remote_client(
    connection: &smelt_client_store::RuntimeConnection,
) -> Result<RuntimeClient, String> {
    let endpoint: smelt_iroh::EndpointId = connection
        .endpoint_id
        .parse()
        .map_err(|_| format!("运行时的 endpoint id 不合法：{}", connection.endpoint_id))?;
    let relay = connection
        .relay_url
        .as_deref()
        .map(smelt_iroh::RelaySettings::parse)
        .transpose()
        .map_err(|error| format!("relay 地址不合法：{error:#}"))?;
    Ok(RuntimeClient::new(
        iroh_client()?.connector(endpoint, relay.as_ref()),
    ))
}

fn status_of(client: &RuntimeClient) -> RuntimeStatus {
    match client.describe() {
        Ok(described) => RuntimeStatus::Online {
            version: described.daemon.version,
            platform: described
                .daemon
                .os
                .zip(described.daemon.arch)
                .map(|(os, arch)| format!("{os}/{arch}")),
        },
        Err(error) => RuntimeStatus::Offline(error.to_string()),
    }
}

fn update_hub(cx: &mut gpui::App, change: impl FnOnce(&mut RuntimeHub)) {
    let mut hub = cx.try_global::<RuntimeHub>().cloned().unwrap_or_default();
    change(&mut hub);
    cx.set_global(hub);
}

/// 启动时读出登记过的远程运行时并探一次状态。读库、绑 iroh、拨号都在后台。
pub(crate) fn start_runtime_hub(cx: &mut gpui::App) {
    cx.set_global(RuntimeHub::default());
    cx.spawn(async move |cx| {
        let loaded = cx
            .background_executor()
            .spawn(async {
                let connections = smelt_client_store::client_store()
                    .and_then(|store| store.runtime_connections())
                    .map_err(|error| format!("读取运行时列表失败：{error}"))?;
                Ok::<_, String>(
                    connections
                        .into_iter()
                        .map(|connection| match remote_client(&connection) {
                            Ok(client) => RemoteRuntime {
                                runtime: remote_runtime(&connection.runtime_id, client),
                                connection,
                                status: RuntimeStatus::Connecting,
                            },
                            Err(error) => RemoteRuntime {
                                runtime: remote_runtime(
                                    &connection.runtime_id,
                                    RuntimeClient::new(move || {
                                        Err(std::io::Error::other("运行时配置无效"))
                                    }),
                                ),
                                connection,
                                status: RuntimeStatus::Offline(error),
                            },
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .await;
        cx.update(|cx| match loaded {
            Ok(remotes) => {
                update_hub(cx, |hub| hub.remotes = remotes);
                refresh_runtime_status(cx);
            }
            Err(error) => update_hub(cx, |hub| hub.error = Some(error)),
        });
    })
    .detach();
}

/// 重新探一遍每个远程运行时（打开设置窗口时调）。
pub(crate) fn refresh_runtime_status(cx: &mut gpui::App) {
    let Some(hub) = cx.try_global::<RuntimeHub>() else {
        return;
    };
    for remote in hub.remotes.clone() {
        let runtime_id = remote.connection.runtime_id.clone();
        let client = remote.runtime.client().clone();
        cx.spawn(async move |cx| {
            let status = cx
                .background_executor()
                .spawn(async move { status_of(&client) })
                .await;
            cx.update(|cx| {
                update_hub(cx, |hub| {
                    if let Some(remote) = hub
                        .remotes
                        .iter_mut()
                        .find(|remote| remote.connection.runtime_id == runtime_id)
                    {
                        remote.status = status;
                    }
                })
            });
        })
        .detach();
    }
}

pub(crate) fn set_pairing_input(value: &str, cx: &mut gpui::App) {
    let value = value.trim().to_string();
    update_hub(cx, |hub| hub.pairing_input = value);
}

/// 用配对串添加一个远程运行时：以这台机器的主机名配对，问出它的 runtime_id 和主机名，
/// 记进客户端库。同一个运行时重新配对就更新原来那条。
pub(crate) fn add_runtime(cx: &mut gpui::App) {
    let uri = cx
        .try_global::<RuntimeHub>()
        .map(|hub| hub.pairing_input.clone())
        .unwrap_or_default();
    if uri.is_empty() {
        return;
    }
    update_hub(cx, |hub| {
        hub.adding = true;
        hub.error = None;
    });
    cx.spawn(async move |cx| {
        let added = cx
            .background_executor()
            .spawn(async move { pair_with(&uri) })
            .await;
        cx.update(|cx| {
            update_hub(cx, |hub| {
                hub.adding = false;
                match added {
                    Ok(remote) => {
                        hub.pairing_input.clear();
                        hub.remotes.retain(|known| {
                            known.connection.runtime_id != remote.connection.runtime_id
                        });
                        hub.remotes.push(remote);
                    }
                    Err(error) => hub.error = Some(error),
                }
            })
        });
    })
    .detach();
}

fn pair_with(uri: &str) -> Result<RemoteRuntime, String> {
    use smelt_model::control_api::DevicePair;
    use smelt_model::remote_config::PairParams;

    let pairing = smelt_pairing::parse_pairing_uri(uri)?;
    let mut connection = smelt_client_store::RuntimeConnection {
        runtime_id: String::new(),
        name: String::new(),
        endpoint_id: pairing.endpoint_id,
        relay_url: pairing.relay_url,
        added_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default(),
    };
    let client = remote_client(&connection)?;
    client
        .call::<DevicePair>(
            &PairParams {
                code: pairing.code,
                name: smelt_sys::hostname().unwrap_or_else(|| "Smelt 桌面".to_string()),
            },
            PAIR_TIMEOUT,
        )
        .map_err(|error| format!("配对失败：{error}"))?;
    let described = client
        .describe()
        .map_err(|error| format!("配对成功，但读取运行时信息失败：{error}"))?;
    connection.runtime_id = described
        .daemon
        .runtime_id
        .ok_or_else(|| "这个运行时太旧，没有 runtime_id，先升级它".to_string())?;
    connection.name = described
        .daemon
        .hostname
        .clone()
        .unwrap_or_else(|| connection.endpoint_id.chars().take(8).collect());
    smelt_client_store::client_store()
        .and_then(|store| store.put_runtime_connection(&connection))
        .map_err(|error| format!("保存运行时失败：{error}"))?;
    Ok(RemoteRuntime {
        runtime: remote_runtime(&connection.runtime_id, client),
        status: RuntimeStatus::Online {
            version: described.daemon.version,
            platform: described
                .daemon
                .os
                .zip(described.daemon.arch)
                .map(|(os, arch)| format!("{os}/{arch}")),
        },
        connection,
    })
}

/// 配对要走公网拨号，比本机调用慢得多。
const PAIR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 移除一个远程运行时：先请它撤销这台设备（连不上就算了），再从客户端库删掉。
pub(crate) fn remove_runtime(runtime_id: String, cx: &mut gpui::App) {
    let Some(remote) = cx.try_global::<RuntimeHub>().and_then(|hub| {
        hub.remotes
            .iter()
            .find(|remote| remote.connection.runtime_id == runtime_id)
            .cloned()
    }) else {
        return;
    };
    cx.spawn(async move |cx| {
        let removed = cx
            .background_executor()
            .spawn(async move {
                if let Ok(client) = iroh_client() {
                    let _ = remote
                        .runtime
                        .client()
                        .call::<smelt_model::control_api::DeviceRevoke>(
                            &smelt_model::remote_config::DeviceIdParams {
                                endpoint_id: client.id().to_string(),
                            },
                            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
                        );
                }
                smelt_client_store::client_store()
                    .and_then(|store| store.delete_runtime_connection(&runtime_id))
                    .map(|_| runtime_id)
                    .map_err(|error| format!("移除运行时失败：{error}"))
            })
            .await;
        cx.update(|cx| {
            update_hub(cx, |hub| match removed {
                Ok(runtime_id) => hub
                    .remotes
                    .retain(|remote| remote.connection.runtime_id != runtime_id),
                Err(error) => hub.error = Some(error),
            })
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 对着一台真实运行时走一遍「添加运行时」：配对、问出身份、记进客户端库，再连一次。
    /// 要联网、要现成的配对串，默认不跑：
    /// `SMELT_TEST_PAIRING_URI='smelt+iroh://…' cargo test -p smelt runtimes -- --ignored`
    #[test]
    #[ignore = "需要一台真实运行时和一次性配对串"]
    fn pairs_with_a_real_runtime() {
        let uri = std::env::var("SMELT_TEST_PAIRING_URI").expect("设置 SMELT_TEST_PAIRING_URI");
        let home =
            std::env::temp_dir().join(format!("smelt-pair-test-{}", uuid::Uuid::new_v4().simple()));
        // SAFETY: nextest 每个测试一个进程，这里没有别的线程在读环境变量。
        unsafe { std::env::set_var("SMELT_HOME", &home) };

        let remote = pair_with(&uri).unwrap();
        assert!(matches!(remote.status, RuntimeStatus::Online { .. }));
        let stored = smelt_client_store::client_store()
            .unwrap()
            .runtime_connections()
            .unwrap();
        assert_eq!(stored, vec![remote.connection.clone()]);

        let again = remote_client(&stored[0]).unwrap().describe().unwrap();
        assert_eq!(
            again.daemon.runtime_id.as_deref(),
            Some(stored[0].runtime_id.as_str())
        );
        eprintln!("已添加 {}（{}）", stored[0].name, stored[0].runtime_id);
        let _ = std::fs::remove_dir_all(home);
    }
}
