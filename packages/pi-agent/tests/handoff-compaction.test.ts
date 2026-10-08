import { describe, expect, test } from "bun:test";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import {
	buildHandoffInstructions,
	DEFAULT_HANDOFF_INSTRUCTIONS,
	runHandoffCompaction,
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
});
