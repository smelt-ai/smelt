/** Pi 会话里后台子智能体结果的自定义消息类型。Smelt 认它画成状态行，不当用户消息。 */
export const SMELT_SUBAGENT_RESULT_CUSTOM_TYPE = "smelt.subagent_result";

/** 状态行展开后最多显示这么多字符；模型拿到的是完整结果。 */
const OUTPUT_TAIL_CHARS = 4000;

/** 和宿主里那条后台任务记的状态一致：被叫停就是停止，不算失败。 */
export type SubagentResultStatus = "completed" | "failed" | "stopped";

export type SubagentResultNotice = {
	id: string;
	title: string;
	status: SubagentResultStatus;
	text: string;
};

const STATUS_LABEL: Record<SubagentResultStatus, string> = {
	completed: "完成",
	failed: "失败",
	stopped: "已停止",
};

/**
 * 写给模型的结果：说清楚这是后台子智能体交回的结果，不是用户又提了要求。
 * 和后台任务的 `<task-notification>` 同一个写法。
 */
export function subagentResultText(notice: SubagentResultNotice): string {
	// 和界面状态行同一个写法：有标题时括号紧跟「已结束」，没有时空一格。
	const who = notice.title ? `${notice.id}（${notice.title}）` : `${notice.id} `;
	const status = STATUS_LABEL[notice.status];
	return [
		"<subagent-notification>",
		`这是后台子智能体交回的结果，不是用户的新请求。子智能体 ${who}已结束，状态 ${status}。`,
		"<result>",
		notice.text.trim() || "(no output)",
		"</result>",
		"</subagent-notification>",
	].join("\n");
}

/**
 * 发回会话的自定义消息。`display: false`：Pi 自己不画，Smelt 按类型画成一行状态，
 * 点开看 `outputTail`。以前用 `sendUserMessage`，结果被当成用户发的气泡，模型也以为是用户说的。
 */
export function subagentResultMessage(notice: SubagentResultNotice) {
	const text = notice.text.trim();
	return {
		customType: SMELT_SUBAGENT_RESULT_CUSTOM_TYPE,
		content: [{ type: "text" as const, text: subagentResultText(notice) }],
		display: false,
		details: {
			id: notice.id,
			title: notice.title,
			status: notice.status,
			exitCode: null,
			outputTail: text.length > OUTPUT_TAIL_CHARS ? `…${text.slice(-OUTPUT_TAIL_CHARS)}` : text,
		},
	};
}
