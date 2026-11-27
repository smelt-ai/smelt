import { describe, expect, test } from "bun:test";
import {
  brokenConfirmBody,
  describeAction,
  filterSkills,
  formatModified,
  migrationNotice,
  needsRepair,
  placeLabel,
  prettyPath,
  scopeChoices,
  scopeRequestOf,
  skillChip,
  skillSummary,
} from "../model.js";

const t = (key, params = {}) => {
  const dict = {
    "time.unknown": "Unknown time",
    "list.separator": ", ",
    "place.universal": "Universal",
    "place.legacy": "Old location",
    "place.conflict": `Conflict ×${params.count}`,
    "status.choose": "Choose one",
    "status.unread": "No agent reads it",
    "status.agents.one": "1 agent",
    "status.agents.other": `${params.count} agents`,
    "status.missing_one": `${params.agent} can't use it yet`,
    "status.missing_many": `${params.count} agents can't use it yet`,
    "step.copy": `Copy to ${params.path}`,
    "step.move": `Move ${params.from} → ${params.to}`,
    "step.link": `Create link ${params.path}`,
    "step.link_for": `Let ${params.agent} read it: ${params.path}`,
    "step.unlink": `Remove link ${params.path}`,
    "step.unlink_for": `${params.agent} stops reading: ${params.path}`,
    "step.trash": `Move to the trash folder: ${params.path}`,
    "migrate.moved.one": `Moved 1: ${params.names}`,
    "migrate.moved.other": `Moved ${params.count}: ${params.names}`,
    "migrate.skipped.one": `Skipped 1: ${params.names}`,
    "migrate.skipped.other": `Skipped ${params.count}: ${params.names}`,
    "migrate.none": "No skills to migrate.",
    "broken.more": `…${params.count} in total`,
    "broken.confirm_body": `Remove:\n${params.paths}`,
  };
  return dict[key] ?? key;
};

describe("skills model", () => {
  test("prettyPath collapses home prefix to ~", () => {
    expect(prettyPath("/Users/a", "/Users/a")).toBe("~");
    expect(prettyPath("/Users/a", "/Users/a/.agents/skills/demo")).toBe("~/.agents/skills/demo");
    expect(prettyPath("/Users/a", "/tmp/demo")).toBe("/tmp/demo");
  });

  test("formatModified handles valid and missing timestamps", () => {
    expect(formatModified(null, t)).toBe("Unknown time");
    const formatted = formatModified(Date.UTC(2026, 0, 2, 3, 4), t);
    expect(formatted).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });

  test("scopeChoices filters by scope and sorts installed agents first", () => {
    const agents = [
      { label: "Claude Code", has_user_dir: true, has_project_dir: true },
      { label: "Codex", has_user_dir: false, has_project_dir: false },
      { label: "Antigravity", has_user_dir: true, has_project_dir: false },
      { label: "Cursor", has_user_dir: true, has_project_dir: true },
    ];
    expect(scopeChoices(agents, ["Cursor"], false)).toEqual([
      { label: "Cursor", installed: true },
      { label: "Claude Code", installed: false },
      { label: "Antigravity", installed: false },
    ]);
    expect(scopeChoices(agents, ["Cursor"], true)).toEqual([
      { label: "Cursor", installed: true },
      { label: "Claude Code", installed: false },
    ]);
  });

  test("skillChip, skillSummary, needsRepair, scopeRequestOf, filterSkills", () => {
    const skill = {
      name: "demo",
      description: "helpful tool",
      project_scope: true,
      placement: { kind: "universal" },
      missing_bridges: ["Claude Code"],
      coverage: [
        { agent: "Codex", access: "reads" },
        { agent: "Claude Code", access: "missing" },
      ],
      conflicts: [],
    };
    expect(needsRepair(skill)).toBe(true);
    expect(skillChip(skill, t)).toEqual({ label: "Universal", tone: "neutral" });
    expect(skillSummary(skill, t)).toEqual({
      label: "Claude Code can't use it yet",
      tone: "need",
      badge: true,
    });
    expect(scopeRequestOf(skill)).toEqual({
      universal: true,
      labels: [],
      project_scope: true,
    });
    expect(filterSkills([skill], "HELP")).toHaveLength(1);
    expect(filterSkills([skill], "nope")).toHaveLength(0);

    const conflicted = { ...skill, conflicts: [{ dir: "/x" }] };
    expect(needsRepair(conflicted)).toBe(false);
    expect(skillChip(conflicted, t)).toEqual({ label: "Conflict ×2", tone: "bad" });
    expect(skillSummary(conflicted, t)).toEqual({ label: "Choose one", tone: "bad", badge: false });

    const agentOwned = { ...skill, placement: { kind: "agent", label: "Claude Code" } };
    expect(placeLabel(agentOwned.placement, t)).toBe("Claude Code");
    expect(scopeRequestOf(agentOwned)).toEqual({
      universal: false,
      labels: ["Claude Code"],
      project_scope: true,
    });
  });

  test("describeAction, migrationNotice, brokenConfirmBody", () => {
    expect(
      describeAction(
        "/Users/a",
        { kind: "link", at: "/Users/a/.claude/skills/demo", to: "/Users/a/.agents/skills/demo", agent: "Claude Code" },
        t,
      ),
    ).toBe("Let Claude Code read it: ~/.claude/skills/demo");
    expect(
      describeAction("/Users/a", { kind: "move", from: "/Users/a/a", to: "/Users/a/b" }, t),
    ).toBe("Move ~/a → ~/b");

    expect(migrationNotice({ migrated: ["a"], skipped: ["b"] }, t)).toEqual({
      error: true,
      text: "Moved 1: a\nSkipped 1: b",
    });
    expect(migrationNotice({ migrated: [], skipped: [] }, t)).toEqual({
      error: false,
      text: "No skills to migrate.",
    });

    const sixBroken = Array.from({ length: 6 }, (_, i) => ({ path: `/Users/a/.claude/skills/s${i}` }));
    const body = brokenConfirmBody("/Users/a", sixBroken, t);
    expect(body).toContain("~/.claude/skills/s0");
    expect(body).toContain("~/.claude/skills/s4");
    expect(body).not.toContain("~/.claude/skills/s5");
    expect(body).toContain("…6 in total");
  });
});
