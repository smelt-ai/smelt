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
const TERMINAL_OUTPUT_QUEUE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// 优雅关闭的冲刷上限。客户端不读时不能无限占住写线程与 fd。
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

/// 一个客户端输出端。`shutdown` 是写线程 socket 的 duplicate，连接移除时用它同时
/// 唤醒可能阻塞的写线程与 handle_open/handle_watch 的读循环。
pub(crate) struct OutputAttachment {
    pub(crate) fd: RawFd,
    mailbox: Arc<OutputMailbox>,
    shutdown: UnixStream,
}

impl OutputAttachment {
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

    pub(crate) fn close(&self) {
        self.mailbox.close();
        let _ = self.shutdown.shutdown(Shutdown::Both);
    }

    /// 终结会话时用：先让已入队的终态快照写出去，再断开。硬 `close` 会把队列
    /// 直接清空，客户端只看到一个裸 EOF，无法区分「会话被删」和「传输断了」，
    /// 于是按后者重连，把刚删掉的会话原地复活。
    ///
    /// 冲刷有界：客户端不消费时，看门狗到点强制关闭，不会让写线程和 fd 永久挂住。
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
    for stream in streams {
        if stream.enqueue(bytes) {
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
        };
        let live = enqueue_session_streams(vec![attachment], b"x", "test", "attachment");
        assert_eq!(live.len(), 1, "背压 attachment 不应阻塞或影响 PTY 转发");
    }
}
