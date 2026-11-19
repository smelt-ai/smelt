import { Type } from "typebox";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { SMELT_BACKGROUND_TASK_SOCK_ENV, callBackgroundHost } from "./background-task.ts";

/** Smelt 宿主保留的唯一选择题工具与具名内联 Extension 身份。 */
export const SMELT_ELICITATION_TOOL_NAME = "smelt_ask_user";
export const SMELT_ELICITATION_EXTENSION_NAME = "smelt-elicitation";
export const SMELT_ELICITATION_EXTENSION_PATH =
	`<inline:${SMELT_ELICITATION_EXTENSION_NAME}>`;

/** 只在宿主工具内部把兼容输入归一化为选择题，不参与权限判断。 */

export type AskUserOption = {
	label: string;
	description?: string;
};

export type AskUserQuestion = {
	question: string;
	options: AskUserOption[];
	multiSelect: boolean;
};

function optionLabel(value: unknown): string | undefined {
	if (typeof value === "string") {
		const label = value.trim();
		return label.length > 0 ? label : undefined;
	}
	if (value && typeof value === "object") {
		const record = value as Record<string, unknown>;
		for (const key of ["label", "title", "value", "text"]) {
			const label = typeof record[key] === "string" ? record[key].trim() : "";
			if (label.length > 0) {
				return label;
			}
		}
	}
	return undefined;
}

function parseOptions(value: unknown): AskUserOption[] {
	if (!Array.isArray(value)) {
		return [];
	}
	const options: AskUserOption[] = [];
	for (const item of value) {
		const label = optionLabel(item);
		if (!label) {
			continue;
		}
		const description =
			item && typeof item === "object" && typeof (item as { description?: unknown }).description === "string"
				? (item as { description: string }).description.trim()
				: undefined;
		options.push({
			label,
			description: description && description.length > 0 ? description : undefined,
		});
	}
	return options;
}

function parseOneQuestion(value: unknown): AskUserQuestion | undefined {
	if (!value || typeof value !== "object") {
		return undefined;
	}
	const record = value as Record<string, unknown>;
	const question =
		(typeof record.question === "string" && record.question.trim()) ||
		(typeof record.header === "string" && record.header.trim()) ||
		(typeof record.prompt === "string" && record.prompt.trim()) ||
		"";
	const options = parseOptions(record.options ?? record.choices);
	if (!question || options.length === 0) {
		return undefined;
	}
	return {
		question,
		options,
		multiSelect: record.multiSelect === true || record.multi_select === true,
	};
}

/** 把唯一宿主工具的输入归一化为一道或多道选择题。 */
export function parseAskUserQuestions(input: unknown): AskUserQuestion[] {
	if (!input || typeof input !== "object") {
		return [];
	}
	const record = input as Record<string, unknown>;
	if (Array.isArray(record.questions)) {
		return record.questions.flatMap((item) => {
			const parsed = parseOneQuestion(item);
			return parsed ? [parsed] : [];
		});
	}
	const single = parseOneQuestion(record);
	return single ? [single] : [];
}

export type AskUserAnswer = {
	question: string;
	answer: string[];
};

type AskUserReply = {
	ok: boolean;
	error?: string;
	cancelled?: boolean;
	answers?: AskUserAnswer[];
};

const parameters = Type.Object({
	questions: Type.Optional(Type.Array(Type.Any())),
	question: Type.Optional(Type.String()),
	header: Type.Optional(Type.String()),
	prompt: Type.Optional(Type.String()),
	options: Type.Optional(Type.Array(Type.Any())),
	choices: Type.Optional(Type.Array(Type.Any())),
	multiSelect: Type.Optional(Type.Boolean()),
});

function askUserQuestionTool() {
	return defineTool({
		name: SMELT_ELICITATION_TOOL_NAME,
		label: "Ask",
		description:
			"向用户展示选择题并等待点选。需要用户在几个明确选项里做决定时必须用这个工具，不要改口用纯文本追问。多选题把 multiSelect 设为 true。",
		parameters,
		async execute(_toolCallId, params, signal) {
			const questions = parseAskUserQuestions(params);
			if (questions.length === 0) {
				const details: Record<string, unknown> = { error: true };
				return {
					content: [{ type: "text" as const, text: "没有可展示的选项" }],
					details,
					isError: true,
				};
			}
			// 整组题原样交给会话宿主：一张卡、选项带说明、多选是结构化的，答案按题交回。
			const reply = (await callBackgroundHost(
				process.env[SMELT_BACKGROUND_TASK_SOCK_ENV],
				{ op: "ask_user", questions },
				signal,
			)) as AskUserReply;
			if (!reply.ok) {
				const details: Record<string, unknown> = { error: true };
				return {
					content: [{ type: "text" as const, text: reply.error || "没能向用户提问" }],
					details,
					isError: true,
				};
			}
			if (reply.cancelled || !reply.answers) {
				// 跳过的也列出题面，对话记录里能看出跳过的是哪几道。
				const answers: AskUserAnswer[] = questions.map((item) => ({
					question: item.question,
					answer: [],
				}));
				const details: Record<string, unknown> = { cancelled: true, answers };
				return {
					content: [{ type: "text" as const, text: "用户跳过了这个问题，没有作答" }],
					details,
				};
			}
			const details: Record<string, unknown> = { answers: reply.answers };
			return {
				content: [{ type: "text" as const, text: JSON.stringify(reply.answers) }],
				details,
			};
		},
	});
}

function configuredTools(pi: ExtensionAPI): ReturnType<ExtensionAPI["getAllTools"]> | undefined {
	try {
		return pi.getAllTools();
	} catch {
		// 加载期 registry 尚未绑定；session_start 后重试。
		return undefined;
	}
}

export function registerHostElicitationTool(pi: ExtensionAPI): void {
	const tools = configuredTools(pi);
	if (!tools) {
		return;
	}
	const existing = tools.find((tool) => tool.name === SMELT_ELICITATION_TOOL_NAME);
	if (existing) {
		if (existing.sourceInfo.path !== SMELT_ELICITATION_EXTENSION_PATH) {
			console.warn(
				`Reserved Smelt tool "${SMELT_ELICITATION_TOOL_NAME}" is already registered by ${existing.sourceInfo.path}; the host tool was not installed and calls to the conflicting tool will be blocked.`,
			);
		}
		return;
	}
	pi.registerTool(askUserQuestionTool());
}

export function smeltElicitationExtension(pi: ExtensionAPI): void {
	registerHostElicitationTool(pi);
	pi.on("session_start", () => {
		// 初始加载期 Pi 尚未绑定工具 registry；session_start 后再按实际赢家注册。
		registerHostElicitationTool(pi);
	});
}

export default smeltElicitationExtension;
