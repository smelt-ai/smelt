import { Type } from "typebox";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { SMELT_BACKGROUND_TASK_SOCK_ENV, backgroundTaskOwner, callBackgroundHost } from "./background-task.ts";

/**
 * 主动通知用户：桌面弹系统通知，并推给在 Smelt 手机端打开了「智能体消息」的手机。
 * 请求经会话宿主交给主 daemon，来源由 daemon 按宿主的连接认定（见 smeltd 的 `agent_notify`）。
 * 不受自动化通知开关管，人在不在电脑前都推，所以只在真有事要用户知道时用。
 */
export const SMELT_NOTIFY_EXTENSION_NAME = "smelt-notify";
export const SMELT_NOTIFY_EXTENSION_PATH = `<inline:${SMELT_NOTIFY_EXTENSION_NAME}>`;
export const SMELT_NOTIFY_TOOL_NAME = "notify_user";

type NotifyReply = {
	ok: boolean;
	error?: string;
	desktop?: boolean;
	phones?: number;
};

/** 发出去之后写给模型的说明：发到了哪里。 */
export function notifyText(reply: NotifyReply): string {
	const desktop = reply.desktop ? "桌面已弹出通知" : "桌面没有连着的 Smelt 窗口";
	const phones = reply.phones ?? 0;
	const phone =
		phones > 0
			? `已推送到 ${phones} 台手机`
			: "没有手机在接收智能体消息（用户可在 Smelt 手机端的设置里打开推送）";
	return `${desktop}；${phone}。`;
}

function registerNotifyTool(pi: ExtensionAPI): void {
	// 子智能体把结果交回父会话，由父会话决定要不要打扰用户。
	if (backgroundTaskOwner() !== "") return;
	let configured: ReturnType<ExtensionAPI["getAllTools"]>;
	try {
		configured = pi.getAllTools();
	} catch {
		return;
	}
	if (configured.some((tool) => tool.name === SMELT_NOTIFY_TOOL_NAME)) return;
	pi.registerTool(defineTool({
		name: SMELT_NOTIFY_TOOL_NAME,
		label: "通知用户",
		description:
			"给用户发一条通知：电脑上弹出系统通知，并推送到用户的手机（iOS）。用户不在电脑前也能收到。内容由你写，只在真有事需要用户知道时用。",
		promptSnippet: "Send the user a desktop and phone notification",
		promptGuidelines: [
			"Use notify_user only when something needs the user's attention right away, or when the user or the task asks to be notified (e.g. an automation found a problem, a long job the user is waiting on finished).",
			"Do not use notify_user for routine progress or to repeat your reply; the user already sees your replies in the conversation.",
			"Write notify_user messages so they make sense on a lock screen: lead with the conclusion, keep it to a few short sentences.",
		],
		parameters: Type.Object({
			message: Type.String({ description: "通知正文。先说结论，几句话以内，锁屏上也能看懂" }),
			title: Type.Optional(Type.String({ description: "短标题。不填就用会话或自动化的名字" })),
		}),
		async execute(_toolCallId, params, signal) {
			const reply = (await callBackgroundHost(
				process.env[SMELT_BACKGROUND_TASK_SOCK_ENV],
				{ op: "notify", message: params.message, title: params.title },
				signal,
			)) as NotifyReply;
			// details 标成通用对象：成功、失败时形状不同，defineTool 要能推出共同的类型。
			if (!reply.ok) {
				const details: Record<string, unknown> = { error: true };
				return {
					content: [{ type: "text" as const, text: reply.error || "通知没发出去" }],
					details,
					isError: true,
				};
			}
			const details: Record<string, unknown> = { desktop: reply.desktop ?? false, phones: reply.phones ?? 0 };
			return { content: [{ type: "text" as const, text: notifyText(reply) }], details };
		},
	}));
}

export function smeltNotifyExtension(pi: ExtensionAPI): void {
	registerNotifyTool(pi);
	// 工具表在工厂执行时可能还没绑上。session_start 再注册一次。
	pi.on("session_start", () => registerNotifyTool(pi));
}

export default smeltNotifyExtension;
