//! 子进程报「准备好了」的一次性信号。
//!
//! 父进程拉起子进程时递给它一条管道的写端；子进程准备好（smeltd 是绑好控制 socket）就写
//! 一个字节。父进程阻塞读：读到字节是就绪，读到 EOF 是子进程没就绪就退出了（写端随进程
//! 关掉），不用再隔一会儿去试连、等到超时才知道。
//!
//! 写端经 [`READY_ENV`] 告诉子进程：Unix 是 fd 号，Windows 是句柄值。只有被拉起的那一个
//! 子进程拿得到：Unix 在 fork 之后的子进程里才去掉 CLOEXEC；Windows 只能把句柄标成可继承，
//! 父进程这段时间里别的线程同时拉起的进程也会带上它，那种进程活着时 EOF 会晚到，就绪的
//! 字节照样收得到，最坏等到调用方给的上限。

use std::io::{self, Read as _, Write as _};
use std::process::Command;
use std::time::Duration;

/// 递写端用的环境变量。
pub const READY_ENV: &str = "SMELT_READY_PIPE";

/// 等子进程就绪的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// 子进程写来了就绪的字节。
    Ready,
    /// 子进程没报就绪，写端已经全关了：它退出了。
    Exited,
    /// 到了上限还没有结果。
    TimedOut,
}

/// 父进程这一侧：先 [`ReadyPipe::attach`] 到要拉起的命令上，拉起之后 [`ReadyPipe::wait`]。
pub struct ReadyPipe {
    reader: io::PipeReader,
    writer: io::PipeWriter,
}

impl ReadyPipe {
    /// 建一条管道，让 `command` 拉起的子进程继承写端，并把写端告诉它。
    pub fn attach(command: &mut Command) -> io::Result<Self> {
        let (reader, writer) = io::pipe()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::process::CommandExt as _;
            let fd = writer.as_raw_fd();
            command.env(READY_ENV, fd.to_string());
            // SAFETY: 闭包在 fork 之后、exec 之前的子进程里跑，只调异步信号安全的 fcntl；
            // 父进程里的这个 fd 仍带 CLOEXEC，同时拉起的别的子进程拿不到写端。
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
            let handle = writer.as_raw_handle();
            // CreateProcess 只把可继承的句柄带给子进程。
            // SAFETY: handle 是 writer 持有的有效句柄。
            let inheritable =
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
            if inheritable == 0 {
                return Err(io::Error::last_os_error());
            }
            command.env(READY_ENV, (handle as usize).to_string());
        }
        Ok(Self { reader, writer })
    }

    /// 子进程拉起之后调：等它报就绪，最多等 `timeout`。
    ///
    /// 先放掉父进程手里的写端，否则子进程退了也读不到 EOF。管道读不能带超时（Windows 的
    /// 匿名管道也不支持），交给一条线程去读，这边按上限等结果；超时后那条线程留着，子进程
    /// 写字节或退出时它自己结束。
    pub fn wait(self, timeout: Duration) -> io::Result<Readiness> {
        let Self { mut reader, writer } = self;
        drop(writer);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("smelt-ready-pipe".into())
            .spawn(move || {
                let mut byte = [0u8; 1];
                let _ = tx.send(reader.read_exact(&mut byte).is_ok());
            })?;
        Ok(match rx.recv_timeout(timeout) {
            Ok(true) => Readiness::Ready,
            Ok(false) => Readiness::Exited,
            Err(_) => Readiness::TimedOut,
        })
    }
}

/// 子进程这一侧：父进程递来的写端。
pub struct ReadyNotifier(std::fs::File);

impl ReadyNotifier {
    /// 取出父进程递来的写端，并从环境里删掉变量；不是这样被拉起的就是 `None`。写端随即标回
    /// 不可继承：之后拉起的子进程（以及交接 exec 出来的新映像）不该拿着它，否则这个进程没
    /// 就绪就退出时父进程等不到 EOF；变量删掉，免得子进程把同一个号码当成自己的写端。
    ///
    /// # Safety
    ///
    /// 删环境变量不是线程安全的：调用时不能有别的线程在读写环境变量。
    pub unsafe fn take_from_env() -> Option<Self> {
        let value = std::env::var(READY_ENV).ok();
        // SAFETY: 调用方保证没有别的线程在碰环境变量。
        unsafe { std::env::remove_var(READY_ENV) };
        let value = value?;
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd as _;
            let fd: std::os::fd::RawFd = value.parse().ok()?;
            // 号码不是一个打开着的 fd（变量是伪造或残留的）就不接：F_SETFD 会失败。
            // SAFETY: 只改这个 fd 的 CLOEXEC 标志。
            if fd < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return None;
            }
            // SAFETY: 父进程递来的写端，这个进程里只有这里接管它。
            Some(Self(unsafe { std::fs::File::from_raw_fd(fd) }))
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::FromRawHandle as _;
            use windows_sys::Win32::Foundation::{
                HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
            };
            let handle = value.parse::<usize>().ok()? as HANDLE;
            // SAFETY: 只改这个句柄的继承标志；句柄无效时调用失败，不接。
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                return None;
            }
            // SAFETY: 父进程递来的写端，这个进程里只有这里接管它。
            Some(Self(unsafe { std::fs::File::from_raw_handle(handle) }))
        }
    }

    /// 告诉父进程准备好了，随即关掉写端。
    pub fn notify(mut self) {
        let _ = self.0.write_all(&[1]);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// 子进程写了字节就是就绪；没写就退出，父进程立刻读到 EOF，不等上限。
    #[test]
    fn ready_byte_and_early_exit_are_told_apart() {
        let script = format!("printf x >&\"${READY_ENV}\"");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(&script);
        let pipe = ReadyPipe::attach(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        assert_eq!(
            pipe.wait(Duration::from_secs(60)).unwrap(),
            Readiness::Ready
        );
        child.wait().unwrap();

        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("exit 3");
        let pipe = ReadyPipe::attach(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        assert_eq!(
            pipe.wait(Duration::from_secs(60)).unwrap(),
            Readiness::Exited
        );
        child.wait().unwrap();
    }
}
