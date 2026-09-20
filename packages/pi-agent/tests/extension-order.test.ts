import { describe, expect, test } from "bun:test";
import {
	createEventBus,
	createExtensionRuntime,
	ExtensionRunner,
	type ExtensionFactory,
	type ExtensionUIContext,
} from "@earendil-works/pi-coding-agent";
import { loadExtensionFromFactory } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/extensions/loader.js";
import { SMELT_EXTENSION_FACTORIES } from "../src/runtime-extensions.ts";
import smeltContextUsageExtension from "../src/context-usage.ts";
import smeltElicitationExtension from "../src/elicitation.ts";
import smeltPermissionExtension from "../src/smelt-permission.ts";

describe("Smelt Pi extension ordering", () => {
	test("host ships built-in elicitation and keeps permission last", () => {
		expect(SMELT_EXTENSION_FACTORIES).toEqual([
			smeltElicitationExtension,
			smeltContextUsageExtension,
			smeltPermissionExtension,
		]);
	});

	test("host approval observes arguments after mutable user handlers", async () => {
		const mutateInput: ExtensionFactory = (pi) => {
			pi.on("tool_call", async (event) => {
				(event.input as { command: string }).command = "destructive-command";
			});
		};
		const runtime = createExtensionRuntime();
		const eventBus = createEventBus();
		const factories = [mutateInput, ...SMELT_EXTENSION_FACTORIES];
		const extensions = await Promise.all(
			factories.map((factory, index) =>
				loadExtensionFromFactory(
					factory,
					process.cwd(),
					eventBus,
					runtime,
					index === 0 ? "<user:mutator>" : "<inline:smelt-permission>",
				),
			),
		);
		const runner = new ExtensionRunner(
			extensions,
			runtime,
			process.cwd(),
			{} as never,
			{} as never,
		);
		let approvedInput: unknown;
		runner.setUIContext({
			confirm: async (_title, message) => {
				approvedInput = JSON.parse(message).input;
				return true;
			},
		} as ExtensionUIContext);
		const event = {
			type: "tool_call" as const,
			toolCallId: "tool-1",
			toolName: "bash",
			input: { command: "read-only-command" },
		};

		await runner.emitToolCall(event);

		expect(approvedInput).toEqual({ command: "destructive-command" });
		expect(approvedInput).toEqual(event.input);
	});

	test("choice-shaped payloads skip host approval for any tool name", async () => {
		const runtime = createExtensionRuntime();
		const eventBus = createEventBus();
		const extensions = await Promise.all(
			SMELT_EXTENSION_FACTORIES.map((factory) =>
				loadExtensionFromFactory(
					factory,
					process.cwd(),
					eventBus,
					runtime,
					"<inline:smelt-permission>",
				),
			),
		);
		const runner = new ExtensionRunner(
			extensions,
			runtime,
			process.cwd(),
			{} as never,
			{} as never,
		);
		let confirmCalled = false;
		runner.setUIContext({
			confirm: async () => {
				confirmCalled = true;
				return true;
			},
		} as ExtensionUIContext);

		await runner.emitToolCall({
			type: "tool_call",
			toolCallId: "tool-1",
			toolName: "questionnaire",
			input: {
				questions: [{ prompt: "范围？", options: [{ label: "小" }, { label: "大" }] }],
			},
		});

		expect(confirmCalled).toBe(false);
	});
});
