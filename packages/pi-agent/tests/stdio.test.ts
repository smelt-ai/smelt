import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const children: Bun.Subprocess[] = [];
const tempDirs: string[] = [];

afterEach(async () => {
	for (const child of children.splice(0)) {
		if (child.exitCode === null) child.kill();
		await child.exited;
	}
	for (const dir of tempDirs.splice(0)) await rm(dir, { recursive: true, force: true });
});

describe("Pi RPC stdio entry", () => {
	test("speaks Pi's native JSONL protocol and creates a real session", async () => {
		const agentDir = await mkdtemp(join(tmpdir(), "smelt-pi-agent-test-"));
		tempDirs.push(agentDir);
		const child = Bun.spawn([
			process.execPath,
			"src/main.ts",
			"--offline",
			"--no-approve",
			"--extension",
			"src/smelt-permission.ts",
		], {
			cwd: join(import.meta.dir, ".."),
			env: { ...process.env, PI_CODING_AGENT_DIR: agentDir },
			stdin: "pipe",
			stdout: "pipe",
			stderr: "pipe",
		});
		children.push(child);

		child.stdin.write(
			`${JSON.stringify({
				id: "state",
				type: "get_state",
			})}\n`,
		);
		await child.stdin.flush();

		const line = await readLine(child.stdout);
		const response = JSON.parse(line) as {
			id: string;
			type: string;
			command: string;
			success: boolean;
			data: { sessionId: string };
		};
		expect(response.id).toBe("state");
		expect(response.type).toBe("response");
		expect(response.command).toBe("get_state");
		expect(response.success).toBe(true);
		expect(response.data.sessionId.length).toBeGreaterThan(0);

		child.stdin.end();
		await Promise.race([
			child.exited,
			Bun.sleep(5_000).then(() => {
				throw new Error("Pi RPC child did not exit after stdin closed");
			}),
		]);
	});
});

async function readLine(stream: ReadableStream<Uint8Array>): Promise<string> {
	const reader = stream.getReader();
	const decoder = new TextDecoder();
	let text = "";
	try {
		while (!text.includes("\n")) {
			const chunk = await Promise.race([
				reader.read(),
				Bun.sleep(5_000).then(() => {
					throw new Error("Timed out waiting for Pi RPC response");
				}),
			]);
			if (chunk.done) break;
			text += decoder.decode(chunk.value, { stream: true });
		}
	} finally {
		reader.releaseLock();
	}
	const [line] = text.split("\n");
	if (!line) throw new Error("Pi RPC process closed without a response");
	return line;
}
