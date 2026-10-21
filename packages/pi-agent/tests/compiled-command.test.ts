import { describe, expect, test } from "bun:test";
import {
	SMELT_PROTOCOL_ENV,
	checksDaemonProtocol,
	parseCompiledCommand,
	protocolMismatch,
} from "../src/compiled-command.ts";

describe("compiled command router", () => {
	test("drops the bunfs entry and keeps rpc flags", () => {
		expect(parseCompiledCommand(["bun", "/$bunfs/root/smelt-pi", "--offline", "--mode", "rpc"])).toEqual({
			kind: "rpc",
			args: ["--offline", "--mode", "rpc"],
		});
	});

	test("auth and mcp drop the subcommand word", () => {
		expect(parseCompiledCommand(["bun", "/$bunfs/root/smelt-pi", "auth", "list"])).toEqual({
			kind: "auth",
			args: ["list"],
		});
		expect(
			parseCompiledCommand(["bun", "/$bunfs/root/smelt-pi", "mcp", "login", "figma", "--timeout", "30"]),
		).toEqual({ kind: "mcp", args: ["login", "figma", "--timeout", "30"] });
	});

	test("gateway and mcp-gateway are the same entry", () => {
		expect(parseCompiledCommand(["bun", "/$bunfs/root/smelt-pi", "gateway"])).toEqual({ kind: "gateway" });
		expect(parseCompiledCommand(["bun", "/src/compiled-main.ts", "mcp-gateway"])).toEqual({ kind: "gateway" });
	});

	test("a flag in argv[1] is a user argument", () => {
		expect(parseCompiledCommand(["smelt-pi", "--offline"])).toEqual({ kind: "rpc", args: ["--offline"] });
	});
});

describe("daemon protocol", () => {
	test("the same protocol starts", () => {
		expect(protocolMismatch(2, { [SMELT_PROTOCOL_ENV]: "2" })).toBeUndefined();
	});

	test("an older or newer daemon is refused with both numbers", () => {
		expect(protocolMismatch(2, { [SMELT_PROTOCOL_ENV]: "1" })).toContain("协议是 2，守护要的是 1");
		expect(protocolMismatch(2, { [SMELT_PROTOCOL_ENV]: "3" })).toContain("守护要的是 3");
	});

	test("a daemon that predates the protocol is refused", () => {
		expect(protocolMismatch(1, {})).toContain("（没给）");
	});

	// 复现：smelt-pi 自己拉起的 MCP 网关去掉了 SMELT_* 变量，校验它会让所有 stdio MCP 起不来。
	test("only entries the daemon calls check the protocol", () => {
		const parse = (...args: string[]) => parseCompiledCommand(["smelt-pi", ...args]);
		expect(checksDaemonProtocol(parse("--offline"))).toBe(true);
		expect(checksDaemonProtocol(parse("auth", "list"))).toBe(true);
		expect(checksDaemonProtocol(parse("mcp", "login", "figma"))).toBe(true);
		expect(checksDaemonProtocol(parse("gateway"))).toBe(false);
	});
});
