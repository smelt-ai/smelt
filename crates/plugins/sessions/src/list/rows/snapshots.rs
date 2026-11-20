//! 行首头像出图：离屏把会话行首的头像和右下角小标画成 PNG，给人和 agent 读图核对。不做像素
//! 比对。
//!
//! ```sh
//! cargo test -j 2 -p smelt-sessions session_avatar_snapshots -- --ignored --nocapture
//! ```
//!
//! 图在 `target/tmp/snapshots/sessions/avatar-{light,dark,...}.png`，2 倍像素。小人归别的插件，
//! 这里用两列顶替：左列是类似真实 buddy 的浅色圆团子，右列是花花绿绿的像素块（模拟小标压在
//! 最难认的花色上）。每两行是一种小标（智能体首字、Claude、Antigravity、Codex、终端），
//! 依次是普通行和选中行。
//!
//! 默认不跑：主题色板是进程级全局，和并行的其它测试会互相盖掉。

use std::path::Path;
use std::sync::Arc;

use gpui::{HeadlessAppContext, px, size};
use smelt_plugin_api::model::agent_kind::ConversationAgentKind;
use smelt_plugin_api::model::theme_index::{DEFAULT_DARK, DEFAULT_LIGHT};

use super::*;

#[test]
#[ignore = "出图工具，手动跑，见模块说明"]
fn session_avatar_snapshots() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || manifest_dir.join("../../../target"),
        std::path::PathBuf::from,
    );
    let out = target.join("tmp/snapshots/sessions");
    std::fs::create_dir_all(&out).expect("建出图目录");

    let extra_theme_files = [
        manifest_dir.join("../themes/themes/gruvbox.json"),
        manifest_dir.join("../themes/themes/ayu.json"),
        manifest_dir.join("../themes/themes/catppuccin.json"),
    ];
    let extra_sources: Vec<String> = extra_theme_files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();

    let mut themes = vec![(DEFAULT_LIGHT, "light"), (DEFAULT_DARK, "dark")];
    if !extra_sources.is_empty() {
        themes.extend([
            ("Gruvbox Dark", "gruvbox"),
            ("Ayu Light", "ayu-light"),
            ("Catppuccin Mocha", "mocha"),
            ("Catppuccin Latte", "latte"),
        ]);
    }
    for (theme, suffix) in themes {
        let path = out.join(format!("avatar-{suffix}.png"));
        render(theme, &extra_sources, &path);
        println!("{}", path.display());
    }
}

fn render(theme: &str, extra_sources: &[String], path: &Path) {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(smelt_plugin_api::ui::assets::Assets),
        || {
            Ok(Some(Box::new(
                gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
            )))
        },
    );
    cx.update(|cx| {
        gpui_base::init(cx);
        smelt_plugin_api::ui::init(cx);
        if !extra_sources.is_empty() {
            let refs: Vec<&str> = extra_sources.iter().map(String::as_str).collect();
            let _ = smelt_plugin_api::register_theme_sets("snapshot-themes", &refs, cx);
        }
        assert!(
            smelt_plugin_api::apply_theme(theme, cx),
            "主题 {theme} 不在目录里"
        );
    });
    let window = cx
        .open_window(size(px(330.), px(500.)), |window, cx| {
            let buddy: AnyView = cx.new(|_| BuddyStandIn).into();
            let checker: AnyView = cx.new(|_| StandIn).into();
            let sheet = cx.new(|_| Sheet { buddy, checker });
            cx.new(|cx| gpui_base::Root::new(sheet, window, cx))
        })
        .expect("开离屏窗口");
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
        })
        .expect("重画");
    }
    cx.capture_screenshot(window.into())
        .unwrap_or_else(|error| panic!("截图失败: {error:#}"))
        .save(path)
        .unwrap_or_else(|error| panic!("写 {} 失败: {error}", path.display()));
}

/// 模拟常见 buddy 颜色（无外边框的柔和青绿圆团子）。
struct BuddyStandIn;

impl Render for BuddyStandIn {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size(px(AVATAR))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(30.)).rounded_full().bg(rgb(0x88d4e0)))
    }
}

/// 顶替小人：36 见方、4 像素一格的彩色块，颜色挨着换，模拟小标压在花色上的最坏情形。
struct StandIn;

impl Render for StandIn {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        const PALETTE: [u32; 6] = [0xf2c14e, 0xe4572e, 0x76b041, 0x17bebb, 0x3a3042, 0xffffff];
        let cells = (AVATAR / 4.) as usize;
        div()
            .size(px(AVATAR))
            .flex()
            .flex_col()
            .children((0..cells).map(|y| {
                div().flex().children((0..cells).map(move |x| {
                    div()
                        .size(px(4.))
                        .bg(rgb(PALETTE[(x * 7 + y * 3 + x * y) % PALETTE.len()]))
                }))
            }))
    }
}

struct Sheet {
    buddy: AnyView,
    checker: AnyView,
}

impl Render for Sheet {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let claude = ConversationAgentKind::Claude;
        let antigravity = ConversationAgentKind::Antigravity;
        let codex = ConversationAgentKind::Codex;
        let icons = [
            (
                RowIcon::Agent {
                    mark: "开".into(),
                    name: "开发助理".into(),
                },
                "开发助理",
            ),
            (
                RowIcon::Provider {
                    path: claude.icon_asset().into(),
                    label: claude.label().into(),
                },
                "Claude",
            ),
            (
                RowIcon::Provider {
                    path: antigravity.icon_asset().into(),
                    label: antigravity.label().into(),
                },
                "Antigravity",
            ),
            (
                RowIcon::Provider {
                    path: codex.icon_asset().into(),
                    label: codex.label().into(),
                },
                "Codex",
            ),
            (RowIcon::Terminal, "Terminal"),
        ];
        let cases = [(AgentStatus::Idle, false), (AgentStatus::Running, true)];
        let mut rows = Vec::new();
        let variant_badge = |icon: &RowIcon, border: bool, shadow: Option<Vec<gpui::BoxShadow>>| {
            let color = rgb(ui_theme::text());
            let glyph = |path: SharedString| {
                svg()
                    .path(path)
                    .size(px(BADGE_GLYPH))
                    .text_color(color)
                    .into_any_element()
            };
            let mark = match icon {
                RowIcon::Agent { mark, .. } => div()
                    .text_size(px(BADGE_MARK))
                    .line_height(px(BADGE_MARK + 1.))
                    .font_weight(TYPE.ui_medium.weight)
                    .text_color(color)
                    .child(mark.clone())
                    .into_any_element(),
                RowIcon::Provider { path, .. } => glyph(path.clone()),
                RowIcon::Terminal => glyph(IconName::Terminal.path()),
                RowIcon::Conversation => glyph(IconName::MessageSquare.path()),
            };
            div()
                .absolute()
                .right(px(-3.))
                .bottom(px(-2.))
                .size(px(BADGE))
                .rounded_full()
                .bg(rgb(ui_theme::surface()))
                .when(border, |d| {
                    d.border_1().border_color(ui_theme::hair_strong())
                })
                .when_some(shadow, |d, s| d.shadow(s))
                .flex()
                .items_center()
                .justify_center()
                .child(mark)
        };
        let avatar_with = |id: &str, figure: AnyView, icon: &RowIcon, border: bool, shadow| {
            div()
                .id(eid(id.to_string()))
                .relative()
                .size(px(AVATAR))
                .flex_shrink_0()
                .child(figure)
                .child(variant_badge(icon, border, shadow))
        };
        for (i, (icon, label)) in icons.iter().enumerate() {
            for (k, (status, selected)) in cases.into_iter().enumerate() {
                // 1: 纯无边无影  2: 无边+elements(当前)  3: 无边+raised  4: 旧 1px hair_strong+elements
                let c1 = avatar_with(
                    &format!("c1-{i}-{k}"),
                    self.buddy.clone(),
                    icon,
                    false,
                    None,
                );
                let c2 = session_avatar(&format!("c2-{i}-{k}"), Some(self.buddy.clone()), icon);
                let c3 = avatar_with(
                    &format!("c3-{i}-{k}"),
                    self.checker.clone(),
                    icon,
                    false,
                    Some(elevation::raised()),
                );
                let c4 = avatar_with(
                    &format!("c4-{i}-{k}"),
                    self.checker.clone(),
                    icon,
                    true,
                    Some(elevation::elements()),
                );
                let tail = match status {
                    AgentStatus::Running => div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::run()))
                        .child("18s"),
                    _ => div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::row_faint(selected)))
                        .child("5m"),
                };
                rows.push(
                    ListRow::new(eid(format!("row-{i}-{k}")))
                        .selected(selected)
                        .h(px(AVATAR_ROW))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(c1)
                        .child(c2)
                        .child(c3)
                        .child(c4)
                        .child(
                            div()
                                .flex_1()
                                .type_style(TYPE.ui)
                                .text_color(rgb(if selected {
                                    ui_theme::text()
                                } else {
                                    ui_theme::muted()
                                }))
                                .child(*label),
                        )
                        .child(tail),
                );
            }
        }
        div()
            .size_full()
            .bg(rgb(ui_theme::side()))
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .children(rows)
    }
}
