//! 到运行时的一条连接的流类型，各平台一种，协议代码不分叉。
//!
//! 本机连接和远程（iroh 把 QUIC 流接到一对套接字的一端）交出的都是这个类型，上层的
//! 阻塞读写、`try_clone`、读写超时、`shutdown` 对两种传输完全一样。
//!
//! - Unix：`std::os::unix::net::UnixStream` / `UnixListener`。
//! - Windows：系统的 AF_UNIX（Windows 10 1803 起支持），整个自己包，见 [`windows`]：
//!   系统驱动在阻塞调用上会丢唤醒。方法和 std 的一致，`pair()`、`bind`、`incoming`
//!   也有。Tokio 在 Windows 上没有 AF_UNIX，守护的接受循环是同步的，不需要把这条监听
//!   塞进 tokio。

#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use windows::{UnixListener, UnixStream};

pub type RuntimeStream = UnixStream;

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::net::Shutdown;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    use super::{RuntimeStream, UnixListener};

    const ARRIVES: Duration = Duration::from_secs(5);

    /// Windows 的 afunix.sys 有个竞态：读刚挂上时对端恰好写入或关闭，唤醒偶尔丢掉，读一
    /// 直等到超时。每轮让「读」和「对端写完再关」撞在一起，必须先读到数据再读到 EOF。
    /// 一半轮次对端整个关掉，一半只关写方向（半关闭）。
    #[test]
    fn a_read_racing_the_peers_write_and_close_sees_the_bytes_then_eof() {
        for round in 0..1000 {
            let (reader, writer) = RuntimeStream::pair().unwrap();
            reader.set_read_timeout(Some(ARRIVES)).unwrap();
            let start = Arc::new(Barrier::new(2));
            let half_close = round % 2 == 1;
            let peer = {
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    (&writer).write_all(b"x").unwrap();
                    if half_close {
                        writer.shutdown(Shutdown::Write).unwrap();
                        Some(writer)
                    } else {
                        None
                    }
                })
            };
            start.wait();
            let mut received = Vec::new();
            (&reader)
                .read_to_end(&mut received)
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            assert_eq!(received, b"x", "round {round}");
            drop(peer.join().unwrap());
        }
    }

    /// 守护的接受循环：监听先等上，客户端再连，两边都能收发。
    #[test]
    fn a_listener_accepts_a_client_that_connects_after_it_waits() {
        let path = std::env::temp_dir().join(format!(
            "smelt-test-{:016x}.sock",
            uuid::Uuid::new_v4().as_u64_pair().0
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let accepted = listener.incoming().next().unwrap().unwrap();
            accepted.set_read_timeout(Some(ARRIVES)).unwrap();
            let mut byte = [0u8; 1];
            (&accepted).read_exact(&mut byte).unwrap();
            (&accepted).write_all(&byte).unwrap();
        });
        let client = RuntimeStream::connect(&path).unwrap();
        client.set_read_timeout(Some(ARRIVES)).unwrap();
        (&client).write_all(b"y").unwrap();
        let mut echoed = [0u8; 1];
        (&client).read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"y");
        server.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    /// 本地 `shutdown` 要叫醒另一条线程里等着的读，不管它是先等上还是后等上。
    #[test]
    fn local_shutdown_wakes_a_read_waiting_on_a_clone() {
        let (local, _peer) = RuntimeStream::pair().unwrap();
        local.set_read_timeout(Some(ARRIVES)).unwrap();
        let reading = local.try_clone().unwrap();
        let reader = thread::spawn(move || {
            let mut buf = [0u8; 8];
            (&reading).read(&mut buf).map_err(|error| error.kind())
        });
        local.shutdown(Shutdown::Both).unwrap();
        assert_eq!(reader.join().unwrap(), Ok(0));
    }

    #[test]
    fn a_read_with_nothing_to_read_times_out() {
        let (local, _peer) = RuntimeStream::pair().unwrap();
        local
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut buf = [0u8; 8];
        let error = (&local).read(&mut buf).unwrap_err();
        // Unix 报 WouldBlock，Windows 报 WSAETIMEDOUT。
        assert!(
            matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            "{error}"
        );
    }

    #[test]
    fn a_nonblocking_read_with_nothing_to_read_would_block() {
        let (local, _peer) = RuntimeStream::pair().unwrap();
        local.set_nonblocking(true).unwrap();
        let mut buf = [0u8; 8];
        let error = (&local).read(&mut buf).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WouldBlock);
    }

    /// 写满缓冲之后，写要等到对端读走再继续，不能丢字节也不能报错。
    #[test]
    fn a_write_larger_than_the_buffer_waits_for_the_reader() {
        let (writer, reader) = RuntimeStream::pair().unwrap();
        reader.set_read_timeout(Some(ARRIVES)).unwrap();
        writer.set_write_timeout(Some(ARRIVES)).unwrap();
        let payload: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let expected = payload.clone();
        let sender = thread::spawn(move || {
            (&writer).write_all(&payload).unwrap();
        });
        let mut received = Vec::new();
        (&reader).read_to_end(&mut received).unwrap();
        sender.join().unwrap();
        assert_eq!(received.len(), expected.len());
        assert!(received == expected);
    }
}
