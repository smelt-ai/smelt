import { describe, expect, test } from "bun:test";
import { nextActiveTools } from "../src/codemode.ts";

describe("codemode active tools", () => {
	test("enabling adds codemode without dropping other tools", () => {
		expect(nextActiveTools(["read", "bash"], ["read", "bash", "codemode"], true)).toEqual([
			"read",
			"bash",
			"codemode",
		]);
	});

	test("enabling is idempotent", () => {
		expect(nextActiveTools(["read", "codemode"], ["read", "codemode"], true)).toEqual([
			"read",
			"codemode",
		]);
	});

	test("disabling removes only codemode", () => {
		expect(
			nextActiveTools(["read", "codemode", "bash"], ["read", "codemode", "bash"], false),
		).toEqual(["read", "bash"]);
	});

	test("enabling fails when the tool is not installed", () => {
		expect(() => nextActiveTools(["read"], ["read", "bash"], true)).toThrow("codemode");
	});
});
