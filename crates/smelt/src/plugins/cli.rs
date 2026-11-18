//! `smelt plugin …`：写插件时用的命令。
//!
//! 放在桌面程序里而不是 `smelt-cli`：`check` 要用 App 的插件运行时、宿主模块和 GPUI 把插件
//! 真跑一遍，这些只有桌面程序有。Linux 上的 `smelt` 是不带界面的 `smelt-cli`，没有这组命令。
//!
//! - `check <目录>`：不开 App 加载并渲染一次（见 `check`）。
//! - `status [<id>]`：开着的 App 里各插件的加载结果（见 `status`）。给了 id 只看那一个，
//!   没挂上或加载失败时退出码非 0，写完插件保存后拿它确认 App 里挂好了。
//!
//! 和 `smelt-cli` 一样：结果是 stdout 上的 JSON，失败原因写 stderr、退出码非 0。

use rust_i18n::t;

/// 是 `smelt plugin …` 就跑完返回退出码；不是返回 `None`，交给别的入口。
pub(crate) fn maybe_run(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some("plugin") {
        return None;
    }
    smelt_cli::use_system_locale();
    Some(run(&args[2..]))
}

fn run(args: &[String]) -> i32 {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check", dir] => super::check::run(std::path::Path::new(dir)),
        ["status"] => status(None),
        ["status", id] => status(Some(id)),
        _ => {
            eprintln!("{}", t!("plugins.cli_usage"));
            2
        }
    }
}

fn status(id: Option<&str>) -> i32 {
    let Some(snapshot) = super::status::read_live() else {
        eprintln!("{}", t!("plugins.cli_app_not_running"));
        return 1;
    };
    let Some(id) = id else {
        return print(&snapshot.plugins);
    };
    let Some(plugin) = snapshot.plugins.iter().find(|plugin| plugin.id == id) else {
        eprintln!("{}", t!("plugins.cli_not_loaded", id = id));
        return 1;
    };
    let code = print(plugin);
    if let Some(error) = &plugin.error {
        eprintln!("{error}");
        return 1;
    }
    code
}

fn print(value: &impl serde::Serialize) -> i32 {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
