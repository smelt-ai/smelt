//! 各家 agent 的技能目录能力表。
//!
//! 只收主流 agent：smelt 已集成通知 hook 的那几家，加上 Gemini CLI。不在表里的 agent，它目录
//! 下的技能不扫描、也不替它建兼容链接。
//!
//! 每个字段都是被真实数据逼出来的：
//! - `home` 单列，不从 `user_dir` 截首段：`~/.config/opencode` 这类截不出来，有的 agent 没有
//!   技能目录却有配置目录。「在用」就看用户主目录里有没有它。
//! - `user_dir` / `project_dir` 可以没有：Codex、Grok 压根没有自己的技能目录，`.agents/skills`
//!   就是它们的原生位置，给它们建目录是无用功。没有目录的 agent 也不能被单独勾选。
//! - `reads_agents_dir`：自己读 `.agents/skills` 的不需要兼容链接。多建反而有害，同一个技能会
//!   被数两遍。眼下只有 Claude Code 需要。Kiro、Antigravity 文档只写自家目录，产品上先按读
//!   `.agents/skills` 处理（2026-10-17 定）；要支持就把它们改回 `false`。
//!
//! 证据（核实于 2026-02）：
//! - Claude Code：code.claude.com/docs/en/skills，只列 `.claude/skills`。
//! - Codex：developers.openai.com/codex/skills/。
//! - Copilot：docs.github.com，copilot-cli/customize-copilot/add-skills。
//! - Cursor：cursor.com/docs/context/skills。
//! - Gemini CLI：geminicli.com/docs/cli/skills/。
//! - Grok：superagent-ai/grok-cli 的 README 和 src/utils/skills.ts。
//! - Antigravity：antigravity.google/docs/skills。
//! - Kiro：kiro.dev/docs/skills/。
//! - OpenCode：opencode.ai/docs/skills/。

/// 一家 agent 的技能目录。路径都相对用户主目录或项目根。
#[derive(Debug, PartialEq, Eq)]
pub struct Agent {
    pub label: &'static str,
    /// 配置目录：用户主目录里有它，这家 agent 就算在用。
    pub home: &'static str,
    pub user_dir: Option<&'static str>,
    pub project_dir: Option<&'static str>,
    pub reads_agents_dir: bool,
}

impl Agent {
    /// 这一层级里它自己的技能目录；没有就是只读 `.agents/skills`。
    pub fn dir(&self, project_scope: bool) -> Option<&'static str> {
        if project_scope {
            self.project_dir
        } else {
            self.user_dir
        }
    }
}

pub const AGENTS: &[Agent] = &[
    Agent {
        label: "Claude Code",
        home: ".claude",
        user_dir: Some(".claude/skills"),
        project_dir: Some(".claude/skills"),
        reads_agents_dir: false,
    },
    Agent {
        label: "Codex",
        home: ".codex",
        user_dir: None,
        project_dir: None,
        reads_agents_dir: true,
    },
    Agent {
        label: "Copilot",
        home: ".copilot",
        user_dir: Some(".copilot/skills"),
        project_dir: Some(".github/skills"),
        reads_agents_dir: true,
    },
    Agent {
        label: "Cursor",
        home: ".cursor",
        user_dir: Some(".cursor/skills"),
        project_dir: Some(".cursor/skills"),
        reads_agents_dir: true,
    },
    Agent {
        label: "Gemini CLI",
        home: ".gemini",
        user_dir: Some(".gemini/skills"),
        project_dir: Some(".gemini/skills"),
        reads_agents_dir: true,
    },
    Agent {
        label: "Grok",
        home: ".grok",
        user_dir: None,
        project_dir: None,
        reads_agents_dir: true,
    },
    Agent {
        label: "Antigravity",
        home: ".gemini/config",
        user_dir: Some(".gemini/config/skills"),
        project_dir: None,
        reads_agents_dir: true,
    },
    Agent {
        label: "Kiro",
        home: ".kiro",
        user_dir: Some(".kiro/skills"),
        project_dir: Some(".kiro/skills"),
        reads_agents_dir: true,
    },
    Agent {
        label: "OpenCode",
        home: ".config/opencode",
        user_dir: Some(".config/opencode/skills"),
        project_dir: Some(".opencode/skills"),
        reads_agents_dir: true,
    },
];

pub fn find(label: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|agent| agent.label == label)
}

/// 在表里的先后，排序用。
pub fn rank(label: &str) -> usize {
    AGENTS
        .iter()
        .position(|agent| agent.label == label)
        .unwrap_or(AGENTS.len())
}
