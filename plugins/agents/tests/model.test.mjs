import { describe, expect, test } from "bun:test";
import {
  agentCardPrompt,
  agentCatalogMonogram,
  agentConversationCount,
  agentConversationStartError,
  agentDisplayName,
  agentInstructionsFooter,
  agentUsageSummary,
  applyAutomationsPayload,
  automationNamesForAgent,
  automationNextRunAt,
  automationRunsForAgent,
  automationsForAgent,
  buildCapabilitiesList,
  cliExecutableForAgent,
  createAutomationsProjection,
  createEditQueue,
  filterCapabilities,
  formatAutomationRelativeUntil,
  formatAutomationTriggerSummary,
  formatClockHHMM,
  formatEnvLines,
  isCliExecutableAvailable,
  launchEntryIsBuiltin,
  memorySummaryText,
  newAgentDefinition,
  newConversationLabel,
  parseAgentHostSettings,
  parseEnvLines,
  pendingReasonText,
  planNewConversation,
  removeAgent,
  resolveNewConversation,
  retainAutomationsMachines,
  sessionPhaseSettled,
  validateProfile,
  visibleMemories,
} from "../model.js";

const dict = {
  "monogram.empty": "智",
  "agent.unnamed": "未命名",
  "new_conversation.title": "新对话",
  "new_conversation.named": "新对话 · %{name}",
  "instructions.footer": "会作为系统提示词带进每次对话和自动化。",
  "instructions.footer_count": "%{count} 字 · 会作为系统提示词带进每次对话和自动化",
  "error.no_engine": "请选择一个可用的执行引擎",
  "usage.none": "还没有对话",
  "usage.conversations.one": "1 段对话",
  "usage.conversations.other": "%{count} 段对话",
  "usage.automations.one": "1 条自动化",
  "usage.automations.other": "%{count} 条自动化",
  "memory.summary_empty": "还没有记忆",
  "memory.summary": "这个智能体 %{agent} 条 · 全局 %{user} 条",
  "memory.pending.external_content": "外部内容(%{sources})",
  "memory.pending.unattended": "无人值守",
  "plugins.kind.skill": "技能",
  "plugins.kind.extension": "扩展",
  "plugins.missing": "磁盘上已缺失",
  "mcp.disabled": "已停用",
  "mcp.missing": "已不在 mcp.json",
  "pick.unknown": "未知",
  "env.missing_equals": "L%{line}:eq:%{text}",
  "env.invalid_name": "L%{line}:name:%{name}",
  "env.launch_only": "L%{line}:launch:%{name}",
  "env.duplicate": "L%{line}:dup:%{name}",
  "custom_agent.unknown_kind": "unknown:%{kind}",
  "custom_agent.name_required": "name_required",
  "custom_agent.command_required": "cmd_required:%{name}",
  "custom_agent.data_dir_unsupported": "dir_unsupported:%{agent}",
  "custom_agent.data_dir_in_env": "dir_in_env:%{var}",
  "custom_agent.env_not_allowed": "env_not_allowed:%{name}",
};

function t(key, params = {}) {
  let s = dict[key] ?? key;
  for (const [k, v] of Object.entries(params)) {
    s = s.replaceAll(`%{${k}}`, String(v));
  }
  return s;
}

const ENGINES = ["pi"];
const CONVERSATION_AGENTS = [
  { id: "claude", label: "Claude Code", short_label: "Claude", config_dir_env_var: "CLAUDE_CONFIG_DIR" },
  { id: "pi", label: "Pi", short_label: "Pi", config_dir_env_var: "PI_CODING_AGENT_DIR" },
  { id: "cursor", label: "Cursor", short_label: "Cursor", config_dir_env_var: null },
];

describe("agents model", () => {
  test("parseAgentHostSettings validates shape and preserves commands: {}", () => {
    expect(parseAgentHostSettings({ agents: 1 }).ok).toBe(false);
    expect(parseAgentHostSettings(null).ok).toBe(false);
    const parsed = parseAgentHostSettings({
      hooks_enabled: false,
      agents: [{ id: "a1", name: "Writer", engine_kind: "pi" }],
      profiles: [{ id: "p1", kind_id: "claude", label: "Work", workspace_dir: "~/.w" }],
      default_conversation_agent: "a1",
      agent_spaces_root: "/home/u/.smelt/agents",
    });
    expect(parsed.ok).toBe(true);
    expect(parsed.settings.commands).toEqual({});
    expect(parsed.settings.agents[0].memory_enabled).toBe(true);
    expect(parsed.settings.profiles[0].command).toBe("");
    expect(parsed.settings.profiles[0].env).toEqual({});
    expect(parsed.settings.agent_spaces_root).toBe("/home/u/.smelt/agents");
  });

  test("removeAgent deletes definition and clears matching default only", () => {
    const cfg = {
      agents: [newAgentDefinition("a1", "One", "pi"), newAgentDefinition("a2", "Two", "pi")],
      default_conversation_agent: "a1",
    };
    removeAgent(cfg, "a2");
    expect(cfg.agents.map((a) => a.id)).toEqual(["a1"]);
    expect(cfg.default_conversation_agent).toBe("a1");

    removeAgent(cfg, "a1");
    expect(cfg.agents).toEqual([]);
    expect(cfg.default_conversation_agent).toBe(null);
  });

  test("resolveNewConversation & planNewConversation handle draft, default, and stale defaults", () => {
    const a1 = newAgentDefinition("a1", "Alpha", "pi");
    const a2 = newAgentDefinition("a2", "Beta", "unknown");

    const r1 = resolveNewConversation([a1, a2], "a2", null, ENGINES);
    expect(r1.intent).toEqual({ kind: "pick_agent" });
    expect(r1.clear_stored_default).toBe(true);
    expect(r1.ready).toEqual([{ id: "a1", name: "Alpha" }]);

    const rDraft = resolveNewConversation([a1], "a1", "a1", ENGINES);
    expect(rDraft.intent).toEqual({ kind: "create_agent" });
    expect(rDraft.ready).toEqual([]);

    const map = new Map([
      ["", { agents: [a2], default_conversation_agent: "gone" }],
      ["box", { agents: [a1], default_conversation_agent: "a1" }],
    ]);
    const plan = planNewConversation([null, "box"], map, null, ENGINES);
    expect(plan.intent).toEqual({ kind: "start", agent: { machine: "box", id: "a1" } });
    expect(plan.default_name).toBe("Alpha");
    expect(plan.stale_defaults).toEqual([null]);
  });

  test("visibleMemories sorts pending first, then SystemTime last_used desc, then name asc", () => {
    const entries = [
      {
        file_name: "b.md",
        name: "Bravo",
        description: "desc",
        kind: "project",
        body: "body",
        last_used: { secs_since_epoch: 100, nanos_since_epoch: 0 },
        pending_reason: null,
      },
      {
        file_name: "a.md",
        name: "Alpha",
        description: "desc",
        kind: "user",
        body: "rule body",
        last_used: { secs_since_epoch: 200, nanos_since_epoch: 0 },
        pending_reason: null,
      },
      {
        file_name: "p.md",
        name: "Zulu",
        description: "pending note",
        kind: "feedback",
        body: "check",
        last_used: null,
        pending_reason: "unattended",
      },
      {
        file_name: "c.md",
        name: "Charlie",
        description: "never used",
        kind: "reference",
        body: "link",
        last_used: null,
        pending_reason: null,
      },
    ];

    expect(visibleMemories(entries, "", "all").map((e) => e.name)).toEqual([
      "Zulu",
      "Alpha",
      "Bravo",
      "Charlie",
    ]);
    expect(visibleMemories(entries, "", "pending").map((e) => e.name)).toEqual(["Zulu"]);
    expect(visibleMemories(entries, "", "rules").map((e) => e.name)).toEqual(["Zulu", "Alpha"]);
    expect(visibleMemories(entries, "", "facts").map((e) => e.name)).toEqual(["Bravo", "Charlie"]);
    expect(visibleMemories(entries, "RULE BODY", "all").map((e) => e.name)).toEqual(["Alpha"]);
    expect(pendingReasonText("external_content:web_fetch", t)).toBe("外部内容(web_fetch)");
    expect(pendingReasonText("unattended", t)).toBe("无人值守");
    expect(pendingReasonText("legacy custom reason", t)).toBe("legacy custom reason");
  });

  test("automations projection handles revisions, retired store_ids, null data, and machine retention", () => {
    const proj = createAutomationsProjection();
    const payload = (storeId, revision, agentIds) => ({
      revision,
      data: {
        store_id: storeId,
        revision,
        automations: agentIds.map((id, ix) => ({
          id: `auto-${ix}`,
          name: ix === 2 ? "  " : `Auto ${ix}`,
          action: { type: "agent", agent_definition_id: id },
        })),
      },
    });

    expect(applyAutomationsPayload(proj, null, payload("s1", 1, ["a", "a", "b"]))).toBe(true);
    expect(applyAutomationsPayload(proj, "box", payload("r1", 1, ["a"]))).toBe(true);
    expect(automationsForAgent(proj, null, "a").length).toBe(2);
    expect(automationsForAgent(proj, "box", "a").length).toBe(1);

    // Older revision ignored
    expect(applyAutomationsPayload(proj, null, payload("s1", 1, ["b"]))).toBe(false);
    expect(automationsForAgent(proj, null, "a").length).toBe(2);

    // Newer store retires old store
    expect(applyAutomationsPayload(proj, null, payload("s2", 1, ["a", "b", "a"]))).toBe(true);
    expect(automationNamesForAgent(proj, null, "a")).toEqual(["Auto 0", null]);
    expect(applyAutomationsPayload(proj, null, payload("s1", 99, ["b"]))).toBe(false);

    // Null data clears machine
    expect(applyAutomationsPayload(proj, "box", { revision: 0, data: null })).toBe(true);
    expect(automationsForAgent(proj, "box", "a").length).toBe(0);

    expect(retainAutomationsMachines(proj, new Set())).toBe(true);
    expect(automationsForAgent(proj, null, "a").length).toBe(0);

    // States, runs sorting, and trigger/time formatting for the Automations tab
    expect(
      applyAutomationsPayload(proj, null, {
        revision: 1,
        data: {
          store_id: "s3",
          revision: 1,
          automations: [
            { id: "a1", name: "Daily", action: { type: "agent", agent_definition_id: "ag-1" } },
            { id: "a2", name: "Other", action: { type: "agent", agent_definition_id: "ag-2" } },
          ],
          states: [{ automation_id: "a1", next_run_at: 2000 }],
          runs: [
            { id: "r1", automation_id: "a1", created_at: 100, finished_at: 150, status: "completed" },
            { id: "r2", automation_id: "a1", created_at: 200, finished_at: null, status: "running" },
            { id: "r3", automation_id: "a2", created_at: 300, finished_at: 310, status: "failed" },
          ],
        },
      }),
    ).toBe(true);
    expect(automationNextRunAt(proj, null, "a1")).toBe(2000);
    expect(automationRunsForAgent(proj, null, "ag-1").map((r) => r.id)).toEqual(["r2", "r1"]);
    expect(
      formatAutomationTriggerSummary(
        { schedules: [{ type: "daily", hour: 9, minute: 5 }], webhooks: [{ endpoint: "tok" }] },
        t,
      ),
    ).toContain("automations.schedule.daily");
    expect(formatAutomationRelativeUntil(100, 200, t)).toBe("automations.time.due_now");
  });

  test("parseEnvLines, formatEnvLines, and validateProfile match smelt-model rules", () => {
    const ok = parseEnvLines("# comment\nFOO = bar baz \n_BAR_2=1=2", t);
    expect(ok).toEqual({ ok: true, env: { FOO: "bar baz", _BAR_2: "1=2" } });
    expect(formatEnvLines(ok.env)).toBe("FOO=bar baz\n_BAR_2=1=2");

    expect(parseEnvLines("NO_EQ", t)).toEqual({ ok: false, error: "L1:eq:NO_EQ" });
    expect(parseEnvLines("1BAD=x", t)).toEqual({ ok: false, error: "L1:name:1BAD" });
    expect(parseEnvLines("PATH=/bin", t)).toEqual({ ok: false, error: "L1:launch:PATH" });
    expect(parseEnvLines("A=1\nA=2", t)).toEqual({ ok: false, error: "L2:dup:A" });

    expect(
      validateProfile({ kind_id: "custom", label: "MyACP", command: "", env: {}, workspace_dir: "" }, CONVERSATION_AGENTS, t),
    ).toBe("cmd_required:MyACP");
    expect(
      validateProfile({ kind_id: "cursor", label: "Cur", command: "", env: {}, workspace_dir: "~/.c" }, CONVERSATION_AGENTS, t),
    ).toBe("dir_unsupported:Cursor");
    expect(
      validateProfile(
        { kind_id: "claude", label: "Work", command: "", env: { CLAUDE_CONFIG_DIR: "/x" }, workspace_dir: "" },
        CONVERSATION_AGENTS,
        t,
      ),
    ).toBe("dir_in_env:CLAUDE_CONFIG_DIR");
    expect(
      validateProfile(
        { kind_id: "claude", label: "Work", command: "", env: { ANTHROPIC_BASE_URL: "https://x" }, workspace_dir: "~/.w" },
        CONVERSATION_AGENTS,
        t,
      ),
    ).toBe(null);
  });

  test("createEditQueue serializes per-machine edits and chains returned settings", async () => {
    let remote = { count: 0, tags: [] };
    const calls = [];
    const enqueue = createEditQueue({
      getCurrent: () => remote,
      getUnavailable: () => null,
      notReceivedMessage: () => "not received",
      sendEdit: async (_machine, before, after) => {
        calls.push({ before: { ...before }, after: { ...after } });
        await new Promise((r) => setTimeout(r, 5));
        // Simulate server adding a server-side stamp
        const merged = { ...after, count: after.count + 10 };
        remote = merged;
        return merged;
      },
    });

    const p1 = enqueue(null, (draft) => {
      draft.tags.push("first");
      draft.count += 1;
    });
    const p2 = enqueue(null, (draft) => {
      draft.tags.push("second");
      draft.count += 1;
    });
    const [r1, r2] = await Promise.all([p1, p2]);
    expect(r1).toEqual({ count: 11, tags: ["first"] });
    expect(r2).toEqual({ count: 22, tags: ["first", "second"] });
    expect(calls.length).toBe(2);
    expect(calls[1].before).toEqual(r1);
  });

  test("capabilities list includes missing checked plugins & MCPs when loaded and filters cleanly", () => {
    const agent = {
      plugins: ["skill:alpha", "extension:gone"],
      mcp_servers: ["srv1", "old-mcp"],
    };
    const piConfig = {
      plugins: [{ id: "skill:alpha", kind: "skill", name: "alpha", description: "Alpha skill", origin: "~/.pi", broken: null }],
      mcp_servers: { Ok: [{ name: "srv1", description: "Search", enabled: false, transport: { type: "http", url: "https://mcp" } }] },
    };
    const rows = buildCapabilitiesList(agent, piConfig, true, t);
    expect(rows.map((r) => r.key)).toEqual(["plugin:skill:alpha", "mcp:srv1", "plugin:extension:gone", "mcp:old-mcp"]);
    expect(rows[1].broken).toBe("已停用");
    expect(rows[2].broken).toBe("磁盘上已缺失");
    expect(rows[3].broken).toBe("已不在 mcp.json");
    expect(filterCapabilities(rows, "search").map((r) => r.id)).toEqual(["srv1"]);
  });

  test("display, monogram, session phase, launch builtin, and conversation count helpers", () => {
    expect(agentCatalogMonogram("  ", t)).toBe("智");
    expect(agentCatalogMonogram("日志助手", t)).toBe("日志");
    expect(agentCatalogMonogram("Pi Agent", t)).toBe("Pi");
    expect(agentDisplayName("  ", t)).toBe("未命名");
    expect(newConversationLabel("写作助手", t)).toBe("新对话 · 写作助手");
    expect(newConversationLabel(null, t)).toBe("新对话");
    expect(agentCardPrompt("   ")).toBe(null);
    expect(agentInstructionsFooter("", t)).toContain("系统提示词");
    expect(agentInstructionsFooter("abc", t)).toContain("3 字");
    expect(agentConversationStartError({ engine_kind: "pi" }, ENGINES, t)).toBe(null);
    expect(agentConversationStartError({ engine_kind: "x" }, ENGINES, t)).toBe("请选择一个可用的执行引擎");
    expect(agentUsageSummary(0, 0, t)).toBe("还没有对话");
    expect(agentUsageSummary(2, 1, t)).toBe("2 段对话 · 1 条自动化");
    expect(memorySummaryText(0, 0, t)).toBe("还没有记忆");
    expect(memorySummaryText(2, 3, t)).toBe("这个智能体 2 条 · 全局 3 条");
    expect(sessionPhaseSettled("thinking")).toBe(false);
    expect(sessionPhaseSettled("idle")).toBe(true);
    expect(launchEntryIsBuiltin({ provider: "claude" }, ["claude", "codex"])).toBe(true);
    expect(launchEntryIsBuiltin({ provider: null }, ["claude"])).toBe(false);
    expect(
      agentConversationCount(
        {
          conversations: [
            { archived_at: null, acp: { agent_definition_id: "a1", automation_id: null } },
            { archived_at: 123, acp: { agent_definition_id: "a1", automation_id: null } },
            { archived_at: null, acp: { agent_definition_id: "a1", automation_id: "auto-1" } },
          ],
        },
        "a1",
      ),
    ).toBe(1);
    const diag = { acp: { claude: { path: "/usr/bin/claude" } }, terminal: { pi: { path: null } } };
    expect(isCliExecutableAvailable(cliExecutableForAgent(diag, "claude"))).toBe(true);
    expect(isCliExecutableAvailable(cliExecutableForAgent(diag, "pi"))).toBe(false);
    expect(formatClockHHMM(new Date(2026, 0, 1, 9, 5))).toBe("09:05");
  });
});
