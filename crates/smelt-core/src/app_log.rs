//! 整个 app（GUI `smelt` + 守护 `smeltd`）共用的运行日志：记事件/错误/关键操作，
//! 默认开启（不设开关——没人会记得去打开一个平时看不见的开关），落盘在
//! `~/.smelt/app.log`。
//!
//! 大小有硬上限（[`MAX_LOG_BYTES`]）：写入前先检查文件大小，超限就把当前文件轮转成
//! `app.log.1`（覆盖旧的 `.1`，只保留一代），再继续写新文件——不会无限增长占用户磁盘，
//! 稳态占用 ≤ 2×`MAX_LOG_BYTES`。这跟 smeltd 已有的 `daemon.log`（只记交接/网络这类
//! 守护自身生命周期事件）是两份不同的日志：这份是全 app 通用的，两个进程都写，
//! 且覆盖范围更广（含 GUI 侧事件、panic）。
//!
//! 用法：`app_log::info("git", "commit 成功")`；启动时调一次 [`install_panic_hook`]
//! 把 panic 信息也落盘——GUI 崩溃时用户往往看不到终端输出，这份日志是唯一线索。

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// 单文件上限：2MB 对一份纯文本事件日志足够存下相当长时间的历史，又不至于让
/// 用户觉得占地方。
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// 跨线程/跨调用序列化写入：避免两条日志的内容交错写坏（同进程内多线程都可能调用）。
static LOG_LOCK: Mutex<()> = Mutex::new(());

/// 日志文件路径（`~/.smelt/app.log`）。公开给 UI 用——"反馈问题"要能带用户去
/// Finder 里把这个文件拖进 issue（GitHub 附件靠拖拽，没有 API 能替用户自动上传）。
pub fn log_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("app.log"))
}

fn rotated_path(path: &std::path::Path) -> PathBuf {
    path.with_file_name("app.log.1")
}

/// 记一条运行日志。`level` 建议用 "info"/"warn"/"error"；`scope` 标来源子系统
/// （比如 "git"/"acp"/"daemon"/"panic"），方便排障时按来源过滤。
///
/// 磁盘/权限问题一律静默丢弃——日志本身不能成为新的故障点，绝不能因为写日志失败
/// 而 panic 或拖慢主流程。
pub fn log(level: &str, scope: &str, msg: &str) {
    let Some(path) = log_path() else { return };
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // 写之前检查大小，超限先轮转：当前文件整份改名成 .1（覆盖旧的），
    // 后面 OpenOptions::create 会在原路径重新起一份空文件。
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::rename(&path, rotated_path(&path));
        }
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "[{ts}] [{level}] [{scope}] {msg} (pid={})",
            std::process::id()
        );
    }
}

pub fn info(scope: &str, msg: &str) {
    log("info", scope, msg);
}

pub fn warn(scope: &str, msg: &str) {
    log("warn", scope, msg);
}

pub fn error(scope: &str, msg: &str) {
    log("error", scope, msg);
}

/// 安装 panic hook：在系统默认 hook（打印到 stderr）之外，把 panic 信息也追加写进
/// `app.log`。GUI 进程崩溃时用户的终端通常早就看不到了，没有这个钩子这次崩溃就
/// 彻底没有留痕。`scope` 一般传进程名（"smelt" / "smeltd"），区分是哪个进程崩的。
pub fn install_panic_hook(scope: &'static str) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error(scope, &format!("panic: {info}"));
        default_hook(info);
    }));
}

/// 接管本进程的 stderr（fd 2）：另开一个管道当作新 stderr，后台线程逐行读出来，
/// 一份原样透传回真正的终端（开发时 `cargo run` 照样看得见），一份按行追加进
/// `app.log`。
///
/// 这样代码里已有的、散落在 `main.rs`/`terminal.rs`/`tasks.rs`/`acp_conn.rs` 等
/// 几十处 `eprintln!`（工作区恢复失败、会话/分屏失败、任务失败、hooks 安装失败……）
/// 不用一一改调用点去手写 `app_log::error`，也自动进了这份日志——包括以后新增的
/// `eprintln!`，从根上避免「又漏了一处」。仅 unix 有效（本项目目前只发 macOS）；
/// 拿不到 fd 或 dup2 失败就静默放弃，不影响程序正常跑（stderr 退回默认，只是不
/// 落盘）。
#[cfg(unix)]
pub fn tee_stderr(scope: &'static str) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::FromRawFd;

    let mut fds = [0i32; 2];
    // SAFETY：fds 是本地栈上的合法缓冲区，libc::pipe 只在成功时写入两个有效 fd。
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // 留一份原 stderr 用来继续透传，不然把 fd 2 换成管道写端后，谁都看不见输出了。
    // SAFETY：dup/dup2/close 都是对已知合法 fd 的标准操作，返回值逐一检查。
    let orig_stderr = unsafe { libc::dup(2) };
    if orig_stderr < 0 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return;
    }
    if unsafe { libc::dup2(write_fd, 2) } < 0 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
            libc::close(orig_stderr);
        }
        return;
    }
    unsafe { libc::close(write_fd) };

    // SAFETY：orig_stderr/read_fd 都是刚确认过的有效、独占的 fd，交给 File 管理其
    // 生命周期（后台线程退出时随 File Drop 关闭，不会泄漏）。
    let mut passthrough = unsafe { std::fs::File::from_raw_fd(orig_stderr) };
    let reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
    std::thread::Builder::new()
        .name("app-log-stderr-tee".into())
        .spawn(move || {
            let mut buf_reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                match buf_reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // 写端全部关闭（进程退出）或读错误
                    Ok(_) => {
                        let _ = passthrough.write_all(line.as_bytes());
                        let _ = passthrough.flush();
                        log("stderr", scope, line.trim_end());
                    }
                }
            }
        })
        .ok();
}

#[cfg(not(unix))]
pub fn tee_stderr(_scope: &'static str) {}

#[cfg(test)]
mod tests {
    use super::*;

    // 用环境变量兜底的 HOME 隔离测试，避免真的写用户主目录；串行执行防止相互踩文件。
    fn with_temp_home<F: FnOnce()>(f: F) {
        let dir = tempfile_dir();
        let prev = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &dir) };
        f();
        match prev {
            Some(p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempfile_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "smelt-app-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn writes_and_rotates_on_size_limit() {
        with_temp_home(|| {
            let path = log_path().unwrap();
            info("test", "hello");
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.contains("[info] [test] hello"));

            // 手动把文件撑到超过上限，再写一条应该触发轮转。
            {
                let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
                let filler = vec![b'x'; (MAX_LOG_BYTES + 1) as usize];
                f.write_all(&filler).unwrap();
            }
            warn("test", "after-fill");
            assert!(rotated_path(&path).exists(), "超限应轮转出 app.log.1");
            let new_content = std::fs::read_to_string(&path).unwrap();
            assert!(new_content.contains("after-fill"));
            assert!(
                !new_content.contains("hello"),
                "轮转后新文件不该还带着旧内容"
            );
        });
    }
}
