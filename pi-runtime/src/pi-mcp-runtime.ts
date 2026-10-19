import { pathToFileURL } from "node:url";
import type { McpTransportFactory } from "@earendil-works/pi-coding-agent";
import { StdioTransport } from "@earendil-works/pi-mcp";

/**
 * Pi's own transport factory, the one its MCP extension uses when none is given.
 *
 * It is not exported from the package entry, but it is what decides how a `mcp.json` entry becomes
 * a connection: `~` expansion, `${VAR}` and `!cmd` values in `env` and `headers`, OAuth for HTTP
 * servers. Smelt routes stdio servers through the shared gateway and hands everything else back to
 * this factory, and the gateway starts each server with it, so both keep Pi's exact semantics
 * instead of a copy that drifts. Pi is pinned to an exact version; the import fails loudly (and the
 * tests with it) if a Pi upgrade moves the module.
 */
function piModuleUrl(relative: string): string {
	const resolved = import.meta.resolve("@earendil-works/pi-coding-agent");
	const entry = resolved.startsWith("file:") ? resolved : pathToFileURL(resolved).href;
	return new URL(relative, entry).href;
}

const runtime = (await import(piModuleUrl("./extensions/mcp/runtime.js"))) as {
	createDefaultTransport?: unknown;
};

if (typeof runtime.createDefaultTransport !== "function") {
	throw new Error("Pi's MCP runtime no longer exports createDefaultTransport");
}

export const createPiDefaultTransport = runtime.createDefaultTransport as McpTransportFactory;

/**
 * MCP server 进程该拿到的环境：去掉 Smelt 的会话控制变量。
 *
 * `SMELT_SESSION_ID`、`SMELT_AGENT_TOKEN`、`SMELT_SOCK`、`SMELT_BACKGROUND_TASK_SOCK` 等是这条会话
 * 对 smeltd 的身份和凭据，只给 Pi 自己和它的工具命令用。MCP server 多是 npx / uvx 拉来的第三方
 * 代码，拿到它们就能冒充这条会话调 smeltd，所以一个都不能漏给它。`SMELT_HOME` 只是数据目录，留着。
 */
export function mcpServerEnvironment(environment: NodeJS.ProcessEnv): Record<string, string> {
	const result: Record<string, string> = {};
	for (const [name, value] of Object.entries(environment)) {
		if (value === undefined) continue;
		if (name.startsWith("SMELT_") && name !== "SMELT_HOME") continue;
		result[name] = value;
	}
	return result;
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
