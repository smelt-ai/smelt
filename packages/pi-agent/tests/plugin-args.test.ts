import { describe, expect, test } from "bun:test";
import {
	SMELT_AGENT_PLUGIN_ARGS_ENV,
	consumePluginArgs,
} from "../src/plugin-args.ts";

describe("agent plugin selection", () => {
	test("becomes Pi argv and is not inherited by tools", () => {
		const environment: Record<string, string | undefined> = {
			[SMELT_AGENT_PLUGIN_ARGS_ENV]: JSON.stringify([
				"--no-skills",
				"--no-extensions",
				"--skill",
				"/Users/me/.pi/agent/skills/my skill",
				"-e",
				"/Users/me/.pi/agent/extensions/hook.ts",
			]),
		};
		const argv = ["bun", "src/main.ts"];

		expect(consumePluginArgs(environment, argv)).toBe(true);
		// 路径里的空格必须原样保留，这正是走 JSON 而不是拼命令行的原因。
		expect(argv).toEqual([
			"bun",
			"src/main.ts",
			"--no-skills",
			"--no-extensions",
			"--skill",
			"/Users/me/.pi/agent/skills/my skill",
			"-e",
			"/Users/me/.pi/agent/extensions/hook.ts",
		]);
		expect(environment[SMELT_AGENT_PLUGIN_ARGS_ENV]).toBeUndefined();
	});

	test("an empty selection still disables discovery", () => {
		const environment: Record<string, string | undefined> = {
			[SMELT_AGENT_PLUGIN_ARGS_ENV]: JSON.stringify([
				"--no-skills",
				"--no-extensions",
			]),
		};
		const argv = ["bun", "src/main.ts"];

		expect(consumePluginArgs(environment, argv)).toBe(true);
		expect(argv).toEqual([
			"bun",
			"src/main.ts",
			"--no-skills",
			"--no-extensions",
		]);
	});

	test("absent or malformed values leave argv untouched", () => {
		for (const raw of [undefined, "", "   ", "not json", '{"a":1}', "[]"]) {
			const environment: Record<string, string | undefined> = {
				[SMELT_AGENT_PLUGIN_ARGS_ENV]: raw,
			};
			const argv = ["bun", "src/main.ts"];
			expect(consumePluginArgs(environment, argv)).toBe(false);
			expect(argv).toEqual(["bun", "src/main.ts"]);
			expect(environment[SMELT_AGENT_PLUGIN_ARGS_ENV]).toBeUndefined();
		}
	});

	test("non-string entries are dropped instead of reaching Pi", () => {
		const environment: Record<string, string | undefined> = {
			[SMELT_AGENT_PLUGIN_ARGS_ENV]: JSON.stringify([
				"--no-skills",
				42,
				null,
				"--skill",
				"/a",
			]),
		};
		const argv: string[] = [];
		expect(consumePluginArgs(environment, argv)).toBe(true);
		expect(argv).toEqual(["--no-skills", "--skill", "/a"]);
	});
});
