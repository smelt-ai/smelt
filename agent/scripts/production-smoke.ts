#!/usr/bin/env bun

import { access, cp, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const packageRoot = join(import.meta.dir, "..");
const stage = await mkdtemp(join(tmpdir(), "smelt-pi-production-smoke-"));

try {
	const paths = ["package.json", "bun.lock", "src"];
	try {
		await access(join(packageRoot, "patches"));
		paths.push("patches");
	} catch {
		// 没有上游补丁时不复制 patches。
	}
	for (const path of paths) {
		await cp(join(packageRoot, path), join(stage, path), { recursive: true });
	}

	const install = Bun.spawn(
		[
			process.execPath,
			"install",
			"--frozen-lockfile",
			"--production",
			"--ignore-scripts",
		],
		{
			cwd: stage,
			stdin: "ignore",
			stdout: "inherit",
			stderr: "inherit",
		},
	);
	if ((await install.exited) !== 0) {
		throw new Error("production dependency install failed");
	}

	const rpc = Bun.spawn([process.execPath, "src/main.ts", "--offline", "--no-approve"], {
		cwd: stage,
		env: {
			...process.env,
			PI_CODING_AGENT_DIR: join(stage, ".pi-agent"),
		},
		stdin: "pipe",
		stdout: "pipe",
		stderr: "pipe",
	});
	rpc.stdin.write(`${JSON.stringify({ id: "production-state", type: "get_state" })}\n`);
	rpc.stdin.end();

	const [exitCode, stdout, stderr] = await Promise.race([
		Promise.all([
			rpc.exited,
			new Response(rpc.stdout).text(),
			new Response(rpc.stderr).text(),
		]),
		Bun.sleep(15_000).then(() => {
			rpc.kill();
			throw new Error("production RPC smoke timed out");
		}),
	]);
	if (exitCode !== 0) {
		throw new Error(`production RPC exited with ${exitCode}: ${stderr.trim()}`);
	}

	const response = stdout
		.split("\n")
		.filter(Boolean)
		.map((line) => JSON.parse(line) as Record<string, unknown>)
		.find((line) => line.id === "production-state");
	if (!response || response.type !== "response" || response.success !== true) {
		throw new Error(`production RPC get_state failed: ${stdout.trim()} ${stderr.trim()}`);
	}

	const rpcMode = await readFile(
		join(
			stage,
			"node_modules/@earendil-works/pi-coding-agent/dist/modes/rpc/rpc-mode.js",
		),
		"utf8",
	);
	if (rpcMode.includes('case "reload":')) {
		throw new Error("Pi RPC must stay unpatched; /reload is an extension command");
	}
} finally {
	await rm(stage, { recursive: true, force: true });
}
