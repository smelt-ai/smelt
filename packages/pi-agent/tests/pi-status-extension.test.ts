import { afterAll, expect, test } from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// 这个扩展由 smeltd 装给终端里的 pi，加载时就读环境变量决定是否上报。
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "smelt-pi-status-"));
const log = path.join(dir, "events.log");
const notify = path.join(dir, "notify.sh");
fs.writeFileSync(notify, `#!/bin/sh\ncat > /dev/null\necho "$SMELT_HOOK_EVENT" >> "${log}"\n`, {
	mode: 0o755,
});
process.env.SMELT_NOTIFY_BIN = notify;
process.env.SMELT_SESSION_ID = "pi-status-test";
process.env.SMELT_SOCK = path.join(dir, "smeltd.sock");

// 资源在 Rust crate 里，不在本包的类型解析范围内，按路径动态加载。
const extensionPath = path.resolve(import.meta.dir, "../../../crates/smeltd/assets/pi-status-extension.ts");
const { default: statusExtension } = await import(extensionPath);

type Handler = (event?: unknown) => unknown;

function load(): Map<string, Handler> {
	const handlers = new Map<string, Handler>();
	statusExtension({
		on(name: string, handler: Handler) {
			handlers.set(name, handler);
		},
		getSessionName: () => undefined,
	});
	return handlers;
}

function reported(): string[] {
	if (!fs.existsSync(log)) return [];
	return fs.readFileSync(log, "utf8").split("\n").filter(Boolean);
}

async function run(handlers: Map<string, Handler>, stopReason: string, aborted: boolean): Promise<string[]> {
	fs.rmSync(log, { force: true });
	await handlers.get("agent_start")?.();
	await handlers.get("agent_end")?.({ messages: [{ role: "assistant", stopReason }] });
	await handlers.get("agent_settled")?.({ aborted });
	return reported();
}

afterAll(() => {
	fs.rmSync(dir, { recursive: true, force: true });
});

test("用户按 Esc 停下报 Stop，不报失败", async () => {
	const handlers = load();
	expect(await run(handlers, "aborted", true)).toEqual(["UserPromptSubmit", "Stop"]);
	// 退避重试时被停，最后一条消息还是 error。
	expect(await run(handlers, "error", true)).toEqual(["UserPromptSubmit", "Stop"]);
});

test("真正的模型错误仍报 StopFailure", async () => {
	const handlers = load();
	expect(await run(handlers, "error", false)).toEqual(["UserPromptSubmit", "StopFailure"]);
	expect(await run(handlers, "stop", false)).toEqual(["UserPromptSubmit", "Stop"]);
});
