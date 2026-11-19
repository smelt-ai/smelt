use std::path::{Path, PathBuf};

use super::*;

/// 每个测试自己的插件目录，名字带进程号，并行跑、反复跑都不打架。
fn plugin_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("smelt-plugin-assets-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    dir
}

fn write(dir: &Path, file: &str, bytes: &[u8]) {
    std::fs::write(dir.join(file), bytes).unwrap();
}

fn load(path: &str) -> Option<Vec<u8>> {
    Assets.load(path).unwrap().map(Cow::into_owned)
}

#[test]
fn same_file_name_in_two_plugins_stays_apart() {
    let (a, b) = (plugin_dir("same-a"), plugin_dir("same-b"));
    write(&a, "assets/logo.svg", b"a");
    write(&b, "assets/logo.svg", b"b");
    let (pa, pb) = (mount("test.same-a", a), mount("test.same-b", b));
    assert_eq!(
        load(&format!("{pa}/assets/logo.svg")).as_deref(),
        Some(&b"a"[..])
    );
    assert_eq!(
        load(&format!("{pb}/assets/logo.svg")).as_deref(),
        Some(&b"b"[..])
    );
}

#[test]
fn remount_retires_the_old_paths() {
    let dir = plugin_dir("remount");
    write(&dir, "assets/logo.svg", b"old");
    let old = mount("test.remount", dir.clone());
    assert_eq!(
        load(&format!("{old}/assets/logo.svg")).as_deref(),
        Some(&b"old"[..])
    );

    write(&dir, "assets/logo.svg", b"new");
    let new = mount("test.remount", dir);
    assert_ne!(old, new);
    assert_eq!(
        load(&format!("{new}/assets/logo.svg")).as_deref(),
        Some(&b"new"[..])
    );
    assert_eq!(load(&format!("{old}/assets/logo.svg")), None);

    unmount("test.remount");
    assert_eq!(load(&format!("{new}/assets/logo.svg")), None);
}

#[test]
fn missing_files_and_escapes_draw_nothing() {
    let dir = plugin_dir("missing");
    let prefix = mount("test.missing", dir);
    assert_eq!(load(&format!("{prefix}/assets/nope.png")), None);
    // 跑出插件目录的路径 `asset()` 就不给；直接拼出来的，AppAssets 也不读。
    assert!(Assets.load(&format!("{prefix}/../escape.png")).is_err());
    // 认不出的插件资源路径不算插件的，也不是 Smelt 的。
    assert_eq!(load("plugin-assets/test.missing/not-a-number/a.png"), None);
    assert_eq!(load("plugin-assets/never-mounted/1/a.png"), None);
}

#[test]
fn smelt_resources_stay_under_their_own_paths() {
    let icon = smelt_ui::IconName::Puzzle.path();
    assert!(load(&icon).is_some());
    assert!(load("smelt-icons/agent-claude.svg").is_some());
    // 裸路径不会去插件目录里找。
    assert_eq!(load("assets/logo.svg"), None);
}

#[test]
fn listing_follows_the_plugin_directory() {
    let dir = plugin_dir("list");
    write(&dir, "assets/a.png", b"a");
    write(&dir, "assets/b.svg", b"b");
    let prefix = mount("test.list", dir);
    let names = Assets.list(&format!("{prefix}/assets")).unwrap();
    assert_eq!(names, ["a.png", "b.svg"]);
    assert!(!Assets.list("icons/").unwrap().is_empty());
}

/// 资源表读得出字节不等于画得出来：离屏真画一帧，取像素看颜色。
#[cfg(target_os = "macos")]
mod render {
    use std::sync::Arc;

    use gpui::{
        AppContext as _, HeadlessAppContext, IntoElement, ParentElement as _, Render, Rgba,
        Styled as _, div, img, px, rgb, size, svg,
    };

    use super::*;

    const SLOT: f32 = 40.;
    const GAP: f32 = 10.;
    const BACKGROUND: u32 = 0xffffff;

    enum Draw {
        Image(String),
        /// `svg()`：只取形状，涂成给的颜色。
        Mask(String, u32),
    }

    struct Row(Vec<Draw>);

    impl Render for Row {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .gap(px(GAP))
                .bg(rgb(BACKGROUND))
                .children(self.0.iter().map(|draw| {
                    match draw {
                        Draw::Image(path) => img(SharedString::from(path.clone()))
                            .size(px(SLOT))
                            .flex_none()
                            .into_any_element(),
                        Draw::Mask(path, color) => svg()
                            .path(SharedString::from(path.clone()))
                            .size(px(SLOT))
                            .flex_none()
                            .text_color(rgb(*color))
                            .into_any_element(),
                    }
                }))
        }
    }

    fn png(color: [u8; 3]) -> Vec<u8> {
        let image =
            image::RgbaImage::from_pixel(4, 4, image::Rgba([color[0], color[1], color[2], 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn square_svg(fill: &str) -> Vec<u8> {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><rect width="16" height="16" fill="{fill}"/></svg>"#
        )
        .into_bytes()
    }

    /// 画一行，返回每格中心的颜色（0xRRGGBB）。
    fn draw(cx: &mut HeadlessAppContext, row: Vec<Draw>) -> Vec<u32> {
        let count = row.len();
        let width = count as f32 * (SLOT + GAP);
        let window = cx
            .open_window(size(px(width), px(SLOT)), |_, cx| cx.new(|_| Row(row)))
            .unwrap();
        cx.run_until_parked();
        // 图片在后台解码，第一帧画的是空格子；解码完通知重画，再截一次。
        let _ = cx.capture_screenshot(window.into()).unwrap();
        cx.run_until_parked();
        let shot = cx.capture_screenshot(window.into()).unwrap();
        let scale = shot.width() as f32 / width;
        (0..count)
            .map(|slot| {
                let x = (slot as f32 * (SLOT + GAP) + SLOT / 2.) * scale;
                let y = SLOT / 2. * scale;
                let [r, g, b, _] = shot.get_pixel(x as u32, y as u32).0;
                u32::from_be_bytes([0, r, g, b])
            })
            .collect()
    }

    /// 颜色差不多就算：缩放、色彩空间换算会差一两个数。
    fn assert_colors(actual: &[u32], expected: &[u32]) {
        let near = |a: u32, b: u32| {
            let (a, b) = (Rgba::from(rgb(a)), Rgba::from(rgb(b)));
            (a.r - b.r).abs() < 0.08 && (a.g - b.g).abs() < 0.08 && (a.b - b.b).abs() < 0.08
        };
        let ok = actual.len() == expected.len()
            && actual.iter().zip(expected).all(|(a, b)| near(*a, *b));
        let hex = |colors: &[u32]| {
            colors
                .iter()
                .map(|c| format!("{c:06x}"))
                .collect::<Vec<_>>()
        };
        assert!(ok, "画出来 {:?}，应该是 {:?}", hex(actual), hex(expected));
    }

    #[test]
    fn plugin_files_are_drawn_and_reloaded() {
        let (a, b) = (plugin_dir("render-a"), plugin_dir("render-b"));
        write(&a, "assets/logo.png", &png([255, 0, 0]));
        write(&b, "assets/logo.png", &png([0, 0, 255]));
        write(&a, "assets/icon.svg", &square_svg("#000000"));
        write(&a, "assets/badge.svg", &square_svg("#ff00ff"));

        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(gpui_apple::AppleTextSystem::new()),
            Arc::new(Assets),
            || {
                Ok(Some(Box::new(
                    gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
                )))
            },
        );

        let pa = mount("test.render-a", a.clone());
        let pb = mount("test.render-b", b);
        let first = draw(
            &mut cx,
            vec![
                Draw::Image(format!("{pa}/assets/logo.png")),
                // 同名文件，各是各的插件的。
                Draw::Image(format!("{pb}/assets/logo.png")),
                Draw::Mask(format!("{pa}/assets/icon.svg"), 0x00ff00),
                // image() 画 SVG 保留原色。
                Draw::Image(format!("{pa}/assets/badge.svg")),
                // 还没有的文件：空着，不出错。
                Draw::Image(format!("{pa}/assets/later.png")),
            ],
        );
        assert_colors(
            &first,
            &[0xff0000, 0x0000ff, 0x00ff00, 0xff00ff, BACKGROUND],
        );

        // 改一张、删一张、补上刚才缺的，然后重挂（插件目录里文件一变宿主就重挂）。
        write(&a, "assets/logo.png", &png([255, 255, 0]));
        std::fs::remove_file(a.join("assets/icon.svg")).unwrap();
        write(&a, "assets/later.png", &png([0, 255, 255]));
        let reloaded = mount("test.render-a", a);
        let second = draw(
            &mut cx,
            vec![
                Draw::Image(format!("{reloaded}/assets/logo.png")),
                Draw::Mask(format!("{reloaded}/assets/icon.svg"), 0x00ff00),
                // 刚才这个文件不在，加载失败记在旧路径上，新路径不受它连累。
                Draw::Image(format!("{reloaded}/assets/later.png")),
            ],
        );
        // 旧路径的解码结果还在 GPUI 的缓存里（重挂后脚本整个重跑、只拿新路径，没人再画它），
        // 资源表不再认旧路径见 `remount_retires_the_old_paths`。
        assert_colors(&second, &[0xffff00, BACKGROUND, 0x00ffff]);
    }
}
