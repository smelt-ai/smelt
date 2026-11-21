//! 原创代码像素画；没有外部图片、运行时素材加载或 WebView。

use std::sync::Arc;

use gpui::{Bounds, Corners, Pixels, RenderImage, Window, fill, point, px, rgb, size};
use image::RgbaImage;

mod body;
mod cache;
pub(crate) use cache::Assets;

use crate::actors::{Cue, Facing, Frame, Pose};
use crate::layout;
use crate::model::Activity;

/// 绘制单位是 2pt，所有坐标落在整数格上，避免像素边缘模糊。
enum Surface<'a> {
    Window(Bounds<Pixels>, &'a mut Window),
    Raster(&'a mut RgbaImage),
}

struct PixelCanvas<'a> {
    surface: Surface<'a>,
}

impl PixelCanvas<'_> {
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        match &mut self.surface {
            Surface::Window(bounds, window) => window.paint_quad(fill(
                Bounds::new(
                    bounds.origin + point(px((x * 2) as f32), px((y * 2) as f32)),
                    size(px((w * 2) as f32), px((h * 2) as f32)),
                ),
                rgb(color),
            )),
            Surface::Raster(image) => {
                // 每个逻辑像素是 2pt，缓存按 Retina 2x 生成，边缘不经过插值。
                let width = image.width() as usize;
                let (x0, x1) = ((x * 4).max(0) as usize, ((x + w) * 4).max(0) as usize);
                let (y0, y1) = ((y * 4).max(0) as usize, ((y + h) * 4).max(0) as usize);
                let pixel = [color as u8, (color >> 8) as u8, (color >> 16) as u8, 255];
                for row in y0.min(image.height() as usize)..y1.min(image.height() as usize) {
                    let start = (row * width + x0.min(width)) * 4;
                    let end = (row * width + x1.min(width)) * 4;
                    for target in image.as_mut()[start..end].as_chunks_mut::<4>().0 {
                        target.copy_from_slice(&pixel);
                    }
                }
            }
        }
    }

    fn sprite(&mut self, x: i32, y: i32, rows: &[&str], colors: &[u32]) {
        for (dy, row) in rows.iter().enumerate() {
            let bytes = row.as_bytes();
            let mut dx = 0;
            while dx < bytes.len() {
                let byte = bytes[dx];
                let mut end = dx + 1;
                while end < bytes.len() && bytes[end] == byte {
                    end += 1;
                }
                if let Some(index) = byte
                    .checked_sub(b'1')
                    .filter(|i| (*i as usize) < colors.len())
                {
                    self.rect(
                        x + dx as i32,
                        y + dy as i32,
                        (end - dx) as i32,
                        1,
                        colors[index as usize],
                    );
                }
                dx = end;
            }
        }
    }
}

fn paint_image(
    bounds: Bounds<Pixels>,
    image_bounds: Bounds<Pixels>,
    image: &Arc<RenderImage>,
    window: &mut Window,
) {
    let _ = window.paint_image(
        bounds,
        image_bounds,
        Corners::all(px(0.)),
        image.clone(),
        0,
        false,
    );
}

pub(crate) fn floor(
    bounds: Bounds<Pixels>,
    light: bool,
    columns: usize,
    first: bool,
    last: bool,
    assets: &Assets,
    window: &mut Window,
) {
    let theme = usize::from(light);
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    for y in (0..height.ceil() as usize).step_by(64) {
        let image_bounds = Bounds::new(
            bounds.origin + point(px(0.), px(y as f32)),
            size(px(layout::room_width(4)), px(64.)),
        );
        paint_image(bounds, image_bounds, &assets.floor[theme], window);
    }
    if first {
        let image_bounds = Bounds::new(
            bounds.origin,
            size(px(layout::room_width(columns)), px(28.)),
        );
        paint_image(
            bounds,
            image_bounds,
            &assets.wall[theme][columns - 1],
            window,
        );
    }
    let mut c = PixelCanvas {
        surface: Surface::Window(bounds, window),
    };
    let width = (width / 2.).ceil() as i32;
    let height = (height / 2.).ceil() as i32;
    let top = if first { 13 } else { 0 };
    c.rect(
        0,
        top,
        2,
        height - top,
        if light { 0xa7b6b1 } else { 0x4e6263 },
    );
    c.rect(
        width - 2,
        top,
        2,
        height - top,
        if light { 0x7c8f8c } else { 0x283b3f },
    );
    if last {
        c.rect(
            0,
            height - 2,
            width,
            2,
            if light { 0x7c8f8c } else { 0x283b3f },
        );
    }
}

pub(crate) fn workstation(
    bounds: Bounds<Pixels>,
    activity: Activity,
    look: u64,
    frame: Frame,
    light: bool,
    assets: &Assets,
    window: &mut Window,
) {
    let theme = usize::from(light);
    paint_image(
        bounds,
        bounds,
        &assets.rear[theme][(look % 6) as usize],
        window,
    );
    {
        let mut c = PixelCanvas {
            surface: Surface::Window(bounds, window),
        };
        if activity.running() {
            c.rect(37, 14, 18, 9, 0x91bbc0);
            for line in 0..3 {
                c.rect(
                    39,
                    16 + line * 2,
                    5 + ((i32::from(frame.step) + line) % 3) * 3,
                    1,
                    0xd0e5d9,
                );
            }
        }
        character(&mut c, frame, look, cloth(look), light, &assets.bodies);
    }
    paint_image(bounds, bounds, &assets.front[theme], window);
}

fn cloth(look: u64) -> u32 {
    [0x6b9ca5, 0xd29375, 0x8b91c7, 0x91a879, 0xd0aa61, 0xbc84a6][(look % 6) as usize]
}

fn rear(c: &mut PixelCanvas<'_>, look: u64, light: bool) {
    let wood = if light { 0xb58056 } else { 0x986e50 };
    let edge = if light { 0x805840 } else { 0x654b3e };
    let cloth = cloth(look);
    // 隔间内是地毯，门外是公共通道。矮墙先画背面，前沿在人物之后画。
    c.rect(10, 12, 78, 42, if light { 0xc6cbbf } else { 0x414e4d });
    for x in [28, 48, 68] {
        c.rect(x, 12, 1, 42, if light { 0xbec4b9 } else { 0x3c4949 });
    }
    c.rect(4, 60, 90, 3, if light { 0xb5b19e } else { 0x252f30 });
    partition(c, 4, 4, 90, 8, light);
    partition(c, 4, 12, 6, 42, light);
    partition(c, 88, 12, 6, 42, light);
    // 后板上的便签与个人工位配色。
    c.rect(70, 6, 11, 8, edge);
    c.rect(71, 7, 9, 6, 0xc9aa7d);
    c.rect(73, 8, 3, 3, 0xefe5bd);
    c.rect(77, 9, 2, 2, cloth);
    // 桌脚、抽屉、桌面、显示器、键盘和杯子。
    c.rect(27, 29, 3, 15, edge);
    c.rect(63, 29, 3, 15, edge);
    c.rect(58, 30, 8, 12, edge);
    c.rect(59, 32, 6, 3, wood);
    c.rect(61, 33, 2, 1, 0xcebba0);
    c.rect(26, 24, 41, 13, edge);
    c.rect(26, 23, 41, 11, wood);
    c.rect(27, 23, 39, 1, 0xd3a474);
    c.rect(39, 26, 14, 2, 0x484b54);
    c.rect(45, 22, 2, 5, 0x484b54);
    c.rect(36, 13, 20, 12, 0x343943);
    c.rect(37, 14, 18, 9, 0x606b72);
    c.rect(38, 30, 15, 3, 0x53545c);
    c.rect(39, 30, 13, 1, 0xa3a8a9);
    c.rect(60, 26, 3, 4, 0xeee4d0);
    c.rect(63, 27, 1, 2, 0xeee4d0);
    c.rect(75, 28, 6, 6, 0xb78468);
    c.rect(76, 24, 4, 6, 0x607f57);
    c.rect(73, 23, 4, 4, 0x799a68);
    c.rect(79, 22, 4, 5, 0x92aa77);
    // 门垫与隔间外的休息椅，不挡住沿通道进门的路线。
    c.rect(
        layout::DOOR_LEFT,
        61,
        layout::DOOR_RIGHT - layout::DOOR_LEFT,
        5,
        cloth,
    );
    c.rect(
        layout::DOOR_LEFT + 2,
        62,
        layout::DOOR_RIGHT - layout::DOOR_LEFT - 4,
        1,
        0xb6c4b6,
    );
    c.rect(6, 73, 23, 8, 0x667d78);
    c.rect(6, 71, 23, 3, 0x89a49a);
    c.rect(5, 72, 3, 10, 0x536b66);
    c.rect(27, 72, 3, 10, 0x536b66);
    c.rect(8, 81, 2, 2, edge);
    c.rect(25, 81, 2, 2, edge);
}

fn front(c: &mut PixelCanvas<'_>, light: bool) {
    partition(
        c,
        4,
        layout::FRONT_Y,
        layout::DOOR_LEFT - 4,
        layout::WALL_DEPTH,
        light,
    );
    partition(
        c,
        layout::DOOR_RIGHT,
        layout::FRONT_Y,
        94 - layout::DOOR_RIGHT,
        layout::WALL_DEPTH,
        light,
    );
}

fn partition(c: &mut PixelCanvas<'_>, x: i32, y: i32, w: i32, h: i32, light: bool) {
    c.rect(x, y, w, h, if light { 0x718c94 } else { 0x354e57 });
    c.rect(
        x + 1,
        y + 2,
        w - 2,
        h - 2,
        if light { 0xa2b7ba } else { 0x59747c },
    );
    c.rect(x, y, w, 2, if light { 0xd6dfd7 } else { 0x91a7a7 });
    c.rect(x + 1, y, w - 2, 1, if light { 0xedeee1 } else { 0xb6c6bc });
}

fn character(
    c: &mut PixelCanvas<'_>,
    frame: Frame,
    look: u64,
    cloth: u32,
    light: bool,
    bodies: &[Vec<body::Rect>; 3],
) {
    let x = frame.position.x.round() as i32;
    let y = frame.position.y.round() as i32;
    let stride = [0, 1, 0, -1][frame.step as usize % 4];
    let bob = i32::from(frame.pose == Pose::Walk && frame.step % 2 == 1);
    c.rect(
        x - 1,
        y + 19,
        15,
        2,
        if light { 0xb5ac98 } else { 0x2b2e31 },
    );
    let offline = frame.pose == Pose::Offline;
    let hair = if offline {
        0x777b7c
    } else {
        [0x4e3c3a, 0x886248, 0xc49b62, 0x57515e][((look >> 4) % 4) as usize]
    };
    let skin = if offline {
        0xa1a3a0
    } else {
        [0xe8bb91, 0xc48d69, 0x93634e][((look >> 8) % 3) as usize]
    };
    let shirt = if offline { 0x7d8587 } else { cloth };
    let colors = [
        hair,
        skin,
        shirt,
        if offline { 0x666c72 } else { 0x3c4554 },
        0x30333d,
    ];
    let index = match frame.facing {
        Facing::South => 0,
        Facing::North => 1,
        Facing::East | Facing::West => 2,
    };
    body::paint(c, &bodies[index], frame, &colors);
    match frame.pose {
        Pose::Type => {
            c.rect(x - 2, y + 8 + (frame.step % 2) as i32, 3, 2, skin);
            c.rect(x + 12, y + 9 - (frame.step % 2) as i32, 3, 2, skin);
        }
        Pose::Think => {
            c.rect(x + 11, y + 5 + i32::from(frame.step % 4 > 1), 3, 2, skin);
            c.rect(x + 12, y + 7, 2, 4, shirt);
        }
        Pose::Walk => {
            c.rect(x - 1 + stride, y + 10 - bob, 2, 3, skin);
            c.rect(x + 12 - stride, y + 10 - bob, 2, 3, skin);
        }
        Pose::Celebrate => {
            c.rect(x - 2, y + 5 - stride.abs(), 2, 6, shirt);
            c.rect(x + 13, y + 5 - stride.abs(), 2, 6, shirt);
            c.rect(x - 2, y + 3 - stride.abs(), 2, 2, skin);
            c.rect(x + 13, y + 3 - stride.abs(), 2, 2, skin);
        }
        Pose::Rest => {
            // 坐下后闭眼休息；动作收尾后保持这一帧，不靠定时器呼吸/眨眼。
            c.rect(x + 4, y + 5, 2, 1, 0x594944);
            c.rect(x + 8, y + 5, 2, 1, 0x594944);
        }
        Pose::Wait if frame.animating => {
            c.rect(x + 12, y + 7 - stride.abs(), 2, 4, shirt);
            c.rect(x + 12, y + 5 - stride.abs(), 2, 2, skin);
        }
        _ => {}
    }
    if let Some(cue) = frame.cue {
        bubble(
            c,
            x + 15,
            y - 8 - frame.cue_lift,
            cue,
            frame.step,
            frame.animating,
        );
    }
}

fn bubble(c: &mut PixelCanvas<'_>, x: i32, y: i32, cue: Cue, step: u8, moving: bool) {
    let color = match cue {
        Cue::Error => 0xb95b58,
        Cue::Completed => 0x528369,
        Cue::Thought => 0x557d96,
        Cue::Sleep => 0x777b8f,
        _ => 0xb47b36,
    };
    c.rect(x, y, 13, 11, 0xede5d3);
    c.rect(x, y + 11, 3, 2, 0xede5d3);
    match cue {
        Cue::Approval => c.sprite(
            x + 4,
            y + 2,
            &[".11.", ".11.", ".11.", ".11.", "....", ".11."],
            &[color],
        ),
        Cue::Input => c.sprite(
            x + 4,
            y + 2,
            &["111.", "..11", "..11", ".11.", "....", ".11."],
            &[color],
        ),
        Cue::Error => c.sprite(
            x + 3,
            y + 2,
            &["11..11", ".1111.", "..11..", ".1111.", "11..11"],
            &[color],
        ),
        Cue::Completed => c.sprite(
            x + 3,
            y + 3,
            &[".....1", "....11", "1..11.", "1111..", ".11..."],
            &[color],
        ),
        Cue::Sleep => c.sprite(
            x + 4,
            y + 2,
            &["11111", "...11", "..11.", ".11..", "11...", "11111"],
            &[color],
        ),
        Cue::Thought => {
            let count = if moving { 1 + (step / 2) % 3 } else { 3 };
            for i in 0..count {
                c.rect(x + 2 + i32::from(i) * 3, y + 5, 2, 2, color);
            }
        }
    }
}

const SIDE: [&str; 20] = [
    "....11111....",
    "...1111111...",
    "..111111111..",
    "..111122221..",
    "..111122222..",
    "..1111225222.",
    "...1122222...",
    "....22222....",
    "....333333...",
    "...33333333..",
    "...333333332.",
    "...333333322.",
    "...33333332..",
    "....333333...",
    "....444444...",
    "....444444...",
    "....444444...",
    "....444444...",
    "...5555555...",
    "...5555555...",
];

const FRONT: [&str; 20] = [
    "....11111....",
    "...1111111...",
    "..111111111..",
    "..112222211..",
    "..122222221..",
    "..122522521..",
    "...2222222...",
    "....22222....",
    "...3333333...",
    "..333333333..",
    ".23333333332.",
    ".22333333322.",
    "..233333332..",
    "...3333333...",
    "...4444444...",
    "...444.444...",
    "...444.444...",
    "...444.444...",
    "..5555.5555..",
    "..5555.5555..",
];

const BACK: [&str; 20] = [
    "....11111....",
    "...1111111...",
    "..111111111..",
    "..111111111..",
    "..111111111..",
    "..111111111..",
    "...1111111...",
    "....22222....",
    "...3333333...",
    "..333333333..",
    ".33333333333.",
    ".33333333333.",
    "..333333333..",
    "...3333333...",
    "...4444444...",
    "...444.444...",
    "...444.444...",
    "...444.444...",
    "..5555.5555..",
    "..5555.5555..",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprites_have_uniform_rows_and_only_known_palette_indices() {
        for sprite in [&FRONT, &BACK, &SIDE] {
            assert!(sprite.iter().all(|row| row.len() == 13));
            assert!(
                sprite
                    .iter()
                    .flat_map(|row| row.bytes())
                    .all(|byte| byte == b'.' || (b'1'..=b'6').contains(&byte))
            );
        }
    }
}
