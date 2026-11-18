//! 设置「远程」页。按方向分两张卡：连到其他机器（这台电脑连出去），让其他设备连进来（远程
//! 访问、已配对设备、自建 relay）。配对新设备是一次性的短任务，在对话框里做。
//!
//! 连出去的机器表归宿主，每次画都从 `Host::connections` 取。连进来的配置、状态和设备归本机
//! 运行时：页面建好、每次露出来时取一次，改了交回去；远程访问正在开启（要联网找 relay，
//! 可能十几秒）时请运行时等状态变了再回，不隔一会儿查一次。页面收起来就不再等。

use std::rc::Rc;
use std::sync::Arc;

use api::client::protocol::control::EmptyParams;
use api::model::control_api::{
    DeviceList, DevicePairBegin, DeviceRevoke, RemoteConfigGet, RemoteConfigSet, RemoteStatus,
    RemoteStatusWait,
};
use api::model::remote_config::{
    DeviceIdParams, DeviceInfo, REMOTE_STATUS_WAIT_MAX, RemoteAccessState, RemoteConfig,
    RemoteStatusWaitParams,
};
use api::ui::truncated_line::truncated_line;
use api::ui::{
    Button, Clipboard, SettingsPage, SettingsRow, SettingsSection, Switch, TextInput, dialog,
    ui_theme,
};
use api::{
    AddMachine, Connections, HostEvent, MachineTarget, PluginView, RuntimeKey, SettingsEntry,
    SettingsGroup, ViewHandle, ViewSpec,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, Entity, Image, ImageFormat, SharedString, Subscription, Task,
    WeakEntity, Window, div, img, px, rgb,
};
use rust_i18n::t;
use smelt_plugin_api::ui::field::{InputEvent, InputState};
use smelt_plugin_api::{self as api, text};

use crate::OWNER;
use crate::contract::{ADD_MACHINE, VIEW};
use crate::menu::{confirm_remove, report_failure};
use crate::status::{self, Tone};
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

// 运行时最多挂 REMOTE_STATUS_WAIT_MAX 就回，等它的读超时要比这长，不然没变化时会报超时。
const _: () = assert!(api::CALL_TIMEOUT.as_secs() > REMOTE_STATUS_WAIT_MAX.as_secs());

pub(crate) fn register(cx: &mut App) {
    let spec = ViewSpec {
        name: "remote".into(),
        title: text!("page.title"),
        icon: api::ui::IconName::Globe.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::Agents,
            keywords: [
                "多机器",
                "机器",
                "远程机器",
                "添加机器",
                "添加远程机器",
                "其他运行时",
                "配对串",
                "smeltd pair",
                "离线",
                "版本不兼容",
                "远程访问",
                "开启远程",
                "Relay 地址",
                "自建 relay",
                "手机",
                "移动端",
                "配对",
                "二维码",
                "iroh",
                "relay",
                "中继",
                "remote",
                "machines",
                "remote machine",
                "add machine",
                "pairing",
                "offline",
                "incompatible",
                "remote access",
                "phone",
                "mobile",
                "QR code",
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        }),
        fills: Vec::new(),
        build: Rc::new(|window, cx| {
            let page = cx.new(|cx| RemotePage::new(window, cx));
            page.update(cx, |page, cx| page.refresh(cx));
            Rc::new(page) as Rc<dyn ViewHandle>
        }),
    };
    if let Err(error) = api::register_views(OWNER, vec![spec], cx) {
        api::app_log::warn("machines", &format!("设置「远程」页登记失败：{error}"));
    }
}

/// [`ADD_MACHINE`]：打开「远程」页，配对串在第一组里粘。不带参数，进命令面板。
pub(crate) fn add_machine_command() -> api::CommandSpec {
    api::CommandSpec {
        name: ADD_MACHINE.name().to_string(),
        title: text!("command.add"),
        key: None,
        context: "Workspace".to_string(),
        palette: true,
        requires_args: false,
        args: Some(Vec::new()),
        cli: false,
        handler: Rc::new(|_, _, cx| {
            let opened = api::OPEN_VIEW.run(
                &api::OpenView {
                    view: VIEW.to_string(),
                    args: api::serde_json::Value::Null,
                },
                cx,
            );
            api::Handled::Done(cx.spawn(async move |_| match opened.await? {
                true => Ok(api::serde_json::Value::Null),
                false => Err(t!("error.page_unavailable").into_owned()),
            }))
        }),
    }
}

/// 生成好的配对串，二维码在后台编好：页面重绘时直接用，不每帧重算。
#[derive(Clone)]
struct Offer {
    uri: SharedString,
    qr: Option<Arc<Image>>,
}

struct RemotePage {
    /// 「添加机器」的配对串。
    pairing_input: Entity<InputState>,
    /// 「自建 relay」输入框。失焦或回车时交给运行时。
    relay_input: Entity<InputState>,
    /// 上次照配置写进 relay 输入框的值。配置里的变了（读到了、没存成重读了）才改输入框，
    /// 不然会盖掉正在打的字。
    relay_shown: Option<String>,
    adding: bool,
    add_error: Option<String>,
    /// 运行时那边的远程访问配置；还没读到是 `None`，这时开关和 relay 改不了。
    config: Option<RemoteConfig>,
    /// 每交一次配置加一。读配置时记下它，读回来时变了就是这期间改过，旧值不能盖掉改动。
    config_generation: u64,
    config_load: Option<Task<()>>,
    status: RemoteAccessState,
    devices: Vec<DeviceInfo>,
    offer: Option<Offer>,
    generating_offer: bool,
    /// 最近一次操作（保存、生成配对串、撤销、读状态）的失败原因。
    error: Option<String>,
    /// 页面眼下露着。收起再露出来时重新取一次。
    shown: bool,
    /// 正在跟着开启进度的那条等待。新的顶掉旧的：丢掉旧任务就停了它。
    status_watch: Option<Task<()>>,
    /// 配置按提交顺序一次一次交，后一次总是覆盖前一次。
    save_gate: Arc<smol::lock::Mutex<()>>,
    _subscriptions: Vec<Subscription>,
}

impl RemotePage {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pairing_input = cx.new(|cx| InputState::new(window, cx).placeholder("smelt+iroh://…"));
        let enter = cx.subscribe_in(
            &pairing_input,
            window,
            |page, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    page.add_machine(window, cx);
                }
            },
        );
        let relay_input = cx.new(|cx| InputState::new(window, cx));
        // 打字时不存：半截地址交给运行时只会连不上。
        let relay_commit = cx.subscribe(&relay_input, |page, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                let value = input.read(cx).value();
                page.set_relay(&value, cx);
            }
        });
        let events = cx.subscribe(&api::events(cx), |page, _, event: &HostEvent, cx| {
            match event {
                // 机器表归宿主，画的时候取；这里只是叫自己重画。
                HostEvent::MachinesChanged => cx.notify(),
                HostEvent::ShownViewsChanged => {
                    let shown = api::host(cx).view_shown(crate::VIEW, cx);
                    if shown && !page.shown {
                        page.refresh(cx);
                    }
                    page.shown = shown;
                }
                _ => {}
            }
        });
        Self {
            pairing_input,
            relay_input,
            relay_shown: None,
            adding: false,
            add_error: None,
            config: None,
            config_generation: 0,
            config_load: None,
            status: RemoteAccessState::Off,
            devices: Vec::new(),
            offer: None,
            generating_offer: false,
            error: None,
            // 第一次露出来时才建，建好就算露着。
            shown: true,
            status_watch: None,
            save_gate: Arc::default(),
            _subscriptions: vec![enter, relay_commit, events],
        }
    }

    /// 页面露出来：取一次远程访问的配置、状态和设备，请宿主现在重读各台机器的连接。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_config(cx);
        self.refresh_status(cx);
        self.refresh_devices(cx);
        api::REFRESH_CONNECTIONS.run(&(), cx).detach();
    }

    /// 本机运行时可能还在起来：等它在线再读，读失败就退避后回去等。
    fn load_config(&mut self, cx: &mut Context<Self>) {
        let generation = self.config_generation;
        self.config_load = Some(cx.spawn(async move |page, cx| {
            let mut failures = 0u32;
            let config = loop {
                if !api::wait_online(&RuntimeKey::Local, failures, cx).await {
                    return;
                }
                let fetched = cx
                    .update(|cx| {
                        api::call::<RemoteConfigGet>(
                            &RuntimeKey::Local,
                            EmptyParams::default(),
                            api::CALL_TIMEOUT,
                            cx,
                        )
                    })
                    .await;
                match fetched {
                    Ok(config) => break config,
                    Err(error) => {
                        api::app_log::warn("machines", &format!("读远程访问配置失败：{error}"));
                        failures = failures.saturating_add(1);
                    }
                }
            };
            let _ = page.update(cx, |page, cx| {
                if page.config_generation == generation {
                    page.config = Some(config);
                    cx.notify();
                }
            });
        }));
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let status = local_call::<RemoteStatus>(EmptyParams::default(), cx);
        cx.spawn(async move |page, cx| {
            let result = status.await;
            let _ = page.update(cx, |page, cx| {
                match result {
                    Ok(status) => page.set_status(status, cx),
                    Err(error) => page.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_status(&mut self, status: RemoteAccessState, cx: &mut Context<Self>) {
        let starting = status == RemoteAccessState::Starting;
        self.status = status;
        if starting {
            self.watch_status(cx);
        } else {
            self.status_watch = None;
        }
    }

    /// 跟着开启进度，直到开好、开不起来，或页面收起来：每次请运行时等状态和手里的不一样了
    /// 再回（最多等 [`REMOTE_STATUS_WAIT_MAX`]）。
    fn watch_status(&mut self, cx: &mut Context<Self>) {
        self.status_watch = Some(cx.spawn(async move |page, cx| {
            let mut known = RemoteAccessState::Starting;
            loop {
                let params = RemoteStatusWaitParams {
                    known: known.clone(),
                };
                let result = cx
                    .update(|cx| local_call::<RemoteStatusWait>(params, cx))
                    .await;
                let next = page
                    .update(cx, |page, cx| {
                        let status = match result {
                            Ok(status) => status,
                            Err(error) => {
                                page.error = Some(error);
                                cx.notify();
                                return None;
                            }
                        };
                        if page.status != status {
                            page.status = status.clone();
                            cx.notify();
                        }
                        let keep_waiting = status == RemoteAccessState::Starting
                            && api::host(cx).view_shown(VIEW, cx);
                        keep_waiting.then_some(status)
                    })
                    .ok()
                    .flatten();
                match next {
                    Some(status) => known = status,
                    None => return,
                }
            }
        }));
    }

    fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        let devices = local_call::<DeviceList>(EmptyParams::default(), cx);
        cx.spawn(async move |page, cx| {
            let result = devices.await;
            let _ = page.update(cx, |page, cx| {
                match result {
                    Ok(list) => page.devices = list.devices,
                    Err(error) => page.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(mut config) = self.config.clone() else {
            self.error = Some(t!("error.config_not_loaded").into_owned());
            cx.notify();
            return;
        };
        config.enabled = enabled;
        self.save(config, cx);
    }

    fn set_relay(&mut self, relay: &str, cx: &mut Context<Self>) {
        let Some(mut config) = self.config.clone() else {
            self.error = Some(t!("error.config_not_loaded").into_owned());
            cx.notify();
            return;
        };
        let relay = relay.trim();
        if config.iroh_relay == relay {
            return;
        }
        config.iroh_relay = relay.to_string();
        self.save(config, cx);
    }

    /// 交给运行时保存并照着开关。没交成就重读一次，开关和 relay 回到运行时那边的样子。
    fn save(&mut self, config: RemoteConfig, cx: &mut Context<Self>) {
        self.config_generation += 1;
        self.config_load = None;
        self.config = Some(config.clone());
        self.error = None;
        // 配置变了，旧配对串里的 endpoint 或 relay 可能已经不对。
        self.offer = None;
        cx.notify();
        let gate = self.save_gate.clone();
        let client = api::host(cx).client(&RuntimeKey::Local, cx);
        cx.spawn(async move |page, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _guard = gate.lock().await;
                    let client = client.ok_or_else(api::machine_removed)?;
                    client
                        .call::<RemoteConfigSet>(&config, api::CALL_TIMEOUT)
                        .map_err(api::call_error_text)
                })
                .await;
            let _ = page.update(cx, |page, cx| {
                match result {
                    Ok(status) => page.set_status(status, cx),
                    Err(error) => {
                        page.error = Some(t!("error.save_config", error = error).into_owned());
                        page.load_config(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 生成一次性配对串，二维码在后台编好。
    fn begin_pairing(&mut self, cx: &mut Context<Self>) {
        if self.generating_offer {
            return;
        }
        self.generating_offer = true;
        self.error = None;
        cx.notify();
        let offer = local_call::<DevicePairBegin>(EmptyParams::default(), cx);
        cx.spawn(async move |page, cx| {
            let result = match offer.await {
                Ok(offer) => {
                    let uri = offer.pairing_uri;
                    let encode = uri.clone();
                    let qr = cx
                        .background_executor()
                        .spawn(async move { status::qr_png(&encode) })
                        .await
                        .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)));
                    Ok(Offer {
                        uri: uri.into(),
                        qr,
                    })
                }
                Err(error) => Err(error),
            };
            let _ = page.update(cx, |page, cx| {
                page.generating_offer = false;
                match result {
                    Ok(offer) => page.offer = Some(offer),
                    Err(error) => {
                        page.error = Some(t!("error.pairing", error = error).into_owned())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 撤销一台设备：运行时删掉白名单并立即断开它。
    fn revoke(&mut self, endpoint_id: String, cx: &mut Context<Self>) {
        let revoked = local_call::<DeviceRevoke>(DeviceIdParams { endpoint_id }, cx);
        cx.spawn(async move |page, cx| {
            let result = revoked.await;
            let _ = page.update(cx, |page, cx| {
                if let Err(error) = result {
                    page.error = Some(t!("error.revoke", error = error).into_owned());
                    cx.notify();
                }
                page.refresh_devices(cx);
            });
        })
        .detach();
    }

    /// 用输入框里的配对串添加一台机器。成了清空输入框，机器表由宿主更新。
    fn add_machine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding {
            return;
        }
        let pairing_uri = self.pairing_input.read(cx).value().trim().to_string();
        if pairing_uri.is_empty() {
            self.add_error = Some(t!("add.empty").into_owned());
            cx.notify();
            return;
        }
        self.adding = true;
        self.add_error = None;
        cx.notify();
        let added = api::ADD_MACHINE.run(&AddMachine { pairing_uri }, cx);
        cx.spawn_in(window, async move |page, cx| {
            let result = added.await;
            let _ = page.update_in(cx, |page, window, cx| {
                page.adding = false;
                match result {
                    Ok(_) => page
                        .pairing_input
                        .update(cx, |input, cx| input.set_value("", window, cx)),
                    Err(error) => page.add_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// 在本机运行时上调一次。
fn local_call<C>(params: C::Params, cx: &App) -> Task<Result<C::Result, String>>
where
    C: api::client::protocol::control::ControlCall + 'static,
    C::Params: Send + 'static,
    C::Result: Send + 'static,
{
    api::call::<C>(&RuntimeKey::Local, params, api::CALL_TIMEOUT, cx)
}

impl PluginView for RemotePage {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, _: &mut Context<Self>) {}
}

impl Render for RemotePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 配置里的 relay 变了才写进输入框：读到配置、没存成重读之后。
        if let Some(config) = &self.config
            && self.relay_shown.as_deref() != Some(config.iroh_relay.as_str())
        {
            let relay = config.iroh_relay.clone();
            self.relay_input
                .update(cx, |input, cx| input.set_value(relay.clone(), window, cx));
            self.relay_shown = Some(relay);
        }
        let page = cx.entity().downgrade();
        let connections = api::host(cx).connections(cx);
        SettingsPage::new("machines-remote")
            .child(self.outgoing_section(&connections, &page))
            .child(self.incoming_section(&page))
    }
}

impl RemotePage {
    /// 连到其他机器。每台机器的日常操作（重新连接、重命名、中继）在侧栏机器那一行的右键
    /// 菜单里，这里管添加、看连接状态和移除。
    fn outgoing_section(
        &self,
        connections: &Connections,
        page: &WeakEntity<Self>,
    ) -> SettingsSection {
        let adding = self.adding;
        let add = page.clone();
        // 配对串长，输入框和报错占一整格，不挤在行右边。
        let add_machine = column()
            .w_full()
            .gap_2()
            .child(SettingsRow::new(t!("add.title")).description(t!("add.description")))
            .child(
                row()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TextInput::new(&self.pairing_input)),
                    )
                    .child(
                        Button::new("add-machine")
                            .small()
                            .label(if adding {
                                t!("add.adding")
                            } else {
                                t!("add.action")
                            })
                            .loading(adding)
                            .disabled(adding)
                            .on_click(move |_, window, cx| {
                                let _ = add.update(cx, |page, cx| page.add_machine(window, cx));
                            }),
                    ),
            )
            .children(self.add_error.clone().map(|error| {
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::bad()))
                    .child(error)
            }));
        SettingsSection::new()
            .title(t!("outgoing.title"))
            .description(t!("outgoing.description"))
            .child(machine_list(connections))
            .child(add_machine)
    }

    /// 让其他设备连进来：开关和状态、已配对设备、relay。
    fn incoming_section(&self, page: &WeakEntity<Self>) -> SettingsSection {
        let loaded = self.config.is_some();
        let enabled = self.config.as_ref().is_some_and(|config| config.enabled);
        let running = matches!(self.status, RemoteAccessState::Running { .. });
        let (status_text, tone) = status::access_status(&self.status);
        let status_color = tone_color(tone);
        let toggle = page.clone();
        let pair = page.clone();
        SettingsSection::new()
            .title(t!("incoming.title"))
            .description(t!("incoming.description"))
            .child(
                SettingsRow::new(t!("access.title"))
                    .description(t!("access.description"))
                    .child(
                        row().gap_1p5().child(dot(6., status_color)).child(
                            div()
                                .max_w(px(320.))
                                .overflow_hidden()
                                .type_style(TYPE.text_xsmall)
                                .text_color(status_color)
                                .child(truncated_line(status_text)),
                        ),
                    )
                    .child(
                        Switch::new("remote-access-enabled")
                            .checked(enabled)
                            .disabled(!loaded)
                            .accessibility_label(t!("access.title"))
                            .on_change(move |checked, _, cx| {
                                let _ = toggle.update(cx, |page, cx| page.set_enabled(checked, cx));
                            }),
                    ),
            )
            .child(
                SettingsRow::new(t!("devices.title"))
                    .description(t!("pairing.warning"))
                    .child(
                        Button::new("begin-pairing")
                            .small()
                            .label(t!("pairing.begin"))
                            .disabled(!running)
                            .when(!running, |button| {
                                button.tooltip(t!("pairing.needs_access"))
                            })
                            .on_click(move |_, window, cx| {
                                let _ = pair.update(cx, |page, cx| page.begin_pairing(cx));
                                open_pairing_dialog(pair.clone(), window, cx);
                            }),
                    ),
            )
            .child(device_list(&self.devices, self.error.clone(), page))
            .child(
                SettingsRow::new(t!("relay.custom"))
                    .description(t!("relay.custom_description"))
                    .child(
                        div().w(px(220.)).child(
                            TextInput::new(&self.relay_input)
                                .disabled(!loaded)
                                .aria_label(t!("relay.custom")),
                        ),
                    ),
            )
    }
}

/// 配对过的远程机器，一台一行：状态、升级、移除。
fn machine_list(connections: &Connections) -> AnyElement {
    let muted = rgb(ui_theme::muted());
    let mut list = column().gap_2();
    if let Some(error) = &connections.load_error {
        list = list.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::bad()))
                .child(error.clone()),
        );
    }
    if connections.remotes.is_empty() {
        if connections.load_error.is_none() {
            list = list.child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(muted)
                    .child(t!("outgoing.empty").into_owned()),
            );
        }
        return list.into_any_element();
    }
    for (index, connection) in connections.remotes.iter().enumerate() {
        let (text, tone) = status::machine_status(connection);
        let online = matches!(connection.state, api::ConnectionState::Online { .. });
        let upgrade = connection.machine.clone();
        let machine = connection.machine.clone();
        let name = connection.name.clone();
        list = list.child(
            row()
                .gap_2()
                .child(dot(8., if online { rgb(ui_theme::ok()) } else { muted }))
                .child(
                    column()
                        .flex_1()
                        .min_w(px(0.))
                        .gap_0p5()
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::text()))
                                .child(connection.name.clone()),
                        )
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(tone_color(tone))
                                .child(text),
                        )
                        .children(connection.upgrade_note.clone().map(|note| {
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::need()))
                                .child(note)
                        })),
                )
                .when(connection.can_upgrade, |line| {
                    let version =
                        api::smelt_version::RELEASE.unwrap_or(api::smelt_version::VERSION);
                    line.child(
                        Button::new(("upgrade-machine", index))
                            .small()
                            .label(t!("upgrade.action"))
                            .tooltip(t!("upgrade.tooltip", version = version))
                            .on_click(move |_, _, cx| {
                                let target = MachineTarget {
                                    machine: upgrade.clone(),
                                };
                                let upgraded = api::UPGRADE_MACHINE.run(&target, cx);
                                report_failure(t!("upgrade.failed").into_owned(), upgraded, cx);
                            }),
                    )
                })
                .child(
                    Button::new(("remove-machine", index))
                        .small()
                        .label(t!("remove.action"))
                        .on_click(move |_, window, cx| {
                            confirm_remove(machine.clone(), name.clone(), window, cx)
                        }),
                ),
        );
    }
    list.into_any_element()
}

/// 已配对的设备，一台一行；最近一次操作没成的原因写在最上面。
fn device_list(
    devices: &[DeviceInfo],
    error: Option<String>,
    page: &WeakEntity<RemotePage>,
) -> AnyElement {
    let muted = rgb(ui_theme::muted());
    let mut list = column().gap_2().children(error.map(|error| {
        div()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::bad()))
            .child(error)
    }));
    if devices.is_empty() {
        return list
            .child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(t!("devices.empty").into_owned()),
            )
            .into_any_element();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default();
    for (index, device) in devices.iter().enumerate() {
        let endpoint_id = device.endpoint_id.clone();
        let page = page.clone();
        list = list.child(
            row()
                .gap_2()
                .child(dot(
                    8.,
                    if device.connected {
                        rgb(ui_theme::ok())
                    } else {
                        muted
                    },
                ))
                .child(
                    column()
                        .flex_1()
                        .min_w(px(0.))
                        .gap_0p5()
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(rgb(ui_theme::text()))
                                .child(device.name.clone()),
                        )
                        .child(div().type_style(TYPE.text_xsmall).text_color(muted).child(
                            format!(
                                "{} · {}",
                                status::short_id(&device.endpoint_id),
                                status::last_seen_text(device, now_ms)
                            ),
                        )),
                )
                .child(
                    Button::new(("revoke-device", index))
                        .small()
                        .label(t!("devices.revoke"))
                        .on_click(move |_, _, cx| {
                            let endpoint_id = endpoint_id.clone();
                            let _ = page.update(cx, |page, cx| page.revoke(endpoint_id, cx));
                        }),
                ),
        );
    }
    list.into_any_element()
}

/// 配对新设备的对话框。内容每次重绘都从页面读：生成中、出错、二维码。点「完成」时重读设备，
/// 刚配上的那台就出现在列表里。
fn open_pairing_dialog(page: WeakEntity<RemotePage>, window: &mut Window, cx: &mut App) {
    dialog::open(window, cx, move |card, _, cx| {
        pairing_dialog(card, &page, cx)
    });
}

fn pairing_dialog(card: dialog::Card, page: &WeakEntity<RemotePage>, cx: &App) -> dialog::Card {
    let (offer, generating, error) = page
        .upgrade()
        .map(|page| {
            let page = page.read(cx);
            (
                page.offer.clone(),
                page.generating_offer,
                page.error.clone(),
            )
        })
        .unwrap_or_default();
    let muted = rgb(ui_theme::muted());
    let mut body = dialog::content(t!("pairing.title"), None).child(
        div()
            .type_style(TYPE.ui)
            .text_color(rgb(ui_theme::need()))
            .child(t!("pairing.warning").into_owned()),
    );
    if let Some(error) = error {
        body = body.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::bad()))
                .child(error),
        );
    }
    body = match offer {
        None => body.child(
            div()
                .type_style(TYPE.ui)
                .text_color(muted)
                .child(if generating {
                    t!("pairing.generating").into_owned()
                } else {
                    t!("pairing.none").into_owned()
                }),
        ),
        Some(offer) => body.child(
            row()
                .items_start()
                .gap_3()
                .children(offer.qr.map(|image| {
                    div()
                        .flex_none()
                        .p_2()
                        .rounded(px(8.))
                        // 二维码底必须是纯白，两种主题都一样：深色底上的二维码扫不出来。
                        .bg(rgb(0xffffff))
                        .child(img(image).w(px(132.)).h(px(132.)))
                }))
                .child(
                    column()
                        .flex_1()
                        .min_w(px(0.))
                        .gap_2()
                        .child(
                            div()
                                .type_style(TYPE.ui)
                                .text_color(rgb(ui_theme::text()))
                                .child(t!("pairing.instructions").into_owned()),
                        )
                        .child(
                            row()
                                .gap_1()
                                .child(
                                    div()
                                        .max_w(px(280.))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis_middle()
                                        .type_style(TYPE.text_xsmall)
                                        .text_color(muted)
                                        .child(offer.uri.clone()),
                                )
                                .child(
                                    Clipboard::new("copy-pairing-uri")
                                        .value(offer.uri)
                                        .tooltip(t!("pairing.copy"))
                                        .accessibility_label(t!("pairing.copy")),
                                ),
                        )
                        .child(
                            div()
                                .type_style(TYPE.text_xsmall)
                                .text_color(muted)
                                .child(t!("pairing.expiry").into_owned()),
                        ),
                ),
        ),
    };
    let regenerate = page.clone();
    let done = page.clone();
    card.w(px(520.)).child(body).footer(
        dialog::footer()
            .child(
                Button::new("regenerate-pairing")
                    .quiet()
                    .small()
                    .label(t!("pairing.regenerate"))
                    .disabled(generating)
                    .on_click(move |_, _, cx| {
                        let _ = regenerate.update(cx, |page, cx| page.begin_pairing(cx));
                    }),
            )
            .child(
                Button::new("close-pairing")
                    .small()
                    .label(t!("pairing.done"))
                    .on_click(move |_, window, cx| {
                        dialog::close(window, cx);
                        let _ = done.update(cx, |page, cx| page.refresh_devices(cx));
                    }),
            ),
    )
}

fn row() -> Div {
    div().flex().flex_row().items_center()
}

fn column() -> Div {
    div().flex().flex_col()
}

fn dot(size: f32, color: gpui::Rgba) -> Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}

fn tone_color(tone: Tone) -> gpui::Rgba {
    rgb(match tone {
        Tone::Muted => ui_theme::muted(),
        Tone::Ok => ui_theme::ok(),
        Tone::Warning => ui_theme::need(),
        Tone::Bad => ui_theme::bad(),
    })
}
