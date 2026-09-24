import type { ExtensionAPI, SourceInfo } from "@earendil-works/pi-coding-agent";
import {
	SMELT_ELICITATION_EXTENSION_PATH,
	SMELT_ELICITATION_TOOL_NAME,
} from "./elicitation.ts";

/** Rust 端据此识别 Smelt 自己的权限请求，不会误吞用户 Extension 的 confirm。 */
export const SMELT_PERMISSION_TITLE = "smelt.permission.v1";

const READ_ONLY_BUILTIN_TOOLS = new Set(["read", "grep", "find", "ls"]);

export type ToolIdentity = {
	name: string;
	sourceInfo: SourceInfo;
};

function isActualBuiltin(tool: ToolIdentity): boolean {
	return (
		tool.sourceInfo.source === "builtin" &&
		tool.sourceInfo.path === `<builtin:${tool.name}>`
	);
}

function isSmeltElicitationTool(tool: ToolIdentity): boolean {
	return (
		tool.name === SMELT_ELICITATION_TOOL_NAME &&
		tool.sourceInfo.source === "inline" &&
		tool.sourceInfo.path === SMELT_ELICITATION_EXTENSION_PATH
	);
}

export function isReservedSmeltToolConflict(
	toolName: string,
	tool: ToolIdentity | undefined,
): boolean {
	return toolName.startsWith("smelt_") && (!tool || !isSmeltElicitationTool(tool));
}

/** 只有 registry 证明身份的 Pi 内建只读工具和 Smelt 选择题免审批。其余一律 fail closed。 */
export function toolNeedsSmeltApproval(tool: ToolIdentity | undefined, _input?: unknown): boolean {
	if (!tool) {
		return true;
	}
	if (READ_ONLY_BUILTIN_TOOLS.has(tool.name) && isActualBuiltin(tool)) {
		return false;
	}
	return !isSmeltElicitationTool(tool);
}

function resolveToolIdentity(pi: ExtensionAPI, toolName: string): ToolIdentity | undefined {
	try {
		const tool = pi.getAllTools().find((candidate) => candidate.name === toolName);
		return tool ? { name: tool.name, sourceInfo: tool.sourceInfo } : undefined;
	} catch {
		return undefined;
	}
}

export default function smeltPermissionExtension(pi: ExtensionAPI): void {
	pi.on("tool_call", async (event, context) => {
		const tool = resolveToolIdentity(pi, event.toolName);
		if (isReservedSmeltToolConflict(event.toolName, tool)) {
			return {
				block: true,
				reason: `工具名 ${event.toolName} 属于 Smelt 保留命名空间，但实际来源不是 Smelt`,
			};
		}
		if (!toolNeedsSmeltApproval(tool, event.input)) return;

		const approved = await context.ui.confirm(
			SMELT_PERMISSION_TITLE,
			JSON.stringify({
				version: 1,
				toolCallId: event.toolCallId,
				toolName: event.toolName,
				input: event.input,
			}),
		);
		if (!approved) {
			return { block: true, reason: "用户未批准该工具调用" };
		}
	});
}
