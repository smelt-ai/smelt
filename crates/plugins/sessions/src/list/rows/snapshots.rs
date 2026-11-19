//! 行首头像出图：离屏把会话行首的头像和右下角小标画成 PNG，给人和 agent 读图核对。不做像素
//! 比对。
//!
//! ```sh
//! cargo test -j 2 -p smelt-sessions session_avatar_snapshots -- --ignored --nocapture
//! ```
//!
//! 图在 `target/tmp/snapshots/sessions/avatar-{light,dark}.png`，2 倍像素。小人归别的插件，
//! 这里拿一块花花绿绿的像素块顶替，看最难认的情形：小标压在彩色像素上。每三行是一种小标
//! （智能体首字、Claude、Codex、终端），依次是闲着、在跑（选中）、要你。
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
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target"),
        std::path::PathBuf::from,
    );
    let out = target.join("tmp/snapshots/sessions");
    std::fs::create_dir_all(&out).expect("建出图目录");
    for (theme, suffix) in [(DEFAULT_LIGHT, "light"), (DEFAULT_DARK, "dark")] {
        let path = out.join(format!("avatar-{suffix}.png"));
        render(theme, &path);
        println!("{}", path.display());
    }
}

fn render(theme: &str, path: &Path) {
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
        assert!(
            smelt_plugin_api::apply_theme(theme, cx),
            "主题 {theme} 不在目录里"
        );
    });
    let window = cx
        .open_window(size(px(120.), px(580.)), |window, cx| {
            let figure: AnyView = cx.new(|_| StandIn).into();
            let sheet = cx.new(|_| Sheet { figure });
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
    figure: AnyView,
}

impl Render for Sheet {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let claude = ConversationAgentKind::Claude;
        let codex = ConversationAgentKind::Codex;
        let icons = [
            RowIcon::Agent {
                mark: "开".into(),
                name: "开发助理".into(),
            },
            RowIcon::Provider {
                path: claude.icon_asset().into(),
                label: claude.label().into(),
            },
            RowIcon::Provider {
                path: codex.icon_asset().into(),
                label: codex.label().into(),
            },
            RowIcon::Terminal,
        ];
        let cases = [
            (AgentStatus::Idle, false),
            (AgentStatus::Running, true),
            (AgentStatus::NeedsYou, false),
        ];
        let mut rows = Vec::new();
        for (i, icon) in icons.iter().enumerate() {
            for (k, (status, selected)) in cases.into_iter().enumerate() {
                let avatar = session_avatar(
                    &format!("{i}-{k}"),
                    Some(self.figure.clone()),
                    icon,
                    status,
                    selected,
                    None,
                );
                rows.push(
                    ListRow::new(eid(format!("row-{i}-{k}")))
                        .selected(selected)
                        .h(px(AVATAR_ROW))
                        .px_2()
                        .flex()
                        .items_center()
                        .child(avatar),
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
