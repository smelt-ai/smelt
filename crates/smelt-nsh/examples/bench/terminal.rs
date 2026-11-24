//! 真实 `smelt_terminal::TerminalCanvas` 合成流驱动器：通过内存流对（`RuntimeStream::pair`）
//! 扮演守护终端会话侧，支持静态填充（零后台唤醒）与高频 ANSI 彩色日志刷屏两种负载。

use std::{
    io::{self, BufRead as _, BufReader, Read as _, Write as _},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use gpui::{App, AppContext as _, Entity, Window};
use serde_json::{Value, json};
use smelt_plugin_api::{
    RuntimeKey,
    client::{Connector, RuntimeStream, protocol::daemon::DaemonOperation},
};
use smelt_terminal::{TerminalCanvas, TerminalLabels, TerminalSpec};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BenchTerminalMode {
    /// 写入初始 200 行彩色输出后完全静默等待退出（空闲测试）。
    IdlePopulated,
    /// 每 16ms 持续推送约 12 行含 ANSI 256 色与 Cargo/Git 风格的刷屏输出。
    Flood,
}

pub struct BenchTerminalConnector {
    mode: BenchTerminalMode,
    stop_flag: AtomicBool,
    stop_signal: (Mutex<bool>, Condvar),
    connected: AtomicUsize,
}

impl BenchTerminalConnector {
    pub fn new(mode: BenchTerminalMode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            stop_flag: AtomicBool::new(false),
            stop_signal: (Mutex::new(false), Condvar::new()),
            connected: AtomicUsize::new(0),
        })
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
        let mut guard = self
            .stop_signal
            .0
            .lock()
            .expect("terminal stop mutex poisoned");
        *guard = true;
        self.stop_signal.1.notify_all();
    }

    pub fn connected_count(&self) -> usize {
        self.connected.load(Ordering::Acquire)
    }

    fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::Acquire)
    }

    fn wait_idle_until_stopped(&self) {
        let mut guard = self
            .stop_signal
            .0
            .lock()
            .expect("terminal stop mutex poisoned");
        while !*guard {
            guard = self
                .stop_signal
                .1
                .wait(guard)
                .expect("terminal stop condvar poisoned");
        }
    }

    fn wait_flood_tick(&self, interval: Duration) -> bool {
        let guard = self
            .stop_signal
            .0
            .lock()
            .expect("terminal stop mutex poisoned");
        if *guard {
            return false;
        }
        let (guard, _) = self
            .stop_signal
            .1
            .wait_timeout(guard, interval)
            .expect("terminal stop condvar poisoned");
        !*guard
    }
}

/// 持有 `Arc<BenchTerminalConnector>` 的连接器包装，让服务线程能直接等待同一份停机通知。
pub struct ArcTerminalConnector {
    inner: Arc<BenchTerminalConnector>,
}

impl ArcTerminalConnector {
    pub fn new(inner: Arc<BenchTerminalConnector>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl Connector for ArcTerminalConnector {
    fn connect(&self) -> io::Result<RuntimeStream> {
        let (client, mut server) = RuntimeStream::pair()?;
        let shared = Arc::clone(&self.inner);
        let reader_stream = server.try_clone()?;

        thread::spawn(move || {
            let mut handshake_line = String::new();
            {
                let Ok(clone_for_read) = server.try_clone() else {
                    return;
                };
                let mut br = BufReader::new(clone_for_read);
                if br.read_line(&mut handshake_line).is_err() {
                    return;
                }
            }
            let Ok(req) = serde_json::from_str::<Value>(handshake_line.trim()) else {
                return;
            };
            let Ok(op) = serde_json::from_value::<DaemonOperation>(req["op"].clone()) else {
                return;
            };
            if op != DaemonOperation::Open {
                return;
            }

            let rows = req["rows"].as_u64().unwrap_or(24);
            let cols = req["cols"].as_u64().unwrap_or(80);
            let reply = json!({
                "ok": true,
                "rows": rows,
                "cols": cols,
                "replay_len": 0,
                "daemon_handles_color_requests": true,
                "daemon_answers_queries": true,
            });
            let mut reply_bytes = serde_json::to_vec(&reply).unwrap_or_default();
            reply_bytes.push(b'\n');
            if server.write_all(&reply_bytes).is_err() || server.flush().is_err() {
                return;
            }

            // 后台丢弃客户端写回的 resize / ack 帧，避免阻塞流缓冲。
            let drain_shared = Arc::clone(&shared);
            thread::spawn(move || {
                let mut stream = reader_stream;
                let mut buf = [0u8; 4096];
                while !drain_shared.is_stopped() {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            });

            shared.connected.fetch_add(1, Ordering::Release);

            // 先推 200 行带 ANSI 颜色的初始内容，让网格和回滚历史非空。
            let initial = build_initial_ansi_lines(200);
            if server.write_all(initial.as_bytes()).is_err() {
                return;
            }

            match shared.mode {
                BenchTerminalMode::IdlePopulated => {
                    shared.wait_idle_until_stopped();
                }
                BenchTerminalMode::Flood => {
                    let mut seq: u64 = 0;
                    let mut batch = String::with_capacity(2048);
                    while shared.wait_flood_tick(Duration::from_millis(16)) {
                        batch.clear();
                        append_flood_burst(&mut batch, &mut seq, 12);
                        if server.write_all(batch.as_bytes()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(client)
    }
}

fn build_initial_ansi_lines(lines: usize) -> String {
    let mut out = String::with_capacity(lines * 96);
    for i in 0..lines {
        let color = 31 + (i % 6);
        let bg = 234 + (i % 8);
        out.push_str(&format!(
            "\x1b[38;5;{bg}m\x1b[1;{color}m[{i:04}]\x1b[0m \x1b[32mCompiling\x1b[0m smelt-terminal v0.1.0 (/work/smelt/crates/smelt-terminal) \x1b[90m+{} -{}\x1b[0m\r\n",
            (i * 3) % 97,
            i % 19
        ));
    }
    out
}

fn append_flood_burst(out: &mut String, seq: &mut u64, count: usize) {
    for _ in 0..count {
        let i = *seq;
        *seq += 1;
        let color = 196 + ((i as usize * 7) % 36);
        match i % 4 {
            0 => out.push_str(&format!(
                "\x1b[1;32m   Compiling\x1b[0m \x1b[38;5;{color}msmelt-crate-{:02}\x1b[0m v0.1.0 (/work/smelt/crates/smelt-nsh)\r\n",
                i % 24
            )),
            1 => out.push_str(&format!(
                "\x1b[36m[frame {i:06}]\x1b[0m \x1b[33mWARN\x1b[0m throughput probe line with \x1b[4munderlined\x1b[0m & \x1b[1mbold\x1b[0m tokens\r\n"
            )),
            2 => out.push_str(&format!(
                "\x1b[32m+\x1b[0m   let delta_{i} = mirror.apply_frame_delta(offset, total); \x1b[90m// tick {i}\x1b[0m\r\n"
            )),
            _ => out.push_str(&format!(
                "\x1b[31m-\x1b[0m   old_cached_grid_row[{:02}] -> \x1b[38;5;39mrefreshed\x1b[0m\r\n",
                i % 64
            )),
        }
    }
}

pub fn spawn_real_terminal(
    session_id: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalCanvas> {
    smelt_terminal::bind_keys(cx);
    let canvas =
        cx.new(|cx| TerminalCanvas::new(TerminalLabels::default(), None, None, window, cx));
    let spec = TerminalSpec {
        machine: RuntimeKey::Local,
        session_id: session_id.to_owned(),
        cwd: Some("/work/smelt".to_owned()),
        launch: None,
    };
    canvas.update(cx, |canvas, cx| canvas.open(spec, window, cx));
    canvas
}
