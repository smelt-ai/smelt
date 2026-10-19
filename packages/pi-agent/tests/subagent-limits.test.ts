import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "bun:test";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { SMELT_AGENT_MCP_SERVERS_ENV, SMELT_MCP_EXTENSION_NAME, consumeMcpSelection } from "../src/mcp-selection.ts";
import { SMELT_EXTENSION_FACTORIES, withAgentMcp } from "../src/runtime-extensions.ts";
import {
	dispatchStatus,
	runAgent,
	subagentActivity,
	subagentDetails,
	subagentEnvironment,
	subagentSessionArgs,
	subagentSlots,
	subagentTitle,
} from "../src/subagent/index.ts";
import { type AgentConfig, parseMaxTurns } from "../src/subagent/agents.ts";
import {
	MAX_RUNNING_SUBAGENTS,
	ProcessSlots,
	STDERR_TAIL_CHARS,
	SUBAGENT_WRAP_UP_TURNS,
	appendTail,
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
	sessionFile?: string,
) {
	return runAgent({
		cwd: process.cwd(),
		agents: agent ? [agent] : [],
		spec: agent ? { task: "测试任务", agent: agent.name } : { task: "测试任务" },
		signal,
		onUpdate,
		context,
		id: "sa-test",
		sessionFile,
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
	const usage = { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, cost: 0, contextTokens: 2, turns: 1 };
	const result = (overrides: Record<string, unknown>) =>
		({
			id: "sa-1",
			title: "看看",
			agent: "scout",
			agentSource: "inline",
			task: "看看",
			exitCode: 0,
			stderr: "warning: 很多 debug 输出",
			usage,
			toolCalls: 3,
			output: "结果",
			stopReason: "stop",
			...overrides,
		}) as never;

	test("the parent records only each subagent's id, status and result", () => {
		const long = "y".repeat(40_000);
		const details = subagentDetails("parallel", false, [result({ output: long })]);
		const [run] = details.runs;
		expect(details.mode).toBe("parallel");
		expect(details.background).toBe(false);
		expect(Object.keys(run!).sort()).toEqual(["agent", "id", "output", "status", "title", "usage"]);
		expect(run!.id).toBe("sa-1");
		expect(run!.status).toBe("completed");
		expect(run!.output.length).toBeLessThan(long.length);
		expect(run!.output).toContain("[truncated");
	});

	test("a running subagent has no output yet; a stopped one is stopped, not failed", () => {
		const [running] = subagentDetails("single", true, [result({ exitCode: -1, output: "半截" })]).runs;
		expect(running!.status).toBe("running");
		expect(running!.output).toBe("");
		const [stopped] = subagentDetails("single", true, [result({ exitCode: 1, stopReason: "aborted", stopped: true })]).runs;
		expect(stopped!.status).toBe("stopped");
		const [failed] = subagentDetails("single", true, [result({ exitCode: 1, stopReason: "error" })]).runs;
		expect(failed!.status).toBe("failed");
	});

	test("a dispatch is stopped only when every subagent was stopped", () => {
		const done = result({});
		const stopped = result({ exitCode: 1, stopReason: "aborted", stopped: true });
		const failed = result({ exitCode: 1, stopReason: "error" });
		expect(dispatchStatus([done, done])).toBe("completed");
		expect(dispatchStatus([stopped, stopped])).toBe("stopped");
		expect(dispatchStatus([done, stopped])).toBe("completed");
		expect(dispatchStatus([done, failed])).toBe("failed");
	});

	test("a subagent is named by its label, its agent, or the first line of its task", () => {
		expect(subagentTitle({ label: "审计 a", agent: "reviewer", task: "x" })).toBe("审计 a");
		expect(subagentTitle({ agent: "reviewer", task: "x" })).toBe("reviewer");
		expect(subagentTitle({ task: "查登录\n细节" })).toBe("查登录");
	});

	test("what a subagent is doing reads as the tool and its command or path", () => {
		expect(subagentActivity("bash", { command: "rg  -n\nfoo" })).toBe("bash rg -n foo");
		expect(subagentActivity("read", { path: "README.md" })).toBe("read README.md");
		expect(subagentActivity("todo", {})).toBe("todo");
		expect(subagentActivity(undefined, {})).toBeUndefined();
		expect(subagentActivity("bash", { command: "x".repeat(500) })!.length).toBeLessThanOrEqual(121);
	});

	test("a child session is saved where the host says, or not at all outside Smelt", () => {
		expect(subagentSessionArgs("/home/u/.smelt/subagents/sa-1.jsonl")).toEqual([
			"--session",
			"/home/u/.smelt/subagents/sa-1.jsonl",
		]);
		expect(subagentSessionArgs(undefined)).toEqual(["--no-session"]);
	});
});

describe("subagent child process", () => {
	test("the child writes its session to the file the host gave", async () => {
		const dir = temporaryDir();
		const log = join(dir, "log");
		const sessionFile = join(dir, "sa-test.jsonl");
		await runFake({ log, turns: 1, logArgs: true }, undefined, undefined, undefined, sessionFile);
		const argsLine = logLines(log).find((line) => line.startsWith("args "));
		const args = JSON.parse(argsLine!.slice("args ".length)) as string[];
		expect(args).toContain("--session");
		expect(args[args.indexOf("--session") + 1]).toBe(sessionFile);
		expect(args).not.toContain("--no-session");
	});

	test("progress counts turns and tool calls and says what the child is doing", async () => {
		const seen: Array<{ turns: number; toolCalls: number; lastActivity?: string }> = [];
		const result = await runFake({ turns: 3, toolCalls: true }, undefined, (partial) => {
			seen.push({ turns: partial.usage.turns, toolCalls: partial.toolCalls, lastActivity: partial.lastActivity });
		});
		expect(result.id).toBe("sa-test");
		expect(result.usage.turns).toBe(3);
		expect(result.toolCalls).toBe(3);
		expect(result.lastActivity).toBe("bash echo 3");
		expect(seen.some((progress) => progress.toolCalls === 1 && progress.lastActivity === "bash echo 1")).toBe(true);
	});

	test("a child stopped from outside is stopped, not failed", async () => {
		const controller = new AbortController();
		const running = runFake({ slowAfter: 0 }, controller.signal);
		await delay(200);
		controller.abort();
		const result = await running;
		expect(result.stopReason).toBe("aborted");
		expect(result.stopped).toBe(true);
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
