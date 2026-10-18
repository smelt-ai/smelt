//! 终端 attachment 的有界输出邮箱。
//!
//! 每个连接自己一条写队列和写线程：PTY 泵只做常数时间入队，绝不直接等 GUI socket。
//! ACP 快照不走这里。快照是状态，不是字节日志；复用这套超限摘连接的队列会在
//! 历史重放时把还活着的控制通道掐掉。

use std::collections::VecDeque;
use std::io::{ErrorKind, Write};
use std::net::Shutdown;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// 单个终端 attachment 尚未交给内核的输出上限。超过说明该客户端已经长期不消费；
/// 摘掉它以保护守护进程内存，而不是让它反压 PTY 泵或拖慢其它 attachment。
/// 交互连接有流控，在途量远小于它；这里只兜底不回确认的旁观连接和失常的客户端。
const TERMINAL_OUTPUT_QUEUE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// 交互连接上客户端还没确认的实时输出上限。超过就不再往这个客户端发实时字节，
/// 改为合并：等它追上来补一帧当前画面。窗口要盖住慢链路的带宽时延积，否则卡吞吐。
const FLOW_WINDOW_BYTES: u64 = 512 * 1024;

/// 合并中的客户端确认到只剩这么多在途，就补发当前屏幕，不必等完全排空再多等一个往返。
const FLOW_RESUME_BYTES: u64 = 128 * 1024;

/// 优雅关闭的冲刷上限。客户端不读时不能无限占住写线程与 fd。
// 冲刷关闭只被同文件单测调用。bin 目标不编译 tests，所以这里单独放开死代码检查。
#[cfg_attr(not(test), allow(dead_code))]
const GRACEFUL_CLOSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// 单 attachment 的有界输出邮箱。队列大小按字节限制而不是按消息数限制，避免一条
/// 高吞吐 PTY 输出在 GUI 暂停时无界占内存。写线程取走一段后才会进行可能阻塞的
/// `write_all`，因此 PTY 泵只做常数时间的入队操作。
struct OutputMailbox {
    state: Mutex<OutputMailboxState>,
    ready: Condvar,
    max_bytes: usize,
}

struct OutputMailboxState {
    chunks: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    closed: bool,
    /// 不再接收新数据，但已入队的内容仍要写完。`close` 是立即丢弃，两者语义不同。
    finishing: bool,
}

impl OutputMailbox {
    fn new(max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(OutputMailboxState {
                chunks: VecDeque::new(),
                queued_bytes: 0,
                closed: false,
                finishing: false,
            }),
            ready: Condvar::new(),
            max_bytes,
        }
    }

    /// 保留消息边界，确保 attach 快照一定排在后续实时输出之前。空队列允许一份超过
    /// 常规上限的首包：它本身已经在内存中，拒绝只会让所有大历史会话无法重连；在该
    /// 首包尚未写完前，任何新的实时数据都会因上限被隔离。
    fn enqueue(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let mut state = self.state.lock().unwrap();
        if state.closed || state.finishing {
            return false;
        }
        let fits = bytes.len() <= self.max_bytes.saturating_sub(state.queued_bytes);
        if !fits && !state.chunks.is_empty() {
            state.closed = true;
            state.chunks.clear();
            state.queued_bytes = 0;
            drop(state);
            self.ready.notify_all();
            return false;
        }
        state.queued_bytes = state.queued_bytes.saturating_add(bytes.len());
        state.chunks.push_back(bytes.to_vec());
        drop(state);
        self.ready.notify_one();
        true
    }

    fn recv(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(chunk) = state.chunks.pop_front() {
                state.queued_bytes = state.queued_bytes.saturating_sub(chunk.len());
                return Some(chunk);
            }
            if state.closed || state.finishing {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        state.chunks.clear();
        state.queued_bytes = 0;
        drop(state);
        self.ready.notify_all();
    }

    /// 封口但不丢数据：写线程把剩下的 chunk 写完后自然收尾并 shutdown socket。
    #[cfg_attr(not(test), allow(dead_code))]
    fn finish(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.finishing {
            return;
        }
        state.finishing = true;
        drop(state);
        self.ready.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.state.lock().map(|state| state.closed).unwrap_or(true)
    }

    fn is_finishing(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.finishing)
            .unwrap_or(false)
    }
}

/// 合并结束时要补哪一种关键帧。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyframeNeed {
    /// 只画当前屏幕：刷屏还在继续，先让客户端看到最新画面。
    Screen,
    /// 屏幕加滚动历史：客户端已经追平，把合并期间跳过的内容补回它的历史里。
    Full,
}

/// 按客户端确认做的流控（tmux、muxy 同一思路）。同时在途的实时输出有上限，超出的
/// 不排队，客户端追上来时用一帧关键帧代替。慢客户端因此只是看到的帧变少，不会被
/// 断开，也拖不慢 PTY 和其它客户端。只给会回 `Ack` 的交互连接用。
#[derive(Debug, Default)]
struct FlowControl {
    /// 交给邮箱的累计字节数，不含首行 header。
    sent: u64,
    /// 客户端确认已处理的累计字节数。
    acked: u64,
    /// 最近一帧关键帧在流里的结束位置。关键帧本身不占窗口：它是我们主动选的，
    /// 算进去的话补一次完整历史就会把紧随其后的实时输出全挤成合并。
    keyframe_end: u64,
    /// 正在合并：实时输出不再发给这个客户端，它的画面已过时。
    coalescing: bool,
    /// 合并期间跳过的内容也不在客户端的滚动历史里，追平后要补一帧完整的。
    history_stale: bool,
}

impl FlowControl {
    fn with_keyframe(len: u64) -> Self {
        Self {
            sent: len,
            keyframe_end: len,
            ..Self::default()
        }
    }

    /// 这段实时输出能不能发。在途为零时总能发，否则一段超窗的输出会让客户端
    /// 再也收不到可确认的东西，合并就永远结束不了。
    fn admit(&mut self, len: u64) -> bool {
        if self.coalescing {
            return false;
        }
        let in_flight = self.sent - self.acked.max(self.keyframe_end);
        if in_flight > 0 && in_flight + len > FLOW_WINDOW_BYTES {
            self.coalescing = true;
            return false;
        }
        self.sent += len;
        true
    }

    fn keyframe_sent(&mut self, len: u64, need: KeyframeNeed) {
        self.sent += len;
        self.keyframe_end = self.sent;
        self.coalescing = false;
        self.history_stale = need == KeyframeNeed::Screen;
    }

    fn acknowledge(&mut self, consumed: u64) -> Option<KeyframeNeed> {
        self.acked = self.acked.max(consumed.min(self.sent));
        let in_flight = self.sent - self.acked;
        if self.coalescing {
            return (in_flight <= FLOW_RESUME_BYTES).then_some(KeyframeNeed::Screen);
        }
        (self.history_stale && in_flight == 0).then_some(KeyframeNeed::Full)
    }
}

/// 一个客户端输出端。`shutdown` 是写线程 socket 的 duplicate，连接移除时用它同时
/// 唤醒可能阻塞的写线程与 handle_open/handle_watch 的读循环。
pub(crate) struct OutputAttachment {
    pub(crate) fd: RawFd,
    mailbox: Arc<OutputMailbox>,
    shutdown: UnixStream,
    /// `None`：不回确认的旁观连接，仍按邮箱上限隔离。
    flow: Option<FlowControl>,
}

impl OutputAttachment {
    /// 交互连接：`header` 是首行，`keyframe` 是紧随其后的画面快照。客户端从快照第
    /// 一个字节起按累计字节数回确认，流控据此限制在途的实时输出。
    pub(crate) fn interactive(
        stream: UnixStream,
        header: Vec<u8>,
        keyframe: Vec<u8>,
        id: &str,
    ) -> std::io::Result<Self> {
        let flow = FlowControl::with_keyframe(keyframe.len() as u64);
        let mut initial = header;
        initial.extend_from_slice(&keyframe);
        let mut attachment = Self::new(stream, initial, id, "attachment")?;
        attachment.flow = Some(flow);
        Ok(attachment)
    }

    pub(crate) fn new(
        stream: UnixStream,
        initial: Vec<u8>,
        id: &str,
        kind: &'static str,
    ) -> std::io::Result<Self> {
        let fd = stream.as_raw_fd();
        // 不继承任何短写超时。GUI 短暂停顿时由 mailbox 吸收，只有队列真正耗尽才
        // 隔离 attachment；这里若仍是 3 秒超时，会重新把瞬时背压误判成断线。
        stream.set_write_timeout(None)?;
        let shutdown = stream.try_clone()?;
        let mailbox = Arc::new(OutputMailbox::new(TERMINAL_OUTPUT_QUEUE_MAX_BYTES));
        let attachment = Self {
            fd,
            mailbox: Arc::clone(&mailbox),
            shutdown,
            flow: None,
        };
        // 初始 header + keyframe 先入队；调用方仍持有 output_gate，因此后续实时输出
        // 不可能插到它前面。
        if !attachment.enqueue(&initial) {
            return Err(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "terminal output mailbox closed during attach",
            ));
        }

        let writer_id = id.to_string();
        thread::spawn(move || {
            let mut stream = stream;
            while let Some(bytes) = mailbox.recv() {
                if let Err(error) = stream.write_all(&bytes) {
                    // 主动摘除时会先 close mailbox，再 shutdown socket；这种预期的
                    // interrupted write 不需要污染日志。
                    if !mailbox.is_closed() {
                        crate::dlog(&format!(
                            "{kind} writer stopped id={writer_id} fd={fd} error={error}"
                        ));
                    }
                    mailbox.close();
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
            }
            let _ = stream.shutdown(Shutdown::Both);
        });

        Ok(attachment)
    }

    pub(crate) fn enqueue(&self, bytes: &[u8]) -> bool {
        self.mailbox.enqueue(bytes)
    }

    /// 实时输出。流控判定要合并的就不发；返回 false 只表示这个连接已经死了。
    fn send_output(&mut self, bytes: &[u8]) -> bool {
        if let Some(flow) = self.flow.as_mut()
            && !flow.admit(bytes.len() as u64)
        {
            return !self.mailbox.is_closed();
        }
        self.mailbox.enqueue(bytes)
    }

    /// 客户端确认处理到第 `consumed` 个字节，返回此刻要不要补关键帧。
    pub(crate) fn acknowledge(&mut self, consumed: u64) -> Option<KeyframeNeed> {
        self.flow.as_mut()?.acknowledge(consumed)
    }

    /// 补一帧关键帧（终端重置 + 画面快照），它取代合并期间跳过的全部输出。
    pub(crate) fn send_keyframe(&mut self, keyframe: &[u8], need: KeyframeNeed) -> bool {
        if let Some(flow) = self.flow.as_mut() {
            flow.keyframe_sent(keyframe.len() as u64, need);
        }
        self.mailbox.enqueue(keyframe)
    }

    pub(crate) fn close(&self) {
        self.mailbox.close();
        let _ = self.shutdown.shutdown(Shutdown::Both);
    }

    /// 终结会话时用：先让已入队的终态快照写出去，再断开。硬 `close` 会把队列
    /// 直接清空，客户端只看到一个裸 EOF，无法区分「会话被删」和「传输断了」，
    /// 于是按后者重连，把刚删掉的会话原地复活。
    ///
    /// 冲刷有界：客户端不消费时，看门狗到点强制关闭，不会让写线程和 fd 永久挂住。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn close_after_flush(self) {
        self.mailbox.finish();
        let Ok(shutdown) = self.shutdown.try_clone() else {
            self.close();
            return;
        };
        let mailbox = Arc::clone(&self.mailbox);
        // Drop 会看到 finishing 标记，不再硬关；这里只丢掉本结构持有的 fd 复本。
        drop(self);
        thread::spawn(move || {
            thread::sleep(GRACEFUL_CLOSE_DEADLINE);
            mailbox.close();
            let _ = shutdown.shutdown(Shutdown::Both);
        });
    }
}

impl Drop for OutputAttachment {
    fn drop(&mut self) {
        // 正在优雅冲刷的邮箱不能被 Drop 顺手清空，否则终态快照又丢了。
        if self.mailbox.is_finishing() {
            return;
        }
        self.close();
    }
}

/// 同一会话的 output_gate 只负责输出序列。这里将字节复制进各 attachment 的 mailbox，
/// 不进行任何 socket 系统调用；一个冻结 GUI 至多耗尽自己的队列，不会阻塞 PTY 泵、
/// 其它窗口或会话管理。
pub(crate) fn enqueue_session_streams(
    streams: Vec<OutputAttachment>,
    bytes: &[u8],
    id: &str,
    kind: &str,
) -> Vec<OutputAttachment> {
    let mut live = Vec::with_capacity(streams.len());
    for mut stream in streams {
        if stream.send_output(bytes) {
            live.push(stream);
        } else {
            crate::dlog(&format!(
                "terminal {kind} output queue saturated or closed id={id} fd={}",
                stream.fd
            ));
            stream.close();
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Duration;

    #[test]
    fn mailbox_is_bounded_and_closes_before_unbounded_growth() {
        let mailbox = OutputMailbox::new(4);
        assert!(mailbox.enqueue(b"ab"));
        assert!(mailbox.enqueue(b"cd"));
        assert!(
            !mailbox.enqueue(b"e"),
            "超过字节上限的输出必须隔离 attachment，而不是继续堆积"
        );
        assert!(mailbox.is_closed());
        assert_eq!(mailbox.recv(), None, "关闭时应立即释放陈旧待发输出");
    }

    #[test]
    fn attachment_writes_keyframe_before_later_live_output() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let attachment =
            OutputAttachment::new(server, b"header".to_vec(), "test", "attachment").unwrap();
        assert!(attachment.enqueue(b"live"));

        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut received = [0u8; 10];
        client.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"headerlive");
        attachment.close();
    }

    /// 终结会话时先入队的终态快照不能被关连接丢掉：客户端必须先读到它，
    /// 再读到 EOF，否则只能把删除当成传输抖动去重连。
    #[test]
    fn close_after_flush_delivers_queued_bytes_then_eof() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let attachment =
            OutputAttachment::new(server, b"header".to_vec(), "test", "attachment").unwrap();
        assert!(attachment.enqueue(b"final"));
        attachment.close_after_flush();

        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut received = Vec::new();
        client.read_to_end(&mut received).unwrap();
        assert_eq!(received, b"headerfinal");
    }

    #[test]
    fn dispatch_never_writes_to_a_backpressured_attachment_socket() {
        let (mut server, _client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let filler = [0u8; 8192];
        let mut full = false;
        for _ in 0..4096 {
            match server.write(&filler) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    full = true;
                    break;
                }
                Err(error) => panic!("填充测试 socket 失败: {error}"),
            }
        }
        assert!(full, "测试 socket 应进入背压状态");

        // 故意不启动 writer：这个 attachment 的 socket 已不可写。若 PTY 转发重新
        // 直接 write_all，这里会得到 WouldBlock 并被踢出；正确实现只向 mailbox 入队。
        let attachment = OutputAttachment {
            fd: server.as_raw_fd(),
            mailbox: Arc::new(OutputMailbox::new(1024)),
            shutdown: server,
            flow: None,
        };
        let live = enqueue_session_streams(vec![attachment], b"x", "test", "attachment");
        assert_eq!(live.len(), 1, "背压 attachment 不应阻塞或影响 PTY 转发");
    }

    const CHUNK: u64 = 8 * 1024;

    #[test]
    fn flow_passes_output_through_while_the_client_keeps_up() {
        let mut flow = FlowControl::with_keyframe(1000);
        for _ in 0..1000 {
            assert!(flow.admit(CHUNK));
            assert_eq!(flow.acknowledge(flow.sent), None);
        }
        assert!(!flow.coalescing);
    }

    #[test]
    fn flow_coalesces_past_the_window_and_resumes_with_the_screen() {
        let mut flow = FlowControl::with_keyframe(0);
        let mut admitted = 0;
        while flow.admit(CHUNK) {
            admitted += CHUNK;
        }
        assert!(admitted <= FLOW_WINDOW_BYTES, "在途不能超过窗口");
        assert!(flow.coalescing);
        assert!(!flow.admit(1), "合并期间实时输出一律不发");

        assert_eq!(flow.acknowledge(admitted - FLOW_RESUME_BYTES - 1), None);
        assert_eq!(
            flow.acknowledge(admitted - FLOW_RESUME_BYTES),
            Some(KeyframeNeed::Screen),
            "追到只剩少量在途就补当前屏幕"
        );
        flow.keyframe_sent(4096, KeyframeNeed::Screen);
        assert!(flow.admit(CHUNK), "补完画面接着发实时输出");
    }

    #[test]
    fn flow_fills_history_once_the_client_has_fully_caught_up() {
        let mut flow = FlowControl::with_keyframe(0);
        while flow.admit(CHUNK) {}
        flow.acknowledge(flow.sent);
        flow.keyframe_sent(4096, KeyframeNeed::Screen);
        assert!(flow.admit(CHUNK));
        assert_eq!(flow.acknowledge(flow.sent - 1), None, "还有在途就先不补");
        assert_eq!(flow.acknowledge(flow.sent), Some(KeyframeNeed::Full));
        flow.keyframe_sent(1024 * 1024, KeyframeNeed::Full);
        assert_eq!(flow.acknowledge(flow.sent), None, "补过一次就不再补");
    }

    #[test]
    fn a_large_keyframe_in_flight_does_not_push_live_output_into_coalescing() {
        let mut flow = FlowControl::with_keyframe(2 * FLOW_WINDOW_BYTES);
        let mut admitted = 0;
        while admitted < FLOW_WINDOW_BYTES - CHUNK {
            assert!(flow.admit(CHUNK), "关键帧本身不占窗口");
            admitted += CHUNK;
        }
    }

    #[test]
    fn coalescing_always_has_something_left_to_acknowledge() {
        let mut flow = FlowControl::with_keyframe(0);
        flow.acknowledge(flow.sent);
        assert!(
            flow.admit(FLOW_WINDOW_BYTES * 2),
            "在途为零时超窗的一段也要发，否则不会再有确认来结束合并"
        );
    }
}
