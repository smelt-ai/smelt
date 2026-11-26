import type { ExtensionAPI, ToolDefinition } from "@earendil-works/pi-coding-agent";
import { SMELT_BACKGROUND_TASK_SOCK_ENV, callBackgroundHost } from "./background-task.ts";

/**
 * Pi 对话互发消息、起新对话的工具。工具定义、每次调用回给模型的文字和界面要的 details 都由
 * 主 daemon 给（`crates/smeltd/src/peer_tools.rs`，第三方引擎的 MCP 也用这一份），这里只转接：
 * 加载时经会话宿主要一次定义，调用时把参数和这次的 toolCallId 交上去，回答原样交给 Pi。
 * 来源由 daemon 按宿主的连接认定，工具参数里没有来源。子智能体、没有会话宿主时要不到定义，
 * 不注册。
 */
export const SMELT_SESSION_PEERS_EXTENSION_NAME = "smelt-session-peers";
export const SMELT_SESSION_PEERS_EXTENSION_PATH = `<inline:${SMELT_SESSION_PEERS_EXTENSION_NAME}>`;
/** 只读的两个，免审批按名字和来源认（`smelt-permission.ts`）。 */
export const SMELT_SESSION_LIST_TOOL_NAME = "session_list";
export const SMELT_SESSION_READ_TOOL_NAME = "session_read";

type PeerToolDefinition = Pick<ToolDefinition, "name" | "label" | "description" | "promptSnippet" | "promptGuidelines"> & {
	parameters: Record<string, unknown>;
};

type PeerHostReply = {
	ok: boolean;
	error?: string;
	text?: string;
	details?: Record<string, unknown>;
	tools?: PeerToolDefinition[];
};

async function callPeers(request: Record<string, unknown>, signal?: AbortSignal): Promise<PeerHostReply> {
	return (await callBackgroundHost(process.env[SMELT_BACKGROUND_TASK_SOCK_ENV], request, signal)) as PeerHostReply;
}

function peerTool(definition: PeerToolDefinition): ToolDefinition {
	// 只交定义里声明过的参数：多出来的字段（比如 owner）不能盖掉宿主连接上的身份。
	const declared = Object.keys((definition.parameters.properties as Record<string, unknown> | undefined) ?? {});
	return {
		...definition,
		parameters: definition.parameters as ToolDefinition["parameters"],
		async execute(toolCallId, params, signal) {
			const given = params as Record<string, unknown>;
			const args = Object.fromEntries(declared.filter((key) => key in given).map((key) => [key, given[key]]));
			const reply = await callPeers({ ...args, op: definition.name, toolCallId }, signal);
			const text = reply.text || reply.error || "和别的对话通信失败";
			const result = { content: [{ type: "text" as const, text }], details: reply.details ?? {} };
			return reply.ok ? result : { ...result, isError: true };
		},
	};
}

export async function smeltSessionPeersExtension(pi: ExtensionAPI): Promise<void> {
	const reply = await callPeers({ op: "session_tools" });
	const definitions = reply.ok && Array.isArray(reply.tools) ? reply.tools : [];
	if (definitions.length === 0) return;
	const register = () => {
		let configured: ReturnType<ExtensionAPI["getAllTools"]>;
		try {
			configured = pi.getAllTools();
		} catch {
			return;
		}
		// 用户自己有同名工具时不盖掉。
		const names = new Set(configured.map((tool) => tool.name));
		for (const definition of definitions) {
			if (!names.has(definition.name)) pi.registerTool(peerTool(definition));
		}
	};
	register();
	// 工具表在工厂执行时可能还没绑上。session_start 再注册一次。
	pi.on("session_start", register);
}

export default smeltSessionPeersExtension;
