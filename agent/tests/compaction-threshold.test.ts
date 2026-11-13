import { describe, expect, test } from "bun:test";
import { SettingsManager, shouldCompact } from "@earendil-works/pi-coding-agent";
import { compactionReserveTokens, installCompactionThreshold } from "../src/compaction-threshold.ts";

describe("compaction threshold", () => {
	test("reserves 15% of the window", () => {
		expect(compactionReserveTokens(16_384, 256_000)).toBe(38_400);
		expect(compactionReserveTokens(16_384, 1_000_000)).toBe(150_000);
	});

	test("keeps a larger configured reserve and small windows", () => {
		expect(compactionReserveTokens(60_000, 256_000)).toBe(60_000);
		expect(compactionReserveTokens(16_384, 32_000)).toBe(16_384);
		expect(compactionReserveTokens(16_384, undefined)).toBe(16_384);
	});

	test("compacts a 256K window at 85% instead of 93.6%", () => {
		installCompactionThreshold();
		const model = { provider: "p", id: "m", contextWindow: 256_000 };
		const settings = SettingsManager.inMemory().getCompactionSettings(model as any);
		expect(settings.reserveTokens).toBe(38_400);
		// 截图里的 237.6K：原来不压，现在压
		expect(shouldCompact(237_600, 256_000, settings)).toBe(true);
		expect(shouldCompact(217_000, 256_000, settings)).toBe(false);
	});
});
