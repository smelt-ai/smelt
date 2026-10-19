/** Pi 会话里后台子智能体派发结果的自定义消息类型。Smelt 认它画成状态行，不当用户消息。 */
export const SMELT_SUBAGENT_RESULT_CUSTOM_TYPE = "smelt.subagent_result";

/** 状态行展开后最多显示这么多字符；模型拿到的是完整结果。 */
const OUTPUT_TAIL_CHARS = 4000;

/** 一次派发交回会话时的状态。被叫停就是停止，不算失败。 */
export type SubagentResultStatus = "completed" | "failed" | "stopped";

/** 派发里一个子智能体的结束状态。`interrupted`：没跑完就没了下文。 */
export type SubagentRunStatus = "running" | "completed" | "failed" | "stopped" | "interrupted";

export type SubagentRunSummary = {
	id: string;
	title: string;
	status: SubagentRunStatus;
};

export type SubagentResultNotice = {
	/** 派发它的那次 `subagent` 工具调用。宿主等的 ack、界面找卡片都认它。 */
	toolCallId: string;
	title: string;
	status: SubagentResultStatus;
	text: string;
	runs: SubagentRunSummary[];
};

const STATUS_LABEL: Record<SubagentRunStatus, string> = {
	running: "运行中",
	completed: "完成",
	failed: "失败",
	stopped: "已停止",
	interrupted: "已中断",
};

/**
 * 写给模型的结果：说清楚这是后台子智能体交回的结果，不是用户又提了要求。
 * 和后台任务的 `<task-notification>` 同一个写法。
 */
export function subagentResultText(notice: SubagentResultNotice): string {
	const who = notice.title ? `（${notice.title}）` : "";
	const runs = notice.runs.map((run) => `- ${run.id}${run.title ? `（${run.title}）` : ""}：${STATUS_LABEL[run.status]}`);
	return [
		"<subagent-notification>",
		`这是后台子智能体交回的结果，不是用户的新请求。派发${who}已结束，状态 ${STATUS_LABEL[notice.status]}。`,
		...(runs.length > 1 ? ["子智能体：", ...runs] : []),
		"<result>",
		notice.text.trim() || "(no output)",
		"</result>",
		"</subagent-notification>",
	].join("\n");
}

/**
 * 发回会话的自定义消息。`display: false`：Pi 自己不画，Smelt 按类型画成一行状态，
 * 点开看 `outputTail`。`details.id` 是派发的工具调用 id：宿主等 ack、界面找卡片都认它。
 */
export function subagentResultMessage(notice: SubagentResultNotice) {
	const text = notice.text.trim();
	return {
		customType: SMELT_SUBAGENT_RESULT_CUSTOM_TYPE,
		content: [{ type: "text" as const, text: subagentResultText(notice) }],
		display: false,
		details: {
			id: notice.toolCallId,
			title: notice.title,
			status: notice.status,
			exitCode: null,
			outputTail: text.length > OUTPUT_TAIL_CHARS ? `…${text.slice(-OUTPUT_TAIL_CHARS)}` : text,
			runs: notice.runs,
		},
	};
}
