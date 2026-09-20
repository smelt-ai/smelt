import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { isElicitationInput } from "./elicitation.ts";

/** Rust 端据此识别 Smelt 自己的权限请求，不会误吞用户 Extension 的 confirm。 */
export const SMELT_PERMISSION_TITLE = "smelt.permission.v1";

const SAFE_TOOLS = new Set(["read", "grep", "find", "ls"]);

/** 内置有副作用的工具即使参数碰巧像表单，也走审批，不能当成选择题放行。 */
const MUTATING_BUILTIN_TOOLS = new Set(["bash", "powershell", "write", "edit"]);

export function toolNeedsSmeltApproval(toolName: string, input?: unknown): boolean {
	if (SAFE_TOOLS.has(toolName)) {
		return false;
	}
	if (MUTATING_BUILTIN_TOOLS.has(toolName)) {
		return true;
	}
	// 选择题由宿主自带（也可能是用户同名工具）。参数像选择题就放行，避免审批框挡住选择卡。
	return !isElicitationInput(input);
}

export default function smeltPermissionExtension(pi: ExtensionAPI): void {
	pi.on("tool_call", async (event, context) => {
		if (!toolNeedsSmeltApproval(event.toolName, event.input)) return;

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
