import { describe, expect, test } from "bun:test";
import { Type } from "@earendil-works/pi-ai";
import {
	createEventBus,
	createExtensionRuntime,
	defineTool,
	ExtensionRunner,
	type ExtensionFactory,
	type ExtensionUIContext,
	type InlineExtension,
} from "@earendil-works/pi-coding-agent";
import { loadExtensionFromFactory } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/loader.js";
import {
	MULTI_SELECT_TITLE_MARK,
	SMELT_ELICITATION_EXTENSION_PATH,
	SMELT_ELICITATION_TOOL_NAME,
	parseAskUserQuestions,
} from "../src/elicitation.ts";
import { SMELT_EXTENSION_FACTORIES } from "../src/runtime-extensions.ts";

function toolConflicts(
	extensions: Array<{ path: string; tools: Map<string, unknown> }>,
): string[] {
	const owners = new Map<string, string>();
	const conflicts: string[] = [];
	for (const ext of extensions) {
		for (const name of ext.tools.keys()) {
			const existing = owners.get(name);
			if (existing && existing !== ext.path) {
				conflicts.push(`Tool "${name}" conflicts with ${existing}`);
			} else {
				owners.set(name, ext.path);
			}
		}
	}
	return conflicts;
}

const userAskUserQuestion: ExtensionFactory = (pi) => {
	pi.registerTool(
		defineTool({
			name: SMELT_ELICITATION_TOOL_NAME,
			label: "Ask User Question",
			description: "user-owned",
			parameters: Type.Object({}),
			async execute() {
				return { content: [{ type: "text" as const, text: "user" }], details: {} };
			},
		}),
	);
};

function inlineFactory(input: InlineExtension): ExtensionFactory {
	return typeof input === "function" ? input : input.factory;
}

function inlinePath(input: InlineExtension, index: number): string {
	return typeof input === "function" ? `<inline:${index + 1}>` : `<inline:${input.name}>`;
}

async function loadFactories(preceding: ExtensionFactory[] = []) {
	const runtime = createExtensionRuntime();
	const eventBus = createEventBus();
	const factories: InlineExtension[] = [...preceding, ...SMELT_EXTENSION_FACTORIES];
	const extensions = await Promise.all(
		factories.map((input, index) =>
			loadExtensionFromFactory(
				inlineFactory(input),
				process.cwd(),
				eventBus,
				runtime,
				index < preceding.length
					? "/Users/c.chen/.pi/agent/extensions/smelt_ask_user.ts"
					: inlinePath(input, index),
			),
		),
	);
	const runner = new ExtensionRunner(extensions, runtime, process.cwd(), {} as never, {} as never);
	return { runtime, runner, extensions };
}

async function loadHostExtensions(preceding: ExtensionFactory[] = []) {
	const loaded = await loadFactories(preceding);
	loaded.runtime.getAllTools = () =>
		loaded.runner.getAllRegisteredTools().map((tool) => ({
			name: tool.definition.name,
			description: tool.definition.description,
			parameters: tool.definition.parameters,
			promptGuidelines: tool.definition.promptGuidelines,
			sourceInfo: tool.sourceInfo,
		}));
	await loaded.runner.emit({ type: "session_start", reason: "startup" });
	return loaded;
}

describe("elicitation payload shape", () => {
	test("parses Claude-style questions with labels and descriptions", () => {
		const questions = parseAskUserQuestions({
			questions: [
				{
					question: "提醒走哪个渠道？",
					multiSelect: false,
					options: [
						{ label: "Gitea Issue", description: "仓库待办" },
						{ label: "Bark", description: "手机推送" },
					],
				},
			],
		});
		expect(questions).toEqual([
			{
				question: "提醒走哪个渠道？",
				multiSelect: false,
				options: [
					{ label: "Gitea Issue", description: "仓库待办" },
					{ label: "Bark", description: "手机推送" },
				],
			},
		]);
	});

	test("parses Pi question / questionnaire shapes", () => {
		expect(
			parseAskUserQuestions({
				question: "用哪种节奏？",
				options: ["紧凑续集", "开放番外"],
			}),
		).toEqual([
			{
				question: "用哪种节奏？",
				multiSelect: false,
				options: [{ label: "紧凑续集" }, { label: "开放番外" }],
			},
		]);
		expect(
			parseAskUserQuestions({
				questions: [
					{
						id: "scope",
						prompt: "范围？",
						options: [
							{ value: "small", label: "小" },
							{ value: "large", label: "大" },
						],
					},
				],
			}),
		).toHaveLength(1);
	});

	test("rejects payloads that cannot become a choice card", () => {
		expect(parseAskUserQuestions({})).toEqual([]);
		expect(parseAskUserQuestions({ questions: [{ question: "空的", options: [] }] })).toEqual([]);
	});
});

describe("host elicitation tool", () => {
	test("registers only the canonical Smelt tool", async () => {
		const { runner } = await loadHostExtensions();
		const names = runner.getAllRegisteredTools().map((tool) => tool.definition.name);
		expect(names).toEqual([SMELT_ELICITATION_TOOL_NAME]);
		expect(runner.getAllRegisteredTools()[0]?.sourceInfo.path).toBe(
			SMELT_ELICITATION_EXTENSION_PATH,
		);
		for (const alias of ["question", "questionnaire", "ask_user_question", "AskUserQuestion"]) {
			expect(runner.getToolDefinition(alias)).toBeUndefined();
		}
	});

	test("does not override a user tool that occupies the reserved name", async () => {
		const { runner, extensions } = await loadHostExtensions([userAskUserQuestion]);
		expect(runner.getToolDefinition(SMELT_ELICITATION_TOOL_NAME)?.description).toBe("user-owned");
		expect(toolConflicts(extensions)).toEqual([]);
	});

	test("shows a choice card through ui.select", async () => {
		const { runner } = await loadHostExtensions();
		const tool = runner.getToolDefinition(SMELT_ELICITATION_TOOL_NAME);
		expect(tool).toBeDefined();
		const shown: Array<{ title: string; options: string[] }> = [];
		const result = await tool!.execute(
			"tool-1",
			{ question: "用哪种节奏？", options: ["紧凑续集", "开放番外"] },
			new AbortController().signal,
			() => {},
			{
				ui: {
					select: async (title, options) => {
						shown.push({ title, options });
						return options[1];
					},
				} as ExtensionUIContext,
			} as never,
		);
		expect(shown).toEqual([{ title: "用哪种节奏？", options: ["紧凑续集", "开放番外"] }]);
		expect(result.content).toEqual([
			{
				type: "text",
				text: JSON.stringify([{ question: "用哪种节奏？", answer: "开放番外" }]),
			},
		]);
	});

	test("single-select preserves labels that look like JSON arrays", async () => {
		const { runner } = await loadHostExtensions();
		const tool = runner.getToolDefinition(SMELT_ELICITATION_TOOL_NAME);
		expect(tool).toBeDefined();
		const result = await tool!.execute(
			"tool-json-label",
			{ question: "选哪个原文？", options: ['["A"]', "普通标签"] },
			new AbortController().signal,
			() => {},
			{
				ui: {
					select: async () => '["A"]',
				} as unknown as ExtensionUIContext,
			} as never,
		);
		expect(result.content).toEqual([
			{
				type: "text",
				text: JSON.stringify([{ question: "选哪个原文？", answer: '["A"]' }]),
			},
		]);
	});

	test("multi-select returns every chosen label", async () => {
		const { runner } = await loadHostExtensions();
		const tool = runner.getToolDefinition(SMELT_ELICITATION_TOOL_NAME);
		expect(tool).toBeDefined();
		const shown: string[] = [];
		const result = await tool!.execute(
			"tool-2",
			{
				questions: [
					{
						question: "提醒走哪个渠道？",
						multiSelect: true,
						options: [
							{ label: "Gitea Issue", description: "仓库待办" },
							{ label: "Bark", description: "手机推送" },
							{ label: "邮件" },
						],
					},
				],
			},
			new AbortController().signal,
			() => {},
			{
				ui: {
					select: async (title, options) => {
						shown.push(title);
						return JSON.stringify([options[0], options[1]]);
					},
				} as ExtensionUIContext,
			} as never,
		);
		expect(shown).toEqual([`${MULTI_SELECT_TITLE_MARK}提醒走哪个渠道？`]);
		expect(result.content).toEqual([
			{
				type: "text",
				text: JSON.stringify([{ question: "提醒走哪个渠道？", answer: "Gitea Issue、Bark" }]),
			},
		]);
	});
});
