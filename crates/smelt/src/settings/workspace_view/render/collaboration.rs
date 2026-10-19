//! 设置页：远程访问与设备。

use super::*;
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
    let access_group = SettingGroup::new().items(vec![
        SettingItem::new(
            "开启远程访问",
            SettingField::switch(
                |cx: &App| cx.global::<RemoteConfig>().enabled,
                |v: bool, cx: &mut App| apply_remote_toggle(v, cx),
            ),
        )
        .description(
            "打开后，配对过的设备可以经 iroh 连这台机器的运行时：优先直连，连不上自动走 relay。\
             插电时电脑保持可连；电池供电不阻止休眠，只有电脑醒着时才能连。",
        ),
        SettingItem::new(
            "自建 relay",
            SettingField::input(
                |cx: &App| cx.global::<RemoteConfig>().iroh_relay.clone().into(),
                |v: SharedString, cx: &mut App| apply_iroh_relay_value(v, cx),
            ),
        )
        .description(
            "留空使用 n0 的公共 relay 和地址发现，只凭配对串就能找到这台机器。\
             填写自建 relay 的域名、IP 或完整 URL 则只用它；省略协议时使用 https://。",
        ),
        SettingItem::render(move |_, _, cx: &mut App| {
            let state = cx
                .try_global::<RemoteSettingsState>()
                .cloned()
                .unwrap_or_default();
            let muted = cx.theme().muted_foreground;
            let danger = cx.theme().danger;
            let (text, color) = match &state.status {
                RemoteAccessState::Off => ("远程访问已关闭".to_string(), muted),
                RemoteAccessState::Starting => {
                    ("正在开启…（连接 relay、发布地址）".to_string(), muted)
                }
                RemoteAccessState::Running { endpoint_id, relay } => (
                    format!(
                        "已开启 · {} · {}",
                        short_id(endpoint_id),
                        relay.as_deref().unwrap_or("公共 relay")
                    ),
                    muted,
                ),
                RemoteAccessState::Failed { error } => {
                    (format!("开不起来，正在重试：{error}"), danger)
                }
            };
            div()
                .text_xs()
                .text_color(color)
                .child(text)
                .into_any_element()
        }),
    ]);

    let devices_group = SettingGroup::new().title("设备").items(vec![
        // 配对新设备
        SettingItem::render(move |_, _, cx: &mut App| {
            let state = cx
                .try_global::<RemoteSettingsState>()
                .cloned()
                .unwrap_or_default();
            let muted = cx.theme().muted_foreground;
            let danger = cx.theme().danger;
            let fg = cx.theme().foreground;
            let running = matches!(state.status, RemoteAccessState::Running { .. });

            let mut card = v_flex().gap_2();
            if let Some(error) = state.error.clone() {
                card = card.child(div().text_xs().text_color(danger).child(error));
            }
            if !running {
                return card
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("开启远程访问后，可以在这里配对新设备。"),
                    )
                    .into_any_element();
            }
            let label = if state.generating_offer {
                "正在生成…"
            } else if state.offer.is_some() {
                "换一个配对串"
            } else {
                "配对新设备"
            };
            card = card.child(
                h_flex().child(
                    Button::new("begin-pairing")
                        .compact()
                        .label(label)
                        .loading(state.generating_offer)
                        .disabled(state.generating_offer)
                        .on_click(|_, _window, cx: &mut App| begin_pairing(cx)),
                ),
            );
            let Some(offer) = state.offer else {
                return card
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("配对一台设备，就是把这台机器上你这个用户的 shell 权限交给它。"),
                    )
                    .into_any_element();
            };
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
            row =
                row.child(
                    v_flex()
                        .gap_1p5()
                        .min_w(px(0.))
                        .flex_1()
                        .child(
                            div()
                                .max_w(px(280.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_middle()
                                .text_xs()
                                .text_color(fg)
                                .child(offer.uri),
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
                        )
                        .child(div().text_xs().text_color(muted).child(
                            "在另一台电脑的 Smelt「添加运行时」里粘贴，或用手机扫码。\
                         只能用一次，10 分钟内有效。",
                        ))
                        .child(div().text_xs().text_color(muted).child(
                            "配对一台设备，就是把这台机器上你这个用户的 shell 权限交给它。",
                        )),
                );
            card.child(row).into_any_element()
        }),
        // 已配对设备
        SettingItem::render(move |_, _, cx: &mut App| {
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
        }),
    ]);

    let runtimes_group = SettingGroup::new().title("其他运行时").items(vec![
        SettingItem::new(
            "配对串",
            SettingField::input(
                |cx: &App| {
                    cx.try_global::<crate::runtimes::RuntimeHub>()
                        .map(|hub| hub.pairing_input.clone().into())
                        .unwrap_or_default()
                },
                |v: SharedString, cx: &mut App| crate::runtimes::set_pairing_input(&v, cx),
            ),
        )
        .description(
            "在另一台机器上执行 smeltd pair（或在它的「远程访问」里生成配对串），粘贴到这里再点添加。",
        ),
        SettingItem::render(move |_, _, cx: &mut App| {
            let hub = cx
                .try_global::<crate::runtimes::RuntimeHub>()
                .cloned()
                .unwrap_or_default();
            let muted = cx.theme().muted_foreground;
            let danger = cx.theme().danger;
            let fg = cx.theme().foreground;
            let online = gpui::rgb(crate::ui_theme::green());

            let mut list = v_flex().gap_2();
            let label = if hub.adding { "正在配对…" } else { "添加运行时" };
            list = list.child(h_flex().child(
                Button::new("add-runtime")
                    .compact()
                    .label(label)
                    .loading(hub.adding)
                    .disabled(hub.adding)
                    .on_click(|_, _window, cx: &mut App| crate::runtimes::add_runtime(cx)),
            ));
            for error in [hub.load_error.clone(), hub.error.clone()].into_iter().flatten() {
                list = list.child(div().text_xs().text_color(danger).child(error));
            }
            if hub.remotes.is_empty() {
                if hub.load_error.is_none() {
                    list = list
                        .child(div().text_xs().text_color(muted).child("还没有添加其他运行时。"));
                }
                return list.into_any_element();
            }
            for (index, remote) in hub.remotes.iter().enumerate() {
                let runtime_id = remote.connection.runtime_id.clone();
                let (status, is_online) = match &remote.status {
                    crate::runtimes::RuntimeStatus::Connecting => ("正在连接…".to_string(), false),
                    crate::runtimes::RuntimeStatus::Online {
                        version, platform, ..
                    } => (
                        match platform {
                            Some(platform) => format!("在线 · {version} · {platform}"),
                            None => format!("在线 · {version}"),
                        },
                        true,
                    ),
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
                                .child(div().text_xs().text_color(muted).child(status)),
                        )
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
    ]);

    SettingPage::new("远程访问")
        .description("让配对过的电脑和手机经 iroh 连这台机器的运行时，或者添加其他机器上的运行时。")
        .group(access_group)
        .group(devices_group)
        .group(runtimes_group)
}
