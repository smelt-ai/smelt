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
        let mut child = cmd
            .spawn()
            .map_err(|err| format!("启动终端命令失败：{err}"))?;
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
        let id = uuid::Uuid::new_v4().to_string();

        spawn_reader(
            stdout,
            Arc::clone(&output),
            id.clone(),
            event_tx.clone(),
            Arc::clone(&exit),
        );
        spawn_reader(
            stderr,
            Arc::clone(&output),
            id.clone(),
            event_tx.clone(),
            Arc::clone(&exit),
        );

        let wait_output = Arc::clone(&output);
        let wait_exit = Arc::clone(&exit);
        let wait_signal = Arc::clone(&exit_signal);
        let wait_id = id.clone();
        std::thread::Builder::new()
            .name(format!("smelt-acp-term-wait-{}", &id[..id.len().min(8)]))
            .spawn(move || {
                let status = child.wait();
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
) {
    let _ = std::thread::Builder::new()
        .name(format!(
            "smelt-acp-term-read-{}",
            &terminal_id[..terminal_id.len().min(8)]
        ))
        .spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]);
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
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
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
    fn create_echo_captures_output_and_exit() {
        let terminals = AcpTerminals::new();
        let (tx, _rx) = smol::channel::unbounded();
        let id = terminals
            .create(
                "/bin/echo".into(),
                vec!["hello-acp".into()],
                Vec::new(),
                None,
                None,
                tx,
            )
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
        let id = terminals
            .create(
                "/bin/sleep".into(),
                vec!["30".into()],
                Vec::new(),
                None,
                None,
                tx,
            )
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
