//! 进程内的一对流：两端都在本进程里，数据在内存里过，不经过任何内核对象。
//!
//! Windows 的 `RuntimeStream::pair()` 用它（iroh 桥接、测试），Unix 上只为测试编译，好在
//! macOS 上也跑一遍。语义照 socketpair：读写超时、非阻塞在 `try_clone` 的各份之间共享；
//! 本地 `shutdown` 叫醒等着的读写；关掉写方向后对端读完缓冲里的数据再读到 EOF（半关闭，
//! iroh 桥接把远端写完转成它），另一个方向照常；最后一份丢掉等于两个方向都关。每个方向
//! 的缓冲有上限，写满了等对端读走，和套接字的发送缓冲一样给出背压。

// Unix 上只有测试用到它，部分方法只在 Windows 的 `RuntimeStream` 里调用。
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// 每个方向最多缓冲多少字节。
const CAPACITY: usize = 256 * 1024;

pub struct MemoryStream {
    end: Arc<End>,
}

/// 一端。各份克隆共用。
struct End {
    incoming: Arc<Channel>,
    outgoing: Arc<Channel>,
    /// 活着的克隆数。最后一份丢掉时两个方向都关。
    handles: AtomicUsize,
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
    nonblocking: AtomicBool,
}

/// 一个方向。
struct Channel {
    state: Mutex<ChannelState>,
    changed: Condvar,
}

#[derive(Default)]
struct ChannelState {
    buffer: VecDeque<u8>,
    /// 写的一端关了：读完缓冲就是 EOF。
    write_closed: bool,
    /// 读的一端关了：缓冲作废，再写报 BrokenPipe。
    read_closed: bool,
}

impl Channel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ChannelState::default()),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, ChannelState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn close_write(&self) {
        self.lock().write_closed = true;
        self.changed.notify_all();
    }

    fn close_read(&self) {
        {
            let mut state = self.lock();
            state.read_closed = true;
            state.buffer.clear();
        }
        self.changed.notify_all();
    }

    /// 等状态变化。到了期限返回 `None`。
    fn wait<'a>(
        &self,
        state: MutexGuard<'a, ChannelState>,
        deadline: Option<Instant>,
    ) -> Option<MutexGuard<'a, ChannelState>> {
        let Some(deadline) = deadline else {
            return Some(
                self.changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let (state, _) = self
            .changed
            .wait_timeout(state, left)
            .unwrap_or_else(PoisonError::into_inner);
        Some(state)
    }
}

impl MemoryStream {
    pub fn pair() -> (Self, Self) {
        let there = Channel::new();
        let back = Channel::new();
        (
            Self::new(Arc::clone(&back), Arc::clone(&there)),
            Self::new(there, back),
        )
    }

    fn new(incoming: Arc<Channel>, outgoing: Arc<Channel>) -> Self {
        Self {
            end: Arc::new(End {
                incoming,
                outgoing,
                handles: AtomicUsize::new(1),
                read_timeout: Mutex::new(None),
                write_timeout: Mutex::new(None),
                nonblocking: AtomicBool::new(false),
            }),
        }
    }

    pub fn try_clone(&self) -> Self {
        self.end.handles.fetch_add(1, Ordering::Relaxed);
        Self {
            end: Arc::clone(&self.end),
        }
    }

    pub fn shutdown(&self, how: Shutdown) {
        if matches!(how, Shutdown::Read | Shutdown::Both) {
            self.end.incoming.close_read();
        }
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            self.end.outgoing.close_write();
        }
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        *lock(&self.end.read_timeout) = checked_timeout(timeout)?;
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        *lock(&self.end.write_timeout) = checked_timeout(timeout)?;
        Ok(())
    }

    pub fn read_timeout(&self) -> Option<Duration> {
        *lock(&self.end.read_timeout)
    }

    pub fn write_timeout(&self) -> Option<Duration> {
        *lock(&self.end.write_timeout)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) {
        self.end.nonblocking.store(nonblocking, Ordering::SeqCst);
    }

    fn nonblocking(&self) -> bool {
        self.end.nonblocking.load(Ordering::SeqCst)
    }

    /// 读超时从第一次要等时算起，和 SO_RCVTIMEO 一样。
    fn deadline(timeout: &Mutex<Option<Duration>>) -> Option<Instant> {
        lock(timeout).map(|timeout| Instant::now() + timeout)
    }

    pub fn read_bytes(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let channel = &self.end.incoming;
        let mut deadline = None;
        let mut state = channel.lock();
        loop {
            // 和 Unix 一致：本地关了读方向，读到的就是 EOF。
            if state.read_closed {
                return Ok(0);
            }
            if !state.buffer.is_empty() {
                let read = state.buffer.read(buf)?;
                drop(state);
                channel.changed.notify_all();
                return Ok(read);
            }
            if state.write_closed {
                return Ok(0);
            }
            if self.nonblocking() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let until = *deadline.get_or_insert_with(|| Self::deadline(&self.end.read_timeout));
            state = channel.wait(state, until).ok_or_else(timed_out)?;
        }
    }

    pub fn write_bytes(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let channel = &self.end.outgoing;
        let mut deadline = None;
        let mut state = channel.lock();
        loop {
            if state.write_closed || state.read_closed {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let room = CAPACITY.saturating_sub(state.buffer.len());
            if room > 0 {
                let written = room.min(buf.len());
                state.buffer.extend(&buf[..written]);
                drop(state);
                channel.changed.notify_all();
                return Ok(written);
            }
            if self.nonblocking() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let until = *deadline.get_or_insert_with(|| Self::deadline(&self.end.write_timeout));
            state = channel.wait(state, until).ok_or_else(timed_out)?;
        }
    }
}

impl Drop for MemoryStream {
    fn drop(&mut self) {
        if self.end.handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shutdown(Shutdown::Both);
        }
    }
}

impl fmt::Debug for MemoryStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryStream").finish_non_exhaustive()
    }
}

impl Read for &MemoryStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_bytes(buf)
    }
}

impl Write for &MemoryStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "进程内流读写超时")
}

fn checked_timeout(timeout: Option<Duration>) -> io::Result<Option<Duration>> {
    if timeout == Some(Duration::ZERO) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot set a 0 duration timeout",
        ));
    }
    Ok(timeout)
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::net::Shutdown;
    use std::thread;
    use std::time::Duration;

    use super::{CAPACITY, MemoryStream};

    const ARRIVES: Duration = Duration::from_secs(5);

    /// iroh 桥接：远端写完只关本地写方向，回包还要从另一个方向回去。
    #[test]
    fn half_close_delivers_eof_while_the_other_direction_keeps_working() {
        let (bridge, client) = MemoryStream::pair();
        client.set_read_timeout(Some(ARRIVES)).unwrap();
        bridge.set_read_timeout(Some(ARRIVES)).unwrap();
        (&bridge).write_all(b"request").unwrap();
        bridge.shutdown(Shutdown::Write);

        let mut request = Vec::new();
        (&client).read_to_end(&mut request).unwrap();
        assert_eq!(request, b"request");

        (&client).write_all(b"reply").unwrap();
        drop(client);
        let mut reply = Vec::new();
        (&bridge).read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"reply");
    }

    /// 最后一份丢掉才算关：还有克隆活着时对端读不到 EOF。
    #[test]
    fn the_peer_sees_eof_only_after_the_last_clone_is_dropped() {
        let (local, peer) = MemoryStream::pair();
        peer.set_nonblocking(true);
        let clone = local.try_clone();
        drop(local);
        let mut buf = [0u8; 4];
        assert_eq!(
            (&peer).read(&mut buf).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
        drop(clone);
        assert_eq!((&peer).read(&mut buf).unwrap(), 0);
        assert_eq!(
            (&peer).write(b"x").unwrap_err().kind(),
            ErrorKind::BrokenPipe
        );
    }

    /// 本地 shutdown 叫醒另一份克隆上等着的写。
    #[test]
    fn local_shutdown_wakes_a_write_blocked_on_a_full_buffer() {
        let (local, _peer) = MemoryStream::pair();
        (&local).write_all(&vec![0u8; CAPACITY]).unwrap();
        let writing = local.try_clone();
        let writer = thread::spawn(move || (&writing).write(b"x").map_err(|error| error.kind()));
        thread::sleep(Duration::from_millis(50));
        local.shutdown(Shutdown::Both);
        assert_eq!(writer.join().unwrap(), Err(ErrorKind::BrokenPipe));
    }

    #[test]
    fn a_write_to_a_full_buffer_times_out() {
        let (local, _peer) = MemoryStream::pair();
        local
            .set_write_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        (&local).write_all(&vec![0u8; CAPACITY]).unwrap();
        assert_eq!(
            (&local).write(b"x").unwrap_err().kind(),
            ErrorKind::TimedOut
        );
    }

    #[test]
    fn clones_share_settings() {
        let (local, _peer) = MemoryStream::pair();
        let clone = local.try_clone();
        local
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        assert_eq!(clone.read_timeout(), Some(Duration::from_millis(50)));
        local.set_nonblocking(true);
        let mut buf = [0u8; 4];
        assert_eq!(
            (&clone).read(&mut buf).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_zero_timeout_is_rejected_like_std() {
        let (local, _peer) = MemoryStream::pair();
        assert_eq!(
            local
                .set_read_timeout(Some(Duration::ZERO))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
    }
}
