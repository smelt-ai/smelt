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
	SMELT_AGENT_UNATTENDED_ENV,
	consumeAgentMemory,
	resetMemoryRootsForTests,
} from "../src/agent-memory.ts";
import smeltMemoryExtension, {
	SMELT_MEMORY_PROMPT_SECTION,
	SMELT_MEMORY_RECALL_MESSAGE,
} from "../src/memory-extension.ts";
import { SMELT_BACKGROUND_TASK_EXTENSION_PATH } from "../src/background-task.ts";
import { SMELT_ELICITATION_EXTENSION_PATH } from "../src/elicitation.ts";
import { resetEmbeddingCacheForTests } from "../src/memory-embedding.ts";
import { setMemoryHostTransportForTests } from "../src/memory-host.ts";
import { FakeMemoryHost } from "./memory-host-fake.ts";
import { SMELT_SUBAGENT_EXTENSION_PATH } from "../src/subagent/index.ts";
import { SMELT_EXTENSION_FACTORIES } from "../src/runtime-extensions.ts";

const temps: string[] = [];
afterEach(() => {
	for (const dir of temps.splice(0)) rmSync(dir, { recursive: true, force: true });
	resetMemoryRootsForTests();
	setMemoryHostTransportForTests(undefined);
});

function setupRoots(options: { consume?: boolean; unattended?: boolean } = {}) {
	const base = realpathSync(mkdtempSync(join(tmpdir(), "smelt-memory-ext-")));
	temps.push(base);
	const agent = join(base, "agents", "writer", "memory");
	const user = join(base, "memory");
	mkdirSync(agent, { recursive: true });
	mkdirSync(user, { recursive: true });
	if (options.consume !== false) {
		consumeAgentMemory({
			[SMELT_AGENT_MEMORY_DIR_ENV]: agent,
			[SMELT_USER_MEMORY_DIR_ENV]: user,
			[SMELT_AGENT_UNATTENDED_ENV]: options.unattended ? "1" : undefined,
		});
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

async function createRunner(options: { spoofWrite?: boolean; host?: FakeMemoryHost } = {}) {
	const host = options.host ?? new FakeMemoryHost();
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
	const inlineSource = (path: string): SourceInfo => ({ path, source: "inline", scope: "temporary", origin: "top-level" });
	const extraTools: Array<[string, SourceInfo]> = [
		["web_search", { path: "/tmp/web-search.ts", source: "local", scope: "user", origin: "top-level" }],
		["smelt_ask_user", inlineSource(SMELT_ELICITATION_EXTENSION_PATH)],
		["background_task", inlineSource(SMELT_BACKGROUND_TASK_EXTENSION_PATH)],
		["subagent", inlineSource(SMELT_SUBAGENT_EXTENSION_PATH)],
	];
	runtime.getAllTools = () => [
		...["read", "write", "edit", "bash"].map((name): [string, SourceInfo] => [
			name,
			options.spoofWrite && name === "write"
				? { path: "/tmp/user-write.ts", source: "local", scope: "user", origin: "top-level" }
				: builtinSource(name),
		]),
		...extraTools,
	].map(([name, sourceInfo]) => ({
		name,
		description: name,
		parameters: {} as never,
		exposure: "direct" as const,
		sourceInfo,
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
	setMemoryHostTransportForTests(host.transport);
	await runner.emit({ type: "session_start", reason: "startup" });
	return { runner, confirms, notices, host };
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

	test("a second write to the same memory file in one parallel batch is refused", async () => {
		const { agent } = setupRoots();
		const file = join(agent, "reply-in-chinese.md");
		const { runner } = await createRunner();

		// 并行批次里两个 tool_call 都先于执行触发：edit 记下的"原内容"是 write 之前的，
		// 回滚会把 write 刚建的文件删掉。所以第二个直接拒绝。
		const write = await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "p1",
			toolName: "write",
			input: { path: file, content: validMemory },
		});
		const edit = await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "p2",
			toolName: "edit",
			input: { path: file, edits: [{ oldText: "type: feedback", newText: "type: diary" }] },
		});
		expect(write?.block).toBeFalsy();
		expect(edit?.block).toBe(true);
		expect(edit?.reason).toContain("等上一步完成");

		// 前一个写完后就能接着改。
		writeFileSync(file, validMemory);
		await runner.emitToolResult({
			type: "tool_result",
			toolCallId: "p1",
			toolName: "write",
			input: { path: file, content: validMemory },
			content: [{ type: "text", text: "ok" }],
			details: undefined,
			isError: false,
		});
		const next = await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "p3",
			toolName: "edit",
			input: { path: file, edits: [{ oldText: "a", newText: "b" }] },
		});
		expect(next?.block).toBeFalsy();
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
		const { runner, host } = await createRunner();
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

		expect(host.usage.agent.get("reply-in-chinese.md")).toBe(1);
	});

	describe("provenance", () => {
		const turn = (runner: ExtensionRunner, prompt = "hi") =>
			runner.emitBeforeAgentStart(prompt, undefined, { cwd: process.cwd() });
		const toolResult = (runner: ExtensionRunner, toolCallId: string, toolName: string, input: Record<string, unknown> = {}) =>
			runner.emitToolResult({
				type: "tool_result",
				toolCallId,
				toolName,
				input,
				content: [{ type: "text", text: "ok" }],
				details: undefined,
				isError: false,
			});
		async function writeMemory(runner: ExtensionRunner, agent: string, name: string, id: string) {
			const path = join(agent, `${name}.md`);
			const content = validMemory.replace("reply-in-chinese", name);
			const call = await runner.emitToolCall({ type: "tool_call", toolCallId: id, toolName: "write", input: { path, content } });
			expect(call?.block).toBeFalsy();
			writeFileSync(path, content);
			return toolResult(runner, id, "write", { path, content });
		}

		test("a clean turn writes trusted memory and reports it when the run ends", async () => {
			const { agent } = setupRoots();
			const { runner, notices, host } = await createRunner();
			await turn(runner);
			const result = await writeMemory(runner, agent, "clean", "w1");
			await runner.emit({ type: "agent_end", messages: [] } as never);

			expect(result?.content).toBeUndefined();
			expect(host.provenance.agent.get("clean.md")?.trust).toBe("trusted");
			expect(notices.at(-1)).toBe("记忆：记住了 1 条");
		});

		test("after an external tool result in the same turn, writes are pending and not injected", async () => {
			const { agent } = setupRoots();
			const { runner, notices, host } = await createRunner();
			await turn(runner);
			// 选择题、bash、后台任务不算外部内容；网页搜索这类扩展工具、子智能体算。
			await toolResult(runner, "t0", "smelt_ask_user");
			await toolResult(runner, "t1", "bash");
			await toolResult(runner, "t2", "background_task");
			await writeMemory(runner, agent, "before-search", "w0");
			await toolResult(runner, "t3", "web_search");
			const result = await writeMemory(runner, agent, "after-search", "w1");
			await runner.emit({ type: "agent_end", messages: [] } as never);

			expect(host.provenance.agent.get("before-search.md")?.trust).toBe("trusted");
			expect(host.provenance.agent.get("after-search.md")).toMatchObject({
				trust: "pending",
				reason: expect.stringContaining("web_search"),
			});
			expect(JSON.stringify(result?.content)).toContain("待确认");
			expect(notices.at(-1)).toBe("记忆：记住了 1 条；1 条待确认，在智能体的记忆卡片里采纳后生效");

			const section = (await turn(runner))?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION];
			expect(section).toContain("before-search");
			expect(section).not.toContain("after-search");
		});

		test("the next user message clears the mark; subagent results count as external", async () => {
			const { agent } = setupRoots();
			const { runner, host } = await createRunner();
			await turn(runner);
			await toolResult(runner, "s1", "subagent");
			await writeMemory(runner, agent, "tainted", "w1");
			await turn(runner, "记住这个");
			await writeMemory(runner, agent, "clean", "w2");

			expect(host.provenance.agent.get("tainted.md")?.trust).toBe("pending");
			expect(host.provenance.agent.get("clean.md")?.trust).toBe("trusted");
		});

		test("unattended runs only ever write pending memory", async () => {
			const { agent } = setupRoots({ unattended: true });
			const { runner, host } = await createRunner();
			await turn(runner);
			await writeMemory(runner, agent, "from-automation", "w1");
			expect(host.provenance.agent.get("from-automation.md")).toMatchObject({
				trust: "pending",
				reason: expect.stringContaining("自动化"),
			});
		});

		test("editing a pending memory in a clean turn keeps it pending; rewriting it whole does not", async () => {
			const { agent } = setupRoots();
			const path = join(agent, "rule.md");
			const { runner, host } = await createRunner();
			await turn(runner);
			await toolResult(runner, "t", "web_search");
			await writeMemory(runner, agent, "rule", "w1");
			await turn(runner);

			const editInput = { path, edits: [{ oldText: "回复用中文。", newText: "回复一律用中文。" }] };
			await runner.emitToolCall({ type: "tool_call", toolCallId: "e1", toolName: "edit", input: editInput });
			writeFileSync(path, validMemory.replace("reply-in-chinese", "rule").replace("回复用中文。", "回复一律用中文。"));
			await toolResult(runner, "e1", "edit", editInput);
			expect(host.provenance.agent.get("rule.md")?.trust).toBe("pending");

			await writeMemory(runner, agent, "rule", "w2");
			expect(host.provenance.agent.get("rule.md")?.trust).toBe("trusted");
		});

		test("a pending write whose provenance cannot be recorded is undone", async () => {
			const { agent } = setupRoots();
			const { runner, host } = await createRunner();
			host.failWrites = true;
			await turn(runner);
			await toolResult(runner, "t", "web_search");
			const result = await writeMemory(runner, agent, "unrecorded", "w1");

			expect(result?.isError).toBe(true);
			expect(() => readFileSync(join(agent, "unrecorded.md"))).toThrow();
		});

		test("approving a pending memory makes it appear on the next turn", async () => {
			const { agent } = setupRoots();
			const { runner, host } = await createRunner();
			await turn(runner);
			await toolResult(runner, "t", "web_search");
			await writeMemory(runner, agent, "approved-later", "w1");
			const section = async () =>
				(await turn(runner))?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION];

			expect(await section()).not.toContain("approved-later");
			host.approve("agent", "approved-later.md");
			expect(await section()).toContain("approved-later");
		});

		test("without the host nothing is injected or recalled, because pending ones cannot be told apart", async () => {
			const { agent } = setupRoots();
			writeFileSync(join(agent, "reply-in-chinese.md"), validMemory);
			const host = new FakeMemoryHost();
			host.down = true;
			const { runner } = await createRunner({ host });

			const result = await turn(runner, "回复用中文");
			const text = result?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION];
			expect(text).toContain("### 写入");
			expect(text).not.toContain("reply-in-chinese");
			expect(result?.messages ?? []).toHaveLength(0);
		});

		test("turning memory off mid-session removes the section and refuses writes", async () => {
			const { agent } = setupRoots();
			writeFileSync(join(agent, "reply-in-chinese.md"), validMemory);
			const { runner, host } = await createRunner();
			expect((await turn(runner))?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION]).toContain(
				"reply-in-chinese",
			);

			host.enabled = false;
			const off = await turn(runner, "回复用中文");
			expect(off?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION]).toBeUndefined();
			expect(off?.messages ?? []).toHaveLength(0);
			const write = await runner.emitToolCall({
				type: "tool_call",
				toolCallId: "w-off",
				toolName: "write",
				input: { path: join(agent, "new-one.md"), content: validMemory.replace("reply-in-chinese", "new-one") },
			});
			expect(write).toMatchObject({ block: true, reason: expect.stringContaining("已在配置页关闭") });
		});

		test("a session that started with memory off picks it up once memory is turned on", async () => {
			const { agent } = setupRoots();
			writeFileSync(join(agent, "reply-in-chinese.md"), validMemory);
			const host = new FakeMemoryHost();
			host.enabled = false;
			const { runner } = await createRunner({ host });
			expect((await turn(runner))?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION]).toBeUndefined();

			host.enabled = true;
			expect((await turn(runner))?.systemPromptOptions.sections?.[SMELT_MEMORY_PROMPT_SECTION]).toContain(
				"reply-in-chinese",
			);
		});

		test("pending facts are not recalled", async () => {
			const { agent } = setupRoots();
			const fact =
				"---\nname: release-flow\ndescription: 发布走 fx-deploy 流水线\ntype: project\n---\n先发测试环境。\n";
			const { runner } = await createRunner();
			await turn(runner);
			await toolResult(runner, "t", "web_search");
			const path = join(agent, "release-flow.md");
			await runner.emitToolCall({ type: "tool_call", toolCallId: "w", toolName: "write", input: { path, content: fact } });
			writeFileSync(path, fact);
			await toolResult(runner, "w", "write", { path, content: fact });
			await runner.emit({ type: "session_start", reason: "resume" });

			expect((await turn(runner, "发布流程怎么走"))?.messages ?? []).toHaveLength(0);
		});
	});

	test("later turns recall only confident lexical matches, and never the same memory twice", async () => {
		const { agent } = setupRoots();
		writeFileSync(
			join(agent, "meeting-room.md"),
			"---\nname: meeting-room\ndescription: 周会在 3 楼 301 会议室\ntype: reference\n---\n每周二上午十点。\n",
		);
		const { runner } = await createRunner();
		const turn = (prompt: string) => runner.emitBeforeAgentStart(prompt, undefined, { cwd: process.cwd() });
		const custom = (result: Awaited<ReturnType<typeof turn>>) =>
			(result?.messages ?? []).filter((message) => message.customType === SMELT_MEMORY_RECALL_MESSAGE);

		await turn("先聊点别的");
		expect(custom(await turn("今天天气怎么样"))).toHaveLength(0);
		const hit = custom(await turn("周会在哪开"));
		expect(hit).toHaveLength(1);
		expect(String(hit[0]?.content)).toContain("每周二上午十点");
		expect(custom(await turn("周会在哪开"))).toHaveLength(0);
	});

	test("memory_search is registered, needs no approval and returns matching facts", async () => {
		const { agent } = setupRoots();
		writeFileSync(
			join(agent, "meeting-room.md"),
			"---\nname: meeting-room\ndescription: 周会在 3 楼 301 会议室\ntype: reference\n---\n每周二上午十点。\n",
		);
		const { runner } = await createRunner();
		const tool = runner.getToolDefinition("memory_search");
		expect(tool).toBeDefined();
		const result = await tool!.execute("s1", { query: "周会在哪开" }, undefined, undefined, {
			cwd: process.cwd(),
		} as never);
		expect(JSON.stringify(result.content)).toContain("每周二上午十点");
		const missing = await tool!.execute("s2", { query: "快速排序怎么写" }, undefined, undefined, {
			cwd: process.cwd(),
		} as never);
		expect(JSON.stringify(missing.content)).toContain("没有找到");
	});

	describe("vector retrieval", () => {
		const realFetch = globalThis.fetch;
		afterEach(() => {
			globalThis.fetch = realFetch;
			resetEmbeddingCacheForTests();
		});

		/** 假向量服务：文本里含"发布"或"上线"的映射到同一个方向，其余到另一个方向。 */
		function fakeEmbeddings(calls: string[][]) {
			globalThis.fetch = (async (_url: string, init: { body: string }) => {
				const { input } = JSON.parse(init.body) as { input: string[] };
				calls.push(input);
				return new Response(
					JSON.stringify({
						data: input.map((text, index) => ({
							index,
							embedding: /发布|上线/.test(text) ? [1, 0] : [0, 1],
						})),
					}),
				);
			}) as typeof fetch;
		}

		const releaseFlow =
			"---\nname: release-flow\ndescription: 发布走 fx-deploy 流水线\ntype: project\n---\n先发测试环境，再预发，再生产。\n";
		const kanban = "---\nname: kanban\ndescription: 需求看板在飞书\ntype: reference\n---\n飞书项目 Smelt 空间。\n";

		test("a paraphrased question is found through vectors when no rerank model is available", async () => {
			const { agent } = setupRoots();
			writeFileSync(join(agent, "release-flow.md"), releaseFlow);
			writeFileSync(join(agent, "kanban.md"), kanban);
			const calls: string[][] = [];
			fakeEmbeddings(calls);
			const host = new FakeMemoryHost();
			host.embedding = { base_url: "http://embed.test/v1", model: "m" };
			const { runner } = await createRunner({ host });

			// "上线"和记忆在字面上没有一个二元组相同，只有向量能找到。
			const first = await runner.emitBeforeAgentStart("这个版本怎么上线？", undefined, { cwd: process.cwd() });
			expect(String(first?.messages[0]?.content)).toContain("先发测试环境");
			expect(String(first?.messages[0]?.content)).not.toContain("飞书项目");

			// 记忆没变：第二次只给问题算向量，记忆的向量走缓存。
			const tool = runner.getToolDefinition("memory_search")!;
			await tool.execute("s", { query: "上线步骤" }, undefined, undefined, { cwd: process.cwd() } as never);
			expect(calls.at(-1)).toEqual(["上线步骤"]);
		});

		test("memories that cannot be embedded are skipped with a single notice", async () => {
			const { agent } = setupRoots();
			writeFileSync(join(agent, "release-flow.md"), releaseFlow);
			writeFileSync(join(agent, "kanban.md"), kanban);
			// 这个服务拒绝含"看板"的输入（模拟超出模型长度上限的那一条）。
			globalThis.fetch = (async (_url: string, init: { body: string }) => {
				const { input } = JSON.parse(init.body) as { input: string[] };
				if (input.some((text) => text.includes("看板"))) return new Response("too long", { status: 400 });
				return new Response(
					JSON.stringify({ data: input.map((text, index) => ({ index, embedding: /发布|上线/.test(text) ? [1, 0] : [0, 1] })) }),
				);
			}) as typeof fetch;
			const host = new FakeMemoryHost();
			host.embedding = { base_url: "http://embed.test/v1", model: "m" };
			const { runner, notices } = await createRunner({ host });
			const tool = runner.getToolDefinition("memory_search")!;
			const run = (query: string) =>
				tool.execute("s", { query }, undefined, undefined, {
					cwd: process.cwd(),
					ui: { notify: (message: string) => notices.push(message) },
				} as never);

			expect(JSON.stringify((await run("这个版本怎么上线？")).content)).toContain("先发测试环境");
			await run("上线步骤");
			const skipped = notices.filter((notice) => notice.includes("算不出向量"));
			expect(skipped).toHaveLength(1);
			expect(skipped[0]).toContain("kanban");
		});

		test("a failing vector service falls back to keywords and warns once", async () => {
			const { agent } = setupRoots();
			writeFileSync(join(agent, "meeting-room.md"), "---\nname: meeting-room\ndescription: 周会在 3 楼 301 会议室\ntype: reference\n---\n每周二。\n");
			globalThis.fetch = (async () => new Response("down", { status: 503 })) as unknown as typeof fetch;
			const host = new FakeMemoryHost();
			host.embedding = { base_url: "http://embed.test/v1", model: "m" };
			const { runner, notices } = await createRunner({ host });
			const tool = runner.getToolDefinition("memory_search")!;

			const result = await tool.execute("s1", { query: "周会在哪开" }, undefined, undefined, {
				cwd: process.cwd(),
				ui: { notify: (message: string) => notices.push(message) },
			} as never);
			await tool.execute("s2", { query: "周会在哪开" }, undefined, undefined, {
				cwd: process.cwd(),
				ui: { notify: (message: string) => notices.push(message) },
			} as never);

			expect(JSON.stringify(result.content)).toContain("每周二");
			expect(notices.filter((notice) => notice.includes("向量检索不可用"))).toHaveLength(1);
			expect(notices.find((notice) => notice.includes("向量检索不可用"))).toContain("503");
		});
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
		const host = new FakeMemoryHost();
		setMemoryHostTransportForTests(host.transport);
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

	/** 换了说法、关键词一个都对不上的问题，第一轮靠便宜模型重排找回来（这里用 faux 模型代答）。 */
	test("the first turn finds a paraphrased memory through the model rerank", async () => {
		const { base, agent } = setupRoots();
		writeFileSync(
			join(agent, "release-flow.md"),
			"---\nname: release-flow\ndescription: 发布走 fx-deploy 流水线\ntype: project\n---\n先发测试环境，再预发，再生产。\n",
		);
		const received: Array<Array<{ role: string; content?: unknown }>> = [];
		const faux = fauxProvider({ provider: "faux-rerank", models: [{ id: "m" }] });
		faux.setResponses([
			// 第一次调用是重排：只有一个候选，选它。
			() => fauxAssistantMessage("[1]"),
			(context: { messages: unknown[] }) => {
				received.push(context.messages as (typeof received)[number]);
				return fauxAssistantMessage("ok");
			},
		]);
		const modelRuntime = await ModelRuntime.create({
			authPath: join(base, "auth.json"),
			modelsPath: join(base, "models.json"),
		});
		modelRuntime.registerNativeProvider(faux.provider);
		await modelRuntime.setRuntimeApiKey("faux-rerank", "test");
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
		setMemoryHostTransportForTests(new FakeMemoryHost().transport);
		// 真实 RPC 模式下宿主会绑定扩展、触发 session_start，第一轮才走完整召回。
		await session.bindExtensions({ mode: "rpc" });
		try {
			await session.prompt("这个版本怎么上线？");
		} finally {
			session.dispose();
		}

		const recalled = JSON.stringify(received[0]);
		expect(recalled).toContain("先发测试环境，再预发，再生产");
	});

	/** 用 faux 提供方真跑一轮：记录每次模型调用用的是哪个模型，以及状态栏提示。 */
	async function runRecallTurn(rerankModel: { provider: string; id: string } | undefined, prompt: string) {
		const { base, agent } = setupRoots();
		writeFileSync(
			join(agent, "meeting-room.md"),
			"---\nname: meeting-room\ndescription: 周会在 3 楼 301 会议室\ntype: reference\n---\n每周二上午十点。\n",
		);
		const asked: Array<{ model: string; reasoning: unknown }> = [];
		const received: unknown[][] = [];
		const faux = fauxProvider({ provider: "faux-pick", models: [{ id: "main" }, { id: "picked" }] });
		faux.setResponses(
			Array.from(
				{ length: 4 },
				() =>
					(context: { messages: unknown[] }, options: { reasoning?: unknown } | undefined, _state: unknown, model: { id: string }) => {
						asked.push({ model: model.id, reasoning: options?.reasoning });
						if (model.id === "picked") return fauxAssistantMessage("[1]");
						received.push(context.messages);
						return fauxAssistantMessage("ok");
					},
			) as never,
		);
		const modelRuntime = await ModelRuntime.create({
			authPath: join(base, "auth.json"),
			modelsPath: join(base, "models.json"),
		});
		modelRuntime.registerNativeProvider(faux.provider);
		await modelRuntime.setRuntimeApiKey("faux-pick", "test");
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
			model: faux.getModel("main"),
			modelRuntime,
			resourceLoader,
			settingsManager,
			sessionManager: SessionManager.inMemory(base),
			tools: [],
		});
		const host = new FakeMemoryHost();
		host.rerankModel = rerankModel;
		setMemoryHostTransportForTests(host.transport);
		const notices: string[] = [];
		const ui = new Proxy(
			{ notify: (message: string) => notices.push(message) },
			{ get: (target, key) => (key in target ? target[key as "notify"] : () => undefined) },
		);
		await session.bindExtensions({ uiContext: ui as never, mode: "rpc" });
		try {
			await session.prompt(prompt);
		} finally {
			session.dispose();
		}
		return { asked, notices, received: JSON.stringify(received) };
	}

	test("a configured rerank model is used for the rerank, with reasoning left off", async () => {
		const { asked, received } = await runRecallTurn({ provider: "faux-pick", id: "picked" }, "开会的地方在哪");
		expect(asked[0]).toEqual({ model: "picked", reasoning: undefined });
		expect(received).toContain("每周二上午十点");
	});

	test("a configured rerank model that is not available falls back to retrieval with a notice", async () => {
		const { asked, notices, received } = await runRecallTurn({ provider: "faux-pick", id: "gone" }, "周会在哪开");
		expect(asked.map((call) => call.model)).toEqual(["main"]);
		expect(notices.some((notice) => notice.includes("faux-pick/gone") && notice.includes("不可用"))).toBe(true);
		// 检索层的关键词结果照样附上。
		expect(received).toContain("每周二上午十点");
	});
});
