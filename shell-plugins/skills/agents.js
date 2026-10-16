// agent 能力表，取自 plugins/skills/assets/agents.json（证据见那边的 DESIGN.md 附录）。
// 新插件独立于旧插件体系，所以复制一份而不是引用。

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
    "reads_agents_dir": {
      "user": false,
      "project": true
    }
  },
  {
    "label": "Kiro",
    "home": ".kiro",
    "user_dir": ".kiro/skills",
    "project_dir": ".kiro/skills",
    "reads_agents_dir": false
  },
  {
    "label": "OpenCode",
    "home": ".config/opencode",
    "user_dir": ".config/opencode/skills",
    "project_dir": ".opencode/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Amp",
    "home": ".config/amp",
    "user_dir": ".config/amp/skills",
    "project_dir": null,
    "reads_agents_dir": true
  },
  {
    "label": "Qwen Code",
    "home": ".qwen",
    "user_dir": ".qwen/skills",
    "project_dir": ".qwen/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Cline",
    "home": ".cline",
    "user_dir": ".cline/skills",
    "project_dir": ".cline/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Roo Code",
    "home": ".roo",
    "user_dir": ".roo/skills",
    "project_dir": ".roo/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Windsurf",
    "home": ".codeium",
    "user_dir": ".codeium/windsurf/skills",
    "project_dir": ".windsurf/skills",
    "reads_agents_dir": true
  },
  {
    "label": "Trae",
    "home": ".trae",
    "user_dir": ".trae/skills",
    "project_dir": ".trae/skills",
    "reads_agents_dir": true
  }
];
