import type { ExtensionAPI, ExtensionUIContext, McpTransportFactory } from "@earendil-works/pi-coding-agent";

/**
 * 这场会话真正起了连接的 MCP 服务器，报给宿主显示。
 *
 * 宿主不能按配置文件自己猜：裸 Pi 读全局和受信项目的 `mcp.json`，产品智能体只连勾选的，
 * 别的扩展还能 `registerMcpServer`。Pi 的 MCP 扩展不对外暴露连接表，但它每起一个连接都要找
 * transport 工厂要 transport，而两条路径（内置 MCP 的替身、产品智能体的勾选扩展）用的都是
 * Smelt 给的工厂，所以在工厂上记名字就是实际起过的那份。
 */
export const SMELT_MCP_SERVERS_WIDGET = "smelt-mcp-servers";

export const SMELT_MCP_SERVERS_EXTENSION_NAME = "smelt-mcp-servers";

const started = new Set<string>();
const listeners = new Set<() => void>();

/** 包一层工厂：Pi 每为一个服务器起连接就记下它的名字。工厂抛错的不算起了。 */
export function recordingStarts(factory: McpTransportFactory): McpTransportFactory {
	return (entry, cwd, authProvider) => {
		const transport = factory(entry, cwd, authProvider);
		if (!started.has(entry.name)) {
			started.add(entry.name);
			for (const listener of listeners) listener();
		}
		return transport;
	};
}

/** 已起连接的服务器名，按起连接的先后。 */
export function startedMcpServers(): string[] {
	return [...started];
}

/**
 * 会话开始时报一次当前名单（连接在后台起，这时多半还是空的），之后每多一个再报一次。
 * 会话关掉（换会话、`/reload`）时清空：新会话的 MCP 扩展会重新起连接、重新记。
 */
export function smeltMcpServersExtension(pi: ExtensionAPI): void {
	let ui: ExtensionUIContext | undefined;
	const publish = () => {
		try {
			ui?.setWidget(SMELT_MCP_SERVERS_WIDGET, [JSON.stringify(startedMcpServers())]);
		} catch {
			// RPC 宿主忽略 widget 时不能影响对话。
		}
	};
	pi.on("session_start", (_event, ctx) => {
		ui = ctx.ui;
		listeners.add(publish);
		publish();
	});
	pi.on("session_shutdown", () => {
		listeners.delete(publish);
		ui = undefined;
		started.clear();
	});
}
