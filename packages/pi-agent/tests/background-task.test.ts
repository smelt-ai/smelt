import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { keepsForegroundTimeout, watchHostCommand } from "../src/bash-run.ts";
import {
	callBackgroundHost,
	completionMessage,
	completionSendOptions,
	deliveredResultId,
	detachedShellCommand,
	reconnectAfterClose,
	taskNotificationText,
} from "../src/background-task.ts";

async function fakeHost(reply: (request: Record<string, unknown>) => Record<string, unknown>) {
	const path = join(tmpdir(), `smelt-bg-test-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
	const server = createServer((socket) => {
		let buffer = "";
		socket.on("data", (chunk) => {
			buffer += chunk.toString();
			const newline = buffer.indexOf("\n");
			if (newline < 0) return;
			const request = JSON.parse(buffer.slice(0, newline)) as Record<string, unknown>;
			socket.end(`${JSON.stringify(reply(request))}\n`);
		});
	});
	await new Promise<void>((resolve) => server.listen(path, resolve));
	return {
		path,
		close: () =>
			new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			}),
	};
}

describe("background task host client", () => {
	test("returns the host text for a start request", async () => {
		const host = await fakeHost((request) => {
			expect(request.op).toBe("start");
			expect(request.command).toBe("npm test");
			return { ok: true, text: "已在后台启动 bg-1：测试" };
		});
		try {
			const response = await callBackgroundHost(host.path, { op: "start", command: "npm test", title: "测试" });
			expect(response).toEqual({ ok: true, text: "已在后台启动 bg-1：测试" });
		} finally {
			await host.close();
		}
	});

	test("treats nohup and a trailing ampersand as managed background commands", () => {
		expect(detachedShellCommand("npm test")).toBeUndefined();
		expect(detachedShellCommand("npm test && npm run lint")).toBeUndefined();
		expect(detachedShellCommand("sleep 30 &")).toBe("sleep 30");
		expect(detachedShellCommand("nohup npm run dev &")).toBe("npm run dev");
	});

	test("keeps a short explicit timeout in the foreground", () => {
		expect(keepsForegroundTimeout(undefined)).toBe(false);
		expect(keepsForegroundTimeout(0)).toBe(true);
		expect(keepsForegroundTimeout(60)).toBe(true);
		expect(keepsForegroundTimeout(61)).toBe(false);
	});

	test("a fast host command returns its exit code instead of staying background", async () => {
		const result = await watchHostCommand({
			promoteAfterMs: 1_000,
			start: async () => ({ ok: true, id: "bg-1" }),
			until: async () => ({ ok: true, status: "completed", exitCode: 0, output: "ready" }),
		});
		expect(result).toEqual({ promoted: false, exitCode: 0, output: "ready" });
	});

	test("a command still running at the threshold stays on the host", async () => {
		const result = await watchHostCommand({
			promoteAfterMs: 60,
			start: async () => ({ ok: true, id: "bg-9" }),
			until: async () => ({ ok: true, status: "running", exitCode: null, output: "" }),
		});
		expect(result).toEqual({ promoted: true, id: "bg-9" });
	});

	test("abort during a snapshot promotes the command instead of a foreground error", async () => {
		const released: Array<{ id: string; consumed: boolean }> = [];
		const controller = new AbortController();
		const result = await watchHostCommand({
			promoteAfterMs: 5_000,
			signal: controller.signal,
			start: async () => ({ ok: true, id: "bg-7" }),
			until: async () => {
				controller.abort();
				return { ok: false, error: "操作已取消" };
			},
			release: async (id, consumed) => {
				released.push({ id, consumed });
			},
		});
		expect(result).toEqual({ promoted: true, id: "bg-7" });
		expect(released).toEqual([{ id: "bg-7", consumed: false }]);
	});

	test("a snapshot the tool could not read promotes the command", async () => {
		const released: Array<{ id: string; consumed: boolean }> = [];
		const result = await watchHostCommand({
			promoteAfterMs: 1_000,
			start: async () => ({ ok: true, id: "bg-8" }),
			until: async () => ({ ok: false, error: "无法读取任务状态" }),
			release: async (id, consumed) => {
				released.push({ id, consumed });
			},
		});
		expect(result).toEqual({ promoted: true, id: "bg-8" });
		expect(released).toEqual([{ id: "bg-8", consumed: false }]);
	});

	test("abort during the foreground window returns the host id and leaves the task", async () => {
		const controller = new AbortController();
		const result = await watchHostCommand({
			promoteAfterMs: 5_000,
			signal: controller.signal,
			start: async () => ({ ok: true, id: "bg-3" }),
			until: async () => {
				controller.abort();
				return { ok: true, status: "running", exitCode: null };
			},
		});
		expect(result).toEqual({ promoted: true, id: "bg-3" });
	});

	test("a command that finishes inside the window tells the host the result was taken", async () => {
		const released: Array<{ id: string; consumed: boolean }> = [];
		const result = await watchHostCommand({
			promoteAfterMs: 1_000,
			start: async () => ({ ok: true, id: "bg-1" }),
			until: async () => ({ ok: true, status: "completed", exitCode: 0, output: "ready" }),
			release: async (id, consumed) => {
				released.push({ id, consumed });
			},
		});
		expect(result).toEqual({ promoted: false, exitCode: 0, output: "ready" });
		expect(released).toEqual([{ id: "bg-1", consumed: true }]);
	});

	test("a command still running after the window tells the host the result was not taken", async () => {
		const released: Array<{ id: string; consumed: boolean }> = [];
		const result = await watchHostCommand({
			promoteAfterMs: 0,
			start: async () => ({ ok: true, id: "bg-9" }),
			until: async () => ({ ok: true, status: "running", exitCode: null, output: "" }),
			release: async (id, consumed) => {
				released.push({ id, consumed });
			},
		});
		expect(result).toEqual({ promoted: true, id: "bg-9" });
		expect(released).toEqual([{ id: "bg-9", consumed: false }]);
	});

	test("one wait returns a finish that is already done", async () => {
		let calls = 0;
		const result = await watchHostCommand({
			promoteAfterMs: 0,
			start: async () => ({ ok: true, id: "bg-4" }),
			until: async () => {
				calls += 1;
				return { ok: true, status: "completed", exitCode: 0, output: "late" };
			},
			release: async () => {},
		});
		expect(calls).toBe(1);
		expect(result).toEqual({ promoted: false, exitCode: 0, output: "late" });
	});

	test("a failing host command stays a foreground error", async () => {
		const result = await watchHostCommand({
			promoteAfterMs: 1_000,
			start: async () => ({ ok: true, id: "bg-2" }),
			until: async () => ({ ok: true, status: "failed", exitCode: 3, output: "nope" }),
		});
		expect(result).toEqual({ promoted: false, exitCode: 3, output: "nope" });
	});

	test("abort closes the host request without waiting for a reply", async () => {
		const path = join(tmpdir(), `smelt-bg-abort-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
		const server = createServer((socket) => {
			socket.on("data", () => {});
		});
		await new Promise<void>((resolve) => server.listen(path, resolve));
		const controller = new AbortController();
		const pending = callBackgroundHost(path, { op: "wait", id: "bg-1" }, controller.signal);
		controller.abort();
		try {
			const response = await pending;
			expect(response).toEqual({ ok: false, error: "操作已取消" });
		} finally {
			await new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			});
		}
	});

	test("an acknowledged listener reconnects while the extension is alive", () => {
		expect(reconnectAfterClose(false, true)).toBe(true);
		expect(reconnectAfterClose(true, true)).toBe(false);
		expect(reconnectAfterClose(false, false)).toBe(false);
	});

	test("completion text is a command result with the exit code and output", () => {
		const text = taskNotificationText({
			id: "bg-13",
			title: "编译",
			status: "failed",
			exitCode: 1,
			output: "error: boom",
		});
		expect(text).toContain("不是用户的新请求");
		expect(text).toContain("bg-13");
		expect(text).toContain("退出码 1");
		expect(text).toContain("error: boom");
		expect(JSON.stringify(completionSendOptions(true))).toBe('{"triggerTurn":true}');
		expect(JSON.stringify(completionSendOptions(false))).toBe('{"triggerTurn":false}');
		expect(JSON.stringify(completionSendOptions(true))).not.toContain("followUp");
	});

	test("a subagent result comes back as its own message, queued after the current tool calls", () => {
		const notice = {
			id: "call-7",
			kind: "subagent" as const,
			title: "2 个并行任务",
			status: "stopped",
			exitCode: null,
			output: "查完了一半",
			outputTail: "查完了一半",
			runs: [
				{ id: "sa-a", title: "审计 a", status: "stopped" as const },
				{ id: "sa-b", title: "审计 b", status: "stopped" as const },
			],
			wake: false,
		};
		const { message, options } = completionMessage(notice);
		expect(message.customType).toBe("smelt.subagent_result");
		expect(JSON.stringify(message.content)).toContain("查完了一半");
		// 宿主等的 ack、界面找派发卡片都认工具调用 id；每个子智能体的状态一起带上。
		expect((message as { details: { id: string; runs: unknown[] } }).details.id).toBe("call-7");
		expect((message as { details: { id: string; runs: unknown[] } }).details.runs).toHaveLength(2);
		expect(options).toEqual({ triggerTurn: false, deliverAs: "followUp" });
		expect(completionMessage({ ...notice, kind: "command", wake: true }).options).toEqual({ triggerTurn: true });
	});

	test("a message from another conversation comes in as a peer message, never as user input", () => {
		const notice = {
			id: "pm-1a2b3c4d5e6f",
			kind: "peer_message" as const,
			title: "修登录",
			status: "task",
			exitCode: null,
			output: "重跑 flaky e2e，跑完把失败用例发给我",
			outputTail: "",
			runs: [],
			peer: {
				sourceSessionId: "acp-a",
				sourceTitle: "修登录",
				kind: "task" as const,
				hops: 7,
				replyTo: null,
			},
			wake: true,
		};
		const { message, options } = completionMessage(notice);
		expect(message.customType).toBe("smelt.peer_message");
		const text = JSON.stringify(message.content);
		expect(text).toContain("不是用户的请求");
		expect(text).toContain("session_send 发给 acp-a");
		const details = (message as { details: Record<string, unknown> }).details;
		expect(details).toMatchObject({
			id: "pm-1a2b3c4d5e6f",
			sourceSessionId: "acp-a",
			kind: "task",
			hops: 7,
			text: "重跑 flaky e2e，跑完把失败用例发给我",
		});
		// 空闲时开一轮，忙时排到这一轮之后，不插进手上的工具调用。
		expect(options).toEqual({ triggerTurn: true, deliverAs: "followUp" });
		expect(completionMessage({ ...notice, wake: false }).options).toEqual({ triggerTurn: false, deliverAs: "followUp" });
	});

	test("the host results and peer messages are the ones acknowledged", () => {
		expect(deliveredResultId({ role: "custom", customType: "smelt.background_task", details: { id: "bg-1" } })).toBe("bg-1");
		expect(deliveredResultId({ role: "custom", customType: "smelt.subagent_result", details: { id: "bg-2" } })).toBe("bg-2");
		expect(deliveredResultId({ role: "custom", customType: "smelt.peer_message", details: { id: "pm-1" } })).toBe("pm-1");
		expect(deliveredResultId({ role: "custom", customType: "other", details: { id: "bg-3" } })).toBeUndefined();
		expect(deliveredResultId({ role: "user", content: "bg-4" })).toBeUndefined();
	});

	test("reports a missing socket instead of spawning locally", async () => {
		const response = await callBackgroundHost(undefined, { op: "start", command: "npm test" });
		expect(response.ok).toBe(false);
		expect(response.error).toContain("还没就绪");
	});

	test("immediately returns cancelled when signal is already aborted", async () => {
		const controller = new AbortController();
		controller.abort();
		const response = await callBackgroundHost("/tmp/fake.sock", { op: "start" }, controller.signal);
		expect(response.ok).toBe(false);
		expect(response.error).toContain("操作已取消");
	});

	test("aborts pending background host call on signal abort", async () => {
		// 模拟一个从不响应的 host
		const path = join(tmpdir(), `smelt-bg-hang-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
		const server = createServer((_socket) => {
			// 刻意不回消息，模拟长时间卡住
		});
		await new Promise<void>((resolve) => server.listen(path, resolve));
		const controller = new AbortController();
		try {
			const promise = callBackgroundHost(path, { op: "wait", id: "bg-1" }, controller.signal);
			// 延迟 50ms 后触发 abort
			setTimeout(() => controller.abort(), 50);
			const response = await promise;
			expect(response.ok).toBe(false);
			expect(response.error).toContain("操作已取消");
		} finally {
			await new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			});
		}
	});
});
