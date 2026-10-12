import { existsSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import type { ExtensionAPI, SourceInfo } from "@earendil-works/pi-coding-agent";
import { currentMemoryRoots, memoryRunIsUnattended } from "./agent-memory.ts";
import { SMELT_BACKGROUND_TASK_EXTENSION_PATH } from "./background-task.ts";
import { SMELT_ELICITATION_EXTENSION_PATH } from "./elicitation.ts";
import {
	checkCoreBudget,
	type MemoryLayer,
	type MemoryRoots,
	type MemorySnapshot,
	memorySignature,
	recallRelevantFacts,
	renderMemorySnapshot,
	resolveMemoryTarget,
	scanMemoryLayer,
	validateMemoryContent,
} from "./memory-files.ts";
import {
	fetchMemoryMeta,
	type MemoryHostMeta,
	type MemoryProvenance,
	recordMemoryRead,
	recordMemoryWrite,
} from "./memory-host.ts";

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

type InFlightWrite = {
	toolName: "write" | "edit";
	layer: MemoryLayer;
	root: string;
	fileName: string;
	absolutePath: string;
	/** undefined 表示写入前文件不存在。校验或来源记录失败时据此恢复。 */
	previous: string | undefined;
	provenance: MemoryProvenance;
};

function isActualBuiltin(name: string, sourceInfo: SourceInfo | undefined): boolean {
	return sourceInfo?.source === "builtin" && sourceInfo.path === `builtin:${name}`;
}

/**
 * 不会把外部内容带进上下文的工具：Pi 内建工具（bash 也算，见 docs/agent-memory.md 5.4 的取舍）、
 * Smelt 的选择题（答案来自用户）和后台任务（同 bash）。其余——MCP、用户扩展的工具、
 * 子智能体——的结果都当作外部内容。
 */
function isTrustedTool(name: string, sourceInfo: SourceInfo | undefined): boolean {
	if (isActualBuiltin(name, sourceInfo)) return true;
	return (
		sourceInfo?.source === "inline" &&
		(sourceInfo.path === SMELT_ELICITATION_EXTENSION_PATH || sourceInfo.path === SMELT_BACKGROUND_TASK_EXTENSION_PATH)
	);
}

/** 把两层所有记忆都当作待确认：渲染出只有写入约定、没有任何记忆的段落。 */
function everything(roots: MemoryRoots): { agent: Set<string>; user: Set<string> } {
	const all = (dir: string) => new Set(scanMemoryLayer(dir).map((entry) => entry.fileName));
	return { agent: all(roots.agent), user: all(roots.user) };
}

function restore(write: InFlightWrite): boolean {
	try {
		if (write.previous === undefined) unlinkSync(write.absolutePath);
		else writeFileSync(write.absolutePath, write.previous);
		return true;
	} catch {
		return false;
	}
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
 * 统计：模型 read 记忆文件时记一次使用，用于事实索引超限时决定留哪些，以及在界面上标出
 * 长期没用过的记忆。统计和来源都在 Smelt 主库，由主 daemon 代办（见 memory-host.ts）。
 *
 * 来源：按轮次标记。这一轮里有工具把外部内容带进了上下文，或者是无人值守的运行，这时写的
 * 记忆记为待确认：不注入、不召回，等用户在记忆卡片里采纳。来源记在主库，不在正文里。
 * daemon 不可达、拿不到待确认列表时按"不知道"处理：只注入写入约定，不注入任何记忆、不召回。
 * 用户发下一条消息时清除标记——用户新发的话才是权威。
 *
 * 写入：只管 registry 证明身份的内建 write / edit 落在记忆目录里的调用。write 写前校验完整
 * 内容；edit 的匹配规则（含模糊匹配）只有 Pi 自己知道，所以写后读回实际结果校验，不合规就
 * 恢复原样并把这次调用改成失败。注册在 smelt-permission 之前：校验在任何权限模式下都生效。
 */
export default function smeltMemoryExtension(pi: ExtensionAPI): void {
	/**
	 * 正在写的记忆文件 → 这次写入。并行批次里所有 tool_call 都先于执行触发，同一文件的
	 * 第二次写入看到的"原内容"已经过时，回滚会覆盖或删掉前一次的结果，所以直接拒绝。
	 */
	const inFlight = new Map<string, InFlightWrite & { toolCallId: string }>();
	/** 本轮把外部内容带进上下文的工具。 */
	const turnTaint = new Set<string>();
	const turnWrites = { trusted: 0, pending: 0 };
	let cached: { key: string; snapshot: MemorySnapshot } | undefined;
	/** 最近一次从宿主拿到的元数据；拿不到时沿用它，从没拿到过就不注入任何记忆。 */
	let lastMeta: MemoryHostMeta | undefined;
	let warnedTruncation = false;
	let recallPending = false;

	function sourceInfoOf(name: string): SourceInfo | undefined {
		try {
			return pi.getAllTools().find((candidate) => candidate.name === name)?.sourceInfo;
		} catch {
			return undefined;
		}
	}

	async function currentMeta(): Promise<MemoryHostMeta | undefined> {
		lastMeta = (await fetchMemoryMeta()) ?? lastMeta;
		return lastMeta;
	}

	function snapshotFor(roots: MemoryRoots, cwd: string, meta: MemoryHostMeta | undefined): MemorySnapshot {
		const key = JSON.stringify([
			cwd,
			memorySignature(roots),
			meta ? [[...meta.pending.agent], [...meta.pending.user], [...meta.usage]] : null,
		]);
		if (cached?.key !== key) {
			// 不知道哪些待确认，就一条都不注入：宁可少给，不能把待确认的当可信。
			// 写入约定照常给出，模型仍能写（写进来的照样走来源判断）。
			const pending = meta?.pending ?? everything(roots);
			cached = { key, snapshot: renderMemorySnapshot(roots, cwd, { usage: meta?.usage, pending }) };
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

	async function provenanceFor(
		layer: MemoryLayer,
		fileName: string,
		toolName: "write" | "edit",
	): Promise<MemoryProvenance> {
		if (memoryRunIsUnattended()) return { trust: "pending", reason: "无人值守的运行（自动化）写入" };
		if (turnTaint.size > 0) {
			return { trust: "pending", reason: `写入的那一轮读取了外部内容（${[...turnTaint].join("、")}）` };
		}
		if (toolName === "write") return { trust: "trusted", reason: "用户在场时写入" };
		// edit 只改一部分，其余内容仍是待确认时写的，保持待确认；拿不到列表时也按待确认。
		const meta = await currentMeta();
		if (!meta) return { trust: "pending", reason: "无法确认这条记忆原先的来源" };
		return meta.pending[layer].has(fileName)
			? { trust: "pending", reason: "修改的是一条待确认的记忆" }
			: { trust: "trusted", reason: "用户在场时写入" };
	}

	pi.on("session_start", async (_event, context) => {
		const roots = currentMemoryRoots();
		if (!roots) return;
		recallPending = true;
		const meta = await currentMeta();
		if (meta && !meta.enabled) return;
		warnIfTruncated(snapshotFor(roots, context.cwd, meta), (message, type) => context.ui.notify(message, type));
	});

	pi.on("before_agent_start", async (event, context) => {
		const roots = currentMemoryRoots();
		if (!roots) return;
		// 用户发了新消息：新的一轮，外部内容标记清零。
		turnTaint.clear();
		turnWrites.trusted = 0;
		turnWrites.pending = 0;
		const meta = await currentMeta();
		// 用户在配置页关掉了记忆：不给分节。Pi 会在对话末尾追加一条移除它的系统消息。
		if (meta && !meta.enabled) return;
		const snapshot = snapshotFor(roots, context.cwd, meta);
		event.systemPromptOptions.sections ??= {};
		event.systemPromptOptions.sections[SMELT_MEMORY_PROMPT_SECTION] = snapshot.text;
		warnIfTruncated(snapshot, (message, type) => context.ui.notify(message, type));
		if (!recallPending || !meta) return;
		recallPending = false;
		const recalled = recallRelevantFacts(roots, context.cwd, event.prompt, meta.pending.agent);
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
		if (!isActualBuiltin(event.toolName, sourceInfoOf(event.toolName))) return;
		const input = event.input as { path?: unknown; content?: unknown };
		if (typeof input.path !== "string") return;

		const target = resolveMemoryTarget(input.path, context.cwd, roots);
		if (target.kind === "outside") return;
		if (target.kind === "invalid") return { block: true, reason: target.reason };
		const meta = await currentMeta();
		if (meta && !meta.enabled) {
			return { block: true, reason: "这个智能体的记忆已在配置页关闭，不能写记忆" };
		}
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
		}
		let previous: string | undefined;
		try {
			previous = existsSync(target.absolutePath) ? readFileSync(target.absolutePath, "utf8") : undefined;
		} catch {
			return { block: true, reason: "读不了这个记忆文件，无法安全写入" };
		}
		inFlight.set(target.absolutePath, {
			toolCallId: event.toolCallId,
			toolName: event.toolName,
			layer: target.layer,
			root: target.root,
			fileName: target.fileName,
			absolutePath: target.absolutePath,
			previous,
			provenance: await provenanceFor(target.layer, target.fileName, event.toolName),
		});
		approvedMemoryWrites.add(event.toolCallId);
	});

	pi.on("agent_end", async (_event, context) => {
		// 被后续处理拦下或中途取消的调用可能没有 tool_result；一轮结束时清掉，免得永远占着。
		inFlight.clear();
		const { trusted, pending } = turnWrites;
		if (trusted + pending === 0) return;
		const parts = [];
		if (trusted > 0) parts.push(`记住了 ${trusted} 条`);
		if (pending > 0) parts.push(`${pending} 条待确认，在智能体的记忆卡片里采纳后生效`);
		context.ui.notify(`记忆：${parts.join("；")}`, "info");
	});

	pi.on("tool_result", async (event, context) => {
		const roots = currentMemoryRoots();
		if (!roots) return;
		let write: InFlightWrite | undefined;
		for (const [path, entry] of inFlight) {
			if (entry.toolCallId === event.toolCallId) {
				inFlight.delete(path);
				write = entry;
			}
		}
		if (write) {
			return finishMemoryWrite(write, event.isError, event.content);
		}

		const sourceInfo = sourceInfoOf(event.toolName);
		if (!isTrustedTool(event.toolName, sourceInfo)) {
			turnTaint.add(event.toolName);
			return;
		}
		if (event.toolName !== "read" || event.isError) return;
		const path = (event.input as { path?: unknown }).path;
		if (typeof path !== "string") return;
		const target = resolveMemoryTarget(path, context.cwd, roots);
		if (target.kind === "memory") await recordMemoryRead(target.layer, target.fileName);
	});

	async function finishMemoryWrite(
		write: InFlightWrite,
		isError: boolean,
		content: unknown[],
	) {
		if (isError) return;
		if (write.toolName === "edit") {
			let current: string;
			try {
				current = readFileSync(write.absolutePath, "utf8");
			} catch {
				return;
			}
			const problem = validate(write.layer, write.root, write.fileName, current);
			if (problem) {
				return restore(write)
					? errorResult(`记忆校验未通过，这次编辑已撤销：${problem}`)
					: errorResult(`记忆校验未通过，且恢复原文件失败，请手动检查 ${write.absolutePath}：${problem}`);
			}
		}
		const recorded = await recordMemoryWrite(write.layer, write.fileName, write.provenance);
		if (write.provenance.trust === "trusted") {
			// 没有来源记录本来就按可信处理，记录失败无妨。
			turnWrites.trusted++;
			return;
		}
		// 待确认却没记下来，就会被当成可信注入：宁可撤销这次写入。
		if (!recorded) {
			return restore(write)
				? errorResult("记忆来源记录失败，为安全起见这次写入已撤销")
				: errorResult(`记忆来源记录失败，且恢复原文件失败，请手动检查 ${write.absolutePath}`);
		}
		turnWrites.pending++;
		return {
			content: [
				...(content as Array<{ type: "text"; text: string }>),
				{
					type: "text" as const,
					text: `这条记忆已暂存为待确认（${write.provenance.reason}），不会生效，直到用户在智能体的记忆卡片里采纳。请在回复里告诉用户。`,
				},
			],
		};
	}
}
