import { createMcpExtension, type InlineExtension } from "@earendil-works/pi-coding-agent";
import {
	SMELT_BACKGROUND_TASK_EXTENSION_NAME,
	smeltBackgroundTaskExtension,
} from "./background-task.ts";
import smeltCodemodeExtension from "./codemode.ts";
import smeltContextUsageExtension from "./context-usage.ts";
import {
	SMELT_ELICITATION_EXTENSION_NAME,
	smeltElicitationExtension,
} from "./elicitation.ts";
import { SMELT_MCP_EXTENSION_NAME, smeltMcpExtension } from "./mcp-selection.ts";
import smeltMemoryExtension, { SMELT_MEMORY_EXTENSION_NAME } from "./memory-extension.ts";
import { createSmeltMcpTransport } from "./pi-mcp-runtime.ts";
import { SMELT_SESSION_PEERS_EXTENSION_NAME, smeltSessionPeersExtension } from "./session-peers.ts";
import smeltPermissionExtension from "./smelt-permission.ts";
import smeltSubagentExtension, { SMELT_SUBAGENT_EXTENSION_NAME } from "./subagent/index.ts";

// Pi appends inline factories after discovered user/project extensions. Stable names make their
// sourceInfo auditable. Permission stays last so host approval sees the final tool arguments;
// memory sits right before it so its validation and approval hint also see the final arguments.
export const SMELT_EXTENSION_FACTORIES = [
	{ name: SMELT_ELICITATION_EXTENSION_NAME, factory: smeltElicitationExtension, hidden: true },
	{ name: "smelt-context-usage", factory: smeltContextUsageExtension, hidden: true },
	{ name: SMELT_SUBAGENT_EXTENSION_NAME, factory: smeltSubagentExtension, hidden: true },
	{ name: SMELT_BACKGROUND_TASK_EXTENSION_NAME, factory: smeltBackgroundTaskExtension, hidden: true },
	{ name: SMELT_SESSION_PEERS_EXTENSION_NAME, factory: smeltSessionPeersExtension, hidden: true },
	{ name: "smelt-codemode", factory: smeltCodemodeExtension, hidden: true },
	{ name: SMELT_MEMORY_EXTENSION_NAME, factory: smeltMemoryExtension, hidden: true },
	{ name: "smelt-permission", factory: smeltPermissionExtension, hidden: true },
] satisfies InlineExtension[];

/**
 * Add the agent's MCP extension when it checked any server. It goes right before permission, which
 * must stay last so host approval sees the final tool arguments of MCP calls too. No selection, or
 * an empty one, leaves the list unchanged: plain Pi sessions keep the built-in MCP extension, and a
 * product agent that checked nothing connects no server.
 */
export function withAgentMcp(
	factories: readonly InlineExtension[],
	selection: ReadonlySet<string> | undefined,
): InlineExtension[] {
	if (!selection || selection.size === 0) return [...factories];
	const permission = factories.length - 1;
	return [
		...factories.slice(0, permission),
		{ name: SMELT_MCP_EXTENSION_NAME, factory: smeltMcpExtension(selection), hidden: true },
		...factories.slice(permission),
	];
}

/**
 * Pi 内置 MCP 扩展（`builtin:mcp`）的代码，换成只改 transport 的同一个扩展。
 *
 * 裸 Pi 会话用的是内置 MCP 扩展，它默认让 stdio server 整份继承 Pi 进程的环境，会话 token 也在里面。
 * Pi 允许宿主用同名的 `builtin: true` 条目提供内置扩展的代码（后给的覆盖 Pi 自带的），所以默认加载、
 * `pi config` 开关、`--no-extensions` 关掉它、第三方 MCP 扩展注册 `/mcp` 时让位，这些行为都不变。
 */
export const SMELT_BUILTIN_MCP_EXTENSION = {
	name: "mcp",
	factory: createMcpExtension({ createTransport: createSmeltMcpTransport }),
	replaceable: true,
	builtin: true,
} satisfies InlineExtension;

/** 交给 Pi `main` 的全部扩展：内置 MCP 的替身，加上 Smelt 自己的扩展和产品智能体勾选的 MCP。 */
export function smeltExtensionFactories(selection: ReadonlySet<string> | undefined): InlineExtension[] {
	return [SMELT_BUILTIN_MCP_EXTENSION, ...withAgentMcp(SMELT_EXTENSION_FACTORIES, selection)];
}
