//! Agent hook 安装、卸载与状态探测。
//!
//! 这里处理各家 Agent 的配置文件和 `smelt-notify` managed helper；设置页只消费
//! 这些操作的结果，不把 JSON 迁移细节和 GPUI 渲染混在一起。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookProviderStatus {
    pub label: &'static str,
    pub installed: bool,
}

type HookStatus = Vec<HookProviderStatus>;
type CachedHookStatus = (Instant, HookStatus);
type HookOperation = fn() -> Result<(), String>;

struct HookIntegration {
    label: &'static str,
    installed: fn() -> bool,
    install: HookOperation,
    uninstall: HookOperation,
}

/// smelt-notify 安装路径（与 package/安装脚本约定一致）。
pub fn smelt_notify_path() -> std::path::PathBuf {
    smelt_core::agent_event::notify_executable_path()
}

/// cross-agent MCP helper 的稳定安装路径。managed smeltd 与它同目录，ACP/PTY
/// 注入都可从守护可执行文件旁直接定位，不依赖用户 PATH。
pub fn smelt_agent_mcp_path() -> std::path::PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| "/tmp/.smelt".into())
        .join("bin")
        .join("smelt-agent-mcp")
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

/// 把 App / cargo 产物旁的 cross-agent MCP helper 原子同步到 managed 目录。
pub fn sync_bundled_smelt_agent_mcp() -> std::io::Result<()> {
    let bundled = std::env::current_exe()?.with_file_name("smelt-agent-mcp");
    if !bundled.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("App 包内缺少 {}", bundled.display()),
        ));
    }

    let managed = smelt_agent_mcp_path();
    let dir = managed.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("无效的 smelt-agent-mcp 路径：{}", managed.display()),
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let staged = dir.join("smelt-agent-mcp.next");
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

fn grok_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| {
        h.join(".grok")
            .join("hooks")
            .join("smelt-notifications.json")
    })
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
    "StopCancelled",
    "TeammateIdle",
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
    "SessionStop",
    "Notification",
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

/// Antigravity 的 PreToolUse hook 必须返回权限 decision，会改变用户原有审批策略，
/// 因此状态集成只订阅不会参与裁决的事件。PostToolUse 仍携带工具名、stepIdx 和错误。
pub(crate) const ANTIGRAVITY_HOOK_EVENTS: &[&str] =
    &["PreInvocation", "PostInvocation", "PostToolUse", "Stop"];
pub(crate) const ANTIGRAVITY_HOOK_NAME: &str = "smelt-status-notifications";

const CURSOR_HOOK_EVENTS: &[&str] = &[
    "sessionStart",
    "sessionEnd",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
    "subagentStart",
    "subagentStop",
    "beforeSubmitPrompt",
    "stop",
];

// Kiro 的全局 hooks 只由 CLI v3 / IDE 1.0+ 读取。v2 没有不改变默认 agent
// 行为的全局注册点，因此设置页会明确标成 Kiro v3，避免“已安装但当前 CLI 不触发”。
const KIRO_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];

const OPENCODE_PLUGIN_MARKER: &str = "// Managed by Smelt agent notifications v1.";

fn copilot_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".copilot").join("hooks").join("smelt.json"))
}

fn codex_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("hooks.json"))
}

fn antigravity_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".gemini").join("config").join("hooks.json"))
}

fn cursor_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".cursor").join("hooks.json"))
}

fn kiro_hooks_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| {
        h.join(".kiro")
            .join("hooks")
            .join("smelt-notifications.json")
    })
}

fn opencode_plugin_path() -> Option<std::path::PathBuf> {
    let config_root = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))?;
    Some(
        config_root
            .join("opencode")
            .join("plugins")
            .join("smelt-notifications.js"),
    )
}

fn provider_hook_command(provider: &str, event: &str) -> String {
    format!(
        "SMELT_HOOK_PROVIDER={provider} SMELT_HOOK_EVENT={event} {}",
        shell_words::quote(&smelt_notify_path().to_string_lossy())
    )
}

pub(crate) fn command_uses_smelt_notify(command: &str) -> bool {
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

/// hooks 接入状态缓存。设置页每次重绘都会检查所有已注册的 hooks 配置文件
///（read_to_string + JSON 解析），ES 慢 open() 时会把 render 拖住；安装/卸载后主动失效，
/// 其余时间 5s 内复用缓存结果（外部手改配置最多延迟 5s 反映）。
static HOOKS_INSTALLED_CACHE: OnceLock<Mutex<Option<CachedHookStatus>>> = OnceLock::new();

fn current_hooks_status() -> HookStatus {
    HOOK_INTEGRATIONS
        .iter()
        .map(|integration| HookProviderStatus {
            label: integration.label,
            installed: (integration.installed)(),
        })
        .collect()
}

/// 返回所有已注册 provider 的 hooks 接入状态，带 5s 缓存。
pub fn hooks_installed_status() -> HookStatus {
    let cache = HOOKS_INSTALLED_CACHE.get_or_init(|| Mutex::new(None));
    let Ok(mut guard) = cache.lock() else {
        // 锁被占用（极端）：直接实时查，不阻塞 render。
        return current_hooks_status();
    };
    if let Some((at, status)) = guard.as_ref()
        && at.elapsed() < Duration::from_secs(5)
    {
        return status.clone();
    }
    let status = current_hooks_status();
    *guard = Some((Instant::now(), status.clone()));
    status
}

/// 安装/卸载 hooks 后调用，清缓存强制下次重扫。
pub fn invalidate_hooks_cache() {
    if let Some(cache) = HOOKS_INSTALLED_CACHE.get()
        && let Ok(mut guard) = cache.lock()
    {
        *guard = None;
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

pub(crate) fn write_json_atomic(
    path: &std::path::Path,
    root: &serde_json::Value,
) -> Result<(), String> {
    let out = serde_json::to_string_pretty(root).map_err(|e| e.to_string())? + "\n";
    write_text_atomic(path, &out)
}

fn write_text_atomic(path: &std::path::Path, out: &str) -> Result<(), String> {
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

pub(crate) fn uninstall_hook_file(
    path: Option<std::path::PathBuf>,
    events: &[&str],
) -> Result<(), String> {
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

pub(crate) fn antigravity_event_installed(definition: &serde_json::Value, event: &str) -> bool {
    let Some(handlers) = definition.get(event).and_then(|value| value.as_array()) else {
        return false;
    };
    if event == "PostToolUse" {
        handlers.iter().any(|group| {
            group
                .get("hooks")
                .and_then(|value| value.as_array())
                .is_some_and(|nested| {
                    nested.iter().any(|handler| {
                        handler
                            .get("command")
                            .and_then(|value| value.as_str())
                            .is_some_and(|command| command_uses_current_smelt_hook(command, event))
                    })
                })
        })
    } else {
        handlers.iter().any(|handler| {
            handler
                .get("command")
                .and_then(|value| value.as_str())
                .is_some_and(|command| command_uses_current_smelt_hook(command, event))
        })
    }
}

pub fn antigravity_hooks_installed() -> bool {
    if !smelt_notify_available() {
        return false;
    }
    let Some(path) = antigravity_hooks_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(definition) = root.get(ANTIGRAVITY_HOOK_NAME) else {
        return false;
    };
    definition.get("enabled").and_then(|value| value.as_bool()) != Some(false)
        && ANTIGRAVITY_HOOK_EVENTS
            .iter()
            .all(|event| antigravity_event_installed(definition, event))
}

fn remove_smelt_antigravity_handlers(definition: &mut serde_json::Map<String, serde_json::Value>) {
    for event in ANTIGRAVITY_HOOK_EVENTS {
        let Some(handlers) = definition
            .get_mut(*event)
            .and_then(|value| value.as_array_mut())
        else {
            continue;
        };
        if *event == "PostToolUse" {
            handlers.retain_mut(|group| {
                let Some(nested) = group
                    .get_mut("hooks")
                    .and_then(|value| value.as_array_mut())
                else {
                    return true;
                };
                nested.retain(|handler| {
                    !handler
                        .get("command")
                        .and_then(|value| value.as_str())
                        .is_some_and(command_uses_smelt_notify)
                });
                !nested.is_empty()
            });
        } else {
            handlers.retain(|handler| {
                !handler
                    .get("command")
                    .and_then(|value| value.as_str())
                    .is_some_and(command_uses_smelt_notify)
            });
        }
        if handlers.is_empty() {
            definition.remove(*event);
        }
    }
}

pub(crate) fn merge_antigravity_hooks(root: &mut serde_json::Value) -> Result<(), String> {
    let root = root
        .as_object_mut()
        .ok_or_else(|| "Antigravity hooks 文件根不是对象".to_string())?;
    let definition = root
        .entry(ANTIGRAVITY_HOOK_NAME)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("Antigravity hook {ANTIGRAVITY_HOOK_NAME} 不是对象"))?;
    remove_smelt_antigravity_handlers(definition);
    definition.insert("enabled".into(), serde_json::json!(true));

    for event in ANTIGRAVITY_HOOK_EVENTS {
        let command = provider_hook_command("antigravity", event);
        let handler = serde_json::json!({
            "type": "command",
            "command": command,
            "timeout": 3,
        });
        let handlers = definition
            .entry(*event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("{ANTIGRAVITY_HOOK_NAME}.{event} 不是数组"))?;
        if *event == "PostToolUse" {
            handlers.push(serde_json::json!({
                "matcher": "",
                "hooks": [handler],
            }));
        } else {
            handlers.push(handler);
        }
    }
    Ok(())
}

fn install_antigravity_hooks_at(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut root = if path.is_file() {
        let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        serde_json::from_str(&raw)
            .map_err(|error| format!("{} 不是有效 JSON：{error}", path.display()))?
    } else {
        serde_json::json!({})
    };
    merge_antigravity_hooks(&mut root)?;
    write_json_atomic(path, &root)
}

pub fn install_antigravity_hooks() -> Result<(), String> {
    let notify = smelt_notify_path();
    if !notify.is_file() {
        return Err(format!(
            "找不到 {}，请先编译安装 smelt-notify",
            notify.display()
        ));
    }
    let path = antigravity_hooks_path().ok_or_else(|| "无 home 目录".to_string())?;
    install_antigravity_hooks_at(&path)
}

pub(crate) fn uninstall_antigravity_hooks_at(path: &std::path::Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut root: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("{} 不是有效 JSON：{error}", path.display()))?;
    let Some(root_object) = root.as_object_mut() else {
        return Ok(());
    };
    let remove_definition = root_object
        .get_mut(ANTIGRAVITY_HOOK_NAME)
        .and_then(|value| value.as_object_mut())
        .is_some_and(|definition| {
            remove_smelt_antigravity_handlers(definition);
            definition.keys().all(|key| key == "enabled")
        });
    if remove_definition {
        root_object.remove(ANTIGRAVITY_HOOK_NAME);
    }
    write_json_atomic(path, &root)
}

pub fn uninstall_antigravity_hooks() -> Result<(), String> {
    let Some(path) = antigravity_hooks_path() else {
        return Ok(());
    };
    uninstall_antigravity_hooks_at(&path)
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
        && hook_file_installed(grok_hooks_path(), SMELT_HOOK_EVENTS)
}

/// 把 smelt-notify 写入 Claude settings 和 Grok 自己的 hooks 目录（幂等）。
///
/// Grok TUI 读 `~/.grok/hooks/` 与 config 层，不只读 `~/.claude/settings.json`。
/// 只写 Claude 时，Grok 会话能靠 OSC 改标题，但 Stop/idle 进不了 smeltd，
/// 侧栏会停在最后一次 PostToolUse 的运行蓝。
pub fn install_claude_hooks() -> Result<(), String> {
    let claude = claude_settings_path().ok_or_else(|| "无 home 目录".to_string())?;
    let grok = grok_hooks_path().ok_or_else(|| "无 home 目录".to_string())?;
    install_hook_file(claude, SMELT_HOOK_EVENTS, "claude", false)?;
    install_hook_file(grok, SMELT_HOOK_EVENTS, "grok", false)
}

/// 从 Claude settings 和 Grok hooks 目录移除 smelt-notify（其它 hook 保留）。
pub fn uninstall_claude_hooks() -> Result<(), String> {
    uninstall_hook_file(claude_settings_path(), SMELT_HOOK_EVENTS)?;
    uninstall_hook_file(grok_hooks_path(), SMELT_HOOK_EVENTS)
}

fn read_json_or_empty(path: &std::path::Path, provider: &str) -> Result<serde_json::Value, String> {
    if !path.is_file() {
        return Ok(serde_json::json!({}));
    }
    let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&raw).map_err(|error| {
        format!(
            "{} 的 {provider} 配置不是有效 JSON：{error}",
            path.display()
        )
    })
}

fn cursor_event_installed(root: &serde_json::Value, event: &str) -> bool {
    root.pointer(&format!("/hooks/{event}"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|handlers| {
            handlers.iter().any(|handler| {
                handler
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|command| command_uses_current_smelt_hook(command, event))
            })
        })
}

fn remove_cursor_hooks(root: &mut serde_json::Value) {
    let Some(hooks) = root
        .get_mut("hooks")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let event_names = hooks.keys().cloned().collect::<Vec<_>>();
    for event in event_names {
        let Some(handlers) = hooks
            .get_mut(&event)
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        handlers.retain(|handler| {
            !handler
                .get("command")
                .and_then(serde_json::Value::as_str)
                .is_some_and(command_uses_smelt_notify)
        });
        if handlers.is_empty() {
            hooks.remove(&event);
        }
    }
}

fn merge_cursor_hooks(root: &mut serde_json::Value) -> Result<(), String> {
    remove_cursor_hooks(root);
    let root = root
        .as_object_mut()
        .ok_or_else(|| "Cursor hooks 文件根不是对象".to_string())?;
    root.insert("version".into(), serde_json::json!(1));
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| "Cursor hooks 不是对象".to_string())?;
    for event in CURSOR_HOOK_EVENTS {
        let handlers = hooks
            .entry(*event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("Cursor hooks.{event} 不是数组"))?;
        handlers.push(serde_json::json!({
            "command": provider_hook_command("cursor", event),
            "timeout": 3,
        }));
    }
    Ok(())
}

fn cursor_hooks_installed() -> bool {
    if !smelt_notify_available() {
        return false;
    }
    let Some(path) = cursor_hooks_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    CURSOR_HOOK_EVENTS
        .iter()
        .all(|event| cursor_event_installed(&root, event))
}

fn install_cursor_hooks() -> Result<(), String> {
    let notify = smelt_notify_path();
    if !notify.is_file() {
        return Err(format!(
            "找不到 {}，请先编译安装 smelt-notify",
            notify.display()
        ));
    }
    let path = cursor_hooks_path().ok_or_else(|| "无 home 目录".to_string())?;
    let mut root = read_json_or_empty(&path, "Cursor")?;
    merge_cursor_hooks(&mut root)?;
    write_json_atomic(&path, &root)
}

fn uninstall_cursor_hooks() -> Result<(), String> {
    let Some(path) = cursor_hooks_path() else {
        return Ok(());
    };
    if !path.is_file() {
        return Ok(());
    }
    let mut root = read_json_or_empty(&path, "Cursor")?;
    remove_cursor_hooks(&mut root);
    write_json_atomic(&path, &root)
}

fn kiro_event_installed(root: &serde_json::Value, event: &str) -> bool {
    root.get("hooks")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("trigger").and_then(serde_json::Value::as_str) == Some(event)
                    && hook.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
                    && hook
                        .pointer("/action/command")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|command| command_uses_current_smelt_hook(command, event))
            })
        })
}

fn remove_kiro_hooks(root: &mut serde_json::Value) {
    let Some(hooks) = root
        .get_mut("hooks")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    hooks.retain(|hook| {
        !hook
            .pointer("/action/command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(command_uses_smelt_notify)
    });
}

fn merge_kiro_hooks(root: &mut serde_json::Value) -> Result<(), String> {
    remove_kiro_hooks(root);
    let root = root
        .as_object_mut()
        .ok_or_else(|| "Kiro hooks 文件根不是对象".to_string())?;
    root.insert("version".into(), serde_json::json!("v1"));
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| "Kiro hooks 不是数组".to_string())?;
    for event in KIRO_HOOK_EVENTS {
        hooks.push(serde_json::json!({
            "name": format!("Smelt status · {event}"),
            "description": "向 Smelt 上报 agent 状态；不参与工具权限决策",
            "trigger": event,
            "action": {
                "type": "command",
                "command": provider_hook_command("kiro", event),
            },
            "timeout": 3,
            "enabled": true,
        }));
    }
    Ok(())
}

fn kiro_hooks_installed() -> bool {
    if !smelt_notify_available() {
        return false;
    }
    let Some(path) = kiro_hooks_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    KIRO_HOOK_EVENTS
        .iter()
        .all(|event| kiro_event_installed(&root, event))
}

fn install_kiro_hooks() -> Result<(), String> {
    let notify = smelt_notify_path();
    if !notify.is_file() {
        return Err(format!(
            "找不到 {}，请先编译安装 smelt-notify",
            notify.display()
        ));
    }
    let path = kiro_hooks_path().ok_or_else(|| "无 home 目录".to_string())?;
    let mut root = read_json_or_empty(&path, "Kiro")?;
    merge_kiro_hooks(&mut root)?;
    write_json_atomic(&path, &root)
}

fn uninstall_kiro_hooks() -> Result<(), String> {
    let Some(path) = kiro_hooks_path() else {
        return Ok(());
    };
    if !path.is_file() {
        return Ok(());
    }
    let mut root = read_json_or_empty(&path, "Kiro")?;
    remove_kiro_hooks(&mut root);
    write_json_atomic(&path, &root)
}

fn opencode_plugin_source(notify_path: &std::path::Path) -> String {
    let quoted_notify = serde_json::to_string(notify_path.to_string_lossy().as_ref())
        .expect("序列化本地路径不会失败");
    let template = r#"__SMELT_MARKER__
const SMELT_NOTIFY = __SMELT_NOTIFY__;

const phaseBySession = new Map();
const parentBySession = new Map();
const titleBySession = new Map();
const activeSubagents = new Set();
let rootSessionID;
let rootIdlePending = false;
let forwardedRootTitle;

const sessionIDFrom = (value) =>
  value?.sessionID ??
  value?.sessionId ??
  value?.session_id ??
  value?.info?.id ??
  value?.session?.id;

const parentIDFrom = (value) =>
  value?.info?.parentID ??
  value?.session?.parentID ??
  value?.parentID ??
  value?.parentId ??
  value?.parent_id;

const sessionTitleFrom = (properties) => {
  const title = properties.info?.title;
  return typeof title === "string" && title.trim() ? title.trim() : undefined;
};

const belongsToRoot = (sessionID) => {
  if (!sessionID || !rootSessionID) return false;
  const visited = new Set();
  let current = sessionID;
  while (current && !visited.has(current)) {
    if (current === rootSessionID) return true;
    visited.add(current);
    current = parentBySession.get(current);
  }
  return false;
};

const isSubagentSession = (sessionID) =>
  sessionID !== rootSessionID && belongsToRoot(sessionID);

const forward = async (hookEvent, payload = {}) => {
  if (!process.env.SMELT_SESSION_ID || !process.env.SMELT_SOCK) return;
  try {
    const child = Bun.spawn([SMELT_NOTIFY], {
      env: {
        ...process.env,
        SMELT_HOOK_PROVIDER: "opencode",
        SMELT_HOOK_EVENT: hookEvent,
      },
      stdin: "pipe",
      stdout: "ignore",
      stderr: "ignore",
    });
    child.stdin.write(JSON.stringify({ ...payload, hook_event_name: hookEvent }));
    child.stdin.end();
    await child.exited;
  } catch {
    // 状态上报必须 fail-open，不能打断 OpenCode 的 agent loop。
  }
};

const transition = async (sessionID, phase, hookEvent, payload = {}) => {
  const key = sessionID ?? "smelt-session";
  if (phaseBySession.get(key) === phase) return;
  phaseBySession.set(key, phase);
  await forward(hookEvent, { ...payload, session_id: sessionID });
};

const topKnownAncestor = (sessionID) => {
  const visited = new Set();
  let current = sessionID;
  while (parentBySession.has(current) && !visited.has(current)) {
    visited.add(current);
    current = parentBySession.get(current);
  }
  return current;
};

const forwardRootTitle = async (sessionID) => {
  if (!sessionID || sessionID !== rootSessionID) return;
  const title = titleBySession.get(sessionID);
  if (!title || title === forwardedRootTitle) return;
  await forward("SessionTitleChanged", { session_id: sessionID, title });
  forwardedRootTitle = title;
};

const adoptRootSession = async (sessionID) => {
  if (rootSessionID || !sessionID) return;
  rootSessionID = topKnownAncestor(sessionID);
  await transition(rootSessionID, "started", "SessionStart");
  await forwardRootTitle(rootSessionID);
};

const subagentNameFrom = (properties) =>
  properties.info?.title ?? properties.info?.agent ?? properties.agent;

const flushRootStop = async () => {
  if (!rootIdlePending || activeSubagents.size !== 0 || !rootSessionID) return;
  rootIdlePending = false;
  await transition(rootSessionID, "idle", "Stop");
};

const startSubagent = async (sessionID, properties = {}) => {
  if (!isSubagentSession(sessionID) || activeSubagents.has(sessionID)) return;
  activeSubagents.add(sessionID);
  phaseBySession.set(sessionID, "subagent-active");
  await forward("SubagentStart", {
    session_id: sessionID,
    agent_type: subagentNameFrom(properties),
  });
};

const stopSubagent = async (sessionID) => {
  if (!activeSubagents.delete(sessionID)) return;
  phaseBySession.set(sessionID, "idle");
  await forward("SubagentStop", { session_id: sessionID });
  await flushRootStop();
};

const stopRootWhenChildrenFinish = async () => {
  if (activeSubagents.size === 0) {
    rootIdlePending = false;
    await transition(rootSessionID, "idle", "Stop");
    return;
  }
  rootIdlePending = true;
  phaseBySession.set(rootSessionID, "idle-pending");
};

const errorMessage = (error) => {
  const message = error?.data?.message ?? error?.message ?? error;
  return typeof message === "string" ? message : "OpenCode session error";
};

export const SmeltNotifications = async () => ({
  "tool.execute.before": async (input, output) => {
    await adoptRootSession(input.sessionID);
    if (!belongsToRoot(input.sessionID)) return;
    await startSubagent(input.sessionID);
    await forward("PreToolUse", {
      session_id: input.sessionID,
      tool_name: input.tool,
      tool_input: output.args,
      tool_use_id: input.callID,
    });
  },

  "tool.execute.after": async (input) => {
    await adoptRootSession(input.sessionID);
    if (!belongsToRoot(input.sessionID)) return;
    await forward("PostToolUse", {
      session_id: input.sessionID,
      tool_name: input.tool,
      tool_use_id: input.callID,
    });
  },

  event: async ({ event }) => {
    const properties = event.properties ?? {};
    const sessionID = sessionIDFrom(properties);
    const sessionTitle = sessionTitleFrom(properties);
    if (sessionID && sessionTitle) titleBySession.set(sessionID, sessionTitle);

    if (event.type === "session.created") {
      const parentID = parentIDFrom(properties);
      if (sessionID && parentID) parentBySession.set(sessionID, parentID);

      if (!rootSessionID && sessionID && !parentID) {
        await adoptRootSession(sessionID);
      } else if (isSubagentSession(sessionID)) {
        await startSubagent(sessionID, properties);
      }
      return;
    }

    const status = properties.status?.type ?? properties.status;
    const establishesActivity =
      (event.type === "session.status" && status === "busy") ||
      event.type === "permission.updated" ||
      event.type === "permission.asked" ||
      event.type === "permission.v2.asked" ||
      event.type === "question.asked" ||
      event.type === "question.v2.asked";
    if (establishesActivity) await adoptRootSession(sessionID);

    // OpenCode 的事件总线会广播同目录下其它会话；只聚合当前根 Session 及其后代。
    if (!belongsToRoot(sessionID)) return;

    switch (event.type) {
      case "session.updated":
        await forwardRootTitle(sessionID);
        break;
      case "session.status": {
        if (isSubagentSession(sessionID)) {
          if (status === "busy") {
            await startSubagent(sessionID, properties);
            phaseBySession.set(sessionID, "busy");
          } else if (status === "idle") {
            await stopSubagent(sessionID);
          } else if (status === "retry") {
            await transition(sessionID, "retry", "ErrorOccurred", {
              recoverable: true,
              message: errorMessage(properties.status),
            });
          }
        } else if (status === "busy") {
          rootIdlePending = false;
          await transition(sessionID, "busy", "UserPromptSubmit");
        } else if (status === "idle") {
          await stopRootWhenChildrenFinish();
        } else if (status === "retry") {
          await transition(sessionID, "retry", "ErrorOccurred", {
            recoverable: true,
            message: errorMessage(properties.status),
          });
        }
        break;
      }
      case "session.idle":
        if (isSubagentSession(sessionID)) {
          await stopSubagent(sessionID);
        } else {
          await stopRootWhenChildrenFinish();
        }
        break;
      case "session.error": {
        const child = isSubagentSession(sessionID);
        await transition(sessionID, "failed", "ErrorOccurred", {
          recoverable: child,
          message: errorMessage(properties.error),
        });
        if (child) {
          await stopSubagent(sessionID);
        } else {
          rootIdlePending = false;
        }
        break;
      }
      case "session.deleted":
        if (isSubagentSession(sessionID)) {
          await stopSubagent(sessionID);
        } else {
          rootIdlePending = false;
          await forward("SessionEnd", { session_id: sessionID });
        }
        phaseBySession.delete(sessionID ?? "smelt-session");
        titleBySession.delete(sessionID);
        break;
      case "permission.updated":
      case "permission.asked":
      case "permission.v2.asked": {
        const permission =
          typeof properties.permission === "object" ? properties.permission : properties;
        await forward("PermissionRequest", {
          session_id: sessionID,
          tool_name: permission.permission ?? permission.action ?? permission.type ?? "tool",
          message: permission.title,
        });
        break;
      }
      case "permission.replied":
      case "permission.v2.replied":
        await forward("PostToolUse", { session_id: sessionID });
        break;
      case "question.asked":
      case "question.v2.asked": {
        const question = properties.question ?? properties;
        const message =
          question.questions?.[0]?.question ??
          question.questions?.[0]?.header ??
          question.text ??
          question.title ??
          "OpenCode 等待你的输入";
        await forward("Notification", {
          session_id: sessionID,
          notification_type: "elicitation_dialog",
          message,
        });
        break;
      }
      case "question.replied":
      case "question.rejected":
      case "question.v2.replied":
      case "question.v2.rejected":
        await forward("PostToolUse", { session_id: sessionID });
        break;
    }
  },
});
"#;
    template
        .replacen("__SMELT_MARKER__", OPENCODE_PLUGIN_MARKER, 1)
        .replacen("__SMELT_NOTIFY__", &quoted_notify, 1)
}

fn opencode_plugin_installed() -> bool {
    if !smelt_notify_available() {
        return false;
    }
    let Some(path) = opencode_plugin_path() else {
        return false;
    };
    std::fs::read_to_string(path)
        .is_ok_and(|source| source == opencode_plugin_source(&smelt_notify_path()))
}

fn install_opencode_plugin_at(
    path: &std::path::Path,
    notify: &std::path::Path,
) -> Result<(), String> {
    if path.is_file() {
        let source = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        if !source.starts_with(OPENCODE_PLUGIN_MARKER) {
            return Err(format!(
                "{} 已存在且不是 Smelt 管理的插件，请先改名后重试",
                path.display()
            ));
        }
    }
    write_text_atomic(path, &opencode_plugin_source(notify))
}

fn install_opencode_plugin() -> Result<(), String> {
    let notify = smelt_notify_path();
    if !notify.is_file() {
        return Err(format!(
            "找不到 {}，请先编译安装 smelt-notify",
            notify.display()
        ));
    }
    let path = opencode_plugin_path().ok_or_else(|| "无 home/config 目录".to_string())?;
    install_opencode_plugin_at(&path, &notify)
}

fn uninstall_opencode_plugin_at(path: &std::path::Path) -> Result<(), String> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    if source.starts_with(OPENCODE_PLUGIN_MARKER) {
        std::fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn uninstall_opencode_plugin() -> Result<(), String> {
    let Some(path) = opencode_plugin_path() else {
        return Ok(());
    };
    uninstall_opencode_plugin_at(&path)
}

const HOOK_INTEGRATIONS: &[HookIntegration] = &[
    HookIntegration {
        label: "Claude / Grok",
        installed: claude_hooks_installed,
        install: install_claude_hooks,
        uninstall: uninstall_claude_hooks,
    },
    HookIntegration {
        label: "Copilot",
        installed: copilot_hooks_installed,
        install: install_copilot_hooks,
        uninstall: uninstall_copilot_hooks,
    },
    HookIntegration {
        label: "Codex",
        installed: codex_hooks_installed,
        install: install_codex_hooks,
        uninstall: uninstall_codex_hooks,
    },
    HookIntegration {
        label: "Antigravity",
        installed: antigravity_hooks_installed,
        install: install_antigravity_hooks,
        uninstall: uninstall_antigravity_hooks,
    },
    HookIntegration {
        label: "Cursor",
        installed: cursor_hooks_installed,
        install: install_cursor_hooks,
        uninstall: uninstall_cursor_hooks,
    },
    HookIntegration {
        label: "OpenCode",
        installed: opencode_plugin_installed,
        install: install_opencode_plugin,
        uninstall: uninstall_opencode_plugin,
    },
    HookIntegration {
        label: "Kiro v3",
        installed: kiro_hooks_installed,
        install: install_kiro_hooks,
        uninstall: uninstall_kiro_hooks,
    },
];

#[derive(Clone, Copy)]
enum HookOperationKind {
    Install,
    Uninstall,
}

fn run_all_hook_operations(kind: HookOperationKind) -> Result<(), String> {
    let errors = HOOK_INTEGRATIONS
        .iter()
        .filter_map(|integration| {
            let operation = match kind {
                HookOperationKind::Install => integration.install,
                HookOperationKind::Uninstall => integration.uninstall,
            };
            operation()
                .err()
                .map(|error| format!("{}: {error}", integration.label))
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
    run_all_hook_operations(HookOperationKind::Install)
}

pub fn uninstall_agent_hooks() -> Result<(), String> {
    run_all_hook_operations(HookOperationKind::Uninstall)
}

#[cfg(test)]
mod provider_extension_tests {
    use super::*;

    fn smelt_handler_count(handlers: &[serde_json::Value]) -> usize {
        handlers
            .iter()
            .filter(|handler| {
                handler
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(command_uses_smelt_notify)
            })
            .count()
    }

    #[test]
    fn hook_registry_covers_every_terminal_agent_with_an_official_event_surface() {
        let labels = HOOK_INTEGRATIONS
            .iter()
            .map(|integration| integration.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "Claude / Grok",
                "Copilot",
                "Codex",
                "Antigravity",
                "Cursor",
                "OpenCode",
                "Kiro v3",
            ]
        );
        assert!(SMELT_HOOK_EVENTS.contains(&"Stop"));
        assert!(SMELT_HOOK_EVENTS.contains(&"StopCancelled"));
        assert!(SMELT_HOOK_EVENTS.contains(&"TeammateIdle"));
        assert!(CODEX_HOOK_EVENTS.contains(&"Stop"));
        assert!(CODEX_HOOK_EVENTS.contains(&"SessionStop"));
        assert!(CURSOR_HOOK_EVENTS.contains(&"stop"));
        assert!(COPILOT_HOOK_EVENTS.contains(&"agentStop"));
        assert!(KIRO_HOOK_EVENTS.contains(&"Stop"));
        assert_eq!(
            grok_hooks_path().map(|path| path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string()),
            Some("smelt-notifications.json".into())
        );
    }

    #[test]
    fn cursor_hook_merge_is_idempotent_and_preserves_other_handlers() {
        let mut root = serde_json::json!({
            "version": 1,
            "hooks": {
                "stop": [{"command":"/tmp/other-cursor-hook"}]
            }
        });
        merge_cursor_hooks(&mut root).unwrap();
        merge_cursor_hooks(&mut root).unwrap();

        for event in CURSOR_HOOK_EVENTS {
            assert!(cursor_event_installed(&root, event));
        }
        let stop = root["hooks"]["stop"].as_array().unwrap();
        assert_eq!(smelt_handler_count(stop), 1);
        assert!(
            stop.iter()
                .any(|handler| handler["command"] == "/tmp/other-cursor-hook")
        );

        remove_cursor_hooks(&mut root);
        let stop = root["hooks"]["stop"].as_array().unwrap();
        assert_eq!(smelt_handler_count(stop), 0);
        assert!(
            stop.iter()
                .any(|handler| handler["command"] == "/tmp/other-cursor-hook")
        );
    }

    #[test]
    fn kiro_hook_merge_is_idempotent_and_preserves_other_hooks() {
        let mut root = serde_json::json!({
            "version": "v1",
            "hooks": [{
                "name": "other-hook",
                "trigger": "Stop",
                "action": {"type":"command","command":"/tmp/other-kiro-hook"}
            }]
        });
        merge_kiro_hooks(&mut root).unwrap();
        merge_kiro_hooks(&mut root).unwrap();

        for event in KIRO_HOOK_EVENTS {
            assert!(kiro_event_installed(&root, event));
        }
        let hooks = root["hooks"].as_array().unwrap();
        assert_eq!(
            hooks
                .iter()
                .filter(|hook| {
                    hook.pointer("/action/command")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(command_uses_smelt_notify)
                })
                .count(),
            KIRO_HOOK_EVENTS.len()
        );

        remove_kiro_hooks(&mut root);
        let hooks = root["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["name"], "other-hook");
    }

    #[test]
    fn opencode_plugin_is_owned_and_forwards_structured_events() {
        let source = opencode_plugin_source(std::path::Path::new("/tmp/Smelt App/smelt-notify"));
        assert!(source.starts_with(OPENCODE_PLUGIN_MARKER));
        assert!(source.contains(r#"const SMELT_NOTIFY = "/tmp/Smelt App/smelt-notify";"#));
        assert!(source.contains("tool.execute.before"));
        assert!(source.contains("permission.updated"));
        assert!(source.contains("permission.asked"));
        assert!(source.contains("permission.v2.asked"));
        assert!(source.contains("question.asked"));
        assert!(source.contains("session.status"));
    }

    #[test]
    fn opencode_plugin_forwards_session_titles_without_changing_turn_phase() {
        let source = opencode_plugin_source(std::path::Path::new("/tmp/smelt-notify"));

        assert!(
            source.contains(r#"case "session.updated""#),
            "必须订阅 OpenCode 的 session.updated 标题事实"
        );
        assert!(
            source.contains(r#"forward("SessionTitleChanged""#),
            "标题更新必须走独立事件，不能冒充 UserPromptSubmit"
        );
        assert!(
            source.contains("properties.info?.title"),
            "标题必须读取 SDK 的 properties.info.title"
        );
    }

    #[test]
    fn opencode_plugin_aggregates_child_sessions_into_the_root_turn() {
        let source = opencode_plugin_source(std::path::Path::new("/tmp/smelt-notify"));

        assert!(source.contains("parentID"), "必须识别 OpenCode 子 Session");
        assert!(
            source.contains("activeSubagents"),
            "必须跟踪仍在运行的子 Session"
        );
        assert!(
            source.contains("rootIdlePending"),
            "根 Session idle 必须可延迟提交"
        );
        assert!(source.contains(r#"forward("SubagentStart""#));
        assert!(source.contains(r#"forward("SubagentStop""#));
        assert!(
            source.contains("adoptRootSession"),
            "恢复已有 OpenCode Session 时必须从首个活动事件认领根 Session"
        );
    }

    #[test]
    fn opencode_install_never_overwrites_an_unowned_plugin() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "smelt-opencode-hook-test-{}-{nonce}",
            std::process::id()
        ));
        let path = dir.join("smelt-notifications.js");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "export const UserPlugin = async () => ({});\n").unwrap();

        let result = install_opencode_plugin_at(&path, std::path::Path::new("/tmp/smelt-notify"));
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "export const UserPlugin = async () => ({});\n"
        );

        std::fs::remove_file(&path).unwrap();
        install_opencode_plugin_at(&path, std::path::Path::new("/tmp/smelt-notify")).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(OPENCODE_PLUGIN_MARKER)
        );
        uninstall_opencode_plugin_at(&path).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
