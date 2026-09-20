import { describe, expect, test } from "bun:test";
import type { Skill } from "@earendil-works/pi-coding-agent";
import { formatSkillsForPrompt } from "@earendil-works/pi-coding-agent";
import {
	buildContextUsageBuckets,
	estimateTokens,
	formatProjectContext,
} from "../src/context-usage.ts";
import { SMELT_EXTENSION_FACTORIES } from "../src/runtime-extensions.ts";
import smeltContextUsageExtension from "../src/context-usage.ts";

function skill(name: string, description: string): Skill {
	return {
		name,
		description,
		filePath: `/skills/${name}/SKILL.md`,
		baseDir: `/skills/${name}`,
		disableModelInvocation: false,
		sourceInfo: { source: "test", path: `/skills/${name}/SKILL.md` },
	} as Skill;
}

describe("context usage buckets", () => {
	test("host ships the context-usage extension before permission", () => {
		expect(SMELT_EXTENSION_FACTORIES).toContain(smeltContextUsageExtension);
		expect(SMELT_EXTENSION_FACTORIES.at(-1)).not.toBe(smeltContextUsageExtension);
	});

	test("maps Pi parts onto Cursor-style buckets", () => {
		const files = [{ path: "AGENTS.md", content: "use sqlite".repeat(20) }];
		const skills = [skill("fx-deploy", "Trigger FX pipelines and wait for nodes.")];
		const systemPrompt =
			"You are a coding agent.\nBe concise.\n" +
			formatProjectContext(files) +
			formatSkillsForPrompt(skills);
		const buckets = buildContextUsageBuckets({
			systemPrompt,
			contextFiles: files,
			skills,
			tools: [
				{
					name: "bash",
					description: "Run a shell command",
					parameters: { type: "object", properties: { command: { type: "string" } } },
					source: "builtin",
				},
				{
					name: "multica",
					description: "Calendar tool",
					parameters: { type: "object" },
					source: "extension",
				},
				{
					name: "subagent",
					description: "Start a nested agent",
					parameters: { type: "object" },
					source: "builtin",
				},
			],
			messages: [
				{ role: "user", content: "fix the title bar" },
				{ role: "assistant", content: "looking" },
				{ role: "toolResult", content: "file contents ".repeat(30) },
				{ summarized: true, content: "earlier work was compacted" },
			],
		});
		expect(buckets.systemPrompt).toBeGreaterThan(0);
		expect(buckets.rules).toBe(estimateTokens(formatProjectContext(files)));
		expect(buckets.skills).toBeGreaterThan(0);
		expect(buckets.toolsDefinition).toBeGreaterThan(0);
		expect(buckets.mcpDynamic).toBeGreaterThan(0);
		expect(buckets.subagent).toBeGreaterThan(0);
		expect(buckets.summarized).toBeGreaterThan(0);
		expect(buckets.conversation).toBeGreaterThan(0);
		expect(buckets.systemPrompt + buckets.rules + buckets.skills).toBe(
			estimateTokens(systemPrompt),
		);
	});

	test("empty parts stay at zero", () => {
		expect(
			buildContextUsageBuckets({
				systemPrompt: "",
				contextFiles: [],
				skills: [],
				tools: [],
				messages: [],
			}),
		).toEqual({
			systemPrompt: 0,
			toolsDefinition: 0,
			rules: 0,
			skills: 0,
			mcpDynamic: 0,
			subagent: 0,
			summarized: 0,
			conversation: 0,
		});
	});
});
