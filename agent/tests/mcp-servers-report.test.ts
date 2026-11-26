import { describe, expect, test } from "bun:test";
import type { ExtensionAPI, McpTransportFactory } from "@earendil-works/pi-coding-agent";
import {
	recordingStarts,
	SMELT_MCP_SERVERS_WIDGET,
	smeltMcpServersExtension,
} from "../src/mcp-servers-report.ts";

type Handler = (event: unknown, ctx: unknown) => unknown;

function fakePi() {
	const handlers = new Map<string, Handler[]>();
	const pi = {
		on(event: string, handler: Handler) {
			handlers.set(event, [...(handlers.get(event) ?? []), handler]);
		},
	} as unknown as ExtensionAPI;
	const emit = (event: string, ctx: unknown = {}) => {
		for (const handler of handlers.get(event) ?? []) handler({ type: event }, ctx);
	};
	return { pi, emit };
}

function fakeUi() {
	const reports: string[][] = [];
	const ui = {
		setWidget(key: string, lines: string[]) {
			if (key === SMELT_MCP_SERVERS_WIDGET) reports.push(JSON.parse(lines[0]));
		},
	};
	return { ui, reports };
}

const entry = (name: string) => ({ name }) as Parameters<McpTransportFactory>[0];
const transport = {} as ReturnType<McpTransportFactory>;

describe("MCP servers report", () => {
	test("reports the servers Pi started connections for, once each", () => {
		const { pi, emit } = fakePi();
		const { ui, reports } = fakeUi();
		smeltMcpServersExtension(pi);
		const factory = recordingStarts(() => transport);

		emit("session_start", { ui });
		expect(reports).toEqual([[]]);

		expect(factory(entry("figma"), "/", undefined)).toBe(transport);
		factory(entry("rea"), "/", undefined);
		// 断线后重连再要一次 transport，不算新服务器。
		factory(entry("figma"), "/", undefined);
		expect(reports).toEqual([[], ["figma"], ["figma", "rea"]]);

		emit("session_shutdown");
	});

	test("a factory that throws starts nothing", () => {
		const { pi, emit } = fakePi();
		const { ui, reports } = fakeUi();
		smeltMcpServersExtension(pi);
		const factory = recordingStarts(() => {
			throw new Error("bad config");
		});

		emit("session_start", { ui });
		expect(() => factory(entry("broken"), "/", undefined)).toThrow("bad config");
		expect(reports).toEqual([[]]);

		emit("session_shutdown");
	});

	test("a new session starts from an empty list", () => {
		const { pi, emit } = fakePi();
		const first = fakeUi();
		const second = fakeUi();
		smeltMcpServersExtension(pi);
		const factory = recordingStarts(() => transport);

		emit("session_start", { ui: first.ui });
		factory(entry("figma"), "/", undefined);
		emit("session_shutdown");
		emit("session_start", { ui: second.ui });
		factory(entry("rea"), "/", undefined);

		expect(first.reports).toEqual([[], ["figma"]]);
		expect(second.reports).toEqual([[], ["rea"]]);
		emit("session_shutdown");
	});
});
