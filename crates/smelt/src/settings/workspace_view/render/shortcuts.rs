//! 设置页：键盘快捷键一览。

use super::*;

pub(super) fn shortcuts_page(
    _entity: Entity<Workspace>,
    _snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let (fg, muted, border, popover) = {
        let t = cx.theme();
        (t.foreground, t.muted_foreground, t.border, t.popover)
    };

    // —— 键盘快捷键：只展示当前实际绑定，暂不支持用户改键 ——

    SettingPage::new("键盘快捷键").resettable(false).group(
        SettingGroup::new().item(
            SettingItem::render(move |_, window, _| {
                let row = |label: &'static str, keys: Vec<gpui::AnyElement>| {
                    h_flex()
                        .w_full()
                        .min_h(px(38.))
                        .justify_between()
                        .items_center()
                        .gap_4()
                        .border_b_1()
                        .border_color(border)
                        .child(div().text_sm().text_color(fg).child(label))
                        .child(h_flex().flex_none().gap_1().children(keys))
                        .into_any_element()
                };
                let fixed = |label: &'static str, keys: &'static [&'static str]| {
                    row(
                        label,
                        keys.iter()
                            .copied()
                            .map(|key| keycap(key, border, popover, fg))
                            .collect(),
                    )
                };
                let section = |title: &'static str, rows: Vec<gpui::AnyElement>| {
                    v_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            div()
                                .pb_1()
                                .text_xs()
                                .font_semibold()
                                .text_color(muted)
                                .child(title),
                        )
                        .children(rows)
                        .into_any_element()
                };

                v_flex()
                    .w_full()
                    .gap_6()
                    .child(section(
                        "全局",
                        vec![
                            row(
                                "打开设置",
                                vec![bound_key(
                                    window,
                                    &crate::OpenSettings,
                                    None,
                                    "⌘,",
                                    border,
                                    popover,
                                    fg,
                                )],
                            ),
                            fixed("打开命令面板", &["⌘K"]),
                            // 新建任务没有对应的 Action，不能交给 Kbd，否则键帽是空的。
                            fixed("新建任务", &["⇧⌘N"]),
                            row(
                                "退出 Smelt",
                                vec![bound_key(
                                    window,
                                    &crate::Quit,
                                    None,
                                    "⌘Q",
                                    border,
                                    popover,
                                    fg,
                                )],
                            ),
                        ],
                    ))
                    .child(section(
                        "会话与面板",
                        vec![
                            row(
                                "切换左侧栏",
                                vec![bound_key(
                                    window,
                                    &crate::ToggleSidebar,
                                    None,
                                    "⌘B",
                                    border,
                                    popover,
                                    fg,
                                )],
                            ),
                            row(
                                "切换右侧面板",
                                vec![bound_key(
                                    window,
                                    &crate::ToggleToolPanel,
                                    None,
                                    "⌥⌘B",
                                    border,
                                    popover,
                                    fg,
                                )],
                            ),
                            row(
                                "上一个 / 下一个会话",
                                vec![
                                    bound_key(
                                        window,
                                        &crate::PrevSession,
                                        Some("Workspace"),
                                        "⌘↑",
                                        border,
                                        popover,
                                        fg,
                                    ),
                                    bound_key(
                                        window,
                                        &crate::NextSession,
                                        Some("Workspace"),
                                        "⌘↓",
                                        border,
                                        popover,
                                        fg,
                                    ),
                                ],
                            ),
                            fixed("切换到第 1–9 个会话", &["⌘1…9"]),
                            fixed("上一个 / 下一个分屏", &["⌘[", "⌘]"]),
                            fixed("左右分屏 / 上下分屏", &["⌘D", "⇧⌘D"]),
                            fixed("关闭当前分屏或会话", &["⌘W"]),
                        ],
                    ))
                    .child(section(
                        "编辑与导航",
                        vec![
                            fixed("终端内搜索", &["⌘F"]),
                            fixed("保存当前文件", &["⌘S"]),
                            fixed("上一个 / 下一个差异", &["⇧F7", "F7"]),
                            fixed("关闭预览或返回", &["Esc"]),
                            fixed("终端补全 / 反向补全", &["Tab", "⇧Tab"]),
                        ],
                    ))
                    .into_any_element()
            })
            .keywords(["快捷键", "键盘", "shortcut", "keyboard", "hotkey"]),
        ),
    )
}

fn keycap(key: &'static str, border: Hsla, popover: Hsla, fg: Hsla) -> gpui::AnyElement {
    div()
        .min_w(px(34.))
        .h(crate::ui_theme::height::xs())
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(popover)
        .font_family(crate::terminal_view::font_family())
        .text_xs()
        .text_color(fg)
        .child(key)
        .into_any_element()
}

/// 读当前键位。`context` 用 Key Context 声明，不用 predicate。
/// 没有绑定时退回原来的字，避免键帽是空的。
fn bound_key(
    window: &Window,
    action: &dyn gpui::Action,
    context: Option<&str>,
    fallback: &'static str,
    border: Hsla,
    popover: Hsla,
    fg: Hsla,
) -> gpui::AnyElement {
    match gpui_component::kbd::Kbd::binding_for_action(action, context, window) {
        Some(kbd) => kbd
            .min_w(px(34.))
            .h(crate::ui_theme::height::xs())
            .px_2()
            .py_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(popover)
            .font_family(crate::terminal_view::font_family())
            .text_xs()
            .text_color(fg)
            .into_any_element(),
        None => keycap(fallback, border, popover, fg),
    }
}
