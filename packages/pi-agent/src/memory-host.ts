import { randomUUID } from "node:crypto";
import { connect } from "node:net";
import type { EmbeddingConfig } from "./memory-embedding.ts";
import type { MemoryLayer } from "./memory-files.ts";

/**
 * 记忆元数据（使用统计、来源）在 Smelt 主库。Pi 进程不碰数据库，托管它的会话宿主也不
 * 初始化 SQLite，所以直接调主 daemon 的 `agent.memory.*` 控制方法：带上 daemon 注入的
 * 会话 id 和 capability token，记忆层由 daemon 按会话的启动规格决定。
 * 协议见 Rust `smelt_core::control_api::AgentMemory*` 与 `smeltd/src/agent_memory_control.rs`。
 * 信封版本与 Rust `CONTROL_API_VERSION` 对齐：daemon 只接受不低于它最低支持版本的请求，
 * 两边一起升（v3 起不再接受 v2）。
 */
const CONTROL_API_VERSION = 3;
const HOST_TIMEOUT_MS = 5_000;

export type MemoryUsage = { readCount: number; lastReadAtMs: number };
export type MemoryTrust = "trusted" | "pending";
export type MemoryProvenance = { trust: MemoryTrust; reason: string };

export type MemoryHostMeta = {
	/** 智能体当前是否开着记忆。会话启动后用户可能在配置页关掉。 */
	enabled: boolean;
	/** 用户配置的向量检索服务；没配为空。 */
	embedding?: EmbeddingConfig;
	/** 用户指定的重排模型；没指定为空（自动挑）。 */
	rerankModel?: { provider: string; id: string };
	usage: Map<string, MemoryUsage>;
	pending: { agent: Set<string>; user: Set<string> };
};

/** 调一个控制方法；成功返回 result，失败（连不上、超时、被拒）返回 undefined。 */
export type MemoryHostTransport = (method: string, params: Record<string, unknown>) => Promise<unknown | undefined>;

function socketTransport(): MemoryHostTransport {
	return (method, params) => {
		const socketPath = process.env.SMELT_SOCK;
		const sessionId = process.env.SMELT_SESSION_ID;
		const token = process.env.SMELT_AGENT_TOKEN;
		if (!socketPath || !sessionId || !token) return Promise.resolve(undefined);
		return new Promise((resolve) => {
			let buffer = "";
			let settled = false;
			const socket = connect(socketPath);
			const finish = (value: unknown | undefined) => {
				if (settled) return;
				settled = true;
				clearTimeout(timer);
				socket.destroy();
				resolve(value);
			};
			const timer = setTimeout(() => finish(undefined), HOST_TIMEOUT_MS);
			socket.setEncoding("utf8");
			socket.on("connect", () => {
				const request = {
					op: "control",
					control_version: CONTROL_API_VERSION,
					request_id: randomUUID(),
					method,
					params: { session_id: sessionId, token, ...params },
				};
				socket.write(`${JSON.stringify(request)}\n`);
			});
			socket.on("data", (chunk: string) => {
				buffer += chunk;
				const end = buffer.indexOf("\n");
				if (end < 0) return;
				try {
					// ControlResponse 的 outcome 是 flatten 的：`{control_version, request_id, status, result | error}`。
					const response = JSON.parse(buffer.slice(0, end)) as { status?: string; result?: unknown };
					finish(response.status === "ok" ? response.result : undefined);
				} catch {
					finish(undefined);
				}
			});
			socket.on("error", () => finish(undefined));
			socket.on("close", () => finish(undefined));
		});
	};
}

let transport: MemoryHostTransport = socketTransport();

export function setMemoryHostTransportForTests(next: MemoryHostTransport | undefined): void {
	transport = next ?? socketTransport();
}

/** 两层的使用统计和待确认列表；daemon 不可达时返回 undefined，调用方必须按"不知道"处理。 */
export async function fetchMemoryMeta(): Promise<MemoryHostMeta | undefined> {
	type Layer = { usage?: Array<{ file: string; read_count: number; last_read_at_ms: number }>; pending?: string[] };
	const result = (await transport("agent.memory.meta", {})) as
		| {
				enabled?: boolean;
				agent?: Layer;
				user?: Layer;
				embedding?: EmbeddingConfig;
				rerank_model?: { provider: string; id: string };
		  }
		| undefined;
	if (!result) return undefined;
	return {
		enabled: result.enabled === true,
		embedding: result.embedding?.base_url && result.embedding.model ? result.embedding : undefined,
		rerankModel: result.rerank_model?.provider && result.rerank_model.id ? result.rerank_model : undefined,
		usage: new Map(
			(result.agent?.usage ?? []).map((record) => [
				record.file,
				{ readCount: record.read_count, lastReadAtMs: record.last_read_at_ms },
			]),
		),
		pending: { agent: new Set(result.agent?.pending ?? []), user: new Set(result.user?.pending ?? []) },
	};
}

/** 记一次使用。失败无妨，不影响读取本身。 */
export async function recordMemoryRead(layer: MemoryLayer, file: string): Promise<void> {
	await transport("agent.memory.record_read", { layer, file });
}

/** 记下一次成功写入的来源。返回 false 时调用方应撤销待确认的写入，免得它被当成可信。 */
export async function recordMemoryWrite(layer: MemoryLayer, file: string, provenance: MemoryProvenance): Promise<boolean> {
	return (await transport("agent.memory.record_write", { layer, file, ...provenance })) !== undefined;
}
