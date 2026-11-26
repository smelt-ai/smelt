//! Pi 原生 MCP 服务器配置：`~/.pi/agent/mcp.json`。
//!
//! Pi 1.1 起自带 MCP（`builtin:mcp`），会话启动时读这个文件、连上里面启用的服务器。
//! Smelt 只做配置界面：增删、启停、登录，格式和 Pi 文档（`docs/mcp.md`）一致，写回时
//! 保留文件里与本次操作无关的内容。会话里怎么连、怎么调用都交给 Pi。
//!
//! 登录走 `smelt-agent mcp login <name>`（见 [`crate::pi_auth::PiLoginSession::start_mcp`]）：OAuth
//! 流程（动态注册、PKCE、回环回调、令牌落盘到 `mcp-auth.json`）全在 Pi 里，这里不重写。

pub use smelt_model::pi_mcp::*;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use smelt_model::redact::{redact_args, redact_secrets};

use crate::pi_model_settings::{pi_agent_dir, read_or_create_json, write_json_file};

pub fn pi_mcp_path() -> PathBuf {
    pi_agent_dir().join("mcp.json")
}

fn pi_mcp_auth_path_at(agent_dir: &Path) -> PathBuf {
    agent_dir.join("mcp-auth.json")
}

pub fn load_pi_mcp_servers() -> Result<Vec<PiMcpServer>, String> {
    load_pi_mcp_servers_at(&pi_agent_dir())
}

pub fn load_pi_mcp_servers_at(agent_dir: &Path) -> Result<Vec<PiMcpServer>, String> {
    let doc = read_or_create_json(&agent_dir.join("mcp.json"))?;
    // 凭据文件读不出来不影响列出服务器，只是都显示成未登录。
    let signed_in = read_or_create_json(&pi_mcp_auth_path_at(agent_dir))
        .map(|auth| signed_in_urls(&auth))
        .unwrap_or_default();
    Ok(parse_servers(&doc, &signed_in))
}

fn parse_servers(doc: &Value, signed_in: &HashSet<String>) -> Vec<PiMcpServer> {
    let Some(servers) = doc.get("mcpServers").and_then(Value::as_object) else {
        return Vec::new();
    };
    servers
        .iter()
        .map(|(name, entry)| {
            let text = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            };
            let url = text("url");
            // 登录状态按原始地址认；交出去的地址和命令行打过码（见 `PiMcpTransport`）。
            let signed_in = url
                .as_deref()
                .is_some_and(|url| signed_in.contains(&normalize_url(url)));
            let transport = match (url, text("command")) {
                (Some(url), _) => PiMcpTransport::Http {
                    url: redact_secrets(&url),
                },
                (None, Some(command)) => PiMcpTransport::Stdio {
                    command: redact_secrets(&command),
                    args: redact_args(
                        &entry
                            .get("args")
                            .and_then(Value::as_array)
                            .map(|args| {
                                args.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default(),
                    ),
                },
                (None, None) => PiMcpTransport::Unknown,
            };
            PiMcpServer {
                name: name.clone(),
                transport,
                enabled: entry.get("enabled").and_then(Value::as_bool) != Some(false),
                description: text("description"),
                signed_in,
            }
        })
        .collect()
}

/// 旧版预设用单独下载的 Bun 跑（`~/.smelt/runtime/bin/bun`），那份 Bun 已经不再下载。
const RETIRED_BUN_LAUNCHERS: [&str; 2] =
    ["~/.smelt/runtime/bin/bun", "~/.smelt/runtime/bin/bun.exe"];

/// 把 `mcp.json` 里还指着旧 Bun 的条目改成 smelt-agent 自带的 Bun，参数原样保留：
/// `smelt-agent` 带上 `BUN_BE_BUN=1` 就是 Bun 命令行。没有要改的就不碰文件。守护启动时调。
pub fn retire_managed_bun_entries() {
    if let Err(error) = retire_managed_bun_entries_at(&pi_agent_dir()) {
        crate::app_log::error(
            "pi-mcp",
            &format!("迁移 mcp.json 里的旧 Bun 命令失败：{error}"),
        );
    }
}

fn retire_managed_bun_entries_at(agent_dir: &Path) -> Result<(), String> {
    let path = agent_dir.join("mcp.json");
    let retired = |entry: &Value| {
        entry
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| RETIRED_BUN_LAUNCHERS.contains(&command.trim()))
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let doc: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{} isn't valid JSON: {error}", path.display()))?;
    let needs = doc
        .get("mcpServers")
        .and_then(Value::as_object)
        .is_some_and(|servers| servers.values().any(retired));
    if !needs {
        return Ok(());
    }
    edit_servers(agent_dir, |servers| {
        for entry in servers.values_mut().filter(|entry| retired(entry)) {
            let Some(entry) = entry.as_object_mut() else {
                continue;
            };
            entry.insert(
                "command".into(),
                Value::String(smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH.into()),
            );
            let (name, value) = smelt_model::agent_paths::BUN_BE_BUN;
            let env = entry
                .entry("env")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(env) = env.as_object_mut() {
                env.insert(name.into(), Value::String(value.into()));
            }
        }
        Ok(())
    })
}

/// 预设换了跑法，按预设登记过的条目跟着换：不然只有新加的用新的，已经加过的永远停在旧的。
/// 认条目看名字和预设一样、命令是 smelt-agent、参数里有这个包（`包@版本`）：
/// - 预设换了钉的版本：只换那一个参数；
/// - 预设编进了 smelt-agent：`x --bun 包@版本` 换成 `mcp-server <id>`，包后面的参数照旧，
///   去掉 `BUN_BE_BUN`，随后删掉这些包在 `bun x` 缓存里的旧下载。
///
/// 用户改过的别的参数、`env` 照旧；没有要改的就不碰文件。守护启动时调，排在
/// [`retire_managed_bun_entries`] 后面（旧 Bun 的条目先换成 smelt-agent）。
pub fn upgrade_preset_pins() {
    let agent_dir = pi_agent_dir();
    if let Err(error) = upgrade_preset_pins_at(&agent_dir, smelt_paths::remote_desktop_installed())
    {
        crate::app_log::error(
            "pi-mcp",
            &format!("升级 mcp.json 里预设的版本失败：{error}"),
        );
        return;
    }
    if let Some(home) = smelt_paths::smelt_home() {
        remove_bun_x_downloads_of_built_in_packages(&agent_dir, &home.join("cache").join("bun-x"));
    }
}

/// 编进 smelt-agent 之前，这些预设用自带 Bun 的 `x --bun 包@版本` 跑：`(预设 id, 包名)`。
const PACKAGES_BUILT_INTO_SMELT_AGENT: [(&str, &str); 2] = [
    ("browser-mcp", "@agent360/browser-mcp"),
    ("playwright", "@playwright/mcp"),
];

/// 参数里写着 `package@版本` 的那一个的位置。
fn package_arg(args: &[Value], package: &str) -> Option<usize> {
    args.iter().position(|arg| {
        arg.as_str()
            .and_then(|arg| arg.strip_prefix(package))
            .is_some_and(|version| version.starts_with('@'))
    })
}

/// 按预设登记的条目跟上预设后的样子；不用改返回 `None`。
fn follow_preset(name: &str, entry: &Value, remote_desktop: bool) -> Option<Value> {
    let preset = find_preset(name)?.for_machine(remote_desktop);
    let command = entry.get("command").and_then(Value::as_str)?;
    if command.trim() != smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH {
        return None;
    }
    let args = entry.get("args").and_then(Value::as_array)?;
    let mut followed = entry.clone();
    match preset.transport {
        PiMcpPresetTransport::Http { .. } => return None,
        PiMcpPresetTransport::Bun { .. } => {
            let (package, pinned) = preset.pinned_package()?;
            let index = package_arg(args, package)?;
            if args[index] == pinned {
                return None;
            }
            followed["args"][index] = Value::String(pinned.into());
        }
        PiMcpPresetTransport::SmeltAgent { .. } => {
            let (_, package) = PACKAGES_BUILT_INTO_SMELT_AGENT
                .iter()
                .find(|(id, _)| *id == preset.id)?;
            let index = package_arg(args, package)?;
            let moved = [smelt_model::agent_paths::SMELT_AGENT_MCP_SERVER, preset.id]
                .into_iter()
                .map(|arg| Value::String(arg.into()))
                .chain(args[index + 1..].iter().cloned())
                .collect();
            followed["args"] = Value::Array(moved);
            let followed = followed.as_object_mut()?;
            let env_left = followed
                .get_mut("env")
                .and_then(Value::as_object_mut)
                .map(|env| {
                    env.remove(smelt_model::agent_paths::BUN_BE_BUN.0);
                    !env.is_empty()
                });
            if env_left == Some(false) {
                followed.remove("env");
            }
        }
    }
    Some(followed)
}

fn upgrade_preset_pins_at(agent_dir: &Path, remote_desktop: bool) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(agent_dir.join("mcp.json")) else {
        return Ok(());
    };
    let doc: Value = serde_json::from_str(&text)
        .map_err(|error| format!("mcp.json isn't valid JSON: {error}"))?;
    let needs = doc
        .get("mcpServers")
        .and_then(Value::as_object)
        .is_some_and(|servers| {
            servers
                .iter()
                .any(|(name, entry)| follow_preset(name, entry, remote_desktop).is_some())
        });
    if !needs {
        return Ok(());
    }
    edit_servers(agent_dir, |servers| {
        for (name, entry) in servers.iter_mut() {
            if let Some(followed) = follow_preset(name, entry, remote_desktop) {
                *entry = followed;
            }
        }
        Ok(())
    })
}

/// 删掉编进 smelt-agent 的包以前用 `x` 下载的那份（`<cache>/<包名 / 换成 +>@<版本>`，见
/// `agent/src/pi-mcp-runtime.ts` 的 `bunCacheFor`），一份几十上百兆。`mcp.json` 里还有条目在用
/// `x` 跑同一个包（用户自己加的）就留着，不然它每次守护启动都得重新下载。
fn remove_bun_x_downloads_of_built_in_packages(agent_dir: &Path, cache_root: &Path) {
    let Ok(listing) = std::fs::read_dir(cache_root) else {
        return;
    };
    let doc = std::fs::read_to_string(agent_dir.join("mcp.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let still_downloaded = |package: &str| {
        doc.as_ref()
            .and_then(|doc| doc.get("mcpServers"))
            .and_then(Value::as_object)
            .is_some_and(|servers| {
                servers.values().any(|entry| {
                    entry
                        .get("args")
                        .and_then(Value::as_array)
                        .is_some_and(|args| package_arg(args, package).is_some())
                })
            })
    };
    let retired: Vec<String> = PACKAGES_BUILT_INTO_SMELT_AGENT
        .iter()
        .filter(|(_, package)| !still_downloaded(package))
        .map(|(_, package)| format!("{}@", package.replacen('/', "+", 1)))
        .collect();
    for item in listing.flatten() {
        let name = item.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !retired
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
        {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(item.path()) {
            crate::app_log::error(
                "pi-mcp",
                &format!("删除 {} 失败：{error}", item.path().display()),
            );
        }
    }
}

/// `mcp-auth.json` 的键是 `<命名空间>|<URL>`（旧版只有 URL），值里有 `tokens` 才算登录过。
/// 只按 URL 认：命名空间的写法是 Pi 的内部细节，这里不跟。
fn signed_in_urls(auth: &Value) -> HashSet<String> {
    let Some(states) = auth.as_object() else {
        return HashSet::new();
    };
    states
        .iter()
        .filter(|(_, state)| state.get("tokens").is_some_and(|tokens| !tokens.is_null()))
        .map(|(key, _)| {
            let url = key.rsplit_once('|').map_or(key.as_str(), |(_, url)| url);
            normalize_url(url)
        })
        .collect()
}

/// 和 Pi 的 `String(new URL(url))` 对齐：`https://host` 会补成 `https://host/`。
fn normalize_url(url: &str) -> String {
    url::Url::parse(url.trim()).map_or_else(|_| url.trim().to_string(), |url| url.to_string())
}

/// 写哪一套由这台机器决定：装了远程桌面的，浏览器类预设接那里常驻的 Chrome。
pub fn add_pi_mcp_preset(id: &str) -> Result<(), String> {
    add_pi_mcp_preset_at(&pi_agent_dir(), id, smelt_paths::remote_desktop_installed())
}

pub fn add_pi_mcp_preset_at(
    agent_dir: &Path,
    id: &str,
    remote_desktop: bool,
) -> Result<(), String> {
    let preset = find_preset(id)
        .ok_or_else(|| format!("there's no built-in MCP server {id}"))?
        .for_machine(remote_desktop);
    edit_servers(agent_dir, |servers| {
        if servers.contains_key(preset.id) {
            return Err(format!("mcp.json already has a server named {}", preset.id));
        }
        servers.insert(preset.id.into(), preset.config());
        Ok(())
    })
}

pub fn set_pi_mcp_server_enabled(name: &str, enabled: bool) -> Result<(), String> {
    set_pi_mcp_server_enabled_at(&pi_agent_dir(), name, enabled)
}

/// 启用就删掉 `enabled` 键（Pi 的默认值就是启用），停用写 `false`。
pub fn set_pi_mcp_server_enabled_at(
    agent_dir: &Path,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    edit_servers(agent_dir, |servers| {
        let entry = servers
            .get_mut(name)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("mcp.json has no server {name}"))?;
        if enabled {
            entry.remove("enabled");
        } else {
            entry.insert("enabled".into(), Value::Bool(false));
        }
        Ok(())
    })
}

pub fn remove_pi_mcp_server(name: &str) -> Result<(), String> {
    remove_pi_mcp_server_at(&pi_agent_dir(), name)
}

pub fn remove_pi_mcp_server_at(agent_dir: &Path, name: &str) -> Result<(), String> {
    edit_servers(agent_dir, |servers| {
        servers
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| format!("mcp.json has no server {name}"))
    })
}

/// 读 → 改 `mcpServers` → 写回。文件里其他顶层键（如 `autoEnableCodemode`）原样保留。
///
/// 运行时会并发处理控制请求（连点两个开关），读改写不串行的话，后写的拿着旧快照会把
/// 先写的改动盖掉。进程内一把锁就够；Pi 自己改 `mcp.json`（`/mcp` 里切开关）不在此列。
fn edit_servers(
    agent_dir: &Path,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> Result<(), String> {
    static EDIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = EDIT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = agent_dir.join("mcp.json");
    let mut doc = read_or_create_json(&path)?;
    let root = doc
        .as_object_mut()
        .ok_or_else(|| format!("the top level of {} isn't an object", path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("mcpServers in {} isn't an object", path.display()))?;
    edit(servers)?;
    write_json_file(&path, &doc, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧版预设指着单独下载的 Bun；改成 smelt-agent 自带的，参数和别的条目都不动。
    #[test]
    fn entries_on_the_retired_bun_move_to_smelt_agent() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::json!({
                "mcpServers": {
                    "playwright": {
                        "command": "~/.smelt/runtime/bin/bun",
                        "args": ["x", "--bun", "@playwright/mcp@0.0.83", "--extension"],
                        "exposure": "deferred",
                    },
                    "rea": {
                        "command": "~/.smelt/runtime/bin/bun.exe",
                        "args": ["x", "--bun", "rea-agents@6.5.0", "mcp"],
                        "env": {"REA_HOME": "/data"},
                    },
                    "mine": {"command": "npx", "args": ["-y", "mine"]},
                    "figma": {"url": "https://mcp.figma.com/mcp"},
                },
                "settings": {"keep": true},
            })
            .to_string(),
        )
        .unwrap();

        retire_managed_bun_entries_at(&dir).unwrap();

        let doc = read(&dir);
        let playwright = &doc["mcpServers"]["playwright"];
        assert_eq!(
            playwright["command"],
            smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH
        );
        assert_eq!(
            playwright["args"],
            serde_json::json!(["x", "--bun", "@playwright/mcp@0.0.83", "--extension"])
        );
        assert_eq!(playwright["env"], serde_json::json!({"BUN_BE_BUN": "1"}));
        assert_eq!(playwright["exposure"], "deferred");
        let rea = &doc["mcpServers"]["rea"];
        assert_eq!(
            rea["command"],
            smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH
        );
        assert_eq!(
            rea["env"],
            serde_json::json!({"REA_HOME": "/data", "BUN_BE_BUN": "1"})
        );
        assert_eq!(doc["mcpServers"]["mine"]["command"], "npx");
        assert!(doc["mcpServers"]["mine"].get("env").is_none());
        assert_eq!(doc["settings"]["keep"], true);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn nothing_to_retire_leaves_mcp_json_alone() {
        let dir = temp_dir();
        retire_managed_bun_entries_at(&dir).unwrap();
        assert!(!dir.join("mcp.json").exists(), "没有文件就不建");

        let text = "{\"mcpServers\":{\"mine\":{\"command\":\"npx\"}}}";
        std::fs::write(dir.join("mcp.json"), text).unwrap();
        retire_managed_bun_entries_at(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("mcp.json")).unwrap(), text);
        std::fs::remove_dir_all(dir).ok();
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("smelt-agent-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("mcp.json")).unwrap()).unwrap()
    }

    #[test]
    fn missing_file_lists_nothing() {
        let dir = temp_dir();
        assert_eq!(load_pi_mcp_servers_at(&dir).unwrap(), Vec::new());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adding_figma_writes_the_documented_entry_and_keeps_other_content() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("mcp.json"),
            r#"{"autoEnableCodemode":false,"mcpServers":{"docs":{"url":"https://example.com/mcp"}}}"#,
        )
        .unwrap();

        add_pi_mcp_preset_at(&dir, "figma", false).unwrap();

        let doc = read(&dir);
        assert_eq!(doc["autoEnableCodemode"], Value::Bool(false));
        assert_eq!(doc["mcpServers"]["docs"]["url"], "https://example.com/mcp");
        let figma = &doc["mcpServers"]["figma"];
        assert_eq!(figma["url"], "https://mcp.figma.com/mcp");
        assert_eq!(figma["oauth"]["clientName"], "Claude Code");
        assert_eq!(figma["exposure"], "deferred");
        assert!(
            add_pi_mcp_preset_at(&dir, "figma", false).is_err(),
            "重复添加会覆盖用户改过的条目，必须拒绝"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// 预设换了版本：按预设登记的条目只换钉的那个参数；用户自己的条目、同名但不是 smelt-agent
    /// 跑的、已经是新版本的都不动。
    #[test]
    fn registered_presets_follow_the_pinned_version() {
        let dir = temp_dir();
        let launcher = smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH;
        let doc = serde_json::json!({
            "mcpServers": {
                "rea": {
                    "command": launcher,
                    "args": ["x", "--bun", "rea-agents@6.7.0", "mcp"],
                    "env": {"BUN_BE_BUN": "1", "GHIDRA_INSTALL_DIR": "/opt/ghidra"},
                    "timeout": 120,
                },
                "browser-mcp": {"command": "npx", "args": ["-y", "@agent360/browser-mcp@1.0.0"]},
                "mine": {"command": launcher, "args": ["x", "--bun", "rea-agents@6.5.0", "mcp"]},
            },
            "settings": {"keep": true},
        });
        std::fs::write(dir.join("mcp.json"), doc.to_string()).unwrap();

        upgrade_preset_pins_at(&dir, false).unwrap();

        let rea_pin = find_preset("rea").unwrap().pinned_package().unwrap().1;
        assert_eq!(rea_pin, "rea-agents@6.9.0");
        let mut expected = doc;
        expected["mcpServers"]["rea"]["args"][2] = rea_pin.into();
        assert_eq!(read(&dir), expected);

        // 已经是新版本：不再碰文件。
        let before = std::fs::metadata(dir.join("mcp.json"))
            .unwrap()
            .modified()
            .unwrap();
        upgrade_preset_pins_at(&dir, false).unwrap();
        let after = std::fs::metadata(dir.join("mcp.json"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
    }

    /// 预设编进了 smelt-agent：登记过的条目从 `x --bun 包@版本` 换成 `mcp-server <id>`，包后面的
    /// 参数和别的 `env` 照旧，`BUN_BE_BUN` 去掉（带着它 smelt-agent 就成了 Bun 命令行）。用户自己的
    /// 条目不动。
    #[test]
    fn presets_built_into_smelt_agent_move_off_bun_x() {
        let dir = temp_dir();
        let launcher = smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH;
        let doc = serde_json::json!({
            "mcpServers": {
                "playwright": {
                    "command": launcher,
                    "args": ["x", "--bun", "@playwright/mcp@0.0.85", "--extension", "--mine"],
                    "env": {"BUN_BE_BUN": "1"},
                    "exposure": "deferred",
                },
                "browser-mcp": {
                    "command": launcher,
                    "args": ["x", "--bun", "@agent360/browser-mcp@1.30.2"],
                    "env": {"BUN_BE_BUN": "1", "KEEP": "yes"},
                },
                "mine": {"command": launcher, "args": ["x", "--bun", "@playwright/mcp@0.0.85"]},
            },
            "settings": {"keep": true},
        });
        std::fs::write(dir.join("mcp.json"), doc.to_string()).unwrap();

        upgrade_preset_pins_at(&dir, false).unwrap();

        let mut expected = doc;
        expected["mcpServers"]["playwright"] = serde_json::json!({
            "command": launcher,
            "args": ["mcp-server", "playwright", "--extension", "--mine"],
            "exposure": "deferred",
        });
        expected["mcpServers"]["browser-mcp"] = serde_json::json!({
            "command": launcher,
            "args": ["mcp-server", "browser-mcp"],
            "env": {"KEEP": "yes"},
        });
        assert_eq!(read(&dir), expected);
        assert_eq!(
            expected["mcpServers"]["playwright"],
            {
                let mut fresh = find_preset("playwright").unwrap().config();
                fresh["args"].as_array_mut().unwrap().push("--mine".into());
                fresh.as_object_mut().unwrap().remove("description");
                fresh
            },
            "迁移后的条目和新加的预设是同一种写法"
        );

        // 换过一次就不再碰文件。
        let before = std::fs::metadata(dir.join("mcp.json"))
            .unwrap()
            .modified()
            .unwrap();
        upgrade_preset_pins_at(&dir, false).unwrap();
        let after = std::fs::metadata(dir.join("mcp.json"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remote_desktop_playwright_moves_off_bun_x_with_its_endpoint() {
        let dir = temp_dir();
        let launcher = smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH;
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::json!({"mcpServers": {"playwright": {
                "command": launcher,
                "args": ["x", "--bun", "@playwright/mcp@0.0.85", "--cdp-endpoint", "http://127.0.0.1:9222"],
                "env": {"BUN_BE_BUN": "1"},
            }}})
            .to_string(),
        )
        .unwrap();

        upgrade_preset_pins_at(&dir, true).unwrap();

        assert_eq!(
            read(&dir)["mcpServers"]["playwright"],
            serde_json::json!({
                "command": launcher,
                "args": ["mcp-server", "playwright", "--cdp-endpoint", "http://127.0.0.1:9222"],
            })
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// 编进 smelt-agent 的包，`bun x` 缓存里的旧下载删掉；还有条目用 `x` 跑它的留着，别的包不碰。
    #[test]
    fn bun_x_downloads_of_built_in_packages_are_removed() {
        let dir = temp_dir();
        let cache = dir.join("bun-x");
        for name in [
            "@playwright+mcp@0.0.85",
            "@playwright+mcp@0.0.83",
            "@agent360+browser-mcp@1.31.0",
            "@playwright+mcp-tools@1.0.0",
            "rea-agents@6.9.0",
        ] {
            std::fs::create_dir_all(cache.join(name).join("node_modules")).unwrap();
        }
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::json!({"mcpServers": {"mine": {
                "command": smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH,
                "args": ["x", "--bun", "@agent360/browser-mcp@1.31.0"],
            }}})
            .to_string(),
        )
        .unwrap();

        remove_bun_x_downloads_of_built_in_packages(&dir, &cache);

        let mut left: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|item| item.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "@agent360+browser-mcp@1.31.0",
                "@playwright+mcp-tools@1.0.0",
                "rea-agents@6.9.0"
            ]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bun_presets_name_their_pinned_package() {
        assert_eq!(
            find_preset("rea")
                .unwrap()
                .pinned_package()
                .map(|(package, _)| package),
            Some("rea-agents")
        );
        assert_eq!(
            find_preset("chrome-devtools")
                .unwrap()
                .pinned_package()
                .map(|(package, _)| package),
            Some("chrome-devtools-mcp")
        );
        assert_eq!(
            find_preset("playwright").unwrap().pinned_package(),
            None,
            "编进 smelt-agent 的不在运行时下载，没有钉的包"
        );
        for preset in PI_MCP_PRESETS {
            if let PiMcpPresetTransport::Bun { .. } = preset.for_machine(true).transport {
                assert!(
                    preset.for_machine(true).pinned_package().is_some(),
                    "{}",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn adding_rea_writes_a_pinned_stdio_entry() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "rea", false).unwrap();

        let rea = &read(&dir)["mcpServers"]["rea"];
        assert_eq!(
            rea["command"],
            smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH
        );
        assert_eq!(
            rea["args"],
            serde_json::json!(["x", "--bun", "rea-agents@6.9.0", "mcp"]),
            "持久注册必须钉死版本，不能用 @latest；--bun 防止转交给系统 node"
        );
        assert_eq!(
            rea["env"],
            serde_json::json!({"BUN_BE_BUN": "1"}),
            "smelt-agent 带上它才是 Bun 命令行"
        );
        assert_eq!(rea["timeout"], 120);
        assert!(
            rea.get("exposure").is_none(),
            "工具目录大，留在默认 codemode"
        );
        assert!(rea.get("url").is_none());
        let servers = load_pi_mcp_servers_at(&dir).unwrap();
        assert_eq!(
            servers[0].transport,
            PiMcpTransport::Stdio {
                command: smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH.into(),
                args: vec![
                    "x".into(),
                    "--bun".into(),
                    "rea-agents@6.9.0".into(),
                    "mcp".into(),
                ],
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adding_agent360_browser_writes_a_built_in_deferred_stdio_entry() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "browser-mcp", false).unwrap();

        let browser = &read(&dir)["mcpServers"]["browser-mcp"];
        assert_eq!(
            browser["command"],
            smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH
        );
        assert_eq!(
            browser["args"],
            serde_json::json!(["mcp-server", "browser-mcp"]),
            "编进 smelt-agent；上游入口不带参数才启动服务器"
        );
        assert!(
            browser.get("env").is_none(),
            "带 BUN_BE_BUN 就成了 Bun 命令行"
        );
        assert_eq!(browser["exposure"], "deferred");
        assert!(browser.get("url").is_none());
        assert!(
            !find_preset("browser-mcp")
                .unwrap()
                .transport
                .needs_sign_in()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adding_playwright_writes_a_built_in_deferred_stdio_entry() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "playwright", false).unwrap();

        let playwright = &read(&dir)["mcpServers"]["playwright"];
        assert_eq!(
            playwright["command"],
            smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH
        );
        assert_eq!(
            playwright["args"],
            serde_json::json!(["mcp-server", "playwright", "--extension"]),
            "编进 smelt-agent；--extension 接用户已登录的 Chrome"
        );
        assert!(playwright.get("env").is_none());
        assert_eq!(playwright["exposure"], "deferred");
        assert!(playwright.get("url").is_none());
        let preset = find_preset("playwright").unwrap();
        assert!(!preset.transport.needs_sign_in());
        assert!(
            preset
                .extension_url
                .is_some_and(|url| url.starts_with("https://chromewebstore.google.com/")),
            "--extension 模式离不开扩展，得给安装入口"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn playwright_on_a_remote_desktop_connects_to_the_local_debugging_port() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "playwright", true).unwrap();

        let chrome = &read(&dir)["mcpServers"]["playwright"];
        assert_eq!(
            chrome["command"],
            smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH
        );
        assert_eq!(
            chrome["args"],
            serde_json::json!([
                "mcp-server",
                "playwright",
                "--cdp-endpoint",
                "http://127.0.0.1:9222"
            ]),
            "接 smelt-chrome 只绑本机的调试端口，和人在远程桌面里看的是同一个 Chrome"
        );
        assert_eq!(chrome["exposure"], "deferred");
        let preset = find_preset("playwright").unwrap().for_machine(true);
        assert_eq!(preset.id, "playwright", "换接法不换服务器名");
        assert!(preset.extension_url.is_none(), "走调试端口，不需要扩展");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn chrome_devtools_presets_connect_to_the_right_chrome() {
        for (remote_desktop, connection) in [
            (false, "--auto-connect"),
            (true, "--browser-url=http://127.0.0.1:9222"),
        ] {
            let dir = temp_dir();
            add_pi_mcp_preset_at(&dir, "chrome-devtools", remote_desktop).unwrap();

            let chrome = &read(&dir)["mcpServers"]["chrome-devtools"];
            assert_eq!(
                chrome["command"],
                smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH
            );
            assert_eq!(
                chrome["args"],
                serde_json::json!([
                    "x",
                    "--bun",
                    "chrome-devtools-mcp@1.13.0",
                    connection,
                    "--memory-debugging",
                    "--no-usage-statistics",
                    "--no-performance-crux"
                ])
            );
            assert_eq!(
                chrome["env"],
                serde_json::json!({"BUN_BE_BUN": "1", "CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS": "1"})
            );
            assert_eq!(chrome["exposure"], "deferred");
            assert_eq!(chrome["timeout"], 120);
            let preset = find_preset("chrome-devtools")
                .unwrap()
                .for_machine(remote_desktop);
            assert_eq!(
                preset.pinned_package(),
                Some(("chrome-devtools-mcp", "chrome-devtools-mcp@1.13.0"))
            );
            assert!(preset.extension_url.is_none());
            std::fs::remove_dir_all(dir).ok();
        }
    }

    /// 只有浏览器类预设分机器；其余的在远程桌面上照原样写。
    #[test]
    fn presets_without_a_remote_desktop_variant_ignore_the_machine() {
        for preset in PI_MCP_PRESETS {
            if preset.on_remote_desktop.is_none() {
                assert_eq!(preset.for_machine(true), preset, "{}", preset.id);
            }
            assert_eq!(preset.for_machine(false), preset, "{}", preset.id);
        }
    }

    #[test]
    fn preset_ids_are_unique_valid_server_names() {
        let mut seen = HashSet::new();
        for preset in PI_MCP_PRESETS {
            assert!(seen.insert(preset.id), "重复的内置 id {}", preset.id);
            assert!(
                preset
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "Pi 的服务器名只能含字母、数字、_ 和 -：{}",
                preset.id
            );
        }
    }

    #[test]
    fn disabling_writes_false_and_enabling_drops_the_key() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "figma", false).unwrap();

        set_pi_mcp_server_enabled_at(&dir, "figma", false).unwrap();
        assert_eq!(
            read(&dir)["mcpServers"]["figma"]["enabled"],
            Value::Bool(false)
        );
        assert!(!load_pi_mcp_servers_at(&dir).unwrap()[0].enabled);

        set_pi_mcp_server_enabled_at(&dir, "figma", true).unwrap();
        assert!(read(&dir)["mcpServers"]["figma"].get("enabled").is_none());
        assert!(load_pi_mcp_servers_at(&dir).unwrap()[0].enabled);

        remove_pi_mcp_server_at(&dir, "figma").unwrap();
        assert!(load_pi_mcp_servers_at(&dir).unwrap().is_empty());
        assert!(remove_pi_mcp_server_at(&dir, "figma").is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn transports_and_sign_in_come_from_both_files() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("mcp.json"),
            r#"{"mcpServers":{
                "figma":{"url":"https://mcp.figma.com/mcp"},
                "bare":{"url":"https://example.com"},
                "fs":{"command":"npx","args":["-y","server-filesystem"]},
                "override":{"enabled":false}
            }}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("mcp-auth.json"),
            r#"{
                "mcp__figma|https://mcp.figma.com/mcp":{"tokens":{"access_token":"x"}},
                "https://example.com/":{"tokens":null}
            }"#,
        )
        .unwrap();

        let servers = load_pi_mcp_servers_at(&dir).unwrap();
        let by_name = |name: &str| servers.iter().find(|server| server.name == name).unwrap();
        assert!(by_name("figma").signed_in);
        assert!(!by_name("bare").signed_in, "没有令牌不算登录");
        assert_eq!(
            by_name("fs").transport,
            PiMcpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "server-filesystem".into()],
            }
        );
        assert_eq!(by_name("override").transport, PiMcpTransport::Unknown);
        assert!(!by_name("override").enabled);
        std::fs::remove_dir_all(dir).ok();
    }

    /// 地址和命令行里的凭据不出运行时：交给客户端的快照已经打码，登录状态照样按原地址认。
    #[test]
    fn listed_transports_mask_credentials() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("mcp.json"),
            r#"{"mcpServers":{
                "remote":{"url":"https://u:p@mcp.example.com/mcp?api_key=k1&team=a"},
                "local":{"command":"npx","args":["-y","server","--token","t1","-H","Authorization: Bearer b1","DEBUG=1"]}
            }}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("mcp-auth.json"),
            r#"{"mcp__remote|https://u:p@mcp.example.com/mcp?api_key=k1&team=a":{"tokens":{}}}"#,
        )
        .unwrap();

        let servers = load_pi_mcp_servers_at(&dir).unwrap();
        let by_name = |name: &str| servers.iter().find(|server| server.name == name).unwrap();
        assert_eq!(
            by_name("remote").transport,
            PiMcpTransport::Http {
                url: "https://***@mcp.example.com/mcp?api_key=***&team=a".into(),
            }
        );
        assert!(by_name("remote").signed_in, "登录状态按原始地址匹配");
        assert_eq!(
            by_name("local").transport,
            PiMcpTransport::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "server".into(),
                    "--token".into(),
                    "***".into(),
                    "-H".into(),
                    "Authorization: Bearer ***".into(),
                    "DEBUG=1".into(),
                ],
            }
        );
        let raw = std::fs::read_to_string(dir.join("mcp.json")).unwrap();
        assert!(raw.contains("k1") && raw.contains("t1"), "文件本身不动");
        std::fs::remove_dir_all(dir).ok();
    }

    /// 用户把写着令牌的 `mcp.json` 设成 600、还用 dotfiles 链接管着：在设置里切一次开关，
    /// 权限和链接都得原样留着。
    #[cfg(unix)]
    #[test]
    fn toggling_keeps_mode_and_symlink_of_mcp_json() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        let dotfiles = dir.join("dotfiles");
        let agent = dir.join("agent");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        let target = dotfiles.join("mcp.json");
        std::fs::write(
            &target,
            r#"{"mcpServers":{"docs":{"url":"https://example.com/mcp","headers":{"Authorization":"Bearer literal"}}}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, agent.join("mcp.json")).unwrap();

        set_pi_mcp_server_enabled_at(&agent, "docs", false).unwrap();

        assert!(
            std::fs::symlink_metadata(agent.join("mcp.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(doc["mcpServers"]["docs"]["enabled"], Value::Bool(false));
        assert_eq!(
            doc["mcpServers"]["docs"]["headers"]["Authorization"],
            "Bearer literal"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// 并发的读改写不能互相覆盖：每个线程停用一台，最后全都是停用。
    #[test]
    fn concurrent_edits_do_not_lose_updates() {
        let dir = temp_dir();
        let names: Vec<String> = (0..8).map(|index| format!("s{index}")).collect();
        let servers: Map<String, Value> = names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    serde_json::json!({ "url": "https://example.com/mcp" }),
                )
            })
            .collect();
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::json!({ "mcpServers": servers }).to_string(),
        )
        .unwrap();

        std::thread::scope(|scope| {
            for name in &names {
                let dir = &dir;
                scope.spawn(move || set_pi_mcp_server_enabled_at(dir, name, false).unwrap());
            }
        });

        let servers = load_pi_mcp_servers_at(&dir).unwrap();
        assert_eq!(servers.len(), names.len());
        assert!(
            servers.iter().all(|server| !server.enabled),
            "有改动被覆盖：{servers:?}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn auth_keys_match_after_url_normalization() {
        let auth = serde_json::json!({ "https://example.com/": { "tokens": {} } });
        let urls = signed_in_urls(&auth);
        assert!(urls.contains(&normalize_url("https://example.com")));
    }
}
