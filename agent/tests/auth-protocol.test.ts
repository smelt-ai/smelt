import { describe, expect, test } from "bun:test";
import { decodeCommand, encodeEvent, takeLines } from "../src/auth-protocol.ts";

describe("login protocol", () => {
	test("每个事件占一行，宿主可以按行切分", () => {
		const line = encodeEvent({ event: "progress", message: "换取令牌…" });
		expect(line.endsWith("\n")).toBe(true);
		expect(JSON.parse(line.trim())).toEqual({ event: "progress", message: "换取令牌…" });
	});

	test("认得回答与取消", () => {
		expect(decodeCommand('{"command":"prompt_response","id":"2","value":"code-1"}')).toEqual({
			command: "prompt_response",
			id: "2",
			value: "code-1",
		});
		expect(decodeCommand('{"command":"cancel"}')).toEqual({ command: "cancel" });
	});

	test("坏行被忽略而不是中断登录", () => {
		// 空行、半截 JSON、缺字段、未知命令都可能来自版本不一致的宿主；
		// 因为一行噪声就把用户正在做的授权掐掉，比忽略它糟得多。
		expect(decodeCommand("")).toBeUndefined();
		expect(decodeCommand("{")).toBeUndefined();
		expect(decodeCommand('{"command":"prompt_response","id":"2"}')).toBeUndefined();
		expect(decodeCommand('{"command":"restart"}')).toBeUndefined();
		expect(decodeCommand("null")).toBeUndefined();
	});

	test("跨读到一半的行留到下一块", () => {
		// 粘贴的 redirect URL 可能长过一次 read。
		const first = takeLines('{"command":"cancel"}\n{"command":"prompt_res');
		expect(first.lines).toEqual(['{"command":"cancel"}']);
		const second = takeLines(`${first.rest}ponse","id":"1","value":"x"}\n`);
		expect(second.lines.map((line) => decodeCommand(line))).toEqual([
			{ command: "prompt_response", id: "1", value: "x" },
		]);
		expect(second.rest).toBe("");
	});
});
