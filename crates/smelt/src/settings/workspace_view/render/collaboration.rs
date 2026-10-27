//! 设置页：远程。按方向分两张卡：连到其他机器（这台桌面连出去），让其他设备连进来（远程
//! 访问、已配对设备、relay）。配对新设备是一次性的短任务，在对话框里做。

use super::*;
use gpui_component::dialog::Dialog;
use gpui_component::input::{Input, InputEvent, InputState};
use smelt_model::remote_config::RemoteAccessState;

fn short_id(id: &str) -> String {
    if id.len() > 16 {
        format!("{}…{}", &id[..8], &id[id.len() - 8..])
    } else {
        id.to_string()
    }
}

fn last_seen_text(device: &smelt_model::remote_config::DeviceInfo) -> String {
    if device.connected {
        return "在线".to_string();
    }
    let Some(at) = device.last_seen_at_ms else {
        return "还没连过".to_string();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(at);
    let secs = (now - at).max(0) / 1000;
    if secs < 3600 {
        format!("{} 分钟前连过", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{} 小时前连过", secs / 3600)
    } else {
        format!("{} 天前连过", secs / 86_400)
    }
}

pub(super) fn remote_page(
    _entity: Entity<Workspace>,
    _snapshot: &SettingsRenderSnapshot,
    _cx: &App,
) -> SettingPage {
    SettingPage::new("远程")
        .resettable(false)
        .group(outgoing_group())
        .group(incoming_group())
}

/// 连到其他机器。每台机器的日常操作（打开项目、远程桌面、重命名、中继）在侧栏它那一段
/// 标题的 `⋯` 菜单里，这里管添加、看连接状态和移除。
fn outgoing_group() -> SettingGroup {
    SettingGroup::new()
        .title("连到其他机器")
        .description("在别的机器上开项目、跑会话。")
        .item(
        SettingItem::render(move |_, _, cx: &mut App| {
            let hub = cx
                .try_global::<crate::runtimes::RuntimeHub>()
                .cloned()
                .unwrap_or_default();
            let muted = cx.theme().muted_foreground;
            let danger = cx.theme().danger;
            let fg = cx.theme().foreground;
            let online = gpui::rgb(crate::ui_theme::green());
            let warning = cx.theme().warning;

            let mut list = v_flex().gap_2();
            for error in [hub.load_error.clone(), hub.error.clone()].into_iter().flatten() {
                list = list.child(div().text_xs().text_color(danger).child(error));
            }
            if hub.remotes.is_empty() {
                if hub.load_error.is_none() {
                    list = list
                        .child(div().text_sm().text_color(muted).child("还没有连到其他机器。"));
                }
                return list.into_any_element();
            }
            for (index, remote) in hub.remotes.iter().enumerate() {
                let runtime_id = remote.connection.runtime_id.clone();
                let key = remote.runtime.key().clone();
                // 跟侧栏机器菜单同一个判断：连得上、而且能远程升级。Windows、macOS 上的运行时
                // 随那台机器上的 Smelt 一起更新，不给按钮。
                let can_upgrade = crate::runtimes::runtime_can_upgrade(&key, cx);
                // 在线时再写走哪条路、多慢；慢了标黄。
                let (link, link_tone) =
                    crate::runtimes::runtime_link_detail(remote.runtime.key(), cx);
                let slow = link_tone == crate::runtimes::LinkTone::Warning;
                let (status, is_online) = match &remote.status {
                    crate::runtimes::RuntimeStatus::Connecting => ("正在连接…".to_string(), false),
                    crate::runtimes::RuntimeStatus::Online {
                        version, platform, ..
                    } => {
                        let mut status = match platform {
                            Some(platform) => format!("在线 · {version} · {platform}"),
                            None => format!("在线 · {version}"),
                        };
                        if link != "在线" {
                            status = format!("{status} · {}", link.trim_start_matches("在线 · "));
                        }
                        (status, true)
                    }
                    crate::runtimes::RuntimeStatus::Offline(error) => (format!("离线：{error}"), false),
                    crate::runtimes::RuntimeStatus::Incompatible { detail, .. } => {
                        (format!("版本不兼容：{detail}"), false)
                    }
                    crate::runtimes::RuntimeStatus::Upgrading(progress) => {
                        (format!("升级中：{progress}"), false)
                    }
                };
                list = list.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().size(px(8.)).rounded_full().when_else(
                            is_online,
                            |dot| dot.bg(online),
                            |dot| dot.bg(muted),
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(fg)
                                        .child(remote.connection.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if is_online && slow { warning } else { muted })
                                        .child(status),
                                )
                                .children(remote.upgrade_note.clone().map(|note| {
                                    div().text_xs().text_color(warning).child(note)
                                })),
                        )
                        .when(can_upgrade, |row| {
                            row.child(
                                Button::new(("upgrade-runtime", index))
                                    .compact()
                                    .label("升级")
                                    .tooltip(format!(
                                        "请这台机器升到 v{}",
                                        crate::runtimes::DESKTOP_VERSION
                                    ))
                                    .on_click(move |_, _window, cx: &mut App| {
                                        crate::runtime_events::upgrade_remote(&key, cx);
                                    }),
                            )
                        })
                        .child(
                            Button::new(("remove-runtime", index))
                                .compact()
                                .label("移除")
                                .on_click(move |_, _window, cx: &mut App| {
                                    crate::runtimes::remove_runtime(runtime_id.clone(), cx);
                                }),
                        ),
                );
            }
            list.into_any_element()
        }),
        )
        .item(
            SettingItem::new(
                "添加机器",
                SettingField::render(|_, window, cx: &mut App| {
                    let input = window.use_keyed_state("settings-pairing-input", cx, |window, cx| {
                        let pairing = cx
                            .try_global::<crate::runtimes::RuntimeHub>()
                            .map(|hub| hub.pairing_input.clone())
                            .unwrap_or_default();
                        PairingInput::new(pairing, window, cx)
                    });
                    let input = input.read(cx).input.clone();
                    let hub = cx
                        .try_global::<crate::runtimes::RuntimeHub>()
                        .cloned()
                        .unwrap_or_default();
                    // 配对成功后运行时那边清掉了配对串，输入框跟着清。
                    if hub.pairing_input.is_empty() && !input.read(cx).value().is_empty() {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().w(px(260.)).child(Input::new(&input)))
                        .child(
                            Button::new("add-runtime")
                                .compact()
                                .label(if hub.adding { "正在配对…" } else { "添加" })
                                .loading(hub.adding)
                                .disabled(hub.adding)
                                .on_click(|_, _window, cx: &mut App| {
                                    crate::runtimes::add_runtime(cx)
                                }),
                        )
                }),
            )
            .description(
                "粘贴那台机器的配对串：在它上面执行 smeltd pair，或在它的「让其他设备连进来」里生成。",
            )
            .keywords(["配对串", "添加机器", "添加运行时"]),
        )
}

/// 「添加机器」的输入框。改动写回 RuntimeHub，添加按钮从那里取。挂在设置窗口的元素状态上。
struct PairingInput {
    input: Entity<InputState>,
    _change: Subscription,
}

impl PairingInput {
    fn new(value: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("smelt://pair/…")
                .default_value(value)
        });
        let _change = cx.subscribe(&input, |_, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = input.read(cx).value();
                crate::runtimes::set_pairing_input(&value, cx);
            }
        });
        Self { input, _change }
    }
}

/// 让其他设备连进来：开关和状态、已配对设备、relay。
fn incoming_group() -> SettingGroup {
    SettingGroup::new()
        .title("让其他设备连进来")
        .description("配对过的电脑和手机可以经 iroh 连这台机器：能直连就直连，连不上走 relay。")
        .item(
            SettingItem::new(
                "远程访问",
                SettingField::render(|_, _, cx: &mut App| {
                    let state = cx
                        .try_global::<RemoteSettingsState>()
                        .cloned()
                        .unwrap_or_default();
                    let enabled = cx.global::<RemoteConfig>().enabled;
                    let muted = cx.theme().muted_foreground;
                    let (text, color) = match &state.status {
                        RemoteAccessState::Off => ("已关闭".to_string(), muted),
                        RemoteAccessState::Starting => ("正在开启…".to_string(), muted),
                        RemoteAccessState::Running { endpoint_id, relay } => (
                            format!(
                                "已开启 · {} · {}",
                                relay.as_deref().unwrap_or("公共 relay"),
                                short_id(endpoint_id)
                            ),
                            rgb(crate::ui_theme::green()).into(),
                        ),
                        RemoteAccessState::Failed { error } => {
                            (format!("开不起来，正在重试：{error}"), cx.theme().danger)
                        }
                    };
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(div().size(px(6.)).rounded_full().bg(color))
                                .child(
                                    div()
                                        .max_w(px(320.))
                                        .truncate()
                                        .text_xs()
                                        .text_color(color)
                                        .child(text),
                                ),
                        )
                        .child(
                            Switch::new("remote-access-enabled")
                                .checked(enabled)
                                .accessibility_label("远程访问")
                                .on_click(|checked, _window, cx: &mut App| {
                                    apply_remote_toggle(*checked, cx)
                                }),
                        )
                }),
            )
            .description("电池供电时不阻止休眠，只有电脑醒着才能连。")
            .keywords(["开启远程访问", "远程访问", "iroh"]),
        )
        .item(
            SettingItem::new(
                "已配对的设备",
                SettingField::render(|_, _, cx: &mut App| {
                    let state = cx
                        .try_global::<RemoteSettingsState>()
                        .cloned()
                        .unwrap_or_default();
                    let running = matches!(state.status, RemoteAccessState::Running { .. });
                    Button::new("begin-pairing")
                        .compact()
                        .label("配对新设备…")
                        .disabled(!running)
                        .when(!running, |button| button.tooltip("先开启远程访问"))
                        .on_click(|_, window, cx: &mut App| {
                            begin_pairing(cx);
                            window.open_dialog(cx, |dialog, _, cx| pairing_dialog(dialog, cx));
                        })
                }),
            )
            .description("配对一台设备，就是把你在这台机器上的 shell 权限交给它。"),
        )
        .item(SettingItem::render(move |_, _, cx: &mut App| {
            let state = cx
                .try_global::<RemoteSettingsState>()
                .cloned()
                .unwrap_or_default();
            let muted = cx.theme().muted_foreground;
            let fg = cx.theme().foreground;
            let online = gpui::rgb(crate::ui_theme::green());

            if state.devices.is_empty() {
                return div()
                    .text_xs()
                    .text_color(muted)
                    .child("还没有配对过设备。")
                    .into_any_element();
            }
            let mut list = v_flex().gap_2();
            for (index, device) in state.devices.iter().enumerate() {
                let endpoint_id = device.endpoint_id.clone();
                list = list.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().size(px(8.)).rounded_full().when_else(
                            device.connected,
                            |dot| dot.bg(online),
                            |dot| dot.bg(muted),
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .gap_0p5()
                                .child(div().text_xs().text_color(fg).child(device.name.clone()))
                                .child(div().text_xs().text_color(muted).child(format!(
                                    "{} · {}",
                                    short_id(&device.endpoint_id),
                                    last_seen_text(device)
                                ))),
                        )
                        .child(
                            Button::new(("revoke-device", index))
                                .compact()
                                .label("撤销")
                                .on_click(move |_, _window, cx: &mut App| {
                                    revoke_device(endpoint_id.clone(), cx);
                                }),
                        ),
                );
            }
            list.into_any_element()
        }))
        .item(
            SettingItem::new(
                "自建 relay",
                SettingField::input(
                    |cx: &App| cx.global::<RemoteConfig>().iroh_relay.clone().into(),
                    |v: SharedString, cx: &mut App| apply_iroh_relay_value(v, cx),
                ),
            )
            .description(
                "留空用 n0 的公共 relay；填了域名、IP 或 URL 就只用它，省略协议时用 https://。",
            ),
        )
}

/// 配对新设备的对话框。内容每次重绘都从 RemoteSettingsState 读：生成中、出错、二维码。
fn pairing_dialog(dialog: Dialog, cx: &mut App) -> Dialog {
    let state = cx
        .try_global::<RemoteSettingsState>()
        .cloned()
        .unwrap_or_default();
    let muted = cx.theme().muted_foreground;
    let mut body = v_flex().gap_3().child(
        div()
            .text_sm()
            .text_color(cx.theme().warning)
            .child("配对一台设备，就是把你在这台机器上的 shell 权限交给它。"),
    );
    if let Some(error) = state.error.clone() {
        body = body.child(div().text_xs().text_color(cx.theme().danger).child(error));
    }
    body =
        match state.offer.clone() {
            None => body.child(div().text_sm().text_color(muted).child(
                if state.generating_offer {
                    "正在生成配对串…"
                } else {
                    "还没有配对串。"
                },
            )),
            Some(offer) => {
                let uri = offer.uri.clone();
                let mut row = h_flex().items_start().gap_3();
                if let Some(image) = offer.qr_image {
                    row = row.child(
                        div()
                        .p_2()
                        .rounded(px(8.))
                        // 二维码底必须是纯白，两种主题都一样：深色底上的二维码扫不出来。
                        .bg(gpui::rgb(0xffffff))
                        .child(img(image).w(px(132.)).h(px(132.))),
                    );
                }
                body.child(
                    row.child(
                        v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap_2()
                            .child(div().text_sm().child(
                                "手机扫码；或复制配对串，在另一台电脑的「连到其他机器」里粘贴。",
                            ))
                            .child(
                                div()
                                    .max_w(px(280.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_middle()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(offer.uri),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("只能用一次，10 分钟内有效。"),
                            )
                            .child(
                                h_flex().gap_2().child(
                                    Button::new("copy-pairing-uri")
                                        .compact()
                                        .label(copy_btn_label("copy-pairing-uri", "复制配对串", cx))
                                        .on_click(move |_, _window, cx: &mut App| {
                                            copy_with_feedback(uri.clone(), "copy-pairing-uri", cx);
                                        }),
                                ),
                            ),
                    ),
                )
            }
        };
    dialog.title("配对新设备").w(px(520.)).child(body).footer(
        h_flex()
            .w_full()
            .justify_end()
            .gap_2()
            .child(
                Button::new("regenerate-pairing")
                    .quiet()
                    .compact()
                    .label("换一个配对串")
                    .disabled(state.generating_offer)
                    .on_click(|_, _window, cx: &mut App| begin_pairing(cx)),
            )
            .child(
                Button::new("close-pairing")
                    .compact()
                    .label("完成")
                    .on_click(|_, window, cx: &mut App| window.close_dialog(cx)),
            ),
    )
}
