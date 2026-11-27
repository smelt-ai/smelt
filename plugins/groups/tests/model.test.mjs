import { expect, test } from "bun:test";
import {
  activeMentionPrefix,
  applyMentionSuggestion,
  linkableConversations,
  mergeAppendedMessage,
  normalizeAlias,
  pendingApprovalsInMessages,
  prependOlderMessages,
  shouldRefreshActiveGroupDetail,
  sortGroupSummaries,
  suggestAliasFromTitle,
} from "../model.js";

test("normalizeAlias enforces lowercase ASCII and rejects reserved names", () => {
  expect(normalizeAlias(" @Coder_1 ")).toEqual({ ok: true, value: "coder_1" });
  expect(normalizeAlias("")).toEqual({ ok: false, code: "length" });
  expect(normalizeAlias("-bad")).toEqual({ ok: false, code: "format" });
  expect(normalizeAlias("all")).toEqual({ ok: false, code: "reserved", value: "all" });
  expect(normalizeAlias("system")).toEqual({ ok: false, code: "reserved", value: "system" });
});

test("mention autocomplete extracts and inserts @alias cleanly", () => {
  expect(activeMentionPrefix("please check @co")).toBe("co");
  expect(activeMentionPrefix("@rev")).toBe("rev");
  expect(activeMentionPrefix("hello world")).toBeNull();
  expect(applyMentionSuggestion("please check @co", "coder")).toBe("please check @coder ");
});

test("mergeAppendedMessage and prependOlderMessages deduplicate and sort by seq", () => {
  const initial = [
    { id: "m1", seq: 1, content: "a" },
    { id: "m3", seq: 3, content: "c" },
  ];
  const merged = mergeAppendedMessage(initial, { id: "m2", seq: 2, content: "b" });
  expect(merged.map((m) => m.seq)).toEqual([1, 2, 3]);
  const updated = mergeAppendedMessage(merged, { id: "m2", seq: 2, content: "b2" });
  expect(updated).toHaveLength(3);
  expect(updated[1].content).toBe("b2");

  const withOlder = prependOlderMessages(updated, [{ id: "m0", seq: 0, content: "zero" }]);
  expect(withOlder.map((m) => m.seq)).toEqual([0, 1, 2, 3]);
});

test("shouldRefreshActiveGroupDetail avoids redundant fetches on message-only updates", () => {
  const detail = {
    id: "grp-1",
    title: "API",
    description: "",
    mode: "mention",
    paused: false,
    archived_at_ms: null,
    coordinator_member_id: null,
    members: [{ id: "mem-1", removed_at_ms: null }],
    open_tasks: [],
  };
  const sameMeta = {
    id: "grp-1",
    title: "API",
    description: "",
    mode: "mention",
    paused: false,
    archived_at_ms: null,
    coordinator_member_id: null,
    active_member_count: 1,
    open_task_count: 0,
    pending_approval_count: 0,
  };
  expect(shouldRefreshActiveGroupDetail(detail, sameMeta)).toBe(false);
  expect(shouldRefreshActiveGroupDetail(detail, { ...sameMeta, paused: true })).toBe(true);
  expect(shouldRefreshActiveGroupDetail(detail, { ...sameMeta, active_member_count: 2 })).toBe(true);
});

test("pendingApprovalsInMessages, sortGroupSummaries, and linkableConversations work", () => {
  const pending = pendingApprovalsInMessages([
    {
      id: "m1",
      seq: 1,
      mentions: [
        { target_member_id: "mem-a", delivery_status: "delivered" },
        { target_member_id: "mem-b", delivery_status: "pending_approval" },
      ],
    },
  ]);
  expect(pending).toHaveLength(1);
  expect(pending[0].mention.target_member_id).toBe("mem-b");

  const sorted = sortGroupSummaries([
    { id: "b", updated_at_ms: 100, archived_at_ms: 50 },
    { id: "a", updated_at_ms: 200, archived_at_ms: null },
    { id: "c", updated_at_ms: 300, archived_at_ms: null },
  ]);
  expect(sorted.map((g) => g.id)).toEqual(["c", "a", "b"]);

  const linkable = linkableConversations(
    {
      conversations: [
        { id: "c1", custom_title: "Backend", acp: { sid: "acp-1", agent: "codex", cwd: "/repo" } },
        { id: "c2", custom_title: "Already", acp: { sid: "acp-2", agent: "pi", cwd: "/repo" } },
      ],
    },
    new Set(["acp-2"]),
  );
  expect(linkable).toHaveLength(1);
  expect(linkable[0].session_id).toBe("acp-1");
  expect(suggestAliasFromTitle("Backend API!")).toBe("backend-api");
});
