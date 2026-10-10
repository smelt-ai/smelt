import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fauxAssistantMessage, fauxProvider } from "@earendil-works/pi-ai";
import {
	createAgentSession,
	createEventBus,
	createExtensionRuntime,
	DefaultResourceLoader,
	ExtensionRunner,
	ModelRuntime,
	SessionManager,
	SettingsManager,
	type ExtensionFactory,
	type ExtensionUIContext,
	type InlineExtension,
	type SourceInfo,
} from "@earendil-works/pi-coding-agent";
import { loadExtensionFromFactory } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/loader.js";
import {
	SMELT_AGENT_MEMORY_DIR_ENV,
	SMELT_USER_MEMORY_DIR_ENV,
	consumeAgentMemory,
	resetMemoryRootsForTests,
} from "../src/agent-memory.ts";
import smeltMemoryExtension, {
	SMELT_MEMORY_PROMPT_SECTION,
	SMELT_MEMORY_RECALL_MESSAGE,
} from "../src/memory-extension.ts";
import { readMemoryUsage } from "../src/memory-usage.ts";
import { SMELT_EXTENSION_FACTORIES } from "../src/runtime-extensions.ts";

const temps: string[] = [];
afterEach(() => {
	for (const dir of temps.splice(0)) rmSync(dir, { recursive: true, force: true });
	resetMemoryRootsForTests();
});

function setupRoots(options: { consume?: boolean } = {}) {
	const base = realpathSync(mkdtempSync(join(tmpdir(), "smelt-memory-ext-")));
	temps.push(base);
	const agent = join(base, "agents", "writer", "memory");
	const user = join(base, "memory");
	mkdirSync(agent, { recursive: true });
	mkdirSync(user, { recursive: true });
	if (options.consume !== false) {
		consumeAgentMemory({ [SMELT_AGENT_MEMORY_DIR_ENV]: agent, [SMELT_USER_MEMORY_DIR_ENV]: user });
	}
	return { base, agent, user };
}

const builtinSource = (name: string): SourceInfo => ({
	path: `builtin:${name}`,
	source: "builtin",
	scope: "temporary",
	origin: "top-level",
});

function inlineFactory(input: InlineExtension): ExtensionFactory {
	return typeof input === "function" ? input : input.factory;
}

async function createRunner(options: { spoofWrite?: boolean } = {}) {
	const runtime = createExtensionRuntime();
	const extensions = await Promise.all(
		SMELT_EXTENSION_FACTORIES.map((input, index) =>
			loadExtensionFromFactory(
				inlineFactory(input),
				process.cwd(),
				createEventBus(),
				runtime,
				typeof input === "function" ? `<inline:${index + 1}>` : `<inline:${input.name}>`,
			),
		),
	);
	const runner = new ExtensionRunner(extensions, runtime, process.cwd(), {} as never, {} as never);
	runtime.getAllTools = () =>
		["read", "write", "edit", "bash"].map((name) => ({
			name,
			description: name,
			parameters: {} as never,
			exposure: "direct" as const,
			sourceInfo:
				options.spoofWrite && name === "write"
					? { path: "/tmp/user-write.ts", source: "local", scope: "user", origin: "top-level" }
					: builtinSource(name),
		}));
	const confirms: unknown[] = [];
	const notices: string[] = [];
	runner.setUIContext({
		confirm: async (_title: string, message: string) => {
			confirms.push(JSON.parse(message).input);
			return true;
		},
		notify: (message: string) => notices.push(message),
	} as unknown as ExtensionUIContext);
	await runner.emit({ type: "session_start", reason: "startup" });
	return { runner, confirms, notices };
}

async function memorySectionFor(runner: ExtensionRunner) {
	const result = await runner.emitBeforeAgentStart("hi", undefined, { cwd: process.cwd() });
	expect(result?.systemPromptOptions.forceSystemPrompt).toBeUndefined();
	return result?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION];
}

const validMemory = "---\nname: reply-in-chinese\ndescription: 用户要求中文回复\ntype: feedback\n---\n回复用中文。\n";

describe("smelt-memory extension", () => {
	test("a valid memory write by the built-in write tool skips host approval", async () => {
		const { agent } = setupRoots();
		const { runner, confirms } = await createRunner();

		const result = await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "w1",
			toolName: "write",
			input: { path: join(agent, "reply-in-chinese.md"), content: validMemory },
		});

		expect(result?.block).toBeFalsy();
		expect(confirms).toEqual([]);
	});

	test("an invalid memory write is blocked with a reason the model can act on", async () => {
		const { agent } = setupRoots();
		const { runner, confirms } = await createRunner();

		const result = await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "w2",
			toolName: "write",
			input: { path: join(agent, "note.md"), content: "随手记一笔" },
		});

		expect(result?.block).toBe(true);
		expect(result?.reason).toContain("frontmatter");
		expect(confirms).toEqual([]);
	});

	test("writes outside memory, bash, and spoofed write tools still ask the user", async () => {
		const { base, agent } = setupRoots();
		const { runner, confirms } = await createRunner();
		await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "o1",
			toolName: "write",
			input: { path: join(base, "notes.md"), content: validMemory },
		});
		await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "o2",
			toolName: "bash",
			input: { command: `echo hi > ${join(agent, "MEMORY.md")}` },
		});
		expect(confirms).toHaveLength(2);

		const spoofed = await createRunner({ spoofWrite: true });
		await spoofed.runner.emitToolCall({
			type: "tool_call",
			toolCallId: "o3",
			toolName: "write",
			input: { path: join(agent, "reply-in-chinese.md"), content: validMemory },
		});
		expect(spoofed.confirms).toHaveLength(1);
	});

	test("without a memory session the memory folder is an ordinary path", async () => {
		const { agent } = setupRoots({ consume: false });
		const { runner, confirms } = await createRunner();
		await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "n1",
			toolName: "write",
			input: { path: join(agent, "reply-in-chinese.md"), content: validMemory },
		});
		expect(confirms).toHaveLength(1);
	});

	test("an edit that leaves an invalid memory file is rolled back and reported as an error", async () => {
		const { agent } = setupRoots();
		const file = join(agent, "reply-in-chinese.md");
		writeFileSync(file, validMemory);
		const { runner, confirms } = await createRunner();
		const input = { path: file, edits: [{ oldText: "type: feedback", newText: "type: diary" }] };

		const call = await runner.emitToolCall({ type: "tool_call", toolCallId: "e1", toolName: "edit", input });
		expect(call?.block).toBeFalsy();
		expect(confirms).toEqual([]);
		writeFileSync(file, validMemory.replace("type: feedback", "type: diary"));

		const result = await runner.emitToolResult({
			type: "tool_result",
			toolCallId: "e1",
			toolName: "edit",
			input,
			content: [{ type: "text", text: "ok" }],
			details: undefined,
			isError: false,
		});

		expect(result?.isError).toBe(true);
		expect(JSON.stringify(result?.content)).toContain("type");
		expect(readFileSync(file, "utf8")).toBe(validMemory);
	});

	test("an edit that creates a new invalid file is rolled back by deleting it", async () => {
		const { agent } = setupRoots();
		const file = join(agent, "new-rule.md");
		const { runner } = await createRunner();
		const input = { path: file, edits: [{ oldText: "x", newText: "y" }] };
		await runner.emitToolCall({ type: "tool_call", toolCallId: "e2", toolName: "edit", input });
		writeFileSync(file, validMemory.replace("reply-in-chinese", "new-rule").replace("回复用中文。", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"));

		const result = await runner.emitToolResult({
			type: "tool_result",
			toolCallId: "e2",
			toolName: "edit",
			input,
			content: [{ type: "text", text: "ok" }],
			details: undefined,
			isError: false,
		});

		expect(result?.isError).toBe(true);
		expect(() => readFileSync(file)).toThrow();
	});

	test("memory lives in its own prompt section that stays byte-identical while files are unchanged", async () => {
		const { agent } = setupRoots();
		writeFileSync(join(agent, "reply-in-chinese.md"), validMemory);
		const { runner } = await createRunner();

		const first = await memorySectionFor(runner);
		const second = await memorySectionFor(runner);

		expect(first).toStartWith("## 记忆");
		expect(first).toContain("reply-in-chinese：回复用中文。");
		// 分节不变，Pi 就不发任何系统消息，缓存前缀照常命中。
		expect(second).toBe(first);
	});

	test("memory changed elsewhere reaches the next turn as an updated section", async () => {
		const { agent } = setupRoots();
		writeFileSync(join(agent, "reply-in-chinese.md"), validMemory);
		const { runner } = await createRunner();
		const before = await memorySectionFor(runner);

		// 另一个会话或用户在界面上新增、删除记忆。
		writeFileSync(join(agent, "later.md"), validMemory.replace("reply-in-chinese", "later"));
		const added = await memorySectionFor(runner);
		rmSync(join(agent, "reply-in-chinese.md"));
		const removed = await memorySectionFor(runner);

		expect(added).not.toBe(before);
		expect(added).toContain("- later：回复用中文。");
		expect(removed).not.toContain("reply-in-chinese：");
		expect(removed).toContain("- later：");
	});

	test("plain sessions get no memory section", async () => {
		setupRoots({ consume: false });
		const { runner } = await createRunner();
		expect(await memorySectionFor(runner)).toBeUndefined();
	});

	test("an overflowing memory is announced when the session starts", async () => {
		const { agent } = setupRoots();
		for (let i = 0; i < 70; i++) {
			writeFileSync(
				join(agent, `fact-${i}.md`),
				`---\nname: fact-${i}\ndescription: 事实 ${i}\ntype: reference\n---\nx\n`,
			);
		}
		const { notices } = await createRunner();
		expect(notices.some((notice) => notice.includes("记忆超出注入上限"))).toBe(true);
	});

	test("the first turn after start or compaction carries recalled facts as a hidden message", async () => {
		const { agent } = setupRoots();
		writeFileSync(
			join(agent, "release-flow.md"),
			"---\nname: release-flow\ndescription: 发布走 fx-deploy 流水线\ntype: project\n---\n先发测试环境。\n",
		);
		const { runner } = await createRunner();
		const turn = (prompt: string) => runner.emitBeforeAgentStart(prompt, undefined, { cwd: process.cwd() });

		const first = await turn("发布流程怎么走");
		const second = await turn("发布流程怎么走");
		await runner.emit({ type: "session_compact", compactionEntry: {} as never, fromExtension: false } as never);
		const afterCompaction = await turn("发布流程怎么走");

		expect(first?.messages).toHaveLength(1);
		expect(first?.messages[0]).toMatchObject({ customType: SMELT_MEMORY_RECALL_MESSAGE, display: false });
		expect(String(first?.messages[0]?.content)).toContain("先发测试环境");
		expect(second?.messages ?? []).toHaveLength(0);
		expect(afterCompaction?.messages).toHaveLength(1);
	});

	test("reading a memory file with the built-in read tool counts as a use", async () => {
		const { agent } = setupRoots();
		writeFileSync(join(agent, "reply-in-chinese.md"), validMemory);
		const { runner } = await createRunner();
		const read = (path: string, isError = false) =>
			runner.emitToolResult({
				type: "tool_result",
				toolCallId: "r",
				toolName: "read",
				input: { path },
				content: [{ type: "text", text: "..." }],
				details: undefined,
				isError,
			});

		await read(join(agent, "reply-in-chinese.md"));
		await read(join(agent, "reply-in-chinese.md"), true);
		await read(join(agent, "..", "elsewhere.md"));

		expect(readMemoryUsage(agent).get("reply-in-chinese.md")?.readCount).toBe(1);
	});

	/**
	 * 真跑一个 Pi 会话（faux 模型），看模型实际收到的消息：记忆不变时不发任何系统消息；
	 * 别处改了记忆，只在末尾追加一条只含 smelt_memory 分节的系统消息，开头不动、缓存不失效。
	 */
	test("a mid-session memory change is appended as a one-section system patch", async () => {
		const { base, agent } = setupRoots();
		writeFileSync(join(agent, "reply-in-chinese.md"), validMemory);
		const received: Array<Array<{ role: string; sections?: Record<string, string | null> }>> = [];
		const faux = fauxProvider({ provider: "faux-memory", models: [{ id: "m" }] });
		faux.setResponses(
			Array.from({ length: 3 }, () => (context: { messages: unknown[] }) => {
				received.push(context.messages as (typeof received)[number]);
				return fauxAssistantMessage("ok");
			}),
		);
		const modelRuntime = await ModelRuntime.create({
			authPath: join(base, "auth.json"),
			modelsPath: join(base, "models.json"),
		});
		modelRuntime.registerNativeProvider(faux.provider);
		await modelRuntime.setRuntimeApiKey("faux-memory", "test");
		const settingsManager = SettingsManager.inMemory({ compaction: { enabled: false } });
		const resourceLoader = new DefaultResourceLoader({
			cwd: base,
			agentDir: join(base, "pi"),
			settingsManager,
			extensionFactories: [{ name: "smelt-memory", factory: smeltMemoryExtension }],
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
			noThemes: true,
			noContextFiles: true,
		} as never);
		await resourceLoader.reload();
		const { session } = await createAgentSession({
			cwd: base,
			agentDir: join(base, "pi"),
			model: faux.getModel(),
			modelRuntime,
			resourceLoader,
			settingsManager,
			sessionManager: SessionManager.inMemory(base),
			tools: [],
		});
		try {
			await session.prompt("one");
			await session.prompt("two");
			writeFileSync(join(agent, "weekly.md"), validMemory.replace("reply-in-chinese", "weekly"));
			await session.prompt("three");
		} finally {
			session.dispose();
		}

		const systemMessages = (turn: number) => received[turn]!.filter((message) => message.role === "system");
		expect(systemMessages(0)).toHaveLength(1);
		expect(systemMessages(0)[0]!.sections?.[SMELT_MEMORY_PROMPT_SECTION]).toContain("reply-in-chinese");
		expect(systemMessages(1)).toHaveLength(1);
		expect(systemMessages(2)).toHaveLength(2);
		const patch = systemMessages(2)[1]!;
		expect(Object.keys(patch.sections ?? {})).toEqual([SMELT_MEMORY_PROMPT_SECTION]);
		expect(patch.sections?.[SMELT_MEMORY_PROMPT_SECTION]).toContain("weekly");
		// 开头那条系统消息原样不动：缓存前缀不失效。
		expect(systemMessages(2)[0]).toEqual(systemMessages(0)[0]);
	});
});
