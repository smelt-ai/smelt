//! 本机维护的受控底层能力（无界面）：
//!
//! - `~/.smelt` 未登记顶层残留扫描与删除（[`residue`]）；
//! - macOS 终端 `smelt` 软链管理（[`cli_command`]：启动同步 `~/.smelt/bin/smelt`，按需检测/提权安装 `/usr/local/bin/smelt`）。
//!
//! 所有界面与交互均在 JS 插件 `plugins/maintenance/` 中实现，通过 `smelt.app/*` 无界面能力命令调用。

#[cfg(target_os = "macos")]
pub(crate) mod cli_command;
mod residue;

use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{Value, json};
use smelt_plugin_api::{CommandSpec, Handled};

/// 启动时在后台把 `~/.smelt/bin/smelt` 拨到这个 App。
#[cfg(target_os = "macos")]
pub(crate) fn sync_cli_link_at_startup(cx: &gpui::App) {
    cx.background_executor()
        .spawn(async {
            if let Err(error) = cli_command::sync_managed_link() {
                eprintln!("[maintenance] 放 ~/.smelt/bin/smelt 失败：{error}");
            }
        })
        .detach();
}

/// 暴露给插件的本机维护底层能力命令（不进命令面板、不绑键）。
pub(crate) fn app_commands() -> Vec<CommandSpec> {
    vec![
        capability("storage-residue-scan", false, |_, _, cx| {
            let task = cx.background_executor().spawn(async {
                residue::scan()
                    .map(|items| Value::Array(items.into_iter().map(residue_json).collect()))
                    .map_err(|error| error.to_string())
            });
            Handled::Done(task)
        }),
        capability("storage-residue-remove", true, |args, _, cx| {
            let paths: Vec<PathBuf> = args
                .get("paths")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default();
            let task = cx.background_executor().spawn(async move {
                let failures: Vec<Value> = residue::remove(&paths)
                    .into_iter()
                    .map(|(path, error)| {
                        eprintln!("[maintenance] 删 {} 失败：{error}", path.display());
                        json!({
                            "path": path.to_string_lossy(),
                            "error": error,
                        })
                    })
                    .collect();
                match residue::scan() {
                    Ok(items) => Ok(json!({
                        "failures": failures,
                        "items": items.into_iter().map(residue_json).collect::<Vec<_>>(),
                    })),
                    Err(error) => Ok(json!({
                        "failures": failures,
                        "scan_error": error.to_string(),
                        "items": [],
                    })),
                }
            });
            Handled::Done(task)
        }),
        capability("cli-link-status", false, |_, _, cx| {
            #[cfg(target_os = "macos")]
            {
                let task = cx.background_executor().spawn(async {
                    let status = cli_command::system_command();
                    Ok(json!({
                        "supported": true,
                        "link": cli_command::SYSTEM_LINK,
                        "status": status.as_str(),
                    }))
                });
                Handled::Done(task)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = cx;
                Handled::result(Ok(json!({
                    "supported": false,
                    "link": "",
                    "status": "unknown",
                })))
            }
        }),
        capability("cli-link-install", false, |_, _, cx| {
            #[cfg(target_os = "macos")]
            {
                let task = cx.background_executor().spawn(async {
                    let result = cli_command::install();
                    let status = cli_command::system_command();
                    Ok(match result {
                        Ok(()) => json!({
                            "ok": true,
                            "link": cli_command::SYSTEM_LINK,
                            "status": status.as_str(),
                        }),
                        Err(error) => {
                            eprintln!("[maintenance] 装 smelt 命令失败：{error:?}");
                            json!({
                                "ok": false,
                                "code": error.code(),
                                "error": error.detail(),
                                "link": cli_command::SYSTEM_LINK,
                                "status": status.as_str(),
                            })
                        }
                    })
                });
                Handled::Done(task)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = cx;
                Handled::result(Ok(json!({
                    "ok": false,
                    "code": "unsupported",
                    "error": "unsupported platform",
                    "link": "",
                    "status": "unknown",
                })))
            }
        }),
    ]
}

fn residue_json(item: residue::Residue) -> Value {
    json!({
        "path": item.path.to_string_lossy(),
        "label": item.label,
        "bytes": item.bytes,
    })
}

fn capability(
    name: &'static str,
    requires_args: bool,
    handler: impl Fn(&Value, Option<&mut gpui::Window>, &mut gpui::App) -> Handled + 'static,
) -> CommandSpec {
    CommandSpec {
        name: name.into(),
        title: name.into(),
        key: None,
        context: "Workspace".into(),
        palette: false,
        requires_args,
        args: None,
        cli: false,
        handler: Rc::new(handler),
    }
}
