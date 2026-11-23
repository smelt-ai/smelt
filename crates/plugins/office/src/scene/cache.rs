//! 固定数量的共享静态纹理；与会话数无关，只在创建视图时生成。

use image::Frame as ImageFrame;

use super::*;

pub(crate) struct Assets {
    pub(super) bodies: [Vec<body::Rect>; 3],
    pub(super) rear: [[Arc<RenderImage>; 6]; 2],
    pub(super) front: [Arc<RenderImage>; 2],
    pub(super) floor: [Arc<RenderImage>; 2],
    pub(super) wall: [[Arc<RenderImage>; 4]; 2],
}

impl Assets {
    fn images(&self) -> impl Iterator<Item = &Arc<RenderImage>> {
        self.rear
            .iter()
            .flatten()
            .chain(self.front.iter())
            .chain(self.floor.iter())
            .chain(self.wall.iter().flatten())
    }

    pub(crate) fn release(&self, cx: &mut gpui::App) {
        for image in self.images() {
            cx.drop_image(image.clone(), None);
        }
    }

    pub(crate) fn new() -> Self {
        Self {
            bodies: [
                body::compile(&FRONT),
                body::compile(&BACK),
                body::compile(&SIDE),
            ],
            rear: std::array::from_fn(|theme| {
                std::array::from_fn(|look| {
                    raster(layout::WIDTH, layout::HEIGHT, |c| {
                        rear(c, look as u64, theme == 1)
                    })
                })
            }),
            front: std::array::from_fn(|theme| {
                raster(layout::WIDTH, layout::HEIGHT, |c| front(c, theme == 1))
            }),
            floor: std::array::from_fn(|theme| {
                raster(layout::room_width(4), 64., |c| floor_strip(c, theme == 1))
            }),
            wall: std::array::from_fn(|theme| {
                std::array::from_fn(|col| {
                    let width = layout::room_width(col + 1);
                    raster(width, 28., |c| {
                        back_wall(c, (width / 2.) as i32, theme == 1)
                    })
                })
            }),
        }
    }
}

fn raster(width: f32, height: f32, paint: impl FnOnce(&mut PixelCanvas<'_>)) -> Arc<RenderImage> {
    let mut pixels = RgbaImage::new((width * 2.) as u32, (height * 2.) as u32);
    paint(&mut PixelCanvas {
        surface: Surface::Raster(&mut pixels),
    });
    Arc::new(RenderImage::new(vec![ImageFrame::new(pixels)]))
}

fn floor_strip(c: &mut PixelCanvas<'_>, light: bool) {
    let width = (layout::room_width(4) / 2.) as i32;
    let base = if light { 0xe2d5bc } else { 0x363839 };
    let seam = if light { 0xd3c5a9 } else { 0x2d3032 };
    c.rect(0, 0, width, 32, base);
    for row in 0..2 {
        let y = row * 16;
        c.rect(0, y, width, 1, seam);
        for col in 0..(width / 32 + 1) {
            c.rect(col * 32 + if row % 2 == 0 { 0 } else { 16 }, y, 1, 16, seam);
        }
    }
}

fn back_wall(c: &mut PixelCanvas<'_>, width: i32, light: bool) {
    c.rect(0, 0, width, 13, if light { 0xa7b6b1 } else { 0x4e6263 });
    c.rect(0, 0, width, 2, if light { 0xd8ddd0 } else { 0x82958f });
    c.rect(0, 12, width, 2, if light { 0x7c8f8c } else { 0x283b3f });
    for x in (16..width - 25).step_by(50) {
        c.rect(x, 3, 24, 8, if light { 0x6c878b } else { 0x293e47 });
        c.rect(x + 1, 4, 22, 6, if light { 0xc3e0de } else { 0x8dafb7 });
        c.rect(x + 2, 4, 5, 1, 0xe3e9d4);
        c.rect(x + 11, 4, 2, 6, if light { 0x6c878b } else { 0x293e47 });
        c.rect(x - 1, 10, 26, 2, if light { 0xe0ddc8 } else { 0x9ba99e });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_layers_are_shared_bounded_and_keep_foreground_transparent() {
        let assets = Assets::new();
        let all: Vec<_> = assets.images().collect();
        assert_eq!(all.len(), 24);
        let bytes: usize = all
            .iter()
            .map(|image| image.as_bytes(0).unwrap().len())
            .sum();
        assert!(bytes < 16 * 1024 * 1024);
        assert_eq!(
            assets.rear[0][0].size(0),
            size(gpui::DevicePixels(384), gpui::DevicePixels(352))
        );
        let front = assets.front[0].as_bytes(0).unwrap();
        assert_eq!(&front[..4], &[0, 0, 0, 0]);
        let rear = assets.rear[0][0].as_bytes(0).unwrap();
        assert!(rear.as_chunks::<4>().0.iter().any(|pixel| pixel[3] == 255));
        assert_ne!(assets.rear[0][0].as_bytes(0), assets.rear[1][0].as_bytes(0));
    }
}
