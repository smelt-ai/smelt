//! 交互连接的流控：跟不上的客户端不再被摘掉，追上来时用关键帧补齐画面和历史。

use super::watch_tests::make_dummy_session;
use super::*;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::index::Column;
use std::io::{ErrorKind, Read};
use std::time::Duration;

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// PTY 泵做的事：推进守护的终端，再分发给各连接。
fn pump(sess: &Session, parser: &mut Processor, bytes: &[u8]) {
    let _output_gate = sess.output_gate.lock().unwrap();
    parser.advance(&mut *sess.term.lock().unwrap(), bytes);
    dispatch_session_outputs_under_gate(sess, bytes, "t", true, true);
}

/// 读出 socket 里眼下所有的字节。
fn read_available(stream: &mut UnixStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => panic!("连接被守护断开了"),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return out;
            }
            Err(error) => panic!("读失败：{error}"),
        }
    }
}

fn text_lines(term: &Term<VoidListener>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = term.topmost_line();
    while line <= term.bottommost_line() {
        let row = &term.grid()[line];
        let text: String = (0..term.columns()).map(|col| row[Column(col)].c).collect();
        lines.push(text.trim_end().to_string());
        line += 1;
    }
    lines
}

/// 模拟桌面客户端：解析收到的字节，按累计字节数回确认。
struct Client {
    stream: UnixStream,
    term: Term<VoidListener>,
    parser: Processor,
    consumed: u64,
    header_skipped: bool,
}

impl Client {
    fn read_and_acknowledge(&mut self, sess: &Session, fd: RawFd) -> usize {
        let mut bytes = read_available(&mut self.stream);
        if !self.header_skipped {
            let end = bytes.iter().position(|&b| b == b'\n').unwrap() + 1;
            bytes.drain(..end);
            self.header_skipped = true;
        }
        self.parser.advance(&mut self.term, &bytes);
        self.consumed += bytes.len() as u64;
        acknowledge_terminal_output(sess, fd, self.consumed);
        bytes.len()
    }
}

#[test]
fn a_client_that_falls_behind_is_coalesced_instead_of_detached() {
    let sess = make_dummy_session(ROWS, COLS);
    let (server, stream) = UnixStream::pair().unwrap();
    let keyframe = {
        let _output_gate = sess.output_gate.lock().unwrap();
        terminal_keyframe(&sess, KeyframeNeed::Full)
    };
    let attachment =
        OutputAttachment::interactive(server, b"{}\n".to_vec(), keyframe, "t").unwrap();
    let fd = attachment.fd;
    sess.out.lock().unwrap().clients.push(attachment);
    let mut client = Client {
        stream,
        term: new_daemon_term(ROWS, COLS, VoidListener),
        parser: Processor::new(),
        consumed: 0,
        header_skipped: false,
    };

    // 客户端一个字节都不读，守护这边刷出约 3MB，远超在途窗口。
    let mut parser = Processor::new();
    let total = 200_000;
    for batch in (0..total).step_by(100) {
        let chunk: String = (batch..batch + 100)
            .map(|n| format!("line {n}\r\n"))
            .collect();
        pump(&sess, &mut parser, chunk.as_bytes());
    }
    assert_eq!(
        sess.out.lock().unwrap().clients.len(),
        1,
        "跟不上的客户端不能被摘掉"
    );

    let first = client.read_and_acknowledge(&sess, fd);
    assert!(
        first < 1024 * 1024,
        "在途输出必须受窗口限制，实际 {first} 字节"
    );

    // 追上来了：守护补一帧当前屏幕，客户端直接看到最新的输出。
    client.read_and_acknowledge(&sess, fd);
    let last = format!("line {}", total - 1);
    assert!(
        text_lines(&client.term).contains(&last),
        "补帧后应当看到最新一行"
    );

    // 完全追平：再补一帧带历史的，合并时跳过的行回到客户端的滚动历史里。
    client.read_and_acknowledge(&sess, fd);
    let skipped = format!("line {}", total - 5_000);
    assert!(
        text_lines(&client.term).contains(&skipped),
        "合并期间跳过的行应当补回历史"
    );

    // 补完以后恢复实时输出。
    pump(&sess, &mut parser, b"after\r\n");
    client.read_and_acknowledge(&sess, fd);
    assert!(text_lines(&client.term).contains(&"after".to_string()));
}
