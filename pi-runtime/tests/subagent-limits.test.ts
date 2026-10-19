import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "bun:test";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { SMELT_AGENT_MCP_SERVERS_ENV, SMELT_MCP_EXTENSION_NAME, consumeMcpSelection } from "../src/mcp-selection.ts";
import { SMELT_EXTENSION_FACTORIES, withAgentMcp } from "../src/runtime-extensions.ts";
import {
	finalSubagentDetails,
	runAgent,
	subagentEnvironment,
	subagentSlots,
} from "../src/subagent/index.ts";
import { type AgentConfig, parseMaxTurns } from "../src/subagent/agents.ts";
import {
	MAX_RUNNING_SUBAGENTS,
	PROGRESS_FIELD_CHARS,
	PROGRESS_MESSAGES,
	ProcessSlots,
	STDERR_TAIL_CHARS,
	SUBAGENT_WRAP_UP_TURNS,
	appendTail,
	progressMessage,
	progressWindow,
	throttleLatest,
	turnLimitAction,
	turnLimitMessage,
} from "../src/subagent/limits.ts";

const FAKE_PI = join(import.meta.dir, "fixtures", "fake-pi-rpc.ts");
const context = { isProjectTrusted: () => true } as unknown as ExtensionContext;

const cleanups: Array<() => void> = [];
afterEach(() => {
	for (const cleanup of cleanups.splice(0)) cleanup();
	consumeMcpSelection({});
});

function temporaryDir(): string {
	const dir = mkdtempSync(join(tmpdir(), "smelt-subagent-"));
	cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
	return dir;
}

function logLines(path: string): string[] {
	return existsSync(path) ? readFileSync(path, "utf8").split("\n").filter(Boolean) : [];
}

/** 轮数上限测试用的智能体定义。 */
const MAX_TURNS = 20;
const cappedAgent: AgentConfig = {
	name: "capped",
	description: "有轮数上限的测试智能体",
	maxTurns: MAX_TURNS,
	systemPrompt: "干活",
	source: "user",
	filePath: "/dev/null",
};

/** 用假的 RPC 子进程跑一个子智能体。`agent` 给了就按那个定义跑。 */
function runFake(
	config: Record<string, unknown>,
	signal?: AbortSignal,
	onUpdate?: Parameters<typeof runAgent>[0]["onUpdate"],
	agent?: AgentConfig,
) {
	return runAgent({
		cwd: process.cwd(),
		agents: agent ? [agent] : [],
		spec: agent ? { task: "测试任务", agent: agent.name } : { task: "测试任务" },
		signal,
		onUpdate,
		context,
		piInvocation: (args) => ({ command: process.execPath, args: [FAKE_PI, JSON.stringify(config), ...args] }),
	});
}

const delay = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

describe("subagent MCP selection", () => {
	test("a subagent child gets the MCP servers its parent was started with", () => {
		consumeMcpSelection({ [SMELT_AGENT_MCP_SERVERS_ENV]: JSON.stringify(["rea", "figma"]) });
		const childEnvironment: Record<string, string | undefined> = subagentEnvironment({ PATH: "/bin" }, "owner/sub-1");

		expect(JSON.parse(childEnvironment[SMELT_AGENT_MCP_SERVERS_ENV]!)).toEqual(["rea", "figma"]);
		// 子进程照父进程的路子消费它，连上同一批 server。
		const childSelection = consumeMcpSelection(childEnvironment);
		const names = withAgentMcp(SMELT_EXTENSION_FACTORIES, childSelection).map((input) =>
			typeof input === "function" ? "" : input.name,
		);
		expect(names).toContain(SMELT_MCP_EXTENSION_NAME);
	});

	test("a parent without a selection does not invent one for its children", () => {
		consumeMcpSelection({});
		expect(subagentEnvironment({ PATH: "/bin" }, "owner/sub-1")[SMELT_AGENT_MCP_SERVERS_ENV]).toBeUndefined();
	});
});

describe("subagent limits", () => {
	test("process slots queue in order and free on release", async () => {
		const slots = new ProcessSlots(2);
		const first = await slots.acquire();
		const second = await slots.acquire();
		const order: string[] = [];
		const third = slots.acquire().then((release) => {
			order.push("third");
			return release;
		});
		const fourth = slots.acquire().then((release) => {
			order.push("fourth");
			return release;
		});
		await delay(10);
		expect(order).toEqual([]);
		expect(slots.active).toBe(2);
		expect(slots.queued).toBe(2);

		first!();
		first!(); // 交回只算一次
		await delay(0);
		expect(order).toEqual(["third"]);
		second!();
		await delay(0);
		expect(order).toEqual(["third", "fourth"]);
		(await third)!();
		(await fourth)!();
		expect(slots.active).toBe(0);
	});

	test("a queued acquire gives up its place when aborted", async () => {
		const slots = new ProcessSlots(1);
		const held = await slots.acquire();
		const controller = new AbortController();
		const waiting = slots.acquire(controller.signal);
		controller.abort();
		expect(await waiting).toBeUndefined();
		expect(slots.queued).toBe(0);
		held!();
		expect(slots.active).toBe(0);
		expect(await slots.acquire(controller.signal)).toBeUndefined();
	});

	test("turn limit asks the child to wrap up once, then aborts", () => {
		expect(turnLimitAction(MAX_TURNS - 1, true, MAX_TURNS)).toBe("continue");
		expect(turnLimitAction(MAX_TURNS, true, MAX_TURNS)).toBe("wrap_up");
		expect(turnLimitAction(MAX_TURNS + 1, true, MAX_TURNS)).toBe("continue");
		expect(turnLimitAction(MAX_TURNS + SUBAGENT_WRAP_UP_TURNS, true, MAX_TURNS)).toBe("abort");
		// 这轮本来就收尾了：不插话、不中止。
		expect(turnLimitAction(MAX_TURNS, false, MAX_TURNS)).toBe("continue");
		expect(turnLimitAction(MAX_TURNS + SUBAGENT_WRAP_UP_TURNS, false, MAX_TURNS)).toBe("continue");
	});

	test("without max_turns in the definition there is no turn limit", () => {
		// 写代码的子智能体跑上百次模型调用很正常，默认不能一刀切。
		expect(turnLimitAction(1_000, true, undefined)).toBe("continue");
	});

	test("max_turns accepts only positive integers", () => {
		expect(parseMaxTurns(12)).toBe(12);
		expect(parseMaxTurns(" 30 ")).toBe(30);
		expect(parseMaxTurns(0)).toBeUndefined();
		expect(parseMaxTurns(-3)).toBeUndefined();
		expect(parseMaxTurns(2.5)).toBeUndefined();
		expect(parseMaxTurns("many")).toBeUndefined();
		expect(parseMaxTurns(undefined)).toBeUndefined();
	});

	test("progress drops thinking and clips long text and tool arguments", () => {
		const big = "z".repeat(PROGRESS_FIELD_CHARS * 5);
		const message = progressMessage({
			role: "assistant",
			content: [
				{ type: "thinking", thinking: big },
				{ type: "text", text: big },
				{ type: "toolCall", id: "t1", name: "write", arguments: { path: "a.ts", content: big } },
			],
		});
		expect(message.content.map((part: { type: string }) => part.type)).toEqual(["text", "toolCall"]);
		const text = message.content[0] as { text: string };
		const call = message.content[1] as { arguments: { path: string; content: string } };
		expect(text.text.length).toBeLessThan(PROGRESS_FIELD_CHARS + 50);
		expect(call.arguments.path).toBe("a.ts");
		expect(call.arguments.content.length).toBeLessThan(PROGRESS_FIELD_CHARS + 50);
	});

	test("progress window is bounded by size, keeping at least the newest message", () => {
		const message = (n: number) => ({ n, text: "w".repeat(1_000) });
		const window = progressWindow(Array.from({ length: 10 }, (_, n) => message(n)), 20, 3_500);
		expect(window.map((item) => item.n)).toEqual([7, 8, 9]);
		expect(progressWindow([message(1)], 20, 10).map((item) => item.n)).toEqual([1]);
	});

	test("stderr keeps only its tail", () => {
		expect(appendTail("abc", "def", 4)).toBe("cdef");
		expect(appendTail("", "ab", 4)).toBe("ab");
	});

	test("progress sends the first update at once and then only the latest per interval", async () => {
		const delivered: number[] = [];
		const progress = throttleLatest((value: number) => delivered.push(value), 40);
		progress.push(1);
		progress.push(2);
		progress.push(3);
		expect(delivered).toEqual([1]);
		await delay(70);
		expect(delivered).toEqual([1, 3]);
		progress.push(4);
		progress.cancel();
		await delay(70);
		expect(delivered).toEqual([1, 3]);
	});
});

describe("subagent details", () => {
	test("the settled details keep only the result, not the child's messages", () => {
		const long = "y".repeat(40_000);
		const details = finalSubagentDetails("parallel", [
			{
				agent: "scout",
				agentSource: "inline",
				task: "看看",
				exitCode: 0,
				stderr: "warning: 很多 debug 输出",
				usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, cost: 0, contextTokens: 2, turns: 1 },
				messages: [{ role: "assistant", content: [{ type: "thinking", thinking: "很长的思考" }] } as never],
				output: long,
				stopReason: "stop",
			},
		]);
		const [result] = details.results;
		expect(details.mode).toBe("parallel");
		expect("messages" in result!).toBe(false);
		expect("stderr" in result!).toBe(false);
		expect(result!.agent).toBe("scout");
		expect(result!.stopReason).toBe("stop");
		expect(result!.output.length).toBeLessThan(long.length);
		expect(result!.output).toContain("[truncated");
	});
});

describe("subagent child process", () => {
	test("progress carries only the most recent messages", async () => {
		const seen: number[] = [];
		const result = await runFake({ turns: PROGRESS_MESSAGES + 10 }, undefined, (partial) => {
			seen.push(partial.messages?.length ?? 0);
		});
		expect(result.usage.turns).toBe(PROGRESS_MESSAGES + 10);
		expect(Math.max(...seen)).toBe(PROGRESS_MESSAGES);
	});

	test("waits for a free process slot shared by every call", async () => {
		const dir = temporaryDir();
		const log = join(dir, "log");
		const held = await Promise.all(Array.from({ length: MAX_RUNNING_SUBAGENTS }, () => subagentSlots.acquire()));
		cleanups.push(() => {
			for (const release of held) release?.();
		});
		const running = runFake({ log, turns: 1 });
		await delay(300);
		expect(logLines(log)).toEqual([]);

		held[0]!();
		const result = await running;
		expect(logLines(log)[0]).toBe("start");
		expect(result.exitCode).toBe(0);
		expect(result.output).toBe("turn 1");
		// 子进程结束就交回名额。
		expect(subagentSlots.active).toBe(MAX_RUNNING_SUBAGENTS - 1);
	});

	test("a subagent stopped while queued never starts", async () => {
		const dir = temporaryDir();
		const log = join(dir, "log");
		const held = await Promise.all(Array.from({ length: MAX_RUNNING_SUBAGENTS }, () => subagentSlots.acquire()));
		cleanups.push(() => {
			for (const release of held) release?.();
		});
		const controller = new AbortController();
		const running = runFake({ log, turns: 1 }, controller.signal);
		controller.abort();
		const result = await running;
		expect(result.stopReason).toBe("aborted");
		expect(logLines(log)).toEqual([]);
	});

	test("keeps only the tail of the child's stderr", async () => {
		const result = await runFake({ turns: 1, stderrChars: STDERR_TAIL_CHARS * 4 });
		expect(result.stderr.length).toBe(STDERR_TAIL_CHARS);
		expect(result.stderr.endsWith("END")).toBe(true);
	});

	test("a child that reaches the turn limit is asked to wrap up", async () => {
		const dir = temporaryDir();
		const log = join(dir, "log");
		const result = await runFake({ log, slowAfter: MAX_TURNS - 5 }, undefined, undefined, cappedAgent);
		expect(logLines(log)).toContain("steer");
		expect(logLines(log)).not.toContain("abort");
		expect(result.stopReason).toBe("stop");
		expect(result.usage.turns).toBeGreaterThan(MAX_TURNS);
		expect(result.usage.turns).toBeLessThan(MAX_TURNS + SUBAGENT_WRAP_UP_TURNS);
	});

	test("a child that ignores the wrap-up request is aborted", async () => {
		const dir = temporaryDir();
		const log = join(dir, "log");
		const result = await runFake(
			{ log, ignoreSteer: true, slowAfter: MAX_TURNS - 5 },
			undefined,
			undefined,
			cappedAgent,
		);
		const lines = logLines(log);
		expect(lines).toContain("steer");
		expect(lines).toContain("abort");
		expect(result.stopReason).toBe("aborted");
		expect(result.errorMessage).toBe(turnLimitMessage(result.usage.turns, MAX_TURNS));
		expect(result.output.startsWith(result.errorMessage!)).toBe(true);
	}, 15_000);
});
