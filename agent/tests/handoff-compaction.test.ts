import { describe, expect, test } from "bun:test";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import {
	buildHandoffInstructions,
	DEFAULT_HANDOFF_INSTRUCTIONS,
	MAX_OVERFLOW_RETRIES,
	runHandoffCompaction,
	shrinkPreparation,
	summarizationModel,
	TRUNCATED_FOR_RETRY_MARKER,
} from "../src/handoff-compaction.ts";

describe("handoff compaction", () => {
	test("generates default structured handoff instructions", () => {
		const instructions = buildHandoffInstructions();
		expect(instructions).toBe(DEFAULT_HANDOFF_INSTRUCTIONS);
		expect(instructions).toContain("1. 任务核心目标 (Goal & Scope)");
		expect(instructions).toContain("2. 架构决策与排查发现 (Decisions & Key Findings)");
		expect(instructions).toContain("3. 已完成修改与落地成果 (Completed Changes)");
		expect(instructions).toContain("4. 当前进行中状态 (Current Status & Blockers)");
		expect(instructions).toContain("5. 明确的下一步行动清单 (Next Steps)");
		expect(instructions).toContain("6. 关键上下文参考 (Critical Context & Command References)");
	});

	test("appends user custom instructions under user focus section", () => {
		const instructions = buildHandoffInstructions("聚焦 backend 重构与单元测试");
		expect(instructions).toContain(DEFAULT_HANDOFF_INSTRUCTIONS);
		expect(instructions).toContain("用户附加交接要求 (User Focus):");
		expect(instructions).toContain("聚焦 backend 重构与单元测试");
	});

	test("returns undefined gracefully if context lacks model or registry", async () => {
		const dummyPreparation = {
			firstKeptEntryId: "entry-1",
			messagesToSummarize: [],
			turnPrefixMessages: [],
			isSplitTurn: false,
			tokensBefore: 100,
			fileOps: { read: new Set(), written: new Set(), modified: new Set() },
			settings: { enabled: true, reserveTokens: 1000, keepRecentTokens: 500 },
		} as any;

		const ctxWithoutModel = {
			model: undefined,
			modelRegistry: {} as any,
		} as ExtensionContext;

		const result = await runHandoffCompaction(dummyPreparation, ctxWithoutModel);
		expect(result).toBeUndefined();
	});

	test("returns undefined gracefully if model registry fails auth resolution", async () => {
		const dummyPreparation = {
			firstKeptEntryId: "entry-1",
			messagesToSummarize: [],
			turnPrefixMessages: [],
			isSplitTurn: false,
			tokensBefore: 100,
			fileOps: { read: new Set(), written: new Set(), modified: new Set() },
			settings: { enabled: true, reserveTokens: 1000, keepRecentTokens: 500 },
		} as any;

		const ctxWithFailedAuth = {
			model: { id: "test-model", provider: "openai" } as any,
			modelRegistry: {
				getApiKeyAndHeaders: async () => ({ ok: false, error: "Missing API key" }),
			} as any,
		} as ExtensionContext;

		const result = await runHandoffCompaction(dummyPreparation, ctxWithFailedAuth);
		expect(result).toBeUndefined();
	});

	test("caps summary output so a nearly full window still fits", () => {
		// 1M 窗口、384K 输出：82 万待摘要历史加上 384K 必然超窗。
		const model = { id: "deepseek-v4.1-flash", contextWindow: 1_048_576, maxTokens: 384_000 };
		const settings = { enabled: true, reserveTokens: 16_384, keepRecentTokens: 20_000 };
		const capped = summarizationModel(model, settings);
		expect(capped.maxTokens).toBe(13_107);
		expect(capped.contextWindow).toBe(1_048_576);
		expect(825_819 + capped.maxTokens).toBeLessThan(model.contextWindow);
		expect(model.maxTokens).toBe(384_000);
	});

	test("keeps a model whose output limit already fits the budget", () => {
		const model = { id: "small", maxTokens: 8_192 };
		const settings = { enabled: true, reserveTokens: 16_384, keepRecentTokens: 20_000 };
		expect(summarizationModel(model, settings)).toBe(model);
	});

	test("caps summary output when reserve grows with the window", () => {
		const model = { id: "big", maxTokens: 128_000 };
		const settings = { enabled: true, reserveTokens: 38_400, keepRecentTokens: 20_000 };
		expect(summarizationModel(model, settings).maxTokens).toBe(16_000);
	});
});

const OVERFLOW = new Error("Summarization failed: 400 Your input exceeds the context window of this model.");

function user(text: string) {
	return { role: "user", content: text, timestamp: 0 } as any;
}
function toolResult(text: string) {
	return { role: "toolResult", toolCallId: "t", toolName: "read", content: [{ type: "text", text }], isError: false, timestamp: 0 } as any;
}
function preparation(messages: any[], extra: Record<string, unknown> = {}) {
	return {
		firstKeptEntryId: "entry-1",
		messagesToSummarize: messages,
		turnPrefixMessages: [],
		isSplitTurn: false,
		tokensBefore: 100,
		fileOps: { read: new Set(), written: new Set(), edited: new Set() },
		settings: { enabled: true, reserveTokens: 16_384, keepRecentTokens: 20_000 },
		...extra,
	} as any;
}
const ctx = {
	model: { id: "m", provider: "openai", maxTokens: 8_000 },
	modelRegistry: { getApiKeyAndHeaders: async () => ({ ok: true, apiKey: "k" }) },
} as unknown as ExtensionContext;
const RESULT = { summary: "ok", firstKeptEntryId: "entry-1", tokensBefore: 100 } as any;

describe("compaction overflow retry", () => {
	test("drops the oldest fifth and marks the truncation", () => {
		const messages = Array.from({ length: 10 }, (_, i) => user(`m${i}`));
		const smaller = shrinkPreparation(preparation(messages))!;
		expect(smaller.messagesToSummarize.map((m: any) => m.content)).toEqual([
			TRUNCATED_FOR_RETRY_MARKER,
			"m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9",
		]);
		// 再缩一次不把标记算进历史，也不叠第二个标记
		const again = shrinkPreparation(smaller)!;
		expect(again.messagesToSummarize.map((m: any) => m.content)).toEqual([
			TRUNCATED_FOR_RETRY_MARKER,
			"m3", "m4", "m5", "m6", "m7", "m8", "m9",
		]);
	});

	test("does not leave a tool result without its call", () => {
		const messages = [user("a"), toolResult("r1"), toolResult("r2"), user("b"), user("c")];
		const smaller = shrinkPreparation(preparation(messages))!;
		expect(smaller.messagesToSummarize.map((m: any) => m.role)).toEqual(["user", "user", "user"]);
		expect((smaller.messagesToSummarize[1] as any).content).toBe("b");
	});

	test("falls back to the split turn prefix when history cannot shrink", () => {
		const prefix = Array.from({ length: 5 }, (_, i) => user(`p${i}`));
		const smaller = shrinkPreparation(preparation([], { isSplitTurn: true, turnPrefixMessages: prefix }))!;
		expect(smaller.turnPrefixMessages.map((m: any) => m.content)).toEqual([
			TRUNCATED_FOR_RETRY_MARKER, "p1", "p2", "p3", "p4",
		]);
		expect(shrinkPreparation(preparation([user("only")]))).toBeUndefined();
	});

	test("retries a context overflow with less history", async () => {
		const sizes: number[] = [];
		const compactFn = (async (prep: any) => {
			sizes.push(prep.messagesToSummarize.length);
			if (sizes.length < 3) throw OVERFLOW;
			return RESULT;
		}) as any;
		const messages = Array.from({ length: 10 }, (_, i) => user(`m${i}`));
		const result = await runHandoffCompaction(preparation(messages), ctx, undefined, undefined, compactFn);
		expect(result).toBe(RESULT);
		expect(sizes).toEqual([10, 9, 8]);
	});

	test("gives up after the retry limit", async () => {
		let calls = 0;
		const compactFn = (async () => {
			calls += 1;
			throw OVERFLOW;
		}) as any;
		const messages = Array.from({ length: 50 }, (_, i) => user(`m${i}`));
		const result = await runHandoffCompaction(preparation(messages), ctx, undefined, undefined, compactFn);
		expect(result).toBeUndefined();
		expect(calls).toBe(MAX_OVERFLOW_RETRIES + 1);
	});

	test("does not retry other errors", async () => {
		let calls = 0;
		const compactFn = (async () => {
			calls += 1;
			throw new Error("Summarization failed: 401 Unauthorized");
		}) as any;
		const messages = Array.from({ length: 10 }, (_, i) => user(`m${i}`));
		expect(await runHandoffCompaction(preparation(messages), ctx, undefined, undefined, compactFn)).toBeUndefined();
		expect(calls).toBe(1);
	});
});

