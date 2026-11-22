//! `smelt/pilot`：给外壳和所有插件的纯同步宿主模块，JS 里 `nativeModule("smelt/pilot")` 拿 `Smelt`。
//!
//! 跑在 NSH 的引擎线程，不碰 GPUI、不做磁盘或网络 IO；只放和运行时协议共用同一份规则的纯函数
//! （路径规范、项目名、新建会话启动规格、UUID、本机系统标识），不认识任何具体插件。

use nsh_core::prelude::*;
use smelt_plugin_api::{
    self as api,
    model::{
        agent_host::AgentHostSettings, new_session::NewSessionAction, runtime_path::PathStyle,
        workspace_directory,
    },
};

fn parse_style(style: &str) -> Result<PathStyle, String> {
    match style {
        "posix" => Ok(PathStyle::Posix),
        "windows" => Ok(PathStyle::Windows),
        other => Err(format!(
            "style 要是 \"posix\" 或 \"windows\"，收到 {other:?}"
        )),
    }
}

#[module]
pub struct Smelt;

#[module]
impl Smelt {
    /// 新的 UUID v4（守护会话 id、新建对话 id 等）。
    pub fn new_session_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// 本机操作系统标识，同 Node `os.platform()`（`"darwin"` / `"linux"` / `"win32"`）。
    pub fn local_os() -> String {
        match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        }
        .to_owned()
    }

    pub fn path_clean(style: String, path: String) -> Result<String, String> {
        Ok(parse_style(&style)?.clean(&path))
    }

    pub fn path_join(style: String, base: String, child: String) -> Result<String, String> {
        Ok(parse_style(&style)?.join(&base, &child))
    }

    pub fn path_file_name(style: String, path: String) -> Result<Option<String>, String> {
        Ok(parse_style(&style)?.file_name(&path).map(str::to_owned))
    }

    pub fn path_parent(style: String, path: String) -> Result<Option<String>, String> {
        Ok(parse_style(&style)?.parent(&path))
    }

    pub fn path_is_absolute(style: String, path: String) -> Result<bool, String> {
        Ok(parse_style(&style)?.is_absolute(&path))
    }

    pub fn path_starts_with(style: String, path: String, base: String) -> Result<bool, String> {
        Ok(parse_style(&style)?.starts_with(&path, &base))
    }

    pub fn project_labels(style: String, roots: Vec<String>) -> Result<Vec<String>, String> {
        let style = parse_style(&style)?;
        let refs: Vec<&str> = roots.iter().map(String::as_str).collect();
        Ok(workspace_directory::project_labels(style, &refs))
    }

    pub fn project_root_of(
        style: String,
        roots: Vec<String>,
        path: String,
    ) -> Result<Option<String>, String> {
        let style = parse_style(&style)?;
        Ok(workspace_directory::project_root_of(style, &roots, &path).map(str::to_owned))
    }

    pub fn new_session_icon(action_json: String) -> Result<String, String> {
        let action: NewSessionAction = serde_json::from_str(&action_json)
            .map_err(|error| format!("new_session_icon：action 不对：{error}"))?;
        Ok(api::new_session_icon(&action.target))
    }

    pub fn conversation_acp(
        settings_json: String,
        option_id: String,
        sid: String,
        cwd: String,
    ) -> Result<Option<String>, String> {
        let settings: AgentHostSettings = serde_json::from_str(&settings_json)
            .map_err(|error| format!("conversation_acp：settings 不对：{error}"))?;
        match api::conversation_acp(&settings, &option_id, sid, cwd) {
            Some(acp) => serde_json::to_string(&acp)
                .map(Some)
                .map_err(|error| format!("conversation_acp：序列化失败：{error}")),
            None => Ok(None),
        }
    }
}

/// 交给 `set_ui_sets` 的那组。
pub(crate) fn registry() -> Registry {
    let mut registry = Registry::ui_set("smelt/pilot");
    registry.module::<Smelt>();
    registry
}
