//! 控制台的 `smelt`：只有命令行子命令，没有窗口。
//!
//! - Windows：`smelt.exe` 是 GUI 子系统，在 cmd / PowerShell 里跑 `smelt agent list`，输出
//!   没有控制台可去，shell 也不等它退出。发版时把这个程序改名成 `smelt.com` 放在 `smelt.exe`
//!   旁边：敲 `smelt` 时先找到 `.com`（PATHEXT 里排在 `.exe` 前面），命令行子命令在这里跑完；
//!   不带子命令就拉起旁边的 GUI。`smelt plugin …` 只有 GUI 程序里有，交给它跑并等结果。
//! - Linux：没有桌面 GUI，运行时包里的 `smelt` 就是它（装到 `~/.smelt/bin/smelt`）。不带
//!   子命令时打印用法。
//! - macOS：GUI 的 `smelt` 自己就是控制台程序、先拦截子命令，不发这一份。

// 和 lib 用同一份文案表（同一个 locales 目录）；当前语言是 rust-i18n 的全局值。
rust_i18n::i18n!("locales", fallback = "en");

use rust_i18n::t;

fn main() {
    smelt_cli::use_system_locale();
    if let Some(code) = smelt_cli::maybe_run(std::env::args()) {
        std::process::exit(code);
    }
    std::process::exit(without_subcommand());
}

/// 不认识的参数原样交给 GUI，由它决定怎么处理。
#[cfg(windows)]
fn without_subcommand() -> i32 {
    let gui = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name("smelt.exe"),
        Err(error) => {
            eprintln!("{}", t!("main.exe_dir_missing", error = error));
            return 1;
        }
    };
    // `smelt plugin …` 只有 GUI 程序里有（要它的 NSH 编译器和类型），但结果是给终端的：
    // 等它跑完，把输出和退出码原样交回，不像开窗口那样拉起就走。
    if std::env::args().nth(1).as_deref() == Some("plugin") {
        return run_to_completion(
            std::process::Command::new(&gui).args(std::env::args_os().skip(1)),
        );
    }
    match std::process::Command::new(&gui)
        .args(std::env::args_os().skip(1))
        .spawn()
    {
        Ok(_) => 0,
        Err(error) => {
            eprintln!(
                "{}",
                t!(
                    "main.gui_launch_failed",
                    path = gui.display(),
                    error = error
                )
            );
            1
        }
    }
}

/// 跑完 `command`，把它的 stdout、stderr 和退出码转成自己的。
///
/// 用管道接输出：GUI 子系统的 `smelt.exe` 没有控制台，往继承来的控制台句柄写不一定写得出来，
/// 管道一定行。命令的输出都不大，跑完一次性转出。
#[cfg_attr(not(windows), allow(dead_code))]
fn run_to_completion(command: &mut std::process::Command) -> i32 {
    use std::io::Write as _;
    let program = command.get_program().to_owned();
    match command.stdin(std::process::Stdio::null()).output() {
        Ok(output) => {
            let _ = std::io::stdout().write_all(&output.stdout);
            let _ = std::io::stderr().write_all(&output.stderr);
            // 没有退出码是被外力结束的，算失败。
            output.status.code().unwrap_or(1)
        }
        Err(error) => {
            eprintln!(
                "{}",
                t!(
                    "main.gui_launch_failed",
                    path = std::path::Path::new(&program).display(),
                    error = error
                )
            );
            1
        }
    }
}

/// 没有 GUI 可开：不带参数打印用法，带了不认识的参数说清楚再打印用法。
#[cfg(not(windows))]
fn without_subcommand() -> i32 {
    if let Some(arg) = std::env::args().nth(1) {
        eprintln!("{}\n", t!("main.unknown_command", command = arg));
    }
    smelt_cli::print_help();
    2
}

#[cfg(all(test, unix))]
mod tests {
    use super::run_to_completion;

    #[test]
    fn the_child_exit_code_becomes_ours() {
        let code = run_to_completion(
            std::process::Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
        );
        assert_eq!(code, 3);
        assert_eq!(
            run_to_completion(&mut std::process::Command::new("true")),
            0
        );
    }

    #[test]
    fn a_missing_program_is_a_failure() {
        assert_eq!(
            run_to_completion(&mut std::process::Command::new("/nonexistent/smelt.exe")),
            1
        );
    }
}
