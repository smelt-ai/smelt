//! 终端里的 kitty 图形，桌面端这一侧。
//!
//! smeltd 把程序的图形命令处理好，转成带 `k=1` 的转发命令混在输出里发过来（见
//! `smelt_model::kitty_graphics`）。读线程把它们挑出来，图片就地解码成 `RenderImage` 放进这里；
//! 生成画面时按网格里的锚点标记和占位符格子来取。
//!
//! 换下来的 `RenderImage` 先攒着，等前台 `drop_image`：sprite atlas 按图片 id 留纹理，只丢
//! `Arc` 不会释放显存。

use std::collections::HashMap;
use std::sync::Arc;

use gpui::RenderImage;
use smelt_model::kitty_graphics::{Layout, PixelFormat, PlacementKey, Relay};

/// 解码后的图片边长上限。sprite atlas 单张纹理到 16384px 就画不出来了；原始尺寸另外记着，
/// 源图裁剪按原始像素算。
const MAX_IMAGE_DIM: u32 = 4096;

pub(crate) struct TerminalImage {
    pub(crate) render: Arc<RenderImage>,
    /// 原始像素尺寸。
    pub(crate) size: (u32, u32),
}

/// 在读线程上准备好的转发命令：图片已经解码，拿锁时只做替换，不让渲染等解码。
pub(crate) enum PreparedRelay {
    /// 解码失败是 `None`：同 id 的旧图照样换下来。
    Image(u32, Option<TerminalImage>),
    Other(Relay),
}

impl PreparedRelay {
    pub(crate) fn prepare(relay: Relay) -> Self {
        match relay {
            Relay::Image { id, format, data } => Self::Image(id, decode(format, data)),
            other => Self::Other(other),
        }
    }
}

/// 一个终端连接收到的图片和放置。
#[derive(Default)]
pub(crate) struct TerminalImages {
    images: HashMap<u32, TerminalImage>,
    placements: HashMap<PlacementKey, (u32, Layout)>,
    released: Vec<Arc<RenderImage>>,
}

impl TerminalImages {
    pub(crate) fn apply(&mut self, relay: PreparedRelay) {
        match relay {
            PreparedRelay::Image(id, image) => {
                let old = match image {
                    Some(image) => self.images.insert(id, image),
                    None => self.images.remove(&id),
                };
                self.release(old);
            }
            PreparedRelay::Other(Relay::Place { key, image, layout }) => {
                self.placements.insert(key, (image, layout));
            }
            PreparedRelay::Other(Relay::Unplace(key)) => {
                self.placements.remove(&key);
            }
            PreparedRelay::Other(Relay::DropImage(id)) => {
                let old = self.images.remove(&id);
                self.release(old);
                self.placements.retain(|_, (image, _)| *image != id);
            }
            PreparedRelay::Other(Relay::DropAll) => self.release_all(),
            // prepare 已经把图片拆成 `PreparedRelay::Image`。
            PreparedRelay::Other(Relay::Image { .. }) => {}
        }
    }

    fn release(&mut self, image: Option<TerminalImage>) {
        if let Some(image) = image {
            self.released.push(image.render);
        }
    }

    /// 清空，连接换掉时用。换下来的图片照样等前台释放。
    pub(crate) fn release_all(&mut self) {
        self.released
            .extend(self.images.drain().map(|(_, image)| image.render));
        self.placements.clear();
    }

    /// 等前台 `drop_image` 的图片。
    pub(crate) fn take_released(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.released)
    }

    /// 直接放置：锚点号对应的图片和版面。
    pub(crate) fn anchored(&self, anchor: u64) -> Option<(&TerminalImage, &Layout)> {
        let (image, layout) = self.placements.get(&PlacementKey::Anchor(anchor))?;
        Some((self.images.get(image)?, layout))
    }

    /// 虚拟放置：占位符格子里的图片 id 和放置 id 对应的。放置 id 是 0（格子没写下划线颜色）时，
    /// 照 kitty 取这张图的任意一个虚拟放置。
    pub(crate) fn virtual_placement(
        &self,
        image: u32,
        placement: u32,
    ) -> Option<(&TerminalImage, &Layout)> {
        let layout = match self
            .placements
            .get(&PlacementKey::Virtual { image, placement })
        {
            Some((_, layout)) => layout,
            None if placement == 0 => self.placements.iter().find_map(|(key, (_, layout))| {
                matches!(key, PlacementKey::Virtual { image: i, .. } if *i == image)
                    .then_some(layout)
            })?,
            None => return None,
        };
        Some((self.images.get(&image)?, layout))
    }

    /// 直接放置里最高的占几行：可视区上方这么多行内的锚点，图片还可能伸进可视区。
    pub(crate) fn tallest_anchored_rows(&self) -> u32 {
        self.placements
            .iter()
            .filter(|(key, _)| matches!(key, PlacementKey::Anchor(_)))
            .map(|(_, (_, layout))| layout.rows)
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn has_placements(&self) -> bool {
        !self.placements.is_empty()
    }
}

fn decode(format: PixelFormat, data: Vec<u8>) -> Option<TerminalImage> {
    let decoded = match format {
        PixelFormat::Png => {
            image::load_from_memory_with_format(&data, image::ImageFormat::Png).ok()?
        }
        PixelFormat::Rgb { width, height } => {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(width, height, data)?)
        }
        PixelFormat::Rgba { width, height } => {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(width, height, data)?)
        }
    };
    let size = (decoded.width(), decoded.height());
    let decoded = if size.0 > MAX_IMAGE_DIM || size.1 > MAX_IMAGE_DIM {
        decoded.resize(
            MAX_IMAGE_DIM,
            MAX_IMAGE_DIM,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    let mut pixels = decoded.into_rgba8();
    // RGBA → BGRA：gpui 纹理格式。
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(TerminalImage {
        render: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(width: u32, height: u32) -> Relay {
        Relay::Image {
            id: 1,
            format: PixelFormat::Rgba { width, height },
            data: [0x11, 0x22, 0x33, 0xff].repeat((width * height) as usize),
        }
    }

    #[test]
    fn decodes_to_bgra_and_keeps_the_original_size() {
        let PreparedRelay::Image(1, Some(image)) = PreparedRelay::prepare(rgba(2, 1)) else {
            panic!("应解码成功");
        };
        assert_eq!(image.size, (2, 1));
        assert_eq!(
            image.render.as_bytes(0).unwrap()[..4],
            [0x33, 0x22, 0x11, 0xff]
        );
    }

    #[test]
    fn replaced_and_dropped_images_wait_for_release() {
        let mut images = TerminalImages::default();
        images.apply(PreparedRelay::prepare(rgba(1, 1)));
        images.apply(PreparedRelay::prepare(Relay::Place {
            key: PlacementKey::Anchor(5),
            image: 1,
            layout: Layout {
                rows: 3,
                ..Layout::default()
            },
        }));
        assert!(images.anchored(5).is_some());
        assert_eq!(images.tallest_anchored_rows(), 3);
        // 同 id 换图：旧图等释放，放置还在。
        images.apply(PreparedRelay::prepare(rgba(1, 1)));
        assert_eq!(images.take_released().len(), 1);
        assert!(images.anchored(5).is_some());

        images.apply(PreparedRelay::Other(Relay::DropAll));
        assert_eq!(images.take_released().len(), 1);
        assert!(images.anchored(5).is_none());
        assert!(!images.has_placements());
    }

    #[test]
    fn virtual_placement_without_an_id_matches_any_of_the_image() {
        let mut images = TerminalImages::default();
        images.apply(PreparedRelay::prepare(rgba(1, 1)));
        images.apply(PreparedRelay::prepare(Relay::Place {
            key: PlacementKey::Virtual {
                image: 1,
                placement: 4,
            },
            image: 1,
            layout: Layout::default(),
        }));
        assert!(images.virtual_placement(1, 4).is_some());
        assert!(images.virtual_placement(1, 0).is_some());
        assert!(images.virtual_placement(1, 9).is_none());
        assert!(images.virtual_placement(2, 0).is_none());
    }
}
