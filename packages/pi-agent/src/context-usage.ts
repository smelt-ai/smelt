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

export type RuntimeDebugModelCall = {
	sequence: number;
	source: "pi_before_provider_request";
	model: {
		provider?: string;
		id?: string;
		api?: string;
		thinkingLevel?: string;
	};
	payload: unknown;
	redactedPaths: string[];
};

export type RuntimeDebugPayload = {
	version: 2;
	source: "pi_runtime_debug";
	systemPrompt: string;
	tools: ContextUsageTool[];
	modelCall?: RuntimeDebugModelCall;
};

const SENSITIVE_PAYLOAD_KEYS = new Set([
	"authorization",
	"proxyauthorization",
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

function redactProviderPayload(
	value: unknown,
	path: string,
	redactedPaths: string[],
	seen: WeakSet<object>,
): unknown {
	if (value === null || ["string", "number", "boolean"].includes(typeof value)) {
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
	for (const [key, child] of Object.entries(value)) {
		const childPath = `${path}.${key}`;
		if (SENSITIVE_PAYLOAD_KEYS.has(normalizedPayloadKey(key))) {
			result[key] = "[REDACTED]";
			redactedPaths.push(childPath);
			continue;
		}
		result[key] = redactProviderPayload(child, childPath, redactedPaths, seen);
	}
	return result;
}

export function captureProviderRequest(
	sequence: number,
	payload: unknown,
	model: RuntimeDebugModelCall["model"],
): RuntimeDebugModelCall {
	const redactedPaths: string[] = [];
	return {
		sequence,
		source: "pi_before_provider_request",
		model,
		payload: redactProviderPayload(payload, "$", redactedPaths, new WeakSet()),
		redactedPaths,
	};
}

export function buildRuntimeDebugPayload(
	systemPrompt: string,
	tools: ContextUsageTool[],
	modelCall?: RuntimeDebugModelCall,
): RuntimeDebugPayload {
	return {
		version: 2,
		source: "pi_runtime_debug",
		systemPrompt,
		tools,
		...(modelCall ? { modelCall } : {}),
	};
}

function publishRuntimeDebug(
	ctx: ExtensionContext,
	systemPrompt: string,
	tools: ContextUsageTool[],
	modelCall?: RuntimeDebugModelCall,
): void {
	try {
		ctx.ui.setWidget(SMELT_RUNTIME_DEBUG_WIDGET, [
			JSON.stringify(buildRuntimeDebugPayload(systemPrompt, tools, modelCall)),
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

		const publishFrom = (
			ctx: ExtensionContext,
			systemPrompt: string,
			includeRuntimeDebug: boolean,
		) => {
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
			if (includeRuntimeDebug) {
				publishRuntimeDebug(ctx, systemPrompt, tools);
			}
		};

		pi.on("before_agent_start", (event, ctx) => {
			lastOptions = {
				contextFiles: event.systemPromptOptions.contextFiles ?? [],
				skills: event.systemPromptOptions.skills ?? [],
			};
			publishFrom(ctx, event.systemPrompt, true);
		});
		pi.on("context", (_event, ctx) => {
			publishFrom(ctx, ctx.getSystemPrompt(), false);
		});
		pi.on("before_provider_request", (event, ctx) => {
			modelCallSequence += 1;
			const modelCall = captureProviderRequest(modelCallSequence, event.payload, {
				provider: ctx.model?.provider,
				id: ctx.model?.id,
				api: ctx.model?.api,
				thinkingLevel: ctx.thinkingLevel,
			});
			publishRuntimeDebug(ctx, lastSystemPrompt, lastTools, modelCall);
		});
	};
}

export default createContextUsageExtension();
