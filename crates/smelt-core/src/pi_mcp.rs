//! Pi 原生 MCP 服务器配置：`~/.pi/agent/mcp.json`。
//!
//! Pi 1.1 起自带 MCP（`builtin:mcp`），会话启动时读这个文件、连上里面启用的服务器。
//! Smelt 只做配置界面：增删、启停、登录，格式和 Pi 文档（`docs/mcp.md`）一致，写回时
//! 保留文件里与本次操作无关的内容。会话里怎么连、怎么调用都交给 Pi。
//!
//! 登录走 `smelt-pi mcp login <name>`：OAuth 流程
//! （动态注册、PKCE、回环回调、令牌落盘到 `mcp-auth.json`）全在 Pi 里，这里不重写。

pub use smelt_model::pi_mcp::*;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use smelt_model::redact::{redact_args, redact_secrets};

use crate::pi_model_settings::{pi_agent_dir, read_or_create_json, write_json_file};

/// 登录最多等浏览器这么久，和 `pi mcp login` 的默认值一致。
const LOGIN_TIMEOUT_SECS: u64 = 300;

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
    let runtime = crate::managed_runtime::sync_managed_pi_agent(status)?;
    let mut command = runtime.command(Some("mcp"));
    command
        .args(["login", name, "--timeout", &LOGIN_TIMEOUT_SECS.to_string()])
        .stdin(std::process::Stdio::null());
    let output = command
        .output()
        .map_err(|error| format!("启动 Pi MCP 助手失败：{error}"))?;
    let last_lines = |bytes: &[u8]| last_output_line(&String::from_utf8_lossy(bytes));
    if output.status.success() {
        Ok(last_lines(&output.stdout).unwrap_or_else(|| format!("已登录 {name}")))
    } else {
        Err(last_lines(&output.stderr)
            .or_else(|| last_lines(&output.stdout))
            .unwrap_or_else(|| format!("登录 {name} 失败")))
    }
}

/// 助手输出的最后一个非空行，要显示在界面上：OAuth 出错时这一行可能带着令牌端点的
/// 错误体或带凭据的地址，先打码。
fn last_output_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(redact_secrets)
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
    fn adding_playwright_writes_a_pinned_deferred_stdio_entry() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "playwright").unwrap();

        let playwright = &read(&dir)["mcpServers"]["playwright"];
        assert_eq!(playwright["command"], "~/.smelt/runtime/bin/bun");
        assert_eq!(
            playwright["args"],
            serde_json::json!(["x", "--bun", "@playwright/mcp@0.0.83", "--extension"]),
            "钉死版本，不能用 @latest；--bun 防止转交给系统 node；--extension 接用户已登录的 Chrome"
        );
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
    fn adding_desktop_chrome_connects_to_the_local_debugging_port() {
        let dir = temp_dir();
        add_pi_mcp_preset_at(&dir, "desktop-chrome").unwrap();

        let chrome = &read(&dir)["mcpServers"]["desktop-chrome"];
        assert_eq!(chrome["command"], "~/.smelt/runtime/bin/bun");
        assert_eq!(
            chrome["args"],
            serde_json::json!([
                "x",
                "--bun",
                "@playwright/mcp@0.0.83",
                "--cdp-endpoint",
                "http://127.0.0.1:9222"
            ]),
            "接 smelt-chrome 只绑本机的调试端口，和人在远程桌面里看的是同一个 Chrome"
        );
        assert_eq!(chrome["exposure"], "deferred");
        assert!(
            find_preset("desktop-chrome")
                .unwrap()
                .extension_url
                .is_none(),
            "走调试端口，不需要扩展"
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

    #[test]
    fn login_output_line_is_the_last_non_empty_one_and_redacted() {
        assert_eq!(
            last_output_line(
                "Opening browser…\nError: token endpoint https://auth.example.com/token?client_secret=s1 returned {\"access_token\":\"a1\"}\n\n"
            )
            .as_deref(),
            Some(
                "Error: token endpoint https://auth.example.com/token?client_secret=*** returned {\"access_token\":\"***\"}"
            )
        );
        assert_eq!(last_output_line(" \n\n"), None);
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
