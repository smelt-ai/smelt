/**
 * 编译出来的 smelt-pi 必须在没有 node_modules 的目录里走完 RPC 握手。
 * 用法：bun scripts/compiled-smoke.ts <path-to-smelt-pi>
 */
import { spawn } from "node:child_process";
import { mkdtempSync, copyFileSync, rmSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, dirname } from "node:path";
import { createInterface } from "node:readline";
import pkg from "../packages/pi-agent/package.json" with { type: "json" };

const source = process.argv[2];
if (!source) {
	console.error("用法：bun scripts/compiled-smoke.ts <smelt-pi>");
	process.exit(2);
}

const dir = mkdtempSync(join(tmpdir(), "smelt-pi-smoke-"));
const agentDir = join(dir, "agent");
const extensionDir = join(agentDir, "extensions");
mkdirSync(extensionDir, { recursive: true });
writeFileSync(
	join(extensionDir, "smoke-command.ts"),
	`export default function smokeCommand(pi) {
  pi.registerCommand("smelt-smoke-command", {
    description: "compiled smoke",
    handler: async () => {},
  });
}
`,
);
// 沿用原文件名：Windows 上没有 .exe 后缀的文件起不了进程。
const binary = join(dir, basename(source));
copyFileSync(source, binary);
copyFileSync(join(dirname(source), "photon_rs_bg.wasm"), join(dir, "photon_rs_bg.wasm"));
const { chmodSync } = await import("node:fs");
chmodSync(binary, 0o755);

const probe = spawn(binary, [], {
	cwd: dir,
	env: { ...process.env, SMELT_PI_PROBE_WORKERS: "1", PI_CODING_AGENT_DIR: agentDir },
	stdio: ["ignore", "pipe", "pipe"],
});
const probeResult = await new Promise<{ text: string; code: number | null }>((resolve) => {
	let text = "";
	const timer = setTimeout(() => {
		probe.kill("SIGKILL");
	}, 30_000);
	probe.stdout.setEncoding("utf8");
	probe.stderr.setEncoding("utf8");
	probe.stdout.on("data", (chunk: string) => {
		text += chunk;
	});
	probe.stderr.on("data", (chunk: string) => {
		text += chunk;
	});
	// 起不来时只有 error 没有 exit；不接住它，这个 Promise 永远不落地，发版作业就挂到超时。
	probe.on("error", (error) => {
		clearTimeout(timer);
		resolve({ text: `${text}${error.message}\n`, code: null });
	});
	probe.on("exit", (code) => {
		clearTimeout(timer);
		resolve({ text, code });
	});
});
if (
	probeResult.code !== 0 ||
	!probeResult.text.includes("image-resize-worker.js ok") ||
	!probeResult.text.includes("codemode/worker.js ok")
) {
	console.error(probeResult.text);
	rmSync(dir, { recursive: true, force: true });
	process.exit(1);
}

const child = spawn(binary, ["--offline", "--no-approve"], {
	cwd: dir,
	env: {
		...process.env,
		PI_CODING_AGENT_DIR: agentDir,
		PATH: process.env.PATH ?? "",
		// 守护起 smelt-pi 时一定带这个；不带它会拒绝启动。
		SMELT_PI_PROTOCOL: String(pkg.smeltProtocol),
	},
	stdio: ["pipe", "pipe", "pipe"],
});

const stderr: string[] = [];
child.stderr.setEncoding("utf8");
child.stderr.on("data", (chunk: string) => stderr.push(chunk));

const lines = createInterface({ input: child.stdout });
const pending = new Map<string, (value: Record<string, unknown>) => void>();
const REQUEST_TIMEOUT_MS = 20_000;
let failed = false;

function fail(message: string): never {
	if (failed) process.exit(1);
	failed = true;
	child.kill("SIGKILL");
	console.error(message);
	if (stderr.length) console.error(stderr.join(""));
	rmSync(dir, { recursive: true, force: true });
	process.exit(1);
}

child.on("error", (error) => fail(`smelt-pi 起不来：${error.message}`));

child.on("exit", (code, signal) => {
	if (failed || pending.size === 0) return;
	const waiting = [...pending.keys()].join(", ");
	fail(`smelt-pi 在请求 ${waiting} 尚未回执时退出（code=${code ?? "null"} signal=${signal ?? "null"}）`);
});

lines.on("line", (line) => {
	let value: Record<string, unknown>;
	try {
		value = JSON.parse(line) as Record<string, unknown>;
	} catch {
		return;
	}
	const id = typeof value.id === "string" ? value.id : "";
	if (value.type === "response" && pending.has(id)) {
		pending.get(id)?.(value);
		pending.delete(id);
	}
});

function request(id: string, command: Record<string, unknown>): Promise<Record<string, unknown>> {
	return new Promise((resolve) => {
		const timer = setTimeout(() => {
			pending.delete(id);
			fail(`请求 ${id} 超过 ${REQUEST_TIMEOUT_MS}ms 没有回执`);
		}, REQUEST_TIMEOUT_MS);
		pending.set(id, (value) => {
			clearTimeout(timer);
			resolve(value);
		});
		child.stdin.write(`${JSON.stringify({ id, ...command })}\n`);
	});
}

const started = performance.now();
const state = await request("smoke-state", { type: "get_state" });
const readyMs = performance.now() - started;
if (state.success !== true) fail(`get_state 失败：${JSON.stringify(state)}`);
const models = await request("smoke-models", { type: "get_available_models" });
if (models.success !== true) fail(`get_available_models 失败：${JSON.stringify(models)}`);
const thinking = await request("smoke-thinking", { type: "get_available_thinking_levels" });
if (thinking.success !== true) fail(`get_available_thinking_levels 失败：${JSON.stringify(thinking)}`);
const commands = await request("smoke-commands", { type: "get_commands" });
if (commands.success !== true) fail(`get_commands 失败：${JSON.stringify(commands)}`);
const commandList = JSON.stringify(commands);
if (!commandList.includes("smelt-smoke-command")) {
	fail(`jiti 扩展命令没有注册：${commandList}`);
}

const seen: string[] = [];
lines.on("line", (line) => seen.push(line));
const prompt = await request("smoke-prompt", {
	type: "prompt",
	message: "reply with the word pong",
	images: [
		{
			type: "image",
			mimeType: "image/png",
			data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
		},
	],
});
const MODULE_MISSING = /cannot find module|MODULE_NOT_FOUND|module not found/i;
const REACHED_MODEL =
	/unauthorized|invalid api key|api key|authentication|ENOTFOUND|ECONNREFUSED|fetch failed|network|offline|401|403/i;
const trail = () => `${JSON.stringify(prompt)}\n${seen.join("\n")}\n${stderr.join("")}`;
// 每来一行输出看一次：缺模块立刻失败，走到模型或网络错误就算通过，20 秒是上限。
await new Promise<void>((resolve) => {
	const timer = setTimeout(done, 20_000);
	function done() {
		clearTimeout(timer);
		lines.off("line", check);
		child.stderr.off("data", check);
		resolve();
	}
	function check() {
		const text = trail();
		if (MODULE_MISSING.test(text)) fail(`懒加载块没有留在 bunfs 里：${text.slice(-2000)}`);
		if (REACHED_MODEL.test(text)) done();
	}
	lines.on("line", check);
	child.stderr.on("data", check);
	check();
});
const trailed = trail();
if (!REACHED_MODEL.test(trailed)) {
	fail(`提示没有走到真实的模型或网络错误：${trailed.slice(-2000)}`);
}

child.kill("SIGTERM");
await new Promise((resolve) => child.once("exit", resolve));
rmSync(dir, { recursive: true, force: true });
console.log(`smelt-pi RPC get_state ${readyMs.toFixed(1)}ms`);
