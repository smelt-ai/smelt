//! 设置面板：外观 / LLM / 启动参数 / 更新 等分组，含嵌入式设置页
//! （主窗口右上角齿轮）和独立设置窗口共用的渲染逻辑。
//!
//! 跟 git_panel.rs / file_tree.rs 同一个套路：从 main.rs 拆出来的 `impl Workspace`
//! 方法 + 独立类型/函数，字段仍然声明在 main.rs 的 `Workspace` struct 里。
//!
//! 自动更新（`update_status`/`check_for_update`/`upgrade_daemon_seamless` 等）**不在
//! 这里**——那是应用级生命周期状态，不属于任何一个面板，仍留在 main.rs；这里的
//! 「更新」SettingPage 只是读它、展示它、提供按钮触发它。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::input::Input;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::notification::Notification;
use gpui_component::progress::Progress;
// use gpui_component::radio::{Radio, RadioGroup}; // 宠物删除后未用
use gpui_component::setting::{
    SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_component::*;

use crate::{Workspace, agent, terminal, terminal_view, updater};

// ===================== 外观 / 启动 配置类型 =====================

fn default_theme_mode() -> ThemeMode {
    ThemeMode::Dark
}

/// 老版本 appearance.json 没有 font_px 字段时的回退，跟 terminal_view::FONT_PX_ATOM
/// 的出厂默认值保持一致。
fn default_font_px() -> u32 {
    13
}

/// `bg_color` 从未被用户改过时的出厂值——终端背景层要不要跟着主题模式自动换色，
/// 就看当前值是不是还等于这个（见 `Appearance::bg_color_is_default`）。
const DEFAULT_BG_COLOR: u32 = 0x1a1b26;

/// 终端外观设置（全局单例，供所有终端渲染读取；存 ~/.smelt/appearance.json）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Appearance {
    /// 终端底色（0xRRGGBB）。
    pub bg_color: u32,
    /// 背景图片绝对路径（None = 无）。
    pub bg_image: Option<String>,
    /// 不透明度 0.3–1.0；<1 时窗口转透明/模糊，桌面透出。
    pub opacity: f32,
    /// 旧配置兼容字段；液态玻璃现已始终启用。
    pub blur: bool,
    /// 明暗主题模式。
    #[serde(default = "default_theme_mode")]
    pub theme_mode: ThemeMode,
    /// 终端字号（px）。
    #[serde(default = "default_font_px")]
    pub font_px: u32,
    /// 终端字体族。空 = 出厂默认（terminal_view::DEFAULT_FONT_FAMILY）；填了但机器上
    /// 没装时，渲染/测量会一致地落到 Menlo 兜底（见 terminal_view::terminal_font）。
    #[serde(default)]
    pub font_family: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            bg_color: DEFAULT_BG_COLOR,
            bg_image: None,
            opacity: 1.0,
            blur: true,
            theme_mode: ThemeMode::Dark,
            font_px: default_font_px(),
            font_family: String::new(),
        }
    }
}

impl Global for Appearance {}

impl Appearance {
    /// 据当前设置推导窗口背景外观。
    pub fn window_bg(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Blurred
    }

    /// `bg_color` 是否还是没被用户碰过的出厂值。是的话终端背景层该跟主题模式自动
    /// 切换（见 terminal_view.rs 的 bg_layer）；用户显式选过颜色后就不再跟随，
    /// 保留其选择（深浅色模式来回切也不丢）。
    pub fn bg_color_is_default(&self) -> bool {
        self.bg_color == DEFAULT_BG_COLOR
    }
}

/// 把主题模式落到所有吃颜色的层：gpui-component 部件、自绘 UI 语义色板、终端调色板。
/// **唯一入口**——三处必须同时切，漏一处就是「面板变浅了但终端还是黑的」这种半吊子。
/// 只改全局态不重绘，调用方自己决定什么时候 `cx.refresh_windows()`
/// （启动时还没有窗口，切换时才需要）。
pub fn apply_theme_mode(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    crate::ui_theme::set_light(!mode.is_dark());
    terminal::set_dark_mode(mode.is_dark());
    // Theme::change 装的是组件库自带色板，跟 ui_theme 是两套值——同屏里
    // `t.border` 和 `ui_theme::border_mid()` 挨着出现就会差一档。这里按语义位
    // 把组件库主题覆写成 ui_theme 的值，色真源收敛成一个。
    // 覆写必须在 Theme::change 之后：它会整套 apply_config 覆盖回默认。
    crate::ui_theme::apply_to_component_theme(cx);
}

/// 外观设置文件路径：~/.smelt/appearance.json。
fn appearance_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("appearance.json"))
}

/// 读取外观设置；缺失/损坏回退默认。
pub fn load_appearance() -> Appearance {
    crate::json_store::load_json(appearance_path())
}

/// 写回外观设置（失败静默忽略）。
fn save_appearance(a: &Appearance) {
    crate::json_store::save_json(appearance_path(), a)
}

/// 项目行「+」下拉菜单里的一条可配置启动项：显示名 + shell 启动命令。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct LaunchEntry {
    pub label: String,
    pub command: String,
}

/// 出厂默认启动项：与当前常用配置对齐（各 agent 默认带全权限参数）。
/// 用户可在设置里增删改；需要更保守时把参数删掉即可。
/// 「继续上次」不放默认里，需要的人自己在设置里加。
pub fn default_launch_entries() -> Vec<LaunchEntry> {
    vec![
        LaunchEntry {
            label: "Claude Code".into(),
            command: "claude --dangerously-skip-permissions".into(),
        },
        LaunchEntry {
            label: "Codex".into(),
            command: "codex --dangerously-bypass-approvals-and-sandbox".into(),
        },
        LaunchEntry {
            label: "Copilot".into(),
            command: "copilot --allow-all".into(),
        },
        LaunchEntry {
            label: "Grok".into(),
            command: "grok".into(),
        },
    ]
}

/// 项目行「+」可配置启动项列表（全局单例，存 ~/.smelt/launch.json）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchConfig {
    /// 除固定的「新建终端」「新建 Worktree…」外，下拉菜单里的启动项。
    pub entries: Vec<LaunchEntry>,
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            entries: default_launch_entries(),
        }
    }
}

/// 按命令前缀猜侧栏/菜单图标（自定义 agent 走通用终端图标）。
pub fn icon_for_launch_command(command: &str) -> IconName {
    let cmd = command.trim();
    if cmd.starts_with("claude") {
        IconName::Asterisk
    } else if cmd.starts_with("codex") {
        IconName::Bot
    } else if cmd.starts_with("copilot") {
        IconName::Github
    } else if cmd.starts_with("grok") {
        IconName::Bot
    } else {
        IconName::SquareTerminal
    }
}

/// 过滤出可展示的启动项（名/命令非空）。
pub fn active_launch_entries(cx: &App) -> Vec<LaunchEntry> {
    cx.global::<LaunchConfig>()
        .entries
        .iter()
        .filter(|e| !e.label.trim().is_empty() && !e.command.trim().is_empty())
        .cloned()
        .collect()
}

impl Global for LaunchConfig {}

fn launch_config_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("launch.json"))
}

/// 磁盘上的原始形状：兼容旧版「全权限」三开关，也兼容新版 `entries` 列表。
/// `entries: None` 表示文件里没写这个键（旧格式）→ 迁到出厂默认并回写；
/// `Some([])` 表示用户清空了列表，照用。
#[derive(serde::Serialize, serde::Deserialize)]
struct LaunchConfigFile {
    #[serde(default)]
    entries: Option<Vec<LaunchEntryFile>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct LaunchEntryFile {
    label: String,
    command: String,
}

impl From<LaunchEntryFile> for LaunchEntry {
    fn from(entry: LaunchEntryFile) -> Self {
        Self {
            label: entry.label,
            command: entry.command,
        }
    }
}

/// 读取启动配置；缺失/损坏/旧格式（无 `entries`）回退出厂默认并写成新格式。
pub fn load_launch_config() -> LaunchConfig {
    let Some(path) = launch_config_path() else {
        return LaunchConfig::default();
    };
    load_launch_config_from_path(&path)
}

fn load_launch_config_from_path(path: &std::path::Path) -> LaunchConfig {
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return LaunchConfig::default();
    };
    let Ok(file) = serde_json::from_str::<LaunchConfigFile>(&raw) else {
        return LaunchConfig::default();
    };
    match file.entries {
        Some(entries) => LaunchConfig {
            entries: entries.into_iter().map(LaunchEntry::from).collect(),
        },
        None => {
            // 旧版只有全权限开关：直接用出厂默认（已含全权限参数）并回写。
            let c = LaunchConfig::default();
            crate::json_store::save_json(Some(path.to_path_buf()), &c);
            c
        }
    }
}

/// 写回启动配置（失败静默忽略）。
fn save_launch_config(c: &LaunchConfig) {
    crate::json_store::save_json(launch_config_path(), c)
}

/// 改启动配置全局 + 存盘，不触发 view 重绘，用法同 [`apply_appearance`]。
fn apply_launch_config(f: impl FnOnce(&mut LaunchConfig), cx: &mut App) {
    let mut c = cx.global::<LaunchConfig>().clone();
    f(&mut c);
    save_launch_config(&c);
    cx.set_global(c);
}

// ===================== Agent UI / Claude hooks（B 路线） =====================
//
// AcpAgentKind / AcpProfile 搬进 smelt-core（本身不需要 GPUI），AgentUiConfig
// （需要 `gpui::Global`）搬进 smelt-ui——都是 acp_view.rs 独立成 smelt-acp-view
// crate 之后要跨 crate 共用的数据模型。这里重导出成原来的裸名字，本文件剩下
// 的 UI 渲染代码（acp_cmd_setting_item、手动添加 workspace 的编辑器等）不用
// 逐处改路径。
pub use smelt_core::agent_kind::{AcpAgentKind, AcpProfile};
pub use smelt_ui::agent_ui_config::{AgentUiConfig, apply_agent_ui, load_agent_ui_config};

/// 全局配置里某个 agent 的启动命令；配置还没装载就退回出厂值。
pub fn acp_cmd_for(agent: AcpAgentKind, cx: &App) -> String {
    cx.try_global::<AgentUiConfig>()
        .map(|c| c.acp_cmd_for(agent))
        .unwrap_or_else(|| agent.default_cmd())
}

/// 设置页只在用户覆盖内置适配器时显示原始命令；默认 `bunx`、CLI 参数等属于
/// smelt 的实现细节，不应要求用户理解或维护。
fn acp_cmd_setting_value(agent: AcpAgentKind, command: String) -> SharedString {
    if command == agent.default_cmd() {
        SharedString::default()
    } else {
        command.into()
    }
}

/// 设置页「Agent 集成」里每个 agent 一条自定义启动命令输入框（从枚举派生，加
/// 一家 agent 不用回来抄第四遍）。
fn acp_cmd_setting_item(agent: AcpAgentKind) -> SettingItem {
    SettingItem::new(
        format!("{} 自定义启动命令", agent.label()),
        SettingField::input(
            move |cx: &App| acp_cmd_setting_value(agent, acp_cmd_for(agent, cx)),
            move |v: SharedString, cx: &mut App| {
                let v = v.trim().to_string();
                // 留空 = 使用内置适配器（不是清成空串跑不起来）。
                let cmd = if v.is_empty() { agent.default_cmd() } else { v };
                apply_agent_ui(move |c| c.set_acp_cmd_for(agent, cmd), cx);
            },
        ),
    )
    .description(format!(
        "留空使用内置适配器；仅在需要替换适配器或追加参数时填写。\
         改动只影响之后新建的「{}」对话会话。",
        agent.label()
    ))
    .keywords(["acp", "对话", "agent", agent.id()])
}

/// smelt-notify 安装路径（与 package/安装脚本约定一致）。
pub fn smelt_notify_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| "/tmp".into())
        .join(".smelt")
        .join("bin")
        .join("smelt-notify")
}

/// 把 App / cargo 产物旁的 `smelt-notify` 原子同步到 hooks 使用的稳定路径。
///
/// hooks 会在 GUI 关闭时运行，不能直接指向可能被 DMG 覆盖的 App 包。每次启动都覆盖
/// managed 副本，避免“hook JSON 已升级、helper 仍是旧版”的半升级状态。rename 替换
/// 不影响已经启动的旧 helper：它仍持有旧 inode，下一次 hook 自动使用新文件。
pub fn sync_bundled_smelt_notify() -> std::io::Result<()> {
    let bundled = std::env::current_exe()?.with_file_name("smelt-notify");
    if !bundled.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("App 包内缺少 {}", bundled.display()),
        ));
    }

    let managed = smelt_notify_path();
    let dir = managed.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("无效的 smelt-notify 路径：{}", managed.display()),
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let staged = dir.join("smelt-notify.next");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(&bundled, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&staged)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&staged, permissions)?;
    }
    std::fs::rename(staged, managed)
}

fn claude_settings_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

const SMELT_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

const CODEX_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "SessionEnd",
];

const COPILOT_HOOK_EVENTS: &[&str] = &[
    "sessionStart",
    "sessionEnd",
    "userPromptSubmitted",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
    "subagentStart",
    "subagentStop",
    "preCompact",
    "agentStop",
    "errorOccurred",
    "permissionRequest",
    "notification",
];

const COPILOT_LEGACY_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "subagentStart",
    "SubagentStop",
    "PreCompact",
    "Stop",
    "ErrorOccurred",
    "PermissionRequest",
    "Notification",
];

fn copilot_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".copilot").join("hooks").join("smelt.json"))
}

fn codex_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("hooks.json"))
}

fn provider_hook_command(provider: &str, event: &str) -> String {
    format!(
        "SMELT_HOOK_PROVIDER={provider} SMELT_HOOK_EVENT={event} {}",
        shell_words::quote(&smelt_notify_path().to_string_lossy())
    )
}

fn command_uses_smelt_notify(command: &str) -> bool {
    let Ok(words) = shell_words::split(command) else {
        return false;
    };
    words
        .iter()
        .find(|word| {
            let Some((name, _)) = word.split_once('=') else {
                return true;
            };
            name.is_empty()
                || !name
                    .chars()
                    .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        })
        .and_then(|word| std::path::Path::new(word).file_name())
        .is_some_and(|name| name == "smelt-notify")
}

fn command_uses_current_smelt_hook(command: &str, event: &str) -> bool {
    command_uses_smelt_notify(command) && command.contains(&format!("SMELT_HOOK_EVENT={event}"))
}

fn smelt_notify_available() -> bool {
    smelt_notify_path().is_file()
}

/// hooks 接入状态缓存。设置页每次重绘都会检查 3 个 hooks 配置文件（read_to_string +
/// JSON 解析），ES 慢 open() 时会把 render 拖住；安装/卸载 hooks 后主动失效，
/// 其余时间 5s 内复用缓存结果（外部手改配置最多延迟 5s 反映）。
static HOOKS_INSTALLED_CACHE: OnceLock<Mutex<Option<(Instant, [bool; 3])>>> = OnceLock::new();

/// 返回 [claude, copilot, codex] 的 hooks 接入状态，带 5s 缓存。
pub fn hooks_installed_status() -> [bool; 3] {
    let cache = HOOKS_INSTALLED_CACHE.get_or_init(|| Mutex::new(None));
    let Ok(mut guard) = cache.lock() else {
        // 锁被占用（极端）：直接实时查，不阻塞 render。
        return [
            claude_hooks_installed(),
            copilot_hooks_installed(),
            codex_hooks_installed(),
        ];
    };
    if let Some((at, status)) = guard.as_ref() {
        if at.elapsed() < Duration::from_secs(5) {
            return *status;
        }
    }
    let status = [
        claude_hooks_installed(),
        copilot_hooks_installed(),
        codex_hooks_installed(),
    ];
    *guard = Some((Instant::now(), status));
    status
}

/// 安装/卸载 hooks 后调用，清缓存强制下次重扫。
pub fn invalidate_hooks_cache() {
    if let Some(cache) = HOOKS_INSTALLED_CACHE.get() {
        if let Ok(mut guard) = cache.lock() {
            *guard = None;
        }
    }
}

fn hook_file_installed(path: Option<std::path::PathBuf>, events: &[&str]) -> bool {
    if !smelt_notify_available() {
        return false;
    }
    let Some(path) = path else { return false };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(hooks) = root.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    events.iter().all(|event| {
        hooks
            .get(*event)
            .and_then(|v| v.as_array())
            .is_some_and(|groups| {
                groups.iter().any(|group| {
                    group
                        .get("hooks")
                        .and_then(|v| v.as_array())
                        .is_some_and(|handlers| {
                            handlers.iter().any(|handler| {
                                ["command", "bash"].iter().any(|key| {
                                    handler.get(*key).and_then(|v| v.as_str()).is_some_and(
                                        |command| command_uses_current_smelt_hook(command, event),
                                    )
                                })
                            })
                        })
                })
            })
    })
}

fn write_json_atomic(path: &std::path::Path, root: &serde_json::Value) -> Result<(), String> {
    let target = if std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        std::fs::canonicalize(path).map_err(|e| e.to_string())?
    } else {
        path.to_path_buf()
    };
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} 没有父目录", target.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let staged = parent.join(format!(
        ".{}.smelt-{}-{nonce}.tmp",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("hooks"),
        std::process::id()
    ));
    let out = serde_json::to_string_pretty(root).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&staged, out).map_err(|e| e.to_string())?;
    if let Ok(metadata) = std::fs::metadata(&target) {
        std::fs::set_permissions(&staged, metadata.permissions()).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&staged, &target).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        e.to_string()
    })
}

fn install_hook_file(
    path: std::path::PathBuf,
    events: &[&str],
    provider: &str,
    copilot_format: bool,
) -> Result<(), String> {
    let notify = smelt_notify_path();
    if !notify.is_file() {
        return Err(format!(
            "找不到 {}，请先编译安装 smelt-notify",
            notify.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut root = if path.is_file() {
        let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&raw).map_err(|e| format!("{} 不是有效 JSON：{e}", path.display()))?
    } else {
        serde_json::json!({})
    };
    if copilot_format {
        root["version"] = serde_json::json!(1);
    }
    let hooks = root
        .as_object_mut()
        .ok_or_else(|| format!("{} 根不是对象", path.display()))?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "hooks 不是对象".to_string())?;
    for event in events {
        let command = provider_hook_command(provider, event);
        let groups = hooks
            .entry(*event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("hooks.{event} 不是数组"))?;
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
                return true;
            };
            let contained_smelt = handlers.iter().any(|handler| {
                ["command", "bash"].iter().any(|key| {
                    handler
                        .get(*key)
                        .and_then(|v| v.as_str())
                        .is_some_and(command_uses_smelt_notify)
                })
            });
            if contained_smelt {
                handlers.retain(|handler| {
                    !["command", "bash"].iter().any(|key| {
                        handler
                            .get(*key)
                            .and_then(|v| v.as_str())
                            .is_some_and(command_uses_smelt_notify)
                    })
                });
            }
            !contained_smelt || !handlers.is_empty()
        });
        let handler = if copilot_format {
            serde_json::json!({ "type": "command", "bash": command, "timeoutSec": 3 })
        } else {
            serde_json::json!({ "type": "command", "command": command, "timeout": 3 })
        };
        groups.push(serde_json::json!({ "matcher": "", "hooks": [handler] }));
    }
    write_json_atomic(&path, &root)
}

fn uninstall_hook_file(path: Option<std::path::PathBuf>, events: &[&str]) -> Result<(), String> {
    let Some(path) = path else { return Ok(()) };
    if !path.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut root: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let Some(hooks) = root.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return Ok(());
    };
    for event in events {
        let Some(groups) = hooks.get_mut(*event).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        groups.retain_mut(|group| {
            let Some(handlers) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
                return true;
            };
            handlers.retain(|handler| {
                !["command", "bash"].iter().any(|key| {
                    handler
                        .get(*key)
                        .and_then(|v| v.as_str())
                        .is_some_and(command_uses_smelt_notify)
                })
            });
            !handlers.is_empty()
        });
        if groups.is_empty() {
            hooks.remove(*event);
        }
    }
    write_json_atomic(&path, &root)
}

pub fn copilot_hooks_installed() -> bool {
    if !smelt_notify_available() {
        return false;
    }
    let Some(path) = copilot_hooks_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(hooks) = root.get("hooks").and_then(|v| v.as_object()) else {
        return false;
    };
    COPILOT_HOOK_EVENTS.iter().all(|event| {
        hooks
            .get(*event)
            .and_then(|v| v.as_array())
            .is_some_and(|handlers| {
                handlers.iter().any(|handler| {
                    handler
                        .get("bash")
                        .and_then(|v| v.as_str())
                        .is_some_and(|command| command_uses_current_smelt_hook(command, event))
                })
            })
    })
}

pub fn codex_hooks_installed() -> bool {
    hook_file_installed(codex_hooks_path(), CODEX_HOOK_EVENTS)
}

pub fn install_copilot_hooks() -> Result<(), String> {
    let path = copilot_hooks_path().ok_or_else(|| "无 home 目录".to_string())?;
    let notify = smelt_notify_path();
    if !notify.is_file() {
        return Err(format!(
            "找不到 {}，请先编译安装 smelt-notify",
            notify.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut root = if path.is_file() {
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{} 不是有效 JSON：{e}", path.display()))?
    } else {
        serde_json::json!({})
    };
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "hooks 文件根不是对象".to_string())?;
    root_obj.insert("version".into(), serde_json::json!(1));
    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| "hooks 不是对象".to_string())?;
    for event in COPILOT_LEGACY_HOOK_EVENTS {
        let Some(handlers) = hooks.get_mut(*event).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        handlers.retain(|handler| {
            !handler
                .get("bash")
                .and_then(|v| v.as_str())
                .is_some_and(command_uses_smelt_notify)
        });
        if handlers.is_empty() {
            hooks.remove(*event);
        }
    }
    for event in COPILOT_HOOK_EVENTS {
        let command = provider_hook_command("copilot", event);
        let handlers = hooks
            .entry(*event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("hooks.{event} 不是数组"))?;
        handlers.retain(|handler| {
            !handler
                .get("bash")
                .and_then(|v| v.as_str())
                .is_some_and(command_uses_smelt_notify)
        });
        handlers.push(serde_json::json!({ "type": "command", "bash": command, "timeoutSec": 3 }));
    }
    write_json_atomic(&path, &root)
}

pub fn install_codex_hooks() -> Result<(), String> {
    let path = codex_hooks_path().ok_or_else(|| "无 home 目录".to_string())?;
    install_hook_file(path.clone(), CODEX_HOOK_EVENTS, "codex", false)?;
    let commands = CODEX_HOOK_EVENTS
        .iter()
        .map(|event| provider_hook_command("codex", event))
        .collect::<Vec<_>>();
    let cwd = std::env::current_dir().map_err(|error| format!("读取当前目录失败：{error}"))?;
    smelt_core::codex_app_server::grant_codex_hook_trust(&path, &cwd, &commands)
        .map(|_| ())
        .map_err(|error| format!("Codex hooks 已安装，但自动信任失败：{error}"))
}

pub fn uninstall_copilot_hooks() -> Result<(), String> {
    let Some(path) = copilot_hooks_path() else {
        return Ok(());
    };
    if !path.is_file() {
        return Ok(());
    }
    let mut root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let Some(hooks) = root.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return Ok(());
    };
    for event in COPILOT_HOOK_EVENTS.iter().chain(COPILOT_LEGACY_HOOK_EVENTS) {
        let Some(handlers) = hooks.get_mut(*event).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        handlers.retain(|handler| {
            !handler
                .get("bash")
                .and_then(|v| v.as_str())
                .is_some_and(command_uses_smelt_notify)
        });
        if handlers.is_empty() {
            hooks.remove(*event);
        }
    }
    write_json_atomic(&path, &root)
}

pub fn uninstall_codex_hooks() -> Result<(), String> {
    uninstall_hook_file(codex_hooks_path(), CODEX_HOOK_EVENTS)
}

/// Claude hooks 是否已完整装上 smelt-notify。
pub fn claude_hooks_installed() -> bool {
    hook_file_installed(claude_settings_path(), SMELT_HOOK_EVENTS)
}

/// 把 smelt-notify 写入 ~/.claude/settings.json（幂等）；成功返回 Ok。
pub fn install_claude_hooks() -> Result<(), String> {
    let path = claude_settings_path().ok_or_else(|| "无 home 目录".to_string())?;
    install_hook_file(path, SMELT_HOOK_EVENTS, "claude", false)
}

/// 从 Claude settings 移除 smelt-notify hooks（其它 hook 保留）。
pub fn uninstall_claude_hooks() -> Result<(), String> {
    uninstall_hook_file(claude_settings_path(), SMELT_HOOK_EVENTS)
}

fn run_all_hook_operations(
    operations: &[(&str, fn() -> Result<(), String>)],
) -> Result<(), String> {
    let errors = operations
        .iter()
        .filter_map(|(provider, operation)| {
            operation()
                .err()
                .map(|error| format!("{provider}: {error}"))
        })
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

pub fn install_agent_hooks() -> Result<(), String> {
    if !smelt_notify_available() {
        sync_bundled_smelt_notify().map_err(|error| format!("准备 smelt-notify 失败：{error}"))?;
    }
    run_all_hook_operations(&[
        ("Claude", install_claude_hooks),
        ("Copilot", install_copilot_hooks),
        ("Codex", install_codex_hooks),
    ])
}

pub fn uninstall_agent_hooks() -> Result<(), String> {
    run_all_hook_operations(&[
        ("Claude", uninstall_claude_hooks),
        ("Copilot", uninstall_copilot_hooks),
        ("Codex", uninstall_codex_hooks),
    ])
}

// ===================== 远程操作网关（见 docs/remote-ops-roadmap.md） =====================

/// 远程操作网关的持久化配置（全局单例，存 ~/.smelt/collab.json）。用户填写的
/// relay 地址会持久化；设备配对 token 由 smeltd 单独保存在 owner-only 文件中。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteConfig {
    pub enabled: bool,
    /// 用户自己的 iroh relay。空值表示未配置，不会回退到公共 relay。
    #[serde(default)]
    pub iroh_relay: String,
    /// 这条链接是否允许 approve/deny/reply（Phase 6，见 smeltd.rs「远程操控」）。
    /// `#[serde(default)]`：比 `enabled` 更晚加，旧配置缺省按只读处理——不能让
    /// 老用户的配置在升级后突然变成可写。链接分享出去本身就是授权，这里没有
    /// 额外的"当面确认"一说，开这个开关前的取舍由用户自己判断。
    #[serde(default)]
    pub write_enabled: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            iroh_relay: String::new(),
            write_enabled: false,
        }
    }
}

impl Global for RemoteConfig {}

fn remote_config_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".smelt").join("collab.json"))
}

/// 读取远程网关开关；缺失/损坏回退默认（关闭）。
pub fn load_remote_config() -> RemoteConfig {
    crate::json_store::load_json(remote_config_path())
}

fn save_remote_config(c: &RemoteConfig) {
    crate::json_store::save_json_private(remote_config_path(), c)
}

/// 内嵌远程网关的运行时状态（不落盘，纯展示用）。网关只作为 iroh 的本机落点，
/// 因而 UI 只需要保留启动错误；token 和绑定地址不再单独展示。
#[derive(Clone, Default)]
pub struct RemoteRuntimeState {
    pub error: Option<String>,
}

impl Global for RemoteRuntimeState {}

fn set_remote_from_start_result(result: Result<terminal::RemoteStatus, String>, cx: &mut App) {
    match result {
        Ok(_) => cx.set_global(RemoteRuntimeState::default()),
        Err(e) => cx.set_global(RemoteRuntimeState { error: Some(e) }),
    }
}

// ===================== iroh P2P 隧道（见 smeltd.rs「iroh 隧道」） =====================

/// iroh 隧道运行时（不落盘）：配对 URI + 二维码 PNG。
///
/// 存的是 `smelt+iroh://` 配对 URI，由持久化的 `endpoint_id` + token 组成。
/// 两者重启都不变，只有用户手动刷新 token 才需要重新扫码。
#[derive(Clone, Default)]
pub struct IrohRuntimeState {
    pub connecting: bool,
    pub pairing_uri: Option<String>,
    pub qr_png: Option<Vec<u8>>,
    pub error: Option<String>,
    pub write: bool,
}

impl Global for IrohRuntimeState {}

/// 异步拉起 iroh 隧道。绑定要连接用户配置的 relay，可能耗时数秒，**必须**走后台。
fn spawn_iroh_start(write: bool, cx: &mut App) {
    let config = cx.global::<RemoteConfig>().clone();
    let relay = config.iroh_relay;
    cx.set_global(IrohRuntimeState {
        connecting: true,
        ..Default::default()
    });
    cx.spawn(async move |cx| {
        let (result, remote, qr_png) = cx
            .background_executor()
            .spawn(async move {
                let result = terminal::iroh_start(write, &relay);
                // iroh_start 可能顺带把网关也开了（守护侧的 ensure_remote_gateway），
                // 所以要回读一次网关现状，否则 UI 上「本机链接」那块会一直是空的。
                let remote = terminal::remote_status();
                // 二维码在后台线程算好再交给 UI：绝不在 UI 线程现算 QR。
                let qr_png = match &result {
                    Ok(s) => match (
                        s.endpoint_id.as_deref(),
                        s.token.as_deref(),
                        s.relay.as_deref(),
                    ) {
                        (Some(id), Some(tok), Some(relay)) if !tok.is_empty() => {
                            qr_png_for_url(&smelt_core::pairing::iroh_pairing_uri(id, tok, relay))
                        }
                        _ => None,
                    },
                    Err(_) => None,
                };
                (result, remote, qr_png)
            })
            .await;
        let _ = cx.update(|cx| {
            if remote.running {
                cx.set_global(RemoteRuntimeState::default());
            }
            let rt = match result {
                Ok(s) => {
                    let uri = match (
                        s.endpoint_id.as_deref(),
                        s.token.as_deref(),
                        s.relay.as_deref(),
                    ) {
                        (Some(id), Some(tok), Some(relay)) if !tok.is_empty() => {
                            Some(smelt_core::pairing::iroh_pairing_uri(id, tok, relay))
                        }
                        _ => None,
                    };
                    match uri {
                        Some(uri) => IrohRuntimeState {
                            connecting: false,
                            pairing_uri: Some(uri),
                            qr_png,
                            error: None,
                            write: s.write,
                        },
                        // 隧道起来了但 token 没拿到：配对码缺一半，给不出可用的码。
                        None => IrohRuntimeState {
                            connecting: false,
                            error: Some("P2P 通道已建立，但分享密钥还没就绪，点重试即可".into()),
                            ..Default::default()
                        },
                    }
                }
                Err(e) => IrohRuntimeState {
                    connecting: false,
                    error: Some(e),
                    ..Default::default()
                },
            };
            cx.set_global(rt);
        });
    })
    .detach();
}

fn apply_iroh_relay_value(value: SharedString, cx: &mut App) {
    let mut config = cx.global::<RemoteConfig>().clone();
    config.iroh_relay = value.trim().to_string();
    let was_enabled = config.enabled;
    save_remote_config(&config);
    cx.set_global(config);
    if was_enabled {
        terminal::iroh_stop();
        cx.set_global(IrohRuntimeState {
            error: Some("Relay 配置已更新，点重试应用新配置".into()),
            ..Default::default()
        });
    }
    cx.refresh_windows();
}

/// 唯一远程开关：本机网关只作为 iroh 的 loopback 落点，不单独对用户暴露。
pub fn apply_remote_toggle(enabled: bool, cx: &mut App) {
    if enabled {
        let c = cx.global::<RemoteConfig>().clone();
        let write = c.write_enabled;
        set_remote_from_start_result(terminal::remote_start("127.0.0.1", write), cx);
        let mut c = cx.global::<RemoteConfig>().clone();
        c.enabled = true;
        save_remote_config(&c);
        cx.set_global(c);
        spawn_iroh_start(write, cx);
    } else {
        // iroh 要赶在网关之前停，理由同 smeltd 的 cleanup：反过来正在转发的流
        // 会撞上已死端口，手机侧看到的是连接被拒而非干净关闭。
        terminal::iroh_stop();
        terminal::remote_stop();
        cx.set_global(RemoteRuntimeState::default());
        cx.set_global(IrohRuntimeState::default());
        let mut c = cx.global::<RemoteConfig>().clone();
        c.enabled = false;
        save_remote_config(&c);
        cx.set_global(c);
    }
}

fn qr_png_for_url(url: &str) -> Option<Vec<u8>> {
    use qrcode::QrCode;
    let code = QrCode::new(url.as_bytes()).ok()?;
    let luma = code
        .render::<image::Luma<u8>>()
        .dark_color(image::Luma([0u8]))
        .light_color(image::Luma([255u8]))
        .min_dimensions(160, 160)
        .quiet_zone(true)
        .build();
    let rgb = image::DynamicImage::ImageLuma8(luma).into_rgb8();
    let mut buf = Vec::new();
    rgb.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .ok()?;
    Some(buf)
}

pub fn spawn_iroh_start_public(cx: &mut App) {
    let write = cx.global::<RemoteConfig>().write_enabled;
    spawn_iroh_start(write, cx);
}

/// 复制按钮的短暂「已复制 ✓」状态（设置页读它改按钮文案）。
#[derive(Clone, Default)]
struct CopyFlash {
    id: String,
    until: Option<Instant>,
}

impl Global for CopyFlash {}

/// 「存储」设置页的扫描结果 + 上一次清理的提示文案（点按钮时同步刷新，扫描很快
/// 不值得像更新检查那样搞异步状态机）。
#[derive(Clone, Default)]
struct CleanupState {
    scan: Option<crate::storage_cleanup::CleanupScan>,
    message: Option<SharedString>,
}

impl Global for CleanupState {}

fn copy_btn_label(id: &str, idle: &str, cx: &App) -> String {
    if let Some(f) = cx.try_global::<CopyFlash>() {
        if f.id == id {
            if let Some(until) = f.until {
                if Instant::now() < until {
                    return "已复制 ✓".into();
                }
            }
        }
    }
    idle.into()
}

/// 写入剪贴板 + 成功 toast + 按钮文案闪「已复制 ✓」约 2 秒。
fn copy_with_feedback(
    text: String,
    btn_id: &'static str,
    toast: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    cx.set_global(CopyFlash {
        id: btn_id.into(),
        until: Some(Instant::now() + Duration::from_millis(2000)),
    });
    window.push_notification(Notification::success(toast), cx);
    cx.refresh_windows();

    let clear_id = btn_id.to_string();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(2000))
            .await;
        let _ = cx.update(|cx| {
            let same = cx
                .try_global::<CopyFlash>()
                .map(|f| f.id == clear_id)
                .unwrap_or(false);
            if same {
                cx.set_global(CopyFlash::default());
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

pub fn apply_write_toggle(enabled: bool, cx: &mut App) {
    let mut c = cx.global::<RemoteConfig>().clone();
    c.write_enabled = enabled;
    save_remote_config(&c);
    cx.set_global(c.clone());

    if !c.enabled {
        // 远程没开：只记偏好，下次打开总开关时自动带上。
        return;
    }

    // 写权限是服务端策略，重启网关与隧道应用新值即可；持久化 token 保持不变，
    // 已配对手机不应因为权限切换被迫重新扫码。
    terminal::iroh_stop();
    terminal::remote_stop();
    set_remote_from_start_result(terminal::remote_start("127.0.0.1", enabled), cx);
    spawn_iroh_start(enabled, cx);
}

/// 分享卡片上的「重试」：按当前配置把网关与 iroh 隧道重新拉齐。
pub fn retry_remote_setup(cx: &mut App) {
    let c = cx.global::<RemoteConfig>().clone();
    if !c.enabled {
        return;
    }
    let write = c.write_enabled;

    terminal::iroh_stop();
    // 网关先停再起，让端口与 iroh 转发目标对齐；持久化 token 保持不变。
    terminal::remote_stop();
    set_remote_from_start_result(terminal::remote_start("127.0.0.1", write), cx);
    spawn_iroh_start(write, cx);
}

/// 用户主动刷新设备凭证。普通重试、服务重启、电脑重启和写权限切换都不会调用它。
pub fn refresh_remote_token(window: &mut Window, cx: &mut App) {
    let config = cx.global::<RemoteConfig>().clone();
    if !config.enabled {
        return;
    }
    cx.set_global(IrohRuntimeState {
        connecting: true,
        ..Default::default()
    });
    match terminal::remote_rotate_token()
        .and_then(|()| terminal::remote_start("127.0.0.1", config.write_enabled))
    {
        Ok(_) => {
            cx.set_global(RemoteRuntimeState::default());
            spawn_iroh_start(config.write_enabled, cx);
            window.push_notification(
                Notification::success("配对 Token 已刷新；旧配对已失效，请重新扫码"),
                cx,
            );
        }
        Err(error) => {
            cx.set_global(RemoteRuntimeState {
                error: Some(error.clone()),
            });
            cx.set_global(IrohRuntimeState {
                error: Some(error.clone()),
                ..Default::default()
            });
            window.push_notification(Notification::error(error), cx);
        }
    }
    cx.refresh_windows();
}

/// 改外观全局 + 存盘，不触发 view 重绘（调用方按需自己 notify/refresh）。
/// 供只有 `&mut App`（没有 `Context<Self>`）的场景用，比如设置页 SettingField 的 get/set 闭包。
fn apply_appearance(f: impl FnOnce(&mut Appearance), cx: &mut App) {
    let mut a = cx.global::<Appearance>().clone();
    f(&mut a);
    save_appearance(&a);
    cx.set_global(a);
}

/// 改 LLM 配置全局 + 存盘，不触发 view 重绘，用法同 [`apply_appearance`]。
fn apply_llm_config(f: impl FnOnce(&mut agent::LlmConfig), cx: &mut App) {
    let mut c = cx.global::<agent::LlmConfig>().clone();
    f(&mut c);
    agent::save_llm_config(&c);
    cx.set_global(c);
}

/// Hsla → 0xRRGGBB（取色器回调把颜色写回 config 用）。
fn hsla_to_rgb(c: Hsla) -> u32 {
    let rgba = Rgba::from(c);
    let q = |f: f32| ((f.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
    (q(rgba.r) << 16) | (q(rgba.g) << 8) | q(rgba.b)
}

// ===================== 设置页专属类型 =====================

/// 宠物大脑配置的四个输入框（base_url / api_key / model / persona）。
#[derive(Clone)]
pub struct LlmInputs {
    base_url: Entity<gpui_component::input::InputState>,
    api_key: Entity<gpui_component::input::InputState>,
    model: Entity<gpui_component::input::InputState>,
    persona: Entity<gpui_component::input::InputState>,
}

/// 启动项列表编辑器：每项一对 label/command 输入框。
pub struct LaunchInputs {
    rows: Vec<(
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
    )>,
    _subs: Vec<Subscription>,
}

/// 手动添加 workspace 列表编辑器：每项一对 label/workspace_dir 输入框；agent
/// 种类走下拉选择（离散值，不需要输入框），选完直接存盘不用另外的 InputState。
pub struct ProfileInputs {
    rows: Vec<(
        Entity<gpui_component::input::InputState>,
        Entity<gpui_component::input::InputState>,
    )>,
    _subs: Vec<Subscription>,
}

/// 独立设置窗口的根 view：只是个薄壳，真正状态都还在传进来的 Workspace 实体上，
/// 每次渲染转手调 `render_settings_content`。
///
/// 但「转手调」不等于「跟着刷新」：`cx.notify()` 标脏的是 Workspace，设置窗口不在它
/// 的观察者名单里，不会因此重绘。所以得显式 observe 一把，否则后台改的状态——更新
/// 运行时长的人话格式：秒 → 「3 小时 12 分」。只保留两级单位，设置页那行不需要秒级精度。
fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs} 秒"),
        (0, 0, m) => format!("{m} 分钟"),
        (0, h, m) => format!("{h} 小时 {m} 分"),
        (d, h, _) => format!("{d} 天 {h} 小时"),
    }
}

/// 守护运行信息拼成一行：`v0.5.4 · PID 64954 · 启动于 07-16 20:38（已运行 3 小时 12 分）· 5 个会话`。
/// 老守护回不出的字段直接不显示——宁可少一段，也不摆「未知」占位。
fn daemon_info_line(info: &terminal::DaemonInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = &info.version {
        parts.push(format!("v{v}"));
    }
    if let Some(pid) = info.pid {
        parts.push(format!("PID {pid}"));
    }
    if let Some(started) = info.started_at {
        // 本地时区显示；秒数换算成人话时长跟在后面。
        let started_txt = chrono::DateTime::from_timestamp(started as i64, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "?".into());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // saturating：守护跟 GUI 之间时钟若有漂移，别算出个天文数字。
        parts.push(format!(
            "启动于 {started_txt}（已运行 {}）",
            fmt_uptime(now.saturating_sub(started))
        ));
    }
    if let Some(n) = info.session_count {
        parts.push(format!("{n} 个会话"));
    }
    parts.join(" · ")
}

/// 下载进度、守护进程检测结果——在设置窗口里会一直停在打开那一刻的样子。
pub struct SettingsWindow {
    workspace: Entity<Workspace>,
    _observe_workspace: Subscription,
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 设置内容 +（可选）守护管理弹层：弹层必须画在本窗，不能只改 Workspace 上的
        // flag 却在主窗口 render——用户点的是设置里的按钮，弹窗却跑到主界面。
        self.workspace.update(cx, |ws, cx| {
            div()
                .relative()
                .size_full()
                .child(ws.render_settings_content(cx))
                .children(
                    ws.show_daemon_restart_confirm
                        .then(|| ws.render_daemon_restart_confirm(cx)),
                )
                .children(
                    ws.session_manager_open
                        .then(|| ws.render_session_manager(cx)),
                )
        })
    }
}

/// 独立设置窗口的单例句柄：已经开着就聚焦复用，避免重复开出好几扇一样的窗口。
pub struct SettingsWindowHandle(pub Option<WindowHandle<Root>>);
impl Global for SettingsWindowHandle {}

// ===================== Workspace 方法 =====================

impl Workspace {
    /// 懒创建宠物大脑配置的输入框（需要 window，故在首次渲染设置面板时调）。
    /// 每个框预填当前配置值，变更时写回 LlmConfig 并存盘。
    pub fn init_llm_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};
        let lc = cx.global::<agent::LlmConfig>().clone();

        let base_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.deepseek.com/chat/completions")
                .default_value(lc.base_url.clone())
        });
        let api_key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("sk-...（留空则用 config.toml/env）")
                .masked(true)
                .default_value(lc.api_key.clone())
        });
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("deepseek-chat")
                .default_value(lc.model.clone())
        });
        let persona = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 5)
                .placeholder("人设 / system prompt")
                .default_value(lc.persona.clone())
        });

        // 变更即写回对应字段（Change 覆盖键入，Blur 兜底）。
        let save_on = |ev: &InputEvent| matches!(ev, InputEvent::Change | InputEvent::Blur);
        self.llm_subs.clear();
        self.llm_subs.push(
            cx.subscribe(&base_url, move |this, s, ev: &InputEvent, cx| {
                if save_on(ev) {
                    let v = s.read(cx).value().to_string();
                    this.update_llm_config(|c| c.base_url = v, cx);
                }
            }),
        );
        self.llm_subs
            .push(cx.subscribe(&api_key, move |this, s, ev: &InputEvent, cx| {
                if save_on(ev) {
                    let v = s.read(cx).value().to_string();
                    this.update_llm_config(|c| c.api_key = v, cx);
                }
            }));
        self.llm_subs
            .push(cx.subscribe(&model, move |this, s, ev: &InputEvent, cx| {
                if save_on(ev) {
                    let v = s.read(cx).value().to_string();
                    this.update_llm_config(|c| c.model = v, cx);
                }
            }));
        self.llm_subs
            .push(cx.subscribe(&persona, move |this, s, ev: &InputEvent, cx| {
                if save_on(ev) {
                    let v = s.read(cx).value().to_string();
                    this.update_llm_config(|c| c.persona = v, cx);
                }
            }));

        self.llm_inputs = Some(LlmInputs {
            base_url,
            api_key,
            model,
            persona,
        });

        // —— 有状态组件：不透明度滑块 + 字体大小滑块 + 背景色 / 宠物色取色器 ——
        let ap = cx.global::<Appearance>().clone();
        let opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(60.0)
                .max(100.0)
                .step(5.0)
                .default_value(ap.opacity * 100.0)
        });
        let font_size_slider = cx.new(|_| {
            SliderState::new()
                .min(terminal_view::MIN_FONT_PX as f32)
                .max(terminal_view::MAX_FONT_PX as f32)
                .step(1.0)
                .default_value(ap.font_px as f32)
        });
        let bg_color_picker =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(rgb(ap.bg_color)));

        self.settings_subs.clear();

        self.settings_subs.push(
            cx.subscribe(&opacity_slider, |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let op = (*x / 100.0).clamp(0.3, 1.0);
                    this.set_appearance(move |a| a.opacity = op, cx);
                }
            }),
        );
        self.settings_subs.push(cx.subscribe(
            &font_size_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let size = x.round().clamp(
                        terminal_view::MIN_FONT_PX as f32,
                        terminal_view::MAX_FONT_PX as f32,
                    ) as u32;
                    terminal_view::set_font_px(size);
                    this.set_appearance(move |a| a.font_px = size, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &bg_color_picker,
            |this, _s, ev: &ColorPickerEvent, cx| {
                let ColorPickerEvent::Change(c) = ev;
                if let Some(hsla) = c {
                    let color = hsla_to_rgb(*hsla);
                    this.set_appearance(move |a| a.bg_color = color, cx);
                }
            },
        ));
        self.opacity_slider = Some(opacity_slider);
        self.font_size_slider = Some(font_size_slider);
        self.bg_color_picker = Some(bg_color_picker);
    }

    /// 无 window 版：改全局 + 存盘 + 重绘。窗口背景（透明/模糊）由 render 里的
    /// applied_window_bg 同步——供 slider/color_picker 的订阅回调用（它们拿不到 window）。
    pub fn set_appearance(&mut self, f: impl FnOnce(&mut Appearance), cx: &mut Context<Self>) {
        apply_appearance(f, cx);
        cx.notify();
    }

    /// 修改 LLM 配置：改全局 + 存盘 + 重绘。
    pub fn update_llm_config(
        &mut self,
        f: impl FnOnce(&mut agent::LlmConfig),
        cx: &mut Context<Self>,
    ) {
        apply_llm_config(f, cx);
        cx.notify();
    }

    /// 启动项条数变了就重建输入框（增删后调用）。
    pub fn reset_launch_inputs(&mut self) {
        self.launch_inputs = None;
    }

    /// 懒创建启动项列表编辑器（需要 window）。
    pub fn ensure_launch_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = cx.global::<LaunchConfig>().entries.len();
        let stale = self
            .launch_inputs
            .as_ref()
            .is_none_or(|i| i.rows.len() != count);
        if stale {
            self.init_launch_inputs(window, cx);
        }
    }

    fn init_launch_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};

        let entries = cx.global::<LaunchConfig>().entries.clone();
        let save_on = |ev: &InputEvent| matches!(ev, InputEvent::Change | InputEvent::Blur);
        let mut rows = Vec::new();
        let mut subs = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            let label_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("显示名称")
                    .default_value(entry.label.clone())
            });
            let command_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("启动命令，如 claude")
                    .default_value(entry.command.clone())
            });
            subs.push(
                cx.subscribe(&label_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        apply_launch_config(
                            |c| {
                                if let Some(e) = c.entries.get_mut(i) {
                                    e.label = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            subs.push(
                cx.subscribe(&command_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        apply_launch_config(
                            |c| {
                                if let Some(e) = c.entries.get_mut(i) {
                                    e.command = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            rows.push((label_input, command_input));
        }
        self.launch_inputs = Some(LaunchInputs { rows, _subs: subs });
    }

    pub fn add_launch_entry(&mut self, cx: &mut Context<Self>) {
        apply_launch_config(
            |c| {
                c.entries.push(LaunchEntry {
                    label: "新启动项".into(),
                    command: String::new(),
                });
            },
            cx,
        );
        self.reset_launch_inputs();
        cx.notify();
    }

    pub fn remove_launch_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        apply_launch_config(
            |c| {
                if index < c.entries.len() {
                    c.entries.remove(index);
                }
            },
            cx,
        );
        self.reset_launch_inputs();
        cx.notify();
    }

    /// 手动添加 workspace 条数变了就重建输入框（增删后调用）。
    pub fn reset_profile_inputs(&mut self) {
        self.profile_inputs = None;
    }

    /// 懒创建 workspace 列表编辑器（需要 window）。
    pub fn ensure_profile_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = cx.global::<AgentUiConfig>().profiles.len();
        let stale = self
            .profile_inputs
            .as_ref()
            .is_none_or(|i| i.rows.len() != count);
        if stale {
            self.init_profile_inputs(window, cx);
        }
    }

    fn init_profile_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{InputEvent, InputState};

        let profiles = cx.global::<AgentUiConfig>().profiles.clone();
        let save_on = |ev: &InputEvent| matches!(ev, InputEvent::Change | InputEvent::Blur);
        let mut rows = Vec::new();
        let mut subs = Vec::new();
        for (i, p) in profiles.iter().enumerate() {
            let id = p.id.clone();
            let label_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("显示名称")
                    .default_value(p.label.clone())
            });
            let dir_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("workspace 目录，如 ~/.claude-quant")
                    .default_value(p.workspace_dir.clone())
            });
            let id_for_label = id.clone();
            subs.push(
                cx.subscribe(&label_input, move |_, s, ev: &InputEvent, cx| {
                    if save_on(ev) {
                        let v = s.read(cx).value().to_string();
                        let id = id_for_label.clone();
                        apply_agent_ui(
                            move |c| {
                                if let Some(p) = c.profiles.iter_mut().find(|p| p.id == id) {
                                    p.label = v;
                                }
                            },
                            cx,
                        );
                    }
                }),
            );
            let id_for_dir = id.clone();
            subs.push(cx.subscribe(&dir_input, move |_, s, ev: &InputEvent, cx| {
                if save_on(ev) {
                    let v = s.read(cx).value().to_string();
                    let id = id_for_dir.clone();
                    apply_agent_ui(
                        move |c| {
                            if let Some(p) = c.profiles.iter_mut().find(|p| p.id == id) {
                                p.workspace_dir = v;
                            }
                        },
                        cx,
                    );
                }
            }));
            let _ = i;
            rows.push((label_input, dir_input));
        }
        self.profile_inputs = Some(ProfileInputs { rows, _subs: subs });
    }

    /// 新增一个手动 workspace：默认接 Claude（用户改目录之前就得选个 agent，
    /// Claude 是最常见的场景，跟「新建 ACP 对话」菜单的默认排位一致）。
    pub fn add_profile(&mut self, cx: &mut Context<Self>) {
        apply_agent_ui(
            |c| {
                c.profiles.push(AcpProfile {
                    id: uuid::Uuid::new_v4().to_string(),
                    kind_id: AcpAgentKind::Claude.id().to_string(),
                    label: "新 workspace".into(),
                    workspace_dir: String::new(),
                });
            },
            cx,
        );
        self.reset_profile_inputs();
        cx.notify();
    }

    pub fn remove_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        apply_agent_ui(
            |c| {
                if index < c.profiles.len() {
                    c.profiles.remove(index);
                }
            },
            cx,
        );
        self.reset_profile_inputs();
        cx.notify();
    }

    /// 改某个 workspace 接的 agent 种类（下拉菜单选中项回调）。
    pub fn set_profile_kind(&mut self, index: usize, kind: AcpAgentKind, cx: &mut Context<Self>) {
        apply_agent_ui(
            move |c| {
                if let Some(p) = c.profiles.get_mut(index) {
                    p.kind_id = kind.id().to_string();
                }
            },
            cx,
        );
        cx.notify();
    }

    /// 设置 / 清除背景图（不影响窗口透明度，故无需 window）。
    pub fn set_bg_image(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        apply_appearance(|a| a.bg_image = path, cx);
        cx.notify();
    }

    /// 弹原生选择框选一张背景图。
    pub fn pick_bg_image(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择背景图片".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths
                    .into_iter()
                    .next()
                    .and_then(|p| p.to_str().map(String::from))
                {
                    this.update(cx, |this, cx| this.set_bg_image(Some(p), cx))
                        .ok();
                }
            }
        })
        .detach();
    }

    /// 渲染独立设置页面：铺满主区、居中限宽、支持滚动。
    /// 设置页主体：外观 / LLM / 启动 / 更新 等分组。供嵌入式设置页（主窗口右上角齿轮，
    /// 带「返回」头）和独立设置窗口（原生标题栏，无需「返回」）共用，各自决定外层怎么包。
    pub fn render_settings_content(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted, border, popover) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.border, t.popover)
        };
        let entity = cx.entity();

        // 统一的小按钮：固定高度 + flex_none，避免被 flex 布局拉伸成大块。
        // move 闭包：捕获的四个颜色都是 Copy，闭包本身因此也是 Copy，可以放心
        // 塞进下面多个 SettingField::render 的 move 闭包里各用一份。
        //
        // 注意：GPUI 的 `.hover()` 只能挂一次（debug_assert「hover style already set」），
        // 所以默认 hover 写在这里；需要换 hover 色的按钮请用 `btn_hover`，别再链式 `.hover()`。
        let btn_base = move |id: &'static str, label: String| {
            div()
                .id(id)
                .h(px(26.))
                .px_3()
                .flex()
                .flex_none()
                .items_center()
                .rounded_md()
                .cursor_pointer()
                .text_xs()
                .text_color(fg)
                .bg(popover)
                .border_1()
                .border_color(border)
                .child(label)
        };
        let btn =
            move |id: &'static str, label: String| btn_base(id, label).hover(|s| s.bg(border));
        let btn_hover = move |id: &'static str, label: String, hover_bg: Hsla| {
            btn_base(id, label).hover(move |s| s.bg(hover_bg))
        };

        // —— 外观 ——
        let bg_color_picker = self.bg_color_picker.clone();
        let opacity_slider = self.opacity_slider.clone();
        let font_size_slider = self.font_size_slider.clone();
        let pick_entity = entity.clone();
        let clear_entity = entity.clone();
        // 终端字体下拉的选项：内嵌默认置顶（值为空 = 用默认），其后按字母序列出系统
        // 已装的全部字体族。不做等宽过滤——系统没有可靠的「是否等宽」元数据，漏判
        // 误判都更糟；选了非等宽的后果只是难看，fallback 链保证不会渲染错乱。
        //
        // 扫字体贵（见 `font_options` 字段注释），只在第一次渲染设置页时做一次。
        let font_options = self
            .font_options
            .get_or_init(|| {
                let mut names = cx.text_system().all_font_names();
                names.sort();
                names.dedup();
                // 选项 label 同时也是下拉按钮上的文字，而 Button 既不截断也不收缩，
                // 全名「JetBrainsMono Nerd Font Mono」会把按钮顶出设置页右边界。
                // 这里只取第一段，完整名字放在 description 里。
                let short = terminal_view::DEFAULT_FONT_FAMILY
                    .split_whitespace()
                    .next()
                    .unwrap_or(terminal_view::DEFAULT_FONT_FAMILY);
                std::iter::once((
                    SharedString::from(""),
                    SharedString::from(format!("默认（{short}）")),
                ))
                .chain(
                    names
                        .into_iter()
                        .map(|n| (SharedString::from(n.clone()), SharedString::from(n))),
                )
                .collect()
            })
            .clone();
        let appearance_page =
            SettingPage::new("外观")
                .default_open(true)
                .group(SettingGroup::new().items(vec![
                SettingItem::new(
                    "主题模式",
                    SettingField::switch(
                        |cx: &App| cx.global::<Appearance>().theme_mode.is_dark(),
                        |v: bool, cx: &mut App| {
                            let mode = if v { ThemeMode::Dark } else { ThemeMode::Light };
                            apply_appearance(|a| a.theme_mode = mode, cx);
                            apply_theme_mode(mode, cx);
                            // 色板是进程级全局态（见 ui_theme），改完不重绘就还是旧色。
                            cx.refresh_windows();
                        },
                    )
                    .default_value(true),
                )
                .description("开启为深色主题，关闭为浅色主题"),
                SettingItem::new(
                    "字体大小",
                    SettingField::render(move |_, _, cx: &mut App| {
                        let size = cx.global::<Appearance>().font_px;
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(200.))
                                    .children(font_size_slider.as_ref().map(Slider::new)),
                            )
                            .child(
                                div()
                                    .w(px(32.))
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{size}px")),
                            )
                    }),
                ),
                SettingItem::new(
                    "终端字体",
                    SettingField::scrollable_dropdown(
                        font_options,
                        |cx: &App| cx.global::<Appearance>().font_family.clone().into(),
                        |v: SharedString, cx: &mut App| {
                            let name = v.trim().to_string();
                            terminal_view::set_font_family(&name);
                            apply_appearance(move |a| a.font_family = name, cx);
                            cx.refresh_windows();
                        },
                    )
                    // 系统里总有名字长得离谱的字体，选中后同样会顶爆按钮，这里封顶兜住。
                    .max_w(px(220.))
                    .overflow_hidden(),
                )
                .description(concat!(
                    "终端使用的字体；建议选等宽字体，图标缺字自动回落内嵌默认（",
                    "JetBrainsMono Nerd Font Mono）",
                )),
                SettingItem::new(
                    "背景色",
                    SettingField::render(move |_, _, _| {
                        div().children(
                            bg_color_picker
                                .as_ref()
                                .map(|p| ColorPicker::new(p).small()),
                        )
                    }),
                ),
                SettingItem::new(
                    "背景图片",
                    SettingField::render(move |_, _, cx: &mut App| {
                        let img_name = cx
                            .global::<Appearance>()
                            .bg_image
                            .as_deref()
                            .and_then(|p| p.rsplit('/').next())
                            .unwrap_or("无")
                            .to_string();
                        let pick_entity = pick_entity.clone();
                        let clear_entity = clear_entity.clone();
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                // 文件名长度不可控，必须自己封顶：SettingItem 外层是
                                // overflow_hidden，撑爆的部分不会换行，只会把右边的按钮
                                // 顶出可视区，导致「选择图片…／清除」点都点不到。
                                // 中间省略号保留开头和扩展名，比末尾截断更容易认出是哪张图。
                                div()
                                    .max_w(px(140.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_middle()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(img_name),
                            )
                            .child(
                                btn("pick-img", "选择图片…".into())
                                    .flex_shrink_0()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_, _window, cx: &mut App| {
                                            pick_entity
                                                .update(cx, |this, cx| this.pick_bg_image(cx));
                                        },
                                    ),
                            )
                            .child(
                                btn("clear-img", "清除".into())
                                    .flex_shrink_0()
                                    .text_color(muted)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_, _window, cx: &mut App| {
                                            clear_entity
                                                .update(cx, |this, cx| this.set_bg_image(None, cx));
                                        },
                                    ),
                            )
                    }),
                ),
                SettingItem::new(
                    "不透明度",
                    SettingField::render(move |_, _, _| {
                        div()
                            .w(px(200.))
                            .children(opacity_slider.as_ref().map(Slider::new))
                    }),
                ),
            ]));

        // —— LLM：Git 面板「生成 commit message」等功能的 OpenAI 兼容接口配置 ——
        // 桌面宠物已删除（原「宠物大脑」区块整体并入这里，去掉宠物语义）。
        let llm_inputs = self.llm_inputs.clone();
        let llm_page = SettingPage::new("LLM").group(SettingGroup::new().items(vec![
            SettingItem::new(
                "启用 LLM",
                SettingField::switch(
                    |cx: &App| cx.global::<agent::LlmConfig>().enabled,
                    |v: bool, cx: &mut App| apply_llm_config(|c| c.enabled = v, cx),
                ),
            )
            .description("接入 OpenAI 兼容接口，用于 Git 面板自动生成 Conventional Commits 提交信息。"),
            SettingItem::render(move |_, _, _| {
                let field = |label: &str, state: &Entity<gpui_component::input::InputState>| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_xs().text_color(muted).child(label.to_string()))
                        .child(Input::new(state).small())
                };
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(llm_inputs.as_ref().map(|inp| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(field("接口地址 base_url", &inp.base_url))
                            .child(field("API Key", &inp.api_key))
                            .child(field("模型 model", &inp.model))
                            .child(field("人设 persona", &inp.persona))
                    }))
            }),
        ]));

        // —— 启动：项目「+」下拉菜单的可配置启动项 ——
        // Settings 的 list 测量项高度时，百分比宽度（w_full）经常解析不到确定父宽，
        // 卡片会缩成「内容宽」——输入框只露出几个字。这里用窗口视口算绝对像素宽。
        let launch_editor_entity = entity.clone();
        let launch_page = SettingPage::new("启动").group(
            SettingGroup::new()
                .item(
                    SettingItem::render(move |_, window, cx: &mut App| {
                        let muted = cx.theme().muted_foreground;
                        let border = cx.theme().border;
                        let fg = cx.theme().foreground;
                        let popover = cx.theme().popover;
                        let secondary = cx.theme().secondary;
                        let danger = cx.theme().danger;
                        let danger_fg = cx.theme().danger_foreground;
                        // 侧栏默认 250 + 左右 padding/滚动条余量；再夹到合理区间。
                        let field_w = {
                            let vw = f32::from(window.viewport_size().width);
                            let w = (vw - 250. - 80.).clamp(360., 720.);
                            px(w)
                        };
                        launch_editor_entity.update(cx, |ws, cx| {
                            ws.ensure_launch_inputs(window, cx);
                            let Some(inputs) = ws.launch_inputs.as_ref() else {
                                return div().into_any_element();
                            };
                            let mut col = v_flex()
                                .w(field_w)
                                .gap_3()
                                .child(
                                    v_flex()
                                        .w(field_w)
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(fg)
                                                .child("快捷启动项"),
                                        )
                                        .child(
                                            div().w(field_w).text_sm().text_color(muted).child(
                                                "项目行「+」菜单里除「新建终端」「新建 Worktree…」外的项。\
                                                 显示名会出现在菜单上；命令是在该项目目录下执行的 shell 命令\
                                                 （可含参数）。",
                                            ),
                                        ),
                                );
                            // 名称和命令并排成两列，而不是上下堆叠：之前两个输入框同宽同字体，
                            // 只靠上方一行小灰字区分，扫视时根本分不出哪个是哪个。改成
                            // 「窄名称列 + 宽命令列 + 命令用等宽字体」——列位置、宽度、字体三重
                            // 区分，比标签文字有效得多，顺带把每项从 4 行压到 1 行。
                            // 名称短（"Claude Code" 这种）、命令长（带一串参数），宽度按
                            // 信息量分：名称够放就行，剩下的全给命令。
                            let name_w = px(140.);
                            let del_w = px(28.);
                            let cmd_w = field_w - name_w - del_w - px(40.);
                            let mono = terminal_view::font_family();

                            let mut list = v_flex()
                                .w(field_w)
                                .gap_2()
                                .p_3()
                                .rounded_lg()
                                .border_1()
                                .border_color(border)
                                .bg(secondary)
                                // 列名只在表头出现一次，不必每项重复一遍「名称」「命令」。
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(div().w(name_w).child("名称"))
                                        .child(div().w(cmd_w).child("命令"))
                                        // 占位：让表头两列跟下面的行严格对齐（删除按钮那一列）。
                                        .child(div().w(del_w)),
                                );
                            for (ix, (label, command)) in inputs.rows.iter().enumerate() {
                                let del_entity = launch_editor_entity.clone();
                                let row_ix = ix;
                                list = list.child(
                                    h_flex()
                                        .id(("launch-row", row_ix))
                                        .w_full()
                                        .gap_2()
                                        .items_center()
                                        .child(Input::new(label).w(name_w))
                                        // 命令是 shell 代码，用终端同款等宽字体——参数里的
                                        // `-`/`_` 对齐后好读，也一眼跟左边的显示名区分开。
                                        .child(
                                            Input::new(command)
                                                .w(cmd_w)
                                                .font_family(mono.clone()),
                                        )
                                        .child(
                                            div()
                                                .id(("del-launch", row_ix))
                                                .size(del_w)
                                                .flex()
                                                .flex_none()
                                                .items_center()
                                                .justify_center()
                                                .rounded_md()
                                                .cursor_pointer()
                                                .text_sm()
                                                .text_color(muted)
                                                // 删除是破坏性操作，hover 时给红底明示。
                                                .hover(|s| s.bg(danger).text_color(danger_fg))
                                                .child("×")
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    move |_, _, cx: &mut App| {
                                                        del_entity.update(cx, |ws, cx| {
                                                            ws.remove_launch_entry(row_ix, cx);
                                                        });
                                                    },
                                                ),
                                        ),
                                );
                            }
                            col = col.child(list);
                            let add_entity = launch_editor_entity.clone();
                            col.child(
                                div()
                                    .id("add-launch")
                                    .h(px(36.))
                                    .w(field_w)
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .text_sm()
                                    .text_color(fg)
                                    .bg(popover)
                                    .border_1()
                                    .border_color(border)
                                    .hover(|s| s.bg(border))
                                    .child("+ 添加启动项")
                                    .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                        add_entity.update(cx, |ws, cx| ws.add_launch_entry(cx));
                                    }),
                            )
                            .into_any_element()
                        })
                    })
                    .keywords(["快捷启动", "launch", "命令", "claude", "codex", "copilot"]),
                ),
        );

        // —— 更新：检查/下载全自动静默，生效推迟到退出时 ——
        let update_entity = entity.clone();
        let daemon_entity = entity.clone();
        let update_page = SettingPage::new("更新").resettable(false).group(
            SettingGroup::new()
                .item(SettingItem::render(move |_, _, cx: &mut App| {
                let status = update_entity.read(cx).update_status.clone();
                // 字节数换算成 MB 展示，只在拿得到 Content-Length 时才有百分比。
                let mb = |b: u64| b as f64 / 1024.0 / 1024.0;
                let status_text = match &status {
                    updater::UpdateStatus::Idle => String::new(),
                    updater::UpdateStatus::Checking => "检查中…".to_string(),
                    updater::UpdateStatus::UpToDate => "已是最新版本".to_string(),
                    updater::UpdateStatus::Downloading { version, received, total } => match total {
                        Some(total) if *total > 0 => format!(
                            "正在下载 v{version}… {:.0}%（{:.1} / {:.1} MB）",
                            *received as f64 / *total as f64 * 100.0,
                            mb(*received),
                            mb(*total),
                        ),
                        _ => format!("正在下载 v{version}…（已下载 {:.1} MB）", mb(*received)),
                    },
                    updater::UpdateStatus::Installing { version } => {
                        format!("正在安装 v{version}…")
                    }
                    updater::UpdateStatus::ReadyToInstall { version, .. } => {
                        format!("新版本 v{version} 已就绪，下次启动生效")
                    }
                    updater::UpdateStatus::Failed(e) => format!("检查失败：{e}"),
                };
                // 进度条：能算出百分比就走确定进度，否则跑不确定的滑动动画。
                let progress_bar = match &status {
                    updater::UpdateStatus::Downloading { received, total: Some(total), .. }
                        if *total > 0 =>
                    {
                        Some(
                            Progress::new("update-progress")
                                .value(*received as f32 / *total as f32 * 100.0),
                        )
                    }
                    updater::UpdateStatus::Downloading { .. }
                    | updater::UpdateStatus::Installing { .. } => {
                        Some(Progress::new("update-progress").loading(true))
                    }
                    _ => None,
                };
                let busy = matches!(
                    status,
                    updater::UpdateStatus::Checking
                        | updater::UpdateStatus::Downloading { .. }
                        | updater::UpdateStatus::Installing { .. }
                );
                let ready = matches!(status, updater::UpdateStatus::ReadyToInstall { .. });

                let check_label: String = match &status {
                    updater::UpdateStatus::Checking => "检查中…".into(),
                    updater::UpdateStatus::Downloading { .. } => "下载中…".into(),
                    updater::UpdateStatus::Installing { .. } => "安装中…".into(),
                    _ => "检查更新".into(),
                };
                let check_entity = update_entity.clone();
                let check_btn = btn("check-update", check_label)
                    .text_color(if busy { muted } else { fg })
                    .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                        check_entity.update(cx, |this, cx| {
                            if !matches!(
                                this.update_status,
                                updater::UpdateStatus::Checking
                                    | updater::UpdateStatus::Downloading { .. }
                                    | updater::UpdateStatus::Installing { .. }
                            ) {
                                this.check_for_update(false, cx);
                            }
                        });
                    });
                let restart_btn = ready.then(|| {
                    btn_hover(
                        "restart-update",
                        "立即重启更新".into(),
                        Hsla::from(crate::ui_theme::tint(crate::ui_theme::blue(), 0x40)),
                    )
                        .text_color(rgb(crate::ui_theme::blue()))
                        .bg(Hsla::from(crate::ui_theme::tint(crate::ui_theme::blue(), 0x24)))
                        .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                            if let updater::UpdateStatus::ReadyToInstall { staged_app, .. } = &status {
                                let staged = staged_app.clone();
                                // 装包挪后台（内部 copy/rename 整份 .app + 30s 级等待，
                                // ES 慢 open() 会冻结主线程）；装完回主线程重启退出。
                                cx.spawn(async move |cx| {
                                    let ok = cx
                                        .background_executor()
                                        .spawn(async move {
                                            crate::terminal::install_app_preserving_sessions(
                                                &staged,
                                            )
                                            .is_ok()
                                        })
                                        .await;
                                    if ok {
                                        // 排好重启再退；拉不起来也只是退化成手动打开，不该拦着退出。
                                        let _ = updater::relaunch();
                                        cx.update(|cx| cx.quit());
                                    }
                                })
                                .detach();
                            }
                        })
                });

                v_flex()
                    .w_full()
                    .gap_3()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(fg)
                                            .child(concat!("当前版本 v", env!("CARGO_PKG_VERSION"))),
                                    )
                                    .child(
                                        div()
                                            .id("settings-github-link")
                                            .text_xs()
                                            .cursor_pointer()
                                            .text_color(muted)
                                            .hover(|s| s.text_color(fg))
                                            .child("GitHub ↗")
                                            .on_mouse_down(MouseButton::Left, |_, _window, cx| {
                                                cx.open_url("https://github.com/smelt-ai/smelt");
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id("settings-help-link")
                                            .text_xs()
                                            .cursor_pointer()
                                            .text_color(muted)
                                            .hover(|s| s.text_color(fg))
                                            .child("帮助文档 ↗")
                                            .on_mouse_down(MouseButton::Left, |_, _window, cx| {
                                                cx.open_url("https://smelt.onoo.io/");
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id("settings-open-log-link")
                                            .text_xs()
                                            .cursor_pointer()
                                            .text_color(muted)
                                            .hover(|s| s.text_color(fg))
                                            .child("打开日志 ↗")
                                            .on_mouse_down(MouseButton::Left, |_, _window, _cx| {
                                                // 反馈问题时手动带上这份日志：GitHub
                                                // issue 附件靠拖拽，没有 API 能替用户
                                                // 自动上传，所以帮用户直接定位到文件。
                                                if let Some(path) = smelt_core::app_log::log_path()
                                                {
                                                    #[cfg(target_os = "macos")]
                                                    {
                                                        let _ = std::process::Command::new("open")
                                                            .arg("-R")
                                                            .arg(&path)
                                                            .spawn();
                                                    }
                                                    #[cfg(not(target_os = "macos"))]
                                                    {
                                                        if let Some(dir) = path.parent() {
                                                            let _ = std::process::Command::new(
                                                                "xdg-open",
                                                            )
                                                            .arg(dir)
                                                            .spawn();
                                                        }
                                                    }
                                                }
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id("settings-report-issue-link")
                                            .text_xs()
                                            .cursor_pointer()
                                            .text_color(muted)
                                            .hover(|s| s.text_color(fg))
                                            .child("反馈问题 ↗")
                                            .on_mouse_down(MouseButton::Left, |_, _window, cx| {
                                                // 先把日志在 Finder 里选出来，用户点开
                                                // issue 页面后可以直接把它拖进去。
                                                if let Some(path) = smelt_core::app_log::log_path()
                                                {
                                                    #[cfg(target_os = "macos")]
                                                    {
                                                        let _ = std::process::Command::new("open")
                                                            .arg("-R")
                                                            .arg(&path)
                                                            .spawn();
                                                    }
                                                }
                                                cx.open_url(
                                                    "https://github.com/smelt-ai/smelt/issues/new/choose",
                                                );
                                            }),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(check_btn)
                                    .children(restart_btn),
                            ),
                    )
                    .children((!status_text.is_empty()).then(|| {
                        div().text_xs().text_color(muted).child(status_text)
                    }))
                    .children(progress_bar)
            }))
                .item(SettingItem::render(move |_, _, cx: &mut App| {
                    let outdated = daemon_entity.read(cx).daemon_outdated;
                    let upgrading = daemon_entity.read(cx).daemon_upgrading;
                    let upgrade_msg = daemon_entity.read(cx).daemon_upgrade_msg.clone();
                    let upgrade_entity = daemon_entity.clone();
                    let restart_entity = daemon_entity.clone();
                    // 首选：无缝升级（exec 交接，会话不中断）。
                    let upgrade_daemon_btn = (outdated == Some(true)).then(|| {
                        btn(
                            "upgrade-daemon",
                            if upgrading { "升级中…".into() } else { "无缝升级".into() },
                        )
                        .when(!upgrading, |b| {
                            b.on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                                upgrade_entity.update(cx, |this, cx| {
                                    this.upgrade_daemon_seamless(cx);
                                });
                            })
                        })
                    });
                    // 硬重启：常驻入口（守护卡死 / 想强制换二进制时用），会断会话。
                    // 不受版本是否落后限制；点击走二次确认弹窗兜底。
                    // 用 btn_hover：自定义 hover 色，避免在已有 hover 的 btn 上再链式 .hover() 崩。
                    let restart_daemon_btn = btn_hover(
                        "restart-daemon",
                        "重启守护进程".into(),
                        Hsla::from(crate::ui_theme::tint(crate::ui_theme::red(), 0x40)),
                    )
                        .text_color(rgb(crate::ui_theme::red()))
                        .bg(Hsla::from(crate::ui_theme::tint(crate::ui_theme::red(), 0x24)))
                        .on_mouse_down(MouseButton::Left, move |_, _window, cx: &mut App| {
                            restart_entity.update(cx, |this, cx| {
                                this.show_daemon_restart_confirm = true;
                                cx.notify();
                            });
                        });
                    let status_text = match outdated {
                        Some(true) => "版本落后于当前安装包，升级守护后新功能/修复才生效。".to_string(),
                        Some(false) => "已是最新。".to_string(),
                        None => "检测中…".to_string(),
                    };
                    // 运行信息：守护没起就明说，别留空白让人以为没加载出来。
                    let info = daemon_entity.read(cx).daemon_info.clone();
                    let info_text = match (&info, outdated) {
                        (Some(i), _) => Some(daemon_info_line(i)),
                        // outdated 已探测完但拿不到 info → 守护确实没跑。
                        (None, Some(_)) => Some("未在运行（新建终端时会自动拉起）".to_string()),
                        (None, None) => None,
                    };
                    // 「N 个会话」不只是个数字——守护持有的会话不全是侧栏认领的
                    // （测试跑出来的游离会话、忘了关的临时会话也计在内），点开能看
                    // 到明细并单独清理，不用被迫走「重启守护进程」那种连坐所有
                    // 会话的核选项。守护没起来就没什么可看的，不露这个入口。
                    let manage_sessions_entity = daemon_entity.clone();
                    let manage_sessions_link = info.is_some().then(|| {
                        div()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(|s| s.text_color(fg))
                            .child("查看/清理会话 ›")
                            .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                                manage_sessions_entity.update(cx, |ws, cx| {
                                    ws.open_session_manager(cx);
                                });
                            })
                    });

                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .items_center()
                                .child(div().text_sm().text_color(fg).child("守护进程（smeltd）"))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .children(upgrade_daemon_btn)
                                        .child(restart_daemon_btn),
                                ),
                        )
                        .child(div().text_xs().text_color(muted).child(status_text))
                        .children(
                            info_text.map(|t| div().text_xs().text_color(muted).child(t)),
                        )
                        .children(manage_sessions_link)
                        .children(upgrade_msg.map(|m| div().text_xs().text_color(muted).child(m)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("「重启守护进程」会断开并终止当前所有终端会话（含正在跑的 agent）；若只是版本落后，优先用会话不中断的「无缝升级」。"),
                        )
                })),
        );

        // —— 存储：`~/.smelt` 下的历史残留（老 schema / 老实现留下的遗留文件）扫描与清理 ——
        let storage_page = SettingPage::new("存储").resettable(false).group(
            SettingGroup::new()
                .title("残留数据清理")
                .description(
                    "smelt 迭代过程中换过任务存储格式、worktree 检出方式，\
                     旧版本留下的文件不会再被读写，纯占地方。这里可以扫一遍并一键清掉。",
                )
                .item(SettingItem::render(move |_, _, cx: &mut App| {
                    let state = cx.default_global::<CleanupState>().clone();

                    let summary_text = match &state.scan {
                        None => "还没扫描".to_string(),
                        Some(s) if s.is_empty() => "没有发现残留数据".to_string(),
                        Some(s) => format!(
                            "发现 {} 处残留：遗留 prompt 文件 {} 个、旧版任务文件 {} 个、\
                             旧版 worktree 目录 {} 个",
                            s.total_items(),
                            s.legacy_prompts.len(),
                            s.legacy_task_files.len(),
                            s.legacy_worktree_dirs.len(),
                        ),
                    };

                    let has_findings = state.scan.as_ref().is_some_and(|s| !s.is_empty());

                    let scan_btn = btn("storage-scan", "扫描残留数据".into()).on_mouse_down(
                        MouseButton::Left,
                        move |_, _window, cx: &mut App| {
                            let scan = crate::storage_cleanup::scan();
                            cx.set_global(CleanupState {
                                scan: Some(scan),
                                message: None,
                            });
                        },
                    );

                    let clean_btn = has_findings.then(|| {
                        btn_hover(
                            "storage-clean",
                            "清理".into(),
                            Hsla::from(crate::ui_theme::tint(crate::ui_theme::blue(), 0x40)),
                        )
                        .text_color(rgb(crate::ui_theme::blue()))
                        .bg(Hsla::from(crate::ui_theme::tint(
                            crate::ui_theme::blue(),
                            0x24,
                        )))
                        .on_mouse_down(
                            MouseButton::Left,
                            move |_, _window, cx: &mut App| {
                                let Some(scan) = cx.default_global::<CleanupState>().scan.clone()
                                else {
                                    return;
                                };
                                let removed = crate::storage_cleanup::clean(&scan);
                                cx.set_global(CleanupState {
                                    scan: Some(crate::storage_cleanup::scan()),
                                    message: Some(format!("已清理 {removed} 项").into()),
                                });
                            },
                        )
                    });

                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .items_center()
                                .child(div().text_sm().text_color(fg).child("~/.smelt 历史残留"))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(scan_btn)
                                        .children(clean_btn),
                                ),
                        )
                        .child(div().text_xs().text_color(muted).child(summary_text))
                        .children(
                            state
                                .message
                                .map(|m| div().text_xs().text_color(muted).child(m)),
                        )
                })),
        );

        // —— Agent 集成：ACP 启动命令 + 审批通知 + Claude hooks 安装/还原 ——
        let agent_page = SettingPage::new("Agent 集成").group(
            SettingGroup::new()
                .item(
                    SettingItem::new(
                        "等待批准通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentUiConfig>()
                                    .map(|c| c.notify_approval)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_ui(|c| c.notify_approval = v, cx);
                            },
                        ),
                    )
                    .description("Agent 明确进入等待审批状态时提醒。")
                    .keywords(["通知", "notification", "审批"]),
                )
                .item(
                    SettingItem::new(
                        "等待输入通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentUiConfig>()
                                    .map(|c| c.notify_input)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_ui(|c| c.notify_input = v, cx);
                            },
                        ),
                    )
                    .description("Agent 提问或等待你继续时提醒。"),
                )
                .item(
                    SettingItem::new(
                        "任务完成通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentUiConfig>()
                                    .map(|c| c.notify_success)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_ui(|c| c.notify_success = v, cx);
                            },
                        ),
                    )
                    .description("Agent 当前回合正常完成时提醒。"),
                )
                .item(
                    SettingItem::new(
                        "任务失败通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentUiConfig>()
                                    .map(|c| c.notify_failure)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_ui(|c| c.notify_failure = v, cx);
                            },
                        ),
                    )
                    .description("Agent 因错误中断时提醒。"),
                )
                .item(
                    SettingItem::new(
                        "终端响铃通知",
                        SettingField::switch(
                            |cx: &App| {
                                cx.try_global::<AgentUiConfig>()
                                    .map(|c| c.notify_terminal_bell)
                                    .unwrap_or(true)
                            },
                            |v: bool, cx: &mut App| {
                                apply_agent_ui(|c| c.notify_terminal_bell = v, cx);
                            },
                        ),
                    )
                    .description("终端输出 BEL 控制字符时显示普通信息提醒。")
                    .keywords(["通知", "notification", "响铃", "bell"]),
                )
                .item(acp_cmd_setting_item(AcpAgentKind::Claude))
                .item(acp_cmd_setting_item(AcpAgentKind::Copilot))
                .item(acp_cmd_setting_item(AcpAgentKind::Codex))
                .item(acp_cmd_setting_item(AcpAgentKind::Grok))
                .item(
                    SettingItem::render({
                        let profile_editor_entity = entity.clone();
                        move |_, window, cx: &mut App| {
                            let muted = cx.theme().muted_foreground;
                            let border = cx.theme().border;
                            let fg = cx.theme().foreground;
                            let popover = cx.theme().popover;
                            let secondary = cx.theme().secondary;
                            let danger = cx.theme().danger;
                            let danger_fg = cx.theme().danger_foreground;
                            let field_w = {
                                let vw = f32::from(window.viewport_size().width);
                                let w = (vw - 250. - 80.).clamp(360., 720.);
                                px(w)
                            };
                            profile_editor_entity.update(cx, |ws, cx| {
                                ws.ensure_profile_inputs(window, cx);
                                let Some(inputs) = ws.profile_inputs.as_ref() else {
                                    return div().into_any_element();
                                };
                                let mut col = v_flex()
                                    .w(field_w)
                                    .gap_3()
                                    .child(
                                        v_flex()
                                            .w(field_w)
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .text_color(fg)
                                                    .child("手动添加 workspace"),
                                            )
                                            .child(
                                                div().w(field_w).text_sm().text_color(muted).child(
                                                    "同一家 agent 可以同时用好几个 workspace（比如 Claude \
                                                     默认的 ~/.claude 之外再开一个 ~/.claude-quant）。选好\
                                                     agent 类型、填上目录，启动命令自动拼好，不用自己写 \
                                                     shell 语法。「新建对话」菜单和历史会话页都会多出对应\
                                                     的入口。",
                                                ),
                                            ),
                                    );

                                let kind_w = px(120.);
                                let name_w = px(140.);
                                let del_w = px(28.);
                                let dir_w = field_w - kind_w - name_w - del_w - px(56.);
                                let mono = terminal_view::font_family();

                                let mut list = v_flex()
                                    .w(field_w)
                                    .gap_2()
                                    .p_3()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(border)
                                    .bg(secondary)
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(div().w(kind_w).child("Agent"))
                                            .child(div().w(name_w).child("名称"))
                                            .child(div().w(dir_w).child("Workspace 目录"))
                                            .child(div().w(del_w)),
                                    );

                                let profiles = cx.global::<AgentUiConfig>().profiles.clone();
                                for (ix, ((label, dir), p)) in
                                    inputs.rows.iter().zip(profiles.iter()).enumerate()
                                {
                                    let row_ix = ix;
                                    let kind_entity = profile_editor_entity.clone();
                                    let del_entity = profile_editor_entity.clone();
                                    let current_kind = p.kind();
                                    list = list.child(
                                        h_flex()
                                            .id(("profile-row", row_ix))
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                Button::new(("profile-kind", row_ix))
                                                    .ghost()
                                                    .small()
                                                    .w(kind_w)
                                                    .label(current_kind.short_label())
                                                    .dropdown_menu(move |mut menu, _window, _cx| {
                                                        for kind in AcpAgentKind::ALL {
                                                            let kind_entity = kind_entity.clone();
                                                            menu = menu.item(
                                                                PopupMenuItem::new(kind.label())
                                                                    .on_click(move |_ev, _window, cx| {
                                                                        kind_entity.update(cx, |ws, cx| {
                                                                            ws.set_profile_kind(
                                                                                row_ix, kind, cx,
                                                                            );
                                                                        });
                                                                    }),
                                                            );
                                                        }
                                                        menu
                                                    }),
                                            )
                                            .child(Input::new(label).w(name_w))
                                            .child(
                                                Input::new(dir).w(dir_w).font_family(mono.clone()),
                                            )
                                            .child(
                                                div()
                                                    .id(("del-profile", row_ix))
                                                    .size(del_w)
                                                    .flex()
                                                    .flex_none()
                                                    .items_center()
                                                    .justify_center()
                                                    .rounded_md()
                                                    .cursor_pointer()
                                                    .text_sm()
                                                    .text_color(muted)
                                                    .hover(|s| s.bg(danger).text_color(danger_fg))
                                                    .child("×")
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        move |_, _, cx: &mut App| {
                                                            del_entity.update(cx, |ws, cx| {
                                                                ws.remove_profile(row_ix, cx);
                                                            });
                                                        },
                                                    ),
                                            ),
                                    );
                                }
                                col = col.child(list);
                                let add_entity = profile_editor_entity.clone();
                                col.child(
                                    div()
                                        .id("add-profile")
                                        .h(px(36.))
                                        .w(field_w)
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_lg()
                                        .cursor_pointer()
                                        .text_sm()
                                        .text_color(fg)
                                        .bg(popover)
                                        .border_1()
                                        .border_color(border)
                                        .hover(|s| s.bg(border))
                                        .child("+ 添加 workspace")
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                            add_entity.update(cx, |ws, cx| ws.add_profile(cx));
                                        }),
                                )
                                .into_any_element()
                            })
                        }
                    })
                    .keywords(["workspace", "claude-quant", "config dir", "多工作区", "agent"]),
                )
                .item(SettingItem::render(move |_, _, cx: &mut App| {
                    // render 路径不走实时读盘：hooks 状态 5s 缓存，装/卸后主动失效。
                    let [claude_installed, copilot_installed, codex_installed] =
                        hooks_installed_status();
                    let installed = claude_installed && copilot_installed && codex_installed;
                    let (fg, muted, border) = {
                        let t = cx.theme();
                        (t.foreground, t.muted_foreground, t.border)
                    };
                    let status = format!(
                        "Claude {}  ·  Copilot {}  ·  Codex {}  ·  Grok {}",
                        if claude_installed { "已接入" } else { "未接入" },
                        if copilot_installed { "已接入" } else { "未接入" },
                        if codex_installed { "已接入" } else { "未接入" },
                        if claude_installed {
                            "已接入"
                        } else {
                            "未接入"
                        },
                    );
                    let status_color: Hsla = if installed {
                        rgb(crate::ui_theme::green()).into()
                    } else {
                        muted
                    };
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(status_color)
                                .child(status),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!(
                                    "路径：{}",
                                    smelt_notify_path().display()
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .id("install-agent-hooks")
                                        .px_3()
                                        .py(px(6.))
                                        .rounded_md()
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(border)
                                        .bg(crate::ui_theme::tint(crate::ui_theme::green(), 0x22))
                                        .text_sm()
                                        .text_color(rgb(crate::ui_theme::green()))
                                        .hover(|s| s.opacity(0.9))
                                        .child(if installed {
                                            "重新安装 hooks"
                                        } else {
                                            "安装 hooks"
                                        })
                                        .on_mouse_down(MouseButton::Left, move |_, window, cx: &mut App| {
                                            // 先记用户意图：即使某个 provider 本次安装失败，
                                            // 下次启动也会继续校正，而不是悄悄退回关闭。
                                            apply_agent_ui(|c| {
                                                c.agent_hooks_enabled = true;
                                            }, cx);
                                            let result = install_agent_hooks();
                                            invalidate_hooks_cache();
                                            match result {
                                                Ok(()) => {
                                                    window.push_notification(
                                                        Notification::success("Agent hooks 已安装"),
                                                        cx,
                                                    );
                                                    cx.refresh_windows();
                                                }
                                                Err(e) => {
                                                    eprintln!("[workspace] 安装 hooks 失败：{e}");
                                                    window.push_notification(
                                                        Notification::error(format!("安装失败：{e}")),
                                                        cx,
                                                    );
                                                    cx.refresh_windows();
                                                }
                                            }
                                        }),
                                )
                                .child(
                                    div()
                                        .id("uninstall-agent-hooks")
                                        .px_3()
                                        .py(px(6.))
                                        .rounded_md()
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(border)
                                        .text_sm()
                                        .text_color(fg)
                                        .hover(|s| s.bg(border))
                                        .child("移除 Smelt hooks")
                                        .on_mouse_down(MouseButton::Left, move |_, window, cx: &mut App| {
                                            // 先关闭自动安装，避免部分移除失败后重启又被装回。
                                            apply_agent_ui(|c| {
                                                c.agent_hooks_enabled = false;
                                            }, cx);
                                            let result = uninstall_agent_hooks();
                                            invalidate_hooks_cache();
                                            match result {
                                                Ok(()) => {
                                                    window.push_notification(
                                                        Notification::success("Smelt hooks 已移除"),
                                                        cx,
                                                    );
                                                    cx.refresh_windows();
                                                }
                                                Err(e) => {
                                                    eprintln!("[workspace] 移除 hooks 失败：{e}");
                                                    window.push_notification(
                                                        Notification::error(format!("移除失败：{e}")),
                                                        cx,
                                                    );
                                                    cx.refresh_windows();
                                                }
                                            }
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(
                                    "分别写入 Claude 设置、~/.copilot/hooks/smelt.json 和 ~/.codex/hooks.json；\
                                     Grok 会读取 Claude-compatible hooks。只增删 Smelt 条目；Codex 信任由 app-server 精确授权并复核，重开会话后生效。",
                                ),
                        )
                        .into_any_element()
                })),
        );

        // —— 远程：开启 → iroh → 写入 → 分享卡片（复制 + 扫码）——
        let remote_page = SettingPage::new("远程").group(
            SettingGroup::new().items(vec![
                SettingItem::new(
                    "开启远程",
                    SettingField::switch(
                        |cx: &App| cx.global::<RemoteConfig>().enabled,
                        |v: bool, cx: &mut App| apply_remote_toggle(v, cx),
                    ),
                )
                .description(
                    "打开后启用 iroh：优先打洞直连，打不通自动使用配置的 relay。关掉会停止分享。",
                ),
                SettingItem::new(
                    "Relay 地址",
                    SettingField::input(
                        |cx: &App| cx.global::<RemoteConfig>().iroh_relay.clone().into(),
                        |v: SharedString, cx: &mut App| apply_iroh_relay_value(v, cx),
                    ),
                )
                .description(
                    "填写自建 relay 的域名、IP 或完整 URL；省略协议时使用 https://。留空不会使用公共 relay。",
                ),
                SettingItem::new(
                    "允许远程写入",
                    SettingField::switch(
                        |cx: &App| cx.global::<RemoteConfig>().write_enabled,
                        |v: bool, cx: &mut App| apply_write_toggle(v, cx),
                    ),
                )
                .description(
                    "配对码持有者可在手机上输入、批准/拒绝权限。分享即授权。\
                     切换权限不会改变配对 Token，已配对手机继续有效。",
                ),
                // 分享卡片只展示 iroh 配对码；loopback 网关是内部实现，不对用户暴露。
                SettingItem::render(move |_, _, cx: &mut App| {
                    let cfg = cx.global::<RemoteConfig>().clone();
                    let remote = cx.global::<RemoteRuntimeState>().clone();
                    let iroh = cx
                        .try_global::<IrohRuntimeState>()
                        .cloned()
                        .unwrap_or_default();
                    let danger = cx.theme().danger;
                    let muted = cx.theme().muted_foreground;
                    let fg = cx.theme().foreground;

                    if !cfg.enabled {
                        return div()
                            .text_xs()
                            .text_color(muted)
                            .child("打开「开启远程」后，这里出现配对码与二维码。")
                            .into_any_element();
                    }

                    // iroh 准备中（绑定要连接用户配置的 relay）
                    if iroh.connecting {
                        return div()
                            .text_xs()
                            .text_color(muted)
                            .child("正在建立 iroh 通道…（连接 relay + 打洞）")
                            .into_any_element();
                    }

                    if let Some(err) = iroh.error.as_ref().or(remote.error.as_ref()) {
                        return v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(danger)
                                    .child(format!("出了点问题：{err}")),
                            )
                            .child(
                                btn("retry-remote", "重试".into()).on_mouse_down(
                                    MouseButton::Left,
                                    |_, _window, cx: &mut App| retry_remote_setup(cx),
                                ),
                            )
                            .into_any_element();
                    }

                    let Some(primary) = iroh.pairing_uri.clone() else {
                        return v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("还没有可用的配对码。"),
                            )
                            .child(
                                btn("retry-remote-empty", "重试".into()).on_mouse_down(
                                    MouseButton::Left,
                                    |_, _window, cx: &mut App| retry_remote_setup(cx),
                                ),
                            )
                            .into_any_element();
                    };

                    let scope = "iroh（优先直连，必要时中继）";
                    let mode = if iroh.write { "可写入" } else { "只读" };

                    let primary_copy = primary.clone();
                    // 仅展示后台预生成的二维码（绝不在 UI 线程现算 QR）。
                    let qr_png = iroh.qr_png.clone();

                    let mut card = v_flex().gap_2();
                    let mut row = h_flex().items_start().gap_3();
                    if let Some(png) = qr_png {
                        if !png.is_empty() {
                            row = row.child(
                                div()
                                    .p_2()
                                    .rounded(px(8.))
                                    // 二维码底必须是纯白，两种主题都一样：
                                    // 深色底上的二维码扫不出来。别跟着色板走。
                                    .bg(gpui::rgb(0xffffff))
                                    .child(
                                        img(std::sync::Arc::new(Image::from_bytes(
                                            ImageFormat::Png,
                                            png,
                                        )))
                                        .w(px(132.))
                                        .h(px(132.)),
                                    ),
                            );
                        }
                    }
                    row = row.child(
                        v_flex()
                            .gap_1p5()
                            .min_w(px(0.))
                            .flex_1()
                            .child(
                                div()
                                    .max_w(px(280.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_middle()
                                    .text_xs()
                                    .text_color(fg)
                                    .child(primary.clone()),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                    btn(
                                        "copy-share-link",
                                        copy_btn_label("copy-share-link", "复制配对码", cx),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_, window, cx: &mut App| {
                                            copy_with_feedback(
                                                primary_copy.clone(),
                                                "copy-share-link",
                                                "已复制配对码",
                                                window,
                                                cx,
                                            );
                                        },
                                    ),
                                    )
                                    .child(
                                        btn("refresh-remote-token", "刷新 Token".into())
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                |_, window, cx: &mut App| {
                                                    refresh_remote_token(window, cx)
                                                },
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{scope} · {mode}")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(
                                        "用 smelt 手机 App 扫码配对。电脑或服务重启不会改变 Token；手动刷新会使旧配对失效。",
                                    ),
                            ),
                    );
                    card = card.child(row);

                    card.into_any_element()
                }),
            ]),
        );

        // —— 键盘快捷键：只展示当前实际绑定，暂不支持用户改键 ——
        let shortcuts_page = SettingPage::new("键盘快捷键").resettable(false).group(
            SettingGroup::new().item(
                SettingItem::render(move |_, _, _| {
                    let keycap = move |key: &'static str| {
                        div()
                            .min_w(px(34.))
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .bg(popover)
                            .font_family("monospace")
                            .text_xs()
                            .text_color(fg)
                            .child(key)
                    };
                    let section = move |title: &'static str,
                                        shortcuts: &'static [(
                        &'static str,
                        &'static [&'static str],
                    )]| {
                        v_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                div()
                                    .pb_1()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(muted)
                                    .child(title),
                            )
                            .children(shortcuts.iter().map(|(label, keys)| {
                                h_flex()
                                    .w_full()
                                    .min_h(px(38.))
                                    .justify_between()
                                    .items_center()
                                    .gap_4()
                                    .border_b_1()
                                    .border_color(border)
                                    .child(div().text_sm().text_color(fg).child(*label))
                                    .child(
                                        h_flex()
                                            .flex_none()
                                            .gap_1()
                                            .children(keys.iter().map(|key| keycap(key))),
                                    )
                            }))
                    };

                    const GLOBAL: &[(&str, &[&str])] = &[
                        ("打开设置", &["⌘,"]),
                        ("打开命令面板", &["⌘K"]),
                        ("新建任务", &["⇧⌘N"]),
                        ("退出 Smelt", &["⌘Q"]),
                    ];
                    const SESSION: &[(&str, &[&str])] = &[
                        ("切换右侧面板", &["⌘B"]),
                        ("上一个 / 下一个会话", &["⌘↑", "⌘↓"]),
                        ("切换到第 1–9 个会话", &["⌘1…9"]),
                        ("上一个 / 下一个分屏", &["⌘[", "⌘]"]),
                        ("左右分屏 / 上下分屏", &["⌘D", "⇧⌘D"]),
                        ("关闭当前分屏或会话", &["⌘W"]),
                    ];
                    const NAVIGATION: &[(&str, &[&str])] = &[
                        ("保存当前文件", &["⌘S"]),
                        ("上一个 / 下一个差异", &["⇧F7", "F7"]),
                        ("关闭预览或返回", &["Esc"]),
                        ("终端补全 / 反向补全", &["Tab", "⇧Tab"]),
                    ];

                    v_flex()
                        .w_full()
                        .gap_6()
                        .child(section("全局", GLOBAL))
                        .child(section("会话与面板", SESSION))
                        .child(section("编辑与导航", NAVIGATION))
                        .into_any_element()
                })
                .keywords(["快捷键", "键盘", "shortcut", "keyboard", "hotkey"]),
            ),
        );

        div().size_full().child(
            // id 里带 nonce：见 `settings_page_nonce`，用来强制跳到 settings_page_ix。
            Settings::new(("settings", self.settings_page_nonce))
                .default_selected_index(SelectIndex {
                    page_ix: self.settings_page_ix,
                    group_ix: None,
                })
                .pages(vec![
                    appearance_page,
                    llm_page,
                    launch_page,
                    agent_page,
                    update_page,
                    storage_page,
                    remote_page,
                    shortcuts_page,
                ]),
        )
    }

    /// 打开独立设置窗口：已经开着就聚焦提到前台，不重复开第二扇。窗口只是个薄壳
    /// （[`SettingsWindow`]），真正的状态（颜色选择器/LLM 输入框等）还挂在这个
    /// Workspace 实体上没挪窝，薄壳每次渲染都转手调回来，天然跟主窗口保持同步。
    ///
    /// 必须用 `cx.defer` 推迟到当前这轮 `Workspace::update` 彻底返回之后再开窗：
    /// 这里被点齿轮的 `cx.listener` 调用时，`Workspace` 这个 entity 正被 update
    /// 占着；若同步 `cx.open_window`，新窗口首帧 `SettingsWindow::render` 里会
    /// 马上又对同一个 `Workspace` entity 调 `update`，两层嵌套 update 撞上 GPUI
    /// 的重入保护直接 panic 崩溃（"cannot update ... while it is already being
    /// updated"）——这就是「点齿轮整个 app 崩溃」的真正原因。
    pub fn open_settings_window(&self, cx: &mut Context<Self>) {
        let workspace = cx.entity();
        cx.defer(move |cx| {
            if let Some(handle) = cx.try_global::<SettingsWindowHandle>().and_then(|h| h.0) {
                if handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
                {
                    return;
                }
            }
            // 启动项编辑需要较宽的命令输入区；侧栏约 250，内容区至少要能放下长命令。
            let bounds = WindowBounds::centered(size(px(900.), px(700.)), cx);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("设置".into()),
                    ..Default::default()
                }),
                window_bounds: Some(bounds),
                ..Default::default()
            };
            let handle = cx
                .open_window(options, |window, cx| {
                    window.set_rem_size(px(19.));
                    let view = cx.new(|cx| SettingsWindow {
                        _observe_workspace: cx.observe(&workspace, |_, _, cx| cx.notify()),
                        workspace: workspace.clone(),
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("打开设置窗口失败");
            cx.set_global(SettingsWindowHandle(Some(handle)));
        });
    }
}

#[cfg(test)]
mod iroh_pairing_tests {
    use super::{RemoteConfig, qr_png_for_url};
    use smelt_core::pairing::iroh_pairing_uri;

    #[test]
    fn old_config_with_retired_remote_fields_still_loads() {
        // serde 默认忽略旧版的多通路开关；升级后 enabled 是唯一远程开关。
        let json = r#"{"enabled":true,"iroh_enabled":true,"tunnel_enabled":false,"webrtc_enabled":true,
                       "signal_http":"https://s.example.com","write_enabled":true}"#;
        let c: RemoteConfig = serde_json::from_str(json).expect("旧配置必须还能解析");
        assert!(c.enabled);
        assert!(c.write_enabled);
    }

    #[test]
    fn pairing_uri_renders_to_a_qr() {
        // 配对码比 http 链接长（endpoint_id 是 64 个十六进制字符），
        // 这里钉住「它确实能编成二维码」——超长内容会让 QrCode::new 失败。
        let uri = iroh_pairing_uri(
            &"a".repeat(64),
            &"b".repeat(32),
            "https://relay.example.test",
        );
        let png = qr_png_for_url(&uri).expect("配对码必须能生成二维码");
        assert!(!png.is_empty());
        assert_eq!(&png[1..4], b"PNG", "应当是 PNG 字节流");
    }
}

#[cfg(test)]
mod daemon_info_tests {
    use super::{acp_cmd_setting_value, command_uses_smelt_notify, daemon_info_line, fmt_uptime};
    use crate::terminal::DaemonInfo;
    use smelt_core::agent_kind::AcpAgentKind;

    #[test]
    fn builtin_agent_command_is_hidden_in_settings() {
        let agent = AcpAgentKind::Codex;
        assert!(acp_cmd_setting_value(agent, agent.default_cmd()).is_empty());
    }

    #[test]
    fn custom_agent_command_remains_visible_in_settings() {
        assert_eq!(
            acp_cmd_setting_value(AcpAgentKind::Codex, "codex-acp --custom".into()).as_ref(),
            "codex-acp --custom"
        );
    }

    #[test]
    fn fmt_uptime_picks_two_units() {
        assert_eq!(fmt_uptime(45), "45 秒");
        assert_eq!(fmt_uptime(600), "10 分钟");
        assert_eq!(fmt_uptime(3600 * 3 + 60 * 12), "3 小时 12 分");
        assert_eq!(fmt_uptime(86400 * 2 + 3600 * 5), "2 天 5 小时");
    }

    /// 老守护只回 version/exe_mtime：拿不到的字段整段省掉，不摆「未知」占位。
    #[test]
    fn old_daemon_without_new_fields_shows_only_version() {
        let info = DaemonInfo {
            version: Some("0.5.4".into()),
            ..Default::default()
        };
        assert_eq!(daemon_info_line(&info), "v0.5.4");
    }

    /// 全字段齐活：各段用 · 连起来，PID 和会话数都在。
    #[test]
    fn full_info_joins_all_parts() {
        let info = DaemonInfo {
            version: Some("0.5.4".into()),
            pid: Some(64954),
            started_at: Some(1_000_000),
            session_count: Some(5),
        };
        let line = daemon_info_line(&info);
        assert!(
            line.starts_with("v0.5.4 · PID 64954 · 启动于 "),
            "got {line}"
        );
        assert!(line.contains("已运行 "), "got {line}");
        assert!(line.ends_with("· 5 个会话"), "got {line}");
    }

    /// 守护时钟比 GUI 快时不能算出天文数字（saturating_sub 兜底）。
    #[test]
    fn future_started_at_does_not_underflow() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 9999;
        let info = DaemonInfo {
            started_at: Some(future),
            ..Default::default()
        };
        assert!(daemon_info_line(&info).contains("已运行 0 秒"));
    }

    #[test]
    fn hook_ownership_requires_smelt_notify_as_the_executable() {
        assert!(command_uses_smelt_notify(
            "SMELT_HOOK_PROVIDER=copilot /Users/test/.smelt/bin/smelt-notify"
        ));
        assert!(command_uses_smelt_notify(
            "'/Applications/Smelt App/smelt-notify'"
        ));
        assert!(!command_uses_smelt_notify("echo smelt-notify"));
        assert!(!command_uses_smelt_notify(
            "if test -x /tmp/smelt-notify; then /tmp/other-hook; fi"
        ));
    }

    #[test]
    fn removing_smelt_hook_preserves_third_party_handlers() {
        let dir = std::env::temp_dir().join(format!(
            "smelt-hook-remove-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hooks.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "hooks": {
                    "PreToolUse": [{
                        "matcher": "*",
                        "hooks": [
                            {"type":"command","command":"/tmp/smelt-notify"},
                            {"type":"command","command":"/tmp/orca-hook"}
                        ]
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        super::uninstall_hook_file(Some(path.clone()), &["PreToolUse"]).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let handlers = value["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["command"], "/tmp/orca-hook");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_json_write_keeps_dotfile_symlink() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "smelt-hook-symlink-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("managed-settings.json");
        let link = dir.join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        symlink(&target, &link).unwrap();

        super::write_json_atomic(&link, &serde_json::json!({"hooks":{"Stop":[]}})).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(value["hooks"]["Stop"], serde_json::json!([]));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
