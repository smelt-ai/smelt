import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { keepsForegroundTimeout, watchHostCommand } from "../src/bash-run.ts";
import {
	attachBackgroundTask,
	callBackgroundHost,
	completionSendOptions,
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

describe("attached background task", () => {
	/** 收下挂接请求、回一个 id，之后把每一行记下来；`stop()` 往连接里写停止请求。 */
	async function fakeAttachHost() {
		const path = join(tmpdir(), `smelt-bg-attach-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
		const lines: Record<string, unknown>[] = [];
		let peer: import("node:net").Socket | undefined;
		let closed!: () => void;
		const connectionClosed = new Promise<void>((resolve) => {
			closed = resolve;
		});
		const server = createServer((socket) => {
			peer = socket;
			let buffer = "";
			socket.on("data", (chunk) => {
				buffer += chunk.toString();
				let newline = buffer.indexOf("\n");
				while (newline >= 0) {
					const line = JSON.parse(buffer.slice(0, newline)) as Record<string, unknown>;
					buffer = buffer.slice(newline + 1);
					newline = buffer.indexOf("\n");
					lines.push(line);
					if (line.op === "attach") socket.write(`${JSON.stringify({ ok: true, id: "bg-sub" })}\n`);
				}
			});
			socket.on("close", () => closed());
		});
		await new Promise<void>((resolve) => server.listen(path, resolve));
		return {
			path,
			lines,
			connectionClosed,
			stop: () => peer?.write(`${JSON.stringify({ op: "stop" })}\n`),
			close: () =>
				new Promise<void>((resolve, reject) => {
					server.close((error) => (error ? reject(error) : resolve()));
				}),
		};
	}

	test("registers with the tool call, reports progress, relays stop, and finishes once", async () => {
		const host = await fakeAttachHost();
		try {
			let stops = 0;
			const task = await attachBackgroundTask(
				{ title: "8 个并行任务", command: "子智能体 sub-3", toolCallId: "call-1" },
				() => {
					stops += 1;
				},
				host.path,
			);
			expect(task?.id).toBe("bg-sub");
			expect(host.lines[0]).toMatchObject({ op: "attach", title: "8 个并行任务", toolCallId: "call-1" });

			task!.progress("2/8 done");
			host.stop();
			await Bun.sleep(50);
			expect(stops).toBe(1);

			task!.finish("stopped");
			task!.finish("failed");
			await host.connectionClosed;
			expect(host.lines.slice(1)).toEqual([
				{ op: "progress", text: "2/8 done" },
				{ op: "finish", status: "stopped" },
			]);
		} finally {
			await host.close();
		}
	});

	test("runs untracked when the host is unreachable", async () => {
		const missing = join(tmpdir(), `smelt-bg-missing-${process.pid}.sock`);
		expect(await attachBackgroundTask({ title: "t", command: "c" }, () => {}, missing)).toBeUndefined();
	});
});
