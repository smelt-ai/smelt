import { connect, type Socket } from "node:net";
import { Type } from "@earendil-works/pi-ai";
import { createBashTool, defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { keepsForegroundTimeout, watchHostCommand } from "./bash-run.ts";
import {
	SMELT_SUBAGENT_RESULT_CUSTOM_TYPE,
	type SubagentResultStatus,
	subagentResultMessage,
} from "./subagent/result.ts";

/** 会话宿主创建的控制套接字。任务进程不在 Pi 里。 */
export const SMELT_BACKGROUND_TASK_SOCK_ENV = "SMELT_BACKGROUND_TASK_SOCK";
/**
 * 这个 Pi 进程在宿主里的归属。会话本身不设（空串）；后台子智能体由父进程设成
 * `"<父归属>/<子智能体>"`。宿主按它隔离任务：查询、停止、完成通知都只在同一归属里，
 * 子智能体的命令不会进父会话，父会话结束子智能体时连它名下的任务一起停。
 */
export const SMELT_BACKGROUND_TASK_OWNER_ENV = "SMELT_BACKGROUND_TASK_OWNER";

export function backgroundTaskOwner(environment: NodeJS.ProcessEnv = process.env): string {
	return environment[SMELT_BACKGROUND_TASK_OWNER_ENV]?.trim() ?? "";
}
export const SMELT_BACKGROUND_TASK_EXTENSION_NAME = "smelt-background-task";
export const SMELT_BACKGROUND_TASK_EXTENSION_PATH = `<inline:${SMELT_BACKGROUND_TASK_EXTENSION_NAME}>`;
export const SMELT_BACKGROUND_TASK_TOOL_NAME = "background_task";
export const SMELT_BACKGROUND_TASK_OUTPUT_TOOL_NAME = "background_task_output";
export const SMELT_BACKGROUND_TASK_STOP_TOOL_NAME = "background_task_stop";
export const SMELT_BACKGROUND_TASK_WAIT_TOOL_NAME = "background_task_wait";
/** 普通 bash 超过这段时间仍未结束，就留在宿主里继续跑。 */
export const AUTO_BACKGROUND_SECONDS = 60;
/** Pi 会话里这条自定义消息的类型。界面不画它，重放时还原成状态行。 */
export const SMELT_BACKGROUND_TASK_CUSTOM_TYPE = "smelt.background_task";

/**
 * `nohup` 或行尾 `&` 会脱离工具管理。返回剥掉这两处之后的命令；不是这种写法时返回 undefined。
 * `&&` 结尾不算后台。
 */
export function detachedShellCommand(command: string): string | undefined {
	const trimmed = command.trim();
	if (!trimmed) return undefined;
	let body = trimmed;
	if (/^nohup\s+/i.test(body)) {
		body = body.replace(/^nohup\s+/i, "");
	}
	const trailingBackground = /&\s*$/.test(body) && !/&&\s*$/.test(body);
	if (!/^nohup\s+/i.test(trimmed) && !trailingBackground) return undefined;
	if (trailingBackground) body = body.replace(/&\s*$/, "");
	const commandOnly = body.trim();
	return commandOnly || undefined;
}

export type BackgroundHostTask = {
	id?: string;
	status?: string;
	exitCode?: number | null;
};

export type BackgroundHostResponse = {
	ok: boolean;
	text?: string;
	error?: string;
	id?: string;
	status?: string;
	exitCode?: number | null;
	timedOut?: boolean;
	tasks?: BackgroundHostTask[];
	output?: string;
	nextOffset?: number;
	endOfLog?: boolean;
};

type HostRequest = Record<string, unknown>;

export type TaskCompletionNotice = {
	id: string;
	/** `command` 是宿主起的命令，`subagent` 是挂接的后台子智能体交回的结果。 */
	kind: "command" | "subagent";
	title: string;
	status: string;
	exitCode: number | null;
	output: string;
	outputTail: string;
	wake: boolean;
};

/**
 * 写给模型的结果。这句话要说明是命令结束，不是用户又提了一个要求。
 * 输出用宿主给的尾部，退出码单独写明。
 */
export function taskNotificationText(notice: Pick<TaskCompletionNotice, "id" | "title" | "status" | "exitCode" | "output">): string {
	const title = notice.title ? `（${notice.title}）` : "";
	const code = notice.exitCode == null ? "无" : String(notice.exitCode);
	const lines = [
		"<task-notification>",
		`这是已结束命令的结果，不是用户的新请求。后台任务 ${notice.id}${title} 已结束，状态 ${notice.status}，退出码 ${code}。`,
	];
	const output = notice.output.trim();
	if (output) {
		lines.push("<output>", output, "</output>");
	}
	lines.push("</task-notification>");
	return lines.join("\n");
}

/** 只决定要不要开一轮。不传 deliverAs，避免落到 followUp 队列。 */
export function completionSendOptions(wake: boolean): { triggerTurn: boolean } {
	return { triggerTurn: wake };
}

function parseCompletion(value: unknown): TaskCompletionNotice | undefined {
	if (!value || typeof value !== "object") return undefined;
	const row = value as Record<string, unknown>;
	if (row.op !== "completion" || typeof row.id !== "string" || !row.id) return undefined;
	return {
		id: row.id,
		kind: row.kind === "subagent" ? "subagent" : "command",
		title: typeof row.title === "string" ? row.title : "",
		status: typeof row.status === "string" ? row.status : "",
		exitCode: typeof row.exitCode === "number" ? row.exitCode : null,
		output: typeof row.output === "string" ? row.output : "",
		outputTail: typeof row.outputTail === "string" ? row.outputTail : "",
		wake: row.wake === true,
	};
}

/** 宿主交回的结果写成会话里的哪条消息、怎么送。命令和后台子智能体各用自己的消息类型。 */
export function completionMessage(notice: TaskCompletionNotice): {
	message: Parameters<ExtensionAPI["sendMessage"]>[0];
	options: Parameters<ExtensionAPI["sendMessage"]>[1];
} {
	if (notice.kind === "subagent") {
		const status: SubagentResultStatus =
			notice.status === "completed" ? "completed" : notice.status === "stopped" ? "stopped" : "failed";
		return {
			message: subagentResultMessage({ id: notice.id, title: notice.title, status, text: notice.output }),
			// 子智能体的结果等这一轮做完再看，不打断手上的工具调用。
			options: { triggerTurn: notice.wake, deliverAs: "followUp" },
		};
	}
	return {
		message: {
			customType: SMELT_BACKGROUND_TASK_CUSTOM_TYPE,
			content: [{ type: "text", text: taskNotificationText(notice) }],
			display: false,
			details: {
				id: notice.id,
				title: notice.title,
				status: notice.status,
				exitCode: notice.exitCode,
				outputTail: notice.outputTail,
			},
		},
		options: completionSendOptions(notice.wake),
	};
}

/** 宿主等 ack 的那两类结果消息。Pi 把它交给模型时，扩展能看到它的 `message_start`。 */
export function deliveredResultId(message: unknown): string | undefined {
	const row = message as { role?: unknown; customType?: unknown; details?: { id?: unknown } } | undefined;
	if (row?.role !== "custom") return undefined;
	if (row.customType !== SMELT_BACKGROUND_TASK_CUSTOM_TYPE && row.customType !== SMELT_SUBAGENT_RESULT_CUSTOM_TYPE) {
		return undefined;
	}
	return typeof row.details?.id === "string" && row.details.id ? row.details.id : undefined;
}

/** 告诉宿主这条已经交给模型。连接不在时，宿主会把它交给下一条连接，到时再认一次。 */
function acknowledgeResult(state: CompletionListener, id: string): void {
	state.consumed.add(id);
	const socket = state.socket;
	if (socket && !socket.destroyed) socket.write(`${JSON.stringify({ op: "ack", id })}\n`);
}

function deliverCompletion(pi: ExtensionAPI, state: CompletionListener, notice: TaskCompletionNotice): void {
	// 已经交给过模型、ack 没到宿主（连接断了）的那条，宿主会再写一次。只补 ack。
	if (state.consumed.has(notice.id)) {
		acknowledgeResult(state, notice.id);
		return;
	}
	const { message, options } = completionMessage(notice);
	try {
		// 同一批里连续调用，不等待。第一条会同步进入运行，后面的跟进当前这一轮。
		pi.sendMessage(message, options);
	} catch {
		// 会话已经换掉（fork、reload），旧的 pi 不能再用。不 ack，宿主交给新会话的监听。
		return;
	}
	// 不开回合的结果直接写进会话，不进 Pi 的队列，`clear_queue` 清不掉它，这时就算收下。
	// 开回合的要么进队列、要么开一轮，等它的 `message_start` 再认。
	if (!options?.triggerTurn) acknowledgeResult(state, notice.id);
}

type CompletionListener = {
	socket?: Socket;
	closed: boolean;
	/** 已经交给模型的结果。宿主重写时只补 ack，不再送。 */
	consumed: Set<string>;
};

/** 已经收下 ack 的连接断开，并且扩展还在，就由新连接接着收还没写到的结果。没连上过就停。 */
export function reconnectAfterClose(closed: boolean, acknowledged: boolean): boolean {
	return !closed && acknowledged;
}

function ensureCompletionListener(pi: ExtensionAPI, state: CompletionListener): void {
	if (state.closed) return;
	if (state.socket && !state.socket.destroyed) return;
	const path = process.env[SMELT_BACKGROUND_TASK_SOCK_ENV];
	if (!path) return;
	const socket = connect(path);
	state.socket = socket;
	let buffer = "";
	let acknowledged = false;
	let finished = false;
	const stop = () => {
		if (finished) return;
		finished = true;
		if (state.socket === socket) state.socket = undefined;
		if (!reconnectAfterClose(state.closed, acknowledged)) return;
		queueMicrotask(() => ensureCompletionListener(pi, state));
	};
	socket.on("connect", () => {
		socket.write(`${JSON.stringify({ op: "listen", owner: backgroundTaskOwner() })}\n`);
	});
	socket.on("data", (chunk: Buffer | string) => {
		buffer += chunk.toString();
		const lines = buffer.split("\n");
		buffer = lines.pop() ?? "";
		const batch: TaskCompletionNotice[] = [];
		for (const line of lines) {
			if (!line.trim()) continue;
			let parsed: unknown;
			try {
				parsed = JSON.parse(line);
			} catch {
				continue;
			}
			const row = parsed && typeof parsed === "object" ? parsed as Record<string, unknown> : undefined;
			if (row?.ok === true && row.op !== "completion") {
				acknowledged = true;
				continue;
			}
			const notice = parseCompletion(parsed);
			if (notice) batch.push(notice);
		}
		for (const notice of batch) deliverCompletion(pi, state, notice);
	});
	socket.on("error", () => {
		socket.destroy();
	});
	socket.on("close", () => stop());
}

/** 向会话宿主发一条请求。套接字不在时说明这轮 Pi 不是宿主拉起的。 */
export function callBackgroundHost(
	socketPath: string | undefined,
	request: HostRequest,
	signal?: AbortSignal,
): Promise<BackgroundHostResponse> {
	if (!socketPath) {
		return Promise.resolve({ ok: false, error: "后台任务服务还没就绪" });
	}
	if (signal?.aborted) {
		return Promise.resolve({ ok: false, error: "操作已取消" });
	}
	return new Promise((resolve) => {
		const socket = connect(socketPath);
		let settled = false;
		const onAbort = () => {
			if (settled) return;
			settled = true;
			socket.destroy();
			resolve({ ok: false, error: "操作已取消" });
		};
		signal?.addEventListener("abort", onAbort, { once: true });
		let buffer = "";
		const finish = (response: BackgroundHostResponse) => {
			if (settled) return;
			settled = true;
			signal?.removeEventListener("abort", onAbort);
			socket.removeAllListeners();
			socket.end();
			resolve(response);
		};
		socket.setTimeout(60 * 60 * 1000);
		socket.on("data", (chunk: Buffer | string) => {
			buffer += chunk.toString();
			const newline = buffer.indexOf("\n");
			if (newline < 0) return;
			try {
				const parsed = JSON.parse(buffer.slice(0, newline)) as BackgroundHostResponse;
				finish(parsed && typeof parsed.ok === "boolean" ? parsed : { ok: false, error: "宿主返回了无效结果" });
			} catch {
				finish({ ok: false, error: "宿主返回了无效结果" });
			}
		});
		socket.on("timeout", () => finish({ ok: false, error: "后台任务请求超时" }));
		socket.on("error", (error) => finish({ ok: false, error: error.message }));
		socket.on("close", () => finish({ ok: false, error: "后台任务连接已断开" }));
		socket.write(`${JSON.stringify({ owner: backgroundTaskOwner(), ...request })}\n`);
	});
}

/**
 * 子智能体结束：停掉它（及它的后代）名下还在跑的任务，结果不再送给任何人。
 * 只能结束自己的后代，宿主会拒绝越级。
 */
export function endBackgroundTaskOwner(target: string): Promise<BackgroundHostResponse> {
	return callBackgroundHost(process.env[SMELT_BACKGROUND_TASK_SOCK_ENV], { op: "end_owner", target });
}

/**
 * 挂接在宿主里的一个后台任务。`finish` 只算第一次，之后的调用什么也不做。
 * 带上 `result` 时宿主把它交回会话：和后台命令的完成走同一条监听，等 Pi 真交给模型才算送到，
 * 会话换了（fork）也交给新会话。
 */
export type AttachedBackgroundTask = {
	id: string;
	progress(text: string): void;
	finish(status: "completed" | "failed" | "stopped", result?: string): void;
};

/** 等宿主回 id 最多这么久。宿主不应答时不能把派子智能体卡住。 */
const ATTACH_ACK_TIMEOUT_MS = 5_000;

/**
 * 把不由宿主启动的后台工作（后台子智能体）登记进宿主，让它和后台命令一样出现在会话的
 * 后台任务列表里，模型也能用 list / stop 管它。这条连接就是它的命：断开即结束，Pi 被杀
 * 也不会留下一个永远在跑的假任务。宿主要停它时（模型调 stop、会话结束）调用 `onStop`。
 *
 * 拿不到宿主（不在 Smelt 里、套接字不通）时返回 undefined：调用方照常干活，只是看不到。
 */
export function attachBackgroundTask(
	request: { title: string; command: string; toolCallId?: string },
	onStop: () => void,
	socketPath = process.env[SMELT_BACKGROUND_TASK_SOCK_ENV],
): Promise<AttachedBackgroundTask | undefined> {
	if (!socketPath) return Promise.resolve(undefined);
	return new Promise((resolve) => {
		const socket: Socket = connect(socketPath);
		let attached: AttachedBackgroundTask | undefined;
		let settled = false;
		let ended = false;
		let buffer = "";
		const settle = (task: AttachedBackgroundTask | undefined) => {
			if (settled) return;
			settled = true;
			resolve(task);
		};
		const send = (message: Record<string, unknown>) => {
			if (ended || socket.destroyed) return;
			socket.write(`${JSON.stringify(message)}\n`);
		};
		socket.setTimeout(ATTACH_ACK_TIMEOUT_MS);
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
				let message: { ok?: unknown; id?: unknown; op?: unknown };
				try {
					message = JSON.parse(line);
				} catch {
					continue;
				}
				if (!attached) {
					if (message.ok !== true || typeof message.id !== "string") {
						socket.destroy();
						settle(undefined);
						return;
					}
					socket.setTimeout(0);
					attached = {
						id: message.id,
						progress: (text) => send({ op: "progress", text }),
						finish: (status, result) => {
							send(result === undefined ? { op: "finish", status } : { op: "finish", status, result });
							ended = true;
							socket.end();
						},
					};
					settle(attached);
					continue;
				}
				if (message.op === "stop") onStop();
			}
		});
		socket.on("error", () => settle(undefined));
		socket.on("close", () => settle(undefined));
		send({ op: "attach", owner: backgroundTaskOwner(), ...request });
	});
}

function hostText(response: BackgroundHostResponse): { content: [{ type: "text"; text: string }]; details: { error: boolean }; isError: boolean } {
	const error = !response.ok;
	return {
		content: [{ type: "text" as const, text: error ? response.error || "后台任务失败" : response.text || "" }],
		details: { error },
		isError: error,
	};
}

function registerBackgroundTools(pi: ExtensionAPI): void {
	let configured: ReturnType<ExtensionAPI["getAllTools"]>;
	try {
		configured = pi.getAllTools();
	} catch {
		return;
	}
	const names = new Set(configured.map((tool) => tool.name));
	const socket = () => process.env[SMELT_BACKGROUND_TASK_SOCK_ENV];
	// 子智能体这一轮做完，父进程就结束它，它名下的后台任务跟着被停掉。所以子智能体里
	// 命令一律前台跑完，也不提供后台启动；不然跑长测试转到后台，结果永远等不回来。
	const inSubagent = backgroundTaskOwner() !== "";
	if (!inSubagent && !names.has(SMELT_BACKGROUND_TASK_TOOL_NAME)) {
		pi.registerTool(defineTool({
			name: SMELT_BACKGROUND_TASK_TOOL_NAME,
			label: "后台任务",
			description:
				"在会话宿主里启动一条 shell 命令并立即返回任务 id。命令不会被这一轮或 /reload 停掉。不要用来跑需要交互输入的命令。",
			promptSnippet: "Start a shell command in the session host and return immediately",
			promptGuidelines: [
				"Use background_task when a command should keep running after this turn.",
				"background_task_wait reports the current status and returns immediately. Completion arrives later.",
				"Use background_task_output with offset and nextOffset to page logs.",
				"Use background_task_stop to stop it. Do not use it for interactive prompts.",
			],
			parameters: Type.Object({
				command: Type.String({ description: "要在后台执行的 shell 命令" }),
				title: Type.Optional(Type.String({ description: "对话页上显示的短标题" })),
				cwd: Type.Optional(Type.String({ description: "工作目录，默认当前会话目录" })),
				timeoutSeconds: Type.Optional(Type.Number({ description: "到点后停止任务。不填则一直跑到会话结束" })),
			}),
			async execute(_toolCallId, params, signal) {
				return hostText(await callBackgroundHost(socket(), {
					op: "start",
					command: params.command,
					title: params.title,
					cwd: params.cwd,
					timeoutSeconds: params.timeoutSeconds,
				}, signal));
			},
		}));
	}
	if (!names.has(SMELT_BACKGROUND_TASK_OUTPUT_TOOL_NAME)) {
		pi.registerTool(defineTool({
			name: SMELT_BACKGROUND_TASK_OUTPUT_TOOL_NAME,
			label: "后台输出",
			description: "按字节偏移读取一个后台任务的日志。用返回的 nextOffset 翻页，单次最多 64 KiB。",
			parameters: Type.Object({
				id: Type.String({ description: "任务 id，例如 bg-3f9a2c71" }),
				offset: Type.Optional(Type.Number({ description: "起始字节，默认 0" })),
				limitBytes: Type.Optional(Type.Number({ description: "本次最多读取的字节数，默认 16384" })),
			}),
			async execute(_toolCallId, params, signal) {
				return hostText(await callBackgroundHost(socket(), {
					op: "output",
					id: params.id,
					offset: params.offset,
					limitBytes: params.limitBytes,
				}, signal));
			},
		}));
	}
	if (!names.has(SMELT_BACKGROUND_TASK_STOP_TOOL_NAME)) {
		pi.registerTool(defineTool({
			name: SMELT_BACKGROUND_TASK_STOP_TOOL_NAME,
			label: "停止后台任务",
			description: "停止一个正在运行的后台任务。",
			parameters: Type.Object({
				id: Type.String({ description: "要停止的任务 id" }),
			}),
			async execute(_toolCallId, params, signal) {
				return hostText(await callBackgroundHost(socket(), { op: "stop", id: params.id }, signal));
			},
		}));
	}
	const bash = configured.find((tool) => tool.name === "bash");
	if (!bash || bash.sourceInfo?.path !== SMELT_BACKGROUND_TASK_EXTENSION_PATH) {
		pi.registerTool(defineTool({
			name: "bash",
			label: "bash",
			description: inSubagent
				? "在当前目录执行 shell 命令，等它结束后返回输出。nohup 或末尾 & 会转入后台并返回任务 id。"
				: "在当前目录执行 shell 命令。不超过 60 秒的命令沿用 Pi 的执行器并返回输出；更久、nohup 或末尾 & 会转入后台并返回任务 id。",
			promptSnippet: "Execute bash commands (ls, grep, find, etc.)",
			promptGuidelines: inSubagent
				? [
						"Use bash for ordinary commands. Output comes back when the command finishes, however long it takes.",
						"Do not append & or use nohup. Those start as background tasks, which stop when you finish.",
					]
				: [
						"Use bash for ordinary commands. Output comes back when the command finishes within 60 seconds.",
						"A timeout of 60 seconds or less stops the command. A longer timeout, or no timeout, moves it to the background at 60 seconds.",
						"Do not append & or use nohup. Those start as background tasks immediately.",
						"Use background_task_output, background_task_wait, and background_task_stop with a returned task id.",
					],
			parameters: Type.Object({
				command: Type.String({ description: "Shell command to execute" }),
				timeout: Type.Optional(
					Type.Number({
						description: inSubagent
							? "Timeout in seconds. The command stops when it runs out."
							: "Timeout in seconds. At most 60 stops the command; above that it continues in the background.",
					}),
				),
			}),
			async execute(toolCallId, params, signal, onUpdate, ctx) {
				const command = String(params.command ?? "");
				const detached = detachedShellCommand(command);
				if (detached) {
					const started = await callBackgroundHost(socket(), {
						op: "start",
						command: detached,
						cwd: ctx.cwd,
						title: detached.slice(0, 80),
					}, signal);
					if (!started.ok || !started.id) return hostText(started.id ? started : { ...started, error: started.error || started.text });
					return hostText({ ok: true, id: started.id, text: `已转入后台 ${started.id}。nohup 和末尾 & 不会脱离管理。` });
				}
				const timeout = typeof params.timeout === "number" ? params.timeout : undefined;
				if (inSubagent || keepsForegroundTimeout(timeout)) {
					return createBashTool(ctx.cwd).execute(toolCallId, { command, timeout }, signal, onUpdate);
				}
				const ran = await watchHostCommand({
					signal,
					start: async () => {
						const started = await callBackgroundHost(socket(), {
							op: "start",
							command,
							cwd: ctx.cwd,
							title: command.slice(0, 80),
							timeoutSeconds: timeout,
							watch: true,
						}, signal);
						return { ok: started.ok, id: started.id, error: started.error || started.text };
					},
					release: async (id, consumed) => {
						await callBackgroundHost(socket(), { op: "unwatch", id, consumed });
					},
					until: async (id, timeoutMs) => {
						const status = await callBackgroundHost(socket(), {
							op: "wait",
							id,
							...(timeoutMs > 0 ? { timeoutMs } : {}),
						}, signal);
						const task = status.tasks?.find((item) => item.id === id) ?? status.tasks?.[0];
						if (!status.ok || !task || task.status === "running") {
							onUpdate?.({ content: [{ type: "text", text: status.text || "" }], details: undefined });
							return { ok: status.ok, status: task?.status, exitCode: task?.exitCode, error: status.error };
						}
						const output = await callBackgroundHost(socket(), { op: "output", id, offset: 0 }, signal);
						return {
							ok: output.ok,
							status: task.status,
							exitCode: task.exitCode ?? output.exitCode,
							output: output.output || output.text,
							error: output.error,
						};
					},
				});
				if (!ran.promoted) {
					if (signal?.aborted) return hostText({ ok: false, error: "命令已取消" });
					if (ran.exitCode === 0) {
						return { content: [{ type: "text" as const, text: ran.output }], details: { error: false } };
					}
					const code = ran.exitCode === null ? "无" : String(ran.exitCode);
					return {
						content: [{ type: "text" as const, text: `${ran.output}\n\nCommand exited with code ${code}` }],
						details: { error: true },
						isError: true,
					};
				}
				return hostText({
					ok: true,
					id: ran.id,
					text: `命令仍在运行，已转入后台 ${ran.id}。用 background_task_output 查看输出。`,
				});
			},
		}));
	}
	if (!names.has(SMELT_BACKGROUND_TASK_WAIT_TOOL_NAME)) {
		pi.registerTool(defineTool({
			name: SMELT_BACKGROUND_TASK_WAIT_TOOL_NAME,
			label: "等待后台任务",
			description: "立刻返回后台任务的当前状态。任务结束时宿主会另行通知，这一次调用不会等待。",
			parameters: Type.Object({
				id: Type.Optional(Type.String({ description: "单个任务 id" })),
				ids: Type.Optional(Type.Array(Type.String(), { description: "多个任务 id" })),
			}),
			async execute(_toolCallId, params, signal) {
				return hostText(await callBackgroundHost(socket(), {
					op: "wait",
					id: params.id,
					ids: params.ids,
				}, signal));
			},
		}));
	}
}

export function smeltBackgroundTaskExtension(pi: ExtensionAPI): void {
	registerBackgroundTools(pi);
	const listener: CompletionListener = { closed: false, consumed: new Set() };
	ensureCompletionListener(pi, listener);
	// 工具表在工厂执行时可能还没绑上。session_start 再注册一次。
	// 这里不停任务：进程在会话宿主里，/reload 只重建扩展。
	pi.on("session_start", () => {
		registerBackgroundTools(pi);
		ensureCompletionListener(pi, listener);
	});
	// Pi 把排队或开回合送出的结果交给模型了，这时才告诉宿主收下。
	pi.on("message_start", (event) => {
		const id = deliveredResultId(event.message);
		if (id) acknowledgeResult(listener, id);
	});
	pi.on("session_shutdown", () => {
		listener.closed = true;
		listener.socket?.destroy();
		listener.socket = undefined;
	});
	pi.on("user_bash", async (event) => {
		const detached = detachedShellCommand(event.command);
		if (!detached) return;
		const started = await callBackgroundHost(process.env[SMELT_BACKGROUND_TASK_SOCK_ENV], {
			op: "start",
			command: detached,
			cwd: event.cwd,
			title: detached.slice(0, 80),
		});
		const output = started.ok && started.id
			? `已转入后台 ${started.id}。nohup 和末尾 & 不会脱离管理。`
			: started.error || "无法转入后台";
		return {
			result: {
				output,
				exitCode: started.ok && started.id ? 0 : 1,
				cancelled: false,
				truncated: false,
			},
		};
	});
}

export default smeltBackgroundTaskExtension;
