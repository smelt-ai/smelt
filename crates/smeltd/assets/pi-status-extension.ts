import { spawn } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const NOTIFY_BIN = process.env.SMELT_NOTIFY_BIN;
const ENABLED = Boolean(
	NOTIFY_BIN && process.env.SMELT_SESSION_ID && process.env.SMELT_SOCK,
);
const REPORT_TIMEOUT_MS = 2500;

type HookPayload = Record<string, unknown>;

function invokeNotify(eventName: string, payload: HookPayload): Promise<void> {
	if (!ENABLED || !NOTIFY_BIN) return Promise.resolve();

	let input: string;
	try {
		input = JSON.stringify({ ...payload, hook_event_name: eventName });
	} catch {
		return Promise.resolve();
	}

	return new Promise((resolve) => {
		let finished = false;
		let timer: ReturnType<typeof setTimeout> | undefined;
		const finish = () => {
			if (finished) return;
			finished = true;
			if (timer) clearTimeout(timer);
			resolve();
		};

		let child: ReturnType<typeof spawn>;
		try {
			child = spawn(NOTIFY_BIN, [], {
				env: {
					...process.env,
					SMELT_HOOK_PROVIDER: "pi",
					SMELT_HOOK_EVENT: eventName,
				},
				stdio: ["pipe", "ignore", "ignore"],
			});
		} catch {
			finish();
			return;
		}

		timer = setTimeout(() => {
			child.kill();
			finish();
		}, REPORT_TIMEOUT_MS);
		child.once("error", finish);
		child.once("close", finish);
		child.stdin.on("error", () => {});
		child.stdin.end(input);
	});
}

export default function (pi: ExtensionAPI) {
	let queue: Promise<void> = Promise.resolve();
	let pendingPrompt: string | undefined;
	let lastStopReason: string | undefined;

	function report(eventName: string, payload: HookPayload = {}): Promise<void> {
		if (!ENABLED) return Promise.resolve();
		const next = queue.then(() => invokeNotify(eventName, payload));
		queue = next.catch(() => {});
		return next;
	}

	pi.on("session_start", async () => {
		await report("SessionStart");
		const name = pi.getSessionName();
		if (name) await report("SessionTitleChanged", { title: name });
	});

	pi.on("session_info_changed", async (event) => {
		if (event.name) {
			await report("SessionTitleChanged", { title: event.name });
		}
	});

	pi.on("before_agent_start", (event) => {
		pendingPrompt = event.prompt;
	});

	pi.on("agent_start", async () => {
		lastStopReason = undefined;
		const prompt = pendingPrompt;
		pendingPrompt = undefined;
		await report("UserPromptSubmit", prompt ? { prompt } : {});
	});

	pi.on("tool_execution_start", async (event) => {
		await report("PreToolUse", {
			tool_name: event.toolName,
			tool_use_id: event.toolCallId,
			tool_input: event.args,
		});
	});

	pi.on("tool_execution_end", async (event) => {
		await report(event.isError ? "PostToolUseFailure" : "PostToolUse", {
			tool_name: event.toolName,
			tool_use_id: event.toolCallId,
			error: event.isError ? "Pi tool execution failed" : undefined,
		});
	});

	pi.on("agent_end", (event) => {
		lastStopReason = finalAssistantStopReason(event.messages);
	});

	pi.on("agent_settled", async () => {
		const reason = lastStopReason;
		lastStopReason = undefined;
		if (reason && reason !== "stop" && reason !== "toolUse") {
			await report("StopFailure", { error_type: reason });
		} else {
			await report("Stop");
		}
	});

	pi.on("session_shutdown", async (event) => {
		if (event.reason === "quit") await report("SessionEnd");
	});
}

function finalAssistantStopReason(messages: readonly unknown[]): string | undefined {
	for (let index = messages.length - 1; index >= 0; index--) {
		const message = messages[index] as {
			role?: unknown;
			stopReason?: unknown;
		};
		if (message?.role === "assistant" && typeof message.stopReason === "string") {
			return message.stopReason;
		}
	}
	return undefined;
}
