import { createServer, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import { openSubagentDispatch } from "../src/subagent/host.ts";

/**
 * 假的会话宿主：收下派发请求回 ok，登记一个回一个 id 和子会话文件，其余每行记下来；
 * `stop(id)` 往连接里写停止请求。
 */
async function fakeHost() {
	const path = join(tmpdir(), `smelt-subagent-host-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
	const lines: Record<string, unknown>[] = [];
	let peer: Socket | undefined;
	let next = 0;
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
				if (line.op === "subagent_dispatch") socket.write(`${JSON.stringify({ ok: true })}\n`);
				if (line.op === "start") {
					next += 1;
					socket.write(
						`${JSON.stringify({ op: "started", ref: line.ref, id: `sa-${next}`, sessionFile: `/s/sa-${next}.jsonl` })}\n`,
					);
				}
			}
		});
		socket.on("close", () => closed());
	});
	await new Promise<void>((resolve) => server.listen(path, resolve));
	return {
		path,
		lines,
		connectionClosed,
		stop: (id: string) => peer?.write(`${JSON.stringify({ op: "stop", id })}\n`),
		close: () =>
			new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			}),
	};
}

describe("subagent dispatch connection", () => {
	test("registers each subagent, reports progress and ends, relays stop, and finishes once", async () => {
		const host = await fakeHost();
		try {
			const stops: string[] = [];
			const dispatch = await openSubagentDispatch(
				{ toolCallId: "call-1", background: true, title: "2 个并行任务" },
				(id) => stops.push(id),
				host.path,
			);
			expect(dispatch).toBeDefined();
			expect(host.lines[0]).toMatchObject({ op: "subagent_dispatch", toolCallId: "call-1", background: true });

			const [first, second] = await Promise.all([
				dispatch!.register({ parentSessionId: "pi-1", title: "审计 a", agent: "reviewer" }),
				dispatch!.register({ parentSessionId: "pi-1", title: "审计 b", agent: "reviewer" }),
			]);
			expect(new Set([first.id, second.id])).toEqual(new Set(["sa-1", "sa-2"]));
			expect(first.sessionFile).toBe(`/s/${first.id}.jsonl`);

			dispatch!.progress(first.id, { turns: 2, toolCalls: 3, lastActivity: "bash rg" });
			host.stop(second.id);
			await Bun.sleep(50);
			expect(stops).toEqual([second.id]);

			dispatch!.end(first.id, "completed");
			dispatch!.end(second.id, "stopped");
			dispatch!.finish("两项结果");
			dispatch!.finish("又一次");
			await host.connectionClosed;
			expect(host.lines.filter((line) => line.op !== "start").slice(1)).toEqual([
				{ op: "progress", id: first.id, turns: 2, toolCalls: 3, lastActivity: "bash rg" },
				{ op: "end", id: first.id, status: "completed" },
				{ op: "end", id: second.id, status: "stopped" },
				{ op: "finish", text: "两项结果" },
			]);
		} finally {
			await host.close();
		}
	});

	test("registering after the dispatch finished fails instead of hanging", async () => {
		const host = await fakeHost();
		try {
			const dispatch = await openSubagentDispatch({ toolCallId: "call-2", background: false, title: "t" }, () => {}, host.path);
			dispatch!.finish();
			await expect(dispatch!.register({ parentSessionId: "", title: "t", agent: "a" })).rejects.toThrow();
		} finally {
			await host.close();
		}
	});

	test("runs untracked when the host is unreachable", async () => {
		const missing = join(tmpdir(), `smelt-subagent-missing-${process.pid}.sock`);
		expect(await openSubagentDispatch({ toolCallId: "c", background: true, title: "t" }, () => {}, missing)).toBeUndefined();
	});
});
