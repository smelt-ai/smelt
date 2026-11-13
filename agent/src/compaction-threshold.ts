import { SettingsManager } from "@earendil-works/pi-coding-agent";

/**
 * 自动压缩的预留比例：上下文用到窗口的 85% 就压缩。
 *
 * Pi 只预留固定的 `reserveTokens`（默认 16384），256K 窗口要到 93.6% 才压缩，
 * 一个大工具结果就能一步越过阈值直接超窗，只能走溢出恢复。业界做法都按比例
 * 提前压缩（Codex 90%、OpenCode 窗口 − max(15%, 16K)、Cline 80%）。
 */
export const COMPACTION_RESERVE_RATIO = 0.15;

/** 用户配置的预留量与按窗口比例算出的预留量取较大者。 */
export function compactionReserveTokens(configured: number, contextWindow: number | undefined): number {
	if (!contextWindow || contextWindow <= 0) return configured;
	return Math.max(configured, Math.floor(contextWindow * COMPACTION_RESERVE_RATIO));
}

let installed = false;

/**
 * Pi 没有按比例预留的配置项，只能在 Smelt 的入口里改写读取预留量的那个方法。
 * Pi 判断是否压缩（`shouldCompact`）时传入的是完整模型，能拿到 `contextWindow`。
 */
export function installCompactionThreshold(): void {
	if (installed) return;
	installed = true;
	const proto = SettingsManager.prototype;
	const original = proto.getCompactionReserveTokens;
	proto.getCompactionReserveTokens = function (model) {
		const configured = original.call(this, model);
		return compactionReserveTokens(configured, (model as { contextWindow?: number } | undefined)?.contextWindow);
	};
}
