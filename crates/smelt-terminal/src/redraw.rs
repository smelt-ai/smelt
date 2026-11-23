//! 读线程到 UI 的重绘唤醒：一对一，连着来的合并成一次。
//!
//! 不用 channel：它唤醒等待方时攥着自己内部的锁（`event-listener` 在锁里调 `Waker::wake`），
//! 而等待方一醒来就要拿这把锁。读线程恰好停在那一下，主线程就跟着等。这里只有原子量，
//! [`AtomicWaker`] 先取出 waker 再调它。

use std::future::poll_fn;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use atomic_waker::AtomicWaker;

#[derive(Default)]
struct Shared {
    pending: AtomicBool,
    closed: AtomicBool,
    waker: AtomicWaker,
}

impl Shared {
    /// `Some` 是有一次唤醒，`None` 是已经关闭。
    fn take(&self) -> Poll<Option<()>> {
        // 先读 closed：关闭发生在最后一次 send 之后，看见关闭就一定看得见那次 send。
        let closed = self.closed.load(Ordering::Acquire);
        if self.pending.swap(false, Ordering::AcqRel) {
            Poll::Ready(Some(()))
        } else if closed {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

/// 读线程那头。丢掉就是关闭。
pub(crate) struct RedrawSender(Arc<Shared>);

/// UI 那头。[`AtomicWaker`] 只记一个等待方，所以不能 clone。
pub(crate) struct RedrawReceiver(Arc<Shared>);

pub(crate) fn channel() -> (RedrawSender, RedrawReceiver) {
    let shared = Arc::new(Shared::default());
    (RedrawSender(shared.clone()), RedrawReceiver(shared))
}

impl RedrawSender {
    pub(crate) fn send(&self) {
        // 上一次还没被取走就不再叫：取走它的那次会看到现在的内容。
        if !self.0.pending.swap(true, Ordering::AcqRel) {
            self.0.waker.wake();
        }
    }
}

impl Drop for RedrawSender {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.waker.wake();
    }
}

impl RedrawReceiver {
    /// 等下一次唤醒。还没取走的先交出来，之后关闭了是 `None`。
    pub(crate) async fn recv(&mut self) -> Option<()> {
        poll_fn(|cx| match self.0.take() {
            Poll::Pending => {
                self.0.waker.register(cx.waker());
                // 登记期间来的 send 可能没叫到人，登记完再看一次。
                self.0.take()
            }
            ready => ready,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn no_wake_is_lost() {
        let (tx, mut rx) = channel();
        let (ack, acked) = mpsc::channel();
        let consumer = std::thread::spawn(move || {
            smol::block_on(async {
                while rx.recv().await.is_some() {
                    ack.send(()).unwrap();
                }
            })
        });
        // 一来一回，让 send 反复撞上「正在登记」；丢一次就等不到回音。
        for round in 0..100_000 {
            tx.send();
            acked
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("第 {round} 次唤醒丢了"));
        }
        drop(tx);
        consumer.join().unwrap();
    }

    #[test]
    fn bursts_coalesce_and_are_delivered_before_close() {
        let (tx, mut rx) = channel();
        tx.send();
        tx.send();
        drop(tx);
        smol::block_on(async {
            assert_eq!(rx.recv().await, Some(()));
            assert_eq!(rx.recv().await, None);
        });
    }
}
