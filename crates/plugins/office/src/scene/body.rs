//! 缓存人物轮廓的同色矩形，保留原像素、镜像和分腿动作，不逐像素提交 quad。

use super::*;

pub(super) struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: usize,
    leg: bool,
}

pub(super) fn compile(rows: &[&str]) -> Vec<Rect> {
    let mut rects: Vec<Rect> = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        let row = row.as_bytes();
        let mut x = 0;
        while x < row.len() {
            let byte = row[x];
            if !(b'1'..=b'5').contains(&byte) {
                x += 1;
                continue;
            }
            let leg = y >= 14;
            let mut end = x + 1;
            while end < row.len() && row[end] == byte && !(leg && (end == 6 || end == 7)) {
                end += 1;
            }
            let color = (byte - b'1') as usize;
            let width = (end - x) as i32;
            if let Some(rect) = rects.iter_mut().find(|r| {
                r.x == x as i32
                    && r.y + r.h == y as i32
                    && r.w == width
                    && r.color == color
                    && r.leg == leg
            }) {
                rect.h += 1;
            } else {
                rects.push(Rect {
                    x: x as i32,
                    y: y as i32,
                    w: width,
                    h: 1,
                    color,
                    leg,
                });
            }
            x = end;
        }
    }
    rects
}

pub(super) fn paint(c: &mut PixelCanvas<'_>, rects: &[Rect], frame: Frame, colors: &[u32; 5]) {
    let stride = [0, 1, 0, -1][frame.step as usize % 4];
    let bob = i32::from(frame.pose == Pose::Walk && frame.step % 2 == 1);
    let shake = if frame.pose == Pose::Error && frame.animating {
        stride
    } else {
        0
    };
    for rect in rects {
        let x = if frame.facing == Facing::West {
            13 - rect.x - rect.w
        } else {
            rect.x
        };
        let leg = if frame.pose == Pose::Walk && rect.leg {
            if x < 6 { stride } else { -stride }
        } else {
            0
        };
        c.rect(
            frame.position.x.round() as i32 + x + shake,
            frame.position.y.round() as i32 + rect.y - bob + leg,
            rect.w,
            rect.h,
            colors[rect.color],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_bodies_match_original_pixels_including_mirrored_walking_legs() {
        for facing in [Facing::South, Facing::North, Facing::East, Facing::West] {
            let rows = match facing {
                Facing::South => &FRONT,
                Facing::North => &BACK,
                _ => &SIDE,
            };
            let rects = compile(rows);
            assert!(rects.len() < 50);
            for pose in [Pose::Walk, Pose::Error, Pose::Think] {
                for step in 0..4 {
                    let mut frame = Frame::settled(Activity::Thinking);
                    frame.position.x = 5.;
                    frame.position.y = 5.;
                    frame.facing = facing;
                    frame.pose = pose;
                    frame.step = step;
                    frame.animating = true;
                    let colors = [0x123456, 0xaabbcc, 0x998877, 0x102030, 0x665544];
                    let mut actual = RgbaImage::new(128, 128);
                    paint(
                        &mut PixelCanvas {
                            surface: Surface::Raster(&mut actual),
                        },
                        &rects,
                        frame,
                        &colors,
                    );
                    let mut expected = RgbaImage::new(128, 128);
                    let mut c = PixelCanvas {
                        surface: Surface::Raster(&mut expected),
                    };
                    let stride = [0, 1, 0, -1][step as usize];
                    for (dy, row) in rows.iter().enumerate() {
                        for (dx, byte) in row.bytes().enumerate() {
                            if !(b'1'..=b'5').contains(&byte) {
                                continue;
                            }
                            let dx = if facing == Facing::West { 12 - dx } else { dx } as i32;
                            let leg = if pose == Pose::Walk && dy >= 14 {
                                if dx < 6 { stride } else { -stride }
                            } else {
                                0
                            };
                            let bob = i32::from(pose == Pose::Walk && step % 2 == 1);
                            let shake = if pose == Pose::Error { stride } else { 0 };
                            c.rect(
                                5 + dx + shake,
                                5 + dy as i32 - bob + leg,
                                1,
                                1,
                                colors[(byte - b'1') as usize],
                            );
                        }
                    }
                    assert_eq!(actual, expected, "{facing:?} {pose:?} step {step}");
                }
            }
        }
    }
}
