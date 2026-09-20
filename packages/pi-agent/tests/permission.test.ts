import { describe, expect, test } from "bun:test";
import { SMELT_PERMISSION_TITLE, toolNeedsSmeltApproval } from "../src/smelt-permission.ts";

describe("Smelt Pi permission extension", () => {
	test("only known read-only tools bypass host approval", () => {
		for (const tool of ["read", "grep", "find", "ls"]) {
			expect(toolNeedsSmeltApproval(tool)).toBe(false);
		}
		for (const tool of ["write", "edit", "bash", "powershell", "stock_trade"]) {
			expect(toolNeedsSmeltApproval(tool)).toBe(true);
		}
	});

	test("skips approval when the payload is a choice form, regardless of tool name", () => {
		const choice = {
			question: "用哪种节奏？",
			options: ["紧凑续集", "开放番外"],
		};
		for (const name of ["question", "questionnaire", "ask_user_question", "AskUserQuestion", "my_quiz"]) {
			expect(toolNeedsSmeltApproval(name, choice)).toBe(false);
		}
		expect(toolNeedsSmeltApproval("my_quiz", { symbol: "NIO" })).toBe(true);
		expect(toolNeedsSmeltApproval("bash", choice)).toBe(true);
	});

	test("uses a versioned title reserved for the Rust driver", () => {
		expect(SMELT_PERMISSION_TITLE).toBe("smelt.permission.v1");
	});
});
