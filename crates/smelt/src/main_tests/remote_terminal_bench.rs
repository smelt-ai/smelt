//! 远程运行时在大量输出下的表现。
//!
//! 终端：原始字节吞吐、桌面解析后多久看到结尾、跟不上的客户端会不会被摘掉、重连那一下
//! 的快照多大、刷屏期间另一个会话敲键的回显延迟、慢客户端会不会拖住同一会话上的快客户端。
//! 对话：用一个不调模型的假 agent 量打开、短回合往返、模拟真实节奏的流式回复、极快的
//! 长回复、长回复期间的终端回显、带长历史重开和翻页。
//!
//! 每个测试要一条一次性配对串，默认不跑：
//! `SMELT_TEST_PAIRING_URI='smelt+iroh://…' cargo nextest run -p smelt remote_terminal_under_heavy_output --run-ignored only --no-capture`
//! 对话那组把测试名换成 `remote_conversation_under_heavy_output`。

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

use smelt_client::acp_client::{ConversationClientHandle, ConversationClientLaunch};
use smelt_model::acp_session::{AcpUserAction, ConversationSnapshot};
use smelt_model::daemon_state::DaemonPhase;

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

/// 用临时 `SMELT_HOME` 配对，不碰本机桌面的数据。返回（运行时，临时目录）。
fn pair_for_bench() -> (Runtime, std::path::PathBuf) {
    let uri = std::env::var("SMELT_TEST_PAIRING_URI").expect("设置 SMELT_TEST_PAIRING_URI");
    let home = std::env::temp_dir().join(format!("smelt-bench-{}", uuid::Uuid::new_v4().simple()));
    // SAFETY: nextest 每个测试一个进程，这里没有别的线程在读环境变量。
    unsafe { std::env::set_var("SMELT_HOME", &home) };
    let remote = crate::runtimes::pair_with(&uri).unwrap();
    (remote.runtime, home)
}

/// 撤销这次配对的设备，删掉临时目录。
fn unpair_after_bench(runtime: &Runtime, home: std::path::PathBuf) {
    let me = crate::runtimes::iroh_client().unwrap().id().to_string();
    let _ = runtime
        .client()
        .call::<smelt_model::control_api::DeviceRevoke>(
            &smelt_model::remote_config::DeviceIdParams { endpoint_id: me },
            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
        );
    let _ = std::fs::remove_dir_all(home);
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
    let (runtime, home) = pair_for_bench();
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
    unpair_after_bench(&runtime, home);
}

/// 假 agent 的启动命令。运行时按空白切命令、不经过 shell，所以脚本按字节编进
/// `python3 -c`，不用先把文件传到那台机器上。
fn bench_agent_launch() -> smelt_model::agent_kind::ConversationLaunchSpec {
    let bytes: Vec<String> = include_str!("bench_acp_agent.py")
        .bytes()
        .map(|byte| byte.to_string())
        .collect();
    smelt_model::agent_kind::ConversationLaunchSpec {
        command: format!("python3 -c exec(bytes([{}]).decode())", bytes.join(",")),
        env: Default::default(),
    }
}

fn open_conversation(runtime: &Runtime, id: &str) -> ConversationClientHandle {
    smelt_client::acp_client::spawn_acp_client(
        runtime.client().clone(),
        ConversationClientLaunch {
            id: id.to_string(),
            cwd: None,
            launch: bench_agent_launch(),
            engine_kind: smelt_model::agent_kind::ConversationAgentKind::OpenCode,
            ephemeral_env: Default::default(),
            resume_id: None,
            fork_id: None,
            fork_cut: None,
            retained_entries_end: 0,
            conversation_binding: Default::default(),
            pending_agent_preset: None,
        },
    )
}

fn next_snapshot(
    handle: &ConversationClientHandle,
    timeout: Duration,
) -> Option<ConversationSnapshot> {
    smol::block_on(smol::future::or(
        async { handle.snapshot_rx.recv().await.ok() },
        async {
            smol::Timer::after(timeout).await;
            None
        },
    ))
}

fn assistant_contains(snapshot: &ConversationSnapshot, needle: &str) -> bool {
    snapshot.entries.iter().any(|entry| {
        matches!(entry, smelt_model::acp_chat::AcpEntry::Assistant { text, .. } if text.contains(needle))
    })
}

/// 一个回合在客户端这边看起来怎样。
#[derive(Debug, Default)]
struct TurnView {
    /// 第一个片段出现。
    first: Option<Duration>,
    /// 结束标记出现。
    end: Option<Duration>,
    snapshots: usize,
    /// 相邻两份快照之间最长的间隔：用户看到的「卡住」有多久。
    max_gap: Duration,
}

fn send_prompt(handle: &ConversationClientHandle, text: String) {
    handle
        .action_tx
        .try_send(AcpUserAction::Prompt {
            text,
            images: Vec::new(),
            delivery_id: None,
        })
        .unwrap();
}

/// 发一回合 `stream <片段数> <每片字节> <间隔毫秒> <标签>`，一直收快照到结束标记。
fn run_turn(
    handle: &ConversationClientHandle,
    count: usize,
    size: usize,
    delay_ms: u64,
    tag: &str,
) -> TurnView {
    send_prompt(handle, format!("stream {count} {size} {delay_ms} {tag}"));
    watch_turn(handle, tag, Duration::from_secs(300))
}

fn watch_turn(handle: &ConversationClientHandle, tag: &str, timeout: Duration) -> TurnView {
    let start = Instant::now();
    let first_needle = format!("[{tag} 000000]");
    let end_needle = format!("__END_{tag}__");
    let mut view = TurnView::default();
    let mut last = start;
    while start.elapsed() < timeout {
        let Some(snapshot) = next_snapshot(handle, Duration::from_millis(500)) else {
            continue;
        };
        let now = Instant::now();
        view.snapshots += 1;
        view.max_gap = view.max_gap.max(now - last);
        last = now;
        if view.first.is_none() && assistant_contains(&snapshot, &first_needle) {
            view.first = Some(start.elapsed());
        }
        if assistant_contains(&snapshot, &end_needle) {
            view.end = Some(start.elapsed());
            return view;
        }
    }
    view
}

fn report_turn(label: &str, view: &TurnView) {
    eprintln!(
        "[{label}] 首片 {:?}，结束 {:?}，快照 {} 份，最长间隔 {:.2}s",
        view.first,
        view.end,
        view.snapshots,
        view.max_gap.as_secs_f64()
    );
}

/// 线路上一回合实际传了多少字节、花在哪些字段上。客户端拿到的是还原后的快照，
/// 看不出线路上的量，所以这里直接读原始连接。
fn conversation_wire_bytes(runtime: &Runtime, count: usize, size: usize, delay_ms: u64) {
    let id = sid("chat-wire");
    measure_conversation_wire(runtime, &id, count, size, delay_ms);
    smelt_client::acp_client::kill_acp_session(runtime.client(), &id);
}

fn measure_conversation_wire(
    runtime: &Runtime,
    id: &str,
    count: usize,
    size: usize,
    delay_ms: u64,
) {
    let mut stream = runtime.client().connect().unwrap();
    let open = serde_json::json!({
        "op": "acp_open",
        "id": id,
        "cwd": null,
        "launch": bench_agent_launch(),
        "agent": "opencode",
        "ephemeral_env": {},
        "resume_id": null,
        "fork_id": null,
        "fork_cut": null,
        "tail_limit": 100,
        "omit_history": true,
        "conversation_binding": smelt_model::conversation::ConversationBinding::default(),
    });
    writeln!(stream, "{open}").unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    // 增量帧只带变了的字段，状态要经镜像还原才读得出来。
    let mut mirror = smelt_model::acp_wire::FrameMirror::default();
    let mut restore = |line: &str| -> serde_json::Value {
        let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
        match frame.get("delta") {
            Some(delta) => serde_json::from_value(delta.clone())
                .ok()
                .and_then(|delta| mirror.apply(&delta).ok())
                .unwrap_or_default(),
            None => {
                mirror.full(&frame);
                frame
            }
        }
    };
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            eprintln!("[对话 线路] 打开失败");
            return;
        }
        let frame = restore(&line);
        match frame["live"]["phase"].as_str() {
            Some("connecting") | None => {}
            Some("dead" | "failed") => {
                eprintln!(
                    "[对话 线路] 假 agent 没起来：{}",
                    frame["live"]["end_reason"]
                );
                return;
            }
            Some(_) => break,
        }
    }
    let prompt = AcpUserAction::Prompt {
        text: format!("stream {count} {size} {delay_ms} wire"),
        images: Vec::new(),
        delivery_id: None,
    };
    writeln!(stream, "{}", serde_json::to_string(&prompt).unwrap()).unwrap();
    let mut frames = 0usize;
    let mut total = 0usize;
    let mut by_field: std::collections::BTreeMap<String, usize> = Default::default();
    let size_of = |value: &serde_json::Value| value.to_string().len();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        frames += 1;
        total += line.len();
        let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
        for (key, value) in frame.as_object().into_iter().flatten() {
            if key == "snapshot" {
                for (field, value) in value.as_object().into_iter().flatten() {
                    *by_field.entry(format!("snapshot.{field}")).or_default() += size_of(value);
                }
            } else {
                *by_field.entry(key.clone()).or_default() += size_of(value);
            }
        }
        if line.contains("__END_wire__") {
            break;
        }
    }
    let reply = count * size;
    eprintln!(
        "[对话 线路 {count}×{size}B/{delay_ms}ms] 回复本身 {:.2}MB，线路 {frames} 帧共 {:.2}MB（{:.1} 倍）",
        mb(reply as u64),
        mb(total as u64),
        total as f64 / reply as f64
    );
    let mut fields: Vec<_> = by_field.into_iter().collect();
    fields.sort_by(|a, b| b.1.cmp(&a.1));
    for (field, bytes) in fields.into_iter().take(6) {
        eprintln!("    {field}: {:.2}MB", mb(bytes as u64));
    }
}

#[test]
#[ignore = "需要一台真实运行时和一次性配对串"]
fn remote_conversation_under_heavy_output() {
    let (runtime, home) = pair_for_bench();
    let id = sid("chat");

    // 0. 线路上的量：模拟真实节奏的一回合，按字段拆开看。
    conversation_wire_bytes(&runtime, 300, 60, 25);

    // 1. 打开：到 agent 就绪、可以发 prompt。
    let start = Instant::now();
    let handle = open_conversation(&runtime, &id);
    let mut ready = None;
    while start.elapsed() < Duration::from_secs(60) {
        let Some(snapshot) = next_snapshot(&handle, Duration::from_millis(500)) else {
            continue;
        };
        if !matches!(snapshot.phase, DaemonPhase::Connecting) {
            assert!(
                !matches!(snapshot.phase, DaemonPhase::Dead | DaemonPhase::Failed),
                "假 agent 没起来：{}",
                snapshot.end_reason
            );
            ready = Some(start.elapsed());
            break;
        }
    }
    eprintln!("[对话 打开] 到就绪 {ready:?}");

    // 2. 短回合往返：一句话进去、一句话出来，就是用户感受到的「发出去多久有回应」。
    let short: Vec<_> = (0..5)
        .map(|i| run_turn(&handle, 1, 16, 0, &format!("s{i}")).end)
        .collect();
    eprintln!("[对话 短回合] {short:?}");

    // 3. 模拟真实模型的节奏：每 25ms 一片、共 300 片，生成本身 7.5s。
    let live = run_turn(&handle, 300, 60, 25, "live");
    report_turn("对话 真实节奏 300×60B/25ms", &live);

    // 4. 极快的长回复：5000 片 × 200B ≈ 1MB 一口气吐完。
    let burst = run_turn(&handle, 5000, 200, 0, "burst");
    report_turn("对话 极快长回复 5000×200B", &burst);

    // 5. 长回复期间同一运行时上另一个终端的回显。
    {
        let echo_id = sid("chat-echo");
        let mut echo = Terminal::spawn(&runtime, 40, 160, None, &echo_id, Some("cat")).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        send_prompt(&handle, "stream 5000 200 0 busy".to_string());
        let echoes: Vec<_> = (0..5)
            .map(|i| {
                let tag = format!("chat{i}x");
                let start = Instant::now();
                echo.send_input(tag.as_bytes());
                wait_for_screen(&echo, &tag, Duration::from_secs(30)).map(|_| start.elapsed())
            })
            .collect();
        eprintln!("[对话 长回复中的终端回显] {echoes:?}");
        let busy = watch_turn(&handle, "busy", Duration::from_secs(300));
        report_turn("对话 长回复（同时敲终端）", &busy);
        drop(echo);
        crate::terminal::kill_remote(&runtime, &echo_id);
    }

    // 6. 带长历史重开（桌面重启、切回这个对话走的就是这条），再往前翻一页。
    drop(handle);
    std::thread::sleep(Duration::from_secs(1));
    let start = Instant::now();
    let reopened = open_conversation(&runtime, &id);
    // 窗口只订当前一轮，首帧不带历史；历史按页往前翻。
    let mut first = None;
    while start.elapsed() < Duration::from_secs(60) {
        let Some(snapshot) = next_snapshot(&reopened, Duration::from_millis(500)) else {
            continue;
        };
        if !matches!(snapshot.phase, DaemonPhase::Connecting) {
            first = Some((
                start.elapsed(),
                snapshot.entries_offset,
                snapshot.entries_total,
                serde_json::to_vec(&snapshot)
                    .map(|json| json.len())
                    .unwrap_or(0),
            ));
            break;
        }
    }
    eprintln!("[对话 重开] 首帧（耗时, 起点, 总条数, 字节）{first:?}");
    if let Some((_, offset, _, _)) = first.filter(|(_, offset, _, _)| *offset > 0) {
        let start = Instant::now();
        let page = smelt_client::acp_client::load_acp_history(runtime.client(), &id, offset, 50);
        eprintln!(
            "[对话 翻页 50 条] {:.2}s，{} 条，{:.2}MB",
            start.elapsed().as_secs_f64(),
            page.as_ref().map(|page| page.entries.len()).unwrap_or(0),
            mb(page
                .as_ref()
                .ok()
                .and_then(|page| serde_json::to_vec(page).ok())
                .map(|json| json.len() as u64)
                .unwrap_or(0))
        );
    }
    drop(reopened);

    smelt_client::acp_client::kill_acp_session(runtime.client(), &id);
    unpair_after_bench(&runtime, home);
}
