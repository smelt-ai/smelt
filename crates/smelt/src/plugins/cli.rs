//! `smelt plugin …`：写 NSH JS/TSX 插件时用的命令。
//!
//! 放在桌面程序里而不是 `smelt-cli`：`types` 和 `check` 都要使用与运行时完全一致的 `smelt-nsh`
//! 宿主 UI 集、主题令牌、SVG 图标和 QuickJS 编译器，这些只有桌面程序有。Linux 上的 `smelt`
//! 是不带界面的 `smelt-cli`，没有这组命令。
//!
//! - `init <id> [--theme] [--name <名称>] [--dir <目录>]`：创建 NSH TSX 起步插件（或 `--theme` 纯主题插件）与 TypeScript 声明（见 `init`）。
//! - `fork <builtin-id> [--as <new-id>] [--dir <目录>]`：拷贝自带插件源码到用户目录以覆写或改名（见 `fork`）。
//! - `types <目录>`：生成或刷新 `<目录>/.nsh/types/` 与 `.nsh/tsconfig.json`。
//! - `check <目录>`：离线校验清单、locales、主题、TSX 打包、import 与 QuickJS 编译（见 `check`）。
//! - `status [<id>]`：开着的 App 里各插件的加载结果、来源、覆写状态与已登记视图（见 `status`）。
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
    let head = args.first().map(String::as_str);
    match head {
        Some("init") => super::init::run(&args[1..]),
        Some("fork") => super::fork::run(&args[1..]),
        Some("types") if args.len() == 2 => types(std::path::Path::new(&args[1])),
        Some("check") if args.len() == 2 => super::check::run(std::path::Path::new(&args[1])),
        Some("stage") if args.len() == 3 => stage(
            std::path::Path::new(&args[1]),
            std::path::Path::new(&args[2]),
        ),
        Some("status") if args.len() == 1 => status(None),
        Some("status") if args.len() == 2 => status(Some(&args[1])),
        _ => {
            eprintln!("{}", t!("plugins.cli_usage"));
            2
        }
    }
}

fn stage(src: &std::path::Path, dst: &std::path::Path) -> i32 {
    match smelt_nsh::stage_builtin_plugins(src, dst) {
        Ok(report) => print(&serde_json::json!({
            "ok": true,
            "total": report.total,
            "script_plugins": report.script_plugins,
            "theme_only": report.theme_only,
            "elapsed_ms": report.elapsed_ms,
        })),
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn types(dir: &std::path::Path) -> i32 {
    let root = match std::fs::canonicalize(dir) {
        Ok(root) => root,
        Err(error) => {
            eprintln!(
                "{}",
                t!("plugins.cli_no_dir", path = dir.display(), error = error)
            );
            return 1;
        }
    };
    match smelt_nsh::write_plugin_types(&root) {
        Ok(report) => print(&serde_json::json!({
            "ok": true,
            "path": root,
            "types_dir": report.types_dir,
            "files_written": report.files_written,
            "files_unchanged": report.files_unchanged,
            "created_tsconfig": report.created_tsconfig,
        })),
        Err(error) => {
            eprintln!("{error}");
            1
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
