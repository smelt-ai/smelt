/** 另一个 Pi 对话发来的消息在 Pi 会话里的自定义消息类型。Smelt 认它画成事件行，不当用户消息。 */
export const SMELT_PEER_MESSAGE_CUSTOM_TYPE = "smelt.peer_message";

/** `bounce` 是退信：发出去的消息没送到，只由主 daemon 写。 */
export type PeerMessageKind = "message" | "task" | "completion" | "bounce";

export function peerMessageKind(value: unknown): PeerMessageKind {
	return value === "task" || value === "completion" || value === "bounce" ? value : "message";
}

/** 宿主交来的一条：主 daemon 已经核过来源，这里只负责写成消息。 */
export type PeerMessageNotice = {
	/** 主 daemon 按（来源，toolCallId）算出的 `pm-` id。宿主等的 ack 认它。 */
	id: string;
	sourceSessionId: string;
	sourceTitle: string;
	kind: PeerMessageKind;
	hops: number;
	replyTo: string | null;
	text: string;
	/** 写给模型的 `<peer-message>`，主 daemon 写好的（`smelt_model::acp_chat::format_peer_message_text`）。 */
	prompt: string;
};

/**
 * 发进会话的自定义消息。`display: false`：Pi 自己不画，Smelt 按类型画成事件行。
 * `details.text` 是对方的原文，界面显示它；`prompt` 只给模型看。
 */
export function peerMessage(notice: PeerMessageNotice) {
	return {
		customType: SMELT_PEER_MESSAGE_CUSTOM_TYPE,
		content: [{ type: "text" as const, text: notice.prompt }],
		display: false,
		details: {
			id: notice.id,
			sourceSessionId: notice.sourceSessionId,
			sourceTitle: notice.sourceTitle,
			kind: notice.kind,
			hops: notice.hops,
			replyTo: notice.replyTo,
			text: notice.text,
		},
	};
}
