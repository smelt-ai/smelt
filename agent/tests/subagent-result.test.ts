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
		const text = subagentResultText({
			toolCallId: "call-1",
			title: "迁移",
			status: "completed",
			text: "改好了",
			runs: [{ id: "sa-1", title: "迁移", status: "completed" }],
		});
		expect(text).toContain("不是用户的新请求");
		expect(text).toContain("派发（迁移）已结束，状态 完成。");
		expect(text).toContain("改好了");
	});

	test("a parallel dispatch lists each subagent's status", () => {
		const text = subagentResultText({
			toolCallId: "call-2",
			title: "2 个并行任务",
			status: "failed",
			text: "结果",
			runs: [
				{ id: "sa-a", title: "审计 a", status: "completed" },
				{ id: "sa-b", title: "审计 b", status: "failed" },
			],
		});
		expect(text).toContain("- sa-a（审计 a）：完成");
		expect(text).toContain("- sa-b（审计 b）：失败");
	});

	test("is a hidden custom message keyed by the dispatching tool call", () => {
		const message = subagentResultMessage({
			toolCallId: "call-3",
			title: "",
			status: "failed",
			text: "x".repeat(5000),
			runs: [{ id: "sa-3", title: "", status: "failed" }],
		});
		expect(message.customType).toBe(SMELT_SUBAGENT_RESULT_CUSTOM_TYPE);
		expect(message.display).toBe(false);
		expect(message.details).toMatchObject({ id: "call-3", status: "failed" });
		expect(message.details.runs).toEqual([{ id: "sa-3", title: "", status: "failed" }]);
		expect(message.details.outputTail.length).toBeLessThan(4100);
		expect(message.details.outputTail.startsWith("…")).toBe(true);
		expect(message.content[0].text).toContain("x".repeat(5000));
	});

	test("a stopped dispatch reports stopped, not failed", () => {
		const message = subagentResultMessage({
			toolCallId: "call-4",
			title: "审计",
			status: "stopped",
			text: "aborted",
			runs: [{ id: "sa-4", title: "审计", status: "stopped" }],
		});
		expect(message.details).toMatchObject({ status: "stopped" });
		expect(message.content[0].text).toContain("派发（审计）已结束，状态 已停止。");
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
