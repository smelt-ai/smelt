//! 终端里的 kitty 图形，smeltd 这一侧。
//!
//! 程序的原始图形命令只在这里处理（协议见 `smelt_core::kitty_graphics`）。每条命令变成三份字节：
//! 发给能显示图片的客户端的（转发命令 + 网格字节）、发给不显示图片的客户端的（只有网格字节，
//! 不挂锚点，网格布局和前者一样），以及写回 PTY 的应答。守护自己的网格吃第一份，快照里因此
//! 带着锚点。
//!
//! 图片数据存在守护里不只是为了重连：客户端跟不上时流控会跳过一段实时输出、改发关键帧，被跳过
//! 的字节里可能就有图片，关键帧得把画面上还用得到的图重新带上（[`TerminalGraphics::keyframe_relays`]）。
//!
//! 不支持：共享内存传输（`t=s`）、动画（`a=f/a/c`）、相对放置（`P`）、按位置或层级删除。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;
use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use smelt_core::kitty_graphics::{
    Command, Controls, GraphicsSplitter, Layout, MAX_COMMAND_BYTES, Piece, PixelFormat,
    PlacementCursor, PlacementKey, Relay, anchors_in, encode_image_relay, grid_size,
    placement_bytes, png_dimensions, source_size,
};

/// 程序分块发的 base64 有的带填充有的不带，拼起来解码时两种都认。
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// 每个会话存的图片数据上限（解码后的字节）。超了先删没有放置的旧图。
const STORE_MAX_BYTES: usize = 128 << 20;

/// 放置数超过上次清理后存活数的两倍（至少这么多）才扫一遍网格，清掉锚点已经被擦掉的放置。
const GC_MIN_PLACEMENTS: usize = 64;

type CommandResult<T> = Result<T, &'static str>;

/// 一段 PTY 输出处理完的结果。
#[derive(Default)]
pub(crate) struct FedOutput {
    /// 发给能显示图片的客户端。
    pub(crate) graphics: Vec<u8>,
    /// 发给不显示图片的客户端。
    pub(crate) plain: Vec<u8>,
    /// 写回 PTY 的应答。
    pub(crate) reply: Vec<u8>,
}

/// 推进守护网格。普通字节原样推进、原样转发；图形命令交给 `graphics`，它产生的网格字节也推进
/// 网格。
///
/// 处理命令前先结束 vte 的同步更新：Pi 把整帧包在 `CSI ? 2026 h … l` 里，vte 会把前面的
/// 光标定位攒着不执行，这时读到的光标是旧的。守护的网格不给人看，提前结束同步更新没有画面上的
/// 代价；转发给客户端的字节不变，客户端照常按同步更新处理。
pub(crate) fn feed_output<T: EventListener>(
    chunk: &[u8],
    splitter: &mut GraphicsSplitter,
    parser: &mut Processor,
    term: &mut Term<T>,
    graphics: &mut TerminalGraphics,
    cell: (u32, u32),
    out: &mut FedOutput,
) {
    splitter.feed(chunk, |piece| match piece {
        Piece::Bytes(bytes) => {
            parser.advance(term, bytes);
            out.graphics.extend_from_slice(bytes);
            out.plain.extend_from_slice(bytes);
        }
        Piece::Command(body) => {
            if parser.sync_timeout().sync_timeout().is_some() {
                parser.stop_sync(term);
            }
            let outcome = graphics.handle(body, term, cell);
            parser.advance(term, &outcome.grid);
            out.graphics.extend_from_slice(&outcome.relay);
            out.graphics.extend_from_slice(&outcome.grid);
            out.plain.extend_from_slice(&outcome.plain_grid);
            out.reply.extend_from_slice(&outcome.reply);
        }
        Piece::Reset => out.graphics.extend(graphics.reset()),
    });
}

/// 一条命令的结果。
#[derive(Default)]
pub(crate) struct CommandOutcome {
    /// 只发给能显示图片的客户端的转发命令。
    pub(crate) relay: Vec<u8>,
    /// 守护网格和能显示图片的客户端吃的网格字节（挂锚点、移光标）。
    pub(crate) grid: Vec<u8>,
    /// 不显示图片的客户端吃的网格字节：光标移动一样，不挂锚点。
    pub(crate) plain_grid: Vec<u8>,
    /// 写回 PTY 的应答。
    pub(crate) reply: Vec<u8>,
}

struct StoredImage {
    format: PixelFormat,
    data: Vec<u8>,
    size: (u32, u32),
    /// 传输先后，配额超了先删旧的。
    stamp: u64,
}

#[derive(Clone, Copy)]
struct Placement {
    image: u32,
    layout: Layout,
}

/// 正在分块传输的命令。
struct Loading {
    controls: Controls,
    payload: Vec<u8>,
}

/// 一个会话的图片和放置。
#[derive(Default)]
pub(crate) struct TerminalGraphics {
    images: HashMap<u32, StoredImage>,
    /// `I`（图片编号）→ 守护分配的图片 id。
    numbers: HashMap<u32, u32>,
    placements: HashMap<PlacementKey, Placement>,
    /// 带放置 id 的直接放置：(图片, 放置 id) → 锚点。同一对再放一次时替换旧的。
    named_anchors: HashMap<(u32, u32), u64>,
    loading: Option<Loading>,
    stored_bytes: usize,
    stamp: u64,
    gc_threshold: usize,
}

impl TerminalGraphics {
    pub(crate) fn handle<T: EventListener>(
        &mut self,
        body: &[u8],
        term: &Term<T>,
        cell: (u32, u32),
    ) -> CommandOutcome {
        let Command { controls, payload } = Command::parse(body);
        let (controls, payload) = match self.loading.take() {
            // 分块传输的后续块只带 `m`（和 `q`），控制项以第一块为准。
            Some(mut loading) => {
                loading.payload.extend_from_slice(&payload);
                if loading.payload.len() > MAX_COMMAND_BYTES {
                    return CommandOutcome {
                        reply: response(
                            &loading.controls,
                            None,
                            Err("EFBIG: image data too large"),
                        ),
                        ..CommandOutcome::default()
                    };
                }
                if controls.more {
                    self.loading = Some(loading);
                    return CommandOutcome::default();
                }
                (loading.controls, loading.payload)
            }
            None if controls.more && matches!(controls.action, b't' | b'T' | b'q') => {
                self.loading = Some(Loading { controls, payload });
                return CommandOutcome::default();
            }
            None => (controls, payload),
        };

        let mut out = CommandOutcome::default();
        let result = match controls.action {
            b't' => self.transmit(&controls, &payload, term, &mut out),
            b'T' => self
                .transmit(&controls, &payload, term, &mut out)
                .and_then(|id| self.place(id, &controls, term, cell, &mut out).map(|()| id)),
            b'q' => decode_image(&controls, &payload).map(|_| controls.image_id),
            b'p' => self
                .lookup(&controls)
                .and_then(|id| self.place(id, &controls, term, cell, &mut out).map(|()| id)),
            b'd' => {
                self.delete(&controls, term, &mut out);
                return out;
            }
            _ => Err("EINVAL: unsupported action"),
        };
        out.reply = response(&controls, result.as_ref().ok().copied(), result.map(|_| ()));
        out
    }

    /// 程序发了 RIS：kitty 这时清掉所有图片。
    pub(crate) fn reset(&mut self) -> Vec<u8> {
        let had_any = !self.images.is_empty() || !self.placements.is_empty();
        *self = Self::default();
        if had_any {
            Relay::DropAll.encode()
        } else {
            Vec::new()
        }
    }

    /// 关键帧要带的转发命令：先清空客户端的图片，再补上网格最底下 `lines` 行里的锚点用到的放置
    /// 和图片，以及所有虚拟放置（占位符格子在哪不好查，全带上）。
    pub(crate) fn keyframe_relays<T: EventListener>(
        &self,
        term: &Term<T>,
        lines: usize,
    ) -> Vec<u8> {
        let mut out = Relay::DropAll.encode();
        let anchors = anchors_in_lines(term, lines);
        let shown: Vec<(PlacementKey, Placement)> = self
            .placements
            .iter()
            .filter(|(key, _)| match key {
                PlacementKey::Anchor(anchor) => anchors.contains(anchor),
                PlacementKey::Virtual { .. } => true,
            })
            .map(|(key, placement)| (*key, *placement))
            .collect();
        let images: BTreeSet<u32> = shown.iter().map(|(_, placement)| placement.image).collect();
        for id in images {
            if let Some(image) = self.images.get(&id) {
                out.extend(encode_image_relay(id, image.format, &image.data));
            }
        }
        for (key, placement) in shown {
            out.extend(
                Relay::Place {
                    key,
                    image: placement.image,
                    layout: placement.layout,
                }
                .encode(),
            );
        }
        out
    }

    fn lookup(&self, c: &Controls) -> CommandResult<u32> {
        let id = if c.image_id != 0 {
            c.image_id
        } else {
            self.numbers.get(&c.image_number).copied().unwrap_or(0)
        };
        if self.images.contains_key(&id) {
            Ok(id)
        } else {
            Err("ENOENT: image not found")
        }
    }

    fn transmit<T: EventListener>(
        &mut self,
        c: &Controls,
        payload: &[u8],
        term: &Term<T>,
        out: &mut CommandOutcome,
    ) -> CommandResult<u32> {
        let decoded = decode_image(c, payload)?;
        let id = if c.image_id != 0 {
            c.image_id
        } else {
            self.unused_image_id()
        };
        // 同一个 id 再传一次就是换图，旧图的放置一起删掉。
        self.remove_image(id, out);
        if c.image_id == 0 && c.image_number != 0 {
            self.numbers.insert(c.image_number, id);
        }
        self.stamp += 1;
        out.relay
            .extend(encode_image_relay(id, decoded.format, &decoded.data));
        self.stored_bytes += decoded.data.len();
        self.images.insert(
            id,
            StoredImage {
                format: decoded.format,
                data: decoded.data,
                size: decoded.size,
                stamp: self.stamp,
            },
        );
        self.enforce_quota(term, out);
        Ok(id)
    }

    /// 程序没给 id 时守护自己分配，从顶上往下取，程序给的 id 多半是小数字。
    fn unused_image_id(&self) -> u32 {
        (1..=u32::MAX)
            .rev()
            .find(|id| !self.images.contains_key(id))
            .unwrap_or(u32::MAX)
    }

    fn place<T: EventListener>(
        &mut self,
        image: u32,
        c: &Controls,
        term: &Term<T>,
        cell: (u32, u32),
        out: &mut CommandOutcome,
    ) -> CommandResult<()> {
        if c.parent_id != 0 {
            return Err("EINVAL: relative placements are not supported");
        }
        let size = self
            .images
            .get(&image)
            .ok_or("ENOENT: image not found")?
            .size;
        let (columns, rows) = grid_size(
            c.columns,
            c.rows,
            source_size(c.source, size),
            (c.offset_x, c.offset_y),
            cell,
        );
        let layout = Layout {
            columns,
            rows,
            source: c.source,
            offset_x: c.offset_x,
            offset_y: c.offset_y,
            z: c.z,
        };
        let key = if c.virtual_placement {
            PlacementKey::Virtual {
                image,
                placement: c.placement_id,
            }
        } else {
            if c.placement_id != 0
                && let Some(old) = self.named_anchors.remove(&(image, c.placement_id))
            {
                self.unplace(PlacementKey::Anchor(old), out);
            }
            let anchor = new_anchor();
            if c.placement_id != 0 {
                self.named_anchors.insert((image, c.placement_id), anchor);
            }
            let cursor = placement_cursor(term);
            out.grid = placement_bytes(cursor, columns, rows, c.keep_cursor, Some(anchor));
            out.plain_grid = placement_bytes(cursor, columns, rows, c.keep_cursor, None);
            PlacementKey::Anchor(anchor)
        };
        self.placements.insert(key, Placement { image, layout });
        out.relay
            .extend(Relay::Place { key, image, layout }.encode());
        self.collect_garbage_if_due(term, out);
        Ok(())
    }

    fn delete<T: EventListener>(&mut self, c: &Controls, term: &Term<T>, out: &mut CommandOutcome) {
        let mut touched = BTreeSet::new();
        let doomed: Vec<(PlacementKey, u32)> = match c.delete.to_ascii_lowercase() {
            // 屏幕上看得见的放置。虚拟放置不在任何位置上，一起删。
            b'a' => {
                let on_screen = anchors_in_lines(term, term.screen_lines());
                self.placements
                    .iter()
                    .filter(|(key, _)| match key {
                        PlacementKey::Anchor(anchor) => on_screen.contains(anchor),
                        PlacementKey::Virtual { .. } => true,
                    })
                    .map(|(key, placement)| (*key, placement.image))
                    .collect()
            }
            selector @ (b'i' | b'n') => {
                let image = if selector == b'i' {
                    c.image_id
                } else {
                    self.numbers.get(&c.image_number).copied().unwrap_or(0)
                };
                touched.insert(image);
                self.placements
                    .iter()
                    .filter(|(key, placement)| {
                        placement.image == image
                            && (c.placement_id == 0
                                || self.placement_id_matches(key, image, c.placement_id))
                    })
                    .map(|(key, _)| (*key, image))
                    .collect()
            }
            // 按位置、按层级等删除不支持。
            _ => Vec::new(),
        };
        for (key, image) in doomed {
            touched.insert(image);
            self.unplace(key, out);
        }
        // 大写：图片没有放置了就连数据一起删。
        if c.delete.is_ascii_uppercase() {
            for image in touched {
                if !self
                    .placements
                    .values()
                    .any(|placement| placement.image == image)
                {
                    self.remove_image(image, out);
                }
            }
        }
    }

    fn placement_id_matches(&self, key: &PlacementKey, image: u32, placement_id: u32) -> bool {
        match key {
            PlacementKey::Virtual { placement, .. } => *placement == placement_id,
            PlacementKey::Anchor(anchor) => {
                self.named_anchors.get(&(image, placement_id)) == Some(anchor)
            }
        }
    }

    fn unplace(&mut self, key: PlacementKey, out: &mut CommandOutcome) {
        if self.placements.remove(&key).is_some() {
            out.relay.extend(Relay::Unplace(key).encode());
        }
        if let PlacementKey::Anchor(anchor) = key {
            self.named_anchors.retain(|_, named| *named != anchor);
        }
    }

    fn remove_image(&mut self, id: u32, out: &mut CommandOutcome) {
        let placed = self
            .placements
            .values()
            .any(|placement| placement.image == id);
        self.placements.retain(|_, placement| placement.image != id);
        self.named_anchors.retain(|(image, _), _| *image != id);
        self.numbers.retain(|_, image| *image != id);
        let stored = self.images.remove(&id);
        if let Some(image) = &stored {
            self.stored_bytes -= image.data.len();
        }
        if placed || stored.is_some() {
            out.relay.extend(Relay::DropImage(id).encode());
        }
    }

    fn enforce_quota<T: EventListener>(&mut self, term: &Term<T>, out: &mut CommandOutcome) {
        if self.stored_bytes <= STORE_MAX_BYTES {
            return;
        }
        self.collect_garbage(term, out);
        while self.stored_bytes > STORE_MAX_BYTES {
            let placed: HashSet<u32> = self.placements.values().map(|p| p.image).collect();
            let oldest = |unplaced_only: bool| {
                self.images
                    .iter()
                    .filter(|(id, _)| !unplaced_only || !placed.contains(id))
                    .min_by_key(|(_, image)| image.stamp)
                    .map(|(id, _)| *id)
            };
            let Some(victim) = oldest(true).or_else(|| oldest(false)) else {
                break;
            };
            self.remove_image(victim, out);
        }
    }

    fn collect_garbage_if_due<T: EventListener>(
        &mut self,
        term: &Term<T>,
        out: &mut CommandOutcome,
    ) {
        if self.placements.len() <= self.gc_threshold.max(GC_MIN_PLACEMENTS) {
            return;
        }
        self.collect_garbage(term, out);
        self.gc_threshold = self.placements.len() * 2;
    }

    /// 清掉锚点已经不在网格里（被擦掉、被改写、滚出回滚区）的放置。备用屏期间主屏的格子看不到，
    /// 不清，等回到主屏再说。
    fn collect_garbage<T: EventListener>(&mut self, term: &Term<T>, out: &mut CommandOutcome) {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let live = anchors_in_lines(term, usize::MAX);
        let dead: Vec<PlacementKey> = self
            .placements
            .keys()
            .filter(|key| matches!(key, PlacementKey::Anchor(anchor) if !live.contains(anchor)))
            .copied()
            .collect();
        for key in dead {
            self.unplace(key, out);
        }
    }
}

/// kitty 的应答规则：只有带了 `i` 或 `I` 的命令才应答；`q=1` 只报错，`q=2` 什么都不回。
fn response(c: &Controls, assigned: Option<u32>, result: CommandResult<()>) -> Vec<u8> {
    if c.image_id == 0 && c.image_number == 0 {
        return Vec::new();
    }
    let message = match result {
        Ok(()) if c.quiet >= 1 => return Vec::new(),
        Ok(()) => "OK",
        Err(_) if c.quiet >= 2 => return Vec::new(),
        Err(error) => error,
    };
    let id = if c.image_id != 0 {
        c.image_id
    } else {
        assigned.unwrap_or(0)
    };
    let mut reply = format!("\x1b_Gi={id}");
    if c.image_number != 0 {
        reply.push_str(&format!(",I={}", c.image_number));
    }
    if c.placement_id != 0 {
        reply.push_str(&format!(",p={}", c.placement_id));
    }
    reply.push(';');
    reply.push_str(message);
    reply.push_str("\x1b\\");
    reply.into_bytes()
}

struct DecodedImage {
    format: PixelFormat,
    data: Vec<u8>,
    size: (u32, u32),
}

fn decode_image(c: &Controls, payload: &[u8]) -> CommandResult<DecodedImage> {
    let raw = match c.medium {
        b'd' => LENIENT_BASE64
            .decode(payload)
            .map_err(|_| "EINVAL: bad base64 data")?,
        b'f' | b't' => {
            let path = LENIENT_BASE64
                .decode(payload)
                .ok()
                .and_then(|path| String::from_utf8(path).ok())
                .ok_or("EINVAL: bad file path")?;
            read_image_file(Path::new(&path), c.medium == b't', c)?
        }
        b's' => return Err("EINVAL: shared memory transmission is not supported"),
        _ => return Err("EINVAL: unknown transmission medium"),
    };
    let mut data = match c.compression {
        None => raw,
        Some(b'z') => inflate(&raw)?,
        Some(_) => return Err("EINVAL: unknown compression"),
    };
    let (format, size) = match c.format {
        100 => (
            PixelFormat::Png,
            png_dimensions(&data).ok_or("EBADPNG: not a PNG image")?,
        ),
        24 | 32 => {
            let (width, height) = (c.data_width, c.data_height);
            let channels = if c.format == 24 { 3 } else { 4 };
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(channels))
                .filter(|&expected| expected > 0 && expected <= data.len())
                .ok_or("EINVAL: pixel data does not match s and v")?;
            data.truncate(expected);
            let format = if c.format == 24 {
                PixelFormat::Rgb { width, height }
            } else {
                PixelFormat::Rgba { width, height }
            };
            (format, (width, height))
        }
        _ => return Err("EINVAL: unknown image format"),
    };
    Ok(DecodedImage { format, data, size })
}

/// `t=f` / `t=t`：文件和程序在同一台机器上，由守护来读（桌面端可能在另一台机器上）。规则照
/// kitty：只读普通文件，不碰 /proc、/sys、/dev；`t=t` 只认临时目录里名字带
/// `tty-graphics-protocol` 的文件，读完删掉。
fn read_image_file(path: &Path, temporary: bool, c: &Controls) -> CommandResult<Vec<u8>> {
    if !path.is_absolute() {
        return Err("EINVAL: file path must be absolute");
    }
    let path = path.canonicalize().map_err(|_| "EBADF: cannot read file")?;
    if ["/proc", "/sys", "/dev"]
        .iter()
        .any(|forbidden| path.starts_with(forbidden))
    {
        return Err("EINVAL: refusing to read this path");
    }
    if temporary && !is_graphics_temp_file(&path) {
        return Err("EINVAL: not a graphics protocol temporary file");
    }
    if !path.is_file() {
        return Err("EINVAL: not a regular file");
    }
    let mut file = std::fs::File::open(&path).map_err(|_| "EBADF: cannot read file")?;
    if c.data_offset > 0 {
        file.seek(SeekFrom::Start(u64::from(c.data_offset)))
            .map_err(|_| "EBADF: cannot read file")?;
    }
    let limit = if c.data_size > 0 {
        u64::from(c.data_size)
    } else {
        MAX_COMMAND_BYTES as u64 + 1
    };
    let mut data = Vec::new();
    file.take(limit)
        .read_to_end(&mut data)
        .map_err(|_| "EBADF: cannot read file")?;
    if data.len() > MAX_COMMAND_BYTES {
        return Err("EFBIG: image data too large");
    }
    if temporary {
        let _ = std::fs::remove_file(&path);
    }
    Ok(data)
}

fn is_graphics_temp_file(path: &Path) -> bool {
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("tty-graphics-protocol"));
    let temp_dirs = [std::env::temp_dir(), PathBuf::from("/tmp")];
    named
        && temp_dirs
            .iter()
            .filter_map(|dir| dir.canonicalize().ok())
            .any(|dir| path.starts_with(dir))
}

fn inflate(data: &[u8]) -> CommandResult<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .take(MAX_COMMAND_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| "EINVAL: bad zlib data")?;
    if out.len() > MAX_COMMAND_BYTES {
        return Err("EFBIG: image data too large");
    }
    Ok(out)
}

/// 锚点号随机取：守护重启后从续接快照里恢复的旧锚点不会撞上新分配的。0 留给「没有锚点」。
fn new_anchor() -> u64 {
    loop {
        let (anchor, _) = uuid::Uuid::new_v4().as_u64_pair();
        if anchor != 0 {
            return anchor;
        }
    }
}

fn placement_cursor<T: EventListener>(term: &Term<T>) -> PlacementCursor {
    let grid = term.grid();
    let line = grid.cursor.point.line.0;
    let last_line = i32::try_from(grid.screen_lines()).unwrap_or(i32::MAX) - 1;
    PlacementCursor {
        column: grid.cursor.point.column.0,
        needs_wrap: grid.cursor.input_needs_wrap,
        lines_below: usize::try_from(last_line - line).unwrap_or(0),
        screen_columns: grid.columns(),
        screen_lines: grid.screen_lines(),
    }
}

/// 当前网格最底下 `lines` 行里挂着的锚点。
fn anchors_in_lines<T: EventListener>(term: &Term<T>, lines: usize) -> HashSet<u64> {
    let grid = term.grid();
    let top = term.topmost_line().0;
    let bottom = term.bottommost_line().0;
    let span = i32::try_from(lines).unwrap_or(i32::MAX);
    let start = bottom.saturating_sub(span).saturating_add(1).max(top);
    let mut anchors = HashSet::new();
    for line in start..=bottom {
        let row = &grid[Line(line)];
        for column in 0..grid.columns() {
            if let Some(zerowidth) = row[Column(column)].zerowidth() {
                anchors.extend(anchors_in(zerowidth));
            }
        }
    }
    anchors
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::Point;
    use alacritty_terminal::vte::ansi::Rgb;
    use base64::engine::general_purpose::STANDARD;
    use smelt_core::kitty_graphics::PLACEHOLDER;

    use crate::new_daemon_term;
    use crate::terminal_snapshot::{snapshot_ansi, snapshot_ansi_plain};

    const ROWS: u16 = 5;
    const COLS: u16 = 20;
    const CELL: (u32, u32) = (10, 20);

    /// 只有 PNG 头的假图：守护只读 IHDR 拿宽高，不解码像素。
    fn fake_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(b"\x08\x06\0\0\0rest-of-png");
        png
    }

    fn apc(controls: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = format!("\x1b_G{controls};").into_bytes();
        out.extend_from_slice(STANDARD.encode(payload).as_bytes());
        out.extend_from_slice(b"\x1b\\");
        out
    }

    struct Harness {
        term: Term<VoidListener>,
        parser: Processor,
        splitter: GraphicsSplitter,
        graphics: TerminalGraphics,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                term: new_daemon_term(ROWS, COLS, VoidListener),
                parser: Processor::new(),
                splitter: GraphicsSplitter::default(),
                graphics: TerminalGraphics::default(),
            }
        }

        fn feed(&mut self, bytes: &[u8]) -> FedOutput {
            let mut out = FedOutput::default();
            feed_output(
                bytes,
                &mut self.splitter,
                &mut self.parser,
                &mut self.term,
                &mut self.graphics,
                CELL,
                &mut out,
            );
            out
        }

        /// 放一张 i=`id` 的图，返回锚点号。
        fn place(&mut self, id: u32, columns: u32, rows: u32) -> u64 {
            let out = self.feed(&apc(
                &format!("a=T,f=100,q=2,i={id},c={columns},r={rows}"),
                &fake_png(40, 40),
            ));
            anchor_placed(&relays_in(&out.graphics))
        }
    }

    fn relays_in(bytes: &[u8]) -> Vec<Relay> {
        let mut relays = Vec::new();
        GraphicsSplitter::default().feed(bytes, |piece| {
            if let Piece::Command(body) = piece {
                relays.extend(Relay::parse(body));
            }
        });
        relays
    }

    fn anchor_placed(relays: &[Relay]) -> u64 {
        relays
            .iter()
            .find_map(|relay| match relay {
                Relay::Place {
                    key: PlacementKey::Anchor(anchor),
                    ..
                } => Some(*anchor),
                _ => None,
            })
            .expect("放置了一张图")
    }

    fn anchors_at<T: EventListener>(term: &Term<T>, line: i32, column: usize) -> Vec<u64> {
        let cell = &term.grid()[Point::new(Line(line), Column(column))];
        anchors_in(cell.zerowidth().unwrap_or_default())
    }

    fn cursor<T: EventListener>(term: &Term<T>) -> (i32, usize) {
        let point = term.grid().cursor.point;
        (point.line.0, point.column.0)
    }

    fn fresh_term_from(bytes: &[u8]) -> Term<VoidListener> {
        let mut term = new_daemon_term(ROWS, COLS, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    #[test]
    fn direct_placement_relays_the_image_and_hangs_an_anchor() {
        let mut h = Harness::new();
        let out = h.feed(&apc("a=T,f=100,i=7,c=4,r=2", &fake_png(40, 40)));
        let relays = relays_in(&out.graphics);
        assert!(matches!(
            relays[0],
            Relay::Image {
                id: 7,
                format: PixelFormat::Png,
                ..
            }
        ));
        let Relay::Place {
            key: PlacementKey::Anchor(anchor),
            image: 7,
            layout,
        } = &relays[1]
        else {
            panic!("第二条应是放置：{relays:?}");
        };
        assert_eq!((layout.columns, layout.rows), (4, 2));
        assert_eq!(anchors_at(&h.term, 0, 0), [*anchor]);
        // Ghostty：右移到图片右边一格，下移 行数 - 1。
        assert_eq!(cursor(&h.term), (1, 4));
        assert_eq!(out.reply, b"\x1b_Gi=7;OK\x1b\\");

        assert!(relays_in(&out.plain).is_empty());
        assert!(!String::from_utf8_lossy(&out.plain).contains('\u{E0001}'));
    }

    #[test]
    fn grid_size_comes_from_pixels_when_not_given() {
        let mut h = Harness::new();
        let out = h.feed(&apc("a=T,f=100,q=2,i=1", &fake_png(35, 30)));
        let relays = relays_in(&out.graphics);
        let Relay::Place { layout, .. } = &relays[1] else {
            panic!("{relays:?}");
        };
        assert_eq!((layout.columns, layout.rows), (4, 2));
    }

    #[test]
    fn plain_clients_end_up_with_the_same_grid_layout() {
        let mut h = Harness::new();
        let bytes = [
            b"ab\x1b[2;3H".to_vec(),
            apc("a=T,f=100,q=2,i=1,c=3,r=2", &fake_png(30, 40)),
            b"cd".to_vec(),
        ]
        .concat();
        let out = h.feed(&bytes);
        let plain = fresh_term_from(&out.plain);
        assert_eq!(cursor(&plain), cursor(&h.term));
        for line in 0..i32::from(ROWS) {
            for column in 0..usize::from(COLS) {
                let point = Point::new(Line(line), Column(column));
                assert_eq!(plain.grid()[point].c, h.term.grid()[point].c, "{point:?}");
            }
        }
        // 放置后光标在图片右边一格（第 3 行第 6 列），后面的字从那里接着写。
        assert_eq!(h.term.grid()[Point::new(Line(2), Column(5))].c, 'c');
    }

    #[test]
    fn placement_inside_a_synchronized_update_uses_the_pending_cursor() {
        let mut h = Harness::new();
        let bytes = [
            b"\x1b[?2026h\x1b[3;5H".to_vec(),
            apc("a=T,f=100,q=2,i=1,c=2,r=1", &fake_png(20, 20)),
            b"\x1b[?2026l".to_vec(),
        ]
        .concat();
        let anchor = anchor_placed(&relays_in(&h.feed(&bytes).graphics));
        assert_eq!(anchors_at(&h.term, 2, 4), [anchor]);
    }

    #[test]
    fn chunked_transmission_split_across_reads() {
        let png = fake_png(20, 20);
        let encoded = STANDARD.encode(&png);
        let (first, rest) = encoded.split_at(8);
        let (second, last) = rest.split_at(8);
        let mut stream = format!("\x1b_Ga=T,f=100,i=3,c=2,r=1,m=1;{first}\x1b\\").into_bytes();
        stream.extend(format!("\x1b_Gm=1;{second}\x1b\\").into_bytes());
        stream.extend(format!("\x1b_Gm=0;{last}\x1b\\").into_bytes());

        let mut h = Harness::new();
        let mut graphics = Vec::new();
        let mut reply = Vec::new();
        for piece in stream.chunks(3) {
            let out = h.feed(piece);
            graphics.extend(out.graphics);
            reply.extend(out.reply);
        }
        let relays = relays_in(&graphics);
        let Relay::Image { id: 3, data, .. } = &relays[0] else {
            panic!("{relays:?}");
        };
        assert_eq!(data, &png);
        let anchor = anchor_placed(&relays);
        assert_eq!(anchors_at(&h.term, 0, 0), [anchor]);
        assert_eq!(reply, b"\x1b_Gi=3;OK\x1b\\");
    }

    #[test]
    fn delete_placements_then_the_image() {
        let mut h = Harness::new();
        let anchor = h.place(5, 2, 1);
        let relays = relays_in(&h.feed(b"\x1b_Ga=d,d=a\x1b\\").graphics);
        assert_eq!(relays, [Relay::Unplace(PlacementKey::Anchor(anchor))]);
        let relays = relays_in(&h.feed(b"\x1b_Ga=d,d=I,i=5\x1b\\").graphics);
        assert_eq!(relays, [Relay::DropImage(5)]);
        let out = h.feed(&apc("a=p,i=5", b""));
        assert_eq!(out.reply, b"\x1b_Gi=5;ENOENT: image not found\x1b\\");
    }

    #[test]
    fn replacing_a_named_placement_drops_the_old_one() {
        let mut h = Harness::new();
        h.feed(&apc("a=t,f=100,q=2,i=2", &fake_png(20, 20)));
        let first = anchor_placed(&relays_in(
            &h.feed(b"\x1b_Ga=p,q=2,i=2,p=9,c=1,r=1\x1b\\").graphics,
        ));
        let relays = relays_in(&h.feed(b"\x1b_Ga=p,q=2,i=2,p=9,c=1,r=1\x1b\\").graphics);
        assert_eq!(relays[0], Relay::Unplace(PlacementKey::Anchor(first)));
        assert_ne!(anchor_placed(&relays), first);
    }

    #[test]
    fn keyframe_carries_only_placements_still_in_the_grid() {
        let mut h = Harness::new();
        let anchor = h.place(1, 2, 1);
        let relays = relays_in(&h.graphics.keyframe_relays(&h.term, usize::from(ROWS)));
        assert_eq!(relays[0], Relay::DropAll);
        assert!(matches!(relays[1], Relay::Image { id: 1, .. }));
        assert!(matches!(
            relays[2],
            Relay::Place { key: PlacementKey::Anchor(a), .. } if a == anchor
        ));

        h.feed(b"\x1b[H\x1b[2K");
        let relays = relays_in(&h.graphics.keyframe_relays(&h.term, usize::from(ROWS)));
        assert_eq!(relays, [Relay::DropAll]);
    }

    #[test]
    fn ris_drops_everything() {
        let mut h = Harness::new();
        h.place(1, 2, 1);
        let relays = relays_in(&h.feed(b"\x1bc").graphics);
        assert_eq!(relays, [Relay::DropAll]);
        let relays = relays_in(&h.graphics.keyframe_relays(&h.term, usize::from(ROWS)));
        assert_eq!(relays, [Relay::DropAll]);
    }

    #[test]
    fn snapshots_keep_or_strip_graphics() {
        let mut h = Harness::new();
        let anchor = h.place(1, 2, 1);
        h.feed(
            "\x1b[3;1H\x1b[38;2;0;0;7m\x1b[58;2;0;0;3m\u{10EEEE}\u{0305}\u{0305}\x1b[m".as_bytes(),
        );

        let kept = fresh_term_from(&snapshot_ansi(&h.term, None));
        assert_eq!(anchors_at(&kept, 0, 0), [anchor]);
        let placeholder = &kept.grid()[Point::new(Line(2), Column(0))];
        assert_eq!(placeholder.c, PLACEHOLDER);
        assert_eq!(
            placeholder.underline_color(),
            Some(alacritty_terminal::vte::ansi::Color::Spec(Rgb {
                r: 0,
                g: 0,
                b: 3
            }))
        );
        assert_eq!(placeholder.zerowidth(), Some(&['\u{0305}', '\u{0305}'][..]));

        let plain = fresh_term_from(&snapshot_ansi_plain(&h.term, usize::from(ROWS)));
        assert!(anchors_at(&plain, 0, 0).is_empty());
        let blank = &plain.grid()[Point::new(Line(2), Column(0))];
        assert_eq!(blank.c, ' ');
        // 下划线颜色还在，alacritty 给这格留着附加数据，零宽字符是空的而不是没有。
        assert!(blank.zerowidth().unwrap_or_default().is_empty());
    }

    #[test]
    fn temporary_file_is_read_then_removed() {
        let path = std::env::temp_dir().join(format!(
            "tty-graphics-protocol-smelt-{}.png",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, fake_png(20, 20)).unwrap();
        let mut h = Harness::new();
        let out = h.feed(&apc("a=t,t=t,f=100,i=4", path.to_string_lossy().as_bytes()));
        assert_eq!(out.reply, b"\x1b_Gi=4;OK\x1b\\");
        assert!(!path.exists());
        assert!(matches!(
            relays_in(&out.graphics)[0],
            Relay::Image { id: 4, .. }
        ));
    }

    #[test]
    fn refuses_device_files() {
        let mut h = Harness::new();
        let out = h.feed(&apc("a=t,t=f,f=100,i=4", b"/dev/zero"));
        assert_eq!(
            out.reply,
            b"\x1b_Gi=4;EINVAL: refusing to read this path\x1b\\"
        );
        assert!(relays_in(&out.graphics).is_empty());
    }

    #[test]
    fn raw_pixels_must_match_their_size() {
        let mut h = Harness::new();
        let out = h.feed(&apc("a=t,f=24,s=2,v=1,i=6", &[0; 6]));
        assert_eq!(out.reply, b"\x1b_Gi=6;OK\x1b\\");
        let out = h.feed(&apc("a=t,f=32,s=2,v=1,i=6", &[0; 6]));
        assert_eq!(
            out.reply,
            b"\x1b_Gi=6;EINVAL: pixel data does not match s and v\x1b\\"
        );
    }
}
