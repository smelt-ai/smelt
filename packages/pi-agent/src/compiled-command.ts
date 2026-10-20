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

export function parseCompiledCommand(argv: readonly string[]): CompiledCommand {
	const args = userArguments(argv);
	const [head, ...rest] = args;
	if (head === "auth") return { kind: "auth", args: rest };
	if (head === "mcp") return { kind: "mcp", args: rest };
	// 设计里的名字是 gateway。入口文件叫 mcp-gateway-main，两个词进同一个进程。
	if (head === "gateway" || head === "mcp-gateway") return { kind: "gateway" };
	return { kind: "rpc", args };
}
