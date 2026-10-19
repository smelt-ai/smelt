// agent 能力表。每个字段为什么存在、各家的证据，见同目录 DESIGN.md 的第三节和附录。
//
// 只收主流 agent：smelt 已集成通知 hook 的那几家（见 crates/smelt/src/settings/agent_hooks.rs），
// 加上 Gemini CLI。不在表里的 agent，它目录下的 skill 不扫描、也不替它建兼容链接。

export const AGENTS = [
  {
    "label": "Claude Code",
    "home": ".claude",
    "user_dir": ".claude/skills",
    "project_dir": ".claude/skills",
    "reads_agents_dir": false
  },
  {
    "label": "Codex",
    "home": ".codex",
    "user_dir": null,
    "project_dir": null,
    "reads_agents_dir": true
  },
  {
    "label": "Copilot",
    "home": ".copilot",
    "user_dir": ".copilot/skills",
    "project_dir": ".github/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Cursor",
    "home": ".cursor",
    "user_dir": ".cursor/skills",
    "project_dir": ".cursor/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Gemini CLI",
    "home": ".gemini",
    "user_dir": ".gemini/skills",
    "project_dir": ".gemini/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Grok",
    "home": ".grok",
    "user_dir": null,
    "project_dir": null,
    "reads_agents_dir": true
  },
  {
    "label": "Antigravity",
    "home": ".gemini/config",
    "user_dir": ".gemini/config/skills",
    "project_dir": null,
    "reads_agents_dir": true
  },
  {
    "label": "Kiro",
    "home": ".kiro",
    "user_dir": ".kiro/skills",
    "project_dir": ".kiro/skills",
    "reads_agents_dir": true
  },
  {
    "label": "OpenCode",
    "home": ".config/opencode",
    "user_dir": ".config/opencode/skills",
    "project_dir": ".opencode/skills",
    "reads_agents_dir": true
  }
];
