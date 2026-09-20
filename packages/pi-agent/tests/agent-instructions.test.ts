import { describe, expect, test } from "bun:test";
import {
	SMELT_AGENT_INSTRUCTIONS_ENV,
	consumeAgentInstructions,
} from "../src/agent-instructions.ts";

describe("AgentDefinition instructions", () => {
	test("become one Pi system-prompt argument and are not inherited by tools", () => {
		const environment: Record<string, string | undefined> = {
			[SMELT_AGENT_INSTRUCTIONS_ENV]: "  第一行\n第二行 with spaces  ",
		};
		const argv = ["bun", "src/main.ts"];

		expect(consumeAgentInstructions(environment, argv)).toBe(true);
		expect(argv).toEqual([
			"bun",
			"src/main.ts",
			"--append-system-prompt",
			"第一行\n第二行 with spaces",
		]);
		expect(environment[SMELT_AGENT_INSTRUCTIONS_ENV]).toBeUndefined();
	});

	test("empty instructions do not add a Pi argument", () => {
		const environment = { [SMELT_AGENT_INSTRUCTIONS_ENV]: " \n " };
		const argv = ["bun", "src/main.ts"];

		expect(consumeAgentInstructions(environment, argv)).toBe(false);
		expect(argv).toEqual(["bun", "src/main.ts"]);
		expect(environment[SMELT_AGENT_INSTRUCTIONS_ENV]).toBeUndefined();
	});
});
