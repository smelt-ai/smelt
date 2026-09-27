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

/**
 * 执行结构化 Handoff 压缩。
 * 如果缺少模型或凭据，返回 undefined 自动回退到 Pi 默认压缩实现。
 */
export async function runHandoffCompaction(
	preparation: CompactionPreparation,
	ctx: ExtensionContext,
	customInstructions?: string,
	signal?: AbortSignal,
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
		return await compact(
			preparation,
			ctx.model,
			auth.apiKey,
			auth.headers as Record<string, string> | undefined,
			instructions,
			signal ?? ctx.signal,
			ctx.thinkingLevel,
		);
	} catch {
		// 任何异常回退到 Pi 原生默认流程，确保鲁棒性
		return undefined;
	}
}
