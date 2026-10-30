import { describe, expect, test } from "bun:test";
import {
	SMELT_SESSION_CONTROL_ENV_NAMES,
	stripSessionControlFromSpawnEnv,
	withoutSessionControlEnv,
} from "../src/session-control-env.ts";

describe("session control env stripping", () => {
	test("bash/MCP children do not inherit session socket or token", () => {
		const env = {
			PATH: "/bin",
			SMELT_HOME: "/home/me/.smelt",
			SMELT_SESSION_ID: "acp-1",
			SMELT_AGENT_TOKEN: "secret",
			SMELT_SOCK: "/tmp/smeltd.sock",
			SMELT_BACKGROUND_TASK_SOCK: "/tmp/bg.sock",
			SMELT_OTHER: "gone",
		};
		const stripped = withoutSessionControlEnv(env);
		for (const name of SMELT_SESSION_CONTROL_ENV_NAMES) {
			expect(stripped[name]).toBeUndefined();
		}
		expect(stripped.SMELT_OTHER).toBeUndefined();
		expect(stripped.PATH).toBe("/bin");
		expect(stripped.SMELT_HOME).toBe("/home/me/.smelt");
		expect(stripSessionControlFromSpawnEnv(env).SMELT_AGENT_TOKEN).toBeUndefined();
	});
});
