import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

/** 从工具参数里认出选择题。只看形状，不看工具名。 */

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

/** 参数里抽得出至少一道带选项的题，才当成选择题。抽不出就当普通工具。 */
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

export function isElicitationInput(input: unknown): boolean {
	return parseAskUserQuestions(input).length > 0;
}

/** Pi `ui.select` 没有多选 API。题目前缀这个不可见标记，宿主据此画勾选卡。 */
export const MULTI_SELECT_TITLE_MARK = "\u200B\u200C\u200B";

/** 宿主自带的选择题：一份实现，挂上模型常喊的名字。已有同名用户工具则跳过，否则 Pi 会把冲突当启动错误退出。 */
export const HOST_ELICITATION_TOOL_NAMES = [
	"question",
	"questionnaire",
	"ask_user_question",
	"AskUserQuestion",
] as const;

function selectLabels(question: AskUserQuestion): string[] {
	return question.options.map((option) =>
		option.description ? `${option.label} — ${option.description}` : option.label,
	);
}

function labelFromSelection(question: AskUserQuestion, selected: string): string {
	const matched = question.options.find(
		(option) =>
			selected === option.label ||
			selected === (option.description ? `${option.label} — ${option.description}` : option.label),
	);
	return matched?.label ?? selected;
}

function parseJsonStringArray(selected: string): string[] | undefined {
	const trimmed = selected.trim();
	if (!trimmed.startsWith("[")) {
		return undefined;
	}
	try {
		const value = JSON.parse(trimmed) as unknown;
		if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
			return undefined;
		}
		return value as string[];
	} catch {
		return undefined;
	}
}

/** 宿主多选会把所选标签编成 JSON 数组字符串；单选仍是一个标签。 */
function selectedLabels(question: AskUserQuestion, selected: string): string[] {
	const values = parseJsonStringArray(selected) ?? [selected];
	return values.map((value) => labelFromSelection(question, value));
}

const parameters = Type.Object({
	questions: Type.Optional(Type.Array(Type.Any())),
	question: Type.Optional(Type.String()),
	header: Type.Optional(Type.String()),
	prompt: Type.Optional(Type.String()),
	options: Type.Optional(Type.Array(Type.Any())),
	choices: Type.Optional(Type.Array(Type.Any())),
	multiSelect: Type.Optional(Type.Boolean()),
});

function askUserQuestionTool(name: string) {
	return defineTool({
		name,
		label: "提问",
		description:
			"向用户展示选择题并等待点选。需要用户在几个明确选项里做决定时必须用这个工具，不要改口用纯文本追问。多选题把 multiSelect 设为 true。",
		parameters,
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			const questions = parseAskUserQuestions(params);
			if (questions.length === 0) {
				return {
					content: [{ type: "text" as const, text: "没有可展示的选项" }],
					details: { cancelled: true },
				};
			}
			const answers: Array<{ question: string; answer: string }> = [];
			for (const question of questions) {
				const title = question.multiSelect
					? `${MULTI_SELECT_TITLE_MARK}${question.question}`
					: question.question;
				const selected = await ctx.ui.select(title, selectLabels(question));
				if (selected === undefined) {
					return {
						content: [{ type: "text" as const, text: "用户取消了选择" }],
						details: { cancelled: true },
					};
				}
				const labels = selectedLabels(question, selected);
				if (labels.length === 0) {
					return {
						content: [{ type: "text" as const, text: "用户取消了选择" }],
						details: { cancelled: true },
					};
				}
				answers.push({
					question: question.question,
					answer: labels.join("、"),
				});
			}
			return {
				content: [{ type: "text" as const, text: JSON.stringify(answers) }],
				details: { answers },
			};
		},
	});
}

function existingToolNames(pi: ExtensionAPI): Set<string> | undefined {
	try {
		return new Set(pi.getAllTools().map((tool) => tool.name));
	} catch {
		// 加载期 getAllTools 还不能用，这时候注册会和 ~/.pi 里的同名工具冲突，Pi 直接退出。
		return undefined;
	}
}

export function registerMissingHostElicitationTools(pi: ExtensionAPI): void {
	const existing = existingToolNames(pi);
	if (!existing) {
		return;
	}
	for (const name of HOST_ELICITATION_TOOL_NAMES) {
		if (existing.has(name)) {
			continue;
		}
		pi.registerTool(askUserQuestionTool(name));
		existing.add(name);
	}
}

export default function smeltElicitationExtension(pi: ExtensionAPI): void {
	registerMissingHostElicitationTools(pi);
	pi.on("session_start", () => {
		registerMissingHostElicitationTools(pi);
	});
}
