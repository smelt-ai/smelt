import type { AgentMessage } from "@earendil-works/pi-agent-core";

/**
 * 粗略估算文本 Token 数（4 个字符约等于 1 个 token）。
 */
export function estimateTokens(text: string): number {
	if (!text) return 0;
	return Math.ceil(text.length / 4);
}

export type ShakeOptions = {
	/** 是否启用无损剪枝，默认 true */
	enabled?: boolean;
	/** 最大命令日志行数，超过则折叠中间日志，默认 30 行 */
	maxCommandLines?: number;
	/** 最大命令日志字符数，默认 1500 字符 */
	maxCommandChars?: number;
	/** 保护最近几个轮次不被剪枝，默认 1（即当前轮次与最新一轮完全保护） */
	preserveRecentTurns?: number;
	/** 是否剪枝历史 thinking 思考链块，默认 true */
	shakeThinking?: boolean;
};

export type ShakeResult = {
	messages: AgentMessage[];
	shakenCount: number;
	savedTokens: number;
	prunedToolCalls: string[];
	prunedThinkingCount: number;
};

export const READ_TOOLS = new Set([
	"read",
	"view_file",
	"read_file",
	"read_url_content",
	"view_image",
]);

export const WRITE_TOOLS = new Set([
	"write",
	"edit",
	"replace_file_content",
	"write_to_file",
	"append_to_file",
]);

export const COMMAND_TOOLS = new Set([
	"bash",
	"powershell",
	"exec",
	"run_command",
]);

export const SEARCH_TOOLS = new Set([
	"grep",
	"find",
	"ls",
	"search_files",
	"find_files",
	"search_code",
	"search_web",
]);

export function extractFilePath(args: unknown): string | undefined {
	if (!args || typeof args !== "object") return undefined;
	const obj = args as Record<string, unknown>;
	for (const key of ["path", "filePath", "AbsolutePath", "file", "TargetFile"]) {
		const val = obj[key];
		if (typeof val === "string" && val.trim().length > 0) {
			return val.trim();
		}
	}
	return undefined;
}

export function extractSearchQuery(args: unknown): string | undefined {
	if (!args || typeof args !== "object") return undefined;
	const obj = args as Record<string, unknown>;
	for (const key of ["query", "pattern", "path"]) {
		const val = obj[key];
		if (typeof val === "string" && val.trim().length > 0) {
			return val.trim();
		}
	}
	return undefined;
}

export function isSearchEmpty(text: string): boolean {
	const trimmed = text.trim();
	if (trimmed.length === 0) return true;
	if (trimmed === "[]" || trimmed === "{}") return true;
	if (
		/^(?:no (?:matches?|files?|results?|occurrences?|entries)|0 matches?|0 files?|0 results?)/i.test(
			trimmed,
		)
	) {
		return true;
	}
	return false;
}

export function isAlreadyShaken(text: string): boolean {
	return (
		text.startsWith("[Stale read of ") ||
		text.startsWith("[Superseded read of ") ||
		text.startsWith("[Superseded edit of ") ||
		text.startsWith("[Empty search for ") ||
		text.startsWith("[Historical thinking process ") ||
		text.includes("elided by shake")
	);
}

export function getTextFromToolContent(content: unknown): string {
	if (typeof content === "string") return content;
	if (Array.isArray(content)) {
		return content
			.map((part) => {
				if (typeof part === "string") return part;
				if (
					part &&
					typeof part === "object" &&
					"type" in part &&
					part.type === "text" &&
					typeof (part as { text?: unknown }).text === "string"
				) {
					return (part as { text: string }).text;
				}
				return "";
			})
			.join("\n");
	}
	return "";
}

export function replaceTextInToolContent(content: unknown, newText: string): unknown {
	if (typeof content === "string") return newText;
	if (Array.isArray(content)) {
		const nonText = content.filter(
			(part) =>
				part &&
				typeof part === "object" &&
				(part as { type?: unknown }).type !== "text",
		);
		return [{ type: "text", text: newText }, ...nonText];
	}
	return [{ type: "text", text: newText }];
}

export function truncateBulkyCommandOutput(
	text: string,
	isError: boolean,
	maxLines = 30,
	maxChars = 1500,
): { text: string; truncated: boolean; omittedLines: number } {
	if (isAlreadyShaken(text)) {
		return { text, truncated: false, omittedLines: 0 };
	}
	const lines = text.split("\n");
	if (lines.length <= maxLines && text.length <= maxChars) {
		return { text, truncated: false, omittedLines: 0 };
	}

	const headLinesCount = 5;
	const tailLinesCount = isError ? 20 : 10;
	if (lines.length <= headLinesCount + tailLinesCount) {
		return { text, truncated: false, omittedLines: 0 };
	}

	const head = lines.slice(0, headLinesCount);
	const tail = lines.slice(lines.length - tailLinesCount);
	const omittedLines = lines.length - headLinesCount - tailLinesCount;

	const statusNote = isError ? "command failed" : "exit code 0";
	const placeholder = `\n... [${omittedLines} lines of command output elided by shake; ${statusNote}] ...\n`;

	const truncatedText = [...head, placeholder, ...tail].join("\n");
	return { text: truncatedText, truncated: true, omittedLines };
}

type ToolCallMeta = {
	toolCallId: string;
	toolName: string;
	args: unknown;
	messageIndex: number;
	turnIndex: number;
};

type FileOp = {
	type: "read" | "write";
	path: string;
	toolCallId: string;
	messageIndex: number;
	turnIndex: number;
};

/**
 * 执行纯规则无损上下文剪枝 (Shake)
 *
 * 识别历史中过期的文件读取、已被覆盖的旧改动、空搜索结果以及过长命令输出，
 * 将其缩略为极小的确定性占位符，0 Token 成本降低上下文体积。
 */
export function shakeMessages(
	messages: AgentMessage[],
	options?: ShakeOptions,
): ShakeResult {
	if (options?.enabled === false || !messages || messages.length === 0) {
		return {
			messages,
			shakenCount: 0,
			savedTokens: 0,
			prunedToolCalls: [],
			prunedThinkingCount: 0,
		};
	}

	const maxCommandLines = options?.maxCommandLines ?? 30;
	const maxCommandChars = options?.maxCommandChars ?? 1500;
	const preserveRecentTurns = options?.preserveRecentTurns ?? 1;
	const shakeThinking = options?.shakeThinking ?? true;

	// 1. 划分轮次（User 消息触发新轮次）
	let currentTurn = 1;
	const turnMap = new Map<number, number>();
	for (let i = 0; i < messages.length; i++) {
		const msg = messages[i] as { role?: string };
		if (msg && msg.role === "user" && i > 0) {
			currentTurn++;
		}
		turnMap.set(i, currentTurn);
	}
	const maxTurn = currentTurn;
	const pruneTurnCutoff = maxTurn - preserveRecentTurns;

	// 2. 扫描所有 Assistant 消息，提取 toolCall 与文件操作时间线
	const toolCallsMap = new Map<string, ToolCallMeta>();
	const fileOpsByPath = new Map<string, FileOp[]>();

	for (let i = 0; i < messages.length; i++) {
		const msg = messages[i] as { role?: string; content?: unknown };
		if (msg && msg.role === "assistant" && Array.isArray(msg.content)) {
			const turn = turnMap.get(i) ?? 1;
			for (const block of msg.content) {
				if (
					block &&
					typeof block === "object" &&
					(block as { type?: string }).type === "toolCall"
				) {
					const toolCall = block as {
						id?: string;
						name?: string;
						arguments?: unknown;
						input?: unknown;
					};
					const id = toolCall.id;
					const name = toolCall.name ?? "";
					const args = toolCall.arguments ?? toolCall.input;
					if (id) {
						toolCallsMap.set(id, {
							toolCallId: id,
							toolName: name,
							args,
							messageIndex: i,
							turnIndex: turn,
						});

						const path = extractFilePath(args);
						if (path) {
							const ops = fileOpsByPath.get(path) ?? [];
							if (READ_TOOLS.has(name)) {
								ops.push({
									type: "read",
									path,
									toolCallId: id,
									messageIndex: i,
									turnIndex: turn,
								});
								fileOpsByPath.set(path, ops);
							} else if (WRITE_TOOLS.has(name)) {
								ops.push({
									type: "write",
									path,
									toolCallId: id,
									messageIndex: i,
									turnIndex: turn,
								});
								fileOpsByPath.set(path, ops);
							}
						}
					}
				}
			}
		}
	}

	// 3. 扫描所有历史消息，根据规则进行无损修剪（ToolResult 与 thinking 块）
	let modified = false;
	const resultMessages = [...messages];
	let shakenCount = 0;
	let savedTokens = 0;
	let prunedThinkingCount = 0;
	const prunedToolCalls: string[] = [];

	for (let i = 0; i < messages.length; i++) {
		const msg = messages[i] as {
			role?: string;
			content?: unknown;
			toolCallId?: string;
			toolName?: string;
			isError?: boolean;
		};

		if (!msg) {
			continue;
		}

		const turn = turnMap.get(i) ?? 1;
		// 处于受保护的最近轮次，绝不剪枝
		if (turn > pruneTurnCutoff) {
			continue;
		}

		// Rule T: 历史 thinking 思考链剪枝
		if (shakeThinking && msg.role === "assistant" && Array.isArray(msg.content)) {
			let assistantModified = false;
			const newContent = msg.content.map((block) => {
				if (
					block &&
					typeof block === "object" &&
					(block as { type?: unknown }).type === "thinking" &&
					typeof (block as { thinking?: unknown }).thinking === "string"
				) {
					const thinkingStr = (block as { thinking: string }).thinking;
					if (thinkingStr && !isAlreadyShaken(thinkingStr)) {
						const newThinking = `[Historical thinking process (${thinkingStr.length} chars) elided by shake]`;
						const tokensBefore = estimateTokens(thinkingStr);
						const tokensAfter = estimateTokens(newThinking);
						savedTokens += Math.max(0, tokensBefore - tokensAfter);
						shakenCount++;
						prunedThinkingCount++;
						assistantModified = true;
						return {
							...block,
							thinking: newThinking,
						};
					}
				}
				return block;
			});

			if (assistantModified) {
				resultMessages[i] = {
					...msg,
					content: newContent,
				} as AgentMessage;
				modified = true;
			}
		}

		if (msg.role !== "toolResult" || !msg.toolCallId) {
			continue;
		}

		const toolMeta = toolCallsMap.get(msg.toolCallId);
		const toolName = toolMeta?.toolName || msg.toolName || "";
		const args = toolMeta?.args;
		const path = extractFilePath(args);
		const originalText = getTextFromToolContent(msg.content);

		if (!originalText || isAlreadyShaken(originalText)) {
			continue;
		}

		let newText: string | undefined;

		// Rule A & B: 只读文件剪枝
		if (path && READ_TOOLS.has(toolName)) {
			const ops = fileOpsByPath.get(path) ?? [];
			// 检查后续是否有针对该文件的写入操作
			const laterWrite = ops.find(
				(op) => op.type === "write" && op.messageIndex > i,
			);
			if (laterWrite) {
				newText = `[Stale read of "${path}" elided by shake; file was subsequently modified]`;
			} else {
				// 检查后续是否有针对该文件的再次读取
				const laterRead = ops.find(
					(op) => op.type === "read" && op.messageIndex > i,
				);
				if (laterRead) {
					newText = `[Superseded read of "${path}" elided by shake; file was re-read later]`;
				}
			}
		}

		// Rule C: 被后续编辑覆盖的中间改动
		if (!newText && path && WRITE_TOOLS.has(toolName)) {
			const ops = fileOpsByPath.get(path) ?? [];
			const laterWrite = ops.find(
				(op) => op.type === "write" && op.messageIndex > i,
			);
			if (laterWrite) {
				newText = `[Superseded edit of "${path}" elided by shake; newer edit applied later]`;
			}
		}

		// Rule D: 空搜索输出
		if (!newText && SEARCH_TOOLS.has(toolName)) {
			if (isSearchEmpty(originalText)) {
				const query = extractSearchQuery(args) || path || "query";
				newText = `[Empty search for "${query}" elided by shake]`;
			}
		}

		// Rule E: 冗长命令输出折叠
		if (!newText && COMMAND_TOOLS.has(toolName)) {
			const isError = Boolean(msg.isError);
			const trunc = truncateBulkyCommandOutput(
				originalText,
				isError,
				maxCommandLines,
				maxCommandChars,
			);
			if (trunc.truncated) {
				newText = trunc.text;
			}
		}

		// 如果产生了剪枝改动，替换消息内容
		if (newText !== undefined && newText !== originalText) {
			const tokensBefore = estimateTokens(originalText);
			const tokensAfter = estimateTokens(newText);
			savedTokens += Math.max(0, tokensBefore - tokensAfter);
			shakenCount++;
			prunedToolCalls.push(msg.toolCallId);

			resultMessages[i] = {
				...msg,
				content: replaceTextInToolContent(msg.content, newText),
			} as AgentMessage;
			modified = true;
		}
	}

	return {
		messages: modified ? resultMessages : messages,
		shakenCount,
		savedTokens,
		prunedToolCalls,
		prunedThinkingCount,
	};
}
