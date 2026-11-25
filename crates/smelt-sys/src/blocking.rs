//! 共享异步与阻塞 IO 调度池：所有会等在内核上的工作（socket RPC、文件/SQLite IO、PTY 打开、
//! 子进程、HTTP 下载）走这里，避免把 GPUI 的 `BackgroundExecutor` 占满而饿死异步定时器与调度器。
//!
//! 内部懒建一个 Tokio 运行时：1 条 reactor 线程驱动异步 IO（平时睡在 kqueue/epoll/IOCP
//! 上），blocking 线程池按需扩缩（平时睡在 Condvar 上并在空闲 30 秒后回收，零轮询）。

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(64)
            .thread_keep_alive(Duration::from_secs(30))
            .thread_name("smelt-io")
            .enable_all()
            .build()
            .expect("smelt blocking IO runtime")
    })
}

/// 在专用阻塞池里跑一个同步闭包，返回标准执行器无关的 [`tokio::task::JoinHandle`]。
pub fn spawn<F, T>(f: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    runtime().spawn_blocking(f)
}

/// 丢进专用阻塞池后台跑完即可的同步闭包（不需要等结果），零排队占主线程或 GPUI 后台池。
pub fn spawn_detached<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    let _ = runtime().spawn_blocking(f);
}

/// 在 GPUI / 任意 async 任务里挂起等待同步阻塞闭包完成，期间不占用调用方执行器线程。
pub async fn run<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match spawn(f).await {
        Ok(value) => value,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("blocking IO task failed: {error}"),
    }
}

/// 直接把依赖 Tokio reactor 的 async future 交给共用 Tokio 运行时驱动并等待结果，
/// 捕获内部 panic 转成 `Err`，等待 IO 期间零 OS 线程阻塞。
pub async fn spawn_tokio<F, T>(fut: F) -> anyhow::Result<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    match runtime().spawn(fut).await {
        Ok(value) => Ok(value),
        Err(error) if error.is_panic() => {
            let payload = error.into_panic();
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "the background task panicked".to_string());
            Err(anyhow::anyhow!("后台任务崩溃（已拦截）：{msg}"))
        }
        Err(error) => Err(anyhow::anyhow!("后台任务被取消：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use super::*;

    fn test_block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime")
            .block_on(fut)
    }

    #[test]
    fn run_executes_off_calling_thread_and_returns_value() {
        let caller = std::thread::current().id();
        let worker = test_block_on(run(move || (std::thread::current().id(), 40 + 2)));
        assert_ne!(worker.0, caller);
        assert_eq!(worker.1, 42);
    }

    #[test]
    fn spawn_tokio_drives_tokio_timer_and_catches_panics() {
        let value = test_block_on(spawn_tokio(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            99
        }))
        .expect("shared Tokio runtime should drive future");
        assert_eq!(value, 99);

        let err = test_block_on(spawn_tokio(async {
            panic!("boom");
        }))
        .unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn blocking_pool_runs_concurrent_tasks_in_parallel() {
        const COUNT: usize = 12;
        let barrier = Arc::new(Barrier::new(COUNT));
        let started = Instant::now();
        let handles: Vec<_> = (0..COUNT)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                spawn(move || {
                    barrier.wait();
                    std::thread::sleep(Duration::from_millis(40));
                    i
                })
            })
            .collect();
        let results: Vec<_> = test_block_on(async {
            let mut out = Vec::with_capacity(handles.len());
            for handle in handles {
                out.push(handle.await.unwrap());
            }
            out
        });
        assert_eq!(results, (0..COUNT).collect::<Vec<_>>());
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "expected parallel blocking pool execution, took {:?}",
            started.elapsed()
        );
    }
}
