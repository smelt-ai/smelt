//! OSC 通知扫描 + 标题 spinner 判定——GUI（`workspace/terminal.rs`）和守护
//! （`smeltd.rs` 的 `StateListener`）共用一份，跨 bin 用 `#[path]` 引入（跟
//! `remote_gateway.rs` 同一个套路），不复制第二份（CLAUDE.md 明令）。
//!
//! 两个信源都在这：
//! - `OscScan`：OSC 9 / 777 通知（alacritty 不解析，逐字节自己扫，跟 cmux 同协议）
//! - `title_starts_with_spinner`：OSC 0/2 标题的 Braille spinner 前缀猜测（可信度
//!   最低，纯猜——见 docs/state-channel-plan.md 的信源分层）

/// OSC 9 / 777 通知扫描：提取 `ESC ] 9 ; 消息 (BEL|ST)`，跨 `feed` 调用保持状态
/// （字节可能跨 PTY read 边界断开）。
#[derive(Default)]
pub struct OscScan {
    prev_esc: bool,
    in_osc: bool,
    buf: Vec<u8>,
}

impl OscScan {
    /// 喂一个字节；扫到一条完整的 OSC 9/777 通知就返回 `Some(消息文本)`，
    /// 调用方自己决定拿这条消息去做什么（GUI 弹通知 / 守护写进 SessionState）。
    pub fn feed(&mut self, b: u8) -> Option<String> {
        if self.in_osc {
            if b == 0x07 {
                return self.finish(); // BEL 结束
            }
            if self.prev_esc && b == 0x5c {
                self.buf.pop(); // 去掉刚推入的 ESC，ST（ESC \）结束
                return self.finish();
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

    fn finish(&mut self) -> Option<String> {
        let msg = std::str::from_utf8(&self.buf).ok().and_then(|s| {
            let (ps, pt) = s.split_once(';')?;
            if ps != "9" && ps != "777" {
                return None;
            }
            // OSC 777 常见格式 `777;notify;title;body`，取最后一段作正文。
            let msg = pt.rsplit(';').next().unwrap_or(pt).trim().to_string();
            (!msg.is_empty()).then_some(msg)
        });
        self.reset();
        msg
    }

    fn reset(&mut self) {
        self.in_osc = false;
        self.prev_esc = false;
        self.buf.clear();
    }
}

/// 标题是否以 Braille spinner（U+2801–U+28FF，盲文块非空白帧）开头——终端协议约定，
/// 任何遵守此约定的 agent（Claude Code 等）都能被识别，不是某家私有格式。
pub fn title_starts_with_spinner(title: &str) -> bool {
    title.chars().next().is_some_and(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_osc9_terminated_by_bel() {
        let mut scan = OscScan::default();
        let mut got = None;
        for &b in b"\x1b]9;hello world\x07" {
            if let Some(m) = scan.feed(b) {
                got = Some(m);
            }
        }
        assert_eq!(got.as_deref(), Some("hello world"));
    }

    #[test]
    fn scans_osc777_terminated_by_st_and_takes_last_segment() {
        let mut scan = OscScan::default();
        let mut got = None;
        for &b in b"\x1b]777;notify;title;body text\x1b\\" {
            if let Some(m) = scan.feed(b) {
                got = Some(m);
            }
        }
        assert_eq!(got.as_deref(), Some("body text"));
    }

    #[test]
    fn ignores_unrelated_osc_codes() {
        let mut scan = OscScan::default();
        let mut got = None;
        for &b in b"\x1b]0;window title\x07" {
            if let Some(m) = scan.feed(b) {
                got = Some(m);
            }
        }
        assert_eq!(got, None);
    }

    #[test]
    fn state_persists_across_feed_calls_split_at_arbitrary_boundary() {
        let mut scan = OscScan::default();
        let full = b"\x1b]9;split across reads\x07";
        let mut got = None;
        for chunk in full.chunks(3) {
            for &b in chunk {
                if let Some(m) = scan.feed(b) {
                    got = Some(m);
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
        // 缓冲区该已经被 reset 过；喂一个正常的短通知应该还能正确扫到。
        let mut got = None;
        for &b in b"\x1b]9;still works\x07" {
            if let Some(m) = scan.feed(b) {
                got = Some(m);
            }
        }
        assert_eq!(got.as_deref(), Some("still works"));
    }

    #[test]
    fn title_starts_with_spinner_matches_braille_range() {
        assert!(title_starts_with_spinner("⠋ doing something"));
        assert!(!title_starts_with_spinner("plain title"));
        assert!(!title_starts_with_spinner(""));
    }
}
