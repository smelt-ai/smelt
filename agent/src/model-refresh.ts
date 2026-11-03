import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export const SMELT_REFRESH_MODELS_COMMAND = "smelt-refresh-models";

/**
 * 让这个会话重读 models.json。Pi 的可用模型是会话启动时的快照：之后在 Smelt 里加的 provider，
 * 不重读的话 `get_available_models` 列不出来，`set_model` 也只会回 Model not found。
 * 只走扩展命令，回执是 handled，不开模型回合；回答进行中也立即执行。
 */
export default function smeltModelRefreshExtension(pi: ExtensionAPI): void {
	pi.registerCommand(SMELT_REFRESH_MODELS_COMMAND, {
		description: "Reload models.json for this session",
		handler: async (_args, ctx) => {
			await ctx.modelRegistry.refresh({ allowNetwork: false });
		},
	});
}
