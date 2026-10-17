//! Pi 的 MCP 服务器数据和内置预设。读写 `mcp.json`、登录在运行时。

use serde_json::{Map, Value};

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
            command: crate::agent_paths::MANAGED_BUN_LAUNCHER_HOME_PATH,
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
            command: crate::agent_paths::MANAGED_BUN_LAUNCHER_HOME_PATH,
            args: &["x", "--bun", "@agent360/browser-mcp@1.30.2"],
        },
        exposure: Some("deferred"),
        timeout_secs: None,
    },
];

impl PiMcpPreset {
    pub fn config(&self) -> Value {
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
