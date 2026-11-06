import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "bun:test";
import smeltNotifyExtension, { notifyText, SMELT_NOTIFY_TOOL_NAME } from "../src/notify-user.ts";

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

/** 假会话宿主：记下每个请求，回固定的答复。 */
async function fakeHost(answer: Record<string, unknown>) {
	const path = join(tmpdir(), `smelt-notify-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
	const requests: Record<string, unknown>[] = [];
	const server = createServer((socket) => {
		let buffer = "";
		socket.on("data", (chunk) => {
			buffer += chunk.toString();
			const newline = buffer.indexOf("\n");
			if (newline < 0) return;
			requests.push(JSON.parse(buffer.slice(0, newline)) as Record<string, unknown>);
			socket.end(`${JSON.stringify(answer)}\n`);
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

describe("notify_user", () => {
	test("tells the model where the notification went", () => {
		expect(notifyText({ ok: true, desktop: true, phones: 2 })).toBe("桌面已弹出通知；已推送到 2 台手机。");
		const text = notifyText({ ok: true, desktop: false, phones: 0 });
		expect(text).toContain("桌面没有连着的 Smelt 窗口");
		expect(text).toContain("没有手机在接收智能体消息");
	});

	test("sends the message and title to the session host", async () => {
		delete process.env.SMELT_BACKGROUND_TASK_OWNER;
		const host = await fakeHost({ ok: true, desktop: true, phones: 1 });
		process.env.SMELT_BACKGROUND_TASK_SOCK = host.path;
		try {
			const { pi, tools } = fakePi();
			smeltNotifyExtension(pi as never);
			const tool = tools.get(SMELT_NOTIFY_TOOL_NAME)!;
			const result = await tool.execute("call-1", { message: "磁盘告警", title: "巡检" });
			expect(host.requests).toEqual([{ op: "notify", message: "磁盘告警", title: "巡检", owner: "" }]);
			expect(result.isError).toBeUndefined();
			expect(result.details).toEqual({ desktop: true, phones: 1 });
			expect(result.content[0].text).toBe("桌面已弹出通知；已推送到 1 台手机。");
		} finally {
			await host.close();
		}
	});

	test("reports the host's refusal as a tool error", async () => {
		delete process.env.SMELT_BACKGROUND_TASK_OWNER;
		const host = await fakeHost({ ok: false, error: "通知内容是空的" });
		process.env.SMELT_BACKGROUND_TASK_SOCK = host.path;
		try {
			const { pi, tools } = fakePi();
			smeltNotifyExtension(pi as never);
			const result = await tools.get(SMELT_NOTIFY_TOOL_NAME)!.execute("call-1", { message: " " });
			expect(result.isError).toBe(true);
			expect(result.content[0].text).toBe("通知内容是空的");
		} finally {
			await host.close();
		}
	});

	test("subagents do not get the tool", () => {
		process.env.SMELT_BACKGROUND_TASK_OWNER = "parent/child";
		const { pi, tools } = fakePi();
		smeltNotifyExtension(pi as never);
		expect(tools.has(SMELT_NOTIFY_TOOL_NAME)).toBe(false);
	});
});
