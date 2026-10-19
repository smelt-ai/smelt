import { connect, type Socket } from "node:net";
import { SMELT_BACKGROUND_TASK_SOCK_ENV, backgroundTaskOwner } from "../background-task.ts";

/** 子智能体结束时报给宿主的状态。 */
export type SubagentEndStatus = "completed" | "failed" | "stopped";

export type SubagentProgress = {
	turns: number;
	toolCalls: number;
	/** 最近一次工具调用的标题：「正在做什么」。 */
	lastActivity?: string;
};

export type SubagentRegistration = {
	parentSessionId: string;
	title: string;
	agent: string;
	model?: string;
};

/**
 * 宿主为一个子智能体分好的 id 和子会话文件。子进程用 `--session <sessionFile>` 起，
 * 过程落在那里；父会话只记 id。
 */
export type RegisteredSubagent = { id: string; sessionFile: string };

/**
 * 一次 `subagent` 工具调用在会话宿主里的派发连接。连接就是这次派发的命：断开即结束，
 * 宿主把还在跑的子智能体记成已中断。`finish` 只算第一次，之后什么也不做。
 */
export type SubagentDispatch = {
	register(request: SubagentRegistration): Promise<RegisteredSubagent>;
	progress(id: string, progress: SubagentProgress): void;
	end(id: string, status: SubagentEndStatus): void;
	/** 派发结束。后台派发带上交回会话的结果；前台派发不带，结果就是工具返回值。 */
	finish(text?: string): void;
};

/** 等宿主应答最多这么久。宿主不应答时不能把派子智能体卡住。 */
const HOST_REPLY_TIMEOUT_MS = 5_000;

/**
 * 向会话宿主开一条派发连接。宿主要停某个子智能体时调 `onStop(id)`。
 *
 * 拿不到宿主（不在 Smelt 里、套接字不通）时返回 undefined：调用方照常干活，子会话不落盘，
 * 界面上也看不到。
 */
export function openSubagentDispatch(
	request: { toolCallId: string; background: boolean; title: string },
	onStop: (id: string) => void,
	socketPath = process.env[SMELT_BACKGROUND_TASK_SOCK_ENV],
): Promise<SubagentDispatch | undefined> {
	if (!socketPath) return Promise.resolve(undefined);
	return new Promise((resolve) => {
		const socket: Socket = connect(socketPath);
		let opened: SubagentDispatch | undefined;
		let settled = false;
		let ended = false;
		let buffer = "";
		let nextRef = 0;
		const pending = new Map<
			number,
			{ resolve: (value: RegisteredSubagent) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }
		>();
		const settle = (dispatch: SubagentDispatch | undefined) => {
			if (settled) return;
			settled = true;
			resolve(dispatch);
		};
		const failPending = (message: string) => {
			for (const waiting of pending.values()) {
				clearTimeout(waiting.timer);
				waiting.reject(new Error(message));
			}
			pending.clear();
		};
		const send = (message: Record<string, unknown>) => {
			if (ended || socket.destroyed) return;
			socket.write(`${JSON.stringify(message)}\n`);
		};
		socket.setTimeout(HOST_REPLY_TIMEOUT_MS);
		socket.on("timeout", () => {
			socket.destroy();
			settle(undefined);
		});
		socket.on("data", (chunk: Buffer | string) => {
			buffer += chunk.toString();
			let newline = buffer.indexOf("\n");
			while (newline >= 0) {
				const line = buffer.slice(0, newline);
				buffer = buffer.slice(newline + 1);
				newline = buffer.indexOf("\n");
				let message: Record<string, unknown>;
				try {
					message = JSON.parse(line);
				} catch {
					continue;
				}
				if (!opened) {
					if (message.ok !== true) {
						socket.destroy();
						settle(undefined);
						return;
					}
					socket.setTimeout(0);
					opened = {
						register: (registration) =>
							new Promise((registerResolve, registerReject) => {
								if (ended || socket.destroyed) {
									registerReject(new Error("子智能体派发连接已断开"));
									return;
								}
								const ref = ++nextRef;
								const timer = setTimeout(() => {
									pending.delete(ref);
									registerReject(new Error("会话宿主没有回应子智能体登记"));
								}, HOST_REPLY_TIMEOUT_MS);
								pending.set(ref, { resolve: registerResolve, reject: registerReject, timer });
								send({ op: "start", ref, ...registration });
							}),
						progress: (id, progress) => send({ op: "progress", id, ...progress }),
						end: (id, status) => send({ op: "end", id, status }),
						finish: (text) => {
							if (ended) return;
							send(text === undefined ? { op: "finish" } : { op: "finish", text });
							ended = true;
							failPending("子智能体派发已经结束");
							socket.end();
						},
					};
					settle(opened);
					continue;
				}
				if (message.op === "started" && typeof message.ref === "number") {
					const waiting = pending.get(message.ref);
					if (!waiting) continue;
					pending.delete(message.ref);
					clearTimeout(waiting.timer);
					if (typeof message.id === "string" && typeof message.sessionFile === "string") {
						waiting.resolve({ id: message.id, sessionFile: message.sessionFile });
					} else {
						waiting.reject(new Error(typeof message.error === "string" ? message.error : "子智能体登记失败"));
					}
					continue;
				}
				if (message.op === "stop" && typeof message.id === "string") onStop(message.id);
			}
		});
		socket.on("error", () => settle(undefined));
		socket.on("close", () => {
			ended = true;
			failPending("子智能体派发连接已断开");
			settle(undefined);
		});
		send({ op: "subagent_dispatch", owner: backgroundTaskOwner(), ...request });
	});
}
