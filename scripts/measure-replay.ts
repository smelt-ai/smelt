/**
 * 对一份短会话和一份 500 条以上、工具输出很大的会话，量 Pi `get_entries` 的
 * 响应时间和字节数。转录本身不再走这条 RPC；这里只为对照旧路径。
 *
 * 用法：bun scripts/measure-replay.ts <path-to-smelt-pi>
 */
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { createInterface } from "node:readline";
import { copyFileSync } from "node:fs";

const source = process.argv[2];
if (!source) {
	console.error("用法：bun scripts/measure-replay.ts <smelt-pi>");
	process.exit(2);
}

const toolBytes = 8 * 1024;
const blob = "x".repeat(toolBytes);

function sessionFile(turns: number): string {
	const lines: string[] = [
		JSON.stringify({ type: "session", version: 3, id: "measure", timestamp: "2026-01-01T00:00:00.000Z", cwd: "/tmp" }),
	];
	let parent: string | null = null;
	const push = (id: string, body: Record<string, unknown>) => {
		lines.push(JSON.stringify({ ...body, id, parentId: parent, timestamp: "2026-01-01T00:00:00.000Z" }));
		parent = id;
	};
	push("model", { type: "model_change", provider: "test", modelId: "seed" });
	let deadParent: string | null = parent;
	for (let index = 0; index < turns; index++) {
		push(`u${index}`, { type: "message", message: { role: "user", content: `u${index}` } });
		if (index === 0) {
			for (let dead = 0; dead < turns; dead++) {
				const id = `dead-${dead}`;
				lines.push(
					JSON.stringify({
						type: "message",
						id,
						parentId: deadParent,
						timestamp: "2026-01-01T00:00:00.000Z",
						message: { role: "user", content: `dead-${dead}:${blob}` },
					}),
				);
				deadParent = id;
			}
		}
		const callId = `call-${index}`;
		push(`a${index}`, {
			type: "message",
			message: {
				role: "assistant",
				content: [{ type: "toolCall", id: callId, name: "bash", arguments: { command: "echo hi" } }],
			},
		});
		push(`t${index}`, {
			type: "message",
			message: {
				role: "toolResult",
				toolCallId: callId,
				content: [{ type: "text", text: blob }],
			},
		});
	}
	return lines.join("\n") + "\n";
}

async function measure(label: string, turns: number, binary: string): Promise<void> {
	const dir = mkdtempSync(join(tmpdir(), `smelt-replay-${label}-`));
	const agentDir = join(dir, "agent");
	mkdirSync(agentDir, { recursive: true });
	const file = join(dir, "session.jsonl");
	writeFileSync(file, sessionFile(turns));
	const exe = join(dir, "smelt-pi");
	copyFileSync(binary, exe);
	copyFileSync(join(dirname(binary), "photon_rs_bg.wasm"), join(dir, "photon_rs_bg.wasm"));
	const { chmodSync } = await import("node:fs");
	chmodSync(exe, 0o755);

	const child = spawn(exe, ["--offline", "--no-approve", "--session", file], {
		cwd: dir,
		env: { ...process.env, PI_CODING_AGENT_DIR: agentDir },
		stdio: ["pipe", "pipe", "pipe"],
	});
	let stderr = "";
	child.stderr.setEncoding("utf8");
	child.stderr.on("data", (chunk: string) => {
		stderr += chunk;
	});
	const lines = createInterface({ input: child.stdout });
	const pending = new Map<string, (line: string, value: Record<string, unknown>) => void>();
	lines.on("line", (line) => {
		try {
			const value = JSON.parse(line) as Record<string, unknown>;
			const id = typeof value.id === "string" ? value.id : "";
			if (value.type === "response" && pending.has(id)) pending.get(id)?.(line, value);
		} catch {
			// 事件行。
		}
	});
	const request = (id: string, command: Record<string, unknown>) =>
		new Promise<{ line: string; value: Record<string, unknown>; ms: number }>((resolve) => {
			const started = performance.now();
			pending.set(id, (line, value) => {
				pending.delete(id);
				resolve({ line, value, ms: performance.now() - started });
			});
			child.stdin.write(`${JSON.stringify({ id, ...command })}\n`);
		});

	const state = await request("state", { type: "get_state" });
	if (state.value.success !== true) {
		child.kill("SIGKILL");
		console.error(stderr);
		throw new Error(`${label} get_state 失败：${state.line.slice(0, 500)}`);
	}
	const entries = await request("entries", { type: "get_entries" });
	child.kill("SIGKILL");
	await new Promise((resolve) => child.once("exit", resolve));
	rmSync(dir, { recursive: true, force: true });
	if (entries.value.success !== true) {
		throw new Error(`${label} get_entries 失败：${entries.line.slice(0, 500)}`);
	}
	const data = entries.value.data as { entries?: unknown[]; leafId?: string } | undefined;
	console.log(
		`SMELT_PI_GET_ENTRIES ${label} startup_get_state_ms=${state.ms.toFixed(1)} response_ms=${entries.ms.toFixed(1)} response_bytes=${Buffer.byteLength(entries.line)} entries=${data?.entries?.length ?? 0} leaf=${data?.leafId ?? ""}`,
	);
}

const root = mkdtempSync(join(tmpdir(), "smelt-replay-bin-"));
const binary = join(root, "smelt-pi");
copyFileSync(source, binary);
copyFileSync(join(dirname(source), "photon_rs_bg.wasm"), join(root, "photon_rs_bg.wasm"));
await measure("short", 8, binary);
await measure("long", 174, binary);
rmSync(root, { recursive: true, force: true });
