import { describe, expect, test } from "bun:test";
import {
  activeMentionQuery,
  applyMentionCompletion,
  buildMemberAddParams,
  buildMentionDecisionCall,
  filterMentionCandidates,
  groupBlindSubmissions,
  mergeGroupsAcrossMachines,
  parseComposerCommand,
  resolveBlindParticipantIds,
  totalGroupAttention,
} from "../model.js";

describe("plugins/groups model", () => {
  const members = [
    { alias: "claude", display_name: "Claude", role_kind: "coordinator", agent: "claude" },
    { alias: "codex", display_name: "Codex", role_kind: "worker", agent: "codex" },
    { alias: "agy", display_name: "Antigravity", role_kind: "reviewer", agent: "antigravity" },
  ];

  test("activeMentionQuery and completion handle @prefix accurately", () => {
    expect(activeMentionQuery("请 @co")).toBe("co");
    expect(activeMentionQuery("@")).toBe("");
    expect(activeMentionQuery("email@test.com")).toBeNull();
    expect(applyMentionCompletion("请 @co", "codex")).toBe("请 @codex ");
    const filtered = filterMentionCandidates(members, "c").map((c) => c.alias);
    expect(filtered).toEqual(["claude", "codex"]);
  });

  test("parseComposerCommand supports /blind, /task, /decision, and chat", () => {
    const blind = parseComposerCommand("/blind @claude @codex 评审鉴权设计", members);
    expect(blind).toEqual({
      kind: "blind",
      aliases: ["claude", "codex"],
      prompt: "评审鉴权设计",
    });
    const blindAll = parseComposerCommand("/blind 评审全文", members);
    expect(blindAll?.aliases).toEqual(["claude", "codex", "agy"]);

    expect(parseComposerCommand("/task @codex 实现刷新", members)).toEqual({
      kind: "task",
      content: "@codex 实现刷新",
    });
    expect(parseComposerCommand("/decision 采用独立 groups.sqlite3", members)).toEqual({
      kind: "decision",
      content: "采用独立 groups.sqlite3",
    });
  });

  test("mergeGroupsAcrossMachines sorts by latest activity and tallies attention", () => {
    const byMachine = {
      __local__: [
        {
          group: { id: "g1", updated_at_ms: 100 },
          waiting_approval_count: 1,
          pending_mention_approval_count: 0,
          last_message_at_ms: 150,
        },
      ],
      "rt-gpu": [
        {
          group: { id: "g2", updated_at_ms: 200 },
          waiting_approval_count: 0,
          pending_mention_approval_count: 2,
          last_message_at_ms: 300,
        },
      ],
    };
    const merged = mergeGroupsAcrossMachines(byMachine);
    expect(merged.map((g) => [g.machine, g.group.id])).toEqual([
      ["rt-gpu", "g2"],
      [null, "g1"],
    ]);
    expect(totalGroupAttention(byMachine)).toBe(3);
  });

  test("groupBlindSubmissions groups submissions by prompt seq", () => {
    const map = groupBlindSubmissions([
      { seq: 1, kind: "blind_prompt", content: "Q" },
      { seq: 2, kind: "blind_submission", reply_to_seq: 1, content: "A1" },
      { seq: 3, kind: "blind_submission", reply_to_seq: 1, content: "A2" },
    ]);
    expect(map.get(1)?.length).toBe(2);
  });

  test("wire payload helpers match Rust Control API contracts", () => {
    const addPayload = buildMemberAddParams("grp-1", {
      alias: "Reviewer",
      agentOptionId: "codex",
      roleKind: "reviewer",
      roleTitle: "Security Reviewer",
      workspaceMode: "shared_readonly",
      ownership: "group_owned",
      runtimeId: null,
      sessionId: null,
      cwd: "/repo",
    });
    expect(addPayload.group_id).toBe("grp-1");
    expect(addPayload.member.alias).toBe("reviewer");
    expect(addPayload.member.tool_restriction_profile).toBe("readonly");

    const approveCall = buildMentionDecisionCall("grp-1", "m1", "gm-1", true, true);
    expect(approveCall.method).toBe("group.mention.approve");
    expect(approveCall.params.allow_chain).toBe(true);

    const rejectCall = buildMentionDecisionCall("grp-1", "m1", "gm-1", false, true);
    expect(rejectCall.method).toBe("group.mention.reject");
    expect(rejectCall.params.allow_chain).toBe(false);

    const participants = resolveBlindParticipantIds(
      ["codex", "claude"],
      [
        { id: "gm-c", alias: "claude" },
        { id: "gm-x", alias: "codex" },
      ],
    );
    expect(participants).toEqual(["gm-x", "gm-c"]);
  });
});
