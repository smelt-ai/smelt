//! alacritty 不解析的 OSC，逐字节自己扫。
//!
//! - `OscScan`：OSC 9 / 99 / 777 通知（跟 cmux 同协议）、OSC 9;4 进度、OSC 7501 程序状态
//!   和 OSC 133 `A` 提示符起点。smeltd 在 PTY 输出泵里扫，结果走事件总线；客户端不扫，
//!   免得没连着时丢、多个客户端各弹一遍。
//! - `TerminalGeometryOscScan`：smeltd 发给渲染端的私有几何标记，只有客户端扫。

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    Esc,
    Osc,
    OscEsc,
    IgnoredString,
    IgnoredStringEsc,
}

/// OSC 9 / 99 / 777 / 7501 / 133 扫描：提取 `ESC ] … (BEL|ST)`，跨 `feed` 调用保持状态
/// （字节可能跨 PTY read 边界断开）。遵循 VT500 串状态：DCS / APC / PM / SOS 内部载荷
/// 不参与 OSC 匹配，CAN / SUB 或未闭合的下一个控制串会立即中止当前 OSC。
#[derive(Default)]
pub struct OscScan {
    state: ScanState,
    buf: Vec<u8>,
    overflow: bool,
}

/// smeltd -> desktop renderer geometry synchronization. A PTY has one real
/// grid, so every renderer must parse subsequent cursor-addressed output with
/// that same size. The sequence is private and ignored by normal terminals:
///
/// `OSC 5151;smelt-terminal-geometry;token;cols;rows;cell_w;cell_h;remote ST`
pub const TERMINAL_GEOMETRY_OSC: &str = "5151";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalGeometryOsc {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
    pub remote_controlled: bool,
}

pub fn terminal_geometry_osc(token: &str, geometry: TerminalGeometryOsc) -> Vec<u8> {
    format!(
        "\x1b]{TERMINAL_GEOMETRY_OSC};smelt-terminal-geometry;{token};{};{};{};{};{}\x1b\\",
        geometry.cols,
        geometry.rows,
        geometry.cell_width,
        geometry.cell_height,
        u8::from(geometry.remote_controlled),
    )
    .into_bytes()
}

/// Incremental scanner for [`terminal_geometry_osc`]. It intentionally does
/// not consume bytes; callers still pass the original stream to their VT
/// parser, which ignores this private OSC.
#[derive(Default)]
pub struct TerminalGeometryOscScan {
    expected_token: Option<String>,
    prev_esc: bool,
    in_osc: bool,
    buf: Vec<u8>,
}

impl TerminalGeometryOscScan {
    pub fn new(expected_token: String) -> Self {
        Self {
            expected_token: Some(expected_token),
            ..Self::default()
        }
    }

    pub fn feed(&mut self, byte: u8) -> Option<TerminalGeometryOsc> {
        if self.in_osc {
            if byte == 0x07 {
                return self.finish();
            }
            if self.prev_esc && byte == 0x5c {
                self.buf.pop();
                return self.finish();
            }
            self.buf.push(byte);
            self.prev_esc = byte == 0x1b;
            if self.buf.len() > 256 {
                self.reset();
            }
        } else if self.prev_esc && byte == 0x5d {
            self.in_osc = true;
            self.buf.clear();
            self.prev_esc = false;
        } else {
            self.prev_esc = byte == 0x1b;
        }
        None
    }

    fn finish(&mut self) -> Option<TerminalGeometryOsc> {
        let geometry = std::str::from_utf8(&self.buf)
            .ok()
            .and_then(|value| parse_terminal_geometry_osc(value, self.expected_token.as_deref()?));
        self.reset();
        geometry
    }

    fn reset(&mut self) {
        self.in_osc = false;
        self.prev_esc = false;
        self.buf.clear();
    }
}

fn parse_terminal_geometry_osc(value: &str, expected_token: &str) -> Option<TerminalGeometryOsc> {
    let mut fields = value.split(';');
    if fields.next()? != TERMINAL_GEOMETRY_OSC || fields.next()? != "smelt-terminal-geometry" {
        return None;
    }
    if fields.next()? != expected_token {
        return None;
    }
    let geometry = TerminalGeometryOsc {
        cols: fields.next()?.parse().ok()?,
        rows: fields.next()?.parse().ok()?,
        cell_width: fields.next()?.parse().ok()?,
        cell_height: fields.next()?.parse().ok()?,
        remote_controlled: match fields.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        },
    };
    if geometry.cols == 0
        || geometry.cols > 1000
        || geometry.rows == 0
        || geometry.rows > 1000
        || geometry.cell_width > 256
        || geometry.cell_height > 256
        || fields.next().is_some()
    {
        return None;
    }
    Some(geometry)
}

/// OSC 通知协议种类。消费者可保留协议细节，但不能由此推断 agent 状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OscNotificationKind {
    Osc9,
    Osc99,
    Osc777,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OscNotification {
    pub kind: OscNotificationKind,
    pub text: String,
}

/// OSC 序列的结束符。查询的回应要用和查询相同的那种。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OscTerminator {
    Bel,
    St,
}

impl OscTerminator {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Bel => b"\x07",
            Self::St => b"\x1b\\",
        }
    }
}

/// 扫出来的一条 OSC。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OscEvent {
    Notification(OscNotification),
    /// OSC 7501 根记录的状态报告。
    ProgramStatus(ProgramStatus),
    /// OSC 7501 特性探测 `OSC 7501 ; ? ST`，终端支持就要回应。
    ProgramStatusQuery(OscTerminator),
    /// OSC 133 `A`（shell 提示符开始）：程序已交回控制权给 shell，规范要求清掉未结束的
    /// `working` / `blocked`（以及 `idle`）记录。
    PromptStart,
    /// OSC 9;4 进度。
    Progress(ProgressReport),
}

/// OSC 9;4（ConEmu 进度，`OSC 9;4;状态[;数值] ST`），按 Ghostty 文档的语义：
/// <https://ghostty.org/docs/vt/osc/conemu>。数值 0..=100，超出的压到 100。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressReport {
    /// 状态 0：清掉进度条。
    Remove,
    /// 状态 1：进行中，没给数值按 0。
    Set(u8),
    /// 状态 2：出错。没给数值就沿用上一个。
    Error(Option<u8>),
    /// 状态 3：不确定进度。
    Indeterminate,
    /// 状态 4：暂停。没给数值就沿用上一个。
    Paused(Option<u8>),
}

/// 解析 `9;4;` 之后的部分。
fn parse_progress(pt: &str) -> Option<ProgressReport> {
    let mut fields = pt.split(';');
    let state = fields.next()?.trim();
    let value = match fields
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(value.parse::<u32>().ok()?.min(100) as u8),
        None => None,
    };
    Some(match state {
        "0" => ProgressReport::Remove,
        "1" => ProgressReport::Set(value.unwrap_or(0)),
        "2" => ProgressReport::Error(value),
        "3" => ProgressReport::Indeterminate,
        "4" => ProgressReport::Paused(value),
        _ => return None,
    })
}

/// OSC 7501（Program Status Protocol，规范 0.4）的状态。
/// <https://www.superlogical.com/rex/docs/build/program-status>
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

/// `blocked` 在等什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramBlockedKind {
    Permission,
    Question,
    Auth,
}

/// 一条 OSC 7501 报告。内容来自 PTY 里任意程序，只能拿来给人看，不能据此授权任何操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    pub kind: Option<ProgramBlockedKind>,
    /// `working` / `blocked` 附带的 0..=100 进度；未提供或不在运行/等待态时为 `None`。
    pub progress: Option<u8>,
    /// 已解 base64 并剥离不可见格式字符的说明文字。
    pub message: Option<String>,
}

/// 回应 OSC 7501 探测，表示支持规范里的全部状态和阻塞种类。
pub fn program_status_reply(terminator: OscTerminator) -> Vec<u8> {
    let mut reply = b"\x1b]7501;?".to_vec();
    reply.extend_from_slice(terminator.bytes());
    reply
}

const PROGRAM_STATUS_MAX_SEQUENCE_BYTES: usize = 4096;
const PROGRAM_STATUS_MAX_KEY_BYTES: usize = 16;
const PROGRAM_STATUS_MAX_APP_BYTES: usize = 32;
const PROGRAM_STATUS_TITLE_MAX_ENCODED: usize = 256;
const PROGRAM_STATUS_TITLE_MAX_DECODED: usize = 192;
/// 规范对 `msg` 编码后长度的上限（解码后 2048 字节）。
const PROGRAM_STATUS_MSG_MAX_ENCODED: usize = 2732;
const PROGRAM_STATUS_MSG_MAX_DECODED: usize = 2048;

fn is_disallowed_program_status_invisible(ch: char) -> bool {
    matches!(
        ch,
        '\u{061C}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// 校验并解码 OSC 7501 的 base64 文本（`msg` / `title`）。超限、非法 base64、非 UTF-8 或含
/// 控制字符时返回 `None`，调用方按规范丢弃整条报告。
fn decode_program_status_text(
    value: &str,
    max_encoded: usize,
    max_decoded: usize,
) -> Option<Option<String>> {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

    if value.len() > max_encoded {
        return None;
    }
    let bytes = STANDARD
        .decode(value.as_bytes())
        .or_else(|_| STANDARD_NO_PAD.decode(value.as_bytes()))
        .ok()?;
    if bytes.len() > max_decoded {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    // 规范：说明或标题里带控制字符的报告整条丢掉。
    if text.chars().any(char::is_control) {
        return None;
    }
    let clean: String = text
        .chars()
        .filter(|&ch| !is_disallowed_program_status_invisible(ch))
        .collect();
    let trimmed = clean.trim();
    Some((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

/// 解析 `7501;` 之后的部分。遵循规范 0.4：
/// - 畸形 pair（缺 `=`、非法 key 或值字符集）跳过，继续解析其余 pair；
/// - 整条序列、key/app/title/msg 超限、base64 非法、解码含控制字符或 `state` 不认识时整条丢弃；
/// - 只认根记录：带 `id`（含空值 `id=`）整条忽略，避免异常 id 回退覆盖根记录。
fn parse_program_status(pt: &str, whole_len: usize) -> Option<ProgramStatus> {
    if whole_len > PROGRAM_STATUS_MAX_SEQUENCE_BYTES {
        return None;
    }
    let mut state = None;
    let mut kind = None;
    let mut progress = None;
    let mut message = None;
    for pair in pt.split(':').filter(|pair| !pair.is_empty()) {
        let Some((raw_key, raw_value)) = pair.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        let value = raw_value.trim();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase()) {
            continue;
        }
        if key.len() > PROGRAM_STATUS_MAX_KEY_BYTES {
            return None;
        }
        if key == "id" {
            return None;
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b))
        {
            continue;
        }
        match key {
            "state" => {
                state = Some(match value {
                    "idle" => ProgramState::Idle,
                    "working" => ProgramState::Working,
                    "done" => ProgramState::Done,
                    "blocked" => ProgramState::Blocked,
                    "error" => ProgramState::Error,
                    "clear" => ProgramState::Clear,
                    _ => return None,
                });
            }
            "kind" => {
                kind = match value {
                    "permission" => Some(ProgramBlockedKind::Permission),
                    "question" => Some(ProgramBlockedKind::Question),
                    "auth" => Some(ProgramBlockedKind::Auth),
                    _ => None,
                };
            }
            "progress" => {
                progress = value.parse::<u8>().ok().filter(|&v| v <= 100);
            }
            "app" => {
                if value.len() > PROGRAM_STATUS_MAX_APP_BYTES {
                    return None;
                }
            }
            "title" if !value.is_empty() => {
                let _ = decode_program_status_text(
                    value,
                    PROGRAM_STATUS_TITLE_MAX_ENCODED,
                    PROGRAM_STATUS_TITLE_MAX_DECODED,
                )?;
            }
            "msg" => {
                if value.is_empty() {
                    message = None;
                } else {
                    message = decode_program_status_text(
                        value,
                        PROGRAM_STATUS_MSG_MAX_ENCODED,
                        PROGRAM_STATUS_MSG_MAX_DECODED,
                    )?;
                }
            }
            _ => {}
        }
    }
    let state = state?;
    Some(ProgramStatus {
        state,
        kind: kind.filter(|_| state == ProgramState::Blocked),
        progress: progress
            .filter(|_| matches!(state, ProgramState::Working | ProgramState::Blocked)),
        message,
    })
}

impl OscScan {
    /// 喂一个字节；扫到一条完整的、认得的 OSC 就返回它。
    pub fn feed(&mut self, b: u8) -> Option<OscEvent> {
        match self.state {
            ScanState::Ground => {
                if b == 0x1b {
                    self.state = ScanState::Esc;
                }
            }
            ScanState::Esc => match b {
                b']' => {
                    self.state = ScanState::Osc;
                    self.buf.clear();
                    self.overflow = false;
                }
                b'P' | b'_' | b'^' | b'X' => {
                    self.state = ScanState::IgnoredString;
                }
                0x1b => {}
                _ => self.state = ScanState::Ground,
            },
            ScanState::Osc => match b {
                0x07 => return self.complete(OscTerminator::Bel),
                0x18 | 0x1a => self.reset(),
                0x1b => self.state = ScanState::OscEsc,
                _ => {
                    if !self.overflow {
                        if self.buf.len() >= PROGRAM_STATUS_MAX_SEQUENCE_BYTES {
                            self.overflow = true;
                            self.buf.clear();
                        } else {
                            self.buf.push(b);
                        }
                    }
                }
            },
            ScanState::OscEsc => match b {
                b'\\' => return self.complete(OscTerminator::St),
                b']' => {
                    self.state = ScanState::Osc;
                    self.buf.clear();
                    self.overflow = false;
                }
                b'P' | b'_' | b'^' | b'X' => {
                    self.buf.clear();
                    self.overflow = false;
                    self.state = ScanState::IgnoredString;
                }
                0x1b => {
                    self.buf.clear();
                    self.overflow = false;
                    self.state = ScanState::Esc;
                }
                _ => self.reset(),
            },
            ScanState::IgnoredString => match b {
                0x18 | 0x1a => self.state = ScanState::Ground,
                0x1b => self.state = ScanState::IgnoredStringEsc,
                _ => {}
            },
            ScanState::IgnoredStringEsc => match b {
                b'\\' | 0x18 | 0x1a => self.state = ScanState::Ground,
                b']' => {
                    self.state = ScanState::Osc;
                    self.buf.clear();
                    self.overflow = false;
                }
                b'P' | b'_' | b'^' | b'X' => self.state = ScanState::IgnoredString,
                0x1b => self.state = ScanState::Esc,
                _ => self.state = ScanState::Ground,
            },
        }
        None
    }

    fn complete(&mut self, terminator: OscTerminator) -> Option<OscEvent> {
        let event = (!self.overflow)
            .then(|| self.parse_finish(terminator))
            .flatten();
        self.reset();
        event
    }

    fn parse_finish(&self, terminator: OscTerminator) -> Option<OscEvent> {
        let s = std::str::from_utf8(&self.buf).ok()?;
        let (ps, pt) = s.split_once(';')?;
        if ps == "9"
            && let Some(progress) = pt.strip_prefix("4;")
        {
            return parse_progress(progress).map(OscEvent::Progress);
        }
        let kind = match ps {
            "9" => OscNotificationKind::Osc9,
            "99" => OscNotificationKind::Osc99,
            "777" => OscNotificationKind::Osc777,
            "133" => {
                return (pt.split(';').next().map(str::trim) == Some("A"))
                    .then_some(OscEvent::PromptStart);
            }
            "7501" if pt.trim() == "?" => {
                return Some(OscEvent::ProgramStatusQuery(terminator));
            }
            "7501" => {
                let whole_len = 2 + self.buf.len() + terminator.bytes().len();
                return parse_program_status(pt, whole_len).map(OscEvent::ProgramStatus);
            }
            _ => return None,
        };
        notify_text_from_osc(ps, pt)
            .map(|text| OscEvent::Notification(OscNotification { kind, text }))
    }

    pub fn reset(&mut self) {
        self.state = ScanState::Ground;
        self.overflow = false;
        self.buf.clear();
    }
}

/// 从 OSC 参数字符串抽出给人看的通知正文。
///
/// - **9**：`9;消息`
/// - **777**：`777;notify;title;body` → 取最后一段
/// - **99**（Kitty）：`99;metadata;payload`；`e=1` 时 payload 为 base64
pub fn notify_text_from_osc(ps: &str, pt: &str) -> Option<String> {
    match ps {
        "9" => {
            let msg = pt.trim();
            // ConEmu/Copilot 等也复用 OSC 9 做任务栏进度，例如
            // `OSC 9;4;3;0 ST`。首段为数字且后面还有参数时是控制命令，不能把
            // `4;3;0` 当成人类通知正文。
            let is_control_command = msg
                .split_once(';')
                .is_some_and(|(command, _)| command.bytes().all(|b| b.is_ascii_digit()));
            if msg.is_empty() || is_control_command {
                None
            } else {
                Some(msg.to_string())
            }
        }
        "777" => {
            let msg = pt.rsplit(';').next().unwrap_or(pt).trim();
            if msg.is_empty() {
                None
            } else {
                Some(msg.to_string())
            }
        }
        "99" => parse_osc99_payload(pt),
        _ => None,
    }
}

/// Kitty desktop notification（OSC 99）：`metadata;payload`。
fn parse_osc99_payload(pt: &str) -> Option<String> {
    let pt = pt.trim();
    if pt.is_empty() {
        return None;
    }

    // 首段含 `=` → 当 Kitty metadata；否则整段纯文本（部分工具简化写法）。
    let (meta, payload) = match pt.split_once(';') {
        Some((m, p)) if m.contains('=') => (Some(m), p),
        _ => (None, pt),
    };

    let mut b64 = false;
    if let Some(m) = meta {
        for kv in m.split(':') {
            if let Some((k, v)) = kv.split_once('=')
                && k == "e"
                && v == "1"
            {
                b64 = true;
            }
        }
    }

    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }

    if b64 {
        let decoded = decode_base64_utf8(payload)?;
        let s = decoded.trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    } else {
        Some(payload.to_string())
    }
}

/// 标准 base64（支持标准 `=` 填充、未填充及空白过滤）→ UTF-8 字符串。
fn decode_base64_utf8(s: &str) -> Option<String> {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

    let clean: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if clean.is_empty() {
        return None;
    }
    let bytes = STANDARD
        .decode(clean.as_bytes())
        .or_else(|_| STANDARD_NO_PAD.decode(clean.as_bytes()))
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_events(bytes: &[u8]) -> Vec<OscEvent> {
        let mut scan = OscScan::default();
        bytes.iter().filter_map(|&b| scan.feed(b)).collect()
    }

    fn scan_all(bytes: &[u8]) -> Option<String> {
        scan_events(bytes)
            .into_iter()
            .filter_map(|event| match event {
                OscEvent::Notification(notification) => Some(notification.text),
                _ => None,
            })
            .next_back()
    }

    fn status(bytes: &[u8]) -> Option<ProgramStatus> {
        match scan_events(bytes).pop()? {
            OscEvent::ProgramStatus(status) => Some(status),
            _ => None,
        }
    }

    #[test]
    fn scans_osc9_terminated_by_bel() {
        assert_eq!(
            scan_all(b"\x1b]9;hello world\x07").as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn terminal_geometry_round_trips_across_read_boundaries() {
        let expected = TerminalGeometryOsc {
            cols: 49,
            rows: 47,
            cell_width: 8,
            cell_height: 15,
            remote_controlled: true,
        };
        let bytes = terminal_geometry_osc("session-secret", expected);
        let mut scan = TerminalGeometryOscScan::new("session-secret".to_string());
        let mut actual = None;
        for chunk in bytes.chunks(3) {
            for &byte in chunk {
                actual = scan.feed(byte).or(actual);
            }
        }
        assert_eq!(actual, Some(expected));
    }

    #[test]
    fn terminal_geometry_scanner_ignores_other_osc_and_invalid_sizes() {
        let mut scan = TerminalGeometryOscScan::new("session-secret".to_string());
        let mut events = Vec::new();
        for &byte in b"\x1b]9;done\x07\x1b]5151;smelt-terminal-geometry;wrong;24;80;8;15;1\x1b\\\x1b]5151;smelt-terminal-geometry;session-secret;0;24;8;15;1\x1b\\" {
            if let Some(event) = scan.feed(byte) {
                events.push(event);
            }
        }
        assert!(events.is_empty());
    }

    #[test]
    fn ignores_osc9_progress_control_commands() {
        assert_eq!(scan_all(b"\x1b]9;4;3;0\x1b\\"), None);
        assert_eq!(notify_text_from_osc("9", "4;3;0"), None);
    }

    #[test]
    fn typed_scan_preserves_notification_protocol() {
        let events = scan_events(b"\x1b]9;done\x07\x1b]99;plain note\x1b\\\x1b]777;notify;t;b\x07");
        assert_eq!(
            events
                .iter()
                .map(|event| match event {
                    OscEvent::Notification(notification) => notification.kind,
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>(),
            vec![
                OscNotificationKind::Osc9,
                OscNotificationKind::Osc99,
                OscNotificationKind::Osc777,
            ]
        );
    }

    #[test]
    fn scans_osc777_terminated_by_st_and_takes_last_segment() {
        assert_eq!(
            scan_all(b"\x1b]777;notify;title;body text\x1b\\").as_deref(),
            Some("body text")
        );
    }

    #[test]
    fn scans_osc99_kitty_metadata_payload() {
        assert_eq!(
            notify_text_from_osc("99", "i=1:d=0:p=body;Waiting for approval").as_deref(),
            Some("Waiting for approval")
        );
        assert_eq!(
            scan_all(b"\x1b]99;i=1:p=body;Agent needs input\x1b\\").as_deref(),
            Some("Agent needs input")
        );
    }

    #[test]
    fn osc99_plain_and_base64() {
        assert_eq!(
            notify_text_from_osc("99", "just a plain note").as_deref(),
            Some("just a plain note")
        );
        let msg = notify_text_from_osc("99", "e=1:p=body;SGkg5om5").unwrap();
        assert_eq!(msg, "Hi 批");
    }

    #[test]
    fn ignores_unrelated_osc_codes() {
        assert_eq!(scan_all(b"\x1b]0;window title\x07"), None);
        assert!(notify_text_from_osc("52", "c;xxx").is_none());
    }

    #[test]
    fn state_persists_across_feed_calls_split_at_arbitrary_boundary() {
        let full = b"\x1b]9;split across reads\x07";
        let mut scan = OscScan::default();
        let mut got = None;
        for chunk in full.chunks(3) {
            for &b in chunk {
                if let Some(OscEvent::Notification(m)) = scan.feed(b) {
                    got = Some(m.text);
                }
            }
        }
        assert_eq!(got.as_deref(), Some("split across reads"));
    }

    #[test]
    fn oversized_buffer_resets_instead_of_growing_forever() {
        let mut scan = OscScan::default();
        scan.feed(0x1b);
        scan.feed(b']');
        for _ in 0..5000 {
            scan.feed(b'x');
        }
        let mut got = None;
        for &b in b"\x1b]9;still works\x07" {
            if let Some(OscEvent::Notification(m)) = scan.feed(b) {
                got = Some(m.text);
            }
        }
        assert_eq!(got.as_deref(), Some("still works"));
    }

    #[test]
    fn program_status_query_is_answered_with_the_same_terminator() {
        assert_eq!(
            scan_events(b"\x1b]7501;?\x1b\\"),
            vec![OscEvent::ProgramStatusQuery(OscTerminator::St)]
        );
        assert_eq!(
            scan_events(b"\x1b]7501;?\x07"),
            vec![OscEvent::ProgramStatusQuery(OscTerminator::Bel)]
        );
        assert_eq!(
            program_status_reply(OscTerminator::St),
            b"\x1b]7501;?\x1b\\"
        );
        assert_eq!(program_status_reply(OscTerminator::Bel), b"\x1b]7501;?\x07");
    }

    #[test]
    fn program_status_reads_state_kind_progress_and_base64_message() {
        // "批准 rm -rf" 的 base64，不带填充。
        let report = concat!(
            "\x1b]7501;state=blocked:kind=permission:app=codex:progress=45:",
            "msg=5om55YeGIHJtIC1yZg\x1b\\",
        );
        let blocked = status(report.as_bytes()).unwrap();
        assert_eq!(blocked.state, ProgramState::Blocked);
        assert_eq!(blocked.kind, Some(ProgramBlockedKind::Permission));
        assert_eq!(blocked.progress, Some(45));
        assert_eq!(blocked.message.as_deref(), Some("批准 rm -rf"));

        let working = status(b"\x1b]7501;state=working:progress=100\x07").unwrap();
        assert_eq!(working.state, ProgramState::Working);
        assert_eq!(working.progress, Some(100));

        let done = status(b"\x1b]7501;state=done:kind=question:progress=50\x07").unwrap();
        assert_eq!(done.state, ProgramState::Done);
        assert_eq!(done.kind, None, "kind 只对 blocked 有意义");
        assert_eq!(done.progress, None, "progress 只在 working/blocked 有效");
        assert_eq!(done.message, None);

        let out_of_range = status(b"\x1b]7501;state=working:progress=101\x07").unwrap();
        assert_eq!(out_of_range.progress, None);
    }

    #[test]
    fn invalid_program_status_reports_are_dropped_whole() {
        assert_eq!(
            status(b"\x1b]7501;state=sleeping\x07"),
            None,
            "不认识的状态"
        );
        assert_eq!(status(b"\x1b]7501;kind=permission\x07"), None, "缺 state");
        assert_eq!(
            status(b"\x1b]7501;state=done:msg=a\x07"),
            None,
            "非法 base64 整条丢弃"
        );
        // "a\nb"：说明里带控制字符。
        assert_eq!(status(b"\x1b]7501;state=done:msg=YQpi\x07"), None);
        // title 带控制字符同样整条丢弃。
        assert_eq!(status(b"\x1b]7501;state=done:title=YQpi\x07"), None);
        assert_eq!(
            status(b"\x1b]7501;state=working:id=build\x07"),
            None,
            "子记录不当成根状态"
        );
        assert_eq!(
            status(b"\x1b]7501;state=working:id=\x07"),
            None,
            "空 id 也不回退覆盖根记录"
        );
        assert_eq!(
            status(b"\x1b]7501;state=working:id=bad!id\x07"),
            None,
            "畸形 id 不得回退覆盖根记录"
        );
        assert_eq!(
            status(&format!("\x1b]7501;state=done:{}=1\x07", "k".repeat(17)).into_bytes()),
            None,
            "key 超 16 字节整条丢弃"
        );
        assert_eq!(
            status(&format!("\x1b]7501;state=done:app={}\x07", "a".repeat(33)).into_bytes()),
            None,
            "app 超 32 字节整条丢弃"
        );
        let oversized = format!("\x1b]7501;state=done:msg={}\x07", "A".repeat(2800));
        assert_eq!(status(oversized.as_bytes()), None);
        let seq_4097 = format!("\x1b]7501;state=done:future={}\x1b\\", "A".repeat(4070));
        assert_eq!(seq_4097.len(), 4097);
        assert_eq!(status(seq_4097.as_bytes()), None);
    }

    #[test]
    fn program_status_skips_malformed_pairs_strips_bidi_and_handles_repeat_keys() {
        // 缺 `=`、值含空格、大写 key 都只跳过该 pair；重复 key 后值生效；key/value 两端去空白。
        let got =
            status(b"\x1b]7501;badpair: state = idle :BAD=1:msg=a b:state = done\x07").unwrap();
        assert_eq!(got.state, ProgramState::Done);
        assert_eq!(got.message, None);

        // 7501 探测必须是精确 `?`（忽略两端空白）。
        assert_eq!(scan_events(b"\x1b]7501;?garbage\x1b\\"), vec![]);
        assert_eq!(
            scan_events(b"\x1b]7501; ? \x07"),
            vec![OscEvent::ProgramStatusQuery(OscTerminator::Bel)]
        );

        // Bidi 覆盖与不可见伪装字符被剥离，ZWJ 组合 emoji 保留。
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD_NO_PAD
            .encode("run\u{202E}txt \u{1F469}\u{200D}\u{1F4BB}\u{200B}");
        let seq = format!("\x1b]7501;state=working:msg={encoded}\x07");
        let got = status(seq.as_bytes()).unwrap();
        assert_eq!(
            got.message.as_deref(),
            Some("runtxt \u{1F469}\u{200D}\u{1F4BB}")
        );
    }

    #[test]
    fn osc_scan_handles_vt_strings_abort_and_osc133_prompt_start() {
        // CAN / SUB 立即中止未闭合 OSC，随后新 OSC 照常解析。
        assert_eq!(
            scan_events(b"\x1b]7501;state=working\x18\x1b]7501;state=done\x07").len(),
            1
        );
        assert_eq!(
            scan_events(b"\x1b]9;aborted\x1a\x1b]9;real\x07")
                .into_iter()
                .map(|e| match e {
                    OscEvent::Notification(n) => n.text,
                    other => panic!("unexpected {other:?}"),
                })
                .collect::<Vec<_>>(),
            vec!["real"]
        );

        // 未闭合的 OSC 被 DCS / APC（`ESC P` / `ESC _`）中止，且其中的 `BEL` 与普通字节不会触发。
        assert_eq!(
            scan_events(
                b"\x1b]7501;state=working\x1bP123]7501;state=done\x07\x1b\\\x1b]9;aborted\x1b_Gabc]9;fake\x07\x1b\\"
            ),
            vec![]
        );

        // 未闭合 OSC 紧跟新 `ESC ]` 时丢弃前一条、解析新一条。
        let got = status(b"\x1b]9;unclosed\x1b]7501;state=done\x07").unwrap();
        assert_eq!(got.state, ProgramState::Done);

        // OSC 133 A（含参数）触发 PromptStart，B / C / D 不触发。
        assert_eq!(
            scan_events(
                b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A;k=i\x1b\\"
            ),
            vec![OscEvent::PromptStart, OscEvent::PromptStart]
        );
    }

    #[test]
    fn program_status_ignores_unknown_keys_and_empty_values() {
        let status = status(b"\x1b]7501;state=working:progress=40:future=1:msg=\x07").unwrap();
        assert_eq!(status.state, ProgramState::Working);
        assert_eq!(status.progress, Some(40));
        assert_eq!(status.message, None);
    }

    #[test]
    fn conemu_progress_reports_are_parsed_not_shown_as_notifications() {
        assert_eq!(
            scan_events(b"\x1b]9;4;1;42\x1b\\"),
            vec![OscEvent::Progress(ProgressReport::Set(42))]
        );
        assert_eq!(
            scan_events(b"\x1b]9;4;0\x07\x1b]9;4;3\x07\x1b]9;4;2\x07\x1b]9;4;4;250\x07"),
            vec![
                OscEvent::Progress(ProgressReport::Remove),
                OscEvent::Progress(ProgressReport::Indeterminate),
                OscEvent::Progress(ProgressReport::Error(None)),
                OscEvent::Progress(ProgressReport::Paused(Some(100))),
            ]
        );
        assert_eq!(scan_events(b"\x1b]9;4;7;10\x07"), vec![], "不认识的状态");
        assert_eq!(scan_events(b"\x1b]9;4;1;abc\x07"), vec![], "数值不是数字");
        assert_eq!(scan_all(b"\x1b]9;4;1;42\x07"), None, "进度不是通知");
    }
}
