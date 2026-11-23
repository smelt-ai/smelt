//! 应用在线更新的无界面生命周期能力：检查、下载、校验、替换 App 包与退出守卫。
//!
//! 仅保留无界面状态机与受控操作（macOS / Windows 在 [`update`] 中实现），所有关于页 UI、
//! 菜单项、命令面板入口与角标均由 JS 插件 `plugins/updater/` 独立提供；本模块不认识任何
//! 插件视图 id，删掉 `plugins/updater/` 本模块无需改动。

#[cfg(any(target_os = "macos", windows))]
mod store;
#[cfg(any(target_os = "macos", windows))]
mod update;

use std::rc::Rc;

use gpui::App;
use serde_json::{Value, json};
use smelt_plugin_api::{self as api, CommandSpec, Handled};

use crate::plugin_host::WorkspaceRef;

/// 更新偏好库与退出守卫的来源 id（沿用 `smelt.updater` 保留已有用户偏好库）。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) const OWNER: &str = "smelt.updater";
/// 状态变化时发出的通用宿主事件名。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) const STATE_EVENT: &str = "smelt.app/update-state-changed";

/// 「反馈问题」跳转的链接。打包时设 `SMELT_FEEDBACK_URL` 换成自己的反馈渠道。
pub(crate) const FEEDBACK_URL: &str = build_time_url(
    option_env!("SMELT_FEEDBACK_URL"),
    "https://github.com/smelt-ai/smelt/issues/new/choose",
);

/// 「帮助文档」的链接。打包时设 `SMELT_DOCS_URL` 换掉。
pub(crate) const DOCS_URL: &str =
    build_time_url(option_env!("SMELT_DOCS_URL"), "https://smelt.onoo.io/");

/// 源码仓库。
pub(crate) const SOURCE_URL: &str = "https://github.com/smelt-ai/smelt";

const fn build_time_url(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(url) if !url.is_empty() => url,
        _ => default,
    }
}

/// 宿主启动时调一次（在 `api::install` 之后、开主窗口之前）：启动无界面更新生命周期。
pub(crate) fn register(cx: &mut App) {
    #[cfg(any(target_os = "macos", windows))]
    update::register(cx);
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = cx;
}

/// 当前更新状态快照（无界面纯数据，供 `smelt.app/update-state` 与事件推送共用）。
pub(crate) fn state_json(cx: &App) -> Value {
    #[cfg(any(target_os = "macos", windows))]
    {
        if let Some(entity) = update::updater(cx) {
            return update::snapshot_json(&entity, cx);
        }
    }
    let _ = cx;
    json!({
        "supported": false,
        "version": api::smelt_version::VERSION,
        "channel": "prod",
        "auto_install": false,
        "status": { "kind": "idle" },
        "can_check": false,
        "can_request_check": false,
        "ready": false,
        "update_available": false,
        "moving_app": false,
        "move_app_error": Value::Null,
        "manual_download_url": Value::Null,
        "docs_url": DOCS_URL,
        "feedback_url": FEEDBACK_URL,
        "source_url": SOURCE_URL,
        "app_log_path": api::app_log::log_path().map(|p| p.to_string_lossy().into_owned()),
    })
}

/// 暴露给插件的更新无界面能力命令（挂在 `smelt.app/*`，不进命令面板、不绑键）。
pub(crate) fn app_commands(workspace: &WorkspaceRef) -> Vec<CommandSpec> {
    let install_ws = workspace.clone();
    vec![
        capability("update-state", false, |_, _, cx| {
            Handled::result(Ok(state_json(cx)))
        }),
        capability("update-check", false, |_, _, cx| {
            #[cfg(any(target_os = "macos", windows))]
            if let Some(entity) = update::updater(cx) {
                entity.update(cx, |this, cx| this.request_check(cx));
            }
            Handled::result(Ok(state_json(cx)))
        }),
        capability("update-set-channel", true, |args, _, cx| {
            #[cfg(any(target_os = "macos", windows))]
            {
                let channel = match args.get("channel").and_then(Value::as_str) {
                    Some("dev") => api::updater::UpdateChannel::Dev,
                    Some("prod") => api::updater::UpdateChannel::Prod,
                    _ => return Handled::err("update-set-channel: channel must be prod or dev"),
                };
                if let Some(entity) = update::updater(cx) {
                    entity.update(cx, |this, cx| this.set_channel(channel, cx));
                }
            }
            #[cfg(not(any(target_os = "macos", windows)))]
            let _ = args;
            Handled::result(Ok(state_json(cx)))
        }),
        capability("update-set-auto-install", true, |args, _, cx| {
            #[cfg(any(target_os = "macos", windows))]
            {
                let Some(auto_install) = args.get("auto_install").and_then(Value::as_bool) else {
                    return Handled::err("update-set-auto-install: missing auto_install bool");
                };
                if let Some(entity) = update::updater(cx) {
                    entity.update(cx, |this, cx| this.set_auto_install(auto_install, cx));
                }
            }
            #[cfg(not(any(target_os = "macos", windows)))]
            let _ = args;
            Handled::result(Ok(state_json(cx)))
        }),
        capability("update-download", true, |args, _, cx| {
            #[cfg(any(target_os = "macos", windows))]
            {
                let Some(version) = args.get("version").and_then(Value::as_str) else {
                    return Handled::err("update-download: missing version");
                };
                let Some(url) = args.get("url").and_then(Value::as_str) else {
                    return Handled::err("update-download: missing url");
                };
                if let Some(entity) = update::updater(cx) {
                    let candidate = api::updater::UpdateCandidate {
                        version: version.to_owned(),
                        url: url.to_owned(),
                    };
                    entity.update(cx, |this, cx| this.start_download(candidate, cx));
                }
            }
            #[cfg(not(any(target_os = "macos", windows)))]
            let _ = args;
            Handled::result(Ok(state_json(cx)))
        }),
        capability("update-install", false, move |_, window, cx| {
            #[cfg(any(target_os = "macos", windows))]
            {
                if let Some(window) = window {
                    update::begin_ready_update_install(window, cx);
                } else {
                    let _ = crate::plugin_host::in_main_window(
                        None,
                        &install_ws,
                        cx,
                        |_, window, cx| update::begin_ready_update_install(window, cx),
                    );
                }
            }
            #[cfg(not(any(target_os = "macos", windows)))]
            let _ = (window, &install_ws);
            Handled::result(Ok(state_json(cx)))
        }),
        capability("update-move-app", false, |_, _, cx| {
            #[cfg(target_os = "macos")]
            if let Some(entity) = update::updater(cx) {
                entity.update(cx, |this, cx| this.move_app_to_applications(cx));
            }
            Handled::result(Ok(state_json(cx)))
        }),
    ]
}

fn capability(
    name: &'static str,
    requires_args: bool,
    handler: impl Fn(&Value, Option<&mut gpui::Window>, &mut App) -> Handled + 'static,
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
