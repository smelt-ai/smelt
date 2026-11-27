//! `process.run`：在这台机器上跑一个程序直到退出，交回退出码和输出。线上形状见
//! `smelt_runtime_client::protocol::process`。
//!
//! 程序和它拉起的子进程放在一棵进程树里（Unix 进程组、Windows Job），超时整棵杀掉。
//! 环境按登录 shell 铺（从 Finder 启动的运行时 `PATH` 只有系统目录，`node`、`git` 的
//! 钩子都找不到），调用方给的变量叠在上面。

use std::borrow::Cow;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use smelt_runtime_client::protocol::process::{
    PROCESS_OUTPUT_LIMIT, PROCESS_RUN_DEFAULT_TIMEOUT_MS, PROCESS_RUN_MAX_TIMEOUT_MS,
    ProcessRunParams, ProcessRunResult,
};
use smelt_sys::process::ProcessTree;

/// 程序退出后，留着输出管道的后代进程被杀掉，再等管道关上最多这么久。
const DRAIN_AFTER_KILL: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub enum ProcessRunError {
    /// 参数本身不成立：程序名为空、工作目录不是绝对路径、超时超出范围。
    Invalid(String),
    /// 程序或工作目录不存在。
    NotFound(String),
    PermissionDenied(String),
    Io(String),
}

impl std::fmt::Display for ProcessRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Io(message) => f.write_str(message),
            Self::NotFound(program) => write!(f, "{program} wasn't found"),
            Self::PermissionDenied(program) => write!(f, "No permission to run {program}"),
        }
    }
}

impl std::error::Error for ProcessRunError {}

pub fn run(params: &ProcessRunParams) -> Result<ProcessRunResult, ProcessRunError> {
    if params.program.trim().is_empty() {
        return Err(ProcessRunError::Invalid("program is empty".into()));
    }
    let timeout_ms = params.timeout_ms.unwrap_or(PROCESS_RUN_DEFAULT_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > PROCESS_RUN_MAX_TIMEOUT_MS {
        return Err(ProcessRunError::Invalid(format!(
            "timeout_ms must be between 1 and {PROCESS_RUN_MAX_TIMEOUT_MS}"
        )));
    }
    let timeout = Duration::from_millis(timeout_ms);
    let deadline = Instant::now() + timeout;

    // 按子进程那份 PATH 找程序：Windows 上 `Command` 只补 `.exe`，npm 装的 `npx`、各家
    // CLI 是 `.cmd`。找不到保留原名，让下面的 spawn 报 `not_found`。
    let search_path = match params.env.get("PATH") {
        Some(path) => Cow::Borrowed(path.as_str()),
        None => crate::login_env::login_path(),
    };
    let program = crate::acp_conn::resolve_in_path(&params.program, &search_path)
        .unwrap_or_else(|| params.program.clone());
    let mut command = Command::new(program);
    command
        .args(&params.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &params.cwd {
        if !Path::new(cwd).is_absolute() {
            return Err(ProcessRunError::Invalid(format!(
                "{cwd} isn't an absolute path"
            )));
        }
        if !Path::new(cwd).is_dir() {
            return Err(ProcessRunError::NotFound(cwd.clone()));
        }
        command.current_dir(cwd);
    }
    crate::login_env::apply_login_environment(
        &mut command,
        params.env.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );

    let mut tree =
        ProcessTree::spawn_no_window(&mut command).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ProcessRunError::NotFound(params.program.clone()),
            io::ErrorKind::PermissionDenied => {
                ProcessRunError::PermissionDenied(params.program.clone())
            }
            _ => ProcessRunError::Io(format!("couldn't start {}: {error}", params.program)),
        })?;

    let (stdout, stderr) = tree.take_output();
    let (tx, rx) = mpsc::channel();
    spawn_reader(stdout, 0, tx.clone());
    spawn_reader(stderr, 1, tx);

    let timed_out = !smelt_sys::process_wait::wait_child_exit(tree.pid(), timeout);
    if timed_out {
        let _ = tree.kill();
    }
    let status = tree
        .wait()
        .map_err(|error| ProcessRunError::Io(format!("couldn't wait for the process: {error}")))?;

    // 程序退了，输出管道可能还被它留下的后代开着；到截止时间就连它们一起杀掉，不无限等。
    let mut outputs: [Option<Captured>; 2] = [None, None];
    let mut killed = timed_out;
    while outputs.iter().any(Option::is_none) {
        let wait = if killed {
            DRAIN_AFTER_KILL
        } else {
            deadline.saturating_duration_since(Instant::now())
        };
        match rx.recv_timeout(wait) {
            Ok((index, captured)) => outputs[index] = Some(captured),
            Err(RecvTimeoutError::Timeout) if !killed => {
                let _ = tree.kill();
                killed = true;
            }
            Err(_) => break,
        }
    }
    let [stdout, stderr] = outputs.map(Option::unwrap_or_default);
    Ok(ProcessRunResult {
        code: if timed_out { None } else { status.code() },
        truncated: stdout.truncated || stderr.truncated,
        stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
        timed_out,
    })
}

#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

/// 读到管道关上。超出上限的读掉丢弃：不读的话子进程会卡在写上。
fn spawn_reader(
    pipe: Option<impl Read + Send + 'static>,
    index: usize,
    tx: mpsc::Sender<(usize, Captured)>,
) {
    let _ = thread::Builder::new()
        .name("smelt-process-run-output".into())
        .spawn(move || {
            let mut captured = Captured::default();
            if let Some(mut pipe) = pipe {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let room = PROCESS_OUTPUT_LIMIT - captured.bytes.len();
                            if n > room {
                                captured.truncated = true;
                            }
                            captured.bytes.extend_from_slice(&buf[..n.min(room)]);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            }
            let _ = tx.send((index, captured));
        });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> ProcessRunParams {
        ProcessRunParams {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            ..Default::default()
        }
    }

    #[test]
    fn captures_exit_code_and_both_streams() {
        let result = run(&sh("printf out; printf err >&2; exit 3")).unwrap();
        assert_eq!(result.code, Some(3));
        assert_eq!(result.stdout, "out");
        assert_eq!(result.stderr, "err");
        assert!(!result.timed_out);
        assert!(!result.truncated);
    }

    #[test]
    fn env_and_cwd_apply() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let mut params = sh("printf \"$SMELT_PROCESS_RUN_TEST:\"; pwd");
        params.cwd = Some(cwd.to_string_lossy().into_owned());
        params
            .env
            .insert("SMELT_PROCESS_RUN_TEST".into(), "hello".into());
        let result = run(&params).unwrap();
        assert_eq!(result.stdout.trim_end(), format!("hello:{}", cwd.display()));
    }

    #[test]
    fn timeout_kills_the_whole_tree() {
        let mut params = sh("sleep 30 & sleep 30");
        params.timeout_ms = Some(200);
        let started = Instant::now();
        let result = run(&params).unwrap();
        assert!(result.timed_out);
        assert_eq!(result.code, None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn lingering_descendant_holding_the_pipe_does_not_hang() {
        // 父进程马上退出，后台子进程拿着 stdout 一直不关。
        let mut params = sh("printf done; sleep 30 &");
        params.timeout_ms = Some(300);
        let started = Instant::now();
        let result = run(&params).unwrap();
        assert_eq!(result.code, Some(0));
        assert!(!result.timed_out);
        assert_eq!(result.stdout, "done");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_program_and_bad_params_are_reported() {
        let missing = ProcessRunParams {
            program: "/definitely/not/here/smelt-nope".into(),
            ..Default::default()
        };
        assert!(matches!(run(&missing), Err(ProcessRunError::NotFound(_))));

        let mut relative = sh("true");
        relative.cwd = Some("relative/dir".into());
        assert!(matches!(run(&relative), Err(ProcessRunError::Invalid(_))));

        let mut too_long = sh("true");
        too_long.timeout_ms = Some(PROCESS_RUN_MAX_TIMEOUT_MS + 1);
        assert!(matches!(run(&too_long), Err(ProcessRunError::Invalid(_))));
    }

    #[test]
    fn output_over_the_limit_is_truncated_without_blocking_the_child() {
        let limit = PROCESS_OUTPUT_LIMIT + 1024;
        let result = run(&sh(&format!("head -c {limit} /dev/zero"))).unwrap();
        assert_eq!(result.code, Some(0));
        assert!(result.truncated);
        assert_eq!(result.stdout.len(), PROCESS_OUTPUT_LIMIT);
    }
}
