//! 控制台子系统的 `smelt`。
//!
//! Windows 上 `smelt.exe` 是 GUI 子系统：在 cmd / PowerShell 里跑 `smelt agent list`，
//! 输出没有控制台可去，shell 也不等它退出。发版时把这个程序改名成 `smelt.com` 放在
//! `smelt.exe` 旁边：敲 `smelt` 时先找到 `.com`（PATHEXT 里排在 `.exe` 前面），命令行
//! 子命令在这里跑完；不带子命令就拉起旁边的 GUI。
//!
//! macOS / Linux 的 `smelt` 本来就是控制台程序，不发这一份。

fn main() {
    if let Some(code) = smelt_cli::maybe_run(std::env::args()) {
        std::process::exit(code);
    }
    std::process::exit(launch_gui());
}

/// 不认识的参数原样交给 GUI，由它决定怎么处理。
fn launch_gui() -> i32 {
    let gui_name = if cfg!(windows) { "smelt.exe" } else { "smelt" };
    let gui = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name(gui_name),
        Err(error) => {
            eprintln!("找不到自己所在的目录：{error}");
            return 1;
        }
    };
    match std::process::Command::new(&gui)
        .args(std::env::args_os().skip(1))
        .spawn()
    {
        Ok(_) => 0,
        Err(error) => {
            eprintln!("启动 {} 失败：{error}", gui.display());
            1
        }
    }
}
