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
    let _btn = move |id: &'static str, label: String| btn_base(id, label).hover(|s| s.bg(border));
    let _btn_hover = move |id: &'static str, label: String, hover_bg: Hsla| {
        btn_base(id, label).hover(move |s| s.bg(hover_bg))
    };

    // —— 键盘快捷键：只展示当前实际绑定，暂不支持用户改键 ——

    SettingPage::new("键盘快捷键").resettable(false).group(
        SettingGroup::new().item(
            SettingItem::render(move |_, _, _| {
                let keycap = move |key: &'static str| {
                    div()
                        .min_w(px(34.))
                        .h(px(24.))
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
                };
                let section = move |title: &'static str,
                                    shortcuts: &'static [(
                    &'static str,
                    &'static [&'static str],
                )]| {
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
                        .children(shortcuts.iter().map(|(label, keys)| {
                            h_flex()
                                .w_full()
                                .min_h(px(38.))
                                .justify_between()
                                .items_center()
                                .gap_4()
                                .border_b_1()
                                .border_color(border)
                                .child(div().text_sm().text_color(fg).child(*label))
                                .child(
                                    h_flex()
                                        .flex_none()
                                        .gap_1()
                                        .children(keys.iter().map(|key| keycap(key))),
                                )
                        }))
                };

                const GLOBAL: &[(&str, &[&str])] = &[
                    ("打开设置", &["⌘,"]),
                    ("打开命令面板", &["⌘K"]),
                    ("新建任务", &["⇧⌘N"]),
                    ("退出 Smelt", &["⌘Q"]),
                ];
                const SESSION: &[(&str, &[&str])] = &[
                    ("切换左侧栏", &["⌘B"]),
                    ("切换右侧面板", &["⌥⌘B"]),
                    ("上一个 / 下一个会话", &["⌘↑", "⌘↓"]),
                    ("切换到第 1–9 个会话", &["⌘1…9"]),
                    ("上一个 / 下一个分屏", &["⌘[", "⌘]"]),
                    ("左右分屏 / 上下分屏", &["⌘D", "⇧⌘D"]),
                    ("关闭当前分屏或会话", &["⌘W"]),
                ];
                const NAVIGATION: &[(&str, &[&str])] = &[
                    ("终端内搜索", &["⌘F"]),
                    ("保存当前文件", &["⌘S"]),
                    ("上一个 / 下一个差异", &["⇧F7", "F7"]),
                    ("关闭预览或返回", &["Esc"]),
                    ("终端补全 / 反向补全", &["Tab", "⇧Tab"]),
                ];

                v_flex()
                    .w_full()
                    .gap_6()
                    .child(section("全局", GLOBAL))
                    .child(section("会话与面板", SESSION))
                    .child(section("编辑与导航", NAVIGATION))
                    .into_any_element()
            })
            .keywords(["快捷键", "键盘", "shortcut", "keyboard", "hotkey"]),
        ),
    )
}
