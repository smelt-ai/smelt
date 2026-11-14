import { mkdirSync, readdirSync, rmSync, statSync, utimesSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import type { McpTransportFactory } from "@earendil-works/pi-coding-agent";
import { createDefaultTransport as piCreateDefaultTransport } from "../node_modules/@earendil-works/pi-coding-agent/dist/extensions/mcp/runtime.js";
import { StdioTransport } from "@earendil-works/pi-mcp";
import { withoutSessionControlEnv } from "./session-control-env.ts";

/**
 * Pi's own transport factory, the one its MCP extension uses when none is given, except that Smelt's
 * MCP presets run on this smelt-agent (`onThisSmeltAgent`).
 *
 * It is not exported from the package entry, but it is what decides how a `mcp.json` entry becomes
 * a connection: `~` expansion, `${VAR}` and `!cmd` values in `env` and `headers`, OAuth for HTTP
 * servers. Smelt routes stdio servers through the shared gateway and hands everything else back to
 * this factory, and the gateway starts each server with it, so both keep Pi's exact semantics
 * instead of a copy that drifts. The path is a file inside the pinned package, not a package
 * export: `import.meta.resolve` plus a dynamic import does not survive `bun build --compile`.
 * Pi is pinned to an exact version; the compile fails if an upgrade moves the module.
 */
export const createPiDefaultTransport: McpTransportFactory = (entry, cwd, authProvider) =>
	onThisSmeltAgent(piCreateDefaultTransport(entry, cwd, authProvider));

/**
 * Smelt 的 MCP 预设写的命令（`smelt_model::agent_paths::SMELT_AGENT_LAUNCHER_HOME_PATH`），配上
 * `BUN_BE_BUN=1` 把 smelt-agent 当 Bun 用：`smelt-agent x --bun 包@版本`。Bun 编在 smelt-agent 里，
 * 不用另装一份。
 */
export const SMELT_AGENT_LAUNCHER = join(homedir(), ".smelt", "bin", "smelt-agent");

/**
 * 命令是 smelt-agent 的启动名时，换成正在跑的这一份：版本和 Pi 一致；Windows 上那条路径只是
 * 指向当前版本的指针文件，执行不了。Pi 展开 `~` 用的也是 `join(homedir(), …)`。`bun x` 下的包
 * 进 Smelt 自己的缓存（`bunCacheFor`），不进用户的 `~/.bun`。
 */
function onThisSmeltAgent(transport: ReturnType<McpTransportFactory>): ReturnType<McpTransportFactory> {
	if (!(transport instanceof StdioTransport)) return transport;
	const command = resolve(transport.options.command);
	const launcher = resolve(SMELT_AGENT_LAUNCHER);
	const same =
		process.platform === "win32" ? command.toLowerCase() === launcher.toLowerCase() : command === launcher;
	if (!same) return transport;
	const cache = transport.options.env?.BUN_INSTALL_CACHE_DIR ? undefined : bunCacheFor(transport.options.args);
	return new StdioTransport({
		...transport.options,
		command: process.execPath,
		...(cache ? { env: { ...transport.options.env, BUN_INSTALL_CACHE_DIR: cache } } : {}),
	});
}

/** 用了就续命；同一个包别的版本这么久没用过才删，免得删掉另一段会话还在跑的那份。 */
const BUN_CACHE_GRACE_MS = 24 * 60 * 60 * 1000;

/**
 * `smelt-agent x [--flags] 包@版本 …` 用的 Bun 缓存：`<SMELT_HOME>/cache/bun-x/<包@版本>`，一个钉死的
 * 版本一个目录（包和它的依赖、`bun x` 装好的那份都在里面）。Bun 的缓存从不淘汰旧版本，混在用户的
 * `~/.bun` 里又分不清哪些是 Smelt 下的；目录归 Smelt、按版本分开，预设换了版本就能整份删掉旧的。
 * 不是 `bun x` 或包名认不出时返回 `undefined`，照 Bun 默认的来；包名按 npm 的规矩（小写、不以点开头），
 * 拼进路径不会跑出缓存目录。
 */
export function bunCacheFor(
	args: readonly string[] | undefined,
	environment: NodeJS.ProcessEnv = process.env,
	now: number = Date.now(),
): string | undefined {
	if (args?.[0] !== "x") return undefined;
	const spec = args.slice(1).find((arg) => !arg.startsWith("-"));
	if (!spec || !/^(@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*(@[A-Za-z0-9][A-Za-z0-9._+-]*)?$/.test(spec)) return undefined;
	const root = join(environment.SMELT_HOME?.trim() || join(homedir(), ".smelt"), "cache", "bun-x");
	const name = spec.replace("/", "+");
	const dir = join(root, name);
	try {
		mkdirSync(dir, { recursive: true });
		utimesSync(dir, now / 1000, now / 1000);
		pruneOtherVersions(root, name, now);
	} catch {
		// 建不了目录就让 Bun 自己报；清不掉旧版本下次再说。
	}
	return dir;
}

function pruneOtherVersions(root: string, name: string, now: number): void {
	const at = name.lastIndexOf("@");
	if (at <= 0) return;
	const prefix = name.slice(0, at + 1);
	for (const other of readdirSync(root)) {
		if (other === name || !other.startsWith(prefix) || other.slice(prefix.length).includes("@")) continue;
		const dir = join(root, other);
		if (now - statSync(dir).mtimeMs < BUN_CACHE_GRACE_MS) continue;
		rmSync(dir, { recursive: true, force: true });
	}
}

/**
 * MCP server 进程该拿到的环境：去掉 Smelt 的会话控制变量。
 *
 * `SMELT_SESSION_ID`、`SMELT_AGENT_TOKEN`、`SMELT_SOCK`、`SMELT_BACKGROUND_TASK_SOCK` 等是这条会话
 * 对 smeltd 的身份和凭据，只给 Pi 自己和它的工具命令用。MCP server 多是 npx / uvx 拉来的第三方
 * 代码，拿到它们就能冒充这条会话调 smeltd，所以一个都不能漏给它。`SMELT_HOME` 只是数据目录，留着。
 */
export function mcpServerEnvironment(environment: NodeJS.ProcessEnv): Record<string, string> {
	return withoutSessionControlEnv(environment);
}

/**
 * Smelt 运行时里所有 MCP 连接用的 transport：语义和 Pi 的默认 factory 完全一样，只是 stdio server
 * 不再整份继承 Pi 进程的环境，而是拿 `mcpServerEnvironment` 过滤后的环境，再叠上 `mcp.json` 里给它
 * 配的 `env`（显式写给它的值照旧生效）。HTTP server 不起进程，原样交给 Pi。
 */
export const createSmeltMcpTransport: McpTransportFactory = (entry, cwd, authProvider) => {
	const transport = createPiDefaultTransport(entry, cwd, authProvider);
	if (!(transport instanceof StdioTransport)) return transport;
	return new StdioTransport({
		...transport.options,
		env: { ...mcpServerEnvironment(process.env), ...transport.options.env },
		inheritEnv: false,
	});
};
