//! ACP `terminal/*`：client 侧持有的后台命令。
//!
//! 协议要求声明 `clientCapabilities.terminal = true` 之后必须实现 create /
//! output / wait_for_exit / kill / release。命令由本进程拉起，输出留在这里，
//! agent 用 id 查询；对话里的 tool_call 通过 `type: terminal` 引用同一份输出。

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use crate::fs::FileSystem;

use agent_client_protocol::schema::v1::TerminalExitStatus;

const DEFAULT_OUTPUT_BYTE_LIMIT: usize = 1024 * 1024;

/// 进程退出后，最多再等这么久让读线程把管道里剩下的输出读完。正常情况下读到 EOF
/// 就立刻往下走；只有后台孙进程继承了 stdout/stderr 一直不关时才会等满。
const OUTPUT_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// stdout、stderr 两个读线程读到 EOF 后各记一次。
const READER_COUNT: usize = 2;

#[derive(Clone, Debug, Default)]
pub struct TerminalSnapshot {
    pub output: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

struct OutputBuf {
    text: String,
    truncated: bool,
    byte_limit: usize,
}

impl OutputBuf {
    fn append(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.text.push_str(chunk);
        if self.text.len() <= self.byte_limit {
            return;
        }
        let overflow = self.text.len() - self.byte_limit;
        let trim_at = self
            .text
            .char_indices()
            .find(|(ix, _)| *ix >= overflow)
            .map(|(ix, _)| ix)
            .unwrap_or(overflow.min(self.text.len()));
        self.text.drain(..trim_at);
        self.truncated = true;
    }
}

struct LiveTerminal {
    output: Arc<Mutex<OutputBuf>>,
    exit: Arc<Mutex<Option<TerminalExitStatus>>>,
    exit_signal: Arc<(Mutex<bool>, Condvar)>,
    pid: u32,
}

/// 一个 ACP 连接上的全部 client 侧终端。连接结束时 drop，杀掉还活着的进程。
pub struct AcpTerminals {
    inner: Mutex<HashMap<String, LiveTerminal>>,
}

impl Default for AcpTerminals {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl Drop for AcpTerminals {
    fn drop(&mut self) {
        let pids: Vec<u32> = self
            .inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .values()
            .map(|term| term.pid)
            .collect();
        for pid in pids {
            kill_pid(pid);
        }
    }
}

impl AcpTerminals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &self,
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<PathBuf>,
        output_byte_limit: Option<u64>,
        event_tx: smol::channel::Sender<(String, TerminalSnapshot)>,
    ) -> Result<String, String> {
        let mut cmd = Command::new(&command);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::login_env::apply_login_environment(&mut cmd);
        for (name, value) in env {
            cmd.env(name, value);
        }
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(
                smelt_sys::process::CREATE_SUSPENDED | smelt_sys::process::CREATE_NO_WINDOW,
            );
        }
        let mut child = cmd
            .spawn()
            .map_err(|err| format!("启动终端命令失败：{err}"))?;
        #[cfg(windows)]
        if let Err(error) = smelt_sys::process::attach_suspended(child.id()) {
            let _ = child.wait();
            return Err(format!("无法把终端命令放进进程树：{error}"));
        }
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "终端 stdout 不可用".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "终端 stderr 不可用".to_string())?;

        let byte_limit = output_byte_limit
            .and_then(|limit| usize::try_from(limit).ok())
            .filter(|limit| *limit > 0)
            .unwrap_or(DEFAULT_OUTPUT_BYTE_LIMIT);
        let output = Arc::new(Mutex::new(OutputBuf {
            text: String::new(),
            truncated: false,
            byte_limit,
        }));
        let exit = Arc::new(Mutex::new(None));
        let exit_signal = Arc::new((Mutex::new(false), Condvar::new()));
        let readers_done = Arc::new((Mutex::new(0usize), Condvar::new()));
        let id = uuid::Uuid::new_v4().to_string();

        spawn_reader(
            stdout,
            Arc::clone(&output),
            id.clone(),
            event_tx.clone(),
            Arc::clone(&exit),
            Arc::clone(&readers_done),
        );
        spawn_reader(
            stderr,
            Arc::clone(&output),
            id.clone(),
            event_tx.clone(),
            Arc::clone(&exit),
            Arc::clone(&readers_done),
        );

        let wait_output = Arc::clone(&output);
        let wait_exit = Arc::clone(&exit);
        let wait_signal = Arc::clone(&exit_signal);
        let wait_id = id.clone();
        let readers_done = Arc::clone(&readers_done);
        std::thread::Builder::new()
            .name(format!("smelt-acp-term-wait-{}", &id[..id.len().min(8)]))
            .spawn(move || {
                let status = child.wait();
                smelt_sys::process::release_tree(pid);
                // 退出码先到，管道里可能还有没读完的输出。等读线程读到 EOF 再定稿，
                // 否则 `wait_for_exit` 和退出事件会丢掉最后一段。
                {
                    let (lock, cvar) = &*readers_done;
                    let done = lock.lock().unwrap_or_else(|err| err.into_inner());
                    let _ = cvar
                        .wait_timeout_while(done, OUTPUT_DRAIN_GRACE, |done| *done < READER_COUNT)
                        .unwrap_or_else(|err| err.into_inner());
                }
                let (exit_code, signal) = match status {
                    Ok(status) => unix_exit(status),
                    Err(_) => (None, Some("wait-failed".into())),
                };
                let snapshot = {
                    let buf = wait_output.lock().unwrap_or_else(|err| err.into_inner());
                    let mut proto = TerminalExitStatus::new();
                    proto.exit_code = exit_code.and_then(|code| u32::try_from(code).ok());
                    proto.signal = signal.clone();
                    *wait_exit.lock().unwrap_or_else(|err| err.into_inner()) = Some(proto);
                    TerminalSnapshot {
                        output: buf.text.clone(),
                        truncated: buf.truncated,
                        exit_code,
                        signal,
                    }
                };
                {
                    let (lock, cvar) = &*wait_signal;
                    *lock.lock().unwrap_or_else(|err| err.into_inner()) = true;
                    cvar.notify_all();
                }
                let _ = event_tx.try_send((wait_id, snapshot));
            })
            .map_err(|err| format!("启动终端等待线程失败：{err}"))?;

        self.inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(
                id.clone(),
                LiveTerminal {
                    output,
                    exit,
                    exit_signal,
                    pid,
                },
            );
        Ok(id)
    }

    pub fn snapshot(&self, id: &str) -> Option<TerminalSnapshot> {
        let inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        let term = inner.get(id)?;
        let buf = term.output.lock().unwrap_or_else(|err| err.into_inner());
        let exit = term.exit.lock().unwrap_or_else(|err| err.into_inner());
        Some(TerminalSnapshot {
            output: buf.text.clone(),
            truncated: buf.truncated,
            exit_code: exit
                .as_ref()
                .and_then(|status| status.exit_code)
                .and_then(|code| i32::try_from(code).ok()),
            signal: exit.as_ref().and_then(|status| status.signal.clone()),
        })
    }

    pub fn proto_exit(&self, id: &str) -> Option<TerminalExitStatus> {
        let inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        inner
            .get(id)?
            .exit
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    /// 阻塞等到进程退出。调用方必须在 JSON-RPC handler 之外跑，避免卡住事件循环。
    pub fn wait(&self, id: &str) -> Result<TerminalExitStatus, String> {
        let term = {
            let inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
            inner
                .get(id)
                .ok_or_else(|| format!("终端不存在：{id}"))?
                .exit_signal
                .clone()
        };
        let (lock, cvar) = &*term;
        let mut done = lock.lock().unwrap_or_else(|err| err.into_inner());
        while !*done {
            done = cvar.wait(done).unwrap_or_else(|err| err.into_inner());
        }
        self.proto_exit(id)
            .ok_or_else(|| format!("终端退出状态丢失：{id}"))
    }

    pub fn kill(&self, id: &str) -> Result<(), String> {
        let pid = {
            let inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
            inner
                .get(id)
                .ok_or_else(|| format!("终端不存在：{id}"))?
                .pid
        };
        kill_pid(pid);
        Ok(())
    }

    pub fn release(&self, id: &str) -> Result<(), String> {
        let term = {
            let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
            inner
                .remove(id)
                .ok_or_else(|| format!("终端不存在：{id}"))?
        };
        if term
            .exit
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .is_none()
        {
            kill_pid(term.pid);
        }
        Ok(())
    }
}

fn spawn_reader(
    mut pipe: impl Read + Send + 'static,
    output: Arc<Mutex<OutputBuf>>,
    terminal_id: String,
    event_tx: smol::channel::Sender<(String, TerminalSnapshot)>,
    exit: Arc<Mutex<Option<TerminalExitStatus>>>,
    done: Arc<(Mutex<usize>, Condvar)>,
) {
    let _ = std::thread::Builder::new()
        .name(format!(
            "smelt-acp-term-read-{}",
            &terminal_id[..terminal_id.len().min(8)]
        ))
        .spawn(move || {
            let mut buf = [0u8; 4096];
            let mut decoder = Utf8Stream::default();
            loop {
                let chunk = match pipe.read(&mut buf) {
                    Ok(0) => decoder.finish(),
                    Ok(n) => decoder.push(&buf[..n]),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => decoder.finish(),
                };
                if !chunk.is_empty() {
                    let snapshot = {
                        let mut guard = output.lock().unwrap_or_else(|err| err.into_inner());
                        guard.append(&chunk);
                        let exit = exit.lock().unwrap_or_else(|err| err.into_inner());
                        TerminalSnapshot {
                            output: guard.text.clone(),
                            truncated: guard.truncated,
                            exit_code: exit
                                .as_ref()
                                .and_then(|status| status.exit_code)
                                .and_then(|code| i32::try_from(code).ok()),
                            signal: exit.as_ref().and_then(|status| status.signal.clone()),
                        }
                    };
                    let _ = event_tx.try_send((terminal_id.clone(), snapshot));
                }
                if decoder.is_finished() {
                    break;
                }
            }
            let (lock, cvar) = &*done;
            *lock.lock().unwrap_or_else(|err| err.into_inner()) += 1;
            cvar.notify_all();
        });
}

/// 把分块读到的字节按 UTF-8 解成文本。管道按字节切块，一个多字节字符（中文、emoji）
/// 可能被切在两块之间；逐块 `from_utf8_lossy` 会把两半各自变成「�」。这里把块尾不完整的
/// 字节留到下一块再解，真正非法的字节才替换成 U+FFFD。
#[derive(Default)]
struct Utf8Stream {
    pending: Vec<u8>,
    finished: bool,
}

impl Utf8Stream {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    rest = &[];
                    break;
                }
                Err(err) => {
                    let (valid, after) = rest.split_at(err.valid_up_to());
                    // SAFETY: `valid_up_to` 之前的字节已校验为合法 UTF-8。
                    out.push_str(unsafe { std::str::from_utf8_unchecked(valid) });
                    match err.error_len() {
                        // 块尾是一个还没收全的字符：留到下一块。
                        None => {
                            rest = after;
                            break;
                        }
                        Some(len) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[len..];
                        }
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        out
    }

    /// 输入结束：剩下的半个字符再也等不到后半，按非法字节处理。
    fn finish(&mut self) -> String {
        self.finished = true;
        let rest = std::mem::take(&mut self.pending);
        String::from_utf8_lossy(&rest).into_owned()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

fn unix_exit(status: std::process::ExitStatus) -> (Option<i32>, Option<String>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return (None, Some(signal.to_string()));
        }
    }
    (status.code(), None)
}

fn kill_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        // 创建时用了 process_group(0)，负 pid 杀掉整组。
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = smelt_sys::process::signal_tree(pid, smelt_sys::process::TreeSignal::Interrupt);
    }
}

pub fn read_text_file(
    path: &std::path::Path,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("fs/read_text_file 的路径必须是绝对路径".into());
    }
    let content = crate::fs::LocalFs
        .read_to_string(path)
        .map_err(|err| format!("读取失败：{err}"))?;
    Ok(slice_lines(&content, line, limit))
}

pub fn write_text_file(path: &std::path::Path, content: &str) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("fs/write_text_file 的路径必须是绝对路径".into());
    }
    crate::fs::LocalFs
        .write(path, content)
        .map_err(|err| format!("写入失败：{err}"))
}

pub fn slice_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    let start = line.unwrap_or(1).max(1).saturating_sub(1) as usize;
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    if start >= lines.len() {
        return String::new();
    }
    let end = match limit {
        Some(limit) => start.saturating_add(limit as usize).min(lines.len()),
        None => lines.len(),
    };
    lines[start..end].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn slice_lines_is_one_based_and_respects_limit() {
        let text = "a\nb\nc\n";
        assert_eq!(slice_lines(text, Some(2), Some(1)), "b\n");
        assert_eq!(slice_lines(text, Some(1), None), "a\nb\nc\n");
        assert_eq!(slice_lines(text, Some(9), Some(1)), "");
    }

    #[test]
    fn output_buf_truncates_from_the_front_on_char_boundary() {
        let mut buf = OutputBuf {
            text: String::new(),
            truncated: false,
            byte_limit: 4,
        };
        buf.append("你好世界");
        assert!(buf.truncated);
        assert!(buf.text.len() <= 4);
        assert!(std::str::from_utf8(buf.text.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_relative_fs_paths() {
        assert!(read_text_file(std::path::Path::new("rel.txt"), None, None).is_err());
        assert!(write_text_file(std::path::Path::new("rel.txt"), "x").is_err());
    }

    #[test]
    fn utf8_split_across_reads_is_reassembled() {
        let text = "中文🙂ok";
        let bytes = text.as_bytes();
        // 每一个切点都试一遍：任何位置切开，拼回来都必须和原文一致。
        for cut in 0..=bytes.len() {
            let mut stream = Utf8Stream::default();
            let mut out = stream.push(&bytes[..cut]);
            out.push_str(&stream.push(&bytes[cut..]));
            out.push_str(&stream.finish());
            assert_eq!(out, text, "切在第 {cut} 字节");
        }
    }

    #[test]
    fn invalid_bytes_become_replacement_characters_without_stalling() {
        let mut stream = Utf8Stream::default();
        // 0xFF 永远不可能是合法 UTF-8：立刻替换，不能当成"没收全"一直攒着。
        assert_eq!(stream.push(b"a\xFFb"), "a\u{FFFD}b");
        // 结尾只收到半个汉字就结束了：结束时按非法字节输出。
        assert_eq!(stream.push(&"中".as_bytes()[..2]), "");
        assert_eq!(stream.finish(), "\u{FFFD}");
    }

    /// 走真实读线程：管道每次只给几个字节，汉字被切开，输出里不能出现「�」。
    #[test]
    fn reader_thread_keeps_multibyte_characters_split_between_reads() {
        struct Trickle(Vec<u8>, usize);
        impl Read for Trickle {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let remaining = &self.0[self.1..];
                let n = remaining.len().min(2).min(buf.len());
                buf[..n].copy_from_slice(&remaining[..n]);
                self.1 += n;
                Ok(n)
            }
        }
        let output = Arc::new(Mutex::new(OutputBuf {
            text: String::new(),
            truncated: false,
            byte_limit: DEFAULT_OUTPUT_BYTE_LIMIT,
        }));
        let done = Arc::new((Mutex::new(0usize), Condvar::new()));
        let (tx, _rx) = smol::channel::unbounded();
        spawn_reader(
            Trickle("你好，世界".as_bytes().to_vec(), 0),
            Arc::clone(&output),
            "trickle".into(),
            tx,
            Arc::new(Mutex::new(None)),
            Arc::clone(&done),
        );
        let (lock, cvar) = &*done;
        let finished = cvar
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |done| {
                *done < 1
            })
            .unwrap();
        assert_eq!(*finished.0, 1, "读线程应读到 EOF 后结束");
        assert_eq!(output.lock().unwrap().text, "你好，世界");
    }

    /// 退出时管道里还有没读完的输出：`wait` 返回、退出事件发出之前必须读完。
    /// 读线程每读一块都要克隆整份缓冲（最多 1MB），追不上进程退出——以前等待线程
    /// 一拿到退出码就取快照，最后那段（这里是 `END`）常常丢掉。
    #[cfg(unix)]
    #[test]
    fn exit_waits_for_the_output_still_in_the_pipe() {
        let terminals = AcpTerminals::new();
        let (tx, rx) = smol::channel::unbounded();
        let id = terminals
            .create(
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    "head -c 2000000 /dev/zero | tr '\\0' a; echo; echo END".into(),
                ],
                Vec::new(),
                None,
                None,
                tx,
            )
            .expect("create");
        let status = terminals.wait(&id).expect("wait");
        assert_eq!(status.exit_code, Some(0));
        let snapshot = terminals.snapshot(&id).expect("snapshot");
        assert!(snapshot.truncated, "输出超过 1MB，应从头截断");
        assert!(
            snapshot.output.ends_with("END\n"),
            "wait 返回时最后一段输出必须已经读进来"
        );
        let exit_event = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|(_, snapshot)| snapshot)
            .find(|snapshot| snapshot.exit_code.is_some())
            .expect("退出事件");
        assert!(
            exit_event.output.ends_with("END\n"),
            "带退出码的那条事件也要带上完整输出"
        );
        terminals.release(&id).expect("release");
    }

    /// 后台孙进程继承了 stdout、一直不关：不能因为等 EOF 就永远不报告退出。
    #[cfg(unix)]
    #[test]
    fn exit_is_reported_even_if_a_grandchild_keeps_the_pipe_open() {
        let terminals = AcpTerminals::new();
        let (tx, _rx) = smol::channel::unbounded();
        let id = terminals
            .create(
                "/bin/sh".into(),
                vec!["-c".into(), "sleep 30 & echo started".into()],
                Vec::new(),
                None,
                None,
                tx,
            )
            .expect("create");
        let started = std::time::Instant::now();
        let status = terminals.wait(&id).expect("wait");
        assert_eq!(status.exit_code, Some(0));
        assert!(
            started.elapsed() < OUTPUT_DRAIN_GRACE + Duration::from_secs(5),
            "孙进程占着管道时，最多再等一个宽限期就要报告退出"
        );
        assert!(terminals.snapshot(&id).unwrap().output.contains("started"));
        // drop 时按进程组杀掉，sleep 30 一起结束。
    }

    #[test]
    fn create_echo_captures_output_and_exit() {
        let terminals = AcpTerminals::new();
        let (tx, _rx) = smol::channel::unbounded();
        let (command, args) = if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/d".into(), "/c".into(), "echo hello-acp".into()],
            )
        } else {
            ("/bin/echo".to_string(), vec!["hello-acp".into()])
        };
        let id = terminals
            .create(command, args, Vec::new(), None, None, tx)
            .expect("create echo");
        let status = terminals.wait(&id).expect("wait echo");
        assert_eq!(status.exit_code, Some(0));
        let snapshot = terminals.snapshot(&id).expect("snapshot");
        assert!(snapshot.output.contains("hello-acp"));
        terminals.release(&id).expect("release");
    }

    #[test]
    fn kill_long_running_command() {
        let terminals = AcpTerminals::new();
        let (tx, _rx) = smol::channel::unbounded();
        let (command, args) = if cfg!(windows) {
            (
                "ping".to_string(),
                vec!["-n".into(), "30".into(), "127.0.0.1".into()],
            )
        } else {
            ("/bin/sleep".to_string(), vec!["30".into()])
        };
        let id = terminals
            .create(command, args, Vec::new(), None, None, tx)
            .expect("create sleep");
        terminals.kill(&id).expect("kill");
        let started = std::time::Instant::now();
        let status = terminals.wait(&id).expect("wait killed");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            status.exit_code.is_none() || status.exit_code != Some(0) || status.signal.is_some()
        );
        terminals.release(&id).expect("release");
    }
}
