import {
	formatSkillsForPrompt,
	type ExtensionAPI,
	type ExtensionContext,
	type Skill,
	type ToolInfo,
} from "@earendil-works/pi-coding-agent";

export const SMELT_CONTEXT_USAGE_WIDGET = "smelt-context-usage";
export const SMELT_RUNTIME_DEBUG_WIDGET = "smelt-runtime-debug";

export type ContextUsageBuckets = {
	systemPrompt: number;
	toolsDefinition: number;
	rules: number;
	skills: number;
	mcpDynamic: number;
	subagent: number;
	summarized: number;
	conversation: number;
};

export type ContextUsageTool = Pick<ToolInfo, "name" | "description" | "parameters"> & {
	source?: string;
};

export type ContextUsageParts = {
	systemPrompt: string;
	contextFiles: Array<{ path: string; content: string }>;
	skills: Array<Pick<Skill, "name" | "description" | "filePath" | "disableModelInvocation">>;
	tools: ContextUsageTool[];
	messages: Array<{ role?: string; content?: unknown; summarized?: boolean }>;
};

/** Pi 自己估上下文也是 chars/4，各桶必须用同一把尺。 */
export function estimateTokens(text: string): number {
	if (!text) {
		return 0;
	}
	return Math.ceil(text.length / 4);
}

export function formatProjectContext(
	files: Array<{ path: string; content: string }>,
): string {
	if (files.length === 0) {
		return "";
	}
	let block = "\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n";
	for (const file of files) {
		block += `<project_instructions path="${file.path}">\n${file.content}\n</project_instructions>\n\n`;
	}
	block += "</project_context>\n";
	return block;
}

function estimateJson(value: unknown): number {
	if (value === undefined || value === null) {
		return 0;
	}
	if (typeof value === "string") {
		return estimateTokens(value);
	}
	if (Array.isArray(value) && value.length === 0) {
		return 0;
	}
	try {
		return estimateTokens(JSON.stringify(value));
	} catch {
		return 0;
	}
}

function toolTokens(tool: ContextUsageTool): number {
	return estimateJson({
		name: tool.name,
		description: tool.description,
		parameters: tool.parameters,
	});
}

function isSubagentTool(name: string): boolean {
	return name.toLowerCase().includes("subagent");
}

export function buildContextUsageBuckets(parts: ContextUsageParts): ContextUsageBuckets {
	const skillsText = formatSkillsForPrompt(parts.skills as Skill[]);
	const rulesText = formatProjectContext(parts.contextFiles);
	const skills = estimateTokens(skillsText);
	const rules = estimateTokens(rulesText);
	const systemAll = estimateTokens(parts.systemPrompt);
	const systemPrompt = Math.max(0, systemAll - skills - rules);
	let toolsDefinition = 0;
	let mcpDynamic = 0;
	let subagent = 0;
	for (const tool of parts.tools) {
		const tokens = toolTokens(tool);
		if (isSubagentTool(tool.name)) {
			subagent += tokens;
		} else if (tool.source === "builtin") {
			toolsDefinition += tokens;
		} else {
			mcpDynamic += tokens;
		}
	}
	let summarized = 0;
	let conversation = 0;
	for (const message of parts.messages) {
		const tokens = estimateJson(message.content ?? message);
		if (message.summarized) {
			summarized += tokens;
		} else {
			conversation += tokens;
		}
	}
	return {
		systemPrompt,
		toolsDefinition,
		rules,
		skills,
		mcpDynamic,
		subagent,
		summarized,
		conversation,
	};
}

function messagesFromSession(ctx: ExtensionContext): ContextUsageParts["messages"] {
	const messages: ContextUsageParts["messages"] = [];
	for (const entry of ctx.sessionManager.getBranch()) {
		if (entry.type === "message") {
			const message = entry.message as { role?: string; content?: unknown };
			messages.push({ role: message.role, content: message.content ?? message });
			continue;
		}
		if (entry.type === "custom_message") {
			messages.push({ role: "custom", content: entry.content });
			continue;
		}
		if (entry.type === "compaction") {
			messages.push({ summarized: true, content: entry.summary });
			continue;
		}
		if (entry.type === "branch_summary") {
			messages.push({ summarized: true, content: entry.summary });
		}
	}
	return messages;
}

function publish(ctx: ExtensionContext, buckets: ContextUsageBuckets): void {
	try {
		ctx.ui.setWidget(SMELT_CONTEXT_USAGE_WIDGET, [JSON.stringify(buckets)]);
	} catch {
		// RPC 宿主忽略 widget 时不能影响对话。
	}
}

export type RuntimeDebugHeaderCapture = {
	capturedAtMs: number;
	headers: Record<string, string | null>;
	redactedPaths: string[];
};

export type RuntimeDebugResponseMetadata = {
	capturedAtMs: number;
	status: number;
	headers: Record<string, string>;
	redactedPaths: string[];
};

export type RuntimeDebugModelCall = {
	sequence: number;
	turn?: number;
	compactionSequence?: number;
	capturedAtMs: number;
	source: "pi_context_with_system" | "pi_before_provider_request";
	piContext: unknown;
	piContextRedactedPaths: string[];
	model: {
		provider?: string;
		id?: string;
		api?: string;
		thinkingLevel?: string;
	};
	requestConfig: {
		systemPrompt: string;
		tools: ContextUsageTool[];
	};
	requestHeaders: RuntimeDebugHeaderCapture[];
	requestCapturedAtMs?: number;
	payload: unknown;
	payloadSource?: "pi_before_provider_request";
	responseMetadata: RuntimeDebugResponseMetadata[];
	response?: unknown;
	responseCapturedAtMs?: number;
	responseRedactedPaths?: string[];
	redactedPaths: string[];
};

export type RuntimeDebugCompactionMessage = {
	segment: "summarized" | "turn_prefix";
	role: string;
	preview: string;
	truncated: boolean;
};

export type RuntimeDebugCompaction = {
	sequence: number;
	turn?: number;
	status: "started" | "completed" | "failed" | "aborted";
	reason: "manual" | "threshold" | "overflow";
	willRetry: boolean;
	startedAtMs: number;
	finishedAtMs?: number;
	firstKeptEntryId?: string;
	tokensBefore?: number;
	isSplitTurn?: boolean;
	previousSummary?: string;
	summarizedMessageCount?: number;
	turnPrefixMessageCount?: number;
	sourceMessages: RuntimeDebugCompactionMessage[];
	requestHeaders: RuntimeDebugHeaderCapture[];
	responseMetadata: RuntimeDebugResponseMetadata[];
	sourceMessagesOmitted: number;
	summary?: string;
	usage?: unknown;
	fromExtension?: boolean;
	aborted?: boolean;
	errorMessage?: string;
};

export type RuntimeDebugPayload = {
	version: 3;
	source: "pi_runtime_debug";
	systemPrompt: string;
	tools: ContextUsageTool[];
	modelCalls: RuntimeDebugModelCall[];
	modelCallsOmitted: number;
	compactions: RuntimeDebugCompaction[];
	compactionsOmitted: number;
};

function compactContentPreview(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) {
		if (!content || typeof content !== "object") return String(content ?? "");
		const item = content as Record<string, unknown>;
		if (item.type === "image") return "[image omitted]";
		if (item.type === "text" && typeof item.text === "string") return item.text;
		if (item.type === "thinking" && typeof item.thinking === "string") {
			return item.thinking;
		}
		return (
			JSON.stringify(redactProviderPayload(content, "$.content", [], new WeakSet<object>())) ??
			"[unserializable content]"
		);
	}
	return content
		.map((part: unknown) => {
			if (typeof part === "string") return part;
			if (!part || typeof part !== "object") return String(part);
			const item = part as Record<string, unknown>;
			switch (item.type) {
				case "text":
					return typeof item.text === "string" ? item.text : "";
				case "thinking":
					return typeof item.thinking === "string" ? item.thinking : "";
				case "toolCall": {
					const input = redactProviderPayload(
						item.arguments ?? item.input ?? {},
						"$.toolCall",
						[],
						new WeakSet<object>(),
					);
					return `[tool call ${String(item.name ?? "unknown")}] ${JSON.stringify(input)}`;
				}
				case "image":
					return "[image omitted]";
				default:
					return (
						JSON.stringify(redactProviderPayload(item, "$.content", [], new WeakSet<object>())) ??
						"[unserializable content]"
					);
			}
		})
		.join("\n");
}

function previewCompactionMessages(
	messages: unknown[],
	segment: RuntimeDebugCompactionMessage["segment"],
): RuntimeDebugCompactionMessage[] {
	return messages.map((message) => {
		const record = message && typeof message === "object"
			? (message as Record<string, unknown>)
			: {};
		const fullPreview = compactContentPreview(record.content);
		return {
			segment,
			role: typeof record.role === "string" ? record.role : "unknown",
			preview: fullPreview,
			truncated: false,
		};
	});
}

const SENSITIVE_PAYLOAD_KEYS = new Set([
	"authorization",
	"proxyauthorization",
	"auth",
	"headers",
	"apikey",
	"accesstoken",
	"refreshtoken",
	"token",
	"clientsecret",
	"secret",
	"password",
	"passwd",
	"cookie",
	"setcookie",
	"credential",
	"credentials",
]);

function normalizedPayloadKey(key: string): string {
	return key.toLowerCase().replaceAll(/[^a-z0-9]/g, "");
}

function isSensitivePayloadKey(key: string): boolean {
	const normalized = normalizedPayloadKey(key);
	return (
		SENSITIVE_PAYLOAD_KEYS.has(normalized) ||
		[
			"authorization",
			"auth",
			"apikey",
			"token",
			"secret",
			"password",
			"passwd",
			"cookie",
			"credential",
		].some((suffix) => normalized.endsWith(suffix))
	);
}

function redactProviderPayload(
	value: unknown,
	path: string,
	redactedPaths: string[],
	seen: WeakSet<object>,
): unknown {
	if (typeof value === "string") {
		if (/^data:image\//i.test(value)) {
			redactedPaths.push(path);
			return "[IMAGE DATA OMITTED]";
		}
		return value;
	}
	if (value === null || ["number", "boolean"].includes(typeof value)) {
		return value;
	}
	if (typeof value === "bigint") {
		return value.toString();
	}
	if (typeof value !== "object") {
		return `[UNSERIALIZABLE:${typeof value}]`;
	}
	if (seen.has(value)) {
		return "[UNSERIALIZABLE:CIRCULAR]";
	}
	seen.add(value);
	if (Array.isArray(value)) {
		return value.map((item, index) =>
			redactProviderPayload(item, `${path}[${index}]`, redactedPaths, seen),
		);
	}
	const result: Record<string, unknown> = {};
	const imageLike = ["image", "inputimage", "imageurl"].includes(
		normalizedPayloadKey(String((value as Record<string, unknown>).type ?? "")),
	);
	for (const [key, child] of Object.entries(value)) {
		const childPath = `${path}.${key}`;
		if (isSensitivePayloadKey(key)) {
			result[key] = "[REDACTED]";
			redactedPaths.push(childPath);
			continue;
		}
		if (imageLike && ["data", "base64", "bytes"].includes(normalizedPayloadKey(key))) {
			result[key] = "[IMAGE DATA OMITTED]";
			redactedPaths.push(childPath);
			continue;
		}
		result[key] = redactProviderPayload(child, childPath, redactedPaths, seen);
	}
	return result;
}

export function capturePiContext(
	sequence: number,
	messages: unknown,
	model: RuntimeDebugModelCall["model"],
	requestConfig: RuntimeDebugModelCall["requestConfig"],
	turn?: number,
	capturedAtMs = Date.now(),
	compactionSequence?: number,
): RuntimeDebugModelCall {
	const piContextRedactedPaths: string[] = [];
	return {
		sequence,
		...(turn ? { turn } : {}),
		...(compactionSequence ? { compactionSequence } : {}),
		capturedAtMs,
		source: "pi_context_with_system",
		piContext: redactProviderPayload(
			messages,
			"$.piContext",
			piContextRedactedPaths,
			new WeakSet(),
		),
		piContextRedactedPaths,
		model,
		requestConfig,
		requestHeaders: [],
		payload: null,
		responseMetadata: [],
		redactedPaths: [],
	};
}

export function captureProviderRequest(
	call: RuntimeDebugModelCall,
	payload: unknown,
	capturedAtMs = Date.now(),
): RuntimeDebugModelCall {
	const redactedPaths = [...call.redactedPaths];
	return {
		...call,
		requestCapturedAtMs: capturedAtMs,
		payloadSource: "pi_before_provider_request",
		payload: redactProviderPayload(payload, "$.payload", redactedPaths, new WeakSet()),
		redactedPaths,
	};
}

function captureRequestHeaderSnapshot(
	headers: Record<string, string | null>,
	path: string,
	capturedAtMs = Date.now(),
): RuntimeDebugHeaderCapture {
	const redactedPaths: string[] = [];
	return {
		capturedAtMs,
		headers: redactProviderPayload(
			headers,
		path,
			redactedPaths,
			new WeakSet(),
		) as Record<string, string | null>,
		redactedPaths,
	};
}

function captureResponseMetadata(
	status: number,
	headers: Record<string, string>,
	path: string,
	capturedAtMs = Date.now(),
): RuntimeDebugResponseMetadata {
	const redactedPaths: string[] = [];
	return {
		capturedAtMs,
		status,
		headers: redactProviderPayload(
			headers,
			path,
			redactedPaths,
			new WeakSet(),
		) as Record<string, string>,
		redactedPaths,
	};
}

export function captureProviderHeaders(
	call: RuntimeDebugModelCall,
	headers: Record<string, string | null>,
	capturedAtMs = Date.now(),
): RuntimeDebugModelCall {
	const index = call.requestHeaders.length;
	return {
		...call,
		requestHeaders: [
			...call.requestHeaders,
			captureRequestHeaderSnapshot(
				headers,
				`$.requestHeaders[${index}].headers`,
				capturedAtMs,
			),
		],
	};
}

export function captureProviderResponseMetadata(
	call: RuntimeDebugModelCall,
	status: number,
	headers: Record<string, string>,
	capturedAtMs = Date.now(),
): RuntimeDebugModelCall {
	const index = call.responseMetadata.length;
	return {
		...call,
		responseMetadata: [
			...call.responseMetadata,
			captureResponseMetadata(
				status,
				headers,
				`$.responseMetadata[${index}].headers`,
				capturedAtMs,
			),
		],
	};
}

type CompactionPreparationLike = {
	firstKeptEntryId: string;
	messagesToSummarize: unknown[];
	turnPrefixMessages: unknown[];
	isSplitTurn: boolean;
	tokensBefore: number;
	previousSummary?: string;
};

export function buildCompactionStartTrace(
	sequence: number,
	turn: number | undefined,
	reason: RuntimeDebugCompaction["reason"],
	willRetry: boolean,
	preparation: CompactionPreparationLike,
	startedAtMs = Date.now(),
): RuntimeDebugCompaction {
	const summarized = previewCompactionMessages(preparation.messagesToSummarize, "summarized");
	const turnPrefix = previewCompactionMessages(preparation.turnPrefixMessages, "turn_prefix");
	const sourceMessages = [...summarized, ...turnPrefix];
	return {
		sequence,
		...(turn ? { turn } : {}),
		status: "started",
		reason,
		willRetry,
		startedAtMs,
		firstKeptEntryId: preparation.firstKeptEntryId,
		tokensBefore: preparation.tokensBefore,
		isSplitTurn: preparation.isSplitTurn,
		...(preparation.previousSummary
			? { previousSummary: preparation.previousSummary }
			: {}),
		summarizedMessageCount: preparation.messagesToSummarize.length,
		turnPrefixMessageCount: preparation.turnPrefixMessages.length,
		sourceMessages,
		sourceMessagesOmitted:
			Math.max(0, preparation.messagesToSummarize.length - summarized.length) +
			Math.max(0, preparation.turnPrefixMessages.length - turnPrefix.length),
		requestHeaders: [],
		responseMetadata: [],
	};
}

export function captureProviderResponse(
	call: RuntimeDebugModelCall,
	response: unknown,
	capturedAtMs = Date.now(),
): RuntimeDebugModelCall {
	const responseRedactedPaths: string[] = [];
	return {
		...call,
		response: redactProviderPayload(
			response,
			"$.response",
			responseRedactedPaths,
			new WeakSet(),
		),
		responseCapturedAtMs: capturedAtMs,
		responseRedactedPaths,
	};
}

export function appendRuntimeDebugModelCall(
	calls: RuntimeDebugModelCall[],
	call: RuntimeDebugModelCall,
): RuntimeDebugModelCall[] {
	calls.push(call);
	return calls;
}

function updateRuntimeDebugModelCall(
	calls: RuntimeDebugModelCall[],
	sequence: number,
	update: (call: RuntimeDebugModelCall) => RuntimeDebugModelCall,
): boolean {
	for (let index = calls.length - 1; index >= 0; index -= 1) {
		const call = calls[index];
		if (call?.sequence === sequence) {
			calls[index] = update(call);
			return true;
		}
	}
	return false;
}

function updateRuntimeDebugCompaction(
	compactions: RuntimeDebugCompaction[],
	sequence: number,
	update: (compaction: RuntimeDebugCompaction) => RuntimeDebugCompaction,
): boolean {
	for (let index = compactions.length - 1; index >= 0; index -= 1) {
		const compaction = compactions[index];
		if (compaction?.sequence === sequence) {
			compactions[index] = update(compaction);
			return true;
		}
	}
	return false;
}

function upsertRuntimeDebugCompaction(
	compactions: RuntimeDebugCompaction[],
	compaction: RuntimeDebugCompaction,
): RuntimeDebugCompaction[] {
	const existing = compactions.findIndex((item) => item.sequence === compaction.sequence);
	const next = [...compactions];
	if (existing >= 0) {
		next[existing] = compaction;
	} else {
		next.push(compaction);
	}
	return next;
}

function currentPiTurn(ctx: ExtensionContext): number | undefined {
	try {
		const turn = ctx.sessionManager.getBranch().filter((entry) => {
			if (entry.type !== "message") return false;
			return (entry.message as { role?: string }).role === "user";
		}).length;
		return turn > 0 ? turn : undefined;
	} catch {
		return undefined;
	}
}

export function buildRuntimeDebugPayload(
	systemPrompt: string,
	tools: ContextUsageTool[],
	modelCalls: RuntimeDebugModelCall[] = [],
	modelCallsOmitted = 0,
	compactions: RuntimeDebugCompaction[] = [],
	compactionsOmitted = 0,
): RuntimeDebugPayload {
	return {
		version: 3,
		source: "pi_runtime_debug",
		systemPrompt,
		tools,
		modelCalls,
		modelCallsOmitted,
		compactions,
		compactionsOmitted,
	};
}

function publishRuntimeDebug(
	ctx: ExtensionContext,
	systemPrompt: string,
	tools: ContextUsageTool[],
	modelCalls: RuntimeDebugModelCall[],
	modelCallsOmitted: number,
	compactions: RuntimeDebugCompaction[],
	compactionsOmitted: number,
): void {
	try {
		const effectiveSystemPrompt = systemPrompt || ctx.getSystemPrompt();
		ctx.ui.setWidget(SMELT_RUNTIME_DEBUG_WIDGET, [
			JSON.stringify(
				buildRuntimeDebugPayload(
					effectiveSystemPrompt,
					tools,
					modelCalls,
					modelCallsOmitted,
					compactions,
					compactionsOmitted,
				),
			),
		]);
	} catch {
		// 审计数据不能影响正常对话。
	}
}

export function createContextUsageExtension(): (pi: ExtensionAPI) => void {
	return (pi) => {
		let lastOptions: {
			contextFiles: Array<{ path: string; content: string }>;
			skills: Skill[];
		} = { contextFiles: [], skills: [] };
		let lastSystemPrompt = "";
		let lastTools: ContextUsageTool[] = [];
		let modelCallSequence = 0;
		let pendingModelCallSequence: number | undefined;
		let modelCalls: RuntimeDebugModelCall[] = [];
		let modelCallsOmitted = 0;
		let compactionSequence = 0;
		let activeCompactionSequence: number | undefined;
		let compactions: RuntimeDebugCompaction[] = [];
		let compactionsOmitted = 0;
		let runtimeDebugDirty = false;

		const flushRuntimeDebug = (ctx: ExtensionContext) => {
			if (!runtimeDebugDirty) return;
			publishRuntimeDebug(
				ctx,
				lastSystemPrompt,
				lastTools,
				modelCalls,
				modelCallsOmitted,
				compactions,
				compactionsOmitted,
			);
			runtimeDebugDirty = false;
		};

		const publishFrom = (ctx: ExtensionContext, systemPrompt: string) => {
			let tools: ContextUsageTool[] = [];
			try {
				const active = new Set(pi.getActiveTools());
				tools = pi
					.getAllTools()
					.filter((tool) => active.has(tool.name))
					.map((tool) => ({
						name: tool.name,
						description: tool.description,
						parameters: tool.parameters,
						source: tool.sourceInfo?.source,
					}));
			} catch {
				tools = [];
			}
			lastSystemPrompt = systemPrompt;
			lastTools = tools;
			publish(
				ctx,
				buildContextUsageBuckets({
					systemPrompt,
					contextFiles: lastOptions.contextFiles,
					skills: lastOptions.skills,
					tools,
					messages: messagesFromSession(ctx),
				}),
			);
		};

		pi.on("session_start", () => {
			lastSystemPrompt = "";
			lastTools = [];
			modelCallSequence = 0;
			pendingModelCallSequence = undefined;
			modelCalls = [];
			modelCallsOmitted = 0;
			compactionSequence = 0;
			activeCompactionSequence = undefined;
			compactions = [];
			compactionsOmitted = 0;
			runtimeDebugDirty = false;
		});

		pi.on("before_agent_start", (event, ctx) => {
			lastOptions = {
				contextFiles: event.systemPromptOptions.contextFiles ?? [],
				skills: event.systemPromptOptions.skills ?? [],
			};
			publishFrom(ctx, event.systemPrompt);
		});
		pi.on("context", (_event, ctx) => {
			publishFrom(ctx, ctx.getSystemPrompt());
		});
		pi.on("context_with_system", (event, ctx) => {
			if (pendingModelCallSequence !== undefined) {
				pendingModelCallSequence = undefined;
				runtimeDebugDirty = true;
			}
			modelCallSequence += 1;
			const sequence = modelCallSequence;
			modelCalls = appendRuntimeDebugModelCall(
				modelCalls,
				capturePiContext(
					sequence,
					event.messages,
					{
						provider: ctx.model?.provider,
						id: ctx.model?.id,
						api: ctx.model?.api,
						thinkingLevel: ctx.thinkingLevel,
					},
					{
						systemPrompt: lastSystemPrompt || ctx.getSystemPrompt(),
						tools: lastTools,
					},
					currentPiTurn(ctx),
					Date.now(),
					activeCompactionSequence,
				),
			);
			pendingModelCallSequence = sequence;
			runtimeDebugDirty = true;
		});
		pi.on("before_provider_headers", (event, _ctx) => {
			if (pendingModelCallSequence !== undefined) {
				updateRuntimeDebugModelCall(modelCalls, pendingModelCallSequence, (call) =>
					captureProviderHeaders(call, event.headers),
				);
				runtimeDebugDirty = true;
			} else if (activeCompactionSequence !== undefined) {
				updateRuntimeDebugCompaction(compactions, activeCompactionSequence, (item) => ({
					...item,
					requestHeaders: [
						...item.requestHeaders,
						captureRequestHeaderSnapshot(
							event.headers,
							`$.compactions[${item.sequence}].requestHeaders[${item.requestHeaders.length}].headers`,
						),
					],
				}));
				runtimeDebugDirty = true;
			}
		});
		pi.on("before_provider_request", (event) => {
			if (pendingModelCallSequence === undefined) return;
			updateRuntimeDebugModelCall(modelCalls, pendingModelCallSequence, (call) =>
				captureProviderRequest(call, event.payload),
			);
			runtimeDebugDirty = true;
		});
		pi.on("after_provider_response", (event) => {
			if (pendingModelCallSequence !== undefined) {
				updateRuntimeDebugModelCall(modelCalls, pendingModelCallSequence, (call) =>
					captureProviderResponseMetadata(call, event.status, event.headers),
				);
				runtimeDebugDirty = true;
			} else if (activeCompactionSequence !== undefined) {
				updateRuntimeDebugCompaction(compactions, activeCompactionSequence, (item) => ({
					...item,
					responseMetadata: [
						...item.responseMetadata,
						captureResponseMetadata(
							event.status,
							event.headers,
							`$.compactions[${item.sequence}].responseMetadata[${item.responseMetadata.length}].headers`,
						),
					],
				}));
				runtimeDebugDirty = true;
			}
		});
		pi.on("message_end", (event, _ctx) => {
			if (
				(event.message as { role?: string }).role !== "assistant" ||
				pendingModelCallSequence === undefined
			) {
				return;
			}
			updateRuntimeDebugModelCall(modelCalls, pendingModelCallSequence, (call) =>
				captureProviderResponse(call, event.message),
			);
			pendingModelCallSequence = undefined;
			runtimeDebugDirty = true;
		});
		pi.on("agent_end", () => {
			if (pendingModelCallSequence === undefined) return;
			// Do not infer a response from agent_end.messages: it can include prior
			// assistant turns. Only message_end is the per-response evidence boundary.
			pendingModelCallSequence = undefined;
			runtimeDebugDirty = true;
		});
		pi.on("agent_settled", (_event, ctx) => {
			if (pendingModelCallSequence !== undefined) {
				pendingModelCallSequence = undefined;
				runtimeDebugDirty = true;
			}
			flushRuntimeDebug(ctx);
		});

		pi.on("session_before_compact", (event, ctx) => {
			compactionSequence += 1;
			activeCompactionSequence = compactionSequence;
			compactions = upsertRuntimeDebugCompaction(
				compactions,
				buildCompactionStartTrace(
					compactionSequence,
					currentPiTurn(ctx),
					event.reason,
					event.willRetry,
					event.preparation,
				),
			);
			runtimeDebugDirty = true;
		});

		pi.on("session_compact", (event, ctx) => {
			const sequence = activeCompactionSequence ?? ++compactionSequence;
			const existing = compactions.find((item) => item.sequence === sequence);
			const trace: RuntimeDebugCompaction = {
				sequence,
				...(existing?.turn ? { turn: existing.turn } : {}),
				status: "completed",
				reason: event.reason,
				willRetry: event.willRetry,
				startedAtMs: existing?.startedAtMs ?? Date.now(),
				finishedAtMs: Date.now(),
				firstKeptEntryId: event.compactionEntry.firstKeptEntryId,
				tokensBefore: event.compactionEntry.tokensBefore,
				sourceMessages: existing?.sourceMessages ?? [],
				sourceMessagesOmitted: existing?.sourceMessagesOmitted ?? 0,
				requestHeaders: existing?.requestHeaders ?? [],
				responseMetadata: existing?.responseMetadata ?? [],
				summary: event.compactionEntry.summary,
				...(event.compactionEntry.usage
					? { usage: event.compactionEntry.usage }
					: {}),
				fromExtension: event.fromExtension,
				...(existing?.summarizedMessageCount !== undefined
					? { summarizedMessageCount: existing.summarizedMessageCount }
					: {}),
				...(existing?.turnPrefixMessageCount !== undefined
					? { turnPrefixMessageCount: existing.turnPrefixMessageCount }
					: {}),
				...(existing?.isSplitTurn !== undefined
					? { isSplitTurn: existing.isSplitTurn }
					: {}),
				...(existing?.previousSummary
					? { previousSummary: existing.previousSummary }
					: {}),
			};
			compactions = upsertRuntimeDebugCompaction(compactions, trace);
			activeCompactionSequence = undefined;
			runtimeDebugDirty = true;
			if (event.reason === "manual") flushRuntimeDebug(ctx);
		});

		pi.on("session_compact_failed", (event, ctx) => {
			const sequence = activeCompactionSequence ?? ++compactionSequence;
			const existing = compactions.find((item) => item.sequence === sequence);
			const trace: RuntimeDebugCompaction = {
				sequence,
				...(existing?.turn ? { turn: existing.turn } : {}),
				status: event.aborted ? "aborted" : "failed",
				reason: event.reason,
				willRetry: event.willRetry,
				startedAtMs: existing?.startedAtMs ?? Date.now(),
				finishedAtMs: Date.now(),
				sourceMessages: existing?.sourceMessages ?? [],
				sourceMessagesOmitted: existing?.sourceMessagesOmitted ?? 0,
				requestHeaders: existing?.requestHeaders ?? [],
				responseMetadata: existing?.responseMetadata ?? [],
				aborted: event.aborted,
				fromExtension: event.fromExtension,
				...(event.errorMessage ? { errorMessage: event.errorMessage } : {}),
				...(existing?.firstKeptEntryId
					? { firstKeptEntryId: existing.firstKeptEntryId }
					: {}),
				...(existing?.tokensBefore !== undefined
					? { tokensBefore: existing.tokensBefore }
					: {}),
				...(existing?.isSplitTurn !== undefined
					? { isSplitTurn: existing.isSplitTurn }
					: {}),
				...(existing?.previousSummary
					? { previousSummary: existing.previousSummary }
					: {}),
				...(existing?.summarizedMessageCount !== undefined
					? { summarizedMessageCount: existing.summarizedMessageCount }
					: {}),
				...(existing?.turnPrefixMessageCount !== undefined
					? { turnPrefixMessageCount: existing.turnPrefixMessageCount }
					: {}),
			};
			compactions = upsertRuntimeDebugCompaction(compactions, trace);
			activeCompactionSequence = undefined;
			runtimeDebugDirty = true;
			if (event.reason === "manual") flushRuntimeDebug(ctx);
		});
	};
}

export default createContextUsageExtension();
