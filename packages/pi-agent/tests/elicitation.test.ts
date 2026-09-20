import { describe, expect, test } from "bun:test";
import { Type } from "@earendil-works/pi-ai";
import {
	createEventBus,
	createExtensionRuntime,
	defineTool,
	ExtensionRunner,
	type ExtensionFactory,
	type ExtensionUIContext,
} from "@earendil-works/pi-coding-agent";
import { loadExtensionFromFactory } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/loader.js";
import {
	HOST_ELICITATION_TOOL_NAMES,
	MULTI_SELECT_TITLE_MARK,
	isElicitationInput,
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
			name: "ask_user_question",
			label: "Ask User Question",
			description: "user-owned",
			parameters: Type.Object({}),
			async execute() {
				return { content: [{ type: "text" as const, text: "user" }], details: {} };
			},
		}),
	);
};

async function loadFactories(preceding: ExtensionFactory[] = []) {
	const runtime = createExtensionRuntime();
	const eventBus = createEventBus();
	const factories = [...preceding, ...SMELT_EXTENSION_FACTORIES];
	const extensions = await Promise.all(
		factories.map((factory, index) =>
			loadExtensionFromFactory(
				factory,
				process.cwd(),
				eventBus,
				runtime,
				index < preceding.length ? "/Users/c.chen/.pi/agent/extensions/ask_user_question.ts" : `<inline:${index}>`,
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
		expect(isElicitationInput({ command: "rm -rf /" })).toBe(false);
		expect(isElicitationInput({ path: "src/main.ts" })).toBe(false);
	});
});

describe("host elicitation tool", () => {
	test("registers one implementation under the names models actually call", async () => {
		const { runner } = await loadHostExtensions();
		const names = runner.getAllRegisteredTools().map((tool) => tool.definition.name);
		expect(names).toEqual([...HOST_ELICITATION_TOOL_NAMES]);
	});

	test("does not register ask_user_question during load when ~/.pi already has it", async () => {
		const { extensions } = await loadFactories([userAskUserQuestion]);
		expect(toolConflicts(extensions)).toEqual([]);
	});

	test("skips user-owned names after session start and fills in the rest", async () => {
		const { runner, extensions } = await loadHostExtensions([userAskUserQuestion]);
		expect(runner.getToolDefinition("ask_user_question")?.description).toBe("user-owned");
		expect(runner.getToolDefinition("question")).toBeDefined();
		expect(toolConflicts(extensions)).toEqual([]);
	});

	test("shows a choice card through ui.select", async () => {
		const { runner } = await loadHostExtensions();
		const tool = runner.getToolDefinition("question");
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

	test("multi-select returns every chosen label", async () => {
		const { runner } = await loadHostExtensions();
		const tool = runner.getToolDefinition("ask_user_question");
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
