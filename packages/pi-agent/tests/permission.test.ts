import { describe, expect, test } from "bun:test";
import {
	SMELT_PERMISSION_TITLE,
	type ToolIdentity,
	toolNeedsSmeltApproval,
} from "../src/smelt-permission.ts";

const builtin = (name: string): ToolIdentity => ({
	name,
	sourceInfo: { path: `<builtin:${name}>`, source: "builtin", scope: "temporary", origin: "top-level" },
});
const extension = (name: string, path: string): ToolIdentity => ({
	name,
	sourceInfo: {
		path,
		source: path.startsWith("<inline:") ? "inline" : "local",
		scope: "user",
		origin: "top-level",
	},
});

describe("Smelt Pi permission extension", () => {
	test("only actual built-in read-only tools bypass host approval", () => {
		for (const tool of ["read", "grep", "find", "ls"]) {
			expect(toolNeedsSmeltApproval(builtin(tool))).toBe(false);
			expect(toolNeedsSmeltApproval(extension(tool, `/tmp/${tool}.ts`))).toBe(true);
		}
		for (const tool of ["write", "edit", "bash", "powershell", "stock_trade"]) {
			expect(toolNeedsSmeltApproval(builtin(tool))).toBe(true);
		}
	});

	test("the built-in subagent extension still requires host approval", () => {
		expect(toolNeedsSmeltApproval(extension("subagent", "<inline:smelt-subagent>"))).toBe(true);
	});

	test("background task tools still require host approval", () => {
		for (const name of ["background_task", "background_task_output", "background_task_stop", "background_task_wait"]) {
			expect(toolNeedsSmeltApproval(extension(name, "<inline:smelt-background-task>"))).toBe(true);
		}
	});

	test("only the Smelt-owned canonical elicitation tool bypasses approval", () => {
		const choice = {
			question: "用哪种节奏？",
			options: ["紧凑续集", "开放番外"],
		};
		expect(
			toolNeedsSmeltApproval(
				extension("smelt_ask_user", "<inline:smelt-elicitation>"),
				choice,
			),
		).toBe(false);
		expect(
			toolNeedsSmeltApproval(extension("smelt_ask_user", "/tmp/host-spoof.ts"), choice),
		).toBe(true);
		expect(toolNeedsSmeltApproval(extension("my_quiz", "/tmp/quiz.ts"), choice)).toBe(true);
	});

	test("uses a versioned title reserved for the Rust driver", () => {
		expect(SMELT_PERMISSION_TITLE).toBe("smelt.permission.v1");
	});
});
