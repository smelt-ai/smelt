//! 远程终端在大量输出下的表现（评估要不要改成「服务端持有画面、只发变化的行」）。
//!
//! 对着一台真实运行时量：原始字节吞吐、桌面解析后多久看到结尾、守护 8MB 邮箱会不会
//! 把连接摘掉、重连那一下的快照多大、刷屏期间另一个会话敲键的回显延迟、慢客户端会不会
//! 拖住同一会话上的快客户端。要一次性配对串，默认不跑：
//! `SMELT_TEST_PAIRING_URI='smelt+iroh://…' cargo nextest run -p smelt remote_terminal_bench --run-ignored only --no-capture`

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

use crate::runtimes::Runtime;
use crate::terminal::Terminal;

/// 命令行里写成 `__DO''NE__`，shell 回显命令时不会提前出现标记。
const DONE: &[u8] = b"__DONE__";

fn sid(name: &str) -> String {
    format!("bench-{name}-{}", uuid::Uuid::new_v4().simple())
}

/// 一条只数字节、不解析的 attach。返回（握手回执, 读端）。
fn raw_attach(
    runtime: &Runtime,
    id: &str,
    launch: Option<&str>,
    create: bool,
) -> (serde_json::Value, BufReader<std::os::unix::net::UnixStream>) {
    let mut stream = runtime.client().connect().unwrap();
    let request = crate::terminal::open_request(40, 160, None, id, launch, create);
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::with_capacity(256 * 1024, stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let reply: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_ne!(reply["ok"].as_bool(), Some(false), "attach 失败：{reply}");
    (reply, reader)
}

struct RawRun {
    bytes: u64,
    elapsed: Duration,
    saw_done: bool,
    /// 没看到结尾就断了：守护把这条连接摘掉了。
    dropped: bool,
}

/// 读到结尾标记、断开或超时为止。`pause` 模拟慢客户端：每读一块歇一下。
fn drain(
    reader: &mut BufReader<std::os::unix::net::UnixStream>,
    timeout: Duration,
    pause: Option<(usize, Duration)>,
) -> RawRun {
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let start = Instant::now();
    let chunk = pause.map(|(size, _)| size).unwrap_or(256 * 1024);
    let mut buf = vec![0u8; chunk];
    let mut tail: Vec<u8> = Vec::new();
    let mut bytes = 0u64;
    loop {
        if start.elapsed() > timeout {
            return RawRun {
                bytes,
                elapsed: start.elapsed(),
                saw_done: false,
                dropped: false,
            };
        }
        match reader.read(&mut buf) {
            Ok(0) => {
                return RawRun {
                    bytes,
                    elapsed: start.elapsed(),
                    saw_done: false,
                    dropped: true,
                };
            }
            Ok(n) => {
                bytes += n as u64;
                // 和桌面读线程一样每块都确认，不然守护会把后续输出全合并掉。
                let mut ack = vec![2u8];
                ack.extend_from_slice(&8u32.to_be_bytes());
                ack.extend_from_slice(&bytes.to_be_bytes());
                let _ = reader.get_ref().write_all(&ack);
                tail.extend_from_slice(&buf[..n]);
                if tail.windows(DONE.len()).any(|w| w == DONE) {
                    return RawRun {
                        bytes,
                        elapsed: start.elapsed(),
                        saw_done: true,
                        dropped: false,
                    };
                }
                let keep = tail.len().saturating_sub(DONE.len());
                tail.drain(..keep);
                if let Some((_, nap)) = pause {
                    std::thread::sleep(nap);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                return RawRun {
                    bytes,
                    elapsed: start.elapsed(),
                    saw_done: false,
                    dropped: true,
                };
            }
        }
    }
}

fn screen_text(term: &Terminal) -> String {
    term.snapshot()
        .rows
        .iter()
        .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_for_screen(term: &Terminal, needle: &str, timeout: Duration) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if screen_text(term).contains(needle) {
            return Some(start.elapsed());
        }
        if term.is_dead() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

/// 约 15MB 纯文本。
const PLAIN_FLOOD: &str = "seq 1 2000000; echo __DO''NE__";
/// 约 25MB 带颜色的「构建日志」。
const COLOR_FLOOD: &str = "seq 1 400000 | awk '{printf \"\\033[3%dm%08d compiling crate_%d v0.1.0 (/home/me/proj/crates/crate_%d) warning: unused variable\\033[0m\\n\", $1%7, $1, $1%97, $1%97}'; echo __DO''NE__";

#[test]
#[ignore = "需要一台真实运行时和一次性配对串"]
fn remote_terminal_under_heavy_output() {
    let uri = std::env::var("SMELT_TEST_PAIRING_URI").expect("设置 SMELT_TEST_PAIRING_URI");
    let home = std::env::temp_dir().join(format!("smelt-bench-{}", uuid::Uuid::new_v4().simple()));
    // SAFETY: nextest 每个测试一个进程，这里没有别的线程在读环境变量。
    unsafe { std::env::set_var("SMELT_HOME", &home) };
    let remote = crate::runtimes::pair_with(&uri).unwrap();
    let runtime = remote.runtime;
    let mut sessions = Vec::new();

    // 往返时延：一次最小的控制调用。
    let rtts: Vec<Duration> = (0..5)
        .map(|_| {
            let start = Instant::now();
            runtime.client().describe().unwrap();
            start.elapsed()
        })
        .collect();
    eprintln!("控制调用往返：{:?}", rtts);

    // 1. 原始字节吞吐（不解析），纯文本和彩色各一次。
    for (name, launch) in [("plain", PLAIN_FLOOD), ("color", COLOR_FLOOD)] {
        let id = sid(name);
        sessions.push(id.clone());
        let (_, mut reader) = raw_attach(&runtime, &id, Some(launch), true);
        let run = drain(&mut reader, Duration::from_secs(300), None);
        eprintln!(
            "[原始 {name}] {:.1}MB 用时 {:.1}s = {:.2}MB/s，看到结尾={}，被摘={}",
            mb(run.bytes),
            run.elapsed.as_secs_f64(),
            mb(run.bytes) / run.elapsed.as_secs_f64(),
            run.saw_done,
            run.dropped
        );
    }

    // 2. 桌面真实路径：Terminal（alacritty 解析）多久看到结尾；断了就量一次重连。
    {
        let id = sid("desktop");
        sessions.push(id.clone());
        let start = Instant::now();
        let term = Terminal::spawn(&runtime, 40, 160, None, &id, Some(COLOR_FLOOD)).unwrap();
        match wait_for_screen(&term, "__DONE__", Duration::from_secs(300)) {
            Some(took) => eprintln!("[桌面 color] 看到结尾用时 {:.1}s", took.as_secs_f64()),
            None => eprintln!(
                "[桌面 color] {:.1}s 时没看到结尾，连接断了={}",
                start.elapsed().as_secs_f64(),
                term.is_dead()
            ),
        }
        // 重连：桌面断线后走的就是这条，守护先发一份带历史的快照。
        let (reply, mut reader) = raw_attach(&runtime, &id, None, false);
        let replay = reply["replay_len"].as_u64().unwrap_or(0);
        let start = Instant::now();
        let mut buf = vec![0u8; 256 * 1024];
        let mut got = 0u64;
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        while got < replay {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n as u64,
            }
        }
        eprintln!(
            "[重连快照] {:.2}MB，传完 {:.2}s",
            mb(replay),
            start.elapsed().as_secs_f64()
        );
        let start = Instant::now();
        let again = Terminal::reattach(&runtime, 40, 160, None, &id).unwrap();
        let shown = wait_for_screen(&again, "__DONE__", Duration::from_secs(60));
        eprintln!(
            "[桌面重连] 到画面出来 {:.2}s（看到结尾={}）",
            start.elapsed().as_secs_f64(),
            shown.is_some()
        );
        drop(term);
        drop(again);
    }

    // 3. 刷屏期间另一个会话的回显延迟（同一条 iroh 连接上的另一条流）。
    {
        let echo_id = sid("echo");
        let flood_id = sid("flood");
        sessions.push(echo_id.clone());
        sessions.push(flood_id.clone());
        let mut echo = Terminal::spawn(&runtime, 40, 160, None, &echo_id, Some("cat")).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let measure = |echo: &mut Terminal, tag: &str| -> Option<Duration> {
            let start = Instant::now();
            echo.send_input(tag.as_bytes());
            wait_for_screen(echo, tag, Duration::from_secs(30)).map(|_| start.elapsed())
        };
        let idle: Vec<_> = (0..5)
            .map(|i| measure(&mut echo, &format!("idle{i}x")))
            .collect();
        eprintln!("[回显 空闲] {:?}", idle);
        let flood = Terminal::spawn(&runtime, 40, 160, None, &flood_id, Some(COLOR_FLOOD)).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let busy: Vec<_> = (0..5)
            .map(|i| measure(&mut echo, &format!("busy{i}x")))
            .collect();
        eprintln!("[回显 刷屏中] {:?}", busy);
        drop(flood);
    }

    // 4. 同一会话上一个慢客户端、一个快客户端：慢的会不会拖住快的。
    {
        let id = sid("shared");
        sessions.push(id.clone());
        let launch = format!("sleep 3; {COLOR_FLOOD}");
        let (_, mut fast) = raw_attach(&runtime, &id, Some(&launch), true);
        let (_, mut slow) = raw_attach(&runtime, &id, None, false);
        let slow_thread = std::thread::spawn(move || {
            drain(
                &mut slow,
                Duration::from_secs(300),
                Some((16 * 1024, Duration::from_millis(20))),
            )
        });
        let fast_run = drain(&mut fast, Duration::from_secs(300), None);
        let slow_run = slow_thread.join().unwrap();
        eprintln!(
            "[共享 快] {:.1}MB {:.1}s 看到结尾={} 被摘={}",
            mb(fast_run.bytes),
            fast_run.elapsed.as_secs_f64(),
            fast_run.saw_done,
            fast_run.dropped
        );
        eprintln!(
            "[共享 慢 每 20ms 读 16KB] {:.1}MB {:.1}s 看到结尾={} 被摘={}",
            mb(slow_run.bytes),
            slow_run.elapsed.as_secs_f64(),
            slow_run.saw_done,
            slow_run.dropped
        );
    }

    for id in &sessions {
        crate::terminal::kill_remote(&runtime, id);
    }
    let me = crate::runtimes::iroh_client().unwrap().id().to_string();
    let _ = runtime
        .client()
        .call::<smelt_model::control_api::DeviceRevoke>(
            &smelt_model::remote_config::DeviceIdParams { endpoint_id: me },
            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
        );
    let _ = std::fs::remove_dir_all(home);
}
