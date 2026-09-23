import { describe, expect, test } from "bun:test";
import type { Skill } from "@earendil-works/pi-coding-agent";
import { formatSkillsForPrompt } from "@earendil-works/pi-coding-agent";
import {
	buildContextUsageBuckets,
	buildRuntimeDebugPayload,
	captureProviderRequest,
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
		const contextUsageIndex = SMELT_EXTENSION_FACTORIES.findIndex(
			(extension) => typeof extension !== "function" && extension.factory === smeltContextUsageExtension,
		);
		expect(contextUsageIndex).toBeGreaterThanOrEqual(0);
		expect(contextUsageIndex).toBeLessThan(SMELT_EXTENSION_FACTORIES.length - 1);
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

	test("runtime debug keeps the exact request config and provider payload", () => {
		const systemPrompt =
			"system line 1\n<project_context>真实上下文</project_context>";
		const tools = [
			{
				name: "bash",
				description: "Run a shell command",
				parameters: {
					type: "object",
					required: ["command"],
					properties: { command: { type: "string" } },
				},
				source: "builtin",
			},
		];
		const modelCall = captureProviderRequest(
			1,
			{
				model: "gpt-test",
				max_tokens: 4096,
				messages: [{ role: "user", content: "hello" }],
				api_key: "must-not-leak",
				headers: { authorization: "Bearer must-not-leak" },
			},
			{
				provider: "openai",
				id: "gpt-test",
				api: "responses",
				thinkingLevel: "high",
			},
		);

		expect(buildRuntimeDebugPayload(systemPrompt, tools, modelCall)).toEqual({
			version: 2,
			source: "pi_runtime_debug",
			systemPrompt,
			tools,
			modelCall: {
				sequence: 1,
				source: "pi_before_provider_request",
				model: {
					provider: "openai",
					id: "gpt-test",
					api: "responses",
					thinkingLevel: "high",
				},
				payload: {
					model: "gpt-test",
					max_tokens: 4096,
					messages: [{ role: "user", content: "hello" }],
					api_key: "[REDACTED]",
					headers: "[REDACTED]",
				},
				redactedPaths: ["$.api_key", "$.headers"],
			},
		});
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
