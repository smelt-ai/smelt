//! Pi 的 MCP 服务器数据和内置预设。读写 `mcp.json`、登录在运行时。

use serde_json::{Map, Value};

/// 内置的一键添加项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PiMcpPreset {
    /// 写进 `mcpServers` 的服务器名，也是工具名前缀 `mcp__<id>__`。
    pub id: &'static str,
    /// 给人看的名字和一句话，按界面语言说（[`Self::name`]、[`Self::summary`]）。
    pub text: PiMcpPresetText,
    /// 写进配置的 `description`：Pi 把它放进系统提示，模型据此判断何时用这组工具。
    pub description: &'static str,
    pub transport: PiMcpPresetTransport,
    /// 不写就是 Pi 的默认 `codemode`：工具不直接声明给模型，靠 codemode 脚本发现和调用。
    pub exposure: Option<&'static str>,
    /// 单次请求超时（秒）。不写用 Pi 的默认 60 秒；进度通知会重置计时。
    pub timeout_secs: Option<u64>,
    /// 要配套安装的 Chrome 扩展的应用商店页，能力库里给个按钮直接打开。
    pub extension_url: Option<&'static str>,
    /// 装了远程桌面的机器上换用这一套（同一个 `id`、`name`），见 [`Self::for_machine`]。
    pub on_remote_desktop: Option<&'static PiMcpPreset>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiMcpPresetTransport {
    Http {
        url: &'static str,
        /// OAuth 动态注册时报的客户端名。部分服务只接受它认识的客户端。
        oauth_client_name: Option<&'static str>,
    },
    /// 用 smelt-agent 自带的 Bun 跑：`smelt-agent <args>`，带上 `BUN_BE_BUN=1`。
    Bun {
        args: &'static [&'static str],
        env: &'static [(&'static str, &'static str)],
    },
    /// 编进 smelt-agent 的服务器：`smelt-agent mcp-server <预设 id> <args>`。包和版本由
    /// `agent/package.json` 钉死，随 smelt-agent 发布，不在运行时下载。
    SmeltAgent { args: &'static [&'static str] },
}

/// 预设给人看的名字和说明归哪一条。远程桌面那套名字一样、说明不同。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiMcpPresetText {
    Figma,
    Rea,
    BrowserMcp,
    Playwright,
    ChromeDevtools,
    PlaywrightOnRemoteDesktop,
    ChromeDevtoolsOnRemoteDesktop,
}

impl PiMcpPreset {
    /// 按当前界面语言说的名字。
    pub fn name(&self) -> String {
        use PiMcpPresetText as T;
        match self.text {
            T::Figma => rust_i18n::t!("mcp_preset.figma.name"),
            T::Rea => rust_i18n::t!("mcp_preset.rea.name"),
            T::BrowserMcp => rust_i18n::t!("mcp_preset.browser_mcp.name"),
            T::Playwright | T::PlaywrightOnRemoteDesktop => {
                rust_i18n::t!("mcp_preset.playwright.name")
            }
            T::ChromeDevtools | T::ChromeDevtoolsOnRemoteDesktop => {
                rust_i18n::t!("mcp_preset.chrome_devtools.name")
            }
        }
        .into_owned()
    }

    /// 按当前界面语言说的一句话说明。
    pub fn summary(&self) -> String {
        use PiMcpPresetText as T;
        match self.text {
            T::Figma => rust_i18n::t!("mcp_preset.figma.summary"),
            T::Rea => rust_i18n::t!("mcp_preset.rea.summary"),
            T::BrowserMcp => rust_i18n::t!("mcp_preset.browser_mcp.summary"),
            T::Playwright => rust_i18n::t!("mcp_preset.playwright.summary"),
            T::ChromeDevtools => rust_i18n::t!("mcp_preset.chrome_devtools.summary"),
            T::PlaywrightOnRemoteDesktop => {
                rust_i18n::t!("mcp_preset.playwright.summary_remote_desktop")
            }
            T::ChromeDevtoolsOnRemoteDesktop => {
                rust_i18n::t!("mcp_preset.chrome_devtools.summary_remote_desktop")
            }
        }
        .into_owned()
    }
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
        text: PiMcpPresetText::Figma,
        description: "Figma designs: read frames, components, variables and screenshots from Figma links, and generate code that matches the design",
        transport: PiMcpPresetTransport::Http {
            url: "https://mcp.figma.com/mcp",
            oauth_client_name: Some("Claude Code"),
        },
        exposure: Some("direct"),
        timeout_secs: None,
        extension_url: None,
        on_remote_desktop: None,
    },
    // REA（github.com/morluto/rea）本机逆向分析。用 Smelt 自带的 Bun 跑它的 package
    // runner（`bun x`），不要求用户装 Node、也不全局安装：包只进 Smelt 自己按版本分开的缓存
    // （`~/.smelt/cache/bun-x/<包@版本>`，见 `agent/src/pi-mcp-runtime.ts`），不碰用户的 `~/.bun`。
    // `--bun` 必不可少：rea 的入口脚本是 node shebang，不加 Bun 会转交给系统 node。
    // 持久注册必须钉死版本号。255 个工具、5MB 的 schema，绝不能直接声明给模型，
    // 留在默认的 codemode 里按需发现。分析可能跑很久，超时放宽到它给 Kimi Code 配的 120 秒。
    PiMcpPreset {
        id: "rea",
        text: PiMcpPresetText::Rea,
        description: "REA (Reverse Engineer Anything): local, evidence-backed reverse engineering of desktop apps, binaries, Electron/JavaScript bundles, websites and traffic captures",
        transport: PiMcpPresetTransport::Bun {
            // 6.8 起要求 Node 26.12.1 以上，内置 Bun 要自报这个版本才跑得起来。
            args: &["x", "--bun", "rea-agents@6.9.0", "mcp"],
            env: &[],
        },
        exposure: None,
        timeout_secs: Some(120),
        extension_url: None,
        on_remote_desktop: None,
    },
    // Agent360 Browser MCP（github.com/Agent360dk/browser-mcp）驱动用户已登录的 Chrome，
    // 另需装它的 Chrome 扩展。编进 smelt-agent；入口不带子命令就是启动 MCP 服务器，
    // 所以预设不带参数。服务器名沿用上游自己注册的 `browser-mcp`，用户按上游
    // 文档加过的话会撞名被拒，不会出现两份。40 个工具、四十多 KB schema，直接声明太占
    // 上下文；但截图、点击是一步一步交互的，codemode 脚本不合适，所以用 `deferred`：
    // 按需检索后直接调用。
    PiMcpPreset {
        id: "browser-mcp",
        text: PiMcpPresetText::BrowserMcp,
        description: "Browser automation in the user's own signed-in Chrome: navigate, click, fill forms, read pages and take screenshots, and ask the user for 2FA codes or CAPTCHAs",
        transport: PiMcpPresetTransport::SmeltAgent { args: &[] },
        exposure: Some("deferred"),
        timeout_secs: None,
        extension_url: Some(
            "https://chromewebstore.google.com/detail/agent360-browser-mcp/jdehgalffmffhfhmmhaokfbfnafnmgcl",
        ),
        on_remote_desktop: None,
    },
    // Playwright MCP（github.com/microsoft/playwright-mcp）。同一个包，按这台机器有没有
    // 远程桌面换接法（见 `on_remote_desktop`）：没有就用 `--extension` 接用户正在用的
    // Chrome，沿用已有的登录状态，需装它的 Chrome 扩展。不加这个参数它会另开一份按项目目录
    // 区分的浏览器配置，换个项目就得重新登录。连接时扩展会弹页面让用户确认；跳过确认要的
    // token 每人不同，不写进预设。同样编进 smelt-agent。服务器名沿用上游示例的 `playwright`。
    // 25 个工具、二十来 KB schema，和 Agent360 一样是逐步交互的浏览器操作，用 `deferred`。
    PiMcpPreset {
        id: "playwright",
        text: PiMcpPresetText::Playwright,
        description: "Browser automation with Playwright in the user's own signed-in Chrome via the Playwright extension: navigate, click, type, fill forms, read accessibility snapshots, take screenshots and inspect console and network",
        transport: PiMcpPresetTransport::SmeltAgent {
            args: &["--extension"],
        },
        exposure: Some("deferred"),
        timeout_secs: None,
        extension_url: Some(
            "https://chromewebstore.google.com/detail/playwright-extension/mmlmfjhmonkocbjadbfplnigmagldckm",
        ),
        on_remote_desktop: Some(&PLAYWRIGHT_ON_REMOTE_DESKTOP),
    },
    // Chrome DevTools MCP 专注网页调试；上游不支持 Bun，这里按用户选择尝试运行，
    // 兼容性需要实际验证。本机接已登录的 Chrome，远程桌面接常驻 Chrome 的调试端口。
    // 关闭统计、CrUX 查询和周期更新检查；工具较多且逐步交互，按需声明。
    PiMcpPreset {
        id: "chrome-devtools",
        text: PiMcpPresetText::ChromeDevtools,
        description: "Chrome DevTools: inspect network requests and console messages, analyze page performance and memory, and automate or debug pages in the user's running Chrome",
        transport: PiMcpPresetTransport::Bun {
            args: &[
                "x",
                "--bun",
                "chrome-devtools-mcp@1.13.0",
                "--auto-connect",
                "--memory-debugging",
                "--no-usage-statistics",
                "--no-performance-crux",
            ],
            env: &[("CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS", "1")],
        },
        exposure: Some("deferred"),
        timeout_secs: Some(120),
        extension_url: None,
        on_remote_desktop: Some(&CHROME_DEVTOOLS_ON_REMOTE_DESKTOP),
    },
];

// 远程桌面（设计文档 D9）里常驻的 Chrome：调试端口只绑本机 9222，Playwright MCP 用
// `--cdp-endpoint` 直接接上，不要扩展、也没有连接确认。人在远程桌面里看的是同一个
// 浏览器，遇到验证码、付款随时接管。`install.sh --with-desktop` 装好桌面后自动加上。
const PLAYWRIGHT_ON_REMOTE_DESKTOP: PiMcpPreset = PiMcpPreset {
    id: "playwright",
    text: PiMcpPresetText::PlaywrightOnRemoteDesktop,
    description: "Browser automation with Playwright in the always-on Chrome on this machine's remote desktop, keeping its signed-in sessions: navigate, click, type, fill forms, read accessibility snapshots and take screenshots. A person can watch and take over the same browser from the remote desktop",
    transport: PiMcpPresetTransport::SmeltAgent {
        args: &["--cdp-endpoint", "http://127.0.0.1:9222"],
    },
    exposure: Some("deferred"),
    timeout_secs: None,
    extension_url: None,
    on_remote_desktop: None,
};

const CHROME_DEVTOOLS_ON_REMOTE_DESKTOP: PiMcpPreset = PiMcpPreset {
    id: "chrome-devtools",
    text: PiMcpPresetText::ChromeDevtoolsOnRemoteDesktop,
    description: "Chrome DevTools: inspect network requests and console messages, analyze page performance and memory, and automate or debug pages in the remote desktop Chrome",
    transport: PiMcpPresetTransport::Bun {
        args: &[
            "x",
            "--bun",
            "chrome-devtools-mcp@1.13.0",
            "--browser-url=http://127.0.0.1:9222",
            "--memory-debugging",
            "--no-usage-statistics",
            "--no-performance-crux",
        ],
        env: &[("CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS", "1")],
    },
    exposure: Some("deferred"),
    timeout_secs: Some(120),
    extension_url: None,
    on_remote_desktop: None,
};

impl PiMcpPreset {
    /// 这台机器上实际用的那一套。`remote_desktop` 由运行时那台机器判断
    /// （`smelt_paths::remote_desktop_installed`），客户端用握手里报上来的同一个结果。
    pub fn for_machine(&'static self, remote_desktop: bool) -> &'static PiMcpPreset {
        match self.on_remote_desktop {
            Some(variant) if remote_desktop => variant,
            _ => self,
        }
    }

    /// `bun x` 跑的预设钉的那个包：`(包名, 包@版本)`。
    pub fn pinned_package(&self) -> Option<(&'static str, &'static str)> {
        let PiMcpPresetTransport::Bun { args, .. } = self.transport else {
            return None;
        };
        let spec = *args.iter().skip(1).find(|arg| !arg.starts_with('-'))?;
        let at = spec.rfind('@').filter(|&at| at > 0)?;
        Some((&spec[..at], spec))
    }

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
            PiMcpPresetTransport::Bun { args, env } => {
                entry.insert(
                    "command".into(),
                    Value::String(crate::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH.into()),
                );
                entry.insert(
                    "args".into(),
                    Value::Array(
                        args.iter()
                            .map(|arg| Value::String((*arg).into()))
                            .collect(),
                    ),
                );
                let (name, value) = crate::agent_paths::BUN_BE_BUN;
                let mut variables = Map::new();
                variables.insert(name.into(), Value::String(value.into()));
                for &(name, value) in env {
                    variables.insert(name.into(), Value::String(value.into()));
                }
                entry.insert("env".into(), Value::Object(variables));
            }
            PiMcpPresetTransport::SmeltAgent { args } => {
                entry.insert(
                    "command".into(),
                    Value::String(crate::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH.into()),
                );
                let args = [crate::agent_paths::SMELT_AGENT_MCP_SERVER, self.id]
                    .into_iter()
                    .chain(args.iter().copied())
                    .map(|arg| Value::String(arg.into()))
                    .collect();
                entry.insert("args".into(), Value::Array(args));
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

/// 给人看的连接方式：运行时读 `mcp.json` 时已用 [`crate::redact`] 打过码（地址里的
/// `?api_key=…`、命令行里的 `--token xxx` 之类），原样显示即可，不能拿来连接。
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
