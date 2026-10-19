//! 守护启动时的分流：内部用的参数不能被命令行子命令吞掉。

use std::process::{Command, Stdio};

/// 守护用 `--acp-session-host` 拉起自己当会话宿主。子命令分流先看到这个参数的话，
/// 会报「未知命令」退出，每个对话会话都起来就挂、无限重启。
/// 标准输入接空设备时，宿主读不到首行请求，应当直接正常退出。
#[test]
fn session_host_arg_is_not_taken_for_a_subcommand() {
    let home = std::env::temp_dir().join(format!("smeltd-dispatch-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_smeltd"))
        .arg("--acp-session-host")
        .env("HOME", &home)
        .env("SMELT_HOME", home.join(".smelt"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        output.status.success(),
        "退出码 {:?}，stderr：{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
