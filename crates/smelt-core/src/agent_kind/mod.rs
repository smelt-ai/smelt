//! Agent 身份标识 + 手动添加的 workspace profile：ACP 对话与终端客户端的能力
//! 不完全重合，分别用 `ConversationAgentKind` / `TerminalAgentKind` 表达，避免只支持终端的
//! 客户端被误放进 ACP 对话菜单。放在 smelt-core 让 GUI 与 daemon 共用判断逻辑。

use std::collections::BTreeMap;

mod dsh;
pub use dsh::*;

/// 全自动任务的 ACP session config 参数映射。各家 agent 的参数名和行为不同，
/// 这里结构化描述，避免 controller/runtime 在外部重复写 provider match。
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskAgentParams {
    /// thinking level 参数名（Claude: "effort", Codex: "reasoning_effort"）。
    /// None 表示这家 agent 不支持此参数。
    pub thinking_key: Option<&'static str>,
    /// fast mode 参数名（Claude: "fast", Codex: "fast-mode"）。
    pub fast_mode_key: Option<&'static str>,
    /// 全权限模式值（Claude: "bypassPermissions", Codex: "agent-full-access"）。
    /// 无人值守任务需要跳过审批，各家 adapter 用不同的 mode 值表达。
    pub full_access_mode: Option<&'static str>,
}

/// 各家 CLI/TUI 把历史 session id 拼进启动命令的语法。`None` 表示没有可在终端
/// 里继续的 CLI（例如只有 ACP 桥的 dsh）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliResumeSyntax {
    /// `--resume {id}`
    Flag,
    /// `--resume={id}`
    FlagEquals,
    /// `resume {id}`（Codex 子命令）
    Subcommand,
    /// `--session {id}`
    SessionFlag,
    /// `--resume-id {id}`（Kiro）
    ResumeIdFlag,
    /// `--conversation {id}`（Antigravity `agy`）
    ConversationFlag,
}

impl CliResumeSyntax {
    pub fn suffix(self, quoted_id: &str) -> String {
        match self {
            Self::Flag => format!("--resume {quoted_id}"),
            Self::FlagEquals => format!("--resume={quoted_id}"),
            Self::Subcommand => format!("resume {quoted_id}"),
            Self::SessionFlag => format!("--session {quoted_id}"),
            Self::ResumeIdFlag => format!("--resume-id {quoted_id}"),
            Self::ConversationFlag => format!("--conversation {quoted_id}"),
        }
    }
}

/// 快捷终端 / 裸 CLI 如何注入 Smelt cross-agent MCP。ACP 会话另走 `session/new`
/// 的 mcpServers；这里只描述启动命令那条路。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalMcpInject {
    /// 没有会话级 CLI 参数，或不该改用户全局 MCP 配置。
    None,
    /// `--mcp-config '{"mcpServers":{...}}'`
    McpConfigJson,
    /// Codex：`-c mcp_servers.smelt={ toml }`
    CodexToml,
    /// Copilot：`--additional-mcp-config '{...}'`，JSON 带 tools/timeout。
    AdditionalMcpConfigJson,
    /// OpenCode：`OPENCODE_CONFIG_CONTENT='{...}'`。这是只对当前进程生效的
    /// 内联配置层，不写用户级或项目级 `opencode.json`。
    OpenCodeConfigContent,
}

/// 快捷终端如何把 CLI 的结构化生命周期接入 smeltd。全局 hook/plugin 由设置页
/// 管理；只有支持进程级扩展的 CLI 才需要在启动边界注入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalStatusBridge {
    None,
    /// `pi --extension <managed-extension>`，不修改用户的 `~/.pi` 配置。
    PiExtension,
}

/// 全自动任务启动时，除 session config 外还要改启动命令的方式。
/// Claude/Codex 走 ACP session config，这里是 None。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskCliPatch {
    #[default]
    None,
    /// 在命令末尾追加全权限 flag，以及可选的 `--flag value` 推理档位。
    Append {
        full_access: &'static str,
        /// 已有这些 flag 之一就视为已经全权限（Copilot 的 `--yolo`）。
        full_access_aliases: &'static [&'static str],
        thinking_flag: Option<&'static str>,
    },
    /// 插在 `marker` 后面（Grok：`grok agent`）。
    AfterMarker {
        marker: &'static str,
        full_access: &'static str,
        thinking_flag: Option<&'static str>,
    },
}

/// Smelt 曾经作为出厂默认值发布过的一版受管 ACP 适配器。
///
/// 包名和版本拆开存，而不是只留一条命令字符串：配置迁移需要还原旧默认命令，
/// Bun 缓存清理则需要精确定位旧包目录，两条路径必须共用同一份版本事实。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedAcpAdapterRelease {
    pub(crate) package: &'static str,
    pub(crate) version: &'static str,
}

impl ManagedAcpAdapterRelease {
    fn command(self) -> String {
        format!("bunx --bun {}@{}", self.package, self.version)
    }
}

/// 由 Smelt 通过受管 Bun 启动的 ACP 适配器版本策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ManagedAcpAdapter {
    current: ManagedAcpAdapterRelease,
    previous_releases: &'static [ManagedAcpAdapterRelease],
    /// 曾作为默认值发布、但不是 `bunx package@version` 形态的命令。
    legacy_default_commands: &'static [&'static str],
}

impl ManagedAcpAdapter {
    pub(crate) fn previous_releases(self) -> &'static [ManagedAcpAdapterRelease] {
        self.previous_releases
    }

    pub(crate) fn current_command(self) -> String {
        self.current.command()
    }

    /// 只认 Smelt 确实发布过的逐字默认值。带环境前缀、额外参数或换过运行器的
    /// 命令都属于用户自定义，不能借版本升级之名覆盖。
    fn should_upgrade_command(self, command: &str) -> bool {
        self.current.command() == command
            || self
                .previous_releases
                .iter()
                .any(|release| release.command() == command)
            || self.legacy_default_commands.contains(&command.trim())
    }

    pub(crate) fn current_release(self) -> ManagedAcpAdapterRelease {
        self.current
    }
}

const CLAUDE_ACP_ADAPTER: ManagedAcpAdapter = ManagedAcpAdapter {
    current: ManagedAcpAdapterRelease {
        package: "@agentclientprotocol/claude-agent-acp",
        version: "0.78.0",
    },
    previous_releases: &[
        ManagedAcpAdapterRelease {
            package: "@agentclientprotocol/claude-agent-acp",
            version: "0.59.0",
        },
        ManagedAcpAdapterRelease {
            package: "@agentclientprotocol/claude-agent-acp",
            version: "0.70.0",
        },
    ],
    legacy_default_commands: &[],
};

const CODEX_ACP_ADAPTER: ManagedAcpAdapter = ManagedAcpAdapter {
    current: ManagedAcpAdapterRelease {
        package: "@agentclientprotocol/codex-acp",
        version: "1.12.0",
    },
    previous_releases: &[
        ManagedAcpAdapterRelease {
            package: "@zed-industries/codex-acp",
            version: "0.16.0",
        },
        ManagedAcpAdapterRelease {
            package: "@agentclientprotocol/codex-acp",
            version: "1.1.7",
        },
        ManagedAcpAdapterRelease {
            package: "@agentclientprotocol/codex-acp",
            version: "1.6.2",
        },
    ],
    legacy_default_commands: &["codex app-server"],
};

// `pi-acp` 是 Smelt 旧版发布过的默认桥；保留这份元数据只为精确迁移旧设置、
// 清理旧缓存。当前 Pi 对话已经改走官方原生 RPC。
const PI_ACP_ADAPTER: ManagedAcpAdapter = ManagedAcpAdapter {
    current: ManagedAcpAdapterRelease {
        package: "pi-acp",
        version: "0.0.33",
    },
    previous_releases: &[],
    legacy_default_commands: &[],
};

/// Smelt 自带的 Pi 原生 RPC 逻辑入口。不要求用户 PATH 中真的存在同名
/// 程序；连接层会把它解析成受管 Bun + Pi 官方 RPC entry。
pub const SMELT_PI_AGENT_COMMAND: &str = "smelt-pi-agent";

/// Smelt 把产品 Agent 的长期指令交给内置 Pi 启动器时使用的私有环境变量。
/// 启动器会立刻消费并删除它，再转换成 Pi 原生的 `--append-system-prompt` 参数，
/// 因此工具子进程不会继承这段指令。
pub const SMELT_AGENT_INSTRUCTIONS_ENV: &str = "SMELT_AGENT_INSTRUCTIONS";

/// 产品 Agent 勾选的 Pi 插件启动参数，JSON 字符串数组。
///
/// 不直接拼进 `ConversationLaunchSpec::command`：那个字段按空白切词，技能路径里只要有
/// 空格就会被拆坏。走环境变量能原样保留路径，启动器消费后立刻删除，工具子进程
/// 不会继承。
pub const SMELT_AGENT_PLUGIN_ARGS_ENV: &str = "SMELT_AGENT_PLUGIN_ARGS";

/// 产品智能体绑的默认模型。启动器消费后立刻删除，工具子进程不会继承。
/// 成对出现才生效；只填一半等于没选。
pub const SMELT_AGENT_MODEL_PROVIDER_ENV: &str = "SMELT_AGENT_MODEL_PROVIDER";
pub const SMELT_AGENT_MODEL_ID_ENV: &str = "SMELT_AGENT_MODEL_ID";

/// 一家 ACP agent 的全部静态属性。**新增一种 agent = `CONVERSATION_AGENTS` 加一行 +
/// 枚举加一条变体**，命令、显示名、设置项、新建菜单都从这张表派生。
///
/// 判定标准：任何 `match kind { Claude => …, Grok => … }` 都是一处漏出去的
/// 能力，应该变成这里的一个字段（或一个函数指针），而不是散在调用方。
pub struct ConversationAgentDescriptor {
    /// 存档标识（稳定，别改）。
    pub id: &'static str,
    /// 给人看的 agent 名（会话标题、启动横幅、菜单项共用）。
    pub label: &'static str,
    /// 短名：会话标题这种窄地方用（「Copilot 对话 · smelt」）。
    pub short_label: &'static str,
    /// SQLite `json/agent_ui.json` 文档里存这家启动命令的键名。历史原因各不相同
    /// （Claude 就叫 `acp_cmd`），改名等于把用户自定义命令悄悄重置回默认，
    /// 所以键名是存档契约的一部分，跟着这张表走。
    pub config_key: &'static str,
    /// 这家 agent 收不收图片。Grok 的 `promptCapabilities.image = false`（实测，
    /// 见 `default_acp_grok_cmd` 的注释），是唯一不收图的。会话交接时据此决定
    /// 图片占位文案怎么写——目标收不了图还只说「未复制」，新会话会一直等一张
    /// 永远不会到的图。
    pub accepts_images: bool,
    /// 出厂启动命令。用函数指针而不是 `&'static str`：几家的默认命令都带着
    /// 版本锁和长注释，留在各自的函数里比塞进表里可读。
    pub default_cmd: fn() -> String,
    /// 曾经作为出厂默认值发布过的命令。设置迁移和 daemon relaunch 只升级这些
    /// 逐字默认值，用户自定义命令不动。受管 Bun 适配器的旧版本另走
    /// `managed_adapter`，不要把 bunx 包版本写进这里。
    pub previous_default_cmds: &'static [&'static str],
    /// 需要 Smelt 负责版本迁移与缓存回收的 Bun 适配器；原生 ACP CLI 为 None。
    pub(crate) managed_adapter: Option<ManagedAcpAdapter>,
    /// 只注入当前 agent 子进程的出厂环境覆盖。需要用配置而不是 CLI flag 表达的
    /// 能力放这里，不能写用户的全局配置文件；用户在设置里显式配置同名变量时覆盖它。
    pub default_env: &'static [(&'static str, &'static str)],
    /// Homebrew cask 名。Grok / Cursor 没有 cask，用 None。
    pub homebrew_cask: Option<&'static str>,
    /// npm 包名（用于 `npx` 或 `npm install -g`）。Cursor 走官方 install 脚本，没有 npm 包。
    pub npm_package: Option<&'static str>,
    /// 自动任务使用的 session config 参数映射。
    pub task_params: TaskAgentParams,
    /// 终端 CLI 续接历史会话的语法。`None` = 没有对应 CLI，调用方应藏入口。
    pub cli_resume: Option<CliResumeSyntax>,
    /// 能否作为裸种类出现在新建菜单 / 历史 tab / 启动命令设置里。
    /// `false` 表示必须走 workspace profile（dsh 原生 profile），不要再列一个空槽。
    pub bare_kind: bool,
    /// 终端 CLI 注入 cross-agent MCP 的方式。
    pub terminal_mcp_inject: TerminalMcpInject,
    /// ACP `session/new` 是否接受 stdio MCP。false 时必须走 CLI 参数（Copilot）。
    pub acp_accepts_session_mcp: bool,
    /// 全自动任务要改启动命令时的补丁；多数 adapter 只靠 session config。
    pub task_cli: TaskCliPatch,
}

/// ACP agent 注册表。顺序 = 新建菜单 / 设置页的排列顺序，且必须与
/// `ConversationAgentKind::ALL` 逐项对齐（`descriptor()` 直接按变体序号索引，
/// `descriptor_table_matches_enum_order` 守着这条不变量）。
pub static CONVERSATION_AGENTS: &[ConversationAgentDescriptor] = &[
    ConversationAgentDescriptor {
        id: "claude",
        label: "Claude Code",
        short_label: "Claude",
        config_key: "acp_cmd",
        accepts_images: true,
        default_cmd: default_acp_cmd,
        previous_default_cmds: &[],
        managed_adapter: Some(CLAUDE_ACP_ADAPTER),
        default_env: &[],
        homebrew_cask: Some("claude-code"),
        npm_package: Some("@anthropic-ai/claude-code"),
        task_params: TaskAgentParams {
            thinking_key: Some("effort"),
            fast_mode_key: Some("fast"),
            full_access_mode: Some("bypassPermissions"),
        },
        cli_resume: Some(CliResumeSyntax::Flag),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::McpConfigJson,
        acp_accepts_session_mcp: true,
        task_cli: TaskCliPatch::None,
    },
    ConversationAgentDescriptor {
        id: "copilot",
        label: "GitHub Copilot",
        short_label: "Copilot",
        config_key: "acp_copilot_cmd",
        accepts_images: true,
        default_cmd: default_acp_copilot_cmd,
        previous_default_cmds: &["copilot --acp"],
        managed_adapter: None,
        default_env: &[],
        homebrew_cask: Some("copilot-cli"),
        npm_package: Some("@github/copilot"),
        // Copilot 的 ACP 实现目前不公开这些参数
        task_params: TaskAgentParams {
            thinking_key: None,
            fast_mode_key: None,
            full_access_mode: None,
        },
        cli_resume: Some(CliResumeSyntax::FlagEquals),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::AdditionalMcpConfigJson,
        acp_accepts_session_mcp: false,
        task_cli: TaskCliPatch::Append {
            full_access: "--allow-all",
            full_access_aliases: &["--yolo"],
            thinking_flag: Some("--effort"),
        },
    },
    ConversationAgentDescriptor {
        id: "codex",
        label: "Codex",
        short_label: "Codex",
        config_key: "acp_codex_cmd",
        accepts_images: true,
        default_cmd: default_acp_codex_cmd,
        previous_default_cmds: &[],
        managed_adapter: Some(CODEX_ACP_ADAPTER),
        default_env: &[],
        homebrew_cask: Some("codex"),
        npm_package: Some("@openai/codex"),
        task_params: TaskAgentParams {
            thinking_key: Some("reasoning_effort"),
            fast_mode_key: Some("fast-mode"),
            full_access_mode: Some("agent-full-access"),
        },
        cli_resume: Some(CliResumeSyntax::Subcommand),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::CodexToml,
        acp_accepts_session_mcp: true,
        task_cli: TaskCliPatch::None,
    },
    ConversationAgentDescriptor {
        id: "grok",
        label: "Grok",
        short_label: "Grok",
        config_key: "acp_grok_cmd",
        accepts_images: false,
        default_cmd: default_acp_grok_cmd,
        previous_default_cmds: &["grok agent stdio"],
        managed_adapter: None,
        default_env: &[],
        homebrew_cask: None,
        npm_package: Some("@xai-official/grok"),
        // Grok 的 ACP 实现目前不公开这些参数
        task_params: TaskAgentParams {
            thinking_key: None,
            fast_mode_key: None,
            full_access_mode: None,
        },
        cli_resume: Some(CliResumeSyntax::Flag),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::None,
        acp_accepts_session_mcp: true,
        task_cli: TaskCliPatch::AfterMarker {
            marker: "grok agent",
            full_access: "--always-approve",
            thinking_flag: Some("--reasoning-effort"),
        },
    },
    ConversationAgentDescriptor {
        id: "cursor",
        label: "Cursor Agent",
        short_label: "Cursor",
        config_key: "acp_cursor_cmd",
        accepts_images: true,
        default_cmd: default_acp_cursor_cmd,
        previous_default_cmds: &["cursor-agent acp"],
        managed_adapter: None,
        default_env: &[],
        homebrew_cask: None,
        npm_package: None,
        // Cursor ACP 目前不公开无人值守任务所需的这些 session config 参数
        task_params: TaskAgentParams {
            thinking_key: None,
            fast_mode_key: None,
            full_access_mode: None,
        },
        cli_resume: Some(CliResumeSyntax::Flag),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::None,
        acp_accepts_session_mcp: true,
        // `--force` / `--yolo` 是顶层 flag，必须插在 `acp` 子命令前，否则
        // 否则无人值守任务会停在 ACP 审批卡上。
        task_cli: TaskCliPatch::AfterMarker {
            marker: "cursor-agent",
            full_access: "--force",
            thinking_flag: None,
        },
    },
    ConversationAgentDescriptor {
        id: "opencode",
        label: "OpenCode",
        short_label: "OpenCode",
        config_key: "acp_opencode_cmd",
        accepts_images: true,
        default_cmd: default_acp_opencode_cmd,
        previous_default_cmds: &[],
        managed_adapter: None,
        // `--auto` 只属于 TUI，ACP 子命令会拒绝它。OpenCode 的配置层接受
        // `permission: "allow"` 并归一为 `{"*":"allow"}`，用进程环境覆盖即可
        // 获得相同的免审批语义，同时不改用户级/项目级 opencode.json。
        default_env: &[("OPENCODE_CONFIG_CONTENT", r#"{"permission":"allow"}"#)],
        homebrew_cask: None,
        npm_package: Some("opencode-ai"),
        // OpenCode ACP 目前不公开无人值守任务所需的这些 session config 参数
        task_params: TaskAgentParams {
            thinking_key: None,
            fast_mode_key: None,
            full_access_mode: None,
        },
        cli_resume: Some(CliResumeSyntax::SessionFlag),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::OpenCodeConfigContent,
        acp_accepts_session_mcp: true,
        // `--auto` 只属于 TUI 默认命令：`opencode --auto acp` 会把 `acp` 当项目名，
        // `opencode acp --auto` 会被 yargs 拒绝。全权限由上面的进程级配置表达，
        // 这里不再给命令行叠一个无效 flag。
        task_cli: TaskCliPatch::None,
    },
    ConversationAgentDescriptor {
        id: "kiro",
        label: "Kiro",
        short_label: "Kiro",
        config_key: "acp_kiro_cmd",
        accepts_images: true,
        default_cmd: default_acp_kiro_cmd,
        previous_default_cmds: &[
            "kiro-cli acp",
            "kiro-cli acp --trust-all-tools",
            "kiro-cli acp --agent-engine v3 --auth-method cli --trust-all-tools",
        ],
        managed_adapter: None,
        default_env: &[],
        homebrew_cask: Some("kiro-cli"),
        npm_package: None,
        // Kiro 的 thinking 走 CLI `--effort`，不是 ACP session config。Terminal 可用
        // `--trust-all-tools`；v3 ACP 拒绝 trust flags，权限改走标准 ACP 请求。
        task_params: TaskAgentParams {
            thinking_key: None,
            fast_mode_key: None,
            full_access_mode: None,
        },
        cli_resume: Some(CliResumeSyntax::ResumeIdFlag),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::None,
        acp_accepts_session_mcp: true,
        task_cli: TaskCliPatch::Append {
            full_access: "--trust-all-tools",
            full_access_aliases: &["-a"],
            thinking_flag: Some("--effort"),
        },
    },
    ConversationAgentDescriptor {
        id: "pi",
        label: "Pi",
        short_label: "Pi",
        config_key: "acp_pi_cmd",
        accepts_images: true,
        default_cmd: default_acp_pi_cmd,
        previous_default_cmds: &[],
        managed_adapter: Some(PI_ACP_ADAPTER),
        default_env: &[],
        homebrew_cask: None,
        npm_package: Some("@earendil-works/pi-coding-agent"),
        // 对话由 Smelt 直接驱动 Pi 官方 JSONL RPC；终端入口仍是 Pi CLI。
        // Pi 不接受 ACP session/new 的 MCP 参数；这里保持 true 是为了阻止上层
        // 错把 Copilot 专用 `--additional-mcp-config` 参数注入 Pi。
        task_params: TaskAgentParams {
            thinking_key: Some("thought_level"),
            fast_mode_key: None,
            full_access_mode: Some("bypassPermissions"),
        },
        cli_resume: Some(CliResumeSyntax::SessionFlag),
        bare_kind: true,
        terminal_mcp_inject: TerminalMcpInject::None,
        acp_accepts_session_mcp: true,
        task_cli: TaskCliPatch::None,
    },
    ConversationAgentDescriptor {
        id: "dsh",
        label: "DeepSeek Harness",
        short_label: "DeepSeek",
        config_key: "acp_dsh_cmd",
        // 官方 `@deepseek-ai/dsh-acp` 是 automation-only 的，明说不上图片、不上
        // 工具呈现；smelt 走的是自己写的 `@smelt-ai/dsh-acp-rich` 桥，
        // `promptCapabilities.image` 由它按 `ctx.attachments` 是否composed 如实
        // 上报。参考 profile 里 attachments 是有的，所以这里是 true。
        accepts_images: true,
        default_cmd: default_acp_dsh_cmd,
        previous_default_cmds: &[],
        managed_adapter: None,
        default_env: &[],
        homebrew_cask: None,
        npm_package: None,
        // dsh 的 ACP 面还没有 thinking / fast / 全权限这几个 session config 参数：
        // 推理强度在 profile 的 `llm-deepseek` 里配，权限在 `sandbox-policy` 里配，
        // 都是启动期的事，不是每轮可切的 session config。
        task_params: TaskAgentParams {
            thinking_key: None,
            fast_mode_key: None,
            full_access_mode: None,
        },
        cli_resume: None,
        bare_kind: false,
        terminal_mcp_inject: TerminalMcpInject::None,
        acp_accepts_session_mcp: true,
        task_cli: TaskCliPatch::None,
    },
];

/// 按存档标识查描述符；认不出返回 None。配置层拿它把「命令存在哪个键」
/// 这类问题也变成查表。
pub fn conversation_descriptor(id: &str) -> Option<&'static ConversationAgentDescriptor> {
    CONVERSATION_AGENTS.iter().find(|d| d.id == id)
}

/// ACP 会话可接的 agent 种类。属性一律查 `CONVERSATION_AGENTS`，这里只留身份。
///
/// 序列化用 `id()` 那串小写标识（存进工作区快照的 ACP 会话存档），不用
/// serde 派生——枚举变体名将来改了不该炸存档。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConversationAgentKind {
    Claude,
    Copilot,
    Codex,
    Grok,
    Cursor,
    OpenCode,
    Kiro,
    Pi,
    Dsh,
}

impl ConversationAgentKind {
    /// 新建菜单 / 设置页的排列顺序。
    pub const ALL: [Self; 9] = [
        Self::Claude,
        Self::Copilot,
        Self::Codex,
        Self::Grok,
        Self::Cursor,
        Self::OpenCode,
        Self::Kiro,
        Self::Pi,
        Self::Dsh,
    ];

    /// 这一家的静态属性。变体序号即表下标，见 `CONVERSATION_AGENTS` 的顺序约定。
    pub fn descriptor(self) -> &'static ConversationAgentDescriptor {
        &CONVERSATION_AGENTS[self as usize]
    }

    /// 存档标识（稳定，别改）。
    pub fn id(self) -> &'static str {
        self.descriptor().id
    }

    /// 存档标识 → 种类；认不出（旧存档 / 手改坏了）返回 None，调用方自己兜底。
    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == s)
    }

    /// 给人看的 agent 名（会话标题、启动横幅、菜单项共用）。
    pub fn label(self) -> &'static str {
        self.descriptor().label
    }

    /// 短名：会话标题这种窄地方用（「Copilot 对话 · smelt」）。
    pub fn short_label(self) -> &'static str {
        self.descriptor().short_label
    }

    /// 这家 agent 收不收图片。
    pub fn accepts_images(self) -> bool {
        self.descriptor().accepts_images
    }

    /// 出厂启动命令。
    pub fn default_cmd(self) -> String {
        (self.descriptor().default_cmd)()
    }

    /// Smelt 负责锁版本、迁移旧默认值和回收缓存的 ACP 适配器元数据。
    pub(crate) fn managed_adapter(self) -> Option<ManagedAcpAdapter> {
        self.descriptor().managed_adapter
    }

    /// 旧设置、历史会话和 daemon relaunch 共用的版本迁移入口。返回 None 表示
    /// 当前命令不是 Smelt 发布过的旧默认值（包括已经是最新版和用户自定义）。
    pub fn upgrade_released_default_command(self, command: &str) -> Option<String> {
        let current = self.default_cmd();
        let trimmed = command.trim();
        if trimmed == current {
            return None;
        }
        let from_adapter = self
            .managed_adapter()
            .is_some_and(|adapter| adapter.should_upgrade_command(command));
        let from_factory = self.descriptor().previous_default_cmds.contains(&trimmed);
        (from_adapter || from_factory).then_some(current)
    }

    /// 出厂启动规格。命令和只对当前 agent 生效的环境覆盖必须从同一注册项派生，
    /// 否则桌面新建、移动端新建、profile 恢复很容易只在其中一条路径漏权限。
    pub fn default_launch(self) -> ConversationLaunchSpec {
        let mut launch = ConversationLaunchSpec::from_command(self.default_cmd());
        for (name, value) in self.descriptor().default_env {
            launch.env.insert((*name).into(), (*value).into());
        }
        launch
    }

    /// Homebrew cask 名（Grok 没有 cask，返回 None）。
    pub fn homebrew_cask(self) -> Option<&'static str> {
        self.descriptor().homebrew_cask
    }

    /// npm 包名。Cursor 没有官方 npm 包，返回 None。
    pub fn npm_package(self) -> Option<&'static str> {
        self.descriptor().npm_package
    }

    /// 全自动任务的 session config 参数映射。
    pub fn task_params(self) -> &'static TaskAgentParams {
        &self.descriptor().task_params
    }

    /// 终端 CLI 续接语法。`None` 表示没有可在终端里继续的 CLI。
    pub fn cli_resume_syntax(self) -> Option<CliResumeSyntax> {
        self.descriptor().cli_resume
    }

    /// 能否作为裸种类出现在新建菜单 / 历史 tab / 启动命令设置里。
    pub fn is_bare_kind(self) -> bool {
        self.descriptor().bare_kind
    }

    /// 侧栏/菜单用的本地 SVG 资源路径，约定 `smelt-icons/agent-<id>.svg`。
    pub fn icon_asset(self) -> String {
        format!("smelt-icons/agent-{}.svg", self.id())
    }

    /// 快捷终端注入 cross-agent MCP 的方式。
    pub fn terminal_mcp_inject(self) -> TerminalMcpInject {
        self.descriptor().terminal_mcp_inject
    }

    /// ACP `session/new` 是否接受 stdio MCP。
    pub fn acp_accepts_session_mcp(self) -> bool {
        self.descriptor().acp_accepts_session_mcp
    }

    /// 全自动任务对启动命令的最小权限补丁。
    pub fn task_cli(self) -> TaskCliPatch {
        self.descriptor().task_cli
    }

    /// 能否注册为一个本机无人值守 ACP runtime。
    ///
    /// 只要是裸种类且本机有对应 CLI 就可报；DSH 必须绑 native profile，
    /// 不能注册一条空的 `dsh` runtime。Antigravity 只有 TUI、不在这张 ACP 表里。
    pub fn supports_headless_runtime(self) -> bool {
        self.is_bare_kind() && self.terminal().is_some()
    }

    /// 探测本机是否已装、并作为 `RuntimeSpec.type` 上报时用的 CLI 文件名。
    pub fn headless_runtime_cli(self) -> Option<&'static str> {
        self.supports_headless_runtime()
            .then(|| self.terminal().map(TerminalAgentKind::cli_program))
            .flatten()
    }

    /// 可供 controller 注册的本机 ACP 协议族，顺序与 `ALL` 一致。
    pub fn headless_runtime_kinds() -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(|kind| kind.supports_headless_runtime())
    }

    /// 从任意命令字符串宽松猜 agent 种类（忽略大小写、不认参数位置）。为保持
    /// 旧存档兼容，普通长度的标识仍使用包含匹配；短标识（Pi）改用边界匹配。
    /// 这样 `pi-acp` 仍能识别 Pi，`api-key` 或 `copilot` 却不会误分类。按 `ALL`
    /// 顺序取第一个命中；同时出现多家关键字这种边缘情况谁在前谁赢。
    pub fn from_command_loose(cmd: &str) -> Option<Self> {
        let c = cmd.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|kind| command_contains_identifier(&c, kind.id()))
    }
}

fn command_contains_identifier(command: &str, identifier: &str) -> bool {
    if identifier.is_empty() {
        return false;
    }
    let identifier = identifier.to_ascii_lowercase();
    // Keep the established coarse `contains` behavior for normal-length provider
    // ids. Pi is intentionally short, so only short ids need token boundaries.
    if identifier.len() > 2 {
        return command.contains(identifier.as_str());
    }
    command
        .match_indices(identifier.as_str())
        .any(|(start, _)| {
            let end = start + identifier.len();
            let is_word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
            !command[..start].chars().next_back().is_some_and(is_word)
                && !command[end..].chars().next().is_some_and(is_word)
        })
}

/// 一个内嵌终端可直接启动的 agent CLI 的静态属性。**新增一种 = `TERMINAL_AGENTS`
/// 加一行 + 枚举加一条变体**。
pub struct TerminalAgentDescriptor {
    /// provider 的稳定标识。它不必等于 CLI 文件名，例如 Antigravity 的命令是 `agy`。
    pub id: &'static str,
    /// PATH 中实际执行的程序名。
    pub cli_program: &'static str,
    pub label: &'static str,
    /// 快捷终端「+」菜单默认启动项的展示名。
    pub quick_terminal_label: &'static str,
    /// 直接启动 CLI 自带的 TUI。出厂统一带全权限参数（与无人值守任务的全权限
    /// 语义一致）：Antigravity 的 `agy` 支持 `--dangerously-skip-permissions`，
    /// Grok 支持 `--always-approve`，默认追加后不再各自弹审批；需要更保守时
    /// 用户自行去掉参数即可。
    pub quick_terminal_cmd: &'static str,
    /// 曾经发布过的快捷终端出厂命令。配置迁移只替换这些逐字旧值，避免覆盖
    /// 用户自定义命令；参数位置或 CLI 兼容性变化时在对应注册项追加即可。
    pub previous_quick_terminal_cmds: &'static [&'static str],
    /// 进程级结构化状态桥。与 cross-agent MCP 是两项独立能力。
    pub status_bridge: TerminalStatusBridge,
    /// 这个 CLI 自己的历史续接语法。
    ///
    /// 只给**没有 ACP 对应项**的纯终端 agent 用（如 Antigravity）；两表都有的
    /// agent 一律以 `ConversationAgentDescriptor::cli_resume` 为准，避免同一事实在
    /// 两处各写一份、改一处忘一处。`None` = 没有可用的终端续接语法。
    pub terminal_only_cli_resume: Option<CliResumeSyntax>,
}

/// 终端 agent 注册表。顺序 = 快捷终端菜单的出厂排列顺序，且必须与
/// `TerminalAgentKind::ALL` 逐项对齐。
pub static TERMINAL_AGENTS: &[TerminalAgentDescriptor] = &[
    TerminalAgentDescriptor {
        id: "claude",
        cli_program: "claude",
        label: "Claude Code",
        quick_terminal_label: "Claude Code",
        quick_terminal_cmd: "claude --dangerously-skip-permissions",
        previous_quick_terminal_cmds: &[],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "copilot",
        cli_program: "copilot",
        label: "GitHub Copilot",
        quick_terminal_label: "GitHub Copilot",
        quick_terminal_cmd: "copilot --allow-all",
        previous_quick_terminal_cmds: &[],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "codex",
        cli_program: "codex",
        label: "Codex",
        quick_terminal_label: "Codex",
        // 标题策略归 CLI 自己所有：Smelt 像普通终端一样接收其完整 OSC 0/2，
        // 不通过启动参数压掉 spinner、项目名或其它 provider 自定义组成部分。
        quick_terminal_cmd: "codex --dangerously-bypass-approvals-and-sandbox",
        previous_quick_terminal_cmds: &[concat!(
            "codex --dangerously-bypass-approvals-and-sandbox ",
            "-c 'tui.terminal_title=[\"thread-title\"]'"
        )],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "grok",
        cli_program: "grok",
        label: "Grok",
        quick_terminal_label: "Grok",
        quick_terminal_cmd: "grok --always-approve",
        previous_quick_terminal_cmds: &["grok"],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "antigravity",
        cli_program: "agy",
        label: "Antigravity",
        quick_terminal_label: "Antigravity",
        quick_terminal_cmd: "agy --dangerously-skip-permissions",
        previous_quick_terminal_cmds: &["agy"],
        status_bridge: TerminalStatusBridge::None,
        // `agy --conversation <id>` 续接历史会话（实测 `agy --help`）。
        // Antigravity 没有 ACP 表项，终端续接语法只能登记在这里。
        terminal_only_cli_resume: Some(CliResumeSyntax::ConversationFlag),
    },
    TerminalAgentDescriptor {
        id: "cursor",
        cli_program: "cursor-agent",
        label: "Cursor Agent",
        quick_terminal_label: "Cursor Agent",
        quick_terminal_cmd: "cursor-agent --force",
        previous_quick_terminal_cmds: &[],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "opencode",
        cli_program: "opencode",
        label: "OpenCode",
        quick_terminal_label: "OpenCode",
        quick_terminal_cmd: "opencode --auto",
        previous_quick_terminal_cmds: &[],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "kiro",
        cli_program: "kiro-cli",
        label: "Kiro",
        quick_terminal_label: "Kiro",
        // Kiro 全局 hooks 只在 v3 生效；快捷终端必须显式选择 v3，不能让安装器
        // 写着 PascalCase hooks，实际会话却仍默认跑 v2。
        quick_terminal_cmd: "kiro-cli chat --v3 --trust-all-tools",
        previous_quick_terminal_cmds: &[
            "kiro-cli --trust-all-tools",
            "kiro-cli chat --trust-all-tools",
        ],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "pi",
        cli_program: "pi",
        label: "Pi",
        quick_terminal_label: "Pi",
        // Pi 的内置工具默认全部启用，没有独立的工具审批旁路参数；`--approve`
        // 是其最高启动信任级别，会加载项目级设置、扩展、skills 等本地资源。
        quick_terminal_cmd: "pi --approve",
        previous_quick_terminal_cmds: &["pi"],
        status_bridge: TerminalStatusBridge::PiExtension,
        terminal_only_cli_resume: None,
    },
    TerminalAgentDescriptor {
        id: "crush",
        cli_program: "crush",
        label: "Crush",
        quick_terminal_label: "Crush",
        // Crush 官方把 --yolo 定义为跳过全部权限确认。当前正式版没有 ACP
        // server，也没有完整的会话生命周期 hook，因此这里只登记终端能力，
        // 不伪造 ACP 或结构化状态桥。
        quick_terminal_cmd: "crush --yolo",
        previous_quick_terminal_cmds: &[],
        status_bridge: TerminalStatusBridge::None,
        terminal_only_cli_resume: None,
    },
];

/// 内嵌终端能直接启动的 agent CLI。这个集合可以比 ACP 多：Antigravity 当前只有
/// `agy` TUI 与 hooks，没有 ACP stdio 服务，因此绝不能放进 `ConversationAgentKind::ALL`。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TerminalAgentKind {
    Claude,
    Copilot,
    Codex,
    Grok,
    Antigravity,
    Cursor,
    OpenCode,
    Kiro,
    Pi,
    Crush,
}

impl TerminalAgentKind {
    /// 快捷终端菜单的出厂排列顺序。
    pub const ALL: [Self; 10] = [
        Self::Claude,
        Self::Copilot,
        Self::Codex,
        Self::Grok,
        Self::Antigravity,
        Self::Cursor,
        Self::OpenCode,
        Self::Kiro,
        Self::Pi,
        Self::Crush,
    ];

    /// 这一家的静态属性。变体序号即表下标，见 `TERMINAL_AGENTS` 的顺序约定。
    pub fn descriptor(self) -> &'static TerminalAgentDescriptor {
        &TERMINAL_AGENTS[self as usize]
    }

    /// provider 的稳定标识。
    pub fn id(self) -> &'static str {
        self.descriptor().id
    }

    /// PATH 中实际执行的程序名。
    pub fn cli_program(self) -> &'static str {
        self.descriptor().cli_program
    }

    pub fn label(self) -> &'static str {
        self.descriptor().label
    }

    /// 快捷终端「+」菜单默认启动项的展示名。
    pub fn quick_terminal_label(self) -> &'static str {
        self.descriptor().quick_terminal_label
    }

    /// 直接启动 CLI 自带的 TUI 的出厂命令。
    pub fn quick_terminal_cmd(self) -> &'static str {
        self.descriptor().quick_terminal_cmd
    }

    pub fn status_bridge(self) -> TerminalStatusBridge {
        self.descriptor().status_bridge
    }

    /// 这个 CLI 的历史续接语法。两表都有的 agent 以 ACP 表为准（单一事实来源），
    /// 只有纯终端 agent 才用终端表自己那份。
    pub fn cli_resume_syntax(self) -> Option<CliResumeSyntax> {
        match self.acp() {
            Some(acp) => acp.cli_resume_syntax(),
            None => self.descriptor().terminal_only_cli_resume,
        }
    }

    /// 若命令逐字命中 Smelt 发布过的旧快捷终端默认值，返回当前默认值。
    /// 用户增删过参数的自定义命令不会命中。
    pub fn upgrade_released_quick_terminal_command(self, command: &str) -> Option<&'static str> {
        let current = self.quick_terminal_cmd();
        let trimmed = command.trim();
        if trimmed == current {
            return None;
        }
        self.descriptor()
            .previous_quick_terminal_cmds
            .contains(&trimmed)
            .then_some(current)
    }

    /// 侧栏/菜单用的本地 SVG 资源路径，约定 `smelt-icons/agent-<id>.svg`。
    pub fn icon_asset(self) -> String {
        format!("smelt-icons/agent-{}.svg", self.id())
    }

    /// 从快捷启动命令的第一个 token 识别 agent；不跨过 `env`/变量前缀，避免图标
    /// 分类偷偷拥有一套不完整的 shell 解析规则。
    pub fn from_command_prefix(cmd: &str) -> Option<Self> {
        let program = cmd.split_whitespace().next()?.trim_matches(['\'', '"']);
        let program = std::path::Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())?;
        Self::ALL
            .into_iter()
            .find(|kind| kind.cli_program() == program)
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// 这个终端 CLI 在 ACP 表里的对应项，两张表按 `id` 对齐。历史存档按
    /// `ConversationAgentKind` 分命名空间，终端会话要对上自己的历史就得先过这一步。
    ///
    /// 与 [`ConversationAgentKind::terminal`] 对称，返回 `Option` 同样是实话：Antigravity
    /// 只有 TUI，没有 ACP 侧的历史命名空间。
    pub fn acp(self) -> Option<ConversationAgentKind> {
        ConversationAgentKind::from_id(self.id())
    }
}

impl ConversationAgentKind {
    /// 这家 ACP agent 在终端表里的对应项，两张表按 `id` 对齐。
    ///
    /// **返回 Option 是实话**：两张表本来就不是同一个集合。Antigravity 只有 TUI
    /// 没有 ACP；反过来 DeepSeek Harness 的 Smelt 入口必须绑定到一个原生
    /// profile，不能退化成不带 profile 身份的基础槽位。
    ///
    /// 之前这里是 `From`，认不出就 `unwrap_or(Claude)` 兜底。全集重合时那个兜底
    /// 永远走不到，加进来一个只有 ACP 的 agent 之后它就成了一颗哑弹：dsh 会被
    /// 静默当成 Claude，「在终端里继续」会去起一个根本无关的 CLI。所以这里由
    /// 调用方显式处理「没有终端对应项」。
    pub fn terminal(self) -> Option<TerminalAgentKind> {
        TerminalAgentKind::from_id(self.id())
    }
}

/// 历史页能列出的「会话来源」。它是 [`ConversationAgentKind`] 的**超集**：一个
/// agent 能不能被读历史，取决于它有没有在本机落盘 transcript，跟它有没有 ACP
/// stdio 服务是两件独立的事。
///
/// 之前历史 tab 直接按 `ConversationAgentKind::ALL` 枚举，等于把「可接 ACP」
/// 当成了「可读历史」的同义词。Antigravity 只有 `agy` TUI，却实打实把会话写在
/// `~/.gemini/antigravity-cli/` 下，那个等号一成立它就永远进不了历史页。
///
/// 注意这里的能力是**不对称**的，别按「来源都一样」去写调用方：
/// - 读列表 / 读详情：全部来源都支持（否则不会出现在这个枚举里）。
/// - 作为迁移**源**：全部支持，因为迁移只需要读得出 transcript。
/// - 作为迁移**目标** / ACP 续接：只有 [`Self::acp`] 为 `Some` 的来源支持，
///   目标侧要新开一条 ACP 会话。
/// - 续接：有 ACP 的走 `session/load`，纯终端来源只能起自己的 TUI。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HistorySourceKind {
    /// 同时具备 ACP 能力的 agent，历史与续接都走既有链路。
    Conversation(ConversationAgentKind),
    /// 只有 TUI 的 agent：历史读得出来，但续接只能起 CLI。
    TerminalOnly(TerminalAgentKind),
}

impl HistorySourceKind {
    /// 历史页来源 tab 的出厂顺序：先全部 ACP 种类（保持原有排列），再挂纯终端来源。
    /// 纯终端来源必须显式登记——不是所有 TUI 都在本机留下可解析的 transcript。
    pub const ALL: [Self; 10] = [
        Self::Conversation(ConversationAgentKind::Claude),
        Self::Conversation(ConversationAgentKind::Copilot),
        Self::Conversation(ConversationAgentKind::Codex),
        Self::Conversation(ConversationAgentKind::Grok),
        Self::Conversation(ConversationAgentKind::Cursor),
        Self::Conversation(ConversationAgentKind::OpenCode),
        Self::Conversation(ConversationAgentKind::Kiro),
        Self::Conversation(ConversationAgentKind::Pi),
        Self::Conversation(ConversationAgentKind::Dsh),
        Self::TerminalOnly(TerminalAgentKind::Antigravity),
    ];

    /// 存档 / 缓存 key 用的稳定标识。两张表按 `id` 对齐，所以这里不需要再分支加前缀。
    pub fn id(self) -> &'static str {
        match self {
            Self::Conversation(kind) => kind.id(),
            Self::TerminalOnly(kind) => kind.id(),
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation(kind) => kind.label(),
            Self::TerminalOnly(kind) => kind.label(),
        }
    }

    /// 窄位置用的短名。终端表没有单独的短名，退回 `label`。
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Conversation(kind) => kind.short_label(),
            Self::TerminalOnly(kind) => kind.label(),
        }
    }

    pub fn icon_asset(self) -> String {
        format!("smelt-icons/agent-{}.svg", self.id())
    }

    /// 这个来源的 ACP 身份。`None` = 纯终端来源，**不能**作为迁移目标或 ACP 续接对象。
    pub fn acp(self) -> Option<ConversationAgentKind> {
        match self {
            Self::Conversation(kind) => Some(kind),
            Self::TerminalOnly(_) => None,
        }
    }

    /// 这个来源的终端 CLI 身份。`None` = 没有可在终端里续接的 CLI（如 dsh）。
    pub fn terminal(self) -> Option<TerminalAgentKind> {
        match self {
            Self::Conversation(kind) => kind.terminal(),
            Self::TerminalOnly(kind) => Some(kind),
        }
    }

    /// 能否作为裸种类出现在历史 tab 里。纯终端来源既然被登记进来，就是能读历史的。
    pub fn is_bare_kind(self) -> bool {
        match self {
            Self::Conversation(kind) => kind.is_bare_kind(),
            Self::TerminalOnly(_) => true,
        }
    }

    /// 在终端里续接这一条历史的语法。`None` = 没有可用的 CLI 续接路径。
    pub fn cli_resume_syntax(self) -> Option<CliResumeSyntax> {
        self.terminal()?.cli_resume_syntax()
    }
}

impl From<ConversationAgentKind> for HistorySourceKind {
    fn from(kind: ConversationAgentKind) -> Self {
        Self::Conversation(kind)
    }
}

impl From<TerminalAgentKind> for HistorySourceKind {
    /// 两表都有的 agent 归结到 ACP 身份，只有终端表独有的才是 `TerminalOnly`。
    ///
    /// 这个收敛必须只在这一处发生：调用方自己 match 写一遍，就有人会把
    /// 本来有 ACP 的 agent 包成 `TerminalOnly`，那它就拥有了两个不相等的历史
    /// 身份，tab、缓存 key 和 Provider 查找会当场分裂。
    fn from(kind: TerminalAgentKind) -> Self {
        match kind.acp() {
            Some(acp) => Self::Conversation(acp),
            None => Self::TerminalOnly(kind),
        }
    }
}

#[cfg(test)]
mod history_source_tests {
    use super::*;

    /// `TerminalOnly` 只能装真正没有 ACP 对应项的 agent。否则同一个 agent 会拥有
    /// 两个不相等的历史身份，历史 tab、缓存 key 和 Provider 查找全会分裂。
    #[test]
    fn terminal_only_sources_really_have_no_acp() {
        for source in HistorySourceKind::ALL {
            if let HistorySourceKind::TerminalOnly(terminal) = source {
                assert_eq!(
                    terminal.acp(),
                    None,
                    "{} 已有 ACP 对应项，应注册为 Conversation",
                    terminal.id()
                );
            }
        }
    }

    /// 从终端身份转换时，两表都有的 agent 必须收敛到 ACP 身份。这条守住「一个
    /// agent 只有一个历史身份」：否则在终端里给 Claude 会话改的名，历史页读不到。
    #[test]
    fn terminal_identities_collapse_onto_acp_when_available() {
        for terminal in TerminalAgentKind::ALL {
            let source = HistorySourceKind::from(terminal);
            match terminal.acp() {
                Some(acp) => assert_eq!(source, HistorySourceKind::Conversation(acp)),
                None => assert_eq!(source, HistorySourceKind::TerminalOnly(terminal)),
            }
            assert_eq!(source.id(), terminal.id());
        }
    }

    /// 来源标识不能重复：`from_id` 和缓存 key 都按 id 寻址。
    #[test]
    fn history_source_ids_are_unique() {
        let mut ids: Vec<&str> = HistorySourceKind::ALL.iter().map(|k| k.id()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "历史来源 id 有重复");
    }

    /// 历史来源是 ACP 种类的超集，且 `id` 在两边一致。
    #[test]
    fn history_sources_are_a_superset_of_acp_kinds() {
        for kind in ConversationAgentKind::ALL {
            let source = HistorySourceKind::from(kind);
            assert_eq!(source.id(), kind.id());
            assert!(HistorySourceKind::ALL.contains(&source), "{}", kind.id());
        }
        assert!(
            HistorySourceKind::ALL.len() > ConversationAgentKind::ALL.len(),
            "超集应严格大于 ACP 种类集合"
        );
    }

    /// Antigravity 的终端续接语法实测自 `agy --help`。
    #[test]
    fn antigravity_resumes_with_conversation_flag() {
        let source = HistorySourceKind::TerminalOnly(TerminalAgentKind::Antigravity);
        assert_eq!(
            source.cli_resume_syntax(),
            Some(CliResumeSyntax::ConversationFlag)
        );
        assert_eq!(
            CliResumeSyntax::ConversationFlag.suffix("'abc'"),
            "--conversation 'abc'"
        );
    }

    /// 两表都有的 agent 以 ACP 表为唯一事实来源，终端表不得再写一份。
    #[test]
    fn dual_table_agents_take_resume_syntax_from_acp() {
        for terminal in TerminalAgentKind::ALL {
            if let Some(acp) = terminal.acp() {
                assert_eq!(terminal.cli_resume_syntax(), acp.cli_resume_syntax());
                assert_eq!(
                    terminal.descriptor().terminal_only_cli_resume,
                    None,
                    "{} 同时在两表里，不该在终端表里重复登记续接语法",
                    terminal.id()
                );
            }
        }
    }
}

pub fn default_acp_cmd() -> String {
    // bunx 由 smelt 解析到受管 bun（~/.smelt/runtime；启动时由 Smelt 代用户同步锁定
    // 版本，见 acp_conn.rs 的 spawn_managed_bun_sync）；
    // 适配器锁版本——方言适配与回归测试都对着这个版本做，升级是主动行为。
    // 启动层会优先将本机 `claude` 注入 CLAUDE_CODE_EXECUTABLE；没装才使用 SDK
    // 携带的原生二进制，避免本机 CLI 与适配器运行时版本脱节。
    //
    // --bun：强制 bunx 用 bun 自己的运行时执行，不 fallback 到系统 Node——实测
    // 发现这个适配器声明了 `engines.node >= 22`，bunx 默认会尊重这个声明主动
    // 切到系统 Node 去跑（哪怕我们准备了受管 bun），「受管运行时不依赖系统装了
    // 什么」这条设计承诺不加这个 flag 就不成立。见 bunx --help 的官方说明。
    //
    // 0.78.0 已用受管 Bun 1.4.0 + 本机 Claude CLI 实测 initialize：协议版本 1、
    // 图片与 MCP 能力正常，`agentCapabilities.loadSession: true`。0.77 删掉的
    // agent-picker / `claudeCode.options.agent` Smelt 没用；不要广告
    // native subagent / session fork，适配器声明了却没实现。
    CLAUDE_ACP_ADAPTER.current_command()
}

pub fn default_acp_copilot_cmd() -> String {
    // Copilot CLI 自带 ACP 服务端，不需要适配器：`copilot --help` 里明写
    // `--acp  Start as Agent Client Protocol server`（实测 1.0.73）。
    // 代价是得先装 CLI（`brew install copilot` / npm `@github/copilot`）并
    // `copilot` 登录过——找不到命令时 Fatal 会带上 stderr 说明。
    // `--allow-all` 与快捷终端 / 无人值守任务全权限对齐；ACP 审批卡不再挡工具调用。
    "copilot --acp --allow-all".to_string()
}

pub fn default_acp_codex_cmd() -> String {
    // 改回标准 ACP 通道：官方 `@agentclientprotocol/codex-acp` 适配器把 Codex
    // app-server 包了一层 ACP，跟 Claude/Copilot/Grok 走同一套 AcpAgent 驱动，
    // 不再需要 smeltd 里那条按命令字符串识别的 codex_app_server 专用 driver
    // （dispatch 逻辑保留，兼容手填 `codex app-server` 的旧存档/自定义命令）。
    // 1.12.0 已用受管 Bun 1.4.0 + 本机 Codex CLI 实测 initialize：协议版本 1、
    // 图片/MCP 与 `loadSession` 能力正常。1.7+ 原生 subagent / fork / 后台任务
    // 未协商则退回现有 tool call。build_agent_args 里的 CODEX_PATH
    // 注入继续优先使用本机 CLI，找不到才 fallback 到适配器 bundle 的版本。
    CODEX_ACP_ADAPTER.current_command()
}

pub fn default_acp_grok_cmd() -> String {
    // Grok CLI 自带 ACP：`grok agent stdio`（help 里只写「Run the agent over
    // stdio」没提协议名，实测发 initialize 能正常握手，agentCapabilities 齐全）。
    // `--always-approve` 插在 `agent` 子命令后，与终端 / 无人值守任务全权限对齐。
    // 需先装 grok CLI 并登录（凭据在 ~/.grok/auth.json）。
    //
    // 注意它 `promptCapabilities.image = false`——是唯一不收图的，粘贴图片
    // 对 Grok 会话没用。
    "grok agent --always-approve stdio".to_string()
}

pub fn default_acp_cursor_cmd() -> String {
    // Cursor CLI 自带 ACP：`cursor-agent acp`（官方文档写 `agent acp`，但 `agent`
    // 会和 Grok 的同名命令撞车；稳定入口是 `cursor-agent`）。
    // 需先装 Cursor CLI 并登录。`--force` 是顶层 flag，必须在 `acp` 子命令前。
    "cursor-agent --force acp".to_string()
}

pub fn default_acp_opencode_cmd() -> String {
    // OpenCode CLI 自带 ACP：`opencode acp`（官方文档，stdio JSON-RPC）。
    // 需先装 CLI（官方脚本落到 ~/.opencode/bin，或 `npm i -g opencode-ai`）并登录。
    // `--auto` 不是 ACP 子命令参数；全权限由注册表的进程级配置覆盖提供。
    "opencode acp".to_string()
}

pub fn default_acp_kiro_cmd() -> String {
    // Kiro CLI 自带 ACP：`kiro-cli acp`（官方文档，stdio JSON-RPC）。
    // Smelt 的 Kiro hooks 使用 v3 全局 schema，因此 ACP 也显式选择 v3；
    // `--auth-method cli` 让无头子进程复用 Kiro CLI 已有登录，而不是要求 ACP
    // 客户端另行提供凭据。Kiro 2.20.0 的 v3 ACP 明确拒绝 `--trust-all-tools`，
    // 工具权限必须走 ACP 的 permission 请求 / 响应，不能照搬 Terminal 参数。
    // 需先装 CLI（`brew install --cask kiro-cli` 或官方 install 脚本落到
    // ~/.local/bin）并 `kiro-cli login`。
    //
    // 官方能力：`loadSession: true`，`promptCapabilities.image: true`。
    "kiro-cli acp --agent-engine v3 --auth-method cli".to_string()
}

pub fn default_acp_pi_cmd() -> String {
    // 这是 Smelt 自带运行时的逻辑名。实际启动时由连接层解析成锁定版本的受管
    // Bun + Pi 官方 RPC entry；配置里不持久化机器相关的绝对路径。
    SMELT_PI_AGENT_COMMAND.to_string()
}

pub fn default_acp_dsh_cmd() -> String {
    "dsh-acp-rich".to_string()
}

impl AcpProfile {
    pub fn is_native_dsh(&self) -> bool {
        self.kind() == Some(ConversationAgentKind::Dsh) && self.id.starts_with("dsh-native-")
    }
}

/// ACP 启动规格：命令本体保留现有的空白分词语义，环境变量单独结构化存储，
/// 避免把带空格的值硬塞回 `VAR=value cmd` 这种不可靠的字符串拼接。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConversationLaunchSpec {
    pub command: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl ConversationLaunchSpec {
    pub fn from_command(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            env: BTreeMap::new(),
        }
    }

    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }
}

/// 只允许由启动 smelt 的那个环境提供的变量名：它们决定进程怎么起、代码和指令
/// 从哪加载、怎么连网络。设置页那个框是「传给 agent 的环境」，不是「改写 smelt
/// 自己的运行环境」——放任改 `PATH` 会让 agent 命令解析失败，且报错完全指不到
/// 这个框上。dsh 的 `.env` 加载器出于同样理由维护了同一类名单。
const LAUNCH_ONLY_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "SHELL",
    "NODE_OPTIONS",
    "NODE_PATH",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "BASH_ENV",
];

/// 变量名是否合法：POSIX shell 标识符。dsh 的 credential 引用（`apiKeyEnv`）
/// 用的是同一套规则，两边对不上会变成「设置里填了、agent 侧解析不到」。
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 解析设置页那个多行 `KEY=VALUE` 输入框。
///
/// 值一律按字面量处理，不做引号剥离、不做 `$VAR` 展开：这里的值大多是 API key
/// 和 URL，任何"聪明"的改写都会在用户看不见的地方改掉密钥。
pub fn parse_env_lines(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("第 {number} 行缺少 `=`：{line}"));
        };
        let name = name.trim();
        if !is_env_name(name) {
            return Err(format!(
                "第 {number} 行的变量名不合法：`{name}`（只能是字母、数字、下划线，且不以数字开头）"
            ));
        }
        if LAUNCH_ONLY_ENV.contains(&name) {
            return Err(format!(
                "第 {number} 行的 `{name}` 只能由启动 smelt 的环境提供：它决定进程怎么起、\
                 代码从哪加载或怎么连网络，在这里改会让 agent 起不来且报错指不到这里"
            ));
        }
        // 值只去掉首尾空白：粘贴 key 时常带一个尾随空格，而带空格的值本身应当保留。
        if env
            .insert(name.to_string(), value.trim().to_string())
            .is_some()
        {
            return Err(format!("第 {number} 行重复定义了 `{name}`"));
        }
    }
    Ok(env)
}

/// `parse_env_lines` 的逆向：把表还原成输入框里的文本。
pub fn format_env_lines(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 手动添加的一个 workspace：底层还是基础 agent 之一，只是换了个数据目录
/// （比如 Claude 的 `CLAUDE_CONFIG_DIR`）。命令不用手填——按 `kind` 的出厂命令
/// 加一段 `ENV=workspace_dir` 前缀自动拼出来（见 `command()`），用户只需要选
/// agent 类型 + 填目录，不用记环境变量名和 shell 语法。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AcpProfile {
    /// 稳定 id（新建时生成，不随 label/kind 改变），历史会话页拿它当 tab key、
    /// 区分"同一个 kind 下的哪一个 workspace"。
    pub id: String,
    /// 底层 agent 种类，存 `ConversationAgentKind::id()`（跟 `AcpSaved.agent` 同一份规则）。
    /// 未知 id 原样保留，供新版配置降级读取；消费方不得改认成另一种 Agent。
    pub kind_id: String,
    /// 设置页 / 历史页 tab 上显示的名字，比如「Claude Quant」。
    pub label: String,
    /// workspace 目录，允许 `~` 开头（展开逻辑跟 build_agent 共用一份，见
    /// `crate::workspace_override`）。
    pub workspace_dir: String,
}

impl AcpProfile {
    pub fn kind(&self) -> Option<ConversationAgentKind> {
        ConversationAgentKind::from_id(&self.kind_id)
    }

    fn required_kind(&self) -> Result<ConversationAgentKind, String> {
        self.kind().ok_or_else(|| {
            format!(
                "workspace profile `{}` 使用了未知 ACP Agent `{}`",
                self.id, self.kind_id
            )
        })
    }

    /// 该 profile 底层 agent 用来覆盖数据目录的环境变量名。
    pub fn env_var(&self) -> Result<&'static str, String> {
        let kind = self.required_kind()?;
        crate::workspace_override::config_dir_env_var(kind.id())
            .ok_or_else(|| format!("{} 不支持自定义 workspace 目录", kind.label()))
    }

    /// 该 profile 的结构化启动规格：命令仍跟着 kind 的出厂值走，workspace 目录
    /// 作为独立环境变量保留原样（包括空格），由下游统一处理兼容旧字符串前缀。
    pub fn launch_spec(&self) -> Result<ConversationLaunchSpec, String> {
        let kind = self.required_kind()?;
        if kind == ConversationAgentKind::Dsh && self.id.starts_with("dsh-native-") {
            return Ok(dsh_profile_launch_spec(&self.workspace_dir));
        }
        Ok(kind
            .default_launch()
            .with_env(self.env_var()?, self.workspace_dir.clone()))
    }

    /// 兼容旧调用方的字符串命令。保留到 GUI/daemon 全链路升级完成为止。
    pub fn command(&self) -> Result<String, String> {
        if self.is_native_dsh() {
            return Ok(self.launch_spec()?.command);
        }
        let kind = self.required_kind()?;
        Ok(format!(
            "{}={} {}",
            self.env_var()?,
            self.workspace_dir,
            kind.default_cmd()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表下标 = 枚举变体序号，`descriptor()` 的索引访问依赖这条；表和枚举
    /// 顺序一旦对不上，所有显示名/命令会整体错位，所以在这里钉死。
    #[test]
    fn descriptor_table_matches_enum_order() {
        assert_eq!(CONVERSATION_AGENTS.len(), ConversationAgentKind::ALL.len());
        for (index, kind) in ConversationAgentKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.descriptor().id, CONVERSATION_AGENTS[index].id);
            assert_eq!(
                ConversationAgentKind::from_id(CONVERSATION_AGENTS[index].id),
                Some(kind)
            );
        }

        assert_eq!(TERMINAL_AGENTS.len(), TerminalAgentKind::ALL.len());
        for (index, kind) in TerminalAgentKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.descriptor().id, TERMINAL_AGENTS[index].id);
            assert_eq!(
                TerminalAgentKind::from_id(TERMINAL_AGENTS[index].id),
                Some(kind)
            );
        }
    }

    /// `From<ConversationAgentKind>` 靠 id 查表，所有支持终端的 agent 必须都在终端表里。
    #[test]
    fn acp_terminal_mapping_is_by_id_and_never_guesses() {
        for kind in ConversationAgentKind::ALL {
            match kind.terminal() {
                // 有对应项时必须是同一个 id——绝不允许「认不出就当 Claude」。
                Some(terminal) => assert_eq!(terminal.id(), kind.id()),
                // 没有对应项时终端表里也确实查不到这个 id，两边说的是同一件事。
                None => assert!(TerminalAgentKind::from_id(kind.id()).is_none()),
            }
        }
    }

    /// dsh 是第一个只有 ACP、没有终端 CLI 的 agent；这条守着 `terminal()` 不被
    /// 顺手改回一个总能返回值的 `From`。
    #[test]
    fn dsh_has_no_terminal_counterpart() {
        assert_eq!(ConversationAgentKind::Dsh.terminal(), None);
    }

    #[test]
    fn cli_resume_syntax_matches_each_agents_tui() {
        assert_eq!(
            ConversationAgentKind::Claude.cli_resume_syntax(),
            Some(CliResumeSyntax::Flag)
        );
        assert_eq!(
            ConversationAgentKind::Copilot.cli_resume_syntax(),
            Some(CliResumeSyntax::FlagEquals)
        );
        assert_eq!(
            ConversationAgentKind::Codex.cli_resume_syntax(),
            Some(CliResumeSyntax::Subcommand)
        );
        assert_eq!(
            ConversationAgentKind::Grok.cli_resume_syntax(),
            Some(CliResumeSyntax::Flag)
        );
        assert_eq!(
            ConversationAgentKind::Cursor.cli_resume_syntax(),
            Some(CliResumeSyntax::Flag)
        );
        assert_eq!(
            ConversationAgentKind::OpenCode.cli_resume_syntax(),
            Some(CliResumeSyntax::SessionFlag)
        );
        assert_eq!(
            ConversationAgentKind::Kiro.cli_resume_syntax(),
            Some(CliResumeSyntax::ResumeIdFlag)
        );
        assert_eq!(
            ConversationAgentKind::Pi.cli_resume_syntax(),
            Some(CliResumeSyntax::SessionFlag)
        );
        assert_eq!(ConversationAgentKind::Dsh.cli_resume_syntax(), None);
        for kind in ConversationAgentKind::ALL {
            assert_eq!(
                kind.cli_resume_syntax().is_some(),
                kind.terminal().is_some(),
                "{} 的续接语法应和有没有终端 CLI 一致",
                kind.id()
            );
        }
    }

    #[test]
    fn only_profile_hosted_agents_are_hidden_from_bare_kind_menus() {
        for kind in ConversationAgentKind::ALL {
            if kind == ConversationAgentKind::Dsh {
                assert!(!kind.is_bare_kind());
            } else {
                assert!(kind.is_bare_kind());
            }
        }
    }

    #[test]
    fn terminal_mcp_inject_is_a_capability_not_a_vendor_name() {
        assert_eq!(
            ConversationAgentKind::Claude.terminal_mcp_inject(),
            TerminalMcpInject::McpConfigJson
        );
        assert_eq!(
            ConversationAgentKind::Copilot.terminal_mcp_inject(),
            TerminalMcpInject::AdditionalMcpConfigJson
        );
        assert_eq!(
            ConversationAgentKind::Codex.terminal_mcp_inject(),
            TerminalMcpInject::CodexToml
        );
        assert_eq!(
            ConversationAgentKind::OpenCode.terminal_mcp_inject(),
            TerminalMcpInject::OpenCodeConfigContent
        );
        for kind in [
            ConversationAgentKind::Grok,
            ConversationAgentKind::Cursor,
            ConversationAgentKind::Kiro,
            ConversationAgentKind::Pi,
            ConversationAgentKind::Dsh,
        ] {
            assert_eq!(kind.terminal_mcp_inject(), TerminalMcpInject::None);
            assert!(kind.acp_accepts_session_mcp());
        }
        assert!(!ConversationAgentKind::Copilot.acp_accepts_session_mcp());
        assert!(ConversationAgentKind::Claude.acp_accepts_session_mcp());
    }

    #[test]
    fn only_pi_uses_a_process_local_terminal_status_bridge() {
        assert_eq!(
            TerminalAgentKind::Pi.status_bridge(),
            TerminalStatusBridge::PiExtension
        );
        for kind in TerminalAgentKind::ALL {
            if kind != TerminalAgentKind::Pi {
                assert_eq!(kind.status_bridge(), TerminalStatusBridge::None);
            }
        }
    }

    #[test]
    fn crush_is_terminal_only_without_an_invented_acp_or_status_bridge() {
        let crush = TerminalAgentKind::from_id("crush").expect("Crush 应登记为终端 agent");
        assert_eq!(crush.quick_terminal_cmd(), "crush --yolo");
        assert_eq!(crush.status_bridge(), TerminalStatusBridge::None);
        assert_eq!(ConversationAgentKind::from_id("crush"), None);
    }

    #[test]
    fn only_cli_patched_headless_agents_touch_the_launch_command() {
        assert!(matches!(
            ConversationAgentKind::Copilot.task_cli(),
            TaskCliPatch::Append { .. }
        ));
        assert!(matches!(
            ConversationAgentKind::Kiro.task_cli(),
            TaskCliPatch::Append {
                full_access: "--trust-all-tools",
                ..
            }
        ));
        assert!(matches!(
            ConversationAgentKind::Grok.task_cli(),
            TaskCliPatch::AfterMarker { .. }
        ));
        assert!(matches!(
            ConversationAgentKind::Cursor.task_cli(),
            TaskCliPatch::AfterMarker {
                marker: "cursor-agent",
                full_access: "--force",
                ..
            }
        ));
        for kind in [
            ConversationAgentKind::Claude,
            ConversationAgentKind::Codex,
            ConversationAgentKind::OpenCode,
            ConversationAgentKind::Pi,
            ConversationAgentKind::Dsh,
        ] {
            assert_eq!(kind.task_cli(), TaskCliPatch::None);
        }
    }

    #[test]
    fn bare_cli_agents_support_headless_runtime_except_profile_hosted() {
        for kind in [
            ConversationAgentKind::Claude,
            ConversationAgentKind::Copilot,
            ConversationAgentKind::Codex,
            ConversationAgentKind::Grok,
            ConversationAgentKind::Cursor,
            ConversationAgentKind::OpenCode,
            ConversationAgentKind::Kiro,
            ConversationAgentKind::Pi,
        ] {
            assert!(
                kind.supports_headless_runtime(),
                "{} 应支持本机无人值守 runtime",
                kind.id()
            );
        }
        assert_eq!(
            ConversationAgentKind::Cursor.headless_runtime_cli(),
            Some("cursor-agent")
        );
        assert_eq!(
            ConversationAgentKind::OpenCode.headless_runtime_cli(),
            Some("opencode")
        );
        assert_eq!(
            ConversationAgentKind::Kiro.headless_runtime_cli(),
            Some("kiro-cli")
        );
        assert!(!ConversationAgentKind::Dsh.supports_headless_runtime());
        assert_eq!(ConversationAgentKind::Dsh.headless_runtime_cli(), None);
        let ids: Vec<_> = ConversationAgentKind::headless_runtime_kinds()
            .map(ConversationAgentKind::id)
            .collect();
        assert_eq!(
            ids,
            [
                "claude", "copilot", "codex", "grok", "cursor", "opencode", "kiro", "pi"
            ]
        );
    }

    /// 每个 kind 的图标在桌面和移动端两份 bundle 里都得真的存在。
    ///
    /// 图标是按 id 拼路径的，所以「新增一家 agent 但没放 SVG」不会让任何代码路径
    /// 失败：桌面画个空图标，移动端掉回通用机器人——用户看见的是**错误的身份**，
    /// 而不是「少了点什么」。这种缺失只能靠人眼发现，所以钉在这里。
    ///
    /// 两个 bundle 无法合并成一份：桌面走 gpui 的 assets，移动端走 Flutter 的
    /// pubspec assets，各自的打包器只认自己目录下的文件。
    #[test]
    fn every_kind_has_an_icon_in_the_desktop_and_mobile_bundles() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|crates| crates.parent())
            .expect("repo root");
        let bundles = [
            repo.join("crates/smelt/assets/icons"),
            repo.join("mobile/assets/agent-icons"),
        ];

        let ids: std::collections::BTreeSet<&str> = ConversationAgentKind::ALL
            .iter()
            .map(|kind| kind.id())
            .chain(TerminalAgentKind::ALL.iter().map(|kind| kind.id()))
            .collect();

        for bundle in bundles {
            let notices = std::fs::read_to_string(bundle.join("THIRD_PARTY_NOTICES.md"))
                .unwrap_or_else(|err| panic!("{} 缺少第三方声明: {err}", bundle.display()));
            for id in &ids {
                let file = format!("agent-{id}.svg");
                assert!(
                    bundle.join(&file).is_file(),
                    "{} 缺少 {file}——那一家 agent 会掉回兜底图标",
                    bundle.display()
                );
                // 图标都是第三方品牌资源，放进 bundle 就得有出处。
                assert!(
                    notices.contains(&file),
                    "{} 的第三方声明没提到 {file}",
                    bundle.display()
                );
            }
        }
    }

    #[test]
    fn icon_asset_is_derived_from_stable_id() {
        for kind in ConversationAgentKind::ALL {
            assert_eq!(
                kind.icon_asset(),
                format!("smelt-icons/agent-{}.svg", kind.id())
            );
        }
        for kind in TerminalAgentKind::ALL {
            assert_eq!(
                kind.icon_asset(),
                format!("smelt-icons/agent-{}.svg", kind.id())
            );
        }
    }

    /// 存档契约：id 与 agent_ui.json 的键名都不能重名，也不能被顺手改掉
    /// （改了等于把用户自定义命令重置回默认）。
    #[test]
    fn descriptor_ids_and_config_keys_are_stable_and_unique() {
        let ids: Vec<_> = CONVERSATION_AGENTS.iter().map(|d| d.id).collect();
        let keys: Vec<_> = CONVERSATION_AGENTS.iter().map(|d| d.config_key).collect();
        assert_eq!(
            ids,
            [
                "claude", "copilot", "codex", "grok", "cursor", "opencode", "kiro", "pi", "dsh"
            ]
        );
        assert_eq!(
            keys,
            [
                "acp_cmd",
                "acp_copilot_cmd",
                "acp_codex_cmd",
                "acp_grok_cmd",
                "acp_cursor_cmd",
                "acp_opencode_cmd",
                "acp_kiro_cmd",
                "acp_pi_cmd",
                "acp_dsh_cmd"
            ]
        );

        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "agent_ui.json 键名重复");
    }

    #[test]
    fn acp_descriptor_lookup_by_id() {
        assert_eq!(
            conversation_descriptor("grok").map(|d| d.config_key),
            Some("acp_grok_cmd")
        );
        assert!(conversation_descriptor("nope").is_none());
    }

    /// 表驱动之后属性仍要保持原值：Grok 是唯一不收图的。
    #[test]
    fn only_grok_rejects_images() {
        for kind in ConversationAgentKind::ALL {
            assert_eq!(kind.accepts_images(), kind != ConversationAgentKind::Grok);
        }
        assert_eq!(ConversationAgentKind::Claude.label(), "Claude Code");
        assert_eq!(ConversationAgentKind::Copilot.short_label(), "Copilot");
        assert_eq!(ConversationAgentKind::Dsh.label(), "DeepSeek Harness");
        assert_eq!(ConversationAgentKind::Dsh.short_label(), "DeepSeek");
        assert_eq!(
            ConversationAgentKind::Codex.default_cmd(),
            default_acp_codex_cmd()
        );
    }

    #[test]
    fn opencode_default_acp_launch_uses_process_local_full_permissions() {
        let launch = ConversationAgentKind::OpenCode.default_launch();

        assert_eq!(launch.command, "opencode acp");
        assert_eq!(
            launch
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .map(String::as_str),
            Some(r#"{"permission":"allow"}"#)
        );
        for kind in ConversationAgentKind::ALL {
            if kind != ConversationAgentKind::OpenCode {
                assert!(kind.default_launch().env.is_empty());
            }
        }
    }

    #[test]
    fn terminal_kind_matches_cli_program_including_antigravity_alias() {
        assert_eq!(
            TerminalAgentKind::from_command_prefix("claude --dangerously-skip-permissions"),
            Some(TerminalAgentKind::Claude)
        );
        assert_eq!(
            TerminalAgentKind::from_command_prefix("copilot --allow-all"),
            Some(TerminalAgentKind::Copilot)
        );
        assert_eq!(
            TerminalAgentKind::from_command_prefix("grok --minimal"),
            Some(TerminalAgentKind::Grok)
        );
        assert_eq!(
            TerminalAgentKind::Grok.quick_terminal_cmd(),
            "grok --always-approve"
        );
        assert_eq!(
            TerminalAgentKind::from_command_prefix("agy"),
            Some(TerminalAgentKind::Antigravity)
        );
        assert_eq!(
            TerminalAgentKind::from_command_prefix("cursor-agent --force"),
            Some(TerminalAgentKind::Cursor)
        );
        assert_eq!(
            TerminalAgentKind::Cursor.quick_terminal_cmd(),
            "cursor-agent --force"
        );
        assert_eq!(
            TerminalAgentKind::from_command_prefix("opencode --auto"),
            Some(TerminalAgentKind::OpenCode)
        );
        assert_eq!(
            TerminalAgentKind::OpenCode.quick_terminal_cmd(),
            "opencode --auto"
        );
        assert_eq!(
            TerminalAgentKind::from_command_prefix("kiro-cli chat --v3 --trust-all-tools"),
            Some(TerminalAgentKind::Kiro)
        );
        assert_eq!(
            TerminalAgentKind::Kiro.quick_terminal_cmd(),
            "kiro-cli chat --v3 --trust-all-tools"
        );
        assert_eq!(
            TerminalAgentKind::from_command_prefix("pi"),
            Some(TerminalAgentKind::Pi)
        );
        assert_eq!(TerminalAgentKind::Pi.quick_terminal_cmd(), "pi --approve");
        let crush = TerminalAgentKind::from_command_prefix("crush --yolo")
            .expect("Crush CLI 应由终端 agent 注册表识别");
        assert_eq!(crush.id(), "crush");
        assert_eq!(crush.quick_terminal_cmd(), "crush --yolo");
        assert_eq!(
            TerminalAgentKind::Pi.upgrade_released_quick_terminal_command("pi"),
            Some("pi --approve")
        );
        assert_eq!(
            TerminalAgentKind::Pi.upgrade_released_quick_terminal_command("pi --model custom"),
            None,
            "用户自定义 Pi 命令不能被迁移覆盖"
        );
        // env 前缀不算「以 agent 开头」，严格版本认不出来。
        assert_eq!(
            TerminalAgentKind::from_command_prefix("CLAUDE_CONFIG_DIR=/tmp claude"),
            None
        );
        assert_eq!(TerminalAgentKind::from_command_prefix("zsh"), None);
        assert_eq!(
            TerminalAgentKind::from_id("antigravity"),
            Some(TerminalAgentKind::Antigravity)
        );
        assert_eq!(TerminalAgentKind::Antigravity.cli_program(), "agy");
        assert_eq!(
            TerminalAgentKind::Antigravity.quick_terminal_cmd(),
            "agy --dangerously-skip-permissions"
        );
    }

    #[test]
    fn codex_terminal_keeps_the_cli_default_title_policy() {
        let expected = "codex --dangerously-bypass-approvals-and-sandbox";
        let forced_thread_title = concat!(
            "codex --dangerously-bypass-approvals-and-sandbox ",
            "-c 'tui.terminal_title=[\"thread-title\"]'"
        );

        assert_eq!(TerminalAgentKind::Codex.quick_terminal_cmd(), expected);
        assert_eq!(
            TerminalAgentKind::Codex.upgrade_released_quick_terminal_command(forced_thread_title),
            Some(expected),
            "仍使用旧强制配置的内置项应恢复 Codex 自己的标题策略"
        );
        assert_eq!(
            TerminalAgentKind::Codex.upgrade_released_quick_terminal_command(expected),
            None
        );
    }

    #[test]
    fn from_command_loose_matches_adapter_and_cli_identifiers() {
        assert_eq!(
            ConversationAgentKind::from_command_loose("bunx --bun @agentclientprotocol/codex-acp"),
            Some(ConversationAgentKind::Codex)
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("bunx --bun pi-acp@0.0.33"),
            Some(ConversationAgentKind::Pi)
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("smelt-pi-agent"),
            Some(ConversationAgentKind::Pi)
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("copilot --acp"),
            Some(ConversationAgentKind::Copilot)
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("cursor-agent acp"),
            Some(ConversationAgentKind::Cursor)
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("opencode acp"),
            Some(ConversationAgentKind::OpenCode)
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose(
                "kiro-cli acp --agent-engine v3 --auth-method cli"
            ),
            Some(ConversationAgentKind::Kiro)
        );
        assert_eq!(
            ConversationAgentKind::Kiro.default_cmd(),
            "kiro-cli acp --agent-engine v3 --auth-method cli"
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("some-other-agent"),
            None
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("npx --yes api-key-helper"),
            None
        );
        assert_eq!(
            ConversationAgentKind::from_command_loose("copilot --acp"),
            Some(ConversationAgentKind::Copilot)
        );
        // 长 id 的历史包含匹配仍兼容自定义包装命令。
        assert_eq!(
            ConversationAgentKind::from_command_loose("mycodexadapter"),
            Some(ConversationAgentKind::Codex)
        );
    }

    #[test]
    fn managed_adapter_defaults_pin_the_verified_latest_versions() {
        assert_eq!(
            default_acp_cmd(),
            "bunx --bun @agentclientprotocol/claude-agent-acp@0.78.0"
        );
        assert_eq!(
            default_acp_codex_cmd(),
            "bunx --bun @agentclientprotocol/codex-acp@1.12.0"
        );
        assert_eq!(default_acp_pi_cmd(), "smelt-pi-agent");
    }

    #[test]
    fn managed_adapter_versions_only_upgrade_released_defaults() {
        assert_eq!(
            ConversationAgentKind::Claude.upgrade_released_default_command(
                "bunx --bun @agentclientprotocol/claude-agent-acp@0.59.0"
            ),
            Some("bunx --bun @agentclientprotocol/claude-agent-acp@0.78.0".into())
        );
        assert_eq!(
            ConversationAgentKind::Claude.upgrade_released_default_command(
                "bunx --bun @agentclientprotocol/claude-agent-acp@0.70.0"
            ),
            Some("bunx --bun @agentclientprotocol/claude-agent-acp@0.78.0".into())
        );
        assert_eq!(
            ConversationAgentKind::Codex.upgrade_released_default_command(
                "bunx --bun @agentclientprotocol/codex-acp@1.1.7"
            ),
            Some("bunx --bun @agentclientprotocol/codex-acp@1.12.0".into())
        );
        assert_eq!(
            ConversationAgentKind::Codex.upgrade_released_default_command(
                "bunx --bun @agentclientprotocol/codex-acp@1.6.2"
            ),
            Some("bunx --bun @agentclientprotocol/codex-acp@1.12.0".into())
        );
        assert!(
            ConversationAgentKind::Codex
                .upgrade_released_default_command(
                    "NO_BROWSER=1 bunx --bun @agentclientprotocol/codex-acp@1.1.7"
                )
                .is_none()
        );
        assert_eq!(
            ConversationAgentKind::Codex.upgrade_released_default_command("  codex app-server  "),
            Some("bunx --bun @agentclientprotocol/codex-acp@1.12.0".into())
        );
        assert_eq!(
            ConversationAgentKind::Copilot.upgrade_released_default_command("copilot --acp"),
            Some("copilot --acp --allow-all".into())
        );
        assert_eq!(
            ConversationAgentKind::Grok.upgrade_released_default_command("grok agent stdio"),
            Some("grok agent --always-approve stdio".into())
        );
        assert_eq!(
            ConversationAgentKind::Cursor.upgrade_released_default_command("cursor-agent acp"),
            Some("cursor-agent --force acp".into())
        );
        assert_eq!(
            ConversationAgentKind::Kiro.upgrade_released_default_command("kiro-cli acp"),
            Some("kiro-cli acp --agent-engine v3 --auth-method cli".into())
        );
        assert!(
            ConversationAgentKind::Copilot
                .upgrade_released_default_command("copilot --acp --custom")
                .is_none()
        );
        assert_eq!(
            ConversationAgentKind::Kiro
                .upgrade_released_default_command("kiro-cli acp --trust-all-tools"),
            Some("kiro-cli acp --agent-engine v3 --auth-method cli".into())
        );
        assert_eq!(
            ConversationAgentKind::Kiro.upgrade_released_default_command(
                "kiro-cli acp --agent-engine v3 --auth-method cli --trust-all-tools"
            ),
            Some("kiro-cli acp --agent-engine v3 --auth-method cli".into())
        );
        assert_eq!(
            ConversationAgentKind::Pi.upgrade_released_default_command("bunx --bun pi-acp@0.0.33"),
            Some("smelt-pi-agent".into())
        );
        assert!(
            ConversationAgentKind::Kiro
                .upgrade_released_default_command(
                    "kiro-cli acp --agent-engine v3 --auth-method cli"
                )
                .is_none()
        );
    }

    #[test]
    fn acp_factory_defaults_use_each_runtime_supported_permission_controls() {
        assert_eq!(
            ConversationAgentKind::Copilot.default_cmd(),
            "copilot --acp --allow-all"
        );
        assert_eq!(
            ConversationAgentKind::Grok.default_cmd(),
            "grok agent --always-approve stdio"
        );
        assert_eq!(
            ConversationAgentKind::Cursor.default_cmd(),
            "cursor-agent --force acp"
        );
        assert_eq!(
            ConversationAgentKind::Kiro.default_cmd(),
            "kiro-cli acp --agent-engine v3 --auth-method cli"
        );
        assert_eq!(ConversationAgentKind::Pi.default_cmd(), "smelt-pi-agent");
        assert_eq!(
            ConversationAgentKind::Pi.task_params().thinking_key,
            Some("thought_level")
        );
        assert_eq!(
            ConversationAgentKind::Claude.task_params().full_access_mode,
            Some("bypassPermissions")
        );
        assert_eq!(
            ConversationAgentKind::Codex.task_params().full_access_mode,
            Some("agent-full-access")
        );
    }

    #[test]
    fn launch_spec_collects_structured_env() {
        let spec = ConversationLaunchSpec::from_command("claude --print")
            .with_env("CLAUDE_CONFIG_DIR", "~/Library/Application Support/Claude")
            .with_env("XDG_CONFIG_HOME", "/Users/example/.config");

        assert_eq!(spec.command, "claude --print");
        assert_eq!(
            spec.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("~/Library/Application Support/Claude")
        );
        assert_eq!(
            spec.env.get("XDG_CONFIG_HOME").map(String::as_str),
            Some("/Users/example/.config")
        );
    }

    #[test]
    fn profile_launch_spec_keeps_workspace_path_with_spaces_in_one_env_value() {
        let profile = AcpProfile {
            id: "claude-quant".to_string(),
            kind_id: "claude".to_string(),
            label: "Claude Quant".to_string(),
            workspace_dir: "~/Library/Application Support/Claude Quant".to_string(),
        };

        let spec = profile.launch_spec().expect("有效 profile");
        assert_eq!(
            spec.command,
            profile.kind().expect("已知 Agent").default_cmd()
        );
        assert_eq!(
            spec.env
                .get(profile.env_var().expect("支持 workspace 覆盖"))
                .map(String::as_str),
            Some("~/Library/Application Support/Claude Quant")
        );
    }

    #[test]
    fn pi_profile_uses_the_agent_directory_override_for_tui_and_acp() {
        let profile = AcpProfile {
            id: "pi-team".to_string(),
            kind_id: "pi".to_string(),
            label: "Pi Team".to_string(),
            workspace_dir: "~/.pi/team".to_string(),
        };

        assert_eq!(profile.env_var().unwrap(), "PI_CODING_AGENT_DIR");
        let launch = profile.launch_spec().expect("Pi profile 应可启动");
        assert_eq!(launch.command, ConversationAgentKind::Pi.default_cmd());
        assert_eq!(
            launch.env.get("PI_CODING_AGENT_DIR").map(String::as_str),
            Some("~/.pi/team")
        );
    }

    #[test]
    fn unknown_profile_kind_cannot_be_launched_as_claude() {
        let profile = AcpProfile {
            id: "future-profile".to_string(),
            kind_id: "future-agent".to_string(),
            label: "Future Agent".to_string(),
            workspace_dir: "~/.future-agent".to_string(),
        };

        assert_eq!(profile.kind(), None);
        let error = profile.launch_spec().expect_err("未知 Agent 必须拒绝启动");
        assert!(error.contains("future-agent"));
    }
}

#[cfg(test)]
mod env_lines_tests {
    use super::{format_env_lines, parse_env_lines};

    #[test]
    fn parses_keys_values_and_skips_blanks_and_comments() {
        let env = parse_env_lines(
            "# DeepSeek\nDEEPSEEK_API_KEY=sk-abc\n\n  DEEPSEEK_BASE_URL=https://gw.example/v1  \n",
        )
        .expect("解析");

        assert_eq!(
            env.get("DEEPSEEK_API_KEY").map(String::as_str),
            Some("sk-abc")
        );
        assert_eq!(
            env.get("DEEPSEEK_BASE_URL").map(String::as_str),
            Some("https://gw.example/v1"),
            "粘贴 key/URL 常带尾随空格，必须去掉"
        );
        assert_eq!(env.len(), 2);
    }

    /// 值里的 `=` 属于值本身——JWT、base64、带 query 的 URL 都会中招。
    #[test]
    fn only_the_first_equals_separates() {
        let env = parse_env_lines("TOKEN=a=b==").expect("解析");
        assert_eq!(env.get("TOKEN").map(String::as_str), Some("a=b=="));
    }

    #[test]
    fn rejects_a_line_without_an_equals() {
        let error = parse_env_lines("DEEPSEEK_API_KEY sk-abc").expect_err("必须报错");
        assert!(error.contains("第 1 行"), "错误要指出行号：{error}");
    }

    #[test]
    fn rejects_an_illegal_name() {
        for bad in ["1KEY=v", "A-B=v", "A B=v"] {
            assert!(
                parse_env_lines(bad).is_err(),
                "`{bad}` 不是合法的 shell 标识符，dsh 的 apiKeyEnv 解析不到"
            );
        }
    }

    /// 这条挡的是最难自查的一类故障：改了 PATH，agent 起不来，报错却指向命令本身。
    #[test]
    fn rejects_variables_only_the_launching_environment_may_set() {
        for bad in ["PATH", "NODE_OPTIONS", "LD_PRELOAD"] {
            let error = parse_env_lines(&format!("{bad}=/tmp")).expect_err("{bad} 必须被拒绝");
            assert!(error.contains(bad), "错误要点名是哪个变量：{error}");
        }
    }

    #[test]
    fn rejects_a_duplicate_key_instead_of_silently_keeping_one() {
        assert!(parse_env_lines("K=1\nK=2").is_err());
    }

    #[test]
    fn round_trips_through_the_text_box() {
        let text = "A=1\nB=two words";
        let env = parse_env_lines(text).expect("解析");
        assert_eq!(format_env_lines(&env), text);
        assert_eq!(
            parse_env_lines(&format_env_lines(&env)).expect("再解析"),
            env
        );
    }
}
