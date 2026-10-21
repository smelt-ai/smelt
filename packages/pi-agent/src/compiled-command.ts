/** 编译产物 `smelt-pi` 的子命令。没有子命令时走现在的 RPC 入口。 */
export type CompiledCommand =
	| { kind: "rpc"; args: string[] }
	| { kind: "auth"; args: string[] }
	| { kind: "mcp"; args: string[] }
	| { kind: "gateway" };

function isCompileEntry(value: string | undefined): boolean {
	if (!value) return false;
	return (
		value.startsWith("/$bunfs/") ||
		value.includes("compiled-main.ts") ||
		value.includes("compiled-main.js")
	);
}

/** 丢掉 argv0，以及 bun compile 插进来的 `/$bunfs/root/...` 虚拟脚本。 */
export function userArguments(argv: readonly string[]): string[] {
	const rest = argv.slice(1);
	if (isCompileEntry(rest[0])) return rest.slice(1);
	return rest;
}

/** 守护起 smelt-pi 时用这个环境变量告诉它自己的握手约定版本。 */
export const SMELT_PROTOCOL_ENV = "SMELT_PI_PROTOCOL";

/**
 * smelt-pi 和守护分开发布：App 升级后、守护交接前，会是旧守护起新 Pi。约定对不上时
 * 返回要写进 stderr 的说明，不让握手莫名失败。改了守护和 Pi 之间的约定（RPC 扩展命令、
 * 握手、auth / mcp 子命令的输出），就把 package.json 的 smeltProtocol 加一。
 */
export function protocolMismatch(
	own: number,
	env: Readonly<Record<string, string | undefined>>,
): string | undefined {
	const wanted = env[SMELT_PROTOCOL_ENV];
	if (wanted === String(own)) return undefined;
	return `smelt-pi 的协议是 ${own}，守护要的是 ${wanted ?? "（没给）"}。重新打开 Smelt，让守护换到同一版本。`;
}

/**
 * 约定版本管的是守护调用 smelt-pi 的接口，所以只有守护直接调用的入口（RPC 会话、auth、mcp）
 * 才校验。`gateway` 是 smelt-pi 自己拉起的同一份二进制，没有版本错配可言；它的环境也按
 * MCP server 的规矩去掉了 SMELT_* 变量，本来就拿不到守护给的版本。
 */
export function checksDaemonProtocol(command: CompiledCommand): boolean {
	return command.kind !== "gateway";
}

export function parseCompiledCommand(argv: readonly string[]): CompiledCommand {
	const args = userArguments(argv);
	const [head, ...rest] = args;
	if (head === "auth") return { kind: "auth", args: rest };
	if (head === "mcp") return { kind: "mcp", args: rest };
	// 设计里的名字是 gateway。入口文件叫 mcp-gateway-main，两个词进同一个进程。
	if (head === "gateway" || head === "mcp-gateway") return { kind: "gateway" };
	return { kind: "rpc", args };
}
