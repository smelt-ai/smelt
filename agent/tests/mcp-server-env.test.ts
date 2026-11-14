import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "bun:test";
import { DefaultResourceLoader, type InlineExtension } from "@earendil-works/pi-coding-agent";
import { builtInExtensions } from "../node_modules/@earendil-works/pi-coding-agent/dist/extensions/index.js";
import { StdioTransport } from "@earendil-works/pi-mcp";
import {
	bunCacheFor,
	createPiDefaultTransport,
	createSmeltMcpTransport,
	pruneOtherVersions,
} from "../src/pi-mcp-runtime.ts";
import { SMELT_BUILTIN_MCP_EXTENSION, smeltExtensionFactories } from "../src/runtime-extensions.ts";

/** daemon 写进 Pi 进程环境的会话控制变量（见 `pi_rpc.rs` 的 `spawn_process`）。 */
const CONTROL = {
	SMELT_SESSION_ID: "session-1",
	SMELT_AGENT_TOKEN: "secret-token",
	SMELT_SOCK: "/tmp/smeltd.sock",
	SMELT_BACKGROUND_TASK_SOCK: "/tmp/background.sock",
};

const cleanups: Array<() => void> = [];
afterEach(() => {
	for (const cleanup of cleanups.splice(0)) cleanup();
});

function withEnvironment(values: Record<string, string>): void {
	const saved = Object.fromEntries(Object.keys(values).map((name) => [name, process.env[name]]));
	Object.assign(process.env, values);
	cleanups.push(() => {
		for (const [name, value] of Object.entries(saved)) {
			if (value === undefined) delete process.env[name];
			else process.env[name] = value;
		}
	});
}

function temporaryDir(): string {
	const dir = mkdtempSync(join(tmpdir(), "smelt-mcp-env-"));
	cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
	return dir;
}

describe("MCP server environment", () => {
	test("a stdio server started by Smelt's Pi does not get the session's control variables", async () => {
		withEnvironment({ ...CONTROL, SMELT_HOME: "/home/me/.smelt" });
		const dir = temporaryDir();
		const dump = join(dir, "env.json");
		// 一个只把自己拿到的环境写出来就退出的「server」。
		const transport = createSmeltMcpTransport(
			{
				name: "probe",
				config: {
					command: process.execPath,
					args: ["-e", `require("node:fs").writeFileSync(${JSON.stringify(dump)}, JSON.stringify(process.env))`],
					env: { PROBE: "1", SMELT_EXPLICIT: "kept" },
				},
				source: "test",
			},
			dir,
			undefined,
		);
		const closed = new Promise<void>((resolve) => transport.onClose(() => resolve()));
		await transport.start();
		await closed;
		await transport.close();

		const seen = JSON.parse(readFileSync(dump, "utf8")) as Record<string, string | undefined>;
		for (const name of Object.keys(CONTROL)) expect(seen[name]).toBeUndefined();
		// 其余环境照旧继承；`mcp.json` 显式写给它的值照旧生效。
		expect(seen.PATH).toBeDefined();
		expect(seen.SMELT_HOME).toBe("/home/me/.smelt");
		expect(seen.PROBE).toBe("1");
		expect(seen.SMELT_EXPLICIT).toBe("kept");
	});

	// MCP 预设写的是 smelt-agent 的启动名；Windows 上它只是指针文件，得换成正在跑的这一份。
	test("presets on the smelt-agent launcher run on this smelt-agent", () => {
		const home = temporaryDir();
		withEnvironment({ SMELT_HOME: home });
		for (const create of [createPiDefaultTransport, createSmeltMcpTransport]) {
			const transport = create(
				{
					name: "playwright",
					config: {
						command: "~/.smelt/bin/smelt-agent",
						args: ["x", "--bun", "@playwright/mcp@0.0.83"],
						env: { BUN_BE_BUN: "1" },
					},
					source: "test",
				},
				process.cwd(),
				undefined,
			);
			expect(transport).toBeInstanceOf(StdioTransport);
			const options = (transport as StdioTransport).options;
			expect(options.command).toBe(process.execPath);
			expect(options.args).toEqual(["x", "--bun", "@playwright/mcp@0.0.83"]);
			expect(options.env?.BUN_BE_BUN).toBe("1");
			expect(options.env?.BUN_INSTALL_CACHE_DIR).toBe(join(home, "cache", "bun-x", "@playwright+mcp@0.0.83"));
		}
		const other = createPiDefaultTransport(
			{ name: "other", config: { command: "npx", args: ["-y", "x"] }, source: "test" },
			process.cwd(),
			undefined,
		);
		expect((other as StdioTransport).options.command).toBe("npx");
	});

	test("bun x packages get one Smelt-owned cache per pinned version", () => {
		const home = temporaryDir();
		const env = { SMELT_HOME: home };
		const root = join(home, "cache", "bun-x");
		expect(bunCacheFor(["x", "--bun", "rea-agents@6.9.0", "mcp"], env)).toBe(join(root, "rea-agents@6.9.0"));
		expect(existsSync(join(root, "rea-agents@6.9.0"))).toBe(true);
		// 不是 `bun x`、包名认不出：照 Bun 默认的来。
		expect(bunCacheFor(["run", "x.ts"], env)).toBeUndefined();
		expect(bunCacheFor(["x", "../../etc"], env)).toBeUndefined();
		expect(bunCacheFor(["x", ".."], env)).toBeUndefined();
		expect(bunCacheFor(["x", "rea@.."], env)).toBeUndefined();
		expect(bunCacheFor(["x", "--bun"], env)).toBeUndefined();
	});

	test("other versions of the same package go once nobody has used them for a day", async () => {
		const home = temporaryDir();
		const env = { SMELT_HOME: home };
		const root = join(home, "cache", "bun-x");
		const now = Date.now();
		const make = (name: string, ageMs: number) => {
			mkdirSync(join(root, name), { recursive: true });
			const at = (now - ageMs) / 1000;
			utimesSync(join(root, name), at, at);
		};
		const day = 24 * 60 * 60 * 1000;
		make("rea-agents@6.5.0", 2 * day);
		make("rea-agents@6.8.0", 60 * 1000);
		make("rea-agents-extra@1.0.0", 2 * day);
		make("@playwright+mcp@0.0.80", 2 * day);

		expect(bunCacheFor(["x", "--bun", "rea-agents@6.9.0", "mcp"], env, now)).toBe(join(root, "rea-agents@6.9.0"));
		await pruneOtherVersions(root, "rea-agents@6.9.0", now);

		expect(existsSync(join(root, "rea-agents@6.5.0"))).toBe(false);
		expect(existsSync(join(root, "rea-agents@6.8.0"))).toBe(true);
		expect(existsSync(join(root, "rea-agents-extra@1.0.0"))).toBe(true);
		expect(existsSync(join(root, "@playwright+mcp@0.0.80"))).toBe(true);
		// 自己这份刚用过，不会被别的版本当成没人用。
		await pruneOtherVersions(root, "rea-agents@6.5.0", now + 2 * day);
		expect(existsSync(join(root, "rea-agents@6.9.0"))).toBe(false);
	});

	test("an explicit BUN_INSTALL_CACHE_DIR in mcp.json wins", () => {
		const transport = createPiDefaultTransport(
			{
				name: "rea",
				config: {
					command: "~/.smelt/bin/smelt-agent",
					args: ["x", "--bun", "rea-agents@6.9.0", "mcp"],
					env: { BUN_BE_BUN: "1", BUN_INSTALL_CACHE_DIR: "/data/bun" },
				},
				source: "test",
			},
			process.cwd(),
			undefined,
		);
		expect((transport as StdioTransport).options.env?.BUN_INSTALL_CACHE_DIR).toBe("/data/bun");
	});

	test("HTTP servers are left to Pi's own transport", () => {
		const transport = createSmeltMcpTransport(
			{ name: "figma", config: { url: "https://mcp.figma.com/mcp" }, source: "test" },
			process.cwd(),
			undefined,
		);
		expect(transport.constructor.name).toBe("StreamableHttpTransport");
	});

	test("Pi loads Smelt's code for its built-in MCP extension", () => {
		// 替的是 Pi 自带的那一个：名字对不上就成了多出来的一个扩展，而 Pi 的照旧泄漏。
		expect(builtInExtensions.some((input) => typeof input !== "function" && input.name === "mcp")).toBe(true);
		const dir = temporaryDir();
		// 和 Pi `main` 拼扩展列表的方式一致：先 Pi 自带的，再宿主给的。
		const loader = new DefaultResourceLoader({
			cwd: dir,
			agentDir: dir,
			extensionFactories: [...builtInExtensions, ...smeltExtensionFactories(undefined)],
		});
		const builtins = (loader as unknown as { builtinExtensions: Map<string, InlineExtension> })
			.builtinExtensions;
		expect(builtins.get("mcp")).toBe(SMELT_BUILTIN_MCP_EXTENSION);
	});
});
