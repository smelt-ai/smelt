import { describe, expect, test } from "bun:test";
import {
	SMELT_BACKGROUND_TASK_OWNER_ENV,
	SMELT_BACKGROUND_TASK_SOCK_ENV,
	backgroundTaskOwner,
} from "../src/background-task.ts";
import { childTaskOwner, subagentEnvironment } from "../src/subagent/index.ts";
import {
	SMELT_SUBAGENT_RESULT_CUSTOM_TYPE,
	subagentResultMessage,
	subagentResultText,
} from "../src/subagent/result.ts";

describe("background subagent result", () => {
	test("tells the model it is a result, not a new user request", () => {
		const text = subagentResultText({ id: "sub-1", title: "迁移", failed: false, text: "改好了" });
		expect(text).toContain("不是用户的新请求");
		expect(text).toContain("子智能体 sub-1（迁移）已结束，状态 完成。");
		expect(text).toContain("改好了");
	});

	test("is a hidden custom message Smelt renders as a status line", () => {
		const message = subagentResultMessage({ id: "sub-2", title: "", failed: true, text: "x".repeat(5000) });
		expect(message.customType).toBe(SMELT_SUBAGENT_RESULT_CUSTOM_TYPE);
		expect(message.display).toBe(false);
		expect(message.details).toMatchObject({ id: "sub-2", status: "failed" });
		expect(message.details.outputTail.length).toBeLessThan(4100);
		expect(message.details.outputTail.startsWith("…")).toBe(true);
		expect(message.content[0].text).toContain("x".repeat(5000));
	});

	test("child gets its own branch of the session's background task owner", () => {
		const owner = childTaskOwner(backgroundTaskOwner({}), "sub-1");
		expect(owner).toBe("/sub-1");
		expect(childTaskOwner(owner, "sub-2")).toBe("/sub-1/sub-2");
		const env = subagentEnvironment({ [SMELT_BACKGROUND_TASK_SOCK_ENV]: "/tmp/host.sock", PATH: "/bin" }, owner);
		expect(env[SMELT_BACKGROUND_TASK_SOCK_ENV]).toBe("/tmp/host.sock");
		expect(env[SMELT_BACKGROUND_TASK_OWNER_ENV]).toBe("/sub-1");
		expect(backgroundTaskOwner(env)).toBe("/sub-1");
	});
});
