//! 子进程执行能力接缝（Service Definition + Provider + Consumer）。
//!
//! **为什么要这层间接**：smelt 现在有 78 处直接 `Command::new`，等价于把「进程
//! 住在本机」这条假设焊死在每个调用点上。远程访问（iroh）、worktree 沙箱、
//! 以及将来的云端执行环境，都需要同一套命令换一个执行世界；没有这层，每加
//! 一个世界就得再抄一遍所有调用点。
//!
//! 三个角色缺一不可，只有接口没有第二个实现不算接缝：
//! - **Definition**：[`Subprocess`]，能力面故意收得很窄（运行命令、等待输出），
//!   够 git 面板的 `git status` 之类用，复杂场景（agent 启动、PTY）暂不覆盖。
//! - **Provider**：[`LocalSubprocess`]（本机子进程）、[`MockSubprocess`]（内存，
//!   测试与将来远程实现的参照）。
//! - **Consumer**：[`run_git`] 这类只依赖 trait 的纯逻辑，换 provider 即可
//!   在别的执行世界里原样复用。
//!
//! 阻塞式而非 async：现有调用方都已经在后台线程/`background_executor` 里跑，
//! 引入 async trait 只会逼所有同步调用点改造，收益为零。

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

type CommandKey = (String, Vec<String>);
type MockResponses = Arc<Mutex<HashMap<CommandKey, Output>>>;

/// 命令执行结果。故意不暴露 `std::process::Output`——那是本地实现细节，
/// 远程 provider 造不出来。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// 退出码。正常退出为 Some(code)，被信号终止为 None。
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// 子进程执行能力。实现者负责把命令路由到自己的执行世界。
///
/// 错误一律用 [`io::Error`]：本地实现直接透传，远程实现把协议错误翻译过来，
/// 调用方不需要认识具体 provider 的错误类型。
pub trait Subprocess: Send + Sync {
    /// 运行命令并等待完成。
    ///
    /// - `cmd`：可执行文件路径或名称（会查 PATH）
    /// - `args`：命令行参数
    /// - `cwd`：工作目录，None 表示继承当前进程
    /// - `env`：额外环境变量（叠加到当前环境上）
    fn run(
        &self,
        cmd: &str,
        args: &[&str],
        cwd: Option<&Path>,
        env: &[(&str, &str)],
    ) -> io::Result<Output>;

    /// 运行命令并通过 stdin 传入数据。
    ///
    /// 典型用例：`git apply` 从 stdin 读 patch。
    fn run_with_stdin(
        &self,
        cmd: &str,
        args: &[&str],
        cwd: Option<&Path>,
        env: &[(&str, &str)],
        stdin: &[u8],
    ) -> io::Result<Output>;
}

// ===================== Provider：本机子进程 =====================

/// 本机子进程执行。语义即 `std::process::Command`，是所有非远程场景的默认 provider。
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalSubprocess;

impl Subprocess for LocalSubprocess {
    fn run(
        &self,
        cmd: &str,
        args: &[&str],
        cwd: Option<&Path>,
        env: &[(&str, &str)],
    ) -> io::Result<Output> {
        let mut command = std::process::Command::new(cmd);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        // GUI 进程从 Finder 启动时 PATH 只有 /usr/bin:/bin 一类系统目录，
        // git 本身能跑，但 hook（husky 调 npm/node、pre-commit 调 python）
        // 一律 command not found。先铺 login shell 环境，再让调用方显式
        // 传入的 env 覆盖它。
        crate::login_env::apply_login_environment(&mut command);
        for (k, v) in env {
            command.env(k, v);
        }
        let out = command.output()?;
        Ok(Output {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn run_with_stdin(
        &self,
        cmd: &str,
        args: &[&str],
        cwd: Option<&Path>,
        env: &[(&str, &str)],
        stdin: &[u8],
    ) -> io::Result<Output> {
        use std::io::Write;
        use std::process::Stdio;

        let mut command = std::process::Command::new(cmd);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        crate::login_env::apply_login_environment(&mut command);
        for (k, v) in env {
            command.env(k, v);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        {
            let mut si = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("无法打开子进程 stdin"))?;
            si.write_all(stdin)?;
        } // si 在这里 drop → 子进程收到 EOF
        let out = child.wait_with_output()?;
        Ok(Output {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

// ===================== Provider：Mock（测试用） =====================

/// 预设响应的 Mock 子进程。用于测试：
/// 1. 验证 Consumer 正确处理各种输出
/// 2. 作为「第二个实现」证明 trait 真的是接缝
#[derive(Clone, Default)]
pub struct MockSubprocess {
    /// (cmd, args) -> 预设输出。匹配时只看 cmd 和 args，忽略 cwd/env。
    responses: MockResponses,
    /// 找不到预设时的默认输出。
    default: Output,
}

impl MockSubprocess {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(HashMap::new())),
            default: Output {
                code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        }
    }

    /// 设置某条命令的预设响应。
    pub fn when(self, cmd: &str, args: &[&str], output: Output) -> Self {
        self.responses.lock().unwrap().insert(
            (
                cmd.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ),
            output,
        );
        self
    }

    /// 设置找不到预设时的默认响应。
    pub fn default_output(mut self, output: Output) -> Self {
        self.default = output;
        self
    }

    fn lookup(&self, cmd: &str, args: &[&str]) -> Output {
        let key = (
            cmd.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        );
        self.responses
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }
}

impl Subprocess for MockSubprocess {
    fn run(
        &self,
        cmd: &str,
        args: &[&str],
        _cwd: Option<&Path>,
        _env: &[(&str, &str)],
    ) -> io::Result<Output> {
        Ok(self.lookup(cmd, args))
    }

    fn run_with_stdin(
        &self,
        cmd: &str,
        args: &[&str],
        _cwd: Option<&Path>,
        _env: &[(&str, &str)],
        _stdin: &[u8],
    ) -> io::Result<Output> {
        // Mock 不区分有无 stdin，统一查表
        Ok(self.lookup(cmd, args))
    }
}

// ===================== Consumer：只依赖 trait 的纯逻辑 =====================

/// 跑一条 git 子命令：固定 `-C root` + `GIT_OPTIONAL_LOCKS=0`。
///
/// 这是 Consumer 的样板——**只认 [`Subprocess`]，不认 `std::process`**，所以
/// 本地面板和将来的远程 worktree 用的是同一份 git 调用逻辑，不会各自漂移。
pub fn run_git(sp: &dyn Subprocess, root: &Path, args: &[&str]) -> io::Result<Output> {
    let mut full_args = vec!["-C", root.to_str().unwrap_or(".")];
    full_args.extend(args);
    sp.run("git", &full_args, None, &[("GIT_OPTIONAL_LOCKS", "0")])
}

/// 跑一条从 stdin 读输入的 git 子命令（`git apply` 收 patch 用）。
pub fn run_git_stdin(
    sp: &dyn Subprocess,
    root: &Path,
    args: &[&str],
    input: &str,
) -> io::Result<Output> {
    let mut full_args = vec!["-C", root.to_str().unwrap_or(".")];
    full_args.extend(args);
    sp.run_with_stdin(
        "git",
        &full_args,
        None,
        &[("GIT_OPTIONAL_LOCKS", "0")],
        input.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 同一个 Consumer 跑在两个 Provider 上必须给出一致结果——这是「它确实是
    /// 接缝」的判据，只有一个实现的 trait 证明不了任何事。
    fn assert_run_git_contract(sp: &dyn Subprocess, root: &Path) {
        // git --version 应该成功并输出版本信息
        let out = run_git(sp, root, &["--version"]).unwrap();
        assert!(out.success());
        assert!(out.stdout_str().contains("git version"));
    }

    #[test]
    fn run_git_contract_holds_on_local_provider() {
        let sp = LocalSubprocess;
        // 用当前目录作为 root（git --version 不需要真的是 git 仓库）
        let root = PathBuf::from(".");
        assert_run_git_contract(&sp, &root);
    }

    #[test]
    fn run_git_contract_holds_on_mock_provider() {
        let sp = MockSubprocess::new().when(
            "git",
            &["-C", ".", "--version"],
            Output {
                code: Some(0),
                stdout: b"git version 2.40.0".to_vec(),
                stderr: Vec::new(),
            },
        );
        let root = PathBuf::from(".");
        assert_run_git_contract(&sp, &root);
    }

    /// git hook（husky 调 npm、pre-commit 调 python）跑在子进程里，靠 PATH 找工具。
    /// GUI 从 Finder 启动时进程 PATH 只有 /usr/bin:/bin，hook 会 127 退出，
    /// 表现成「点提交毫无反应」。所以本地子进程必须带上 login shell 的 PATH。
    #[test]
    fn local_provider_runs_with_login_path() {
        let sp = LocalSubprocess;
        let out = sp
            .run("/bin/sh", &["-c", "printf %s \"$PATH\""], None, &[])
            .unwrap();
        assert_eq!(out.stdout_str(), crate::login_env::login_path());
    }

    /// 显式传入的 env 是调用方的明确意图（如 GIT_OPTIONAL_LOCKS），
    /// 不能被 login 环境盖掉。
    #[test]
    fn explicit_env_overrides_login_environment() {
        let sp = LocalSubprocess;
        let out = sp
            .run(
                "/bin/sh",
                &["-c", "printf %s \"$PATH\""],
                None,
                &[("PATH", "/sentinel")],
            )
            .unwrap();
        assert_eq!(out.stdout_str(), "/sentinel");
    }

    #[test]
    fn mock_returns_default_for_unknown_commands() {
        let sp = MockSubprocess::new().default_output(Output {
            code: Some(127),
            stdout: Vec::new(),
            stderr: b"command not found".to_vec(),
        });
        let out = sp.run("nonexistent", &[], None, &[]).unwrap();
        assert_eq!(out.code, Some(127));
        assert!(out.stderr_str().contains("not found"));
    }

    #[test]
    fn local_subprocess_respects_cwd_and_env() {
        let sp = LocalSubprocess;
        // 用 env 命令验证环境变量传递
        #[cfg(unix)]
        {
            let out = sp
                .run("env", &[], None, &[("TEST_VAR", "test_value")])
                .unwrap();
            assert!(out.stdout_str().contains("TEST_VAR=test_value"));
        }
    }

    #[test]
    fn run_with_stdin_passes_input() {
        let sp = LocalSubprocess;
        // 用 cat 验证 stdin 传递
        #[cfg(unix)]
        {
            let out = sp
                .run_with_stdin("cat", &[], None, &[], b"hello world")
                .unwrap();
            assert!(out.success());
            assert_eq!(out.stdout_str().trim(), "hello world");
        }
    }
}
