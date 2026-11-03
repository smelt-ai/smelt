import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
	createMcpExtension,
	type ExtensionAPI,
	getAgentDir,
	type McpServerConfig,
} from "@earendil-works/pi-coding-agent";
import { sharedTransportFactory } from "./mcp-gateway-client.ts";
import { createSmeltMcpTransport } from "./pi-mcp-runtime.ts";

/**
 * MCP servers a product agent checked, as a JSON string array of `mcp.json` server names.
 *
 * Product agents start with `--no-extensions`, which also drops Pi's built-in MCP extension. When
 * this variable is present, Smelt adds its own MCP extension that connects only the checked
 * servers. Absent means "not a product agent": nothing is registered here and the built-in MCP
 * extension (auto-discovered in plain Pi sessions) keeps reading the whole `mcp.json`.
 */
export const SMELT_AGENT_MCP_SERVERS_ENV = "SMELT_AGENT_MCP_SERVERS";

export const SMELT_MCP_EXTENSION_NAME = "smelt-mcp";

let consumedSelection: ReadonlySet<string> | undefined;

/**
 * Read and remove the agent's MCP selection before Pi starts, so tool subprocesses do not inherit
 * it. Returns `undefined` when there is no usable selection; an empty list means "connect none".
 */
export function consumeMcpSelection(
	environment: Record<string, string | undefined>,
): ReadonlySet<string> | undefined {
	consumedSelection = undefined;
	const raw = environment[SMELT_AGENT_MCP_SERVERS_ENV]?.trim();
	delete environment[SMELT_AGENT_MCP_SERVERS_ENV];
	if (!raw) return undefined;
	let parsed: unknown;
	try {
		parsed = JSON.parse(raw);
	} catch {
		return undefined;
	}
	if (!Array.isArray(parsed)) return undefined;
	consumedSelection = new Set(
		parsed.filter((entry): entry is string => typeof entry === "string" && entry.length > 0),
	);
	return consumedSelection;
}

/**
 * 托管子智能体沿用父进程的 MCP 勾选：把启动时消费掉的名单原样放回子进程环境。
 *
 * 子进程复用父进程的插件参数（`--no-extensions` 加 codemode / tool-search），不把名单一起带上就
 * 只有 MCP 配套扩展、没有一个 server。和插件参数一样只经内存转交，普通工具子进程拿不到。
 */
export function inheritedMcpSelectionEnvironment(): Record<string, string> {
	if (consumedSelection === undefined) return {};
	return { [SMELT_AGENT_MCP_SERVERS_ENV]: JSON.stringify([...consumedSelection]) };
}

/**
 * The checked, enabled entries of a parsed `mcp.json`. Entries stay raw: Pi validates each one when
 * it is registered, with the same rules it applies to `mcp.json`.
 */
export function selectMcpServers(
	document: unknown,
	selection: ReadonlySet<string>,
): Array<[name: string, config: McpServerConfig]> {
	if (!isRecord(document) || !isRecord(document.mcpServers)) return [];
	return Object.entries(document.mcpServers).filter(
		(entry): entry is [string, McpServerConfig] =>
			selection.has(entry[0]) && isRecord(entry[1]) && entry[1].enabled !== false,
	);
}

/**
 * Pi's MCP extension connecting only the checked servers, through public API only.
 *
 * Its own `mcp.json` loading is turned off (`loadConfig` returns nothing); the checked entries of
 * the global `mcp.json` are registered with `pi.registerMcpServer`, which validates them and lets
 * them share the global sign-in (credentials belong to a server name and URL). Project `.pi/mcp.json`
 * is not read: an agent picks from the global list.
 *
 * Stdio servers connect through the shared gateway (`mcp-gateway.ts`), so all conversations use one
 * process per server instead of one each; HTTP servers connect directly, as Pi does. The gateway
 * keeps only the latest configuration of each server running: editing `mcp.json` retires the old
 * process, and conversations reconnect to the new one.
 */
export function smeltMcpExtension(selection: ReadonlySet<string>) {
	const configPath = resolve(getAgentDir(), "mcp.json");
	const connectRegistered = createMcpExtension({
		loadConfig: () => ({ servers: [], errors: [] }),
		createTransport: sharedTransportFactory(createSmeltMcpTransport, configPath),
	});
	return (pi: ExtensionAPI) => {
		connectRegistered(pi);
		const problems: string[] = [];
		let document: unknown;
		try {
			document = JSON.parse(readFileSync(configPath, "utf8"));
		} catch (error) {
			problems.push(`读取 mcp.json 失败：${errorMessage(error)}`);
		}
		for (const [name, config] of selectMcpServers(document, selection)) {
			try {
				pi.registerMcpServer(name, config);
			} catch (error) {
				problems.push(errorMessage(error));
			}
		}
		if (problems.length > 0) {
			pi.on("session_start", (_event, ctx) => {
				ctx.ui.notify(`MCP：${problems.join("；")}`, "warning");
			});
		}
	};
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
