import { describe, expect, test } from "bun:test";
import type { ExtensionContext, RpcExtensionUIRequest, RpcExtensionUIResponse } from "@earendil-works/pi-coding-agent";
import { bridgeSubagentUiRequest } from "../src/subagent/ui-bridge.ts";

describe("subagent RPC UI bridge", () => {
	test("sends child permission confirmation through Smelt's host UI", async () => {
		const calls: unknown[][] = [];
		const responses: RpcExtensionUIResponse[] = [];
		const context = {
			hasUI: true,
			ui: {
				confirm: async (...args: unknown[]) => {
					calls.push(args);
					return true;
				},
			},
		} as unknown as Pick<ExtensionContext, "hasUI" | "ui">;
		const request: RpcExtensionUIRequest = {
			type: "extension_ui_request",
			id: "child-confirm-1",
			method: "confirm",
			title: "smelt.permission.v1",
			message: JSON.stringify({ version: 1, toolCallId: "child-tool-1", toolName: "bash", input: { command: "pwd" } }),
		};

		await bridgeSubagentUiRequest(request, context, undefined, async (response) => {
			responses.push(response);
		});

		expect(calls).toEqual([[request.title, request.message, undefined]]);
		expect(responses).toEqual([
			{ type: "extension_ui_response", id: "child-confirm-1", confirmed: true },
		]);
	});

	test("fails closed when the parent has no UI", async () => {
		const responses: RpcExtensionUIResponse[] = [];
		const context = {
			hasUI: false,
			ui: { confirm: async () => true },
		} as unknown as Pick<ExtensionContext, "hasUI" | "ui">;
		const request: RpcExtensionUIRequest = {
			type: "extension_ui_request",
			id: "child-confirm-2",
			method: "confirm",
			title: "smelt.permission.v1",
			message: "{}",
		};

		await bridgeSubagentUiRequest(request, context, undefined, async (response) => {
			responses.push(response);
		});

		expect(responses).toEqual([
			{ type: "extension_ui_response", id: "child-confirm-2", cancelled: true },
		]);
	});
});
