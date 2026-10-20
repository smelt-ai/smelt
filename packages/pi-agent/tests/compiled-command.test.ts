import { describe, expect, test } from "bun:test";
import { parseCompiledCommand } from "../src/compiled-command.ts";

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
