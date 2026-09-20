import {
	formatSkillsForPrompt,
	type ExtensionAPI,
	type ExtensionContext,
	type Skill,
	type ToolInfo,
} from "@earendil-works/pi-coding-agent";

export const SMELT_CONTEXT_USAGE_WIDGET = "smelt-context-usage";

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

export function createContextUsageExtension(): (pi: ExtensionAPI) => void {
	return (pi) => {
		let lastOptions: {
			contextFiles: Array<{ path: string; content: string }>;
			skills: Skill[];
		} = { contextFiles: [], skills: [] };

		const publishFrom = (ctx: ExtensionContext, systemPrompt: string) => {
			let tools: ContextUsageTool[] = [];
			try {
				const active = new Set(pi.getActiveTools());
				tools = pi
					.getAllTools()
					.filter((tool) => active.size === 0 || active.has(tool.name))
					.map((tool) => ({
						name: tool.name,
						description: tool.description,
						parameters: tool.parameters,
						source: tool.sourceInfo?.source,
					}));
			} catch {
				tools = [];
			}
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
	};
}

export default createContextUsageExtension();
