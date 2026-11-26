import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "bun:test";
import smeltSessionPeersExtension from "../src/session-peers.ts";

type Tool = {
	name: string;
	execute: (toolCallId: string, params: Record<string, unknown>, signal?: AbortSignal) => Promise<{
		content: { text: string }[];
		details: Record<string, unknown>;
		isError?: boolean;
	}>;
};

/** 只够注册工具的假 Pi。 */
function fakePi() {
	const tools = new Map<string, Tool>();
	const pi = {
		getAllTools: () => [],
		registerTool: (tool: Tool) => tools.set(tool.name, tool),
		on: () => {},
	};
	return { pi, tools };
}

/** 假会话宿主：记下每个请求，按 op 回答。 */
async function fakeHost(answer: (request: Record<string, unknown>) => Record<string, unknown>) {
	const path = join(tmpdir(), `smelt-peers-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
	const requests: Record<string, unknown>[] = [];
	const server = createServer((socket) => {
		let buffer = "";
		socket.on("data", (chunk) => {
			buffer += chunk.toString();
			const newline = buffer.indexOf("\n");
			if (newline < 0) return;
			const request = JSON.parse(buffer.slice(0, newline)) as Record<string, unknown>;
			requests.push(request);
			socket.end(`${JSON.stringify(answer(request))}\n`);
		});
	});
	await new Promise<void>((resolve) => server.listen(path, resolve));
	return {
		path,
		requests,
		close: () => new Promise<void>((resolve) => server.close(() => resolve())),
	};
}

const savedSock = process.env.SMELT_BACKGROUND_TASK_SOCK;
const savedOwner = process.env.SMELT_BACKGROUND_TASK_OWNER;
afterEach(() => {
	process.env.SMELT_BACKGROUND_TASK_SOCK = savedSock;
	process.env.SMELT_BACKGROUND_TASK_OWNER = savedOwner;
	if (savedSock === undefined) delete process.env.SMELT_BACKGROUND_TASK_SOCK;
	if (savedOwner === undefined) delete process.env.SMELT_BACKGROUND_TASK_OWNER;
});

/** 主 daemon 给的定义，只留扩展用到的字段。 */
const DEFINITIONS = ["session_list", "session_read", "session_send", "session_spawn"].map((name) => ({
	name,
	label: name,
	description: `${name} 的说明`,
	promptSnippet: name,
	promptGuidelines: [`use ${name}`],
	parameters: { type: "object", properties: { to: { type: "string" }, text: { type: "string" }, kind: { type: "string" } } },
}));

describe("Pi session peers", () => {
	test("tools come from the daemon and their answers are passed through untouched", async () => {
		const host = await fakeHost((request) => {
			if (request.op === "session_tools") return { ok: true, tools: DEFINITIONS };
			if (request.op === "session_send") {
				return {
					ok: true,
					id: "pm-7",
					text: "Smelt 已收下",
					details: { to: { id: "acp-b", title: "测试" }, kind: "task", messageId: "pm-7" },
				};
			}
			return { ok: false, error: "对方还有 8 条消息没收", details: { error: true, reason: "inbox_full" } };
		});
		try {
			process.env.SMELT_BACKGROUND_TASK_SOCK = host.path;
			delete process.env.SMELT_BACKGROUND_TASK_OWNER;
			const { pi, tools } = fakePi();
			await smeltSessionPeersExtension(pi as never);
			expect([...tools.keys()]).toEqual(["session_list", "session_read", "session_send", "session_spawn"]);
			expect(host.requests[0]).toMatchObject({ op: "session_tools", owner: "" });

			const sent = await tools
				.get("session_send")!
				.execute("call-1", { to: "acp-b", text: "跑测试", kind: "task", owner: "x/forged" });
			expect(sent.isError).toBeUndefined();
			expect(sent.content[0]?.text).toBe("Smelt 已收下");
			expect(sent.details).toEqual({ to: { id: "acp-b", title: "测试" }, kind: "task", messageId: "pm-7" });
			// 这次的工具调用 id 一起交上去：主 daemon 按它和来源算出消息 id，重放不会多送。
			expect(host.requests[1]).toMatchObject({
				op: "session_send",
				owner: "",
				toolCallId: "call-1",
				to: "acp-b",
				text: "跑测试",
				kind: "task",
			});
			expect(host.requests[1]).not.toHaveProperty("sourceSessionId");
			// 定义里没有的参数不往上交，盖不掉连接上的身份。
			expect(host.requests[1]?.owner).toBe("");

			const listed = await tools.get("session_list")!.execute("call-2", {});
			expect(listed.isError).toBe(true);
			expect(listed.content[0]?.text).toBe("对方还有 8 条消息没收");
			expect(listed.details).toEqual({ error: true, reason: "inbox_full" });
		} finally {
			await host.close();
		}
	});

	test("without definitions from the daemon no tool is registered", async () => {
		const host = await fakeHost(() => ({ ok: false, error: "Subagents can't message other conversations" }));
		try {
			process.env.SMELT_BACKGROUND_TASK_SOCK = host.path;
			process.env.SMELT_BACKGROUND_TASK_OWNER = "call-1/sa-1";
			const { pi, tools } = fakePi();
			await smeltSessionPeersExtension(pi as never);
			expect(tools.size).toBe(0);
			expect(host.requests[0]).toMatchObject({ op: "session_tools", owner: "call-1/sa-1" });
		} finally {
			await host.close();
		}
		delete process.env.SMELT_BACKGROUND_TASK_SOCK;
		const { pi, tools } = fakePi();
		await smeltSessionPeersExtension(pi as never);
		expect(tools.size).toBe(0);
	});

	test("a tool of the same name the user already has is not replaced", async () => {
		const host = await fakeHost(() => ({ ok: true, tools: DEFINITIONS }));
		try {
			process.env.SMELT_BACKGROUND_TASK_SOCK = host.path;
			delete process.env.SMELT_BACKGROUND_TASK_OWNER;
			const { pi, tools } = fakePi();
			pi.getAllTools = () => [{ name: "session_send" }] as never;
			await smeltSessionPeersExtension(pi as never);
			expect([...tools.keys()]).toEqual(["session_list", "session_read", "session_spawn"]);
		} finally {
			await host.close();
		}
	});
});
