//! 新建工作树后的初始化脚本：每个仓库的 worktree 建好后，跑源仓库里的
//! `.smelt/worktree-setup`（装依赖、拷 `.env` 之类）。
//!
//! 脚本从源仓库的工作区读，没提交进 git 只放在本地也算；在 worktree 目录里用 `sh` 跑，
//! 环境是用户的 login shell 环境，另带 `SMELT_SOURCE_ROOT`（源仓库）和
//! `SMELT_TASK_ROOT`（任务目录）。失败不回滚工作树，只把输出末尾报给调用方。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use smelt_core::workspace_directory::WorkspaceWorksetSetupResult;

/// 源仓库里初始化脚本的位置。
pub(crate) const SETUP_SCRIPT: &str = ".smelt/worktree-setup";

/// 脚本跑太久就杀掉，免得一直占着工作树操作锁；装依赖一般几分钟内完成。
const SETUP_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// 失败时报回去的输出末尾长度。
const OUTPUT_TAIL_BYTES: usize = 4000;

pub(crate) fn run_setup_scripts(
    workset: &smelt_git::worktree::Workset,
) -> Vec<WorkspaceWorksetSetupResult> {
    let task_root = workset.project_root.join("worktrees").join(&workset.task);
    workset
        .repositories
        .iter()
        .filter_map(|repository| {
            let script = repository.root.join(SETUP_SCRIPT);
            script.is_file().then(|| {
                let (success, output) =
                    run_script(&script, &repository.root, &repository.worktree, &task_root);
                WorkspaceWorksetSetupResult {
                    repository: repository.name.clone(),
                    success,
                    output: if success { String::new() } else { output },
                }
            })
        })
        .collect()
}

fn run_script(script: &Path, source: &Path, worktree: &Path, task_root: &Path) -> (bool, String) {
    let log = std::env::temp_dir().join(format!(
        "smelt-worktree-setup-{}.log",
        uuid::Uuid::new_v4().simple()
    ));
    let result = spawn_and_wait(script, source, worktree, task_root, &log);
    let output = std::fs::read(&log)
        .map(|bytes| tail(&bytes))
        .unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    match result {
        Ok(()) => (true, output),
        Err(error) => (false, join_output(output, error)),
    }
}

fn spawn_and_wait(
    script: &Path,
    source: &Path,
    worktree: &Path,
    task_root: &Path,
    log: &Path,
) -> Result<(), String> {
    let file = std::fs::File::create(log)
        .map_err(|error| format!("Couldn't create the log file: {error}"))?;
    let stderr = file
        .try_clone()
        .map_err(|error| format!("Couldn't create the log file: {error}"))?;
    let (source, task_root) = (source.to_string_lossy(), task_root.to_string_lossy());
    let mut command = Command::new("sh");
    command
        .arg(script)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(stderr);
    smelt_core::login_env::apply_login_environment(
        &mut command,
        [
            ("SMELT_SOURCE_ROOT", source.as_ref()),
            ("SMELT_TASK_ROOT", task_root.as_ref()),
        ],
    );
    let child = command
        .spawn()
        .map_err(|error| format!("Couldn't run {} with sh: {error}", script.display()))?;
    let status = smelt_sys::process_wait::wait_child_for(child, SETUP_TIMEOUT, |pid| {
        let _ = smelt_sys::process::terminate_pid(pid);
    })
    .map_err(|error| format!("Couldn't wait for the script to finish: {error}"))?;
    if status.success() {
        return Ok(());
    }
    match status.code() {
        Some(code) => Err(format!("The script exited with code {code}")),
        None => Err(format!(
            "The script was killed (scripts are stopped after {} minutes)",
            SETUP_TIMEOUT.as_secs() / 60
        )),
    }
}

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(OUTPUT_TAIL_BYTES);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
}

fn join_output(output: String, error: String) -> String {
    if output.is_empty() {
        error
    } else {
        format!("{output}\n{error}")
    }
}
