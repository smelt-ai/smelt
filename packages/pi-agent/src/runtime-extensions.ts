import type { InlineExtension } from "@earendil-works/pi-coding-agent";
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
