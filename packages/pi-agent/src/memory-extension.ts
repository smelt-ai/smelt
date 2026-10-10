import { existsSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import type { ExtensionAPI, SourceInfo } from "@earendil-works/pi-coding-agent";
import { currentMemoryRoots } from "./agent-memory.ts";
import {
	checkCoreBudget,
	type MemoryLayer,
	type MemoryRoots,
	type MemorySnapshot,
	memorySignature,
	recallRelevantFacts,
	renderMemorySnapshot,
	resolveMemoryTarget,
	validateMemoryContent,
} from "./memory-files.ts";
import { readMemoryUsage, recordMemoryRead } from "./memory-usage.ts";

export const SMELT_MEMORY_EXTENSION_NAME = "smelt-memory";
/** Pi 结构化系统提示里的分节名，渲染为 `<smelt_memory>…</smelt_memory>`。 */
export const SMELT_MEMORY_PROMPT_SECTION = "smelt_memory";
/** 第一轮按问题召回的记忆，作为隐藏的自定义消息跟在用户消息后面。 */
export const SMELT_MEMORY_RECALL_MESSAGE = "smelt-memory-recall";

/** 已通过记忆校验的调用；smelt-permission 据此免审批，取一次即作废。 */
const approvedMemoryWrites = new Set<string>();

export function consumeApprovedMemoryWrite(toolCallId: string): boolean {
	return approvedMemoryWrites.delete(toolCallId);
}

type PendingEdit = {
	layer: MemoryLayer;
	root: string;
	fileName: string;
	absolutePath: string;
	/** undefined 表示编辑前文件不存在。 */
	previous: string | undefined;
};

function isActualBuiltin(name: string, sourceInfo: SourceInfo | undefined): boolean {
	return sourceInfo?.source === "builtin" && sourceInfo.path === `builtin:${name}`;
}

function errorResult(text: string) {
	return { content: [{ type: "text" as const, text }], isError: true };
}

function validate(layer: MemoryLayer, root: string, fileName: string, content: string): string | undefined {
	return validateMemoryContent(layer, fileName, content) ?? checkCoreBudget(root, fileName, content);
}

/**
 * 智能体记忆：注入与写入校验。
 *
 * 注入：记忆段落放进 Pi 结构化系统提示的 `smelt_memory` 分节。Pi 把每轮的分节和对话记录里
 * 已有的比对：没变就什么都不发，缓存前缀照常命中；变了只在对话末尾追加一条系统消息，
 * 替换这一个分节，前面的缓存不受影响。所以每轮都按当前文件渲染即可——别的会话、自动化或
 * 用户在界面上改了记忆，下一轮就能看到，代价只是这一分节的长度。压缩时 Pi 会把最新分节
 * 折叠进新的开头；恢复旧会话时，新进程渲染出的分节同样以追加方式更新。
 * 不能用 before_agent_start 返回 systemPrompt：那会整段替换请求开头，一变就整段缓存失效。
 *
 * 召回：会话开始和每次压缩后的第一轮，按用户的问题挑出最相关的几条事实，把正文作为隐藏
 * 消息附在这一轮末尾，省一次 read。只追加，不碰系统提示。
 *
 * 统计：模型 read 记忆文件时记一次使用（见 memory-usage.ts），用于事实索引超限时决定留哪些，
 * 以及在界面上标出长期没用过的记忆。
 *
 * 写入：只管 registry 证明身份的内建 write / edit 落在记忆目录里的调用。write 写前校验完整
 * 内容；edit 的匹配规则（含模糊匹配）只有 Pi 自己知道，所以写后读回实际结果校验，不合规就
 * 恢复原样并把这次调用改成失败。注册在 smelt-permission 之前：校验在任何权限模式下都生效。
 */
export default function smeltMemoryExtension(pi: ExtensionAPI): void {
	const pendingEdits = new Map<string, PendingEdit>();
	/**
	 * 正在写的记忆文件 → 发起的调用。并行批次里所有 tool_call 都先于执行触发，同一文件的
	 * 第二次写入看到的"原内容"已经过时，回滚会覆盖或删掉前一次的结果，所以直接拒绝。
	 */
	const inFlight = new Map<string, string>();
	let cached: { key: string; snapshot: MemorySnapshot } | undefined;
	let warnedTruncation = false;
	let recallPending = false;

	function isBuiltinTool(name: string): boolean {
		try {
			return isActualBuiltin(name, pi.getAllTools().find((candidate) => candidate.name === name)?.sourceInfo);
		} catch {
			return false;
		}
	}

	function snapshotFor(roots: MemoryRoots, cwd: string): MemorySnapshot {
		const key = `${cwd}\n${memorySignature(roots)}`;
		// 使用统计只在文件变化、需要重新渲染时才读：读一次记忆不该让段落跟着变。
		if (cached?.key !== key) {
			cached = { key, snapshot: renderMemorySnapshot(roots, cwd, readMemoryUsage(roots.agent)) };
		}
		return cached.snapshot;
	}

	function warnIfTruncated(snapshot: MemorySnapshot, notify: (message: string, type: "warning") => void) {
		if (!snapshot.truncated) {
			warnedTruncation = false;
			return;
		}
		if (warnedTruncation) return;
		warnedTruncation = true;
		notify("记忆超出注入上限，部分未加载。请在智能体的记忆卡片里删掉过时的条目。", "warning");
	}

	pi.on("session_start", async (_event, context) => {
		const roots = currentMemoryRoots();
		if (!roots) return;
		recallPending = true;
		warnIfTruncated(snapshotFor(roots, context.cwd), (message, type) => context.ui.notify(message, type));
	});

	pi.on("before_agent_start", async (event, context) => {
		const roots = currentMemoryRoots();
		if (!roots) return;
		const snapshot = snapshotFor(roots, context.cwd);
		event.systemPromptOptions.sections ??= {};
		event.systemPromptOptions.sections[SMELT_MEMORY_PROMPT_SECTION] = snapshot.text;
		warnIfTruncated(snapshot, (message, type) => context.ui.notify(message, type));
		if (!recallPending) return;
		recallPending = false;
		const recalled = recallRelevantFacts(roots, context.cwd, event.prompt);
		if (!recalled) return;
		return { message: { customType: SMELT_MEMORY_RECALL_MESSAGE, content: recalled, display: false } };
	});

	// 压缩会把之前召回的正文压掉，下一轮重新按问题召回。
	pi.on("session_compact", async () => {
		if (currentMemoryRoots()) recallPending = true;
	});

	pi.on("tool_call", async (event, context) => {
		const roots = currentMemoryRoots();
		if (!roots || (event.toolName !== "write" && event.toolName !== "edit")) return;
		if (!isBuiltinTool(event.toolName)) return;
		const input = event.input as { path?: unknown; content?: unknown };
		if (typeof input.path !== "string") return;

		const target = resolveMemoryTarget(input.path, context.cwd, roots);
		if (target.kind === "outside") return;
		if (target.kind === "invalid") return { block: true, reason: target.reason };
		if (inFlight.has(target.absolutePath)) {
			return {
				block: true,
				reason: "这个记忆文件已有一次写入正在进行，请等上一步完成后再改，不要并行写同一个记忆文件",
			};
		}

		if (event.toolName === "write") {
			const content = typeof input.content === "string" ? input.content : "";
			const problem = validate(target.layer, target.root, target.fileName, content);
			if (problem) return { block: true, reason: problem };
		} else {
			let previous: string | undefined;
			try {
				previous = existsSync(target.absolutePath) ? readFileSync(target.absolutePath, "utf8") : undefined;
			} catch {
				return { block: true, reason: "读不了这个记忆文件，无法安全编辑" };
			}
			pendingEdits.set(event.toolCallId, {
				layer: target.layer,
				root: target.root,
				fileName: target.fileName,
				absolutePath: target.absolutePath,
				previous,
			});
		}
		inFlight.set(target.absolutePath, event.toolCallId);
		approvedMemoryWrites.add(event.toolCallId);
	});

	// 被后续处理拦下或中途取消的调用可能没有 tool_result；一轮结束时清掉，免得永远占着。
	pi.on("agent_end", async () => {
		inFlight.clear();
	});

	pi.on("tool_result", async (event, context) => {
		for (const [path, toolCallId] of inFlight) {
			if (toolCallId === event.toolCallId) inFlight.delete(path);
		}
		if (event.toolName === "read") {
			const roots = currentMemoryRoots();
			const path = (event.input as { path?: unknown }).path;
			if (!roots || event.isError || typeof path !== "string" || !isBuiltinTool("read")) return;
			const target = resolveMemoryTarget(path, context.cwd, roots);
			if (target.kind === "memory") recordMemoryRead(target.root, target.fileName);
			return;
		}
		const pending = pendingEdits.get(event.toolCallId);
		if (!pending) return;
		pendingEdits.delete(event.toolCallId);
		if (event.isError) return;
		let current: string;
		try {
			current = readFileSync(pending.absolutePath, "utf8");
		} catch {
			return;
		}
		const problem = validate(pending.layer, pending.root, pending.fileName, current);
		if (!problem) return;
		try {
			if (pending.previous === undefined) unlinkSync(pending.absolutePath);
			else writeFileSync(pending.absolutePath, pending.previous);
		} catch {
			return errorResult(`记忆校验未通过，且恢复原文件失败，请手动检查 ${pending.absolutePath}：${problem}`);
		}
		return errorResult(`记忆校验未通过，这次编辑已撤销：${problem}`);
	});
}
