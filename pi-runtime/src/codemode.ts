import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

export const SMELT_CODEMODE_COMMAND = "smelt-codemode";
export const SMELT_CODEMODE_WIDGET = "smelt-codemode";
export const CODEMODE_TOOL = "codemode";

/** 打开时把 codemode 加进当前活动工具，关掉时只拿掉它，不改其它工具。 */
export function nextActiveTools(
	active: readonly string[],
	known: readonly string[],
	enabled: boolean,
): string[] {
	if (enabled && !known.includes(CODEMODE_TOOL)) {
		throw new Error("当前 Pi 没有 codemode 工具");
	}
	if (!enabled) {
		return active.filter((name) => name !== CODEMODE_TOOL);
	}
	return active.includes(CODEMODE_TOOL) ? [...active] : [...active, CODEMODE_TOOL];
}

function report(pi: ExtensionAPI, ctx: ExtensionContext): void {
	try {
		ctx.ui.setWidget(SMELT_CODEMODE_WIDGET, [
			JSON.stringify({ enabled: pi.getActiveTools().includes(CODEMODE_TOOL) }),
		]);
	} catch {
		// RPC 宿主忽略 widget 时不能影响对话。
	}
}

function applyCodemode(pi: ExtensionAPI, enabled: boolean): void {
	const known = pi.getAllTools().map((tool) => tool.name);
	pi.setActiveTools(nextActiveTools(pi.getActiveTools(), known, enabled));
}

export default function smeltCodemodeExtension(pi: ExtensionAPI): void {
	pi.on("session_start", (_event, ctx) => {
		report(pi, ctx);
	});
	pi.registerCommand(SMELT_CODEMODE_COMMAND, {
		description: "Turn codemode on or off for the next turn",
		handler: async (args, ctx) => {
			const value = args.trim();
			if (value !== "on" && value !== "off") {
				throw new Error("用法：/smelt-codemode on|off");
			}
			try {
				applyCodemode(pi, value === "on");
			} finally {
				report(pi, ctx);
			}
		},
	});
}
