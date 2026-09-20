//! 插件 UI contribution 的发现与渲染。
//!
//! 宿主对具体插件零编译期依赖：基础身份读 `plugin.json`，面板声明读可选的
//! `plugin-ui.json`，画的是包里的网页。装一个包就多一个 tab，删掉包就少一个
//! tab，换掉包就是升级。
//!
//! Tab 的身份是 `(plugin_id, contribution_id)` 这对字符串。但 `ToolPanelTab`
//! 必须是 `Copy`（它在整个 workspace 里按值传递），所以运行时用注册表里的槽位
//! 下标代表它，序列化时再换回字符串——这样存档不会因为插件增删而错位，
//! 而卸载掉的插件在存档里会自然降级成 Files。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use base64::Engine as _;
use gpui::*;
use sha2::{Digest, Sha256};
use smelt_plugin_api::{
    Contribution, EntityDecorationTone, EntityDecorationView, SettingsActionView,
    SettingsAvatarData, SettingsAvatarRef, SettingsItemView, SettingsSectionView,
};

use crate::settings::plugins::PluginEnablementState;
use crate::{Workspace, overlay, terminal, ui_theme};

/// 面板 invocation 的超时。页面在等一次往返，给不出结果时要让它尽快失败，
/// 而不是把加载态挂在那儿。
const INVOKE_TIMEOUT_MS: u64 = 8_000;
const SETTINGS_ACTION_TIMEOUT_MS: u64 = 30_000;
const SESSION_ACTION_TIMEOUT_MS: u64 = 30_000;
const PRESENTATION_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const MAX_AGENT_ICON_BYTES: u64 = 256 * 1024;
const PLUGIN_AGENT_ASSET_PREFIX: &str = "smelt-plugin-agent/";

/// 一个插件贡献的工具面板 tab。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginTab {
    pub plugin_id: String,
    pub contribution_id: String,
    pub title: String,
    /// WebView 的资源根：入口文件所在目录。页面读不到这个目录以外的东西。
    pub asset_root: PathBuf,
    /// manifest 里声明并已授予的 capability。宿主代办命令按它逐条门控，
    /// 不认插件身份——换句话说这里不会出现任何 `plugin_id` 的判断。
    pub capabilities: Vec<String>,
}

impl PluginTab {
    pub(crate) fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|held| held == capability)
    }

    /// 插件是否获授 `web.browse`（决定 CSP 放不放开 frame-src）。
    fn allow_web_browse(&self) -> bool {
        self.has_capability(smelt_plugin_api::CORE_CAPABILITY_WEB_BROWSE)
    }

    /// WebView 面板 id，同时是自定义协议的 host 段，所以只能含
    /// 字母数字和 `.-_`。
    pub(crate) fn panel_id(&self) -> String {
        let sanitize = |value: &str| {
            value
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                        ch
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
        };
        format!(
            "{}--{}",
            sanitize(&self.plugin_id),
            sanitize(&self.contribution_id)
        )
    }

    /// 存档里的稳定身份。
    pub(crate) fn key(&self) -> String {
        format!("{}/{}", self.plugin_id, self.contribution_id)
    }
}

/// 插件声明的产品级智能体展示信息。这里不携带 ACP provider：同一个量化或
/// 产品智能体可以选择不同执行器，侧栏身份不能跟着执行器变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginAgentPresentation {
    pub plugin_id: String,
    pub contribution_id: String,
    pub name: String,
    pub controller_id: String,
    /// 交给 GPUI AssetSource 的内容寻址虚拟路径。None 时 UI 使用通用 Bot 图标。
    pub icon_asset: Option<String>,
}

impl PluginAgentPresentation {
    fn key(&self) -> String {
        format!("{}/{}", self.plugin_id, self.contribution_id)
    }
}

/// SessionController 暴露给宿主标准位置的动作。只有插件已启用且运行时可用时
/// 才进入注册表；这与静态 Agent 名称/图标的降级语义不同。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginSessionAction {
    pub plugin_id: String,
    pub contribution_id: String,
    pub title: String,
    pub operation: String,
    pub controller_id: String,
    pub locations: Vec<smelt_plugin_api::SessionActionLocation>,
    pub icon: Option<smelt_plugin_api::SessionActionIcon>,
    pub result: smelt_plugin_api::SessionActionResult,
}

impl PluginSessionAction {
    fn key(&self) -> String {
        format!("{}/{}", self.plugin_id, self.contribution_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginCommand {
    pub plugin_id: String,
    pub contribution_id: String,
    pub operation: String,
}

impl PluginCommand {
    fn key(&self) -> String {
        format!("{}/{}", self.plugin_id, self.contribution_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginSettingsSection {
    pub plugin_id: String,
    pub contribution_id: String,
    pub title: String,
    pub description: Option<String>,
    pub snapshot: PluginCommand,
}

impl PluginSettingsSection {
    fn key(&self) -> String {
        format!("{}/{}", self.plugin_id, self.contribution_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginSidebarAccount {
    pub plugin_id: String,
    pub contribution_id: String,
    pub settings_section_id: String,
    pub account_item_id: String,
}

impl PluginSidebarAccount {
    fn key(&self) -> String {
        format!("{}/{}", self.plugin_id, self.contribution_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PluginEntityDecoration {
    plugin_id: String,
    contribution_id: String,
    resource_type: String,
    icon_asset: Option<String>,
    snapshot: PluginCommand,
}

impl PluginEntityDecoration {
    fn key(&self) -> String {
        format!("{}/{}", self.plugin_id, self.contribution_id)
    }
}

/// 注册表里的槽位。`ToolPanelTab` 需要 `Copy`，字符串进不去。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PluginTabSlot(pub(crate) u16);

thread_local! {
    /// GUI 是单线程的，注册表跟着主线程走即可。它的顺序是排过序的，
    /// 因此同一批插件在两次启动之间槽位一致。
    static REGISTRY: RefCell<Vec<PluginTab>> = const { RefCell::new(Vec::new()) };
    static SURFACE_REGISTRY: RefCell<Vec<PluginTab>> = const { RefCell::new(Vec::new()) };
    /// 产品智能体元数据即使插件暂时停用/运行时未就绪也保留：已有会话仍应显示
    /// 自己是谁；能否执行生命周期动作由 daemon 的活动 contribution 决定。
    static AGENT_REGISTRY: RefCell<Vec<PluginAgentPresentation>> = const { RefCell::new(Vec::new()) };
    static SESSION_ACTION_REGISTRY: RefCell<Vec<PluginSessionAction>> = const { RefCell::new(Vec::new()) };
    static COMMAND_REGISTRY: RefCell<Vec<PluginCommand>> = const { RefCell::new(Vec::new()) };
    static SETTINGS_SECTION_REGISTRY: RefCell<Vec<PluginSettingsSection>> = const { RefCell::new(Vec::new()) };
    static SIDEBAR_ACCOUNT_REGISTRY: RefCell<Vec<PluginSidebarAccount>> = const { RefCell::new(Vec::new()) };
    static ENTITY_DECORATION_REGISTRY: RefCell<Vec<PluginEntityDecoration>> = const { RefCell::new(Vec::new()) };
    /// 每个面板最近一次推送成功的上下文，用来只推变化。面板隐藏时清空。
    static LAST_CONTEXT: RefCell<std::collections::HashMap<String, String>> =
        RefCell::new(std::collections::HashMap::new());
    static LAST_PRESENTATION_REFRESH: RefCell<Option<std::time::Instant>> = const { RefCell::new(None) };
    static PRESENTATION_REFRESH_IN_FLIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PRESENTATION_REFRESH_QUEUED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PRESENTATION_REFRESH_TIMER_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PluginSettingsSectionSnapshot {
    pub view: Option<SettingsSectionView>,
    pub error: Option<String>,
    pub action_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PluginEntityDecorationSnapshot {
    view: Option<EntityDecorationView>,
    error: Option<String>,
}

#[derive(Clone)]
struct CachedPluginAvatar {
    revision: String,
    image: Arc<Image>,
}

#[derive(Clone, Default)]
pub(crate) struct PluginPresentationState {
    sections: HashMap<String, PluginSettingsSectionSnapshot>,
    entity_decorations: HashMap<String, PluginEntityDecorationSnapshot>,
    avatars: HashMap<String, CachedPluginAvatar>,
    pending_actions: HashSet<String>,
}

impl Global for PluginPresentationState {}

#[derive(Clone)]
pub(crate) struct PluginSidebarAccountMenuPresentation {
    pub plugin_id: String,
    pub settings_section_id: String,
    pub settings_title: String,
    pub actions: Vec<SettingsActionView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginEntityDecorationPresentation {
    pub icon_asset: Option<String>,
    pub badge: Option<String>,
    pub tone: EntityDecorationTone,
    pub tooltip: Option<String>,
    pub external_url: Option<String>,
}

/// AssetSource 可能在非 GUI 线程取图标，不能读取上面的 thread-local registry。
/// 刷新时整表替换，读取时只克隆单个受 256 KiB 限制的 SVG。
static AGENT_ASSETS: OnceLock<RwLock<HashMap<String, Vec<u8>>>> = OnceLock::new();

fn agent_assets() -> &'static RwLock<HashMap<String, Vec<u8>>> {
    AGENT_ASSETS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 严格从插件包内解析 Agent 图标，并转换为内容寻址的宿主资产路径。路径逃逸、
/// 非 SVG、非常规文件或超大文件都只让该智能体回退通用图标，不阻断插件主体。
fn load_agent_icon(
    package: &smelt_plugin_host::PluginPackage,
    plugin_id: &str,
    contribution_id: &str,
    declared: &str,
) -> Option<(String, Vec<u8>)> {
    let path = package.resolve_asset(declared)?;
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        return None;
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_AGENT_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let asset = format!("{PLUGIN_AGENT_ASSET_PREFIX}{plugin_id}/{contribution_id}/{digest}.svg");
    Some((asset, bytes))
}

/// 从单个已校验包抽取静态 Agent 展示贡献。独立于插件进程是否可启动。
fn discover_package_agents(
    package: &smelt_plugin_host::PluginPackage,
) -> (Vec<PluginAgentPresentation>, HashMap<String, Vec<u8>>) {
    let plugin_id = package.manifest().id.as_str().to_string();
    let mut agents = Vec::new();
    let mut assets = HashMap::new();
    for contribution in &package.manifest().contributions {
        let Contribution::Agent {
            id,
            name,
            icon,
            controller,
        } = contribution
        else {
            continue;
        };
        let icon_asset = icon.as_deref().and_then(|declared| {
            let loaded = load_agent_icon(package, &plugin_id, id.as_str(), declared);
            if loaded.is_none() {
                eprintln!("[plugin-ui] {plugin_id} 的智能体图标 {declared} 无效，使用通用图标");
            }
            loaded.map(|(asset, bytes)| {
                assets.insert(asset.clone(), bytes);
                asset
            })
        });
        agents.push(PluginAgentPresentation {
            plugin_id: plugin_id.clone(),
            contribution_id: id.as_str().to_string(),
            name: name.clone(),
            controller_id: controller.as_str().to_string(),
            icon_asset,
        });
    }
    (agents, assets)
}

fn discover_package_presentations(
    package: &smelt_plugin_host::PluginPackage,
) -> (
    Vec<PluginSettingsSection>,
    Vec<PluginSidebarAccount>,
    Vec<PluginCommand>,
) {
    let plugin_id = package.manifest().id.as_str().to_string();
    let commands = package
        .manifest()
        .contributions
        .iter()
        .filter_map(|contribution| {
            let Contribution::Command { id, operation, .. } = contribution else {
                return None;
            };
            Some(PluginCommand {
                plugin_id: plugin_id.clone(),
                contribution_id: id.as_str().to_string(),
                operation: operation.as_str().to_string(),
            })
        })
        .collect::<Vec<_>>();
    let command = |id: &smelt_plugin_api::ContributionId| {
        commands
            .iter()
            .find(|command| command.contribution_id == id.as_str())
            .cloned()
    };
    let sections = package
        .manifest()
        .contributions
        .iter()
        .filter_map(|contribution| {
            let Contribution::SettingsSection {
                id,
                title,
                description,
                snapshot,
            } = contribution
            else {
                return None;
            };
            let snapshot = command(snapshot)?;
            Some(PluginSettingsSection {
                plugin_id: plugin_id.clone(),
                contribution_id: id.as_str().to_string(),
                title: title.clone(),
                description: description.clone(),
                snapshot,
            })
        })
        .collect::<Vec<_>>();
    let accounts = package
        .manifest()
        .contributions
        .iter()
        .filter_map(|contribution| {
            let Contribution::SidebarAccount {
                id,
                settings_section,
                account_item,
            } = contribution
            else {
                return None;
            };
            Some(PluginSidebarAccount {
                plugin_id: plugin_id.clone(),
                contribution_id: id.as_str().to_string(),
                settings_section_id: settings_section.as_str().to_string(),
                account_item_id: account_item.as_str().to_string(),
            })
        })
        .collect();
    (sections, accounts, commands)
}

fn discover_package_entity_decorations(
    package: &smelt_plugin_host::PluginPackage,
    commands: &[PluginCommand],
) -> (Vec<PluginEntityDecoration>, HashMap<String, Vec<u8>>) {
    let plugin_id = package.manifest().id.as_str().to_string();
    let command = |id: &smelt_plugin_api::ContributionId| {
        commands
            .iter()
            .find(|command| command.contribution_id == id.as_str())
            .cloned()
    };
    let mut decorations = Vec::new();
    let mut assets = HashMap::new();
    for contribution in &package.manifest().contributions {
        let Contribution::EntityDecoration {
            id,
            resource_type,
            icon,
            snapshot,
        } = contribution
        else {
            continue;
        };
        let icon_asset = icon.as_deref().and_then(|declared| {
            let loaded = load_agent_icon(package, &plugin_id, id.as_str(), declared);
            if loaded.is_none() {
                eprintln!("[plugin-ui] {plugin_id} 的实体装饰图标 {declared} 无效，已忽略");
            }
            loaded.map(|(asset, bytes)| {
                assets.insert(asset.clone(), bytes);
                asset
            })
        });
        let Some(snapshot) = command(snapshot) else {
            continue;
        };
        decorations.push(PluginEntityDecoration {
            plugin_id: plugin_id.clone(),
            contribution_id: id.as_str().to_string(),
            resource_type: resource_type.as_str().to_string(),
            icon_asset,
            snapshot,
        });
    }
    (decorations, assets)
}

fn discover_package_session_actions(
    package: &smelt_plugin_host::PluginPackage,
) -> Vec<PluginSessionAction> {
    let plugin_id = package.manifest().id.as_str().to_string();
    package
        .manifest()
        .contributions
        .iter()
        .filter_map(|contribution| {
            let Contribution::SessionAction {
                id,
                title,
                operation,
                controller,
                locations,
                icon,
                result,
            } = contribution
            else {
                return None;
            };
            Some(PluginSessionAction {
                plugin_id: plugin_id.clone(),
                contribution_id: id.as_str().to_string(),
                title: title.clone(),
                operation: operation.as_str().to_string(),
                controller_id: controller.as_str().to_string(),
                locations: locations.clone(),
                icon: *icon,
                result: *result,
            })
        })
        .collect()
}

/// 按 daemon 已认证的 Agent→Controller 绑定查展示贡献。controller 不吻合时
/// 不接受只有同名 agent 的条目，避免损坏存档套用错误的会话 UI。
pub(crate) fn agent_presentation(
    binding: &smelt_plugin_api::AgentSessionBinding,
) -> Option<PluginAgentPresentation> {
    AGENT_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .find(|agent| {
                agent.plugin_id == binding.agent.plugin_id.as_str()
                    && agent.contribution_id == binding.agent.contribution_id.as_str()
                    && agent.plugin_id == binding.controller.plugin_id.as_str()
                    && agent.controller_id == binding.controller.contribution_id.as_str()
            })
            .cloned()
    })
}

pub(crate) fn settings_sections(plugin_id: &str) -> Vec<PluginSettingsSection> {
    SETTINGS_SECTION_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter(|section| section.plugin_id == plugin_id)
            .cloned()
            .collect()
    })
}

fn plugin_command(plugin_id: &str, contribution_id: &str) -> Option<PluginCommand> {
    COMMAND_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .find(|command| {
                command.plugin_id == plugin_id && command.contribution_id == contribution_id
            })
            .cloned()
    })
}

fn sidebar_accounts() -> Vec<PluginSidebarAccount> {
    SIDEBAR_ACCOUNT_REGISTRY.with(|registry| registry.borrow().clone())
}

fn all_plugin_commands() -> Vec<PluginCommand> {
    COMMAND_REGISTRY.with(|registry| registry.borrow().clone())
}

fn settings_section_key(plugin_id: &str, contribution_id: &str) -> String {
    format!("{plugin_id}/{contribution_id}")
}

fn settings_action_key(plugin_id: &str, section_id: &str, action_id: &str) -> String {
    format!("{plugin_id}/{section_id}/{action_id}")
}

fn avatar_key(plugin_id: &str, avatar: &SettingsAvatarRef) -> String {
    format!("{plugin_id}/{}", avatar.command)
}

pub(crate) fn settings_section_snapshot(
    cx: &App,
    plugin_id: &str,
    contribution_id: &str,
) -> PluginSettingsSectionSnapshot {
    cx.try_global::<PluginPresentationState>()
        .and_then(|state| {
            state
                .sections
                .get(&settings_section_key(plugin_id, contribution_id))
                .cloned()
        })
        .unwrap_or_default()
}

pub(crate) fn settings_action_pending(
    cx: &App,
    plugin_id: &str,
    section_id: &str,
    action_id: &str,
) -> bool {
    cx.try_global::<PluginPresentationState>()
        .is_some_and(|state| {
            state
                .pending_actions
                .contains(&settings_action_key(plugin_id, section_id, action_id))
        })
}

pub(crate) fn settings_avatar(
    cx: &App,
    plugin_id: &str,
    avatar: &SettingsAvatarRef,
) -> Option<Arc<Image>> {
    cx.try_global::<PluginPresentationState>()
        .and_then(|state| state.avatars.get(&avatar_key(plugin_id, avatar)))
        .filter(|cached| cached.revision == avatar.revision)
        .map(|cached| cached.image.clone())
}

pub(crate) fn sidebar_account_menu_presentations(
    cx: &App,
) -> Vec<PluginSidebarAccountMenuPresentation> {
    let Some(state) = cx.try_global::<PluginPresentationState>() else {
        return Vec::new();
    };
    let sections = SETTINGS_SECTION_REGISTRY.with(|registry| registry.borrow().clone());
    sidebar_accounts()
        .into_iter()
        .filter_map(|account| {
            let section = sections.iter().find(|section| {
                section.plugin_id == account.plugin_id
                    && section.contribution_id == account.settings_section_id
            })?;
            let snapshot = state.sections.get(&settings_section_key(
                &account.plugin_id,
                &account.settings_section_id,
            ))?;
            let item = snapshot.view.as_ref()?.items.iter().find(|item| {
                item.id().as_str() == account.account_item_id
                    && matches!(item, SettingsItemView::Account { .. })
            })?;
            let SettingsItemView::Account { actions, .. } = item else {
                return None;
            };
            Some(PluginSidebarAccountMenuPresentation {
                plugin_id: account.plugin_id,
                settings_section_id: account.settings_section_id,
                settings_title: section.title.clone(),
                actions: actions.clone(),
            })
        })
        .collect()
}

pub(crate) fn entity_decorations(
    resource: &smelt_plugin_api::PluginResourceRef,
    cx: &App,
) -> Vec<PluginEntityDecorationPresentation> {
    let Some(state) = cx.try_global::<PluginPresentationState>() else {
        return Vec::new();
    };
    ENTITY_DECORATION_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter(|decoration| {
                decoration.plugin_id == resource.plugin_id.as_str()
                    && decoration.resource_type == resource.resource_type.as_str()
            })
            .filter_map(|decoration| {
                let item = state
                    .entity_decorations
                    .get(&decoration.key())?
                    .view
                    .as_ref()?
                    .items
                    .iter()
                    .find(|item| item.resource_id == resource.resource_id)?;
                Some(PluginEntityDecorationPresentation {
                    icon_asset: decoration.icon_asset.clone(),
                    badge: item.badge.clone(),
                    tone: item.tone,
                    tooltip: item.tooltip.clone(),
                    external_url: item.external_url.clone(),
                })
            })
            .collect()
    })
}

fn invoke_plugin_command(
    command: &PluginCommand,
    payload: serde_json::Value,
    timeout_ms: u64,
) -> Result<serde_json::Value, String> {
    let request = smelt_plugin_api::InvocationRequest {
        invocation_id: smelt_plugin_api::InvocationId::new(
            uuid::Uuid::new_v4().simple().to_string(),
        )
        .map_err(|error| error.to_string())?,
        contribution_id: smelt_plugin_api::ContributionId::new(command.contribution_id.clone())
            .map_err(|error| error.to_string())?,
        operation: smelt_plugin_api::InvocationOperation::new(command.operation.clone())
            .map_err(|error| error.to_string())?,
        payload,
        deadline_ms: now_ms().saturating_add(timeout_ms),
    };
    terminal::plugin_invoke(&command.plugin_id, &request)
}

fn settings_view_commands_valid(
    plugin_id: &str,
    view: &SettingsSectionView,
    commands: &HashMap<String, PluginCommand>,
) -> Result<(), String> {
    for item in &view.items {
        for action in item.actions() {
            if !commands.contains_key(action.command.as_str()) {
                return Err(format!(
                    "插件 {plugin_id} 的设置动作 {} 引用了未声明的 Command",
                    action.id
                ));
            }
        }
        if let SettingsItemView::Account {
            avatar: Some(avatar),
            ..
        } = item
            && !commands.contains_key(avatar.command.as_str())
        {
            return Err(format!("插件 {plugin_id} 的设置头像引用了未声明的 Command"));
        }
    }
    Ok(())
}

fn image_format_from_bytes(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(ImageFormat::Webp)
    } else if bytes.starts_with(b"GIF8") {
        Some(ImageFormat::Gif)
    } else {
        None
    }
}

fn load_settings_avatar(command: &PluginCommand) -> Result<Arc<Image>, String> {
    let value = invoke_plugin_command(command, serde_json::json!({}), INVOKE_TIMEOUT_MS)?;
    let avatar: SettingsAvatarData =
        serde_json::from_value(value).map_err(|_| "插件头像响应格式无效".to_string())?;
    const MAX_ENCODED_AVATAR_BYTES: usize = 7 * 1024 * 1024;
    if avatar.data_base64.len() > MAX_ENCODED_AVATAR_BYTES {
        return Err("插件头像响应超过大小限制".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(avatar.data_base64)
        .map_err(|_| "插件头像不是合法 base64".to_string())?;
    let format =
        image_format_from_bytes(&bytes).ok_or_else(|| "插件头像不是支持的图片格式".to_string())?;
    let mime_matches = matches!(
        (avatar.mime.as_str(), format),
        ("image/png", ImageFormat::Png)
            | ("image/jpeg", ImageFormat::Jpeg)
            | ("image/webp", ImageFormat::Webp)
            | ("image/gif", ImageFormat::Gif)
    );
    if !mime_matches {
        return Err("插件头像 MIME 与文件内容不匹配".to_string());
    }
    Ok(Arc::new(Image::from_bytes(format, bytes)))
}

fn bounded_plugin_ui_error(error: &str) -> String {
    let mut output = String::new();
    for character in error.chars().map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    }) {
        if output.len().saturating_add(character.len_utf8()) > 1024 {
            break;
        }
        output.push(character);
    }
    output
}

struct LoadedSettingsSection {
    key: String,
    view: Result<SettingsSectionView, String>,
    avatars: Vec<(String, String, Result<Arc<Image>, String>)>,
}

struct LoadedEntityDecoration {
    key: String,
    view: Result<EntityDecorationView, String>,
}

fn load_entity_decoration(decoration: PluginEntityDecoration) -> LoadedEntityDecoration {
    let key = decoration.key();
    let view = invoke_plugin_command(
        &decoration.snapshot,
        serde_json::json!({}),
        INVOKE_TIMEOUT_MS,
    )
    .and_then(|value| {
        let view: EntityDecorationView =
            serde_json::from_value(value).map_err(|_| "插件实体装饰快照格式无效".to_string())?;
        view.validate().map_err(|error| error.to_string())?;
        Ok(view)
    });
    LoadedEntityDecoration { key, view }
}

fn load_settings_section(
    section: PluginSettingsSection,
    commands: Vec<PluginCommand>,
    cached_avatar_revisions: HashMap<String, String>,
) -> LoadedSettingsSection {
    let key = section.key();
    let commands = commands
        .into_iter()
        .filter(|command| command.plugin_id == section.plugin_id)
        .map(|command| (command.contribution_id.clone(), command))
        .collect::<HashMap<_, _>>();
    let view = invoke_plugin_command(&section.snapshot, serde_json::json!({}), INVOKE_TIMEOUT_MS)
        .and_then(|value| {
            let view: SettingsSectionView =
                serde_json::from_value(value).map_err(|_| "插件设置快照格式无效".to_string())?;
            view.validate().map_err(|error| error.to_string())?;
            settings_view_commands_valid(&section.plugin_id, &view, &commands)?;
            Ok(view)
        });
    let mut avatars = Vec::new();
    if let Ok(view) = &view {
        for avatar in view.items.iter().filter_map(|item| match item {
            SettingsItemView::Account {
                avatar: Some(avatar),
                ..
            } => Some(avatar),
            _ => None,
        }) {
            let cache_key = avatar_key(&section.plugin_id, avatar);
            if cached_avatar_revisions
                .get(&cache_key)
                .is_some_and(|revision| revision == &avatar.revision)
            {
                continue;
            }
            let result = commands
                .get(avatar.command.as_str())
                .ok_or_else(|| "插件头像 Command 不存在".to_string())
                .and_then(load_settings_avatar);
            avatars.push((cache_key, avatar.revision.clone(), result));
        }
    }
    LoadedSettingsSection { key, view, avatars }
}

fn schedule_presentation_refresh(cx: &App) {
    if PRESENTATION_REFRESH_TIMER_PENDING.with(|pending| pending.replace(true)) {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(PRESENTATION_REFRESH_INTERVAL)
            .await;
        PRESENTATION_REFRESH_TIMER_PENDING.with(|pending| pending.set(false));
        cx.update(|cx| start_presentation_refresh(cx, true));
    })
    .detach();
}

fn start_presentation_refresh(cx: &App, force: bool) {
    if PRESENTATION_REFRESH_IN_FLIGHT.with(std::cell::Cell::get) {
        if force {
            PRESENTATION_REFRESH_QUEUED.with(|queued| queued.set(true));
        }
        return;
    }
    if !force
        && LAST_PRESENTATION_REFRESH.with(|last| {
            last.borrow()
                .is_some_and(|last| last.elapsed() < PRESENTATION_REFRESH_INTERVAL)
        })
    {
        return;
    }
    PRESENTATION_REFRESH_IN_FLIGHT.with(|in_flight| in_flight.set(true));
    LAST_PRESENTATION_REFRESH.with(|last| *last.borrow_mut() = Some(std::time::Instant::now()));

    let sections = SETTINGS_SECTION_REGISTRY.with(|registry| registry.borrow().clone());
    let decorations = ENTITY_DECORATION_REGISTRY.with(|registry| registry.borrow().clone());
    let keep_refreshing = !sections.is_empty() || !decorations.is_empty();
    let active_keys = sections
        .iter()
        .map(PluginSettingsSection::key)
        .collect::<HashSet<_>>();
    let active_decoration_keys = decorations
        .iter()
        .map(PluginEntityDecoration::key)
        .collect::<HashSet<_>>();
    let commands = all_plugin_commands();
    let active_command_keys = commands
        .iter()
        .map(PluginCommand::key)
        .collect::<HashSet<_>>();
    let cached_avatar_revisions = cx
        .try_global::<PluginPresentationState>()
        .map(|state| {
            state
                .avatars
                .iter()
                .map(|(key, avatar)| (key.clone(), avatar.revision.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    cx.spawn(async move |cx| {
        let (loaded_sections, loaded_decorations) = cx
            .background_executor()
            .spawn(async move {
                let loaded_sections = sections
                    .into_iter()
                    .map(|section| {
                        load_settings_section(
                            section,
                            commands.clone(),
                            cached_avatar_revisions.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                let loaded_decorations = decorations
                    .into_iter()
                    .map(load_entity_decoration)
                    .collect::<Vec<_>>();
                (loaded_sections, loaded_decorations)
            })
            .await;
        PRESENTATION_REFRESH_IN_FLIGHT.with(|in_flight| in_flight.set(false));
        cx.update(|cx| {
            let had_state = cx.try_global::<PluginPresentationState>().is_some();
            let mut state = cx
                .try_global::<PluginPresentationState>()
                .cloned()
                .unwrap_or_default();
            let previous_sections = state.sections.clone();
            let previous_entity_decorations = state.entity_decorations.clone();
            let previous_avatar_revisions = state
                .avatars
                .iter()
                .map(|(key, avatar)| (key.clone(), avatar.revision.clone()))
                .collect::<HashMap<_, _>>();
            state.sections.retain(|key, _| active_keys.contains(key));
            state
                .entity_decorations
                .retain(|key, _| active_decoration_keys.contains(key));
            state
                .avatars
                .retain(|key, _| active_command_keys.contains(key));
            for loaded in loaded_sections {
                let snapshot = state.sections.entry(loaded.key).or_default();
                match loaded.view {
                    Ok(view) => {
                        snapshot.view = Some(view);
                        snapshot.error = None;
                    }
                    Err(error) => snapshot.error = Some(bounded_plugin_ui_error(&error)),
                }
                for (key, revision, image) in loaded.avatars {
                    match image {
                        Ok(image) => {
                            state
                                .avatars
                                .insert(key, CachedPluginAvatar { revision, image });
                        }
                        Err(error) => snapshot.error = Some(bounded_plugin_ui_error(&error)),
                    }
                }
            }
            for loaded in loaded_decorations {
                let snapshot = state.entity_decorations.entry(loaded.key).or_default();
                match loaded.view {
                    Ok(view) => {
                        snapshot.view = Some(view);
                        snapshot.error = None;
                    }
                    Err(error) => snapshot.error = Some(bounded_plugin_ui_error(&error)),
                }
            }
            let avatar_revisions = state
                .avatars
                .iter()
                .map(|(key, avatar)| (key.clone(), avatar.revision.clone()))
                .collect::<HashMap<_, _>>();
            if !had_state
                || state.sections != previous_sections
                || state.entity_decorations != previous_entity_decorations
                || avatar_revisions != previous_avatar_revisions
            {
                cx.set_global(state);
                cx.refresh_windows();
            }
            let queued = PRESENTATION_REFRESH_QUEUED.with(|queued| queued.replace(false));
            if queued {
                start_presentation_refresh(cx, true);
            } else if keep_refreshing {
                schedule_presentation_refresh(cx);
            }
        });
    })
    .detach();
}

pub(crate) fn refresh_presentations(cx: &App) {
    start_presentation_refresh(cx, false);
}

pub(crate) fn run_settings_action(
    plugin_id: String,
    section_id: String,
    action: SettingsActionView,
    toggle_value: Option<bool>,
    cx: &mut App,
) {
    if action.disabled {
        return;
    }
    let section_exists = settings_sections(&plugin_id)
        .iter()
        .any(|section| section.contribution_id == section_id);
    let Some(command) = plugin_command(&plugin_id, action.command.as_str()) else {
        return;
    };
    if !section_exists {
        return;
    }
    let Some(mut payload) = action.payload.as_object().cloned() else {
        return;
    };
    if let Some(value) = toggle_value {
        payload.insert("value".to_string(), serde_json::Value::Bool(value));
    }
    let pending_key = settings_action_key(&plugin_id, &section_id, action.id.as_str());
    let action_label = action.label;
    let mut state = cx
        .try_global::<PluginPresentationState>()
        .cloned()
        .unwrap_or_default();
    if !state.pending_actions.insert(pending_key.clone()) {
        return;
    }
    state
        .sections
        .entry(settings_section_key(&plugin_id, &section_id))
        .or_default()
        .action_error = None;
    cx.set_global(state);
    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                invoke_plugin_command(
                    &command,
                    serde_json::Value::Object(payload),
                    SETTINGS_ACTION_TIMEOUT_MS,
                )
            })
            .await;
        cx.update(|cx| {
            let mut state = cx
                .try_global::<PluginPresentationState>()
                .cloned()
                .unwrap_or_default();
            state.pending_actions.remove(&pending_key);
            if let Err(error) = result {
                let error = bounded_plugin_ui_error(&error);
                state
                    .sections
                    .entry(settings_section_key(&plugin_id, &section_id))
                    .or_default()
                    .action_error = Some(error.clone());
                let title = if action_label.is_empty() {
                    "插件操作".to_string()
                } else {
                    action_label.clone()
                };
                crate::status_item::notify_error(format!("{title}失败：{error}"));
            }
            cx.set_global(state);
            cx.refresh_windows();
            start_presentation_refresh(cx, true);
        });
    })
    .detach();
}

pub(crate) fn session_actions(
    binding: &smelt_plugin_api::AgentSessionBinding,
    location: smelt_plugin_api::SessionActionLocation,
) -> Vec<PluginSessionAction> {
    if binding.instance.plugin_id != binding.controller.plugin_id
        || agent_presentation(binding).is_none()
    {
        return Vec::new();
    }
    SESSION_ACTION_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter(|action| {
                action.plugin_id == binding.controller.plugin_id.as_str()
                    && action.controller_id == binding.controller.contribution_id.as_str()
                    && action.locations.contains(&location)
            })
            .cloned()
            .collect()
    })
}

enum SessionActionOutcome {
    Completed,
    OpenExternal(String),
}

fn session_action_outcome(
    result_kind: smelt_plugin_api::SessionActionResult,
    value: serde_json::Value,
) -> Result<SessionActionOutcome, String> {
    match result_kind {
        smelt_plugin_api::SessionActionResult::Ignore => Ok(SessionActionOutcome::Completed),
        smelt_plugin_api::SessionActionResult::OpenExternal => {
            let raw = value
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .ok_or_else(|| "插件会话动作未返回 url".to_string())?;
            let parsed = url::Url::parse(raw).map_err(|error| format!("无效外链：{error}"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err("插件会话动作只能打开 http(s) 外链".to_string());
            }
            Ok(SessionActionOutcome::OpenExternal(raw.to_string()))
        }
    }
}

fn invoke_session_action(
    action: &PluginSessionAction,
    payload: &smelt_plugin_api::SessionActionInvocationPayload,
) -> Result<SessionActionOutcome, String> {
    if action.plugin_id != payload.agent_session.controller.plugin_id.as_str()
        || action.controller_id != payload.agent_session.controller.contribution_id.as_str()
    {
        return Err("会话动作与 controller 不匹配".to_string());
    }
    let request = smelt_plugin_api::InvocationRequest {
        invocation_id: smelt_plugin_api::InvocationId::new(
            uuid::Uuid::new_v4().simple().to_string(),
        )
        .map_err(|error| error.to_string())?,
        contribution_id: smelt_plugin_api::ContributionId::new(action.contribution_id.clone())
            .map_err(|error| error.to_string())?,
        operation: smelt_plugin_api::InvocationOperation::new(action.operation.clone())
            .map_err(|error| error.to_string())?,
        payload: serde_json::to_value(payload).map_err(|error| error.to_string())?,
        deadline_ms: now_ms().saturating_add(SESSION_ACTION_TIMEOUT_MS),
    };
    let value = terminal::plugin_invoke(&action.plugin_id, &request)?;
    session_action_outcome(action.result, value)
}

impl Workspace {
    /// 执行插件 controller 声明的标准会话动作。阻塞 IPC 放后台；宿主统一处理
    /// 成功提示和受限外链，具体插件不会进入菜单事件闭包。
    pub(crate) fn run_plugin_session_action(
        &mut self,
        action: PluginSessionAction,
        payload: smelt_plugin_api::SessionActionInvocationPayload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = action.title.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { invoke_session_action(&action, &payload) })
                .await;
            let _ = this.update_in(cx, |_, _window, cx| match result {
                Ok(SessionActionOutcome::Completed) => {
                    crate::status_item::notify_success(format!("{title}：已完成"));
                }
                Ok(SessionActionOutcome::OpenExternal(url)) => cx.open_url(&url),
                Err(error) => crate::status_item::notify_error(format!("{title}失败：{error}")),
            });
        })
        .detach();
    }
}

/// `SmeltAssets` 的线程安全读取入口。
pub(crate) fn plugin_agent_asset(path: &str) -> Option<Vec<u8>> {
    if !path.starts_with(PLUGIN_AGENT_ASSET_PREFIX) {
        return None;
    }
    agent_assets().read().ok()?.get(path).cloned()
}

/// 一条宿主代办命令的注册项。与 `smeltd` 的 Action 表同构：id 加一条
/// capability，没有第三个维度，也没有插件身份。
struct HostCommandDescriptor {
    id: &'static str,
    capability: &'static str,
}

const HOST_COMMANDS: &[HostCommandDescriptor] = &[
    HostCommandDescriptor {
        id: smelt_plugin_api::CORE_HOST_COMMAND_CONTEXT,
        capability: smelt_plugin_api::CORE_CAPABILITY_UI_CONTEXT_READ,
    },
    HostCommandDescriptor {
        id: smelt_plugin_api::CORE_HOST_COMMAND_PICK_DIRECTORY,
        capability: smelt_plugin_api::CORE_CAPABILITY_UI_DIALOG,
    },
    HostCommandDescriptor {
        id: smelt_plugin_api::CORE_HOST_COMMAND_REVEAL,
        capability: smelt_plugin_api::CORE_CAPABILITY_FS_REVEAL,
    },
    HostCommandDescriptor {
        id: smelt_plugin_api::CORE_HOST_COMMAND_OPEN_EXTERNAL,
        capability: smelt_plugin_api::CORE_CAPABILITY_WEB_BROWSE,
    },
];

/// 命令应答。和 invocation 回包用同一个信封，页面那边只有一套等待逻辑。
fn reply_to_panel(panel_id: &str, rid: &str, result: Result<serde_json::Value, String>) {
    let reply = match result {
        Ok(value) => serde_json::json!({ "rid": rid, "result": value }),
        Err(error) => serde_json::json!({ "rid": rid, "error": error }),
    };
    if let Err(error) = smelt_webview::post_to_panel(panel_id, &reply.to_string()) {
        eprintln!("[plugin-ui] 回包失败: {error}");
    }
}

/// 在文件管理器里显示一个路径。
///
/// 只接受存在的绝对路径。这道闸防的是页面（最容易被内容影响的一层）拿相对
/// 路径或不存在的路径去撞 `open`，不是防插件进程——插件本来就是不受 fs 沙箱
/// 约束的原生程序，它自己就能开 Finder。
fn reveal_in_file_manager(path: &str) -> Result<serde_json::Value, String> {
    let candidate = std::path::Path::new(path);
    if !candidate.is_absolute() {
        return Err("reveal path must be absolute".into());
    }
    if !candidate.exists() {
        return Err("reveal path does not exist".into());
    }
    std::process::Command::new("open")
        .arg("-R")
        .arg(candidate)
        .spawn()
        .map(|_| serde_json::Value::Null)
        .map_err(|error| error.to_string())
}

/// 用系统默认浏览器打开链接。只放行 http(s)：页面传什么都可能进来，
/// 这里是最后一道闸。
fn open_external(url: &str) -> Result<serde_json::Value, String> {
    let lowered = url.to_ascii_lowercase();
    if !lowered.starts_with("http://") && !lowered.starts_with("https://") {
        return Err("only http(s) urls can be opened".into());
    }
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| serde_json::Value::Null)
        .map_err(|error| error.to_string())
}

/// 重新扫描已安装且已启用的插件包，重建 tab 注册表。
///
/// 在设置页开关插件之后也要调用——关掉的插件必须立刻从 tab 栏消失。
/// 首帧建一次注册表。放在渲染路径上是因为它依赖 `PluginEnablementState`
/// 这个 global，而后者在窗口打开之后才就绪。
pub(crate) fn refresh_once(cx: &App) {
    use std::cell::Cell;
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
    }
    if DONE.with(Cell::get) {
        return;
    }
    DONE.with(|done| done.set(true));
    refresh(cx);
}

pub(crate) fn refresh(cx: &App) {
    let enablement = cx.try_global::<PluginEnablementState>().cloned();
    let is_enabled = |plugin_id: &str| {
        enablement.as_ref().map_or_else(
            || smelt_core::plugin_enablement::PluginEnablement::default().is_enabled(plugin_id),
            |state| state.is_enabled(plugin_id),
        )
    };
    // 脚本插件的运行时可能还没就位（首次启动正在下载受管 bun）。这时它的进程起不来，
    // tab 摆在那里点开只有空白——跟插件被停用是同一种状态，就按同一条路径隐藏。
    // 运行时下载完成后守护会重启插件集，GUI 这边下一次 refresh 自然把 tab 补回来。
    let bun = smelt_core::acp_conn::managed_bun_if_ready();
    let mut tabs = Vec::new();
    let mut surfaces = Vec::new();
    let mut agents = Vec::new();
    let mut session_actions = Vec::new();
    let mut commands = Vec::new();
    let mut settings_sections = Vec::new();
    let mut sidebar_accounts = Vec::new();
    let mut entity_decorations = Vec::new();
    let mut assets = HashMap::new();
    for package in terminal::discover_installed_plugin_packages()
        .into_iter()
        .filter_map(|result| match result {
            Ok(package) => Some(package),
            Err(error) => {
                eprintln!("[plugin-ui] 忽略不可用插件包：{error}");
                None
            }
        })
    {
        let manifest = package.manifest();
        let plugin_id = manifest.id.as_str().to_string();
        // Agent 名称/图标是已有会话的静态身份，不因插件进程临时不可用而消失。
        // 生命周期动作是否可用仍以 daemon 当前装载的 contribution 为准。
        let (package_agents, package_assets) = discover_package_agents(&package);
        agents.extend(package_agents);
        assets.extend(package_assets);
        if !is_enabled(&plugin_id) {
            continue;
        }
        if let Err(error) = package.runtime_available(bun.as_deref()) {
            eprintln!("[plugin-ui] {plugin_id} 暂不可用：{error}");
            continue;
        }
        session_actions.extend(discover_package_session_actions(&package));
        let (package_sections, package_accounts, package_commands) =
            discover_package_presentations(&package);
        let (package_decorations, package_decoration_assets) =
            discover_package_entity_decorations(&package, &package_commands);
        settings_sections.extend(package_sections);
        sidebar_accounts.extend(package_accounts);
        entity_decorations.extend(package_decorations);
        assets.extend(package_decoration_assets);
        commands.extend(package_commands);
        let capabilities = manifest
            .capabilities
            .iter()
            .map(|capability| capability.as_str().to_string())
            .collect::<Vec<_>>();
        for contribution in &manifest.contributions {
            let (id, title, entry, into_surfaces) = match contribution {
                Contribution::ToolPanel { id, title, entry } => (id, title, entry, false),
                Contribution::WorkspaceSurface { id, title, entry } => (id, title, entry, true),
                _ => continue,
            };
            // manifest 是插件自己写的，不能当安全依据：这一步用 canonicalize
            // 确认入口确实落在包内，解析不出来就跳过这个 contribution。
            let Some(entry_path) = package.resolve_asset(entry) else {
                eprintln!("[plugin-ui] {plugin_id} 的面板入口 {entry} 不在包内，已跳过");
                continue;
            };
            let Some(asset_root) = entry_path.parent().map(PathBuf::from) else {
                continue;
            };
            let tab = PluginTab {
                plugin_id: plugin_id.clone(),
                contribution_id: id.as_str().to_string(),
                title: title.clone(),
                asset_root,
                capabilities: capabilities.clone(),
            };
            if into_surfaces {
                surfaces.push(tab);
            } else {
                tabs.push(tab);
            }
        }
    }
    // 槽位下标要稳定，否则存档里的 tab 会在插件增删后指到别人身上。
    tabs.sort_by_key(PluginTab::key);
    surfaces.sort_by_key(PluginTab::key);
    agents.sort_by_key(PluginAgentPresentation::key);
    session_actions.sort_by_key(PluginSessionAction::key);
    commands.sort_by_key(PluginCommand::key);
    settings_sections.sort_by_key(PluginSettingsSection::key);
    sidebar_accounts.sort_by_key(PluginSidebarAccount::key);
    entity_decorations.sort_by_key(PluginEntityDecoration::key);
    if !tabs.is_empty() {
        eprintln!(
            "[plugin-ui] 装载 {} 个插件面板: {}",
            tabs.len(),
            tabs.iter()
                .map(|tab| format!("{}（{}）", tab.key(), tab.title))
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    if !surfaces.is_empty() {
        eprintln!(
            "[plugin-ui] 装载 {} 个工作台页面: {}",
            surfaces.len(),
            surfaces
                .iter()
                .map(|tab| format!("{}（{}）", tab.key(), tab.title))
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    if !agents.is_empty() {
        eprintln!(
            "[plugin-ui] 装载 {} 个智能体展示贡献: {}",
            agents.len(),
            agents
                .iter()
                .map(|agent| format!("{}（{}）", agent.key(), agent.name))
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    if !session_actions.is_empty() {
        eprintln!(
            "[plugin-ui] 装载 {} 个会话动作: {}",
            session_actions.len(),
            session_actions
                .iter()
                .map(|action| format!("{}（{}）", action.key(), action.title))
                .collect::<Vec<_>>()
                .join("、")
        );
    }

    // 已经不在名单里的面板要连 WebView 一起收掉，不能只是不画。
    let mut removed = REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter(|existing| !tabs.contains(existing))
            .map(PluginTab::panel_id)
            .collect::<Vec<_>>()
    });
    removed.extend(SURFACE_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .filter(|existing| !surfaces.contains(existing))
            .map(PluginTab::panel_id)
            .collect::<Vec<_>>()
    }));
    for panel_id in removed {
        smelt_webview::close_panel(&panel_id);
    }
    REGISTRY.with(|registry| *registry.borrow_mut() = tabs);
    SURFACE_REGISTRY.with(|registry| *registry.borrow_mut() = surfaces);
    AGENT_REGISTRY.with(|registry| *registry.borrow_mut() = agents);
    SESSION_ACTION_REGISTRY.with(|registry| *registry.borrow_mut() = session_actions);
    COMMAND_REGISTRY.with(|registry| *registry.borrow_mut() = commands);
    SETTINGS_SECTION_REGISTRY.with(|registry| *registry.borrow_mut() = settings_sections);
    SIDEBAR_ACCOUNT_REGISTRY.with(|registry| *registry.borrow_mut() = sidebar_accounts);
    ENTITY_DECORATION_REGISTRY.with(|registry| *registry.borrow_mut() = entity_decorations);
    if let Ok(mut registry) = agent_assets().write() {
        *registry = assets;
    }
    start_presentation_refresh(cx, true);
}

pub(crate) fn slots() -> Vec<PluginTabSlot> {
    REGISTRY.with(|registry| {
        (0..registry.borrow().len())
            .map(|index| PluginTabSlot(index as u16))
            .collect()
    })
}

pub(crate) fn tab(slot: PluginTabSlot) -> Option<PluginTab> {
    REGISTRY.with(|registry| registry.borrow().get(usize::from(slot.0)).cloned())
}

pub(crate) fn title(slot: PluginTabSlot) -> String {
    tab(slot).map_or_else(|| "插件".to_string(), |tab| tab.title)
}

/// 存档用：槽位 → `plugin_id/contribution_id`。
pub(crate) fn key_for(slot: PluginTabSlot) -> Option<String> {
    tab(slot).map(|tab| tab.key())
}

/// 存档用：`plugin_id/contribution_id` → 槽位。
///
/// 插件被卸载或停用后这里返回 `None`，调用方据此把存档里的 tab 降级——
/// 这正是"可拔插"在持久化层的表现。
pub(crate) fn slot_for_key(key: &str) -> Option<PluginTabSlot> {
    REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .position(|tab| tab.key() == key)
            .map(|index| PluginTabSlot(index as u16))
    })
}

pub(crate) fn workspace_surfaces() -> Vec<PluginTab> {
    SURFACE_REGISTRY.with(|registry| registry.borrow().clone())
}

pub(crate) fn workspace_surface_by_key(key: &str) -> Option<PluginTab> {
    SURFACE_REGISTRY.with(|registry| {
        registry
            .borrow()
            .iter()
            .find(|tab| tab.key() == key)
            .cloned()
    })
}

fn plugin_webviews() -> Vec<PluginTab> {
    let mut tabs = REGISTRY.with(|registry| registry.borrow().clone());
    tabs.extend(SURFACE_REGISTRY.with(|registry| registry.borrow().clone()));
    tabs
}

fn plugin_tab_by_panel_id(panel_id: &str) -> Option<PluginTab> {
    plugin_webviews()
        .into_iter()
        .find(|tab| tab.panel_id() == panel_id)
}

fn panel_spec(tab: &PluginTab) -> smelt_webview::PanelSpec {
    let hex = |color: u32| format!("#{color:06x}");
    smelt_webview::PanelSpec {
        id: tab.panel_id(),
        source: smelt_webview::PanelSource::PackageRoot(tab.asset_root.clone()),
        theme: vec![
            ("bg".into(), hex(ui_theme::bg_stage())),
            ("fg".into(), hex(ui_theme::text())),
            ("muted".into(), hex(ui_theme::text_muted())),
            ("accent".into(), hex(ui_theme::accent())),
            ("on-accent".into(), hex(ui_theme::on_accent())),
            ("raised".into(), hex(ui_theme::bg_card())),
            ("hairline".into(), hex(ui_theme::border_dim())),
        ],
        allow_web_browse: tab.allow_web_browse(),
        devtools: cfg!(debug_assertions),
    }
}

impl Workspace {
    pub(crate) fn workspace_surface_display_title(&self, key: &str) -> String {
        self.workspace_surface_titles
            .get(key)
            .cloned()
            .unwrap_or_else(|| {
                workspace_surface_by_key(key).map_or_else(|| "插件".to_string(), |tab| tab.title)
            })
    }

    pub(crate) fn open_workspace_surface(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if workspace_surface_by_key(&key).is_none() {
            return;
        }
        self.activate_workspace_tab(crate::WorkspaceRoute::Plugin { key }, window, cx);
    }

    pub(crate) fn close_workspace_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.nav.close_plugin() {
            return;
        }
        self.save_state(cx);
        self.focus_active_stage(window, cx);
        cx.notify();
    }

    /// 当前该显示的插件面板。切走 tab、收起面板、打开动画期间都返回 `None`。
    fn active_plugin_tab(&self) -> Option<PluginTab> {
        if let Some(key) = self.plugin_surface_key() {
            return workspace_surface_by_key(key);
        }
        use crate::tool_panel::ToolPanelTab;
        let slot = match self.active_stage_tool_panel_tab() {
            Some(ToolPanelTab::Plugin(slot)) => Some(slot),
            Some(_) => None,
            None => (self.tool_panel_open && !self.tool_panel_transition.is_opening())
                .then(|| match self.tool_panel_tab {
                    ToolPanelTab::Plugin(slot) => Some(slot),
                    _ => None,
                })
                .flatten(),
        }?;
        tab(slot)
    }

    /// 每帧对齐：只有当前这一个面板可见，其余全部藏起来；一个面板都不可见时，
    /// AppKit 的 first responder 必须回到 GPUI。
    ///
    /// WebView 是原生 view，GPUI 不画那块区域并不会让它消失，所以显隐必须显式做。
    ///
    /// 焦点同理，而且判据只用一个客观事实——**有没有可见的插件面板**，不去猜
    /// 用户想在哪打字：
    ///
    /// - 没有面板可见 → first responder 只能是 GPUI 的 view。切走 tab、收起
    ///   面板、停用插件全都落进这一条，不必在每条路径上分别补。
    /// - 有面板可见 → 由 WebView 宿主在「刚显示」时把子窗口变成 key，避免
    ///   侧栏那次点击之后还要再点一次页面。已经可见时不每帧抢 key。
    ///
    /// 这里**绝不碰 GPUI 的内部焦点**。动了它（比如 blur），GPUI 即便重新拿回
    /// first responder 也没有 focused element 能接收输入法的上屏事件。
    pub(crate) fn sync_plugin_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_plugin_tab().map(|tab| tab.panel_id());
        let host_overlay = overlay::should_suppress_plugin_content(self, window, cx);
        let suppress_plugin = host_overlay;
        for tab in plugin_webviews() {
            let panel_id = tab.panel_id();
            if suppress_plugin || Some(&panel_id) != active.as_ref() {
                smelt_webview::set_panel_visible(&panel_id, false);
                // 面板不可见时忘掉已推上下文：下次显示要重新推一次，
                // 因为页面可能在这期间被卸载或重载过。
                LAST_CONTEXT.with(|last| last.borrow_mut().remove(&panel_id));
            }
        }
        if suppress_plugin || active.is_none() {
            // 已经是 GPUI 持有时内部会短路，每帧调用的开销可以忽略。隐藏面板时
            // 必须释放 WebView first responder，否则输入法仍归不可见的 WebView。
            smelt_webview::release_focus(window);
        }
        if !suppress_plugin {
            self.push_panel_context(cx);
        }
    }

    /// 宿主的 UI 上下文。当前项目和工作目录是 GUI 本地概念，插件进程读不到，
    /// 只能由宿主推给面板。
    ///
    /// 字段只放核心通用领域，不放任何插件特有概念（见 4.4）。这里暂不给会话
    /// 身份：`Session::ui_id` 只是运行时 UI 快照的锚点，对插件没有意义，等真
    /// 有消费者且有稳定会话 id 时再加。
    fn panel_context(&self, cx: &App) -> serde_json::Value {
        serde_json::json!({
            "project_root": self.active_project_root(cx),
            "cwd": self.cur().and_then(|session| session.cwd(cx)),
        })
    }

    /// 上下文变化时推给当前可见的面板。只推变化，避免每帧一次跨进程 post。
    fn push_panel_context(&self, cx: &App) {
        let Some(tab) = self.active_plugin_tab() else {
            return;
        };
        if !tab.has_capability(smelt_plugin_api::CORE_CAPABILITY_UI_CONTEXT_READ) {
            return;
        }
        let panel_id = tab.panel_id();
        let context = self.panel_context(cx).to_string();
        if LAST_CONTEXT.with(|last| last.borrow().get(&panel_id) == Some(&context)) {
            return;
        }
        let payload = serde_json::json!({ "kind": "context", "context": self.panel_context(cx) });
        // 页面还没加载好时 post 会失败：不记账，下一帧再试。
        if smelt_webview::post_to_panel(&panel_id, &payload.to_string()).is_ok() {
            LAST_CONTEXT.with(|last| last.borrow_mut().insert(panel_id, context));
        }
    }

    /// 处理面板页面发上来的消息。
    ///
    /// 页面能做的只有三件事：发起一次 invocation（`rid` + `payload`）、请宿主
    /// 代办一件它自己没有能力做的事（`rid` + `command`），或者发一个不需要
    /// 应答的事件。
    pub(crate) fn drain_plugin_panel_messages(&self, cx: &mut Context<Self>) {
        self.drain_panel_view_events();
        for message in smelt_webview::drain_messages() {
            let Some(tab) = plugin_tab_by_panel_id(&message.panel_id) else {
                continue;
            };
            let Ok(body) = serde_json::from_str::<serde_json::Value>(&message.body) else {
                eprintln!("[plugin-ui] {} 发来的消息不是 JSON", tab.plugin_id);
                continue;
            };
            if let Some(rid) = body.get("rid").and_then(serde_json::Value::as_str) {
                match body.get("command").and_then(serde_json::Value::as_str) {
                    Some(command) => {
                        let command = command.to_string();
                        self.run_host_command(&tab, rid, &command, &body["params"], cx);
                    }
                    None => self.forward_panel_invocation(&tab, rid, body["payload"].clone(), cx),
                }
            } else if let Some(event) = body.get("event") {
                self.handle_panel_event(&tab, event);
            }
        }
    }

    /// 执行一条宿主代办命令。
    ///
    /// 这些操作本来就发生在 GUI 本地（原生对话框要有父窗口，Finder 由前台应用
    /// 唤起），绕一圈 daemon 没有收益。门控只看 capability，不看插件身份。
    fn run_host_command(
        &self,
        tab: &PluginTab,
        rid: &str,
        command: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let panel_id = tab.panel_id();
        let Some(descriptor) = HOST_COMMANDS.iter().find(|entry| entry.id == command) else {
            reply_to_panel(
                &panel_id,
                rid,
                Err(format!("unknown host command {command}")),
            );
            return;
        };
        if !tab.has_capability(descriptor.capability) {
            eprintln!(
                "[plugin-ui] {} 未声明 {}，拒绝 {command}",
                tab.plugin_id, descriptor.capability
            );
            reply_to_panel(
                &panel_id,
                rid,
                Err(format!("{command} requires {}", descriptor.capability)),
            );
            return;
        }
        let text = |key: &str| {
            params
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match command {
            smelt_plugin_api::CORE_HOST_COMMAND_CONTEXT => {
                reply_to_panel(&panel_id, rid, Ok(self.panel_context(cx)));
            }
            smelt_plugin_api::CORE_HOST_COMMAND_REVEAL => {
                reply_to_panel(&panel_id, rid, reveal_in_file_manager(&text("path")));
            }
            smelt_plugin_api::CORE_HOST_COMMAND_OPEN_EXTERNAL => {
                reply_to_panel(&panel_id, rid, open_external(&text("url")));
            }
            smelt_plugin_api::CORE_HOST_COMMAND_PICK_DIRECTORY => {
                let prompt = params
                    .get("prompt")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                self.pick_directory_for_panel(panel_id, rid.to_string(), prompt, cx);
            }
            // HOST_COMMANDS 里有、这里没有 arm 的命令是实现漏写，不能静默。
            other => reply_to_panel(
                &panel_id,
                rid,
                Err(format!("host command {other} has no handler")),
            ),
        }
    }

    /// 原生目录选择框。它是异步的：用户可能一直不选，所以不能阻塞渲染线程。
    fn pick_directory_for_panel(
        &self,
        panel_id: String,
        rid: String,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: prompt.map(Into::into),
        });
        cx.spawn(async move |_workspace, cx| {
            // 用户取消不是错误：回一个 null 路径让页面自行收场。
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths
                    .first()
                    .map(|path| serde_json::Value::String(path.to_string_lossy().into_owned()))
                    .unwrap_or(serde_json::Value::Null),
                _ => serde_json::Value::Null,
            };
            // 回到主线程再碰 WebView：它只能在主线程上操作。
            cx.update(|_cx| {
                reply_to_panel(&panel_id, &rid, Ok(serde_json::json!({ "path": path })));
            })
        })
        .detach();
    }

    /// 把一次页面请求转成 invocation 交给守护里的插件进程。
    ///
    /// 走后台执行器：这是一次跨进程往返，放在渲染线程上会卡住整个窗口。
    fn forward_panel_invocation(
        &self,
        tab: &PluginTab,
        rid: &str,
        payload: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Ok(request) = build_invocation(tab, payload) else {
            eprintln!("[plugin-ui] {} 的 contribution id 非法", tab.plugin_id);
            return;
        };
        let plugin_id = tab.plugin_id.clone();
        let panel_id = tab.panel_id();
        let rid = rid.to_string();
        cx.spawn(async move |_workspace, cx| {
            let outcome = cx
                .background_spawn(async move { terminal::plugin_invoke(&plugin_id, &request) })
                .await;
            // 回到主线程再碰 WebView：它只能在主线程上操作。
            cx.update(|_cx| {
                let reply = match outcome {
                    Ok(result) => serde_json::json!({ "rid": rid, "result": result }),
                    Err(error) => {
                        eprintln!("[plugin-ui] invocation 失败: {error}");
                        serde_json::json!({ "rid": rid, "error": error })
                    }
                };
                if let Err(error) = smelt_webview::post_to_panel(&panel_id, &reply.to_string()) {
                    eprintln!("[plugin-ui] 回包失败: {error}");
                }
            });
        })
        .detach();
    }

    /// 把内容视图的状态变化转发给面板页面。
    ///
    /// 页面画地址栏和前进后退按钮，但真正在加载网页的是宿主管的那个 WebView，
    /// 所以 URL、标题、加载状态得由宿主推回去。
    fn drain_panel_view_events(&self) {
        for event in smelt_webview::drain_view_events() {
            let payload = serde_json::json!({
                "kind": "view.state",
                "state": event.state,
            });
            if let Err(error) = smelt_webview::post_to_panel(&event.panel_id, &payload.to_string())
            {
                eprintln!("[plugin-ui] 转发内容视图状态失败: {error}");
            }
        }
    }

    /// 宿主代办的能力。页面自己没有这些能力，也不该有。
    fn handle_panel_event(&self, tab: &PluginTab, event: &serde_json::Value) {
        let rect_of = |value: &serde_json::Value| {
            let number = |key: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or_default() as f32
            };
            smelt_webview::PanelRect {
                x: number("x"),
                y: number("y"),
                width: number("width"),
                height: number("height"),
            }
        };
        match event.get("kind").and_then(serde_json::Value::as_str) {
            // 生命周期握手由宿主消费，不转发给插件进程；这样页面切换时可以
            // 停掉自己的轮询，而不需要依赖原生 WebView 的 visibilityState。
            Some("panel.loading") => smelt_webview::panel_loading(&tab.panel_id()),
            Some("panel.ready") => smelt_webview::panel_ready(&tab.panel_id()),
            // 内容视图是"浏览器本身"，不是 iframe，因此不受 X-Frame-Options
            // 约束。但也正因如此，能不能用它必须由 capability 说了算。
            Some(kind) if kind.starts_with("view.") && !tab.allow_web_browse() => {
                eprintln!(
                    "[plugin-ui] {} 未声明 web.browse，拒绝 {kind}",
                    tab.plugin_id
                );
            }
            Some("view.navigate") => {
                let Some(url) = event.get("url").and_then(serde_json::Value::as_str) else {
                    return;
                };
                let rect = rect_of(event.get("rect").unwrap_or(&serde_json::Value::Null));
                if let Err(error) = smelt_webview::navigate_panel_view(&tab.panel_id(), url, rect) {
                    eprintln!("[plugin-ui] {error}");
                }
            }
            Some("view.bounds") => {
                let rect = rect_of(event.get("rect").unwrap_or(&serde_json::Value::Null));
                let _ = smelt_webview::set_panel_view_bounds(&tab.panel_id(), rect);
            }
            Some("view.back") => {
                let _ = smelt_webview::panel_view_command(
                    &tab.panel_id(),
                    smelt_webview::PanelViewCommand::Back,
                );
            }
            Some("view.forward") => {
                let _ = smelt_webview::panel_view_command(
                    &tab.panel_id(),
                    smelt_webview::PanelViewCommand::Forward,
                );
            }
            Some("view.reload") => {
                let _ = smelt_webview::panel_view_command(
                    &tab.panel_id(),
                    smelt_webview::PanelViewCommand::Reload,
                );
            }
            Some("view.hide") => {
                let _ = smelt_webview::panel_view_command(
                    &tab.panel_id(),
                    smelt_webview::PanelViewCommand::Hide,
                );
            }
            _ => {
                eprintln!(
                    "[plugin-ui] {} 发来未知事件 {}",
                    tab.plugin_id,
                    event
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("(缺 kind)")
                );
            }
        }
    }

    pub(crate) fn render_workspace_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(key) = self.plugin_surface_key().map(str::to_string) else {
            return missing_panel("未选择工作台");
        };
        let Some(tab) = workspace_surface_by_key(&key) else {
            return missing_panel("这个插件已经不在了");
        };
        self.render_plugin_webview(tab, window, cx)
    }

    /// 渲染插件面板：GPUI 只占位并同步矩形，内容全部由插件的网页负责。
    pub(crate) fn render_plugin_tab(
        &mut self,
        slot: PluginTabSlot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(tab) = tab(slot) else {
            return missing_panel("这个插件已经不在了");
        };
        self.render_plugin_webview(tab, window, cx)
    }

    fn render_plugin_webview(
        &mut self,
        tab: PluginTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let suppress = overlay::should_suppress_plugin_content(self, window, cx);
        let spec = panel_spec(&tab);
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .bg(rgb(ui_theme::bg_stage()))
            .child(
                // WebView 加载有可见延迟，而且页面背景是透明的，需要这层垫底。
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(rgb(ui_theme::text_faint()))
                    .child(format!("{} 加载中…", tab.title)),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _cx| {
                        let rect = smelt_webview::PanelRect {
                            x: f32::from(bounds.origin.x),
                            y: f32::from(bounds.origin.y),
                            width: f32::from(bounds.size.width),
                            height: f32::from(bounds.size.height),
                        };
                        if suppress {
                            // The canvas is painted after Workspace::render's sync pass;
                            // suppress creation/visibility here as well so a newly opened
                            // modal cannot briefly expose a fresh WebView child.
                            smelt_webview::set_panel_visible(&spec.id, false);
                        } else if let Err(error) = smelt_webview::sync_panel(window, &spec, rect) {
                            eprintln!("[plugin-ui] {error}");
                        }
                        // 点在面板之外 → 把 first responder 收回 GPUI。
                        //
                        // 点在面板之内什么都不做：WKWebView 是标准 NSView，
                        // AppKit 自己会让它成为 first responder。这里绝不能去动
                        // GPUI 的内部焦点（比如 blur），否则 GPUI 即便重新拿回
                        // first responder 也没有 focused element 能接收上屏。
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, _cx| {
                            if phase.bubble() && !bounds.contains(&event.position) {
                                smelt_webview::release_focus(window);
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }
}

fn build_invocation(
    tab: &PluginTab,
    payload: serde_json::Value,
) -> Result<smelt_plugin_api::InvocationRequest, smelt_plugin_api::ValidationError> {
    Ok(smelt_plugin_api::InvocationRequest {
        invocation_id: smelt_plugin_api::InvocationId::new(
            uuid::Uuid::new_v4().simple().to_string(),
        )?,
        contribution_id: smelt_plugin_api::ContributionId::new(tab.contribution_id.clone())?,
        // 页面不能自选 operation，见 CORE_INVOCATION_PANEL_MESSAGE 的说明。
        operation: smelt_plugin_api::InvocationOperation::new(
            smelt_plugin_api::CORE_INVOCATION_PANEL_MESSAGE,
        )?,
        payload,
        deadline_ms: now_ms() + INVOKE_TIMEOUT_MS,
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn missing_panel(reason: &str) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .text_color(rgb(ui_theme::text_faint()))
        .child(reason.to_string())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // 不能 `use super::*`：本模块顶部有 `use gpui::*`，它带进来的 `gpui::test`
    // 会顶掉内置的 `#[test]`，展开时直接撞递归上限。
    use super::{
        AGENT_REGISTRY, PluginAgentPresentation, PluginCommand, PluginSessionAction, PluginTab,
        REGISTRY, SESSION_ACTION_REGISTRY, SURFACE_REGISTRY, agent_presentation,
        bounded_plugin_ui_error, discover_package_agents, discover_package_entity_decorations,
        discover_package_presentations, discover_package_session_actions, key_for,
        plugin_tab_by_panel_id, plugin_webviews, session_action_outcome, session_actions,
        settings_view_commands_valid, slot_for_key,
    };
    use std::path::{Path, PathBuf};

    fn fixture_root(label: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("target/plugin-ui-test-fixtures")
            .join(format!("{label}-{}", uuid::Uuid::new_v4().simple()))
    }

    fn tab(plugin_id: &str, contribution_id: &str) -> PluginTab {
        PluginTab {
            plugin_id: plugin_id.into(),
            contribution_id: contribution_id.into(),
            title: "面板".into(),
            asset_root: PathBuf::from("/tmp"),
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn panel_id_is_a_legal_protocol_host() {
        let panel = tab("com.smelt.browser", "browser").panel_id();
        assert_eq!(panel, "com.smelt.browser--browser");
        assert!(
            panel
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        );
    }

    #[test]
    fn ids_that_would_break_the_url_are_sanitized() {
        // contribution id 已经过 manifest 校验，但 panel id 会进 URL 的 host
        // 段，这里再兜一次而不是相信上游。
        let panel = tab("a/b", "c d").panel_id();
        assert!(!panel.contains('/') && !panel.contains(' '));
    }

    #[test]
    fn keys_round_trip_through_the_registry() {
        REGISTRY.with(|registry| {
            *registry.borrow_mut() = vec![tab("com.a", "one"), tab("com.b", "two")];
        });
        let slot = slot_for_key("com.b/two").expect("已注册的 key 必须能查到");
        assert_eq!(key_for(slot).as_deref(), Some("com.b/two"));
        // 插件不在了：查不到槽位，调用方据此降级。
        assert!(slot_for_key("com.missing/gone").is_none());
        REGISTRY.with(|registry| registry.borrow_mut().clear());
    }

    #[test]
    fn workspace_surface_webviews_share_the_host_message_path() {
        SURFACE_REGISTRY.with(|registry| {
            *registry.borrow_mut() = vec![tab("com.example.board", "board")];
        });
        let panel_id = "com.example.board--board";
        let found = plugin_tab_by_panel_id(panel_id).expect("surface 必须能按 panel id 找到");
        assert_eq!(found.plugin_id, "com.example.board");
        assert!(
            plugin_webviews()
                .iter()
                .any(|tab| tab.panel_id() == panel_id)
        );
        SURFACE_REGISTRY.with(|registry| registry.borrow_mut().clear());
    }

    #[test]
    fn agent_lookup_requires_the_declared_controller_not_just_a_matching_name() {
        AGENT_REGISTRY.with(|registry| {
            *registry.borrow_mut() = vec![PluginAgentPresentation {
                plugin_id: "com.example.quant".into(),
                contribution_id: "quant-agent".into(),
                name: "量化智能体".into(),
                controller_id: "quant-session".into(),
                icon_asset: Some("smelt-plugin-agent/example.svg".into()),
            }];
        });
        let binding: smelt_plugin_api::AgentSessionBinding =
            serde_json::from_value(serde_json::json!({
                "agent": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-agent"
                },
                "controller": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-session"
                },
                "instance": {
                    "plugin_id": "com.example.quant",
                    "resource_type": "strategy",
                    "resource_id": "strategy-1"
                }
            }))
            .unwrap();

        assert_eq!(agent_presentation(&binding).unwrap().name, "量化智能体");
        let mut mismatched = binding;
        mismatched.controller.contribution_id =
            smelt_plugin_api::ContributionId::new("other-controller").unwrap();
        assert!(agent_presentation(&mismatched).is_none());
        AGENT_REGISTRY.with(|registry| registry.borrow_mut().clear());
    }

    #[test]
    fn agent_icon_is_loaded_from_its_package_and_gets_a_content_addressed_asset() {
        let root = fixture_root("agent-ui");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("bin/main.ts"), b"export default {};\n").unwrap();
        std::fs::write(
            root.join("assets/quant.svg"),
            br#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0h1v1z"/></svg>"#,
        )
        .unwrap();
        std::fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example.quant",
                "name": "Quant",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/main.ts",
                "capabilities": ["agent.contribute", "session.input.route", "ui.contribute"],
                "contributions": []
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("plugin-input.json"),
            serde_json::json!({
                "contributions": [{
                    "type": "input_route",
                    "id": "quant-input",
                    "operation": "submit_input"
                }]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("plugin-agent.json"),
            serde_json::json!({
                "contributions": [
                    {
                        "type": "session_controller",
                        "id": "quant-session",
                        "input_route": "quant-input"
                    },
                    {
                        "type": "agent",
                        "id": "quant-agent",
                        "name": "量化智能体",
                        "icon": "assets/quant.svg",
                        "controller": "quant-session"
                    },
                    {
                        "type": "session_action",
                        "id": "open-strategy",
                        "title": "打开策略详情",
                        "operation": "open_strategy",
                        "controller": "quant-session",
                        "locations": ["session_menu"],
                        "result": "open_external"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();
        let package = smelt_plugin_host::PluginPackage::load(&root).unwrap();

        let (agents, assets) = discover_package_agents(&package);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "量化智能体");
        let asset = agents[0]
            .icon_asset
            .as_ref()
            .expect("包内 SVG 应注册为宿主图标资源");
        assert!(asset.starts_with("smelt-plugin-agent/com.example.quant/quant-agent/"));
        assert_eq!(
            assets.get(asset).unwrap(),
            &std::fs::read(root.join("assets/quant.svg")).unwrap()
        );
        let actions = discover_package_session_actions(&package);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].controller_id, "quant-session");
        assert_eq!(
            actions[0].result,
            smelt_plugin_api::SessionActionResult::OpenExternal
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn declarative_presentations_are_discovered_without_plugin_specific_code() {
        let root = fixture_root("settings-ui");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/main.ts"), b"export default {};\n").unwrap();
        std::fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example.account",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/main.ts",
                "capabilities": ["ui.contribute"],
                "contributions": [
                    {
                        "type": "command",
                        "id": "settings-snapshot",
                        "title": "Read settings",
                        "operation": "get_settings_view"
                    },
                    {
                        "type": "command",
                        "id": "login",
                        "title": "Log in",
                        "operation": "login"
                    },
                    {
                        "type": "command",
                        "id": "decoration-snapshot",
                        "title": "Read decorations",
                        "operation": "get_decorations"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("plugin-ui.json"),
            serde_json::json!({
                "contributions": [
                    {
                        "type": "sidebar_account",
                        "id": "account-slot",
                        "settings_section": "settings",
                        "account_item": "account"
                    },
                    {
                        "type": "settings_section",
                        "id": "settings",
                        "title": "Example",
                        "snapshot": "settings-snapshot"
                    },
                    {
                        "type": "entity_decoration",
                        "id": "issue-decoration",
                        "resource_type": "issue",
                        "snapshot": "decoration-snapshot"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();
        let package = smelt_plugin_host::PluginPackage::load(&root).unwrap();

        let (sections, accounts, commands) = discover_package_presentations(&package);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].snapshot.operation, "get_settings_view");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].settings_section_id, "settings");
        assert_eq!(commands.len(), 3);
        let (decorations, assets) = discover_package_entity_decorations(&package, &commands);
        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].resource_type, "issue");
        assert_eq!(decorations[0].snapshot.operation, "get_decorations");
        assert!(assets.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_ui_errors_are_bounded_and_single_line() {
        let error = bounded_plugin_ui_error(&format!("failure\n{}", "x".repeat(4096)));
        assert!(!error.contains('\n'));
        assert!(error.len() <= 1024);
    }

    #[test]
    fn dynamic_settings_actions_cannot_invent_plugin_operations() {
        let view: smelt_plugin_api::SettingsSectionView =
            serde_json::from_value(serde_json::json!({
                "items": [{
                    "type": "actions",
                    "id": "commands",
                    "actions": [{
                        "id": "invented",
                        "label": "Run",
                        "command": "not-declared"
                    }]
                }]
            }))
            .unwrap();
        let commands = [(
            "declared".to_string(),
            PluginCommand {
                plugin_id: "com.example".into(),
                contribution_id: "declared".into(),
                operation: "safe_operation".into(),
            },
        )]
        .into_iter()
        .collect();

        assert!(settings_view_commands_valid("com.example", &view, &commands).is_err());
    }

    #[test]
    fn session_actions_are_selected_by_controller_and_location() {
        use smelt_plugin_api::{SessionActionIcon, SessionActionLocation, SessionActionResult};

        AGENT_REGISTRY.with(|registry| {
            *registry.borrow_mut() = vec![PluginAgentPresentation {
                plugin_id: "com.example.quant".into(),
                contribution_id: "quant-agent".into(),
                name: "量化智能体".into(),
                controller_id: "quant-session".into(),
                icon_asset: None,
            }];
        });
        SESSION_ACTION_REGISTRY.with(|registry| {
            *registry.borrow_mut() = vec![PluginSessionAction {
                plugin_id: "com.example.quant".into(),
                contribution_id: "open-strategy".into(),
                title: "打开策略详情".into(),
                operation: "open_strategy".into(),
                controller_id: "quant-session".into(),
                locations: vec![
                    SessionActionLocation::SessionMenu,
                    SessionActionLocation::ProjectMenu,
                ],
                icon: Some(SessionActionIcon::ExternalLink),
                result: SessionActionResult::OpenExternal,
            }];
        });
        let binding: smelt_plugin_api::AgentSessionBinding =
            serde_json::from_value(serde_json::json!({
                "agent": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-agent"
                },
                "controller": {
                    "plugin_id": "com.example.quant",
                    "contribution_id": "quant-session"
                },
                "instance": {
                    "plugin_id": "com.example.quant",
                    "resource_type": "strategy",
                    "resource_id": "strategy-1"
                }
            }))
            .unwrap();

        let actions = session_actions(&binding, SessionActionLocation::ProjectMenu);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "打开策略详情");
        let mut mismatched = binding.clone();
        mismatched.controller.contribution_id =
            smelt_plugin_api::ContributionId::new("other-controller").unwrap();
        assert!(session_actions(&mismatched, SessionActionLocation::SessionMenu).is_empty());
        let mut forged_agent = binding;
        forged_agent.agent.contribution_id =
            smelt_plugin_api::ContributionId::new("other-agent").unwrap();
        assert!(session_actions(&forged_agent, SessionActionLocation::SessionMenu).is_empty());
        AGENT_REGISTRY.with(|registry| registry.borrow_mut().clear());
        SESSION_ACTION_REGISTRY.with(|registry| registry.borrow_mut().clear());
    }

    #[test]
    fn open_external_session_action_rejects_non_http_results() {
        use smelt_plugin_api::SessionActionResult;

        assert!(
            session_action_outcome(
                SessionActionResult::OpenExternal,
                serde_json::json!({"url": "file:///tmp/secret"}),
            )
            .is_err()
        );
        assert!(
            session_action_outcome(
                SessionActionResult::OpenExternal,
                serde_json::json!({"url": "https://example.com/strategy/1"}),
            )
            .is_ok()
        );
    }
}
