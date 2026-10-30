//! kitty 图形协议（`ESC _G <控制项>;<数据> ESC \`）。
//!
//! smeltd 是唯一解析程序原始图形命令的地方：它在 PTY 输出里把命令切出来（[`GraphicsSplitter`]），
//! 按 Ghostty 的语义处理（读文件、拼分块、算占几格、移光标），再给桌面端发一套自己的转发命令
//! （[`Relay`]）。桌面端只认带 `k=1` 的转发命令；旧版守护原样透传的程序命令没有这个键，会被忽略。
//!
//! 图片在网格里怎么落位：
//! - 直接放置（`a=T` / `a=p`，不带 `U=1`）：只在左上角那一格挂一串零宽的锚点标记
//!   （[`anchor_marker`]），图片作为浮层盖在格子上。标记跟着格子滚动、进回滚区、进快照；这一格
//!   被改写或擦掉，图片就跟着没了。不能每格都写占位符：Pi 在 Ghostty 下重绘时逐行
//!   `CUP + EL`，图片下面那几行会被擦掉。
//! - Unicode 占位符（程序自己打印 U+10EEEE）：本来就是普通文字，一格就是一块图，见
//!   [`placeholder_marks`] / [`resolve_placeholder`]。
//!
//! 程序按格子像素算图片占几行（Pi 发 `CSI 16 t` 问，没回答就按 9×18 算，图会被拉变形）。
//! vte 不认这条查询和 `CSI 19 t`，所以也由切分器挑出来（[`Piece::SizeQuery`]），smeltd 用
//! 排版图片的同一份格子尺寸回答。

use std::borrow::Cow;
use std::io::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

mod diacritics;

use diacritics::ROWCOLUMN_DIACRITICS;

const ESC: u8 = 0x1b;

/// vte 不认、由切分器挑出来的 XTWINOPS 查询的共同前缀。
const SIZE_QUERY_PREFIX: &[u8] = b"\x1b[1";

/// vte 不认的 XTWINOPS 尺寸查询。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeQuery {
    /// `CSI 16 t`：一个字符格多少像素，应答 `CSI 6 ; 高 ; 宽 t`。
    Cell,
    /// `CSI 19 t`：屏幕多少行列。终端不知道物理屏幕，同 Ghostty、VTE 报文字区的行列，应答
    /// `CSI 9 ; 行 ; 列 t`。
    Screen,
}

impl SizeQuery {
    fn from_digit(b: u8) -> Option<Self> {
        match b {
            b'6' => Some(Self::Cell),
            b'9' => Some(Self::Screen),
            _ => None,
        }
    }

    /// 扣下的、还差最后的 `t` 的那段。
    fn held(self) -> &'static [u8] {
        match self {
            Self::Cell => b"\x1b[16",
            Self::Screen => b"\x1b[19",
        }
    }

    fn sequence(self) -> &'static [u8] {
        match self {
            Self::Cell => b"\x1b[16t",
            Self::Screen => b"\x1b[19t",
        }
    }
}

/// 一条图形命令（APC 正文）最多攒这么多字节，超了整条丢弃。程序按 4096 字节分块发；守护转发
/// 给桌面端时一张图一条命令，所以上限按单张图来定。
pub const MAX_COMMAND_BYTES: usize = 64 << 20;

/// 切分出来的一段输出，按原顺序交出。
#[derive(Debug, PartialEq, Eq)]
pub enum Piece<'a> {
    /// 原样交给终端解析器的字节。
    Bytes(&'a [u8]),
    /// 一条完整图形命令的正文（`ESC _G` 与 `ESC \` 之间，不含 `G`）。
    Command(&'a [u8]),
    /// 刚交出的字节以 `ESC c`（RIS）结尾。kitty 在 RIS 时清掉所有图片。
    Reset,
    /// 刚交出的字节以 `CSI 16 t` / `CSI 19 t` 结尾：程序问尺寸。
    SizeQuery(SizeQuery),
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum SplitState {
    #[default]
    Ground,
    /// 刚看到 ESC，先扣下：下一字节是 `_` 就可能是图形命令，是 `[` 就可能是尺寸查询。
    Esc,
    /// 已经对上 [`SIZE_QUERY_PREFIX`] 的前这么多字节。扣下的就是它的这一段，对不上时原样交出，
    /// 不用另存。CSI 很多（颜色、光标），绝大多数在 `[` 后第一字节就对不上。
    Query(u8),
    /// 对上了 `ESC [ 1 6` / `ESC [ 1 9`，还差 `t`。
    SizeQuery(SizeQuery),
    /// 看到 `ESC _`，还差 `G`。
    ApcStart,
    Body,
    /// 正文里看到 ESC，等 `\`。
    BodyEsc,
}

/// 把输出切成「普通字节」和「图形命令」，状态跨 `feed` 保持（命令可能被 PTY read 切断）。
///
/// 别的 APC（`ESC _` 后面不是 `G`）原样放过，交给 vte 照常忽略。正文里遇到 ESC 而不是 ST、
/// 或遇到 CAN / SUB 时中止这条命令，跟 vte 处理串的方式一致，免得两边对「命令在哪结束」
/// 看法不同。
#[derive(Default)]
pub struct GraphicsSplitter {
    state: SplitState,
    body: Vec<u8>,
    overflow: bool,
}

impl GraphicsSplitter {
    pub fn feed(&mut self, chunk: &[u8], mut emit: impl FnMut(Piece<'_>)) {
        let mut start = 0;
        for (i, &b) in chunk.iter().enumerate() {
            match self.state {
                SplitState::Ground => {
                    if b == ESC {
                        if start < i {
                            emit(Piece::Bytes(&chunk[start..i]));
                        }
                        self.state = SplitState::Esc;
                        start = i + 1;
                    }
                }
                SplitState::Esc => self.after_esc(b, i, &mut start, &mut emit),
                SplitState::Query(matched) => {
                    let matched = usize::from(matched);
                    if matched < SIZE_QUERY_PREFIX.len() && b == SIZE_QUERY_PREFIX[matched] {
                        self.state = SplitState::Query(matched as u8 + 1);
                    } else if let Some(query) = SizeQuery::from_digit(b)
                        .filter(|_| matched == SIZE_QUERY_PREFIX.len())
                    {
                        self.state = SplitState::SizeQuery(query);
                    } else {
                        emit(Piece::Bytes(&SIZE_QUERY_PREFIX[..matched]));
                        self.ground_from(b, i, &mut start);
                    }
                }
                SplitState::SizeQuery(query) => {
                    if b == b't' {
                        emit(Piece::Bytes(query.sequence()));
                        emit(Piece::SizeQuery(query));
                        self.state = SplitState::Ground;
                        start = i + 1;
                    } else {
                        emit(Piece::Bytes(query.held()));
                        self.ground_from(b, i, &mut start);
                    }
                }
                SplitState::ApcStart => {
                    if b == b'G' {
                        self.state = SplitState::Body;
                        start = i + 1;
                    } else {
                        emit(Piece::Bytes(b"\x1b_"));
                        self.ground_from(b, i, &mut start);
                    }
                }
                SplitState::Body => match b {
                    ESC => self.state = SplitState::BodyEsc,
                    0x18 | 0x1a => {
                        self.finish_body();
                        start = i;
                    }
                    _ => self.push(b),
                },
                SplitState::BodyEsc => {
                    if b == b'\\' {
                        if !self.overflow {
                            emit(Piece::Command(&self.body));
                        }
                        self.finish_body();
                        start = i + 1;
                    } else {
                        self.finish_body();
                        self.after_esc(b, i, &mut start, &mut emit);
                    }
                }
            }
        }
        if self.state == SplitState::Ground && start < chunk.len() {
            emit(Piece::Bytes(&chunk[start..]));
        }
    }

    fn after_esc(&mut self, b: u8, i: usize, start: &mut usize, emit: &mut impl FnMut(Piece<'_>)) {
        match b {
            b'_' => {
                self.state = SplitState::ApcStart;
                *start = i + 1;
            }
            b'c' => {
                emit(Piece::Bytes(b"\x1bc"));
                emit(Piece::Reset);
                self.state = SplitState::Ground;
                *start = i + 1;
            }
            b'[' => self.state = SplitState::Query(2),
            _ => {
                emit(Piece::Bytes(b"\x1b"));
                self.ground_from(b, i, start);
            }
        }
    }

    /// 扣下的字节已经交出去，从 `b` 起回到普通状态；`b` 本身是 ESC 的话接着扣。
    fn ground_from(&mut self, b: u8, i: usize, start: &mut usize) {
        if b == ESC {
            self.state = SplitState::Esc;
            *start = i + 1;
        } else {
            self.state = SplitState::Ground;
            *start = i;
        }
    }

    fn push(&mut self, b: u8) {
        if self.overflow {
            return;
        }
        if self.body.len() >= MAX_COMMAND_BYTES {
            self.overflow = true;
            self.body = Vec::new();
            return;
        }
        self.body.push(b);
    }

    fn finish_body(&mut self) {
        self.state = SplitState::Ground;
        self.overflow = false;
        // 一张大图会把缓冲撑到几十 MB，用完就还回去。
        if self.body.capacity() > 1 << 20 {
            self.body = Vec::new();
        } else {
            self.body.clear();
        }
    }
}

/// 源图裁剪区域（像素）。宽高为 0 表示一直到图片边缘。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 一条命令的控制项。没列出的键（动画、相对放置的偏移）不认。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Controls {
    /// `a`：t 传输、T 传输并放置、p 放置、d 删除、q 查询……
    pub action: u8,
    /// `q`：1 不回成功，2 什么都不回。
    pub quiet: u8,
    /// `f`：24 RGB、32 RGBA、100 PNG。
    pub format: u32,
    /// `t`：d 直接、f 文件、t 临时文件、s 共享内存。
    pub medium: u8,
    /// `o`：z 表示 zlib 压缩。
    pub compression: Option<u8>,
    /// `s` / `v`：原始像素数据的宽高。
    pub data_width: u32,
    pub data_height: u32,
    /// `S` / `O`：从文件读多少字节、从哪读。
    pub data_size: u32,
    pub data_offset: u32,
    pub image_id: u32,
    pub image_number: u32,
    pub placement_id: u32,
    /// `m=1`：后面还有分块。
    pub more: bool,
    /// `x` / `y` / `w` / `h`。
    pub source: SourceRect,
    /// `X` / `Y`：左上角格子里的像素偏移。
    pub offset_x: u32,
    pub offset_y: u32,
    /// `c` / `r`：占几列几行，0 表示按图片算。
    pub columns: u32,
    pub rows: u32,
    /// `C=1`：放置后不移光标。
    pub keep_cursor: bool,
    /// `U=1`：虚拟放置，配合 Unicode 占位符。
    pub virtual_placement: bool,
    pub z: i32,
    /// `P`：相对放置的父图片。不支持相对放置，只用来拒绝。
    pub parent_id: u32,
    /// `d`：删除哪些。
    pub delete: u8,
    /// `k=1`：smeltd 发给桌面端的转发命令。kitty 没有这个键。
    pub relay: bool,
    /// `A`：转发命令里的锚点标记号。kitty 没有这个键。
    pub anchor: u64,
    /// `W` / `L`：转发命令里图片画多宽多高（千分之一格）。kitty 没有这两个键。
    pub display_width: u32,
    pub display_height: u32,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            compression: None,
            data_width: 0,
            data_height: 0,
            data_size: 0,
            data_offset: 0,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            more: false,
            source: SourceRect::default(),
            offset_x: 0,
            offset_y: 0,
            columns: 0,
            rows: 0,
            keep_cursor: false,
            virtual_placement: false,
            z: 0,
            parent_id: 0,
            delete: b'a',
            relay: false,
            anchor: 0,
            display_width: 0,
            display_height: 0,
        }
    }
}

impl Controls {
    fn set(&mut self, key: u8, value: &[u8]) {
        let text = std::str::from_utf8(value).unwrap_or("");
        let number = || text.parse::<u32>().ok();
        let one_byte = || match value {
            [b] => Some(*b),
            _ => None,
        };
        match key {
            b'a' => self.action = one_byte().unwrap_or(self.action),
            b'q' => self.quiet = number().map_or(self.quiet, |q| q.min(2) as u8),
            b'f' => self.format = number().unwrap_or(self.format),
            b't' => self.medium = one_byte().unwrap_or(self.medium),
            b'o' => self.compression = one_byte(),
            b's' => self.data_width = number().unwrap_or(0),
            b'v' => self.data_height = number().unwrap_or(0),
            b'S' => self.data_size = number().unwrap_or(0),
            b'O' => self.data_offset = number().unwrap_or(0),
            b'i' => self.image_id = number().unwrap_or(0),
            b'I' => self.image_number = number().unwrap_or(0),
            b'p' => self.placement_id = number().unwrap_or(0),
            b'm' => self.more = number() == Some(1),
            b'x' => self.source.x = number().unwrap_or(0),
            b'y' => self.source.y = number().unwrap_or(0),
            b'w' => self.source.width = number().unwrap_or(0),
            b'h' => self.source.height = number().unwrap_or(0),
            b'X' => self.offset_x = number().unwrap_or(0),
            b'Y' => self.offset_y = number().unwrap_or(0),
            b'c' => self.columns = number().unwrap_or(0),
            b'r' => self.rows = number().unwrap_or(0),
            b'C' => self.keep_cursor = number() == Some(1),
            b'U' => self.virtual_placement = number() == Some(1),
            b'z' => self.z = text.parse().unwrap_or(0),
            b'P' => self.parent_id = number().unwrap_or(0),
            b'd' => self.delete = one_byte().unwrap_or(self.delete),
            b'k' => self.relay = number() == Some(1),
            b'A' => self.anchor = text.parse().unwrap_or(0),
            b'W' => self.display_width = number().unwrap_or(0),
            b'L' => self.display_height = number().unwrap_or(0),
            _ => {}
        }
    }
}

/// 解析好的一条命令。`payload` 是分号后面的原文（传输时是 base64）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub controls: Controls,
    pub payload: Vec<u8>,
}

impl Command {
    pub fn parse(body: &[u8]) -> Self {
        let (controls_text, payload) = match body.iter().position(|&b| b == b';') {
            Some(at) => (&body[..at], &body[at + 1..]),
            None => (body, &[][..]),
        };
        let mut controls = Controls::default();
        for item in controls_text.split(|&b| b == b',') {
            if let [key, b'=', value @ ..] = item {
                controls.set(*key, value);
            }
        }
        Self {
            controls,
            payload: payload.to_vec(),
        }
    }
}

/// 图片数据的像素格式。zlib 压缩的原始像素守护先解开再转发。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Png,
    Rgb { width: u32, height: u32 },
    Rgba { width: u32, height: u32 },
}

/// 一个放置在桌面端怎么找到：直接放置靠网格里的锚点标记，虚拟放置靠占位符格子里的编号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlacementKey {
    /// 标记号不会是 0：转发命令里 `A` 缺省就是 0，靠它区分两种键。
    Anchor(u64),
    Virtual {
        image: u32,
        placement: u32,
    },
}

/// 放置的版面，由守护按 Ghostty 的规则算好，桌面端照着画。
///
/// 图片在格子里画多大、偏多少按「千分之一格」记：守护按它那边的格子像素算，桌面端乘上自己的
/// 格子尺寸，两边字号不同也画得一致。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// 占几列几行（光标移动、清理都按它）。
    pub columns: u32,
    pub rows: u32,
    /// 源图裁剪（像素）。
    pub source: SourceRect,
    /// 图片左上角相对锚点格的偏移，千分之一格。
    pub offset_x: u32,
    pub offset_y: u32,
    /// 裁剪后的源图画出来多宽多高，千分之一格。
    pub width: u32,
    pub height: u32,
    pub z: i32,
}

/// 每格的千分之一，[`Layout`] 里尺寸和偏移的单位。
pub const MILLI_CELLS: u32 = 1000;

/// smeltd → 桌面端的转发命令。编码成带 `k=1` 的 APC，混在终端输出里按顺序送达；vte 忽略 APC，
/// 不认识它的终端也不受影响。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relay {
    /// 一张图的完整数据，`data` 是解码后的字节。
    Image {
        id: u32,
        format: PixelFormat,
        data: Vec<u8>,
    },
    /// 新增一个放置；同一个键已有放置时替换它。
    Place {
        key: PlacementKey,
        image: u32,
        layout: Layout,
    },
    /// 删掉一个放置，图片数据留着。
    Unplace(PlacementKey),
    /// 删掉一张图和它所有的放置。
    DropImage(u32),
    /// 删掉所有图片和放置：关键帧开头、程序发 RIS 时。
    DropAll,
}

/// [`Relay::Image`] 的编码，借用图片数据，不必为了发一次先拷一份。
pub fn encode_image_relay(id: u32, format: PixelFormat, data: &[u8]) -> Vec<u8> {
    let mut out = b"\x1b_Gk=1,".to_vec();
    let _ = write!(out, "a=t,i={id},");
    let _ = match format {
        PixelFormat::Png => write!(out, "f=100"),
        PixelFormat::Rgb { width, height } => write!(out, "f=24,s={width},v={height}"),
        PixelFormat::Rgba { width, height } => write!(out, "f=32,s={width},v={height}"),
    };
    out.push(b';');
    out.extend_from_slice(STANDARD.encode(data).as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

impl Relay {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = b"\x1b_Gk=1,".to_vec();
        match self {
            Self::Image { id, format, data } => return encode_image_relay(*id, *format, data),
            Self::Place { key, image, layout } => {
                let _ = write!(
                    out,
                    "a=p,i={image},c={},r={},x={},y={},w={},h={},X={},Y={},W={},L={},z={}",
                    layout.columns,
                    layout.rows,
                    layout.source.x,
                    layout.source.y,
                    layout.source.width,
                    layout.source.height,
                    layout.offset_x,
                    layout.offset_y,
                    layout.width,
                    layout.height,
                    layout.z,
                );
                write_key(&mut out, key);
            }
            Self::Unplace(key) => {
                out.extend_from_slice(b"a=d,d=p");
                if let PlacementKey::Virtual { image, .. } = key {
                    let _ = write!(out, ",i={image}");
                }
                write_key(&mut out, key);
            }
            Self::DropImage(id) => {
                let _ = write!(out, "a=d,d=I,i={id}");
            }
            Self::DropAll => out.extend_from_slice(b"a=d,d=A"),
        }
        out.extend_from_slice(b"\x1b\\");
        out
    }

    /// 只认转发命令；程序的原始命令（没有 `k=1`）返回 `None`。
    pub fn parse(body: &[u8]) -> Option<Self> {
        let Command {
            controls: c,
            payload,
        } = Command::parse(body);
        if !c.relay {
            return None;
        }
        let key = if c.anchor != 0 {
            PlacementKey::Anchor(c.anchor)
        } else {
            PlacementKey::Virtual {
                image: c.image_id,
                placement: c.placement_id,
            }
        };
        match c.action {
            b't' => {
                let format = match c.format {
                    100 => PixelFormat::Png,
                    24 => PixelFormat::Rgb {
                        width: c.data_width,
                        height: c.data_height,
                    },
                    32 => PixelFormat::Rgba {
                        width: c.data_width,
                        height: c.data_height,
                    },
                    _ => return None,
                };
                let data = STANDARD.decode(&payload).ok()?;
                Some(Self::Image {
                    id: c.image_id,
                    format,
                    data,
                })
            }
            b'p' => Some(Self::Place {
                key,
                image: c.image_id,
                layout: Layout {
                    columns: c.columns,
                    rows: c.rows,
                    source: c.source,
                    offset_x: c.offset_x,
                    offset_y: c.offset_y,
                    width: c.display_width,
                    height: c.display_height,
                    z: c.z,
                },
            }),
            b'd' => match c.delete {
                b'A' => Some(Self::DropAll),
                b'I' => Some(Self::DropImage(c.image_id)),
                b'p' => Some(Self::Unplace(key)),
                _ => None,
            },
            _ => None,
        }
    }
}

fn write_key(out: &mut Vec<u8>, key: &PlacementKey) {
    let _ = match key {
        PlacementKey::Anchor(anchor) => write!(out, ",A={anchor}"),
        PlacementKey::Virtual { placement, .. } => write!(out, ",U=1,p={placement}"),
    };
}

/// 锚点标记开头：U+E0001（LANGUAGE TAG，已废弃、零宽、不可见），后面跟 16 个标签字符
/// （U+E0030.. / U+E0061..）写成的十六进制标记号。都是零宽字符，alacritty 把它们挂在
/// 前一格上，不占格子、不挪光标。
const ANCHOR_INTRODUCER: char = '\u{E0001}';
const ANCHOR_DIGITS: usize = 16;
const TAG_BASE: u32 = 0xE0000;

/// 标记号 `anchor` 对应的零宽字符串。
pub fn anchor_marker(anchor: u64) -> String {
    let mut marker = String::with_capacity(4 * (ANCHOR_DIGITS + 1));
    marker.push(ANCHOR_INTRODUCER);
    for shift in (0..ANCHOR_DIGITS).rev() {
        let digit = ((anchor >> (shift * 4)) & 0xf) as u32;
        let ascii = char::from_digit(digit, 16).map_or(0, u32::from);
        marker.extend(char::from_u32(TAG_BASE + ascii));
    }
    marker
}

/// 一格的零宽字符里挂着的所有锚点标记号，按挂上的先后。同一格可能被放置过不止一次。
pub fn anchors_in(zerowidth: &[char]) -> Vec<u64> {
    let mut anchors = Vec::new();
    let mut rest = zerowidth;
    while let Some(at) = rest.iter().position(|&c| c == ANCHOR_INTRODUCER) {
        let digits = &rest[at + 1..];
        if let Some(anchor) = parse_anchor_digits(digits) {
            anchors.push(anchor);
            rest = &digits[ANCHOR_DIGITS..];
        } else {
            rest = digits;
        }
    }
    anchors
}

fn parse_anchor_digits(digits: &[char]) -> Option<u64> {
    let digits = digits.get(..ANCHOR_DIGITS)?;
    digits.iter().try_fold(0u64, |anchor, &c| {
        let ascii = u32::from(c).checked_sub(TAG_BASE)?;
        let digit = char::from_u32(ascii)?.to_digit(16)?;
        Some(anchor << 4 | u64::from(digit))
    })
}

/// 去掉零宽字符里的锚点标记，其余（变体选择符、组合符号、国旗的标签序列）原样留着。
/// 复制、搜索、排版前用。
pub fn without_anchors(zerowidth: &[char]) -> Cow<'_, [char]> {
    if !zerowidth.contains(&ANCHOR_INTRODUCER) {
        return Cow::Borrowed(zerowidth);
    }
    let mut kept = Vec::with_capacity(zerowidth.len());
    let mut i = 0;
    while i < zerowidth.len() {
        let c = zerowidth[i];
        if c == ANCHOR_INTRODUCER && parse_anchor_digits(&zerowidth[i + 1..]).is_some() {
            i += ANCHOR_DIGITS + 1;
        } else {
            kept.push(c);
            i += 1;
        }
    }
    Cow::Owned(kept)
}

/// 去掉文字里 kitty 图形留下的痕迹：锚点标记，以及占位符和紧跟其后的行列编号。复制选区、
/// 把终端读成文字时用。
pub fn without_graphics(text: &str) -> Cow<'_, str> {
    if !text.contains([ANCHOR_INTRODUCER, PLACEHOLDER]) {
        return Cow::Borrowed(text);
    }
    let chars: Vec<char> = text.chars().collect();
    let mut kept = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ANCHOR_INTRODUCER && parse_anchor_digits(&chars[i + 1..]).is_some() {
            i += ANCHOR_DIGITS + 1;
        } else if c == PLACEHOLDER {
            i += 1;
            while chars.get(i).is_some_and(|&c| diacritic_index(c).is_some()) {
                i += 1;
            }
        } else {
            kept.push(c);
            i += 1;
        }
    }
    Cow::Owned(kept)
}

/// Unicode 占位符：这一格显示图片的一块，图片 id 的低 24 位在前景色里、放置 id 在下划线颜色里。
pub const PLACEHOLDER: char = '\u{10EEEE}';

fn diacritic_index(c: char) -> Option<u32> {
    ROWCOLUMN_DIACRITICS
        .binary_search(&c)
        .ok()
        .and_then(|index| u32::try_from(index).ok())
}

/// 占位符后面依次跟的编号：行、列、图片 id 的最高字节。可以省略后面的几个。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaceholderMarks {
    pub row: Option<u32>,
    pub column: Option<u32>,
    pub image_id_high: Option<u8>,
}

pub fn placeholder_marks(zerowidth: &[char]) -> PlaceholderMarks {
    let mut values = zerowidth.iter().map(|&c| diacritic_index(c));
    let row = values.next().flatten();
    let column = row.and(values.next().flatten());
    let image_id_high = column
        .and(values.next().flatten())
        .and_then(|high| u8::try_from(high).ok());
    PlaceholderMarks {
        row,
        column,
        image_id_high,
    }
}

/// 一格占位符最终显示哪张图的哪一块。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaceholderCell {
    pub image: u32,
    pub placement: u32,
    pub row: u32,
    pub column: u32,
}

/// 按 kitty 的规则补全省略的编号。`image_low` 取自前景色（24 位），`placement` 取自下划线颜色，
/// `left` 是同一行紧挨着的左边一格（如果也是占位符）。左边一格属于同一张图、同一个放置时：
/// 行列都省略就沿用它的行、列号加一、沿用它的最高字节；只写了行且和它同一行，列号加一。
/// 其余省略的编号按 0 算。
pub fn resolve_placeholder(
    image_low: u32,
    placement: u32,
    marks: PlaceholderMarks,
    left: Option<&PlaceholderCell>,
) -> PlaceholderCell {
    let image_low = image_low & 0xff_ffff;
    let left =
        left.filter(|left| left.image & 0xff_ffff == image_low && left.placement == placement);
    let left_high = left.map_or(0, |left| left.image >> 24);
    let (row, column, high) = match (marks.row, marks.column) {
        (Some(row), Some(column)) => (row, column, marks.image_id_high.map(u32::from)),
        (Some(row), None) => match left.filter(|left| left.row == row) {
            Some(left) => (row, left.column + 1, Some(left_high)),
            None => (row, 0, None),
        },
        (None, _) => match left {
            Some(left) => (left.row, left.column + 1, Some(left_high)),
            None => (0, 0, None),
        },
    };
    PlaceholderCell {
        image: image_low | high.unwrap_or(0) << 24,
        placement,
        row,
        column,
    }
}

/// PNG 的宽高，从 IHDR 里读，不解码像素。
pub fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.get(..8)? != SIGNATURE || data.get(12..16)? != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(data.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(data.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

/// 源图裁剪后的像素大小，超出图片的部分截掉。照 Ghostty 的 `Placement.sourceRect`。
pub fn source_size(source: SourceRect, image: (u32, u32)) -> (u32, u32) {
    let clip = |offset: u32, len: u32, full: u32| {
        let offset = offset.min(full);
        if len == 0 {
            full - offset
        } else {
            len.min(full - offset)
        }
    };
    (
        clip(source.x, source.width, image.0),
        clip(source.y, source.height, image.1),
    )
}

/// 格内偏移不超过一格，照 Ghostty 的 `Placement.cellOffset`。格子像素未知时没有偏移。
fn clamp_offset(offset: (u32, u32), cell: (u32, u32)) -> (u32, u32) {
    let clamp = |offset: u32, cell: u32| if cell == 0 { 0 } else { offset.min(cell - 1) };
    (clamp(offset.0, cell.0), clamp(offset.1, cell.1))
}

fn scale(value: u32, num: u32, den: u32) -> u32 {
    if den == 0 {
        return 0;
    }
    u32::try_from(u64::from(value) * u64::from(num) / u64::from(den)).unwrap_or(u32::MAX)
}

/// 裁剪后的源图画出来多大（像素），照 Ghostty 的 `Placement.pixelSize`：c、r 都没给按原始像素；
/// 都给了撑满这些格子（减去格内偏移）；只给一个，按源图比例算另一边。`offset` 已经截过。
fn pixel_size(
    columns: u32,
    rows: u32,
    source: (u32, u32),
    offset: (u32, u32),
    cell: (u32, u32),
) -> (u32, u32) {
    let width = || cell.0.saturating_mul(columns).saturating_sub(offset.0);
    let height = || cell.1.saturating_mul(rows).saturating_sub(offset.1);
    match (columns > 0, rows > 0) {
        (false, false) => source,
        (true, true) => (width(), height()),
        (true, false) => (width(), scale(width(), source.1, source.0)),
        (false, true) => (scale(height(), source.0, source.1), height()),
    }
}

/// 放置占几列几行，照 Ghostty 的 `Placement.gridSize`：c、r 都给了就用；否则按画出来的像素
/// （见 [`pixel_size`]）加上格内偏移向上取整。格子像素未知时算不了，没给的那边是 0。
pub fn grid_size(
    columns: u32,
    rows: u32,
    source: (u32, u32),
    offset: (u32, u32),
    cell: (u32, u32),
) -> (u32, u32) {
    if columns > 0 && rows > 0 {
        return (columns, rows);
    }
    let (cell_w, cell_h) = cell;
    if cell_w == 0 || cell_h == 0 {
        return (columns, rows);
    }
    let offset = clamp_offset(offset, cell);
    let (width, height) = pixel_size(columns, rows, source, offset, cell);
    let cells = |pixels: u32, offset: u32, cell: u32| pixels.saturating_add(offset).div_ceil(cell);
    let columns = if columns > 0 {
        columns
    } else {
        cells(width, offset.0, cell_w)
    };
    let rows = if rows > 0 {
        rows
    } else {
        cells(height, offset.1, cell_h)
    };
    (columns, rows)
}

fn milli_cells(pixels: u32, cell: u32) -> u32 {
    scale(pixels, MILLI_CELLS, cell)
}

/// 直接放置的版面：占几格、画多大、偏多少都照 Ghostty（见 [`grid_size`]、[`pixel_size`]）。
/// 格子像素未知时只能撑满给定的格子。
pub fn direct_layout(c: &Controls, image: (u32, u32), cell: (u32, u32)) -> Layout {
    let source = source_size(c.source, image);
    let offset = clamp_offset((c.offset_x, c.offset_y), cell);
    let (columns, rows) = grid_size(c.columns, c.rows, source, offset, cell);
    let (width, height, offset_x, offset_y) = if cell.0 == 0 || cell.1 == 0 {
        (columns * MILLI_CELLS, rows * MILLI_CELLS, 0, 0)
    } else {
        let (width, height) = pixel_size(c.columns, c.rows, source, offset, cell);
        (
            milli_cells(width, cell.0),
            milli_cells(height, cell.1),
            milli_cells(offset.0, cell.0),
            milli_cells(offset.1, cell.1),
        )
    };
    Layout {
        columns,
        rows,
        source: c.source,
        offset_x,
        offset_y,
        width,
        height,
        z: c.z,
    }
}

/// 虚拟放置（Unicode 占位符）的版面：占位区域是 c×r 格（没给的按图片算），图片等比缩进这块
/// 区域并居中，跟 kitty 一样。格子像素未知时比例算不了，撑满。
pub fn virtual_layout(c: &Controls, image: (u32, u32), cell: (u32, u32)) -> Layout {
    let source = source_size(c.source, image);
    let (columns, rows) = grid_size(c.columns, c.rows, source, (0, 0), cell);
    let area = (columns * MILLI_CELLS, rows * MILLI_CELLS);
    let (width, height) = if cell.0 == 0 || cell.1 == 0 || source.0 == 0 || source.1 == 0 {
        area
    } else {
        // 源图换算成千分之一格，再等比缩进区域。
        let (w, h) = (milli_cells(source.0, cell.0), milli_cells(source.1, cell.1));
        if u64::from(w) * u64::from(area.1) <= u64::from(h) * u64::from(area.0) {
            (scale(area.1, w, h), area.1)
        } else {
            (area.0, scale(area.0, h, w))
        }
    };
    Layout {
        columns,
        rows,
        source: c.source,
        offset_x: (area.0 - width) / 2,
        offset_y: (area.1 - height) / 2,
        width,
        height,
        z: c.z,
    }
}

/// 放置那一刻的光标和屏幕。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacementCursor {
    /// 光标所在列（0 起）。
    pub column: usize,
    /// alacritty 的 `input_needs_wrap`：刚写满最后一列，下一个字会先换行。
    pub needs_wrap: bool,
    /// 光标下面还有几行才到屏幕底部。
    pub lines_below: usize,
    pub screen_columns: usize,
    pub screen_lines: usize,
}

/// 直接放置写进网格的字节：在光标那一格挂上锚点标记（`anchor` 为 `None` 时不挂，给不显示图片
/// 的端，网格布局和挂了的一样），再按 Ghostty 的规则移光标。
///
/// 挂标记不改那一格的字：先右移一格再写零宽标记，alacritty 把零宽字符挂在前一格上。光标在
/// 最后一列、又没处在「待换行」时右移不了，只好在那一格写个空格（写完进入待换行，标记就挂在
/// 这一格上）。
///
/// 光标移动照 Ghostty 的 `graphics_exec.zig`：`C=1` 回到原位；否则右移到图片右边一格，碰到右
/// 边界就折到下一行行首，下移「行数 - 1（折行再加 1）」行，到底就滚屏。滚屏次数封顶一屏，
/// 防止程序给一个天文数字的行数。
pub fn placement_bytes(
    cursor: PlacementCursor,
    columns: u32,
    rows: u32,
    keep_cursor: bool,
    anchor: Option<u64>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let last_column = cursor.screen_columns.saturating_sub(1);
    let column = cursor.column.min(last_column);
    // 怎么把光标挪回原样：待换行时标记直接挂在光标这一格上，光标没动过。
    let restore: &[u8] = if cursor.needs_wrap {
        b""
    } else if column < last_column {
        out.extend_from_slice(b"\x1b[C");
        b"\x1b[D"
    } else {
        out.push(b' ');
        // 写空格让光标进入了待换行，CHA 清掉它、回到这一列。
        b"\x1b[G"
    };
    if let Some(anchor) = anchor {
        out.extend_from_slice(anchor_marker(anchor).as_bytes());
    }
    if keep_cursor {
        if restore == b"\x1b[G" {
            let _ = write!(out, "\x1b[{}G", column + 1);
        } else {
            out.extend_from_slice(restore);
        }
        return out;
    }
    let target = column.saturating_add(columns as usize);
    let wraps = target >= cursor.screen_columns;
    let requested = (rows as usize).saturating_sub(1) + usize::from(wraps);
    let moves = requested.min(cursor.lines_below + cursor.screen_lines);
    for _ in 0..moves {
        out.extend_from_slice(b"\x1bD");
    }
    let _ = write!(out, "\x1b[{}G", if wraps { 1 } else { target + 1 });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut splitter = GraphicsSplitter::default();
        let mut pieces = Vec::new();
        for chunk in chunks {
            splitter.feed(chunk, |piece| {
                pieces.push(match piece {
                    Piece::Bytes(bytes) => format!("B:{}", String::from_utf8_lossy(bytes)),
                    Piece::Command(body) => format!("G:{}", String::from_utf8_lossy(body)),
                    Piece::Reset => "R".to_string(),
                    Piece::SizeQuery(SizeQuery::Cell) => "Q16".to_string(),
                    Piece::SizeQuery(SizeQuery::Screen) => "Q19".to_string(),
                });
            });
        }
        pieces
    }

    /// 相邻的 `Bytes` 拼起来，只看切分结果、不看切在哪。
    fn merged(pieces: Vec<String>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for piece in pieces {
            match (out.last_mut(), piece.strip_prefix("B:")) {
                (Some(last), Some(bytes)) if last.starts_with("B:") => last.push_str(bytes),
                _ => out.push(piece),
            }
        }
        out
    }

    #[test]
    fn splits_commands_out_of_output() {
        let stream: &[u8] = b"ab\x1b_Ga=T,f=100;QUJD\x1b\\cd\x1b[1mx";
        assert_eq!(
            merged(split(&[stream])),
            ["B:ab", "G:a=T,f=100;QUJD", "B:cd\x1b[1mx"]
        );
    }

    #[test]
    fn split_is_the_same_wherever_reads_break() {
        let stream: &[u8] = b"a\x1b\x1b_Gi=1;xy\x1b\\b\x1b_Xother\x1b\\\x1bcz\x1b_Gq=2\x1b\\";
        let whole = merged(split(&[stream]));
        assert_eq!(
            whole,
            [
                "B:a\x1b",
                "G:i=1;xy",
                "B:b\x1b_Xother\x1b\\\x1bc",
                "R",
                "B:z",
                "G:q=2",
            ]
        );
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(merged(split(&bytes)), whole);
        for at in 0..stream.len() {
            let (head, tail) = stream.split_at(at);
            assert_eq!(merged(split(&[head, tail])), whole, "切在 {at}");
        }
    }

    #[test]
    fn size_queries_are_picked_out_wherever_reads_break() {
        let stream: &[u8] =
            b"a\x1b[16tb\x1b[1mc\x1b[16;1td\x1b[14te\x1b[1\x1b[19tf\x1b[19;2tg\x1b[16\x1b[16t";
        let whole = merged(split(&[stream]));
        assert_eq!(
            whole,
            [
                "B:a\x1b[16t",
                "Q16",
                "B:b\x1b[1mc\x1b[16;1td\x1b[14te\x1b[1\x1b[19t",
                "Q19",
                "B:f\x1b[19;2tg\x1b[16\x1b[16t",
                "Q16",
            ]
        );
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(merged(split(&bytes)), whole);
        for at in 0..stream.len() {
            let (head, tail) = stream.split_at(at);
            assert_eq!(merged(split(&[head, tail])), whole, "切在 {at}");
        }
    }

    /// 对不上的 CSI 不多拆：和以前一样，ESC 开头交出一段，后面接着普通字节。
    #[test]
    fn other_csi_passes_through_in_as_few_pieces_as_before() {
        assert_eq!(split(&[b"a\x1b[38;2;1;2;3mb"]), ["B:a", "B:\x1b[", "B:38;2;1;2;3mb"]);
        assert_eq!(split(&[b"\x1b[1Hx"]), ["B:\x1b[1", "B:Hx"]);
    }

    #[test]
    fn esc_inside_a_command_aborts_it_like_vte() {
        assert_eq!(merged(split(&[b"\x1b_Gi=1;ab\x1b[2Jc"])), ["B:\x1b[2Jc"]);
        assert_eq!(merged(split(&[b"\x1b_Gi=1\x18x"])), ["B:\x18x"]);
    }

    #[test]
    fn trailing_esc_is_held_until_the_next_read() {
        assert_eq!(split(&[b"ab\x1b"]), ["B:ab"]);
        assert_eq!(merged(split(&[b"ab\x1b", b"[m"])), ["B:ab\x1b[m"]);
    }

    #[test]
    fn parses_controls_and_payload() {
        let command =
            Command::parse(b"a=T,f=100,t=f,i=7,p=3,c=10,r=4,C=1,U=1,z=-2,q=2,m=1;cGF0aA==");
        let c = &command.controls;
        assert_eq!(c.action, b'T');
        assert_eq!(c.format, 100);
        assert_eq!(c.medium, b'f');
        assert_eq!((c.image_id, c.placement_id), (7, 3));
        assert_eq!((c.columns, c.rows), (10, 4));
        assert!(c.keep_cursor && c.virtual_placement && c.more);
        assert_eq!((c.z, c.quiet), (-2, 2));
        assert_eq!(command.payload, b"cGF0aA==");

        let defaults = Command::parse(b"");
        assert_eq!(defaults.controls, Controls::default());
        assert_eq!(defaults.controls.action, b't');
        assert_eq!(defaults.controls.medium, b'd');
    }

    #[test]
    fn relay_round_trips_through_the_splitter() {
        let relays = [
            Relay::Image {
                id: 9,
                format: PixelFormat::Png,
                data: b"\x89PNG-bytes".to_vec(),
            },
            Relay::Image {
                id: 4_000_000_000,
                format: PixelFormat::Rgba {
                    width: 2,
                    height: 1,
                },
                data: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
            Relay::Place {
                key: PlacementKey::Anchor(u64::MAX - 5),
                image: 9,
                layout: Layout {
                    columns: 10,
                    rows: 3,
                    source: SourceRect {
                        x: 1,
                        y: 2,
                        width: 3,
                        height: 4,
                    },
                    offset_x: 5,
                    offset_y: 6,
                    width: 8,
                    height: 9,
                    z: -7,
                },
            },
            Relay::Place {
                key: PlacementKey::Virtual {
                    image: 9,
                    placement: 2,
                },
                image: 9,
                layout: Layout {
                    columns: 4,
                    rows: 2,
                    ..Layout::default()
                },
            },
            Relay::Unplace(PlacementKey::Anchor(42)),
            Relay::Unplace(PlacementKey::Virtual {
                image: 9,
                placement: 2,
            }),
            Relay::DropImage(9),
            Relay::DropAll,
        ];
        let stream: Vec<u8> = relays.iter().flat_map(Relay::encode).collect();
        let mut parsed = Vec::new();
        GraphicsSplitter::default().feed(&stream, |piece| {
            if let Piece::Command(body) = piece {
                parsed.push(Relay::parse(body).expect("转发命令"));
            }
        });
        assert_eq!(parsed, relays);
    }

    #[test]
    fn program_commands_are_not_relays() {
        assert_eq!(Relay::parse(b"a=T,f=100,i=1;QUJD"), None);
    }

    #[test]
    fn anchor_marker_round_trips_and_strips() {
        for anchor in [0, 1, 0xdead_beef, u64::MAX] {
            let marker: Vec<char> = anchor_marker(anchor).chars().collect();
            assert_eq!(marker.len(), ANCHOR_DIGITS + 1);
            assert_eq!(anchors_in(&marker), [anchor]);
        }
        let mut zw = vec!['\u{FE0F}'];
        zw.extend(anchor_marker(3).chars());
        zw.push('\u{0301}');
        zw.extend(anchor_marker(5).chars());
        assert_eq!(anchors_in(&zw), [3, 5]);
        assert_eq!(&*without_anchors(&zw), ['\u{FE0F}', '\u{0301}']);
        assert!(matches!(without_anchors(&['\u{0301}']), Cow::Borrowed(_)));
    }

    fn term(columns: usize, lines: usize) -> Term<VoidListener> {
        Term::new(
            Config::default(),
            &TermSize::new(columns, lines),
            VoidListener,
        )
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, bytes);
    }

    fn place(term: &mut Term<VoidListener>, columns: u32, rows: u32, keep: bool, anchor: u64) {
        let grid = term.grid();
        let cursor = &grid.cursor;
        let at = PlacementCursor {
            column: cursor.point.column.0,
            needs_wrap: cursor.input_needs_wrap,
            lines_below: grid.screen_lines() - 1 - cursor.point.line.0 as usize,
            screen_columns: grid.columns(),
            screen_lines: grid.screen_lines(),
        };
        let bytes = placement_bytes(at, columns, rows, keep, Some(anchor));
        feed(term, &bytes);
    }

    fn cursor(term: &Term<VoidListener>) -> (i32, usize) {
        let point = term.grid().cursor.point;
        (point.line.0, point.column.0)
    }

    fn anchors_at(term: &Term<VoidListener>, line: i32, column: usize) -> Vec<u64> {
        let cell = &term.grid()[Point::new(Line(line), Column(column))];
        anchors_in(cell.zerowidth().unwrap_or_default())
    }

    #[test]
    fn placement_hangs_the_marker_on_the_cursor_cell_without_touching_it() {
        let mut t = term(10, 5);
        feed(&mut t, b"abcd\x1b[1;3H");
        place(&mut t, 3, 2, false, 77);
        assert_eq!(anchors_at(&t, 0, 2), [77]);
        assert_eq!(t.grid()[Point::new(Line(0), Column(2))].c, 'c');
        assert!(anchors_at(&t, 0, 3).is_empty());
        // 右移到图片右边一格，下移 行数 - 1。
        assert_eq!(cursor(&t), (1, 5));
    }

    #[test]
    fn placement_keeps_the_cursor_with_c1() {
        let mut t = term(10, 5);
        feed(&mut t, b"\x1b[2;4H");
        place(&mut t, 3, 2, true, 1);
        assert_eq!(anchors_at(&t, 1, 3), [1]);
        assert_eq!(cursor(&t), (1, 3));
    }

    #[test]
    fn placement_reaching_the_right_edge_wraps_to_the_next_line() {
        let mut t = term(10, 5);
        feed(&mut t, b"\x1b[1;9H");
        place(&mut t, 2, 2, false, 1);
        assert_eq!(cursor(&t), (2, 0));
    }

    #[test]
    fn placement_at_the_bottom_scrolls_and_the_marker_scrolls_with_it() {
        let mut t = term(10, 3);
        feed(&mut t, b"\x1b[3;1H");
        place(&mut t, 4, 3, false, 9);
        assert_eq!(cursor(&t), (2, 4));
        assert_eq!(anchors_at(&t, 0, 0), [9]);
    }

    #[test]
    fn placement_on_the_last_column() {
        let mut t = term(10, 5);
        // 刚写满最后一列：标记挂在这一格上，字不动。
        feed(&mut t, b"\x1b[1;10Hz");
        place(&mut t, 1, 1, true, 4);
        assert_eq!(anchors_at(&t, 0, 9), [4]);
        assert_eq!(t.grid()[Point::new(Line(0), Column(9))].c, 'z');
        // 光标停在最后一列但不待换行：只能写个空格再挂。
        let mut t = term(10, 5);
        feed(&mut t, b"\x1b[1;10H");
        place(&mut t, 1, 1, true, 5);
        assert_eq!(anchors_at(&t, 0, 9), [5]);
        assert_eq!(cursor(&t), (0, 9));
    }

    #[test]
    fn erasing_the_anchor_cell_removes_the_marker() {
        let mut t = term(10, 5);
        place(&mut t, 2, 2, true, 3);
        assert_eq!(anchors_at(&t, 0, 0), [3]);
        feed(&mut t, b"\x1b[2K");
        assert!(anchors_at(&t, 0, 0).is_empty());
    }

    #[test]
    fn graphics_traces_are_stripped_from_text() {
        let text = format!(
            "a{}b\u{10EEEE}\u{0305}\u{030D}\u{10EEEE}c\u{0301}",
            anchor_marker(7)
        );
        assert_eq!(without_graphics(&text), "abc\u{0301}");
        assert!(matches!(without_graphics("plain é"), Cow::Borrowed(_)));
    }

    #[test]
    fn placeholder_marks_and_inheritance() {
        let d = |i: usize| ROWCOLUMN_DIACRITICS[i];
        assert_eq!(
            placeholder_marks(&[d(2), d(5), d(1)]),
            PlaceholderMarks {
                row: Some(2),
                column: Some(5),
                image_id_high: Some(1),
            }
        );
        assert_eq!(placeholder_marks(&[]), PlaceholderMarks::default());

        let first = resolve_placeholder(0x00_0102, 7, placeholder_marks(&[d(3), d(0), d(9)]), None);
        assert_eq!(
            first,
            PlaceholderCell {
                image: 0x0900_0102,
                placement: 7,
                row: 3,
                column: 0,
            }
        );
        // 行列都省略：沿用左边的行和最高字节，列号加一。
        let next = resolve_placeholder(0x0102, 7, PlaceholderMarks::default(), Some(&first));
        assert_eq!((next.image, next.row, next.column), (0x0900_0102, 3, 1));
        // 只写了行、和左边同一行：列号加一。
        let same_row = resolve_placeholder(0x0102, 7, placeholder_marks(&[d(3)]), Some(&next));
        assert_eq!((same_row.row, same_row.column), (3, 2));
        // 左边是别的放置：不沿用。
        let other = resolve_placeholder(0x0102, 8, PlaceholderMarks::default(), Some(&next));
        assert_eq!((other.image, other.row, other.column), (0x0102, 0, 0));
    }

    #[test]
    fn diacritic_table_is_sorted() {
        assert!(
            ROWCOLUMN_DIACRITICS
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        assert_eq!(diacritic_index('\u{0305}'), Some(0));
        assert_eq!(diacritic_index('\u{1D244}'), Some(296));
        assert_eq!(diacritic_index('a'), None);
    }

    #[test]
    fn png_dimensions_come_from_ihdr() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), Some((640, 480)));
        assert_eq!(png_dimensions(b"GIF89a"), None);
    }

    #[test]
    fn direct_layout_follows_ghostty_pixel_size() {
        let controls = |columns, rows| Controls {
            columns,
            rows,
            ..Controls::default()
        };
        // 都没给：原始像素 35×30，格子 10×20 → 占 4×2 格，画 3.5×1.5 格。
        let layout = direct_layout(&controls(0, 0), (35, 30), (10, 20));
        assert_eq!((layout.columns, layout.rows), (4, 2));
        assert_eq!((layout.width, layout.height), (3500, 1500));
        // 都给了：撑满。
        let layout = direct_layout(&controls(4, 2), (35, 30), (10, 20));
        assert_eq!((layout.width, layout.height), (4000, 2000));
        // 只给列数：按比例。40px 宽 → 34px 高 → 1.7 格。
        let layout = direct_layout(&controls(4, 0), (35, 30), (10, 20));
        assert_eq!((layout.columns, layout.rows), (4, 2));
        assert_eq!((layout.width, layout.height), (4000, 1700));
        // 格子像素未知：撑满给定的格子。
        let layout = direct_layout(&controls(3, 2), (35, 30), (0, 0));
        assert_eq!((layout.width, layout.height), (3000, 2000));
    }

    #[test]
    fn virtual_layout_fits_and_centers() {
        let controls = Controls {
            columns: 10,
            rows: 2,
            virtual_placement: true,
            ..Controls::default()
        };
        // 40×40 像素在 10×20 的格子里是 4×2 格，放进 10×2 格：高度顶满，宽 4 格，左右各留 3 格。
        let layout = virtual_layout(&controls, (40, 40), (10, 20));
        assert_eq!((layout.width, layout.height), (4000, 2000));
        assert_eq!((layout.offset_x, layout.offset_y), (3000, 0));
    }

    #[test]
    fn grid_size_follows_ghostty() {
        // c、r 都给了。
        assert_eq!(grid_size(10, 4, (100, 100), (0, 0), (8, 16)), (10, 4));
        // 都没给：按原始像素，向上取整。
        assert_eq!(grid_size(0, 0, (100, 33), (0, 0), (8, 16)), (13, 3));
        // 只给列数：按比例算高。80px 宽 → 40px 高 → 3 行。
        assert_eq!(grid_size(10, 0, (200, 100), (0, 0), (8, 16)), (10, 3));
        // 只给行数：64px 高 → 128px 宽 → 16 列。
        assert_eq!(grid_size(0, 4, (200, 100), (0, 0), (8, 16)), (16, 4));
        // 格子像素未知。
        assert_eq!(grid_size(0, 0, (100, 100), (0, 0), (0, 0)), (0, 0));
        assert_eq!(
            source_size(
                SourceRect {
                    x: 90,
                    y: 0,
                    width: 50,
                    height: 0,
                },
                (100, 60)
            ),
            (10, 60)
        );
    }
}
