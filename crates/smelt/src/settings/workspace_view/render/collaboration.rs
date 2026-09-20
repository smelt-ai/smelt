//! 设置页：手机远程。

use super::*;

pub(super) fn remote_page(
    _entity: Entity<Workspace>,
    _snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let (fg, _muted, border, popover) = {
        let t = cx.theme();
        (t.foreground, t.muted_foreground, t.border, t.popover)
    };
    let btn_base = move |id: &'static str, label: String| {
        div()
            .id(id)
            .h(px(26.))
            .px_3()
            .flex()
            .flex_none()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(fg)
            .bg(popover)
            .border_1()
            .border_color(border)
            .child(label)
    };
    let btn = move |id: &'static str, label: String| btn_base(id, label).hover(|s| s.bg(border));
    let _btn_hover = move |id: &'static str, label: String, hover_bg: Hsla| {
        btn_base(id, label).hover(move |s| s.bg(hover_bg))
    };

    // —— 协作与远程：手机配对 ——
    let remote_group = SettingGroup::new().items(vec![
                SettingItem::new(
                    "开启远程",
                    SettingField::switch(
                        |cx: &App| cx.global::<RemoteConfig>().enabled,
                        |v: bool, cx: &mut App| apply_remote_toggle(v, cx),
                    ),
                )
                .description(
                    "打开后启用 iroh：优先打洞直连，打不通自动使用配置的 relay。关掉会停止分享。\
                     插电时电脑保持可连；电池供电不阻止休眠，只有电脑醒着、正在用时才能远程连接。",
                ),
                SettingItem::new(
                    "Relay 地址",
                    SettingField::input(
                        |cx: &App| cx.global::<RemoteConfig>().iroh_relay.clone().into(),
                        |v: SharedString, cx: &mut App| apply_iroh_relay_value(v, cx),
                    ),
                )
                .description(
                    "填写自建 relay 的域名、IP 或完整 URL；省略协议时使用 https://。留空不会使用公共 relay。",
                ),
                SettingItem::new(
                    "允许远程写入",
                    SettingField::switch(
                        |cx: &App| cx.global::<RemoteConfig>().write_enabled,
                        |v: bool, cx: &mut App| apply_write_toggle(v, cx),
                    ),
                )
                .description(
                    "配对码持有者可在手机上输入、批准/拒绝权限。分享即授权。\
                     切换权限不会改变配对 Token，已配对手机继续有效。",
                ),
                // 分享卡片只展示 iroh 配对码；loopback 网关是内部实现，不对用户暴露。
                SettingItem::render(move |_, _, cx: &mut App| {
                    let cfg = cx.global::<RemoteConfig>().clone();
                    let remote = cx.global::<RemoteRuntimeState>().clone();
                    let iroh = cx
                        .try_global::<IrohRuntimeState>()
                        .cloned()
                        .unwrap_or_default();
                    let danger = cx.theme().danger;
                    let muted = cx.theme().muted_foreground;
                    let fg = cx.theme().foreground;

                    if !cfg.enabled {
                        return div()
                            .text_xs()
                            .text_color(muted)
                            .child("打开「开启远程」后，这里出现配对码与二维码。")
                            .into_any_element();
                    }

                    // iroh 准备中（绑定要连接用户配置的 relay）
                    if iroh.connecting {
                        return div()
                            .text_xs()
                            .text_color(muted)
                            .child("正在建立 iroh 通道…（连接 relay + 打洞）")
                            .into_any_element();
                    }

                    if let Some(err) = iroh.error.as_ref().or(remote.error.as_ref()) {
                        return v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(danger)
                                    .child(format!("出了点问题：{err}")),
                            )
                            .child(
                                btn("retry-remote", "重试".into()).on_mouse_down(
                                    MouseButton::Left,
                                    |_, _window, cx: &mut App| retry_remote_setup(cx),
                                ),
                            )
                            .into_any_element();
                    }

                    let Some(primary) = iroh.pairing_uri.clone() else {
                        return v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("还没有可用的配对码。"),
                            )
                            .child(
                                btn("retry-remote-empty", "重试".into()).on_mouse_down(
                                    MouseButton::Left,
                                    |_, _window, cx: &mut App| retry_remote_setup(cx),
                                ),
                            )
                            .into_any_element();
                    };

                    let scope = "iroh（优先直连，必要时中继）";
                    let mode = if iroh.write { "可写入" } else { "只读" };

                    let primary_copy = primary.clone();
                    // 二维码已在隧道状态更新时构造；render 路径只复用图片对象。
                    let qr_image = iroh.qr_image;

                    let mut card = v_flex().gap_2();
                    let mut row = h_flex().items_start().gap_3();
                    if let Some(image) = qr_image {
                        row = row.child(
                            div()
                                .p_2()
                                .rounded(px(8.))
                                // 二维码底必须是纯白，两种主题都一样：
                                // 深色底上的二维码扫不出来。别跟着色板走。
                                .bg(gpui::rgb(0xffffff))
                                .child(img(image).w(px(132.)).h(px(132.))),
                        );
                    }
                    row = row.child(
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
                                    .child(primary),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                    btn(
                                        "copy-share-link",
                                        copy_btn_label("copy-share-link", "复制配对码", cx),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_, _window, cx: &mut App| {
                                            copy_with_feedback(
                                                primary_copy.clone(),
                                                "copy-share-link",
                                                cx,
                                            );
                                        },
                                    ),
                                    )
                                    .child(
                                        btn("refresh-remote-token", "刷新 Token".into())
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                |_, window, cx: &mut App| {
                                                    refresh_remote_token(window, cx)
                                                },
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{scope} · {mode}")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(
                                        "用 smelt 手机 App 扫码配对。电脑或服务重启不会改变 Token；手动刷新会使旧配对失效。",
                                    ),
                            ),
                    );
                    card = card.child(row);

                    card.into_any_element()
                }),
                // 已连接设备列表
                SettingItem::render(move |_, _, cx: &mut App| {
                    let cfg = cx.global::<RemoteConfig>().clone();
                    let conns = cx
                        .try_global::<IrohConnectionsState>()
                        .cloned()
                        .unwrap_or_default();
                    let muted = cx.theme().muted_foreground;
                    let fg = cx.theme().foreground;
                    let success = gpui::rgb(crate::ui_theme::green());

                    if !cfg.enabled {
                        return div().into_any_element();
                    }

                    let mut card = v_flex().gap_2();
                    card = card.child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(fg)
                            .child("已连接设备"),
                    );

                    if conns.connections.is_empty() {
                        card = card.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("暂无移动端设备连接。"),
                        );
                    } else {
                        for conn in &conns.connections {
                            let short_id = if conn.remote_id.len() > 16 {
                                format!("{}…{}", &conn.remote_id[..8], &conn.remote_id[conn.remote_id.len()-8..])
                            } else {
                                conn.remote_id.clone()
                            };
                            let duration = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs().saturating_sub(conn.connected_at))
                                .unwrap_or(0);
                            let duration_str = if duration < 60 {
                                format!("{duration} 秒前连接")
                            } else if duration < 3600 {
                                format!("{} 分钟前连接", duration / 60)
                            } else {
                                format!("{} 小时前连接", duration / 3600)
                            };
                            card = card.child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .size(px(8.))
                                            .rounded_full()
                                            .bg(success),
                                    )
                                    .child(
                                        v_flex()
                                            .gap_0p5()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(fg)
                                                    .child(format!("📱 {short_id}")),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(duration_str),
                                            ),
                                    ),
                            );
                        }
                    }

                    card.into_any_element()
                }),
            ]);

    SettingPage::new("手机远程")
        .description("配对 Smelt 手机 App，远程查看或操作当前工作台。")
        .group(remote_group)
}
