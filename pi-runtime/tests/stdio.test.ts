import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
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

		const stdout = new JsonlReader(child.stdout);
		child.stdin.write(
			`${JSON.stringify({
				id: "state",
				type: "get_state",
			})}\n`,
		);
		await child.stdin.flush();

		const response = (await stdout.response("state")) as {
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

	test("new_session emits session_start on startup and on each upstream rebind", async () => {
		const agentDir = await mkdtemp(join(tmpdir(), "smelt-pi-rebind-test-"));
		tempDirs.push(agentDir);
		const startsFile = join(agentDir, "session-starts.jsonl");
		const extensionFile = join(agentDir, "count-session-starts.ts");
		await writeFile(
			extensionFile,
			`import { appendFileSync } from "node:fs";\n` +
				`export default function (pi) {\n` +
				`  pi.on("session_start", (event) => {\n` +
				`    appendFileSync(${JSON.stringify(startsFile)}, JSON.stringify(event) + "\\n");\n` +
				`  });\n` +
				`}\n`,
		);

		const child = Bun.spawn(
			[
				process.execPath,
				"src/main.ts",
				"--offline",
				"--no-approve",
				"--extension",
				extensionFile,
			],
			{
				cwd: join(import.meta.dir, ".."),
				env: { ...process.env, PI_CODING_AGENT_DIR: agentDir },
				stdin: "pipe",
				stdout: "pipe",
				stderr: "pipe",
			},
		);
		children.push(child);

		const stdout = new JsonlReader(child.stdout);
		child.stdin.write(`${JSON.stringify({ id: "initial", type: "get_state" })}\n`);
		await child.stdin.flush();
		expect(await stdout.response("initial")).toMatchObject({
			id: "initial",
			success: true,
		});

		child.stdin.write(`${JSON.stringify({ id: "new", type: "new_session" })}\n`);
		await child.stdin.flush();
		expect(await stdout.response("new")).toMatchObject({
			id: "new",
			success: true,
		});

		const starts = (await readFile(startsFile, "utf8"))
			.trim()
			.split("\n")
			.map((line) => JSON.parse(line) as { reason: string });
		// 官方 RPC 在 new_session 里还会再 rebind 一次，所以 startup 之后有两次 new。
		expect(starts.map((event) => event.reason)).toEqual(["startup", "new", "new"]);
	});
});

/**
 * Pi RPC 的 stdout 上响应和事件（如扩展的 setWidget）交错输出。
 * 跨次读取保留缓冲，不丢同一块里的后续行；按 id 找响应，跳过事件。
 */
class JsonlReader {
	private readonly reader: ReadableStreamDefaultReader<Uint8Array>;
	private readonly decoder = new TextDecoder();
	private buffer = "";

	constructor(stream: ReadableStream<Uint8Array>) {
		this.reader = stream.getReader();
	}

	async response(id: string): Promise<Record<string, unknown>> {
		for (;;) {
			const message = JSON.parse(await this.line()) as Record<string, unknown>;
			if (message.type === "response" && message.id === id) return message;
		}
	}

	private async line(): Promise<string> {
		while (!this.buffer.includes("\n")) {
			const chunk = await Promise.race([
				this.reader.read(),
				Bun.sleep(5_000).then(() => {
					throw new Error("Timed out waiting for Pi RPC response");
				}),
			]);
			if (chunk.done) throw new Error("Pi RPC process closed without a response");
			this.buffer += this.decoder.decode(chunk.value, { stream: true });
		}
		const end = this.buffer.indexOf("\n");
		const line = this.buffer.slice(0, end);
		this.buffer = this.buffer.slice(end + 1);
		return line;
	}
}
