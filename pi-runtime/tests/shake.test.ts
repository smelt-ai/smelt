import { describe, expect, test } from "bun:test";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import {
	extractFilePath,
	extractSearchQuery,
	isAlreadyShaken,
	isSearchEmpty,
	shakeMessages,
	truncateBulkyCommandOutput,
} from "../src/shake.ts";

describe("shake context pruning", () => {
	test("extracts file path from various tool arguments", () => {
		expect(extractFilePath({ path: "src/main.rs" })).toBe("src/main.rs");
		expect(extractFilePath({ filePath: "src/lib.rs" })).toBe("src/lib.rs");
		expect(extractFilePath({ AbsolutePath: "/repo/file.ts" })).toBe("/repo/file.ts");
		expect(extractFilePath({ file: "README.md" })).toBe("README.md");
		expect(extractFilePath({ TargetFile: "foo.txt" })).toBe("foo.txt");
		expect(extractFilePath(undefined)).toBeUndefined();
		expect(extractFilePath("invalid")).toBeUndefined();
	});

	test("extracts search queries", () => {
		expect(extractSearchQuery({ query: "findMe" })).toBe("findMe");
		expect(extractSearchQuery({ pattern: "*.ts" })).toBe("*.ts");
		expect(extractSearchQuery({ path: "src" })).toBe("src");
		expect(extractSearchQuery(null)).toBeUndefined();
	});

	test("identifies empty search outputs", () => {
		expect(isSearchEmpty("")).toBe(true);
		expect(isSearchEmpty("   ")).toBe(true);
		expect(isSearchEmpty("[]")).toBe(true);
		expect(isSearchEmpty("No matches found")).toBe(true);
		expect(isSearchEmpty("no files matching pattern")).toBe(true);
		expect(isSearchEmpty("0 matches")).toBe(true);
		expect(isSearchEmpty("found 5 matches")).toBe(false);
	});

	test("truncates bulky command output preserving head and tail", () => {
		const lines = Array.from({ length: 60 }, (_, i) => `log line ${i + 1}`);
		const text = lines.join("\n");
		const res = truncateBulkyCommandOutput(text, false, 30, 500);
		expect(res.truncated).toBe(true);
		expect(res.omittedLines).toBe(45);
		expect(res.text).toContain("log line 1");
		expect(res.text).toContain("log line 5");
		expect(res.text).toContain("... [45 lines of command output elided by shake; exit code 0] ...");
		expect(res.text).toContain("log line 60");
		expect(isAlreadyShaken(res.text)).toBe(true);
	});

	test("short command output is not truncated", () => {
		const text = "line 1\nline 2\nline 3";
		const res = truncateBulkyCommandOutput(text, false, 30, 500);
		expect(res.truncated).toBe(false);
		expect(res.text).toBe(text);
	});

	test("does not prune tool results in the protected recent turn", () => {
		const messages: AgentMessage[] = [
			{ role: "user", content: "read file" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "call-1", name: "read", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "call-1",
				toolName: "read",
				content: [{ type: "text", text: "line 1\nline 2\nline 3" }],
			} as any,
		];
		// Turn 1 is the latest turn. Should not be pruned.
		const result = shakeMessages(messages);
		expect(result.shakenCount).toBe(0);
		expect(result.messages).toBe(messages);
	});

	test("prunes stale read after subsequent write across turns", () => {
		const messages: AgentMessage[] = [
			// Turn 1
			{ role: "user", content: "read a.txt" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "call-read-1", name: "read", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "call-read-1",
				toolName: "read",
				content: [{ type: "text", text: "old file content ".repeat(50) }],
			} as any,
			// Turn 2
			{ role: "user", content: "now edit a.txt" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "call-write-1", name: "write", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "call-write-1",
				toolName: "write",
				content: [{ type: "text", text: "file updated" }],
			} as any,
			// Turn 3
			{ role: "user", content: "what next?" } as any,
		];

		const result = shakeMessages(messages);
		expect(result.shakenCount).toBe(1);
		expect(result.savedTokens).toBeGreaterThan(0);
		expect(result.prunedToolCalls).toEqual(["call-read-1"]);

		const shakenReadResult = result.messages[2] as any;
		expect(shakenReadResult.content[0].text).toBe(
			'[Stale read of "a.txt" elided by shake; file was subsequently modified]',
		);
	});

	test("prunes superseded read when file is read again in a later turn", () => {
		const messages: AgentMessage[] = [
			// Turn 1
			{ role: "user", content: "check a.txt" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "read-1", name: "read", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "read-1",
				toolName: "read",
				content: [{ type: "text", text: "first read content ".repeat(20) }],
			} as any,
			// Turn 2
			{ role: "user", content: "re-read a.txt" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "read-2", name: "read", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "read-2",
				toolName: "read",
				content: [{ type: "text", text: "second read content" }],
			} as any,
			// Turn 3
			{ role: "user", content: "done" } as any,
		];

		const result = shakeMessages(messages);
		expect(result.shakenCount).toBe(1);
		expect(result.prunedToolCalls).toEqual(["read-1"]);
		const shakenReadResult = result.messages[2] as any;
		expect(shakenReadResult.content[0].text).toBe(
			'[Superseded read of "a.txt" elided by shake; file was re-read later]',
		);
	});

	test("prunes superseded edit when file is edited again in a later turn", () => {
		const messages: AgentMessage[] = [
			// Turn 1
			{ role: "user", content: "edit a.txt" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "edit-1", name: "edit", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "edit-1",
				toolName: "edit",
				content: [{ type: "text", text: "intermediate diff details ".repeat(20) }],
			} as any,
			// Turn 2
			{ role: "user", content: "edit a.txt again" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "edit-2", name: "edit", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "edit-2",
				toolName: "edit",
				content: [{ type: "text", text: "second edit diff" }],
			} as any,
			// Turn 3
			{ role: "user", content: "status" } as any,
		];

		const result = shakeMessages(messages);
		expect(result.shakenCount).toBe(1);
		expect(result.prunedToolCalls).toEqual(["edit-1"]);
		const shakenEditResult = result.messages[2] as any;
		expect(shakenEditResult.content[0].text).toBe(
			'[Superseded edit of "a.txt" elided by shake; newer edit applied later]',
		);
	});

	test("prunes empty search results from earlier turns", () => {
		const messages: AgentMessage[] = [
			// Turn 1
			{ role: "user", content: "search for missing" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "grep-1", name: "grep", arguments: { query: "missingSymbol" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "grep-1",
				toolName: "grep",
				content: [{ type: "text", text: "No matches found" }],
			} as any,
			// Turn 2
			{ role: "user", content: "ok search for other" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "grep-2", name: "grep", arguments: { query: "otherSymbol" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "grep-2",
				toolName: "grep",
				content: [{ type: "text", text: "src/foo.ts:1: found" }],
			} as any,
			// Turn 3
			{ role: "user", content: "continue" } as any,
		];

		const result = shakeMessages(messages);
		expect(result.shakenCount).toBe(1);
		expect(result.prunedToolCalls).toEqual(["grep-1"]);
		const shakenSearchResult = result.messages[2] as any;
		expect(shakenSearchResult.content[0].text).toBe(
			'[Empty search for "missingSymbol" elided by shake]',
		);
	});

	test("truncates bulky earlier command output", () => {
		const longOutput = Array.from({ length: 50 }, (_, i) => `build line ${i + 1}`).join("\n");
		const messages: AgentMessage[] = [
			// Turn 1
			{ role: "user", content: "build" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "bash-1", name: "bash", arguments: { command: "cargo build" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "bash-1",
				toolName: "bash",
				content: [{ type: "text", text: longOutput }],
			} as any,
			// Turn 2
			{ role: "user", content: "test" } as any,
		];

		const result = shakeMessages(messages, { maxCommandLines: 20 });
		expect(result.shakenCount).toBe(1);
		expect(result.prunedToolCalls).toEqual(["bash-1"]);
		const shakenBash = result.messages[2] as any;
		expect(shakenBash.content[0].text).toContain("build line 1");
		expect(shakenBash.content[0].text).toContain("elided by shake");
		expect(shakenBash.content[0].text).toContain("build line 50");
	});

	test("is idempotent when executed multiple times", () => {
		const messages: AgentMessage[] = [
			{ role: "user", content: "read a.txt" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "read-1", name: "read", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "read-1",
				toolName: "read",
				content: [{ type: "text", text: "stale data ".repeat(30) }],
			} as any,
			{ role: "user", content: "write a.txt" } as any,
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "write-1", name: "write", arguments: { path: "a.txt" } }],
			} as any,
			{
				role: "toolResult",
				toolCallId: "write-1",
				toolName: "write",
				content: [{ type: "text", text: "written" }],
			} as any,
			{ role: "user", content: "done" } as any,
		];

		const firstShake = shakeMessages(messages);
		expect(firstShake.shakenCount).toBe(1);

		const secondShake = shakeMessages(firstShake.messages);
		expect(secondShake.shakenCount).toBe(0);
	});

	test("prunes historical thinking blocks while preserving signature and recent turn", () => {
		const messages: AgentMessage[] = [
			// Turn 1
			{ role: "user", content: "first question" } as any,
			{
				role: "assistant",
				content: [
					{
						type: "thinking",
						thinking: "Let me ponder about the mysteries of the universe in deep detail...".repeat(5),
						thinkingSignature: "sig_abc_123",
					},
					{ type: "text", text: "Answer 1" },
				],
			} as any,
			// Turn 2
			{ role: "user", content: "second question" } as any,
			{
				role: "assistant",
				content: [
					{
						type: "thinking",
						thinking: "Fresh thinking for turn 2 that should NOT be pruned",
						thinkingSignature: "sig_def_456",
					},
					{ type: "text", text: "Answer 2" },
				],
			} as any,
		];

		const result = shakeMessages(messages);
		expect(result.prunedThinkingCount).toBe(1);
		expect(result.shakenCount).toBe(1);
		expect(result.savedTokens).toBeGreaterThan(0);

		// Turn 1 thinking is pruned, signature is preserved
		const turn1Assistant = result.messages[1] as any;
		expect(turn1Assistant.content[0].type).toBe("thinking");
		expect(turn1Assistant.content[0].thinking).toContain("[Historical thinking process");
		expect(turn1Assistant.content[0].thinking).toContain("elided by shake");
		expect(turn1Assistant.content[0].thinkingSignature).toBe("sig_abc_123");
		expect(turn1Assistant.content[1].text).toBe("Answer 1");

		// Turn 2 (latest turn) thinking is untouched
		const turn2Assistant = result.messages[3] as any;
		expect(turn2Assistant.content[0].thinking).toBe(
			"Fresh thinking for turn 2 that should NOT be pruned",
		);
		expect(turn2Assistant.content[0].thinkingSignature).toBe("sig_def_456");

		// Idempotency: re-running does not prune already pruned thinking
		const rerun = shakeMessages(result.messages);
		expect(rerun.prunedThinkingCount).toBe(0);
		expect(rerun.shakenCount).toBe(0);
	});

	test("does not prune thinking blocks when shakeThinking is false", () => {
		const messages: AgentMessage[] = [
			// Turn 1
			{ role: "user", content: "first question" } as any,
			{
				role: "assistant",
				content: [
					{
						type: "thinking",
						thinking: "Detailed reasoning that user wants to retain",
					},
				],
			} as any,
			// Turn 2
			{ role: "user", content: "second question" } as any,
		];

		const result = shakeMessages(messages, { shakeThinking: false });
		expect(result.prunedThinkingCount).toBe(0);
		expect(result.shakenCount).toBe(0);
		const turn1Assistant = result.messages[1] as any;
		expect(turn1Assistant.content[0].thinking).toBe(
			"Detailed reasoning that user wants to retain",
		);
	});
});
