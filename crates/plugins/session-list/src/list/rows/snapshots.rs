//! 行首头像出图：离屏把会话行首的头像和右下角小标画成 PNG，给人和 agent 读图核对。不做像素
//! 比对。
//!
//! ```sh
//! cargo test -j 2 -p smelt-session-list session_avatar_snapshots -- --ignored --nocapture
//! ```
//!
//! 图在 `target/tmp/snapshots/sessions/avatar-{light,dark,...}.png`，2 倍像素。每行最左是
//! 没人填头像插槽时的行首图标；小人归别的插件，后面用替身：三个仿真团子（一个戴深色帽子），
//! 加一块花花绿绿的像素块（模拟小标压在最难认的花色上）。每三行是一种小标（智能体用的 Pi、
//! Claude、Antigravity、Codex、终端），依次是闲着、在跑（选中）、要你。
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
        .open_window(size(px(380.), px(720.)), |window, cx| {
            let figures: Vec<AnyView> = vec![
                cx.new(|_| BuddyStandIn {
                    body: 0x9fd8ee,
                    hat: false,
                })
                .into(),
                cx.new(|_| BuddyStandIn {
                    body: 0xf4c6d7,
                    hat: true,
                })
                .into(),
                cx.new(|_| BuddyStandIn {
                    body: 0xd8ecb0,
                    hat: false,
                })
                .into(),
                cx.new(|_| StandIn).into(),
            ];
            let sheet = cx.new(|_| Sheet { figures });
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

/// 顶替小人：压扁的蛋形团子、两只黑眼睛，可戴一顶深色帽子，接近真实小人在 36 见方画布里的样子。
struct BuddyStandIn {
    body: u32,
    hat: bool,
}

impl Render for BuddyStandIn {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let eye = || div().size(px(4.)).rounded_full().bg(rgb(0x1d1d24));
        div()
            .size(px(AVATAR))
            .relative()
            .child(
                div()
                    .absolute()
                    .left(px(4.))
                    .bottom(px(2.))
                    .w(px(28.))
                    .h(px(if self.hat { 22. } else { 26. }))
                    .rounded(px(13.))
                    .bg(rgb(self.body))
                    .flex()
                    .justify_center()
                    .gap(px(7.))
                    .pt(px(9.))
                    .child(eye())
                    .child(eye()),
            )
            .when(self.hat, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(6.))
                        .top(px(4.))
                        .w(px(24.))
                        .h(px(10.))
                        .rounded_t(px(10.))
                        .bg(rgb(0x2b2b38)),
                )
            })
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
    figures: Vec<AnyView>,
}

impl Render for Sheet {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pi = ConversationAgentKind::Pi;
        let claude = ConversationAgentKind::Claude;
        let antigravity = ConversationAgentKind::Antigravity;
        let codex = ConversationAgentKind::Codex;
        let icons = [
            RowIcon::Provider {
                path: pi.icon_asset().into(),
                label: format!("开发助理 · {}", pi.label()).into(),
            },
            RowIcon::Provider {
                path: claude.icon_asset().into(),
                label: claude.label().into(),
            },
            RowIcon::Provider {
                path: antigravity.icon_asset().into(),
                label: antigravity.label().into(),
            },
            RowIcon::Provider {
                path: codex.icon_asset().into(),
                label: codex.label().into(),
            },
            RowIcon::Terminal,
        ];
        let mut rows = Vec::new();
        for (i, icon) in icons.iter().enumerate() {
            for (status, selected) in [
                (AgentStatus::Idle, false),
                (AgentStatus::Running, true),
                (AgentStatus::NeedsYou, false),
            ] {
                let label = match icon {
                    RowIcon::Provider { label, .. } => label.clone(),
                    RowIcon::Terminal => "Terminal".into(),
                    RowIcon::Conversation => "Conversation".into(),
                };
                rows.push(
                    ListRow::new(eid(format!("row-{i}-{status:?}")))
                        .selected(selected)
                        .h(px(AVATAR_ROW))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(session_icon(
                            &format!("lead-{i}-{status:?}"),
                            icon,
                            status,
                            selected,
                            None,
                        ))
                        .children(self.figures.iter().enumerate().map(|(k, figure)| {
                            session_avatar(
                                &format!("{k}-{i}-{status:?}"),
                                Some(figure.clone()),
                                icon,
                                status,
                            )
                        }))
                        .child(
                            div()
                                .flex_1()
                                .type_style(TYPE.ui)
                                .text_color(rgb(if selected {
                                    ui_theme::text()
                                } else {
                                    ui_theme::muted()
                                }))
                                .child(label),
                        ),
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
