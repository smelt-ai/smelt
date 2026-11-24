//! `smelt/host`：给外壳和所有插件的纯同步宿主模块与原生矢量图标组件：
//! - JS 里 `nativeModule("smelt/host")` 拿 `Smelt`（路径、项目名、启动规格、额度解析、UUID、二维码等纯函数）；
//! - JS 里 `import { Icon } from "smelt/host"` 拿一等 NSH 矢量 `<Icon path="..." />`（走 `nsh_core::element::svg`，
//!   原生支持 `color` / `opacity` / `transform` / `transition` / `animate-spin`，不经 GPUI `NativeComponent`）。

use std::collections::BTreeMap;

use nsh_core::{
    element::{ImageSrc, svg},
    prelude::*,
};
use smelt_plugin_api::{
    self as api,
    model::{
        acp_chat::{diff_line_stats, is_interrupt_marker},
        agent_definition::agent_engine_kinds,
        agent_host::AgentHostSettings,
        agent_kind::{
            ConversationAgentKind, ConversationLaunchSpec, HistorySourceKind, TerminalAgentKind,
            agent_id_from_launch,
        },
        agent_status::AgentStatus,
        automation::is_managed_workspace,
        chrono::DateTime,
        copilot_quota::format_amount,
        daemon_state::DaemonSessionState,
        file_link::parse_file_link,
        new_session::{LaunchEntry, NewSessionAction, default_launch_entries},
        pi_mcp::PI_MCP_PRESETS,
        pi_model_settings::{api_base_url_problem, custom_provider_id_problem},
        provider_quota::ProviderQuotaData,
        quota_service::QuotaTarget,
        runtime_path::PathStyle,
        session_handoff::{HandoffContext, HandoffPeer, build_handoff_prompt},
        session_history::{SessionDetail, Turn, handoff_turns_from_history},
        workspace_directory::{self, WorkspaceAcp},
        workspace_override::{config_dir_env_var, split_env_assignment},
    },
};

const EMPTY_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>"#;

#[derive(Debug, Clone, PartialEq, Props)]
pub struct IconProps {
    pub path: Prop<SharedString>,
}

/// 随包带的线性/品牌 SVG 图标：以一等 NSH 矢量节点渲染，`currentColor` 自动继承元素的 `color` 样式，
/// 透明度、旋转、过渡和转圈动效全部由 JS/CSS 直接控制。
///
/// 挂 `.overflow_hidden()`（在 NSH 映射为 `taffy::Overflow::Clip`）：Taffy 在 `PerformLayout` 阶段
/// 会以 `known_dimensions = None` 量叶节点并把返回的自然尺寸写进 `scrollable_overflow_rect`，
/// Phosphor 等仅声明 `viewBox="0 0 256 256"` 而无 `width/height` 属性的 SVG 若不截断溢出，
/// 会把外层 `<Scroll>` 的可滚范围凭空撑大 256pt。
#[component]
pub fn Icon(props: IconProps) -> impl IntoElement {
    svg(move || {
        let path = props.path.get();
        let markup = smelt_ui::assets::load(path.as_ref())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or(EMPTY_SVG);
        ImageSrc::svg_markup(markup)
    })
    .overflow_hidden()
}

#[derive(Debug, Clone, PartialEq, Props)]
pub struct VectorProps {
    pub markup: Prop<SharedString>,
}

/// 任意内联 SVG 矢量图形：走 NSH 一等矢量管线渲染（如分支图、自定义图表），不经 GPUI Canvas。
/// 同 [`Icon`]，截断 `viewBox` 自然尺寸向祖先 `<Scroll>` 的溢出传播。
#[component]
pub fn Vector(props: VectorProps) -> impl IntoElement {
    svg(move || {
        let markup = props.markup.get();
        if markup.is_empty() {
            ImageSrc::svg_markup(EMPTY_SVG)
        } else {
            ImageSrc::svg_markup(&markup)
        }
    })
    .overflow_hidden()
}

fn parse_style(style: &str) -> Result<PathStyle, String> {
    match style {
        "posix" => Ok(PathStyle::Posix),
        "windows" => Ok(PathStyle::Windows),
        other => Err(format!(
            "style 要是 \"posix\" 或 \"windows\"，收到 {other:?}"
        )),
    }
}

fn parse_settings(settings_json: &str, what: &str) -> Result<AgentHostSettings, String> {
    serde_json::from_str(settings_json).map_err(|error| format!("{what}：settings 不对：{error}"))
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

    /// 当前桌面版本号（优先发版号，开发构建回退到内部版本号）。
    pub fn smelt_version() -> String {
        api::smelt_version::RELEASE
            .unwrap_or(api::smelt_version::VERSION)
            .to_owned()
    }

    /// 把短文本编成二维码点阵（`{ size, bits }`，`'1'` 为深色块、`'0'` 为浅色块）；内容过长返回 `None`。
    pub fn qr_matrix(text: String) -> Option<String> {
        let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
        let size = code.width();
        let bits: String = code
            .into_colors()
            .into_iter()
            .map(|c| match c {
                qrcode::Color::Dark => '1',
                qrcode::Color::Light => '0',
            })
            .collect();
        Some(serde_json::json!({ "size": size, "bits": bits }).to_string())
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

    pub fn path_strip_prefix(
        style: String,
        path: String,
        base: String,
    ) -> Result<Option<String>, String> {
        Ok(parse_style(&style)?.strip_prefix(&path, &base))
    }

    pub fn path_extension(style: String, path: String) -> Result<Option<String>, String> {
        Ok(parse_style(&style)?.extension(&path).map(str::to_owned))
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

    pub fn quota_target(id: String) -> Option<String> {
        let target = QuotaTarget::from_id(&id)?;
        let service = target.service();
        Some(
            serde_json::json!({
                "id": target.id(),
                "service": service.id(),
                "name": target.display_name(),
                "icon": service.icon_asset(),
            })
            .to_string(),
        )
    }

    pub fn quota_facts(data_json: String) -> Result<String, String> {
        let data: ProviderQuotaData = serde_json::from_str(&data_json)
            .map_err(|error| format!("quota_facts：data 不对：{error}"))?;
        let (windows, copilot) = match &data {
            ProviderQuotaData::Windows(quota) => (
                quota
                    .windows
                    .iter()
                    .map(|window| {
                        serde_json::json!({
                            "label": window.label,
                            "display_label": window.display_label(),
                            "scope": window.scope,
                            "used_percent": window.used_percent,
                            "resets_at_ms": window.resets_at_ms,
                            "reset_after_seconds": window.reset_after_seconds,
                        })
                    })
                    .collect::<Vec<_>>(),
                serde_json::Value::Null,
            ),
            ProviderQuotaData::Copilot(quota) => {
                let used = quota
                    .limit
                    .map(|limit| quota.used.unwrap_or((limit - quota.remaining).max(0.0)));
                (
                    Vec::new(),
                    serde_json::json!({
                        "unit": quota.unit.label(),
                        "remaining": format_amount(quota.remaining),
                        "limit": quota.limit.map(format_amount),
                        "used": used.map(format_amount),
                    }),
                )
            }
        };
        Ok(serde_json::json!({
            "short_text": data.short_text(),
            "used_percent": data.used_percent(),
            "windows": windows,
            "copilot": copilot,
        })
        .to_string())
    }

    /// 按与手机/桌面同一套 `smelt-model` 规则解析 Markdown 里的文件链接。`cwd` 传空串表示无基准目录。
    pub fn parse_file_link(
        style: String,
        target: String,
        cwd: String,
    ) -> Result<Option<String>, String> {
        let style = parse_style(&style)?;
        let cwd = (!cwd.is_empty()).then_some(cwd.as_str());
        Ok(parse_file_link(&target, cwd, style).map(|link| {
            serde_json::json!({
                "path": link.path,
                "line": link.line(),
                "fragment": link.fragment,
            })
            .to_string()
        }))
    }

    /// 识别 agent 回显的中断标记（和对话流共用 `smelt-model` 规则）。
    pub fn is_interrupt_marker(text: String) -> bool {
        is_interrupt_marker(&text)
    }

    /// 一处改动的增删行数 `[added, removed]`，和 `<SmeltDiffView>` 同一份按行比对。新建文件 `old` 传空串。
    pub fn diff_line_stats(old: String, new: String) -> Vec<usize> {
        let (added, removed) = diff_line_stats(&old, &new);
        vec![added, removed]
    }

    /// 拖进窗口的 `file://` 按本机规则解成路径；不是文件 URL 的原样返回。
    pub fn file_uri_to_path(uri: String) -> String {
        url::Url::parse(&uri)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok())
            .map_or(uri, |path| path.to_string_lossy().into_owned())
    }

    /// 工作目录是不是运行时按 id 给这条自动化分配的托管目录。`style` 是自动化所在机器的路径风格。
    pub fn is_managed_automation_workspace(
        style: String,
        path: String,
        automation_id: String,
    ) -> Result<bool, String> {
        Ok(is_managed_workspace(
            parse_style(&style)?,
            &path,
            &automation_id,
        ))
    }

    /// 自定义 Provider ID 哪里不对（`id_required` / `id_invalid`），对的给 `None`。和运行时保存时
    /// 拦的是同一份规则；提示的字插件自己写。
    pub fn custom_provider_id_problem(id: String) -> Option<String> {
        custom_provider_id_problem(&id).map(|problem| problem.as_str().to_string())
    }

    /// Provider 的 API 地址哪里不对（`base_url_invalid`），对的给 `None`。同上。
    pub fn api_base_url_problem(base_url: String) -> Option<String> {
        api_base_url_problem(&base_url).map(|problem| problem.as_str().to_string())
    }

    /// 内置 MCP 一键添加预设，按这台机器装没装远程桌面挑那一套，名字和说明按界面语言。
    pub fn pi_mcp_presets(remote_desktop: bool) -> String {
        let presets: Vec<_> = PI_MCP_PRESETS
            .iter()
            .map(|preset| {
                let preset = preset.for_machine(remote_desktop);
                serde_json::json!({
                    "id": preset.id,
                    "name": preset.name(),
                    "summary": preset.summary(),
                    "needs_sign_in": preset.transport.needs_sign_in(),
                    "extension_url": preset.extension_url,
                })
            })
            .collect();
        serde_json::Value::Array(presets).to_string()
    }

    /// 某个 agent 记住的最近选择（去重后），新会话先铺这一份。
    pub fn remembered_acp_config(
        settings_json: String,
        agent_id: String,
    ) -> Result<String, String> {
        let settings = parse_settings(&settings_json, "remembered_acp_config")?;
        let pairs = ConversationAgentKind::from_id(&agent_id)
            .map(|agent| settings.remembered_acp_config(agent))
            .unwrap_or_default();
        serde_json::to_string(&pairs).map_err(|error| format!("remembered_acp_config：{error}"))
    }

    /// 记一次用户显式选择，返回改后的设置；不该记或没变化时返回 `None`。
    pub fn remember_acp_config_value(
        settings_json: String,
        agent_id: String,
        config_id: String,
        value_id: String,
    ) -> Result<Option<String>, String> {
        let before = parse_settings(&settings_json, "remember_acp_config_value")?;
        let Some(agent) = ConversationAgentKind::from_id(&agent_id) else {
            return Ok(None);
        };
        let mut after = before.clone();
        after.remember_acp_config_value(agent, config_id, value_id);
        if after == before {
            return Ok(None);
        }
        serde_json::to_string(&after)
            .map(Some)
            .map_err(|error| format!("remember_acp_config_value：{error}"))
    }

    /// 导出内置 `HistorySourceKind::ALL`、`ConversationAgentKind::ALL`、产品智能体引擎与终端内置项元数据（与运行时同一份表）。
    pub fn agent_catalog() -> String {
        let history_sources: Vec<_> = HistorySourceKind::ALL
            .into_iter()
            .map(|kind| {
                serde_json::json!({
                    "id": kind.id(),
                    "label": kind.label(),
                    "short_label": kind.short_label(),
                    "acp_id": kind.acp().map(|acp| acp.id()),
                    "deletable": kind.history_deletable(),
                    "icon": kind.icon_asset(),
                })
            })
            .collect();
        let conversation_agents: Vec<_> = ConversationAgentKind::ALL
            .into_iter()
            .map(|kind| {
                serde_json::json!({
                    "id": kind.id(),
                    "label": kind.label(),
                    "short_label": kind.short_label(),
                    "icon": kind.icon_asset(),
                    "config_dir_env_var": config_dir_env_var(kind.id()),
                })
            })
            .collect();
        let agent_engines: Vec<_> = agent_engine_kinds()
            .iter()
            .map(|kind| {
                serde_json::json!({
                    "id": kind.id(),
                    "label": kind.label(),
                    "short_label": kind.short_label(),
                    "icon": kind.icon_asset(),
                    "config_dir_env_var": config_dir_env_var(kind.id()),
                })
            })
            .collect();
        let terminal_agent_ids: Vec<_> = TerminalAgentKind::ALL
            .into_iter()
            .map(TerminalAgentKind::id)
            .collect();
        serde_json::json!({
            "history_sources": history_sources,
            "conversation_agents": conversation_agents,
            "agent_engines": agent_engines,
            "terminal_agent_ids": terminal_agent_ids,
        })
        .to_string()
    }

    /// 项目目录若落在某个智能体的 space 下，返回其引擎 id（与 `AgentHostSettings::agent_for_space` 同一规则）。
    pub fn agent_for_space(settings_json: String, root: String) -> Result<Option<String>, String> {
        let settings: AgentHostSettings = serde_json::from_str(&settings_json)
            .map_err(|error| format!("agent_for_space：settings 不对：{error}"))?;
        Ok(settings
            .agent_for_space(&root)
            .and_then(|agent| agent.engine_kind())
            .map(|kind| kind.id().to_owned()))
    }

    /// 解析 `AgentHostSettings` 里的自定义 Agent 列表，标明其基础引擎、是否可启动、是否在历史来源表内。
    pub fn launchable_profiles(settings_json: String) -> Result<String, String> {
        let settings: AgentHostSettings = serde_json::from_str(&settings_json)
            .map_err(|error| format!("launchable_profiles：settings 不对：{error}"))?;
        let profiles: Vec<_> = settings
            .profiles
            .iter()
            .map(|profile| {
                let kind = profile.kind();
                let in_history_sources = kind
                    .is_some_and(|k| HistorySourceKind::ALL.contains(&HistorySourceKind::from(k)));
                serde_json::json!({
                    "id": profile.id,
                    "label": profile.label,
                    "kind": kind.map(|k| k.id()),
                    "launchable": settings.profile_launch_spec(profile).is_ok(),
                    "in_history_sources": in_history_sources,
                })
            })
            .collect();
        Ok(serde_json::Value::Array(profiles).to_string())
    }

    /// 按 `smelt-model` 同一份规则把历史会话轮次压成交接给目标 agent 的首条 prompt。
    pub fn handoff_prompt(payload_json: String) -> Result<String, String> {
        let payload: serde_json::Value = serde_json::from_str(&payload_json)
            .map_err(|error| format!("handoff_prompt：JSON 不对：{error}"))?;
        let source_agent = payload["source_agent"]
            .as_str()
            .and_then(ConversationAgentKind::from_id)
            .ok_or_else(|| "handoff_prompt：缺合法 source_agent".to_owned())?;
        let target_agent = payload["target_agent"]
            .as_str()
            .and_then(ConversationAgentKind::from_id)
            .ok_or_else(|| "handoff_prompt：缺合法 target_agent".to_owned())?;
        let source_profile_label = payload["source_profile_label"].as_str();
        let target_profile_label = payload["target_profile_label"].as_str();
        let source_title = payload["source_title"].as_str().unwrap_or_default();
        let cwd = payload["cwd"].as_str();
        let detail_val = &payload["detail"];
        let model = detail_val["model"].as_str().map(str::to_owned);
        let turns: Vec<Turn> = detail_val["turns"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|turn| Turn {
                        is_user: turn["is_user"].as_bool().unwrap_or(false),
                        timestamp: turn["timestamp_ms"]
                            .as_i64()
                            .and_then(DateTime::from_timestamp_millis),
                        text: turn["text"].as_str().unwrap_or_default().to_owned(),
                        tools: turn["tools"]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(serde_json::Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        tool_paths: turn["tool_paths"]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(serde_json::Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let detail = SessionDetail { turns, model };
        Ok(build_handoff_prompt(&HandoffContext {
            turns: handoff_turns_from_history(&detail),
            source_title,
            source: HandoffPeer::new(source_agent, source_profile_label),
            target: HandoffPeer::new(target_agent, target_profile_label),
            cwd,
            source_model: detail.model,
        }))
    }

    /// 结合那台机器的 `AgentHostSettings` 算出开始对话的 `WorkspaceAcp`（与 `smelt-model` 同源）。
    pub fn resolve_start_acp(
        settings_json: String,
        with_json: String,
        cwd: String,
        sid: String,
    ) -> Result<String, String> {
        let settings: AgentHostSettings = serde_json::from_str(&settings_json)
            .map_err(|error| format!("resolve_start_acp：settings 不对：{error}"))?;
        let with: serde_json::Value = serde_json::from_str(&with_json)
            .map_err(|error| format!("resolve_start_acp：with 不对：{error}"))?;
        let out = if let Some(def_id) = with.get("agent_definition_id").and_then(|v| v.as_str()) {
            resolve_agent_acp(&settings, def_id, cwd, sid)
        } else if let Some(agent) = with.get("agent").and_then(|v| v.as_str()) {
            let profile_id = with.get("profile_id").and_then(|v| v.as_str());
            match ConversationAgentKind::from_id(agent) {
                Some(kind) => resolve_engine_acp(&settings, kind, profile_id, cwd, sid),
                None => resolve_err("error.unknown_agent", serde_json::json!({ "agent": agent })),
            }
        } else {
            resolve_err("error.choose_engine", serde_json::json!({}))
        };
        Ok(out.to_string())
    }

    /// 结合那台机器的 `AgentHostSettings` 算出续接历史会话的 `WorkspaceAcp`。
    pub fn resolve_resume_acp(
        settings_json: String,
        args_json: String,
        sid: String,
    ) -> Result<String, String> {
        let settings: AgentHostSettings = serde_json::from_str(&settings_json)
            .map_err(|error| format!("resolve_resume_acp：settings 不对：{error}"))?;
        let args: serde_json::Value = serde_json::from_str(&args_json)
            .map_err(|error| format!("resolve_resume_acp：args 不对：{error}"))?;
        let agent = args["agent"].as_str().unwrap_or_default();
        let profile_id = args["profile_id"].as_str();
        let cwd = args["cwd"].as_str().unwrap_or_default().to_owned();
        let resume_id = args["resume_id"].as_str().unwrap_or_default().to_owned();
        let arg_def = args["agent_definition_id"].as_str().map(str::to_owned);
        let Some(kind) = HistorySourceKind::from_id(agent).and_then(HistorySourceKind::acp) else {
            return Ok(
                resolve_err("error.no_acp_resume", serde_json::json!({ "agent": agent }))
                    .to_string(),
            );
        };
        let mut acp = match engine_launch_spec(&settings, kind, profile_id) {
            Ok(launch) => WorkspaceAcp {
                cwd: Some(cwd.clone()),
                sid,
                agent: kind.id().to_owned(),
                launch,
                profile_id: profile_id.map(str::to_owned),
                ..WorkspaceAcp::default()
            },
            Err(err) => return Ok(err.to_string()),
        };
        let definition = arg_def
            .or_else(|| settings.agent_for_space(&cwd).map(|a| a.id.clone()))
            .filter(|id| settings.find_agent(id).is_some());
        if definition.is_some() {
            acp.config_values = agent_full_access_config(kind);
        }
        acp.agent_definition_id = definition;
        acp.history_session_id = Some(resume_id);
        Ok(serde_json::json!({ "ok": true, "acp": acp }).to_string())
    }

    /// 结合那台机器的 `AgentHostSettings` 算出交接给另一个 agent 的 `WorkspaceAcp`。
    pub fn resolve_handoff_acp(
        settings_json: String,
        args_json: String,
        sid: String,
    ) -> Result<String, String> {
        let settings: AgentHostSettings = serde_json::from_str(&settings_json)
            .map_err(|error| format!("resolve_handoff_acp：settings 不对：{error}"))?;
        let args: serde_json::Value = serde_json::from_str(&args_json)
            .map_err(|error| format!("resolve_handoff_acp：args 不对：{error}"))?;
        let agent = args["agent"].as_str().unwrap_or_default();
        let profile_id = args["profile_id"].as_str();
        let cwd = args["cwd"].as_str().unwrap_or_default().to_owned();
        let prompt = args["prompt"].as_str().unwrap_or_default().to_owned();
        let Some(kind) = ConversationAgentKind::from_id(agent) else {
            return Ok(resolve_err(
                "error.handoff_unknown_agent",
                serde_json::json!({ "agent": agent }),
            )
            .to_string());
        };
        let launch = match engine_launch_spec(&settings, kind, profile_id) {
            Ok(launch) => launch,
            Err(err) => return Ok(err.to_string()),
        };
        let acp = WorkspaceAcp {
            cwd: Some(cwd),
            sid,
            agent: kind.id().to_owned(),
            launch,
            profile_id: profile_id.map(str::to_owned),
            pending_prompt: Some(prompt).filter(|p| !p.trim().is_empty()),
            ..WorkspaceAcp::default()
        };
        Ok(serde_json::json!({ "ok": true, "acp": acp }).to_string())
    }

    /// 算出在终端里续接一段历史会话的启动项标签与完整命令行。
    pub fn resolve_cli_resume(
        style: String,
        entries_json: String,
        settings_json: String,
        agent_id: String,
        profile_id: String,
        resume_id: String,
    ) -> Result<String, String> {
        let style = parse_style(&style)?;
        let entries: Vec<LaunchEntry> = serde_json::from_str(&entries_json)
            .map_err(|error| format!("resolve_cli_resume：entries 不对：{error}"))?;
        let Some(agent) = HistorySourceKind::from_id(&agent_id) else {
            return Ok(resolve_err(
                "error.unknown_history_source",
                serde_json::json!({ "source": agent_id }),
            )
            .to_string());
        };
        if agent.cli_resume_syntax().is_none() {
            return Ok(resolve_err(
                "error.no_cli_resume",
                serde_json::json!({ "agent": agent.short_label() }),
            )
            .to_string());
        }
        let env: BTreeMap<String, String> = if !profile_id.is_empty() {
            let settings: AgentHostSettings = serde_json::from_str(&settings_json)
                .map_err(|error| format!("resolve_cli_resume：settings 不对：{error}"))?;
            let Some(kind) = agent.acp() else {
                return Ok(resolve_err(
                    "error.no_custom_agent",
                    serde_json::json!({ "agent": agent.short_label() }),
                )
                .to_string());
            };
            match engine_launch_spec(&settings, kind, Some(&profile_id)) {
                Ok(spec) => spec.env,
                Err(err) => return Ok(err.to_string()),
            }
        } else {
            BTreeMap::new()
        };
        let Some(command) = cli_resume_line(style, &entries, agent, &resume_id, &env) else {
            return Ok(resolve_err(
                "error.no_cli_resume",
                serde_json::json!({ "agent": agent.short_label() }),
            )
            .to_string());
        };
        Ok(serde_json::json!({
            "ok": true,
            "label": format!("{} TUI", agent.short_label()),
            "command": command,
        })
        .to_string())
    }

    /// 单个守护会话状态的运行态（`AgentStatus::from_daemon_state`）：`"needs_you"` / `"running"` / `"idle"` / `None`。
    pub fn daemon_state_status(state_json: String) -> Result<Option<String>, String> {
        let state: DaemonSessionState = serde_json::from_str(&state_json)
            .map_err(|error| format!("daemon_state_status：state 不对：{error}"))?;
        Ok(
            AgentStatus::from_daemon_state(&state).map(|status| match status {
                AgentStatus::NeedsYou => "needs_you".to_owned(),
                AgentStatus::Running => "running".to_owned(),
                AgentStatus::Idle => "idle".to_owned(),
            }),
        )
    }

    /// 从终端启动命令行识别内置 agent id（与 `smelt-model` 同源）。
    pub fn agent_id_from_launch(launch: String) -> Option<String> {
        agent_id_from_launch(&launch).map(str::to_owned)
    }
}

fn resolve_err(code: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "ok": false, "code": code, "params": params })
}

fn agent_full_access_config(kind: ConversationAgentKind) -> Vec<(String, String)> {
    kind.task_params()
        .full_access_mode
        .map(|mode| ("mode".to_owned(), mode.to_owned()))
        .into_iter()
        .collect()
}

fn engine_launch_spec(
    settings: &AgentHostSettings,
    kind: ConversationAgentKind,
    profile_id: Option<&str>,
) -> Result<ConversationLaunchSpec, serde_json::Value> {
    let Some(id) = profile_id else {
        if kind == ConversationAgentKind::Custom {
            return Err(resolve_err(
                "error.custom_needs_profile",
                serde_json::json!({}),
            ));
        }
        return Ok(settings.acp_launch_for(kind));
    };
    let Some(profile) = settings.find_profile(id) else {
        return Err(resolve_err(
            "error.profile_missing",
            serde_json::json!({ "id": id }),
        ));
    };
    if profile.kind() != Some(kind) {
        return Err(resolve_err(
            "error.profile_kind_mismatch",
            serde_json::json!({ "profile": profile.label, "engine": kind.short_label() }),
        ));
    }
    settings
        .profile_launch_spec(profile)
        .map_err(|message| resolve_err("error.raw", serde_json::json!({ "message": message })))
}

fn resolve_agent_acp(
    settings: &AgentHostSettings,
    definition_id: &str,
    cwd: String,
    sid: String,
) -> serde_json::Value {
    let Some(definition) = settings.find_agent(definition_id) else {
        return resolve_err("error.agent_gone", serde_json::json!({}));
    };
    let Some(kind) = definition.engine_kind() else {
        return resolve_err("error.choose_engine", serde_json::json!({}));
    };
    if !agent_engine_kinds().contains(&kind) {
        return resolve_err(
            "error.engine_not_available",
            serde_json::json!({ "engine": kind.short_label() }),
        );
    }
    let (kind, launch) = match settings.resolve_agent(definition) {
        Ok(pair) => pair,
        Err(message) => {
            return resolve_err("error.raw", serde_json::json!({ "message": message }));
        }
    };
    let mut config_values = agent_full_access_config(kind);
    if let Some(model) = definition.model_config_value() {
        config_values.push(("model".to_owned(), model));
    }
    let acp = WorkspaceAcp {
        cwd: Some(cwd),
        sid,
        agent: kind.id().to_owned(),
        launch,
        config_values,
        agent_definition_id: Some(definition.id.clone()),
        ..WorkspaceAcp::default()
    };
    serde_json::json!({ "ok": true, "acp": acp })
}

fn resolve_engine_acp(
    settings: &AgentHostSettings,
    kind: ConversationAgentKind,
    profile_id: Option<&str>,
    cwd: String,
    sid: String,
) -> serde_json::Value {
    let launch = match engine_launch_spec(settings, kind, profile_id) {
        Ok(launch) => launch,
        Err(err) => return err,
    };
    let acp = WorkspaceAcp {
        cwd: Some(cwd),
        sid,
        agent: kind.id().to_owned(),
        launch,
        profile_id: profile_id.map(str::to_owned),
        ..WorkspaceAcp::default()
    };
    serde_json::json!({ "ok": true, "acp": acp })
}

fn cli_resume_line(
    style: PathStyle,
    entries: &[LaunchEntry],
    agent: HistorySourceKind,
    resume_id: &str,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    let base = entries
        .iter()
        .filter(|entry| !entry.label.trim().is_empty() && !entry.command.trim().is_empty())
        .cloned()
        .chain(default_launch_entries())
        .find(|entry| command_matches_agent(style, &entry.command, agent))
        .map(|entry| entry.command)
        .unwrap_or_default();
    let command = cli_resume_command(agent, &base, resume_id)?;
    let mut line = String::new();
    for (name, value) in env {
        line.push_str(name);
        line.push('=');
        line.push_str(&env_value(value));
        line.push(' ');
    }
    line.push_str(&command);
    Some(line)
}

fn cli_resume_command(agent: HistorySourceKind, base: &str, resume_id: &str) -> Option<String> {
    let terminal = agent.terminal()?;
    let suffix = agent.cli_resume_syntax()?.suffix(&shell_quote(resume_id));
    let base = base.trim();
    Some(if base.is_empty() {
        format!("{} {suffix}", terminal.cli_program())
    } else {
        format!("{base} {suffix}")
    })
}

fn command_matches_agent(style: PathStyle, command: &str, agent: HistorySourceKind) -> bool {
    let Some(terminal) = agent.terminal() else {
        return false;
    };
    let program = command
        .split_whitespace()
        .find(|token| *token != "env" && split_env_assignment(token).is_none())
        .and_then(|token| style.file_name(token));
    program == Some(terminal.cli_program())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn env_value(value: &str) -> String {
    if value == "~" {
        return "\"$HOME\"".to_owned();
    }
    match value.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"/{}", shell_quote(rest)),
        None => shell_quote(value),
    }
}

/// 交给 `set_ui_sets` 的那组。
pub(crate) fn registry() -> Registry {
    let mut registry = Registry::ui_set("smelt/host");
    registry.register::<Icon>();
    registry.register::<Vector>();
    registry.module::<Smelt>();
    registry
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::Smelt;

    /// 插件手里的条目是 hub 压过图片的线格式（多了 `ref_id`、`preview_uri`，`data_b64` 置空），
    #[test]
    fn file_uris_become_local_paths() {
        assert_eq!(
            Smelt::file_uri_to_path("file:///tmp/my%20shot%20%E6%88%AA%E5%9B%BE.png".into()),
            "/tmp/my shot 截图.png"
        );
        assert_eq!(
            Smelt::file_uri_to_path("/plain/path.png".into()),
            "/plain/path.png"
        );
    }

    /// 校验 smelt-ui 资源表里全部 SVG（Phosphor 图标与第三方智能体图标）都能被 NSH 矢量管线无报错解析。
    #[test]
    fn every_smelt_ui_svg_parses_as_nsh_vector_drawing() {
        let paths = smelt_ui::assets::Assets
            .list("")
            .expect("列出 smelt-ui SVG 资产");
        assert!(!paths.is_empty());
        for path in paths {
            let bytes = smelt_ui::assets::load(path.as_ref())
                .unwrap_or_else(|| panic!("读不到资产 {path}"));
            let markup = std::str::from_utf8(bytes)
                .unwrap_or_else(|err| panic!("资产 {path} 不是 UTF-8：{err}"));
            let drawing = nsh_core::svg::drawing(markup)
                .unwrap_or_else(|err| panic!("NSH 解析 {path} 失败：{err:?}"));
            assert!(!drawing.ops.is_empty(), "NSH 解析 {path} 后绘制指令为空");
        }
    }

    /// endpoint id 有 64 个十六进制字符，确保典型配对串能编成二维码矩阵。
    #[test]
    fn pairing_uri_renders_to_a_qr_matrix() {
        let uri = format!(
            "smelt+iroh://{}/?code=ABCD-EFGH&relay=https%3A%2F%2Frelay.example.test",
            "a".repeat(64)
        );
        let raw = Smelt::qr_matrix(uri).expect("配对串必须能生成二维码");
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let size = parsed["size"].as_u64().unwrap() as usize;
        let bits = parsed["bits"].as_str().unwrap();
        assert!(size >= 21);
        assert_eq!(bits.len(), size * size);
    }

    #[test]
    fn file_link_catalog_and_handoff_match_model_rules() {
        let parsed =
            Smelt::parse_file_link("posix".into(), "src/main.rs:42".into(), "/repo".into())
                .unwrap()
                .expect("应解析出文件链接");
        let link: serde_json::Value = serde_json::from_str(&parsed).unwrap();
        assert_eq!(link["path"], "/repo/src/main.rs");
        assert_eq!(link["line"], 42);
        assert_eq!(
            Smelt::parse_file_link("posix".into(), "https://example.com".into(), "/repo".into())
                .unwrap(),
            None
        );

        let catalog: serde_json::Value = serde_json::from_str(&Smelt::agent_catalog()).unwrap();
        assert!(!catalog["history_sources"].as_array().unwrap().is_empty());
        let conv_agents = catalog["conversation_agents"].as_array().unwrap();
        assert!(!conv_agents.is_empty());
        assert_eq!(
            conv_agents
                .iter()
                .find(|a| a["id"] == "claude")
                .and_then(|a| a["config_dir_env_var"].as_str()),
            Some("CLAUDE_CONFIG_DIR")
        );
        assert!(
            catalog["agent_engines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["id"] == "pi")
        );
        assert!(
            catalog["terminal_agent_ids"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id == "claude")
        );

        let prompt = Smelt::handoff_prompt(
            serde_json::json!({
                "source_agent": "grok",
                "source_profile_label": null,
                "source_title": "老会话",
                "cwd": "/repo",
                "target_agent": "claude",
                "target_profile_label": null,
                "detail": {
                    "model": "grok-4.5",
                    "turns": [
                        { "is_user": true, "text": "接着干", "tools": [], "tool_paths": [] }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(prompt.contains("原会话由 Grok 进行"));
        assert!(prompt.contains("现在由你（Claude Code）接手"));
        assert!(prompt.contains("原会话使用的模型：grok-4.5"));
        assert!(prompt.contains("接着干"));
    }

    #[test]
    fn resolve_acp_and_cli_resume_match_model_rules() {
        use smelt_plugin_api::model::agent_definition::AgentDefinition;
        use smelt_plugin_api::model::agent_host::AgentHostSettings;
        use smelt_plugin_api::model::agent_kind::{AcpProfile, ConversationAgentKind};
        use smelt_plugin_api::model::new_session::LaunchEntry;
        use std::collections::BTreeMap;

        let mut settings = AgentHostSettings::fresh();
        settings.agent_spaces_root = Some("/home/u/.smelt/agents".into());
        settings.agents.push(AgentDefinition {
            id: "writer".into(),
            name: "写手".into(),
            engine_kind_id: "pi".into(),
            model_provider: "anthropic".into(),
            model_id: "opus".into(),
            ..AgentDefinition::default()
        });
        settings.profiles.push(AcpProfile {
            id: "quant".into(),
            kind_id: ConversationAgentKind::Claude.id().into(),
            label: "Quant".into(),
            command: String::new(),
            env: BTreeMap::from([("A".into(), "~/.claude-quant".into())]),
            workspace_dir: String::new(),
        });
        let settings_json = serde_json::to_string(&settings).unwrap();

        let started: serde_json::Value = serde_json::from_str(
            &Smelt::resolve_start_acp(
                settings_json.clone(),
                serde_json::json!({ "agent_definition_id": "writer" }).to_string(),
                "/space".into(),
                "acp-1".into(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(started["ok"], true);
        assert_eq!(started["acp"]["agent"], "pi");
        assert_eq!(started["acp"]["agent_definition_id"], "writer");
        assert!(
            started["acp"]["config_values"]
                .as_array()
                .unwrap()
                .iter()
                .any(|pair| pair == &serde_json::json!(["model", "anthropic/opus"]))
        );

        let resumed: serde_json::Value = serde_json::from_str(
            &Smelt::resolve_resume_acp(
                settings_json.clone(),
                serde_json::json!({
                    "agent": "pi",
                    "profile_id": null,
                    "cwd": "/home/u/.smelt/agents/writer",
                    "resume_id": "h1",
                    "agent_definition_id": null,
                })
                .to_string(),
                "acp-2".into(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(resumed["ok"], true);
        assert_eq!(resumed["acp"]["agent_definition_id"], "writer");
        assert_eq!(resumed["acp"]["history_session_id"], "h1");

        let handed: serde_json::Value = serde_json::from_str(
            &Smelt::resolve_handoff_acp(
                settings_json.clone(),
                serde_json::json!({
                    "agent": "codex",
                    "profile_id": null,
                    "cwd": "/repo",
                    "prompt": "接着做",
                })
                .to_string(),
                "acp-3".into(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(handed["ok"], true);
        assert_eq!(handed["acp"]["pending_prompt"], "接着做");

        let entries = vec![LaunchEntry {
            label: "Claude".into(),
            command: "CLAUDE_CONFIG_DIR=/x claude --allow-all".into(),
            provider: None,
        }];
        let cli: serde_json::Value = serde_json::from_str(
            &Smelt::resolve_cli_resume(
                "posix".into(),
                serde_json::to_string(&entries).unwrap(),
                settings_json,
                "claude".into(),
                "quant".into(),
                "a'b".into(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(cli["ok"], true);
        assert_eq!(cli["label"], "Claude TUI");
        assert_eq!(
            cli["command"],
            "A=\"$HOME\"/'.claude-quant' CLAUDE_CONFIG_DIR=/x claude --allow-all --resume 'a'\\''b'"
        );
    }

    #[test]
    fn svg_icons_and_vectors_do_not_overflow_enclosing_scroll_content() {
        use nsh_core::{
            ApproxTextEngine, Core, MountItem, Points, Size, ViewKind, element::Root, prelude::*,
        };

        use super::{icon, vector};

        #[component]
        fn ScrollWithPhosphorIcons() -> impl IntoElement {
            let big_svg: SharedString =
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256"><path d="M0 0h256v256H0z"/></svg>"#
                    .into();
            scroll().w(px(278.0)).max_h(px(420.0)).child(
                view()
                    .flex_col()
                    .w(relative(1.0))
                    .p(px(4.0))
                    .child(view().h(px(285.0)))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .justify_end()
                            .h(px(28.0))
                            .child(
                                icon()
                                    .path(SharedString::from("icons/star.svg"))
                                    .w(px(13.0))
                                    .h(px(13.0)),
                            )
                            .child(vector().markup(big_svg).w(px(13.0)).h(px(13.0))),
                    )
                    .child(view().h(px(84.0))),
            )
        }

        let mut core = Core::new(ApproxTextEngine, Size::new(Points(800.0), Points(600.0)));
        core.mount_root(&Root::new::<ScrollWithPhosphorIcons>(|| ()));
        let batch = core.commit().expect("initial commit");

        let scroll_id = batch
            .items
            .iter()
            .find_map(|item| match item {
                MountItem::Create {
                    id,
                    kind: ViewKind::Scroll,
                } => Some(*id),
                _ => None,
            })
            .expect("scroll view created");
        let content = batch
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                MountItem::ScrollContent { id, size } if *id == scroll_id => Some(*size),
                _ => None,
            })
            .expect("scroll content emitted");

        // 4 + 285 + 28 + 84 + 4 = 405pt；viewBox 256×256 的 SVG 缩到 13×13 后绝不能把可滚范围撑到 289+256。
        assert_eq!(content, Size::new(Points(278.0), Points(405.0)));
    }
}
