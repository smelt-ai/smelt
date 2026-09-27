import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { keepsForegroundTimeout, watchHostCommand } from "../src/bash-run.ts";
import { callBackgroundHost, detachedShellCommand } from "../src/background-task.ts";

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
			snapshot: async () => ({ ok: true, status: "completed", exitCode: 0, output: "ready" }),
		});
		expect(result).toEqual({ promoted: false, exitCode: 0, output: "ready" });
	});

	test("a command still running at the threshold stays on the host", async () => {
		const result = await watchHostCommand({
			promoteAfterMs: 60,
			start: async () => ({ ok: true, id: "bg-9" }),
			snapshot: async () => ({ ok: true, status: "running", exitCode: null, output: "" }),
		});
		expect(result).toEqual({ promoted: true, id: "bg-9" });
	});

	test("abort during the foreground window returns the host id and leaves the task", async () => {
		const controller = new AbortController();
		const result = await watchHostCommand({
			promoteAfterMs: 5_000,
			signal: controller.signal,
			start: async () => ({ ok: true, id: "bg-3" }),
			snapshot: async () => {
				controller.abort();
				return { ok: true, status: "running", exitCode: null };
			},
		});
		expect(result).toEqual({ promoted: true, id: "bg-3" });
	});

	test("a failing host command stays a foreground error", async () => {
		const result = await watchHostCommand({
			promoteAfterMs: 1_000,
			start: async () => ({ ok: true, id: "bg-2" }),
			snapshot: async () => ({ ok: true, status: "failed", exitCode: 3, output: "nope" }),
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
