//! Rust 和 JS 两边的插件接口一致：Rust 只是内置插件的实现语言，不因此多拿能力。
//!
//! 本 crate 根上给插件的每一样（函数、再导出的库、[`crate::Host`] 的方法）都要在 [`SURFACE`]
//! 里写明 JS 那边是什么：同名函数、换了名字的函数、别的形式（命令、组件、gpui-shell 自带的
//! 模块），或者为什么只有 Rust 有、只有宿主用。JS 模块里的每个函数也要对得上 Rust 的一样，
//! 对不上的写进 [`JS_ONLY`]。加了接口不写进表，这里的测试就不过。

use std::collections::BTreeSet;

/// JS 那边对应的是什么。
enum Js {
    /// 同名的 JS 函数。
    Same,
    /// 换了名字的 JS 函数。
    Named(&'static str),
    /// JS 用别的形式做到：命令、组件、gpui-shell 自带的模块等，写明是什么。
    Via(&'static str),
    /// 只有宿主用（装宿主、宿主读登记来摆界面），不是给插件的。
    Host,
    /// 只有 Rust 有，写明为什么不映射。
    RustOnly(&'static str),
}

use Js::*;

/// 本 crate 根上给插件的每一样；`host.` 开头的是 [`crate::Host`] 的方法。
const SURFACE: &[(&str, Js)] = &[
    // ---- 宿主服务
    (
        "host",
        Via("宿主服务的读法拆成各个同名函数（host.machines → machines）"),
    ),
    ("host.machines", Named("machines")),
    ("host.client", Named("call")),
    (
        "host.connector",
        RustOnly(
            "开原生字节流（终端、对话）；gpui-shell 的宿主函数只收发数据，嵌终端、对话用 SmeltViewInstance",
        ),
    ),
    ("host.machine_online", Named("machine_online")),
    ("host.connections", Named("connections")),
    ("host.view_shown", Named("view_shown")),
    ("host.view_laid_out", Named("view_laid_out")),
    (
        "host.main_window",
        RustOnly("拿 gpui 窗口句柄；JS 的命令由宿主在主窗口里执行"),
    ),
    ("host.window_active", Named("window_active")),
    ("host.overlay_open", Named("overlay_open")),
    ("host.path_style", Named("path_style")),
    ("host.session_states", Named("session_states")),
    ("host.session_presence", Named("session_presence")),
    ("host.workspace_directory", Named("workspace_directory")),
    (
        "host.notification_permission",
        Named("notification_permission"),
    ),
    ("events", Via("next_event")),
    ("call", Same),
    (
        "call_error_text",
        RustOnly("把 Rust 的调用错误转成给人看的话；JS 的 call 直接 reject 这句话"),
    ),
    ("machine_removed", RustOnly("同上，JS 的 call 直接 reject")),
    (
        "machine_to_json",
        RustOnly("Rust 类型和 JSON 互转；JS 直接写 JSON"),
    ),
    ("machine_from_json", RustOnly("同上")),
    ("machine_json", RustOnly("同上（serde 用）")),
    ("wait_online", Same),
    ("active_context", Same),
    ("set_active_context", Same),
    // ---- 命令
    ("command", Same),
    ("commands", Same),
    ("register_commands", Same),
    ("unregister_commands", Via("register_commands([])")),
    ("run", Same),
    ("invoke", Host),
    (
        "no_main_window",
        RustOnly("Rust 命令处理函数报「要主窗口」的错；JS 的 run 在没有主窗口时直接 reject"),
    ),
    (
        "arg_fields",
        RustOnly("从 Rust 参数类型推出参数字段；JS 在 register_commands 里直接写 args"),
    ),
    // ---- 视图、页、布局
    ("register_views", Same),
    ("unregister_views", Via("register_views([])")),
    ("views", Same),
    ("view", Via("views() 里按 id 找")),
    ("is_registered", Via("views() 里按 id 找")),
    ("slot_views", Same),
    ("create_view", Via("smelt-ui 的 SmeltViewInstance 组件")),
    ("ensure_view", Host),
    (
        "built_view",
        RustOnly("取自己建好的视图实体直接调它；JS 的视图状态在脚本里，跨视图用命令和 emit_event"),
    ),
    ("view_badge", Same),
    ("set_view_badge", Via("命令 smelt.app/set-view-badge")),
    ("safe_area", Same),
    (
        "set_safe_area",
        RustOnly("给自己摆的子视图分 safe area；JS 摆不了带圆角让位的子视图，等要用时补"),
    ),
    ("register_pages", Same),
    ("unregister_pages", Via("register_pages([])")),
    ("pages", Same),
    ("page", Via("pages() 里按 id 找")),
    ("home_page", Via("pages() 里 home 为 true 的")),
    ("register_layouts", Same),
    ("unregister_layouts", Via("register_layouts([])")),
    ("layouts", Same),
    ("layout", Host),
    ("layout_stacks", Host),
    ("parse_layout", Host),
    // ---- 菜单、文件、退出
    ("register_menu_items", Same),
    ("unregister_menu_items", Via("register_menu_items([])")),
    ("menu_items", Host),
    ("set_status_menu", Same),
    ("status_menus", Host),
    ("observe_status_menus", Host),
    ("register_context_menu_items", Same),
    (
        "unregister_context_menu_items",
        Via("register_context_menu_items(menu, [])"),
    ),
    ("context_menu_items", Same),
    (
        "context_menu_args",
        Via("context_menu_items 交回的项已经并好参数"),
    ),
    ("register_file_opener", Same),
    ("unregister_file_opener", Via("插件卸下时宿主撤下")),
    ("open_file", Same),
    ("register_quit_guard", Named("set_quit_guard")),
    ("unregister_quit_guard", Via("set_quit_guard(null)")),
    ("quit_guarded", Host),
    // ---- 话题、事件
    ("register_topics", Same),
    ("unregister_topics", Via("register_topics([])")),
    ("subscribed_topics", Host),
    ("emit", Host),
    ("install", Host),
    ("js", Host),
    // ---- 主题、语言
    ("register_theme_sets", Named("register_themes")),
    ("unregister_themes", Via("register_themes([])")),
    ("themes", Via("smelt-ui 的主题")),
    ("apply_theme", Via("smelt-ui 的主题")),
    ("theme_contains", Via("smelt-ui 的主题")),
    ("theme_swatch", Via("smelt-ui 的主题")),
    ("locale", Same),
    ("set_locale", Same),
    ("system_locale", Same),
    ("supported_locale", Via("set_locale 自己归到带的语言上")),
    // ---- 再导出的库：不是宿主服务，JS 用对应的写法
    ("model", Via("线上类型；JS 按同样的 JSON 形状")),
    ("serde_json", Via("JS 原生 JSON")),
    ("ui", Via("smelt-ui 模块")),
    ("client", Via("call")),
    ("session_ops", Via("call")),
    ("plugin_store", Via("store_query / store_execute")),
    ("plugin_data_dir", Named("data_dir")),
    ("rusqlite", Via("store_query / store_execute")),
    ("paths", Via("smelt-fs 和环境变量")),
    (
        "process_usage",
        RustOnly("量进程 CPU、内存的系统库（同第三方库，不是宿主服务）；JS 用 exec 调系统命令"),
    ),
    ("app_log", Via("console（写进 app.log）")),
    ("block_on", RustOnly("Rust 异步的胶水；JS 有自己的 Promise")),
    ("smelt_version", Named("app_version")),
    (
        "updater",
        RustOnly("替换签名过的 App 包，只给随 App 的更新插件；JS 插件不该替换 App"),
    ),
    ("app_location", RustOnly("同上，更新插件判断 App 放在哪")),
];

/// JS 有、Rust 那边不叫这个名字的：各写明对着 Rust 的哪一样。
const JS_ONLY: &[(&str, &str)] = &[
    ("next_event", "events 的订阅"),
    ("emit_event", "Event::emit"),
    ("next_command", "register_commands 的处理函数"),
    ("resolve_command", "处理函数返回 Ok"),
    ("reject_command", "处理函数返回 Err"),
    ("opened_view", "PluginView::open 的参数"),
    ("notify_view", "视图自己的 cx.notify()"),
    ("t", "各 crate 的 t!，文案表是插件目录下的 locales/ui.yml"),
    ("locales", "常量 LOCALES"),
    ("store_query", "plugin_store"),
    ("store_execute", "plugin_store"),
    (
        "asset",
        "内置插件的资源用 include_bytes 编进二进制，不在插件目录里",
    ),
];

/// 从本 crate 的 lib.rs 认出根上给出去的小写名字：`pub use` 列出的函数和库、`pub fn`、
/// `pub mod`，和 `Host` 的方法（记成 `host.<名字>`）。大写开头的类型、常量不算：常量是
/// 命令 id，JS 用 `run` 调。
fn rust_surface() -> BTreeSet<String> {
    let source = include_str!("../lib.rs");
    let mut names = BTreeSet::new();
    let mut statement = String::new();
    let mut in_host = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if line.starts_with("#[cfg(test)]") {
            break;
        }
        if line.starts_with("pub trait Host") {
            in_host = true;
            continue;
        }
        if in_host {
            if line.starts_with('}') {
                in_host = false;
            } else if let Some(rest) = trimmed.strip_prefix("fn ") {
                names.insert(format!("host.{}", ident(rest)));
            }
            continue;
        }
        if !statement.is_empty() || line.starts_with("pub use ") {
            statement.push_str(trimmed);
            statement.push(' ');
            if trimmed.ends_with(';') {
                use_names(&statement, &mut names);
                statement.clear();
            }
            continue;
        }
        for prefix in ["pub fn ", "pub mod "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                names.insert(ident(rest).to_string());
            }
        }
    }
    names.retain(|name| name.starts_with(|c: char| c.is_ascii_lowercase()));
    names
}

/// `pub use a::b::{c, d as e};` 里给出去的名字。
fn use_names(statement: &str, names: &mut BTreeSet<String>) {
    let body = statement
        .trim()
        .trim_start_matches("pub use ")
        .trim_end_matches(';');
    let items: Vec<&str> = match body.split_once('{') {
        Some((_, list)) => list.trim_end_matches('}').split(',').collect(),
        None => vec![body],
    };
    for item in items {
        let item = item.trim();
        if item.is_empty() || item == "self" {
            continue;
        }
        let name = match item.split_once(" as ") {
            Some((_, alias)) => alias,
            None => item.rsplit("::").next().unwrap_or(item),
        };
        names.insert(name.trim().to_string());
    }
}

fn ident(rest: &str) -> &str {
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// 声明里 `export function <名字>(` 的那些名字。
fn declared_functions() -> BTreeSet<String> {
    super::DECLARATIONS
        .lines()
        .filter_map(|line| line.trim().strip_prefix("export function "))
        .map(|rest| ident(rest).to_string())
        .collect()
}

#[test]
fn every_rust_export_says_what_js_has() {
    let functions: BTreeSet<String> = super::JsPlugin::new()
        .module()
        .function_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut problems = Vec::new();

    let listed: BTreeSet<String> = SURFACE.iter().map(|(name, _)| name.to_string()).collect();
    for name in rust_surface().difference(&listed) {
        problems.push(format!(
            "Rust 给插件的「{name}」没写进 SURFACE：JS 那边是什么？"
        ));
    }
    for name in listed.difference(&rust_surface()) {
        problems.push(format!("SURFACE 里的「{name}」Rust 那边已经没有了，删掉"));
    }

    let mut mapped = BTreeSet::new();
    for (name, js) in SURFACE {
        match js {
            Same => {
                mapped.insert(name.to_string());
            }
            Named(js) => {
                mapped.insert(js.to_string());
            }
            Via(what) | RustOnly(what) if what.trim().is_empty() => {
                problems.push(format!("「{name}」没写 JS 怎么做 / 为什么不映射"));
            }
            Via(_) | RustOnly(_) | Host => {}
        }
    }
    for name in mapped.difference(&functions) {
        problems.push(format!("SURFACE 说 JS 有函数「{name}」，模块里没有"));
    }
    let js_only: BTreeSet<String> = JS_ONLY.iter().map(|(name, _)| name.to_string()).collect();
    for name in functions.difference(&mapped) {
        if !js_only.contains(name) {
            problems.push(format!(
                "JS 函数「{name}」对不上 Rust 的哪一样，写进 SURFACE 或 JS_ONLY"
            ));
        }
    }

    let declared = declared_functions();
    for name in functions.difference(&declared) {
        problems.push(format!("JS 函数「{name}」没有 export function 声明"));
    }
    for name in declared.difference(&functions) {
        problems.push(format!("声明了 JS 函数「{name}」，模块里没有"));
    }

    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
