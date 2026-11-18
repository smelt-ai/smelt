//! 控制台的 `smelt`：只有命令行子命令，没有窗口。
//!
//! - Windows：`smelt.exe` 是 GUI 子系统，在 cmd / PowerShell 里跑 `smelt agent list`，输出
//!   没有控制台可去，shell 也不等它退出。发版时把这个程序改名成 `smelt.com` 放在 `smelt.exe`
//!   旁边：敲 `smelt` 时先找到 `.com`（PATHEXT 里排在 `.exe` 前面），命令行子命令在这里跑完；
//!   不带子命令就拉起旁边的 GUI。
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

/// 没有 GUI 可开：不带参数打印用法，带了不认识的参数说清楚再打印用法。
#[cfg(not(windows))]
fn without_subcommand() -> i32 {
    if let Some(arg) = std::env::args().nth(1) {
        eprintln!("{}\n", t!("main.unknown_command", command = arg));
    }
    smelt_cli::print_help();
    2
}
