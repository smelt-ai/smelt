import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { isContextOverflow, type AssistantMessage } from "@earendil-works/pi-ai";
import {
	compact,
	type ExtensionContext,
	type SessionBeforeCompactEvent,
	type SessionBeforeCompactResult,
} from "@earendil-works/pi-coding-agent";

export type CompactionPreparation = SessionBeforeCompactEvent["preparation"];
export type CompactionResult = NonNullable<SessionBeforeCompactResult["compaction"]>;

export const DEFAULT_HANDOFF_INSTRUCTIONS = `The conversation history before this point is being compacted.
Create a structured, high-density task handoff summary that another LLM or successor session can immediately use to continue the task without ambiguity.

Format requirements:
- Preserve EXACT file paths, function/class names, shell commands, and error messages.
- Be concise, objective, and action-oriented.
- Follow this exact structure:

## 1. 任务核心目标 (Goal & Scope)
[Brief summary of what the user asked to accomplish and key boundaries]

## 2. 架构决策与排查发现 (Decisions & Key Findings)
- [Key discoveries, architectural constraints, conventions, or verified assumptions]

## 3. 已完成修改与落地成果 (Completed Changes)
- [x] [File path]: [Summary of changes made, functions added/modified]

## 4. 当前进行中状态 (Current Status & Blockers)
- [What was actively being worked on, recent test/compiler results, or blocking issues]

## 5. 明确的下一步行动清单 (Next Steps)
1. [Prioritized concrete action items for the successor to perform immediately]

## 6. 关键上下文参考 (Critical Context & Command References)
- [Commands to run, ports, environment variables, or essential code snippets]`;

/**
 * 组装交接压缩指令，支持用户通过 /compact 附加特定指令
 */
export function buildHandoffInstructions(customInstructions?: string): string {
	if (!customInstructions || customInstructions.trim().length === 0) {
		return DEFAULT_HANDOFF_INSTRUCTIONS;
	}
	return `${DEFAULT_HANDOFF_INSTRUCTIONS}\n\n### 用户附加交接要求 (User Focus):\n${customInstructions.trim()}`;
}

/** 摘要输出上限。预留量已按窗口比例放大，不能再跟着它涨，否则「输入 + max_tokens」更容易超窗。 */
export const SUMMARY_MAX_TOKENS = 16_000;

/** 摘要请求超窗后最多丢几次最老的历史再重试（同 Claude Code）。 */
export const MAX_OVERFLOW_RETRIES = 3;

/** 每次重试丢掉的待摘要消息比例。 */
const DROP_RATIO = 0.2;

export const TRUNCATED_FOR_RETRY_MARKER = "[earlier conversation truncated for compaction retry]";

/**
 * 摘要请求用的模型：输出上限收到 `min(0.8 × reserveTokens, SUMMARY_MAX_TOKENS)`。
 *
 * Pi 的摘要请求不带 max_tokens（`generateSummary` 的 reserveTokens 参数没用上），
 * 于是落到模型自身上限。
 * 压缩在窗口快满时才触发，待摘要的历史已接近整个窗口，大输出模型（如 1M 窗口、
 * 384K 输出）的「输入 + max_tokens」必然超窗，provider 直接 400，压缩永远失败。
 */
export function summarizationModel<M extends { maxTokens: number }>(
	model: M,
	settings: CompactionPreparation["settings"],
): M {
	const budget = Math.min(Math.floor(0.8 * settings.reserveTokens), SUMMARY_MAX_TOKENS);
	if (model.maxTokens > 0 && model.maxTokens <= budget) {
		return model;
	}
	return { ...model, maxTokens: budget };
}

/** Pi 把摘要失败包成 `Summarization failed: <provider 错误>`，用 pi-ai 的同一套规则判断是否超窗。 */
export function isOverflowError(error: unknown): boolean {
	const errorMessage = error instanceof Error ? error.message : String(error);
	return isContextOverflow({ stopReason: "error", errorMessage } as AssistantMessage);
}

function dropOldest(messages: AgentMessage[]): AgentMessage[] | undefined {
	// 开头可能是上一次重试留下的标记，不算历史
	const start = isRetryMarker(messages[0]) ? 1 : 0;
	const rest = messages.slice(start);
	if (rest.length < 2) return undefined;
	let cut = Math.max(1, Math.floor(rest.length * DROP_RATIO));
	// 不让工具结果脱离它的调用单独留下
	while (cut < rest.length && rest[cut]?.role === "toolResult") cut += 1;
	if (cut >= rest.length) return undefined;
	const marker: AgentMessage = { role: "user", content: TRUNCATED_FOR_RETRY_MARKER, timestamp: Date.now() };
	return [marker, ...rest.slice(cut)];
}

function isRetryMarker(message: AgentMessage | undefined): boolean {
	return message?.role === "user" && message.content === TRUNCATED_FOR_RETRY_MARKER;
}

/**
 * 摘要请求超窗时，丢掉最老的一段待摘要历史，换一个更小的 preparation。
 * 先丢 `messagesToSummarize`；那部分丢不动了（拆分轮次时整段都在 `turnPrefixMessages`）再丢后者。
 * 都丢不动返回 undefined。
 */
export function shrinkPreparation(preparation: CompactionPreparation): CompactionPreparation | undefined {
	const history = dropOldest(preparation.messagesToSummarize);
	if (history) return { ...preparation, messagesToSummarize: history };
	if (preparation.isSplitTurn) {
		const prefix = dropOldest(preparation.turnPrefixMessages);
		if (prefix) return { ...preparation, turnPrefixMessages: prefix };
	}
	return undefined;
}

/**
 * 执行结构化 Handoff 压缩。
 * 如果缺少模型或凭据，返回 undefined 自动回退到 Pi 默认压缩实现。
 * 摘要请求本身超窗时，丢最老的历史重试，最多 {@link MAX_OVERFLOW_RETRIES} 次。
 */
export async function runHandoffCompaction(
	preparation: CompactionPreparation,
	ctx: ExtensionContext,
	customInstructions?: string,
	signal?: AbortSignal,
	compactFn: typeof compact = compact,
): Promise<CompactionResult | undefined> {
	if (!ctx.model || !ctx.modelRegistry) {
		return undefined;
	}
	try {
		const auth = await ctx.modelRegistry.getApiKeyAndHeaders(ctx.model);
		if (!auth || !auth.ok) {
			return undefined;
		}
		const instructions = buildHandoffInstructions(customInstructions);
		const model = summarizationModel(ctx.model, preparation.settings);
		let current = preparation;
		for (let attempt = 0; ; attempt += 1) {
			try {
				return await compactFn(
					current,
					model,
					auth.apiKey,
					auth.headers as Record<string, string> | undefined,
					instructions,
					signal ?? ctx.signal,
					ctx.thinkingLevel,
				);
			} catch (error) {
				if (attempt >= MAX_OVERFLOW_RETRIES || !isOverflowError(error)) throw error;
				const smaller = shrinkPreparation(current);
				if (!smaller) throw error;
				current = smaller;
			}
		}
	} catch {
		// 任何异常回退到 Pi 原生默认流程，确保鲁棒性
		return undefined;
	}
}
