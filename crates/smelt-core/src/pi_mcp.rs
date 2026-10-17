//! Pi 原生 MCP 服务器配置：`~/.pi/agent/mcp.json`。
//!
//! Pi 1.1 起自带 MCP（`builtin:mcp`），会话启动时读这个文件、连上里面启用的服务器。
//! Smelt 只做配置界面：增删、启停、登录，格式和 Pi 文档（`docs/mcp.md`）一致，写回时
//! 保留文件里与本次操作无关的内容。会话里怎么连、怎么调用都交给 Pi。
//!
//! 登录走受管运行时里的 `src/mcp-main.ts`，也就是 `pi mcp login <name>`：OAuth 流程
//! （动态注册、PKCE、回环回调、令牌落盘到 `mcp-auth.json`）全在 Pi 里，这里不重写。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::pi_model_settings::{pi_agent_dir, read_or_create_json, write_json_file};

/// 受管运行时里的 `pi mcp` 入口，相对 pi-agent 根目录。
const MCP_SCRIPT: &str = "src/mcp-main.ts";

/// 登录最多等浏览器这么久，和 `pi mcp login` 的默认值一致。
const LOGIN_TIMEOUT_SECS: u64 = 300;

pub fn pi_mcp_path() -> PathBuf {
    pi_agent_dir().join("mcp.json")
}

fn pi_mcp_auth_path_at(agent_dir: &Path) -> PathBuf {
    agent_dir.join("mcp-auth.json")
}

/// 内置的一键添加项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PiMcpPreset {
    /// 写进 `mcpServers` 的服务器名，也是工具名前缀 `mcp__<id>__`。
    pub id: &'static str,
    pub name: &'static str,
    /// 给人看的一句话。
    pub summary: &'static str,
    /// 写进配置的 `description`：Pi 把它放进系统提示，模型据此判断何时用这组工具。
    pub description: &'static str,
    pub transport: PiMcpPresetTransport,
    /// 不写就是 Pi 的默认 `codemode`：工具不直接声明给模型，靠 codemode 脚本发现和调用。
    pub exposure: Option<&'static str>,
    /// 单次请求超时（秒）。不写用 Pi 的默认 60 秒；进度通知会重置计时。
    pub timeout_secs: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiMcpPresetTransport {
    Http {
        url: &'static str,
        /// OAuth 动态注册时报的客户端名。部分服务只接受它认识的客户端。
        oauth_client_name: Option<&'static str>,
    },
    Stdio {
        command: &'static str,
        args: &'static [&'static str],
    },
}

impl PiMcpPresetTransport {
    pub fn needs_sign_in(&self) -> bool {
        matches!(self, Self::Http { .. })
    }
}

pub const PI_MCP_PRESETS: &[PiMcpPreset] = &[
    // Figma 远程 MCP 只接受已知客户端注册，Pi 文档给的做法就是以 Claude Code 的名义注册。
    // 工具不多且常用，直接声明给模型（`direct`）。
    PiMcpPreset {
        id: "figma",
        name: "Figma",
        summary: "读取 Figma 设计稿、组件和变量，按设计生成代码。用 Figma 账号在浏览器里授权。",
        description: "Figma designs: read frames, components, variables and screenshots from Figma links, and generate code that matches the design",
        transport: PiMcpPresetTransport::Http {
            url: "https://mcp.figma.com/mcp",
            oauth_client_name: Some("Claude Code"),
        },
        exposure: Some("direct"),
        timeout_secs: None,
    },
    // REA（github.com/morluto/rea）本机逆向分析。用 Smelt 自带的 Bun 跑它的 package
    // runner（`bun x`），不要求用户装 Node、也不全局安装：包只进 Bun 的下载缓存。
    // `--bun` 必不可少：rea 的入口脚本是 node shebang，不加 Bun 会转交给系统 node。
    // 持久注册必须钉死版本号。207 个工具、3MB 多的 schema，绝不能直接声明给模型，
    // 留在默认的 codemode 里按需发现。分析可能跑很久，超时放宽到它给 Kimi Code 配的 120 秒。
    PiMcpPreset {
        id: "rea",
        name: "REA 逆向分析",
        summary: "在本机拆解应用、二进制、网页和抓包，带证据讲清某个功能是怎么实现的。用 Smelt 自带的 Bun 运行，不用装 Node；首次连接会下载约 25MB。",
        description: "REA (Reverse Engineer Anything): local, evidence-backed reverse engineering of desktop apps, binaries, Electron/JavaScript bundles, websites and traffic captures",
        transport: PiMcpPresetTransport::Stdio {
            command: crate::managed_runtime::MANAGED_BUN_LAUNCHER_HOME_PATH,
            args: &["x", "--bun", "rea-agents@6.5.0", "mcp"],
        },
        exposure: None,
        timeout_secs: Some(120),
    },
    // Agent360 Browser MCP（github.com/Agent360dk/browser-mcp）驱动用户已登录的 Chrome，
    // 另需装它的 Chrome 扩展。和 REA 一样用自带 Bun 的 `bun x --bun`、钉死版本；入口
    // 不带子命令就是启动 MCP 服务器。服务器名沿用上游自己注册的 `browser-mcp`，用户按上游
    // 文档加过的话会撞名被拒，不会出现两份。40 个工具、四十多 KB schema，直接声明太占
    // 上下文；但截图、点击是一步一步交互的，codemode 脚本不合适，所以用 `deferred`：
    // 按需检索后直接调用。
    PiMcpPreset {
        id: "browser-mcp",
        name: "Agent360 浏览器",
        summary: "操作你已登录的 Chrome：打开网页、点击填表、截图，遇到验证码或二次验证会停下来问你。需先在 Chrome 应用商店安装 Agent360 Browser MCP 扩展。用 Smelt 自带的 Bun 运行。",
        description: "Browser automation in the user's own signed-in Chrome: navigate, click, fill forms, read pages and take screenshots, and ask the user for 2FA codes or CAPTCHAs",
        transport: PiMcpPresetTransport::Stdio {
            command: crate::managed_runtime::MANAGED_BUN_LAUNCHER_HOME_PATH,
            args: &["x", "--bun", "@agent360/browser-mcp@1.30.2"],
        },
        exposure: Some("deferred"),
        timeout_secs: None,
    },
];

impl PiMcpPreset {
    fn config(&self) -> Value {
        let mut entry = Map::new();
        match self.transport {
            PiMcpPresetTransport::Http {
                url,
                oauth_client_name,
            } => {
                entry.insert("url".into(), Value::String(url.into()));
                if let Some(client_name) = oauth_client_name {
                    let mut oauth = Map::new();
                    oauth.insert("clientName".into(), Value::String(client_name.into()));
                    entry.insert("oauth".into(), Value::Object(oauth));
                }
            }
            PiMcpPresetTransport::Stdio { command, args } => {
                entry.insert("command".into(), Value::String(command.into()));
                entry.insert(
                    "args".into(),
                    Value::Array(
                        args.iter()
                            .map(|arg| Value::String((*arg).into()))
                            .collect(),
                    ),
                );
            }
        }
        entry.insert("description".into(), Value::String(self.description.into()));
        if let Some(exposure) = self.exposure {
            entry.insert("exposure".into(), Value::String(exposure.into()));
        }
        if let Some(timeout) = self.timeout_secs {
            entry.insert("timeout".into(), Value::from(timeout));
        }
        Value::Object(entry)
    }
}

pub fn find_preset(id: &str) -> Option<&'static PiMcpPreset> {
    PI_MCP_PRESETS.iter().find(|preset| preset.id == id)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiMcpTransport {
    Http {
        url: String,
    },
    Stdio {
        command: String,
        args: Vec<String>,
    },
    /// 只覆盖开关、没有 `command` / `url` 的条目（Pi 允许项目级这样写），或写错了的条目。
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiMcpServer {
    pub name: String,
    pub transport: PiMcpTransport,
    pub enabled: bool,
    pub description: Option<String>,
    /// `mcp-auth.json` 里有这台服务器的令牌。只用于显示：令牌可能已经过期，真正能不能连
    /// 以会话里的连接结果为准，「登录」本身也会先试一次连接。
    pub signed_in: bool,
}

impl PiMcpServer {
    pub fn is_http(&self) -> bool {
        matches!(self.transport, PiMcpTransport::Http { .. })
    }
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
            let transport = match (text("url"), text("command")) {
                (Some(url), _) => PiMcpTransport::Http { url },
                (None, Some(command)) => PiMcpTransport::Stdio {
                    command,
                    args: entry
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|args| {
                            args.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                },
                (None, None) => PiMcpTransport::Unknown,
            };
            let signed_in = match &transport {
                PiMcpTransport::Http { url } => signed_in.contains(&normalize_url(url)),
                _ => false,
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

pub fn add_pi_mcp_preset(id: &str) -> Result<(), String> {
    add_pi_mcp_preset_at(&pi_agent_dir(), id)
}

pub fn add_pi_mcp_preset_at(agent_dir: &Path, id: &str) -> Result<(), String> {
    let preset = find_preset(id).ok_or_else(|| format!("没有内置的 MCP 服务器 {id}"))?;
    edit_servers(agent_dir, |servers| {
        if servers.contains_key(preset.id) {
            return Err(format!("mcp.json 里已经有名为 {} 的服务器", preset.id));
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
            .ok_or_else(|| format!("mcp.json 里没有服务器 {name}"))?;
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
            .ok_or_else(|| format!("mcp.json 里没有服务器 {name}"))
    })
}

/// 读 → 改 `mcpServers` → 写回。文件里其他顶层键（如 `autoEnableCodemode`）原样保留。
fn edit_servers(
    agent_dir: &Path,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> Result<(), String> {
    let path = agent_dir.join("mcp.json");
    let mut doc = read_or_create_json(&path)?;
    let root = doc
        .as_object_mut()
        .ok_or_else(|| format!("{} 的顶层不是对象", path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{} 里的 mcpServers 不是对象", path.display()))?;
    edit(servers)?;
    write_json_file(&path, &doc, false)
}

/// 跑 `pi mcp login <name>`：Pi 先试连接，需要授权时打开浏览器，等回环回调写好令牌。
/// 阻塞到结束，调用方放后台线程。成功返回 Pi 的最后一行输出。
pub fn login_pi_mcp_server(name: &str, status: &dyn Fn(&str)) -> Result<String, String> {
    let (runtime, script) = crate::managed_runtime::sync_managed_pi_tool(MCP_SCRIPT, status)?;
    let mut command = std::process::Command::new(&runtime.bun);
    command
        .arg(&script)
        .args(["login", name, "--timeout", &LOGIN_TIMEOUT_SECS.to_string()])
        .current_dir(
            script
                .parent()
                .and_then(|src| src.parent())
                .unwrap_or(&script),
        )
        .stdin(std::process::Stdio::null());
    runtime.inherit_into(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("启动 Pi MCP 助手失败：{error}"))?;
    let last_lines = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .last()
            .map(str::to_string)
    };
    if output.status.success() {
        Ok(last_lines(&output.stdout).unwrap_or_else(|| format!("已登录 {name}")))
    } else {
        Err(last_lines(&output.stderr)
            .or_else(|| last_lines(&output.stdout))
            .unwrap_or_else(|| format!("登录 {name} 失败")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("smelt-pi-mcp-{}", uuid::Uuid::new_v4()));
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

        add_pi_mcp_preset_at(&dir, "figma").unwrap();

        let doc = read(&dir);
        assert_eq!(doc["autoEnableCodemode"], Value::Bool(false));
        assert_eq!(doc["mcpServers"]["docs"]["url"], "https://example.com/mcp");
        let figma = &doc["mcpServers"]["figma"];
        assert_eq!(figma["url"], "https://mcp.figma.com/mcp");
        assert_eq!(figma["oauth"]["clientName"], "Claude Code");
        assert_eq!(figma["exposure"], "direct");
        assert!(
            add_pi_mcp_preset_at(&dir, "figma").is_err(),
            "重复添加会覆盖用户改过的条目，必须拒绝"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adding_rea_writes_a_pinned_stdio_entry() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "rea").unwrap();

        let rea = &read(&dir)["mcpServers"]["rea"];
        assert_eq!(rea["command"], "~/.smelt/runtime/bin/bun");
        assert_eq!(
            rea["args"],
            serde_json::json!(["x", "--bun", "rea-agents@6.5.0", "mcp"]),
            "持久注册必须钉死版本，不能用 @latest；--bun 防止转交给系统 node"
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
                command: "~/.smelt/runtime/bin/bun".into(),
                args: vec![
                    "x".into(),
                    "--bun".into(),
                    "rea-agents@6.5.0".into(),
                    "mcp".into(),
                ],
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn adding_agent360_browser_writes_a_pinned_deferred_stdio_entry() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "browser-mcp").unwrap();

        let browser = &read(&dir)["mcpServers"]["browser-mcp"];
        assert_eq!(browser["command"], "~/.smelt/runtime/bin/bun");
        assert_eq!(
            browser["args"],
            serde_json::json!(["x", "--bun", "@agent360/browser-mcp@1.30.2"]),
            "钉死版本，不带子命令才会启动服务器；--bun 防止转交给系统 node"
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
        add_pi_mcp_preset_at(&dir, "figma").unwrap();

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

    #[test]
    fn auth_keys_match_after_url_normalization() {
        let auth = serde_json::json!({ "https://example.com/": { "tokens": {} } });
        let urls = signed_in_urls(&auth);
        assert!(urls.contains(&normalize_url("https://example.com")));
    }
}
