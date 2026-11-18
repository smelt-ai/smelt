import { describe, expect, test } from "bun:test";
import { SettingsManager } from "@earendil-works/pi-coding-agent";
import { installCacheWarmingDefault } from "../src/cache-warming-default.ts";

describe("cache warming default", () => {
	test("warms only while streaming when the user set nothing", () => {
		installCacheWarmingDefault();
		expect(SettingsManager.inMemory().getCacheWarmingMode()).toBe("streaming");
	});

	test("keeps the mode the user chose", () => {
		installCacheWarmingDefault();
		expect(SettingsManager.inMemory({ cacheWarming: "idle" }).getCacheWarmingMode()).toBe("idle");
		expect(SettingsManager.inMemory({ cacheWarming: "off" }).getCacheWarmingMode()).toBe("off");
	});
});
