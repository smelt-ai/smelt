// 假的 Pi RPC 子进程，给子智能体测试用。配置是 argv[2] 的 JSON，之后的参数（真 Pi 的命令行）不看。
// 收到 prompt 后一轮接一轮地发「还要调工具」的助手消息，直到轮数用完、被要求收尾（除非
// ignoreSteer）或被中止；stdin 关闭就退出。
import { appendFileSync } from "node:fs";

type Config = {
	/** 每收到一条命令、启动时各记一行。 */
	log?: string;
	/** 跑到这一轮自己收尾。 */
	turns?: number;
	/** 被要求收尾也不理。 */
	ignoreSteer?: boolean;
	/** 启动时往 stderr 写这么多个 x，再写 END。 */
	stderrChars?: number;
	/** 从这一轮起每轮停 10ms，好让父进程的插话在下一轮之前到。 */
	slowAfter?: number;
	/** 启动时把真 Pi 的命令行参数记一行 `args <json>`。 */
	logArgs?: boolean;
	/** 每轮先调一次 bash 工具（发 `tool_execution_start`）。 */
	toolCalls?: boolean;
};

const config = JSON.parse(process.argv[2] ?? "{}") as Config;
const log = (line: string) => {
	if (config.log) appendFileSync(config.log, `${line}\n`);
};
const send = (record: unknown) => process.stdout.write(`${JSON.stringify(record)}\n`);
const message = (stopReason: string, text: string) => ({
	role: "assistant",
	content: [{ type: "text", text }],
	stopReason,
	usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { total: 0 } },
});

log("start");
if (config.logArgs) log(`args ${JSON.stringify(process.argv.slice(3))}`);
if (config.stderrChars) process.stderr.write(`${"x".repeat(config.stderrChars)}END`);

let steered = false;
let aborted = false;

async function run(): Promise<void> {
	let turn = 0;
	while (true) {
		const pause = turn >= (config.slowAfter ?? Number.POSITIVE_INFINITY) ? 10 : 0;
		await new Promise((resolve) => setTimeout(resolve, pause));
		if (aborted) {
			send({ type: "message_end", message: message("aborted", "aborted") });
			break;
		}
		turn += 1;
		if (config.toolCalls) {
			send({ type: "tool_execution_start", toolCallId: `t${turn}`, toolName: "bash", args: { command: `echo ${turn}` } });
		}
		const finishing = turn >= (config.turns ?? Number.POSITIVE_INFINITY) || (steered && !config.ignoreSteer);
		send({ type: "message_end", message: message(finishing ? "stop" : "toolUse", `turn ${turn}`) });
		if (finishing) break;
	}
	send({ type: "agent_settled" });
}

function handle(command: { id?: string; type: string }): void {
	log(command.type);
	const respond = () => send({ type: "response", id: command.id, command: command.type, success: true });
	switch (command.type) {
		case "get_state":
			respond();
			return;
		case "prompt":
			respond();
			void run();
			return;
		case "steer":
			steered = true;
			respond();
			return;
		case "abort":
			aborted = true;
			respond();
			return;
	}
}

let buffer = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk: string) => {
	buffer += chunk;
	const lines = buffer.split("\n");
	buffer = lines.pop() ?? "";
	for (const line of lines) if (line.trim()) handle(JSON.parse(line));
});
process.stdin.on("end", () => process.exit(0));
