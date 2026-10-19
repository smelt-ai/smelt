import { describe, expect, test } from "bun:test";
import { Type } from "@earendil-works/pi-ai";
import {
	createEventBus,
	createExtensionRuntime,
	defineTool,
	ExtensionRunner,
	type ExtensionFactory,
} from "@earendil-works/pi-coding-agent";
import { loadExtensionFromFactory } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/loader.js";
import smeltSubagentExtension, {
	SMELT_SUBAGENT_EXTENSION_PATH,
	SMELT_SUBAGENT_TOOL_NAME,
	backgroundSubagentCommand,
	subagentShouldBackground,
} from "../src/subagent/index.ts";

function bindToolRegistry(
	runtime: ReturnType<typeof createExtensionRuntime>,
	runner: ExtensionRunner,
): void {
	runtime.getAllTools = () =>
		runner.getAllRegisteredTools().map((tool) => ({
			name: tool.definition.name,
			description: tool.definition.description,
			parameters: tool.definition.parameters,
			promptGuidelines: tool.definition.promptGuidelines,
			exposure: tool.definition.exposure ?? "direct",
			sourceInfo: tool.sourceInfo,
		}));
}

const userSubagentExtension: ExtensionFactory = (pi) => {
	pi.registerTool(
		defineTool({
			name: SMELT_SUBAGENT_TOOL_NAME,
			label: "User Subagent",
			description: "user-owned subagent implementation",
			parameters: Type.Object({}),
			async execute() {
				return { content: [{ type: "text" as const, text: "user" }], details: {} };
			},
		}),
	);
};

async function loadSubagentExtensions(preceding: ExtensionFactory[] = []) {
	const runtime = createExtensionRuntime();
	const eventBus = createEventBus();
	const factories = [
		...preceding.map((factory, index) => ({ factory, path: `<user:${index}>` })),
		{ factory: smeltSubagentExtension, path: SMELT_SUBAGENT_EXTENSION_PATH },
	];
	const extensions = await Promise.all(
		factories.map(({ factory, path }) =>
			loadExtensionFromFactory(factory, process.cwd(), eventBus, runtime, path),
		),
	);
	const runner = new ExtensionRunner(extensions, runtime, process.cwd(), {} as never, {} as never);
	bindToolRegistry(runtime, runner);
	await runner.emit({ type: "session_start", reason: "startup" });
	return runner;
}

describe("Smelt Pi subagent extension", () => {
	test("registers a built-in subagent tool when no user extension provides one", async () => {
		const runner = await loadSubagentExtensions();
		const tool = runner.getAllRegisteredTools().find((candidate) => candidate.definition.name === "subagent");

		expect(tool?.sourceInfo.path).toBe(SMELT_SUBAGENT_EXTENSION_PATH);
		expect(tool?.definition.description).toContain("isolated subagent");
	});

	test("preserves a user-provided subagent extension without registering a duplicate", async () => {
		const runner = await loadSubagentExtensions([userSubagentExtension]);
		const tools = runner.getAllRegisteredTools().filter((candidate) => candidate.definition.name === "subagent");

		expect(tools).toHaveLength(1);
		expect(tools[0]?.sourceInfo.path).toBe("<user:0>");
		expect(tools[0]?.definition.description).toBe("user-owned subagent implementation");
	});

	test("describes a background subagent by what it is doing", () => {
		expect(
			backgroundSubagentCommand({ tasks: [{ task: "看 a.rs", label: "审计 a" }, { task: "看 b.rs", agent: "reviewer" }] }),
		).toBe("审计 a · reviewer");
		expect(backgroundSubagentCommand({ task: "迁移数据库" })).toBe("迁移数据库");
	});

	test("rejects too many parallel tasks before going to the background", async () => {
		const runner = await loadSubagentExtensions();
		const tool = runner.getAllRegisteredTools().find((candidate) => candidate.definition.name === "subagent");
		const tasks = Array.from({ length: 11 }, (_, index) => ({ task: `audit ${index}` }));
		const result = (await tool!.definition.execute(
			"call-1",
			{ tasks } as never,
			undefined,
			undefined,
			{ cwd: process.cwd(), hasUI: false } as never,
		)) as { content: Array<{ text: string }>; isError?: boolean };

		expect(result.isError).toBe(true);
		expect(result.content[0]?.text).toContain("Too many parallel tasks (11)");
		expect(result.content[0]?.text).not.toContain("已在后台启动");
	});

	test("runs single and parallel subagents in the background unless asked to wait", () => {
		expect(subagentShouldBackground({ task: "查日志" } as never)).toBe(true);
		expect(subagentShouldBackground({ background: false } as never)).toBe(false);
		expect(subagentShouldBackground({ chain: [{ task: "先查" }, { task: "再改" }] })).toBe(false);
	});
});
