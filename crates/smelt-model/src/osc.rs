//! alacritty 不解析的 OSC，逐字节自己扫。
//!
//! - `OscScan`：OSC 9 / 99 / 777 通知（跟 cmux 同协议）、OSC 9;4 进度和 OSC 7501 程序状态。smeltd 在
//!   PTY 输出泵里扫，结果走事件总线；客户端不扫，免得没连着时丢、多个客户端各弹一遍。
//! - `TerminalGeometryOscScan`：smeltd 发给渲染端的私有几何标记，只有客户端扫。

/// OSC 9 / 99 / 777 / 7501 扫描：提取 `ESC ] … (BEL|ST)`，跨 `feed` 调用保持状态
/// （字节可能跨 PTY read 边界断开）。
#[derive(Default)]
pub struct OscScan {
    prev_esc: bool,
    in_osc: bool,
    buf: Vec<u8>,
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
    /// 程序自报的机器名，如 `codex`、`pi`。
    pub app: Option<String>,
    /// 已解 base64 的说明文字。
    pub message: Option<String>,
}

/// 回应 OSC 7501 探测，表示支持规范里的全部状态和阻塞种类。
pub fn program_status_reply(terminator: OscTerminator) -> Vec<u8> {
    let mut reply = b"\x1b]7501;?".to_vec();
    reply.extend_from_slice(terminator.bytes());
    reply
}

/// 规范对 `msg` 编码后长度的上限（解码后 2048 字节）。
const PROGRAM_STATUS_MSG_MAX_ENCODED: usize = 2732;

/// 解析 `7501;` 之后的部分。不合规范的整条丢掉（规范要求终端这样做）。只认根记录：
/// 带 `id` 的子记录是程序内部的分项进度，这里用不上。
fn parse_program_status(pt: &str) -> Option<ProgramStatus> {
    let mut state = None;
    let mut kind = None;
    let mut app = None;
    let mut message = None;
    for pair in pt.split(':').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=')?;
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b))
        {
            return None;
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
            "app" if (1..=32).contains(&value.len()) && !value.contains([',', '/', '=']) => {
                app = Some(value.to_string());
            }
            "msg" if !value.is_empty() => {
                if value.len() > PROGRAM_STATUS_MSG_MAX_ENCODED {
                    return None;
                }
                let text = decode_base64_utf8(value)?;
                // 规范：说明里带控制字符的报告整条丢掉。
                if text.chars().any(char::is_control) {
                    return None;
                }
                message = Some(text.trim().to_string()).filter(|text| !text.is_empty());
            }
            "id" if !value.is_empty() => return None,
            _ => {}
        }
    }
    let state = state?;
    Some(ProgramStatus {
        state,
        kind: kind.filter(|_| state == ProgramState::Blocked),
        app,
        message,
    })
}

impl OscScan {
    /// 喂一个字节；扫到一条完整的、认得的 OSC 就返回它。
    pub fn feed(&mut self, b: u8) -> Option<OscEvent> {
        if self.in_osc {
            if b == 0x07 {
                return self.finish(OscTerminator::Bel);
            }
            if self.prev_esc && b == 0x5c {
                self.buf.pop(); // 去掉刚推入的 ESC，ST（ESC \）结束
                return self.finish(OscTerminator::St);
            }
            self.buf.push(b);
            self.prev_esc = b == 0x1b;
            if self.buf.len() > 4096 {
                self.reset(); // 异常超长，丢弃
            }
        } else if self.prev_esc && b == 0x5d {
            self.in_osc = true; // ESC ] 进入 OSC
            self.buf.clear();
            self.prev_esc = false;
        } else {
            self.prev_esc = b == 0x1b;
        }
        None
    }

    fn finish(&mut self, terminator: OscTerminator) -> Option<OscEvent> {
        let event = std::str::from_utf8(&self.buf).ok().and_then(|s| {
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
                "7501" if pt.starts_with('?') => {
                    return Some(OscEvent::ProgramStatusQuery(terminator));
                }
                "7501" => return parse_program_status(pt).map(OscEvent::ProgramStatus),
                _ => return None,
            };
            notify_text_from_osc(ps, pt)
                .map(|text| OscEvent::Notification(OscNotification { kind, text }))
        });
        self.reset();
        event
    }

    fn reset(&mut self) {
        self.in_osc = false;
        self.prev_esc = false;
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
    fn program_status_reads_state_kind_app_and_base64_message() {
        // "批准 rm -rf" 的 base64，不带填充。
        let report = concat!(
            "\x1b]7501;state=blocked:kind=permission:app=codex:",
            "msg=5om55YeGIHJtIC1yZg\x1b\\",
        );
        let blocked = status(report.as_bytes()).unwrap();
        assert_eq!(blocked.state, ProgramState::Blocked);
        assert_eq!(blocked.kind, Some(ProgramBlockedKind::Permission));
        assert_eq!(blocked.app.as_deref(), Some("codex"));
        assert_eq!(blocked.message.as_deref(), Some("批准 rm -rf"));

        let done = status(b"\x1b]7501;state=done:kind=question\x07").unwrap();
        assert_eq!(done.state, ProgramState::Done);
        assert_eq!(done.kind, None, "kind 只对 blocked 有意义");
        assert_eq!(done.message, None);
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
            status(b"\x1b]7501;state=done:msg=a b\x07"),
            None,
            "值里有空格"
        );
        // "a\nb"：说明里带控制字符。
        assert_eq!(status(b"\x1b]7501;state=done:msg=YQpi\x07"), None);
        assert_eq!(
            status(b"\x1b]7501;state=working:id=build\x07"),
            None,
            "子记录不当成根状态"
        );
        let oversized = format!("\x1b]7501;state=done:msg={}\x07", "A".repeat(2800));
        assert_eq!(status(oversized.as_bytes()), None);
    }

    #[test]
    fn program_status_ignores_unknown_keys_and_empty_values() {
        let status = status(b"\x1b]7501;state=working:progress=40:future=1:msg=\x07").unwrap();
        assert_eq!(status.state, ProgramState::Working);
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
