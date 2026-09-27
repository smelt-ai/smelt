import { describe, expect, test } from "bun:test";
import type { ExtensionAPI, ExtensionContext, Skill } from "@earendil-works/pi-coding-agent";
import { formatSkillsForPrompt } from "@earendil-works/pi-coding-agent";
import {
	appendRuntimeDebugModelCall,
	MAX_RETAINED_RUNTIME_DEBUG_MODEL_CALLS,
	buildCompactionStartTrace,
	buildContextUsageBuckets,
	buildRuntimeDebugPayload,
	capturePiContext,
	captureProviderHeaders,
	captureProviderRequest,
	captureProviderResponse,
	captureProviderResponseMetadata,
	estimateTokens,
	formatProjectContext,
	SMELT_RUNTIME_DEBUG_WIDGET,
	type RuntimeDebugPayload,
} from "../src/context-usage.ts";
import { SMELT_EXTENSION_FACTORIES } from "../src/runtime-extensions.ts";
import smeltContextUsageExtension from "../src/context-usage.ts";

function skill(name: string, description: string): Skill {
	return {
		name,
		description,
		filePath: `/skills/${name}/SKILL.md`,
		baseDir: `/skills/${name}`,
		disableModelInvocation: false,
		sourceInfo: { source: "test", path: `/skills/${name}/SKILL.md` },
	} as Skill;
}

describe("context usage buckets", () => {
	test("host ships the context-usage extension before permission", () => {
		const contextUsageIndex = SMELT_EXTENSION_FACTORIES.findIndex(
			(extension) => typeof extension !== "function" && extension.factory === smeltContextUsageExtension,
		);
		expect(contextUsageIndex).toBeGreaterThanOrEqual(0);
		expect(contextUsageIndex).toBeLessThan(SMELT_EXTENSION_FACTORIES.length - 1);
	});

	test("maps Pi parts onto Cursor-style buckets", () => {
		const files = [{ path: "AGENTS.md", content: "use sqlite".repeat(20) }];
		const skills = [skill("fx-deploy", "Trigger FX pipelines and wait for nodes.")];
		const systemPrompt =
			"You are a coding agent.\nBe concise.\n" +
			formatProjectContext(files) +
			formatSkillsForPrompt(skills);
		const buckets = buildContextUsageBuckets({
			systemPrompt,
			contextFiles: files,
			skills,
			tools: [
				{
					name: "bash",
					description: "Run a shell command",
					parameters: { type: "object", properties: { command: { type: "string" } } },
					source: "builtin",
				},
				{
					name: "multica",
					description: "Calendar tool",
					parameters: { type: "object" },
					source: "extension",
				},
				{
					name: "subagent",
					description: "Start a nested agent",
					parameters: { type: "object" },
					source: "builtin",
				},
			],
			messages: [
				{ role: "user", content: "fix the title bar" },
				{ role: "assistant", content: "looking" },
				{ role: "toolResult", content: "file contents ".repeat(30) },
				{ summarized: true, content: "earlier work was compacted" },
			],
		});
		expect(buckets.systemPrompt).toBeGreaterThan(0);
		expect(buckets.rules).toBe(estimateTokens(formatProjectContext(files)));
		expect(buckets.skills).toBeGreaterThan(0);
		expect(buckets.toolsDefinition).toBeGreaterThan(0);
		expect(buckets.mcpDynamic).toBeGreaterThan(0);
		expect(buckets.subagent).toBeGreaterThan(0);
		expect(buckets.summarized).toBeGreaterThan(0);
		expect(buckets.conversation).toBeGreaterThan(0);
		expect(buckets.systemPrompt + buckets.rules + buckets.skills).toBe(
			estimateTokens(systemPrompt),
		);
	});

	test("runtime trace pairs Pi context, sanitized headers, provider payload and response", () => {
		const systemPrompt = "system line 1\n<project_context>真实上下文</project_context>";
		const tools = [
			{
				name: "bash",
				description: "Run a shell command",
				parameters: {
					type: "object",
					required: ["command"],
					properties: { command: { type: "string" } },
				},
				source: "builtin",
			},
		];
		const context = capturePiContext(
			1,
			[
				{ role: "system", content: systemPrompt },
				{ role: "user", content: [{ type: "text", text: "hello" }] },
			],
			{
				provider: "openai",
				id: "gpt-test",
				api: "responses",
				thinkingLevel: "high",
			},
			{ systemPrompt, tools },
			3,
			1234,
			2,
		);
		const withHeaders = captureProviderHeaders(
			context,
			{
				Authorization: "Bearer must-not-leak",
				"X-Api-Key": "must-not-leak",
				"X-Request-Source": "smelt",
			},
			1235,
		);
		const request = captureProviderRequest(
			withHeaders,
			{
				model: "gpt-test",
				max_tokens: 4096,
				messages: [{ role: "user", content: "hello" }],
				api_key: "must-not-leak",
				image: {
					type: "image",
					data: "large-base64-payload",
					url: "data:image/png;base64,also-large",
				},
			},
			1236,
		);
		const metadata = captureProviderResponseMetadata(
			request,
			200,
			{
				"X-Request-Id": "req-1",
				"Set-Cookie": "session=must-not-leak",
			},
			1240,
		);
		const call = captureProviderResponse(
			metadata,
			{
				role: "assistant",
				content: [{ type: "text", text: "done" }],
				usage: { input: 100, output: 20 },
			},
			1250,
		);

		expect(call.source).toBe("pi_context_with_system");
		expect(call.piContext).toEqual([
			{ role: "system", content: systemPrompt },
			{ role: "user", content: [{ type: "text", text: "hello" }] },
		]);
		expect(call.requestHeaders[0]?.headers).toEqual({
			Authorization: "[REDACTED]",
			"X-Api-Key": "[REDACTED]",
			"X-Request-Source": "smelt",
		});
		expect(call.payloadSource).toBe("pi_before_provider_request");
		expect(call.payload).toMatchObject({
			api_key: "[REDACTED]",
			image: { data: "[IMAGE DATA OMITTED]", url: "[IMAGE DATA OMITTED]" },
		});
		expect(call.responseMetadata[0]).toMatchObject({
			status: 200,
			headers: { "X-Request-Id": "req-1", "Set-Cookie": "[REDACTED]" },
		});
		expect(call.response).toMatchObject({ role: "assistant", usage: { output: 20 } });
		expect(buildRuntimeDebugPayload(systemPrompt, tools, [call]).modelCalls[0]).toEqual(call);
	});

	test("runtime extension pairs hooks to context_with_system and ignores warm requests", () => {
		type Handler = (event: any, context: ExtensionContext) => void;
		const handlers = new Map<string, Handler>();
		const widgets: Array<{ key: string; lines: string[] }> = [];
		const pi = {
			on: (event: string, handler: Handler) => {
				handlers.set(event, handler);
				return () => {};
			},
			getActiveTools: () => [],
			getAllTools: () => [],
		} as unknown as ExtensionAPI;
		const context = {
			ui: {
				setWidget: (key: string, lines: string[]) => widgets.push({ key, lines }),
			},
			sessionManager: {
				getBranch: () => [{ type: "message", message: { role: "user" } }],
			},
			model: { provider: "openai", id: "gpt-test", api: "responses" },
			thinkingLevel: "high",
			getSystemPrompt: () => "exact system prompt",
		} as unknown as ExtensionContext;
		const emit = (event: string, value: unknown) => handlers.get(event)?.(value, context);

		smeltContextUsageExtension(pi);
		emit("before_provider_request", { payload: { model: "cache-warm" } });
		const contextMessages = [
			{ role: "system", content: "actual prompt", tools: [{ name: "bash" }] },
			{ role: "user", content: [{ type: "text", text: "inspect this" }] },
			{
				role: "assistant",
				content: [{ type: "toolCall", name: "bash", arguments: { api_key: "context-secret" } }],
			},
		];
		emit("context_with_system", { messages: contextMessages });
		emit("before_provider_headers", {
			headers: { Authorization: "Bearer secret", "X-Request-Source": "smelt" },
		});
		emit("before_provider_request", {
			payload: { model: "gpt-test", messages: [{ role: "user", content: "inspect this" }] },
		});
		emit("after_provider_response", {
			status: 200,
			headers: { "X-Request-Id": "request-1", "Set-Cookie": "secret" },
		});
		emit("message_end", {
			message: { role: "assistant", content: [{ type: "text", text: "done" }], usage: { output: 3 } },
		});
		emit("agent_settled", {});
		const traceWidget = widgets.find((widget) => widget.key === SMELT_RUNTIME_DEBUG_WIDGET);
		const trace = JSON.parse(traceWidget?.lines[0] ?? "{}") as RuntimeDebugPayload;
		expect(trace.modelCalls).toHaveLength(1);
		expect(trace.modelCalls[0]?.piContext).toEqual([
			...contextMessages.slice(0, 2),
			{
				role: "assistant",
				content: [{ type: "toolCall", name: "bash", arguments: { api_key: "[REDACTED]" } }],
			},
		]);
		expect(trace.modelCalls[0]?.piContextRedactedPaths).toEqual([
			"$.piContext[2].content[0].arguments.api_key",
		]);
		expect(trace.modelCalls[0]?.requestHeaders[0]?.headers.Authorization).toBe("[REDACTED]");
		expect(trace.modelCalls[0]?.responseMetadata[0]?.status).toBe(200);
		expect(trace.modelCalls[0]?.responseMetadata[0]?.headers["X-Request-Id"]).toBe("request-1");
		expect(trace.modelCalls[0]?.payload).toMatchObject({ model: "gpt-test" });
		expect(trace.modelCalls[0]?.response).toMatchObject({ role: "assistant", usage: { output: 3 } });
	});

	test("provider responses and usage stay paired with their request and are redacted", () => {
		const request = captureProviderRequest(
			capturePiContext(
				1,
				[{ role: "system", content: "system" }],
				{},
				{ systemPrompt: "system", tools: [] },
				2,
				100,
			),
			{ messages: [] },
			110,
		);
		const completed = captureProviderResponse(
			request,
			{
				role: "assistant",
				content: [{ type: "text", text: "done" }],
				usage: { input: 100, output: 20 },
				provider_metadata: { api_key: "must-not-leak" },
			},
			200,
		);
		expect(completed.responseCapturedAtMs).toBe(200);
		expect(completed.response).toMatchObject({
			role: "assistant",
			usage: { input: 100, output: 20 },
			provider_metadata: { api_key: "[REDACTED]" },
		});
		expect(completed.responseRedactedPaths).toEqual([
			"$.response.provider_metadata.api_key",
		]);
	});

	test("model-call history keeps the newest requests and drops the rest", () => {
		let calls: ReturnType<typeof capturePiContext>[] = [];
		for (let sequence = 1; sequence <= 100; sequence += 1) {
			calls = appendRuntimeDebugModelCall(
				calls,
				capturePiContext(sequence, [], {}, { systemPrompt: "", tools: [] }),
			);
		}
		expect(calls).toHaveLength(MAX_RETAINED_RUNTIME_DEBUG_MODEL_CALLS);
		expect(calls[0]?.sequence).toBe(100 - MAX_RETAINED_RUNTIME_DEBUG_MODEL_CALLS + 1);
		expect(calls.at(-1)?.sequence).toBe(100);
	});

	test("compaction capture records the actual boundary and message previews", () => {
		const compaction = buildCompactionStartTrace(
			2,
			4,
			"overflow",
			true,
			{
				firstKeptEntryId: "entry-20",
				messagesToSummarize: [
					{ role: "user", content: [{ type: "text", text: "preserve this" }] },
					{ role: "user", content: [{ type: "image", data: "not copied" }] },
				],
				turnPrefixMessages: [],
				isSplitTurn: false,
				tokensBefore: 90_000,
				previousSummary: "earlier summary",
			},
			1234,
		);
		expect(compaction.status).toBe("started");
		expect(compaction.turn).toBe(4);
		expect(compaction.firstKeptEntryId).toBe("entry-20");
		expect(compaction.tokensBefore).toBe(90_000);
		expect(compaction.sourceMessages).toEqual([
			{
				segment: "summarized",
				role: "user",
				preview: "preserve this",
				truncated: false,
			},
			{
				segment: "summarized",
				role: "user",
				preview: "[image omitted]",
				truncated: false,
			},
		]);
	});

	test("compaction captures all source messages and full-length text previews", () => {
		const trace = buildCompactionStartTrace(
			1,
			1,
			"threshold",
			false,
			{
				firstKeptEntryId: "keep",
				messagesToSummarize: Array.from({ length: 65 }, (_, index) => ({
					role: "user",
					content: index === 0 ? "x".repeat(2_001) : `message ${index}`,
				})),
				turnPrefixMessages: [{ role: "assistant", content: "not included after limit" }],
				isSplitTurn: true,
				tokensBefore: 20_000,
			},
			100,
		);
		expect(trace.sourceMessages).toHaveLength(66);
		expect(trace.sourceMessages[0]?.preview).toHaveLength(2_001);
		expect(trace.sourceMessages[0]?.truncated).toBe(false);
		expect(trace.sourceMessagesOmitted).toBe(0);
	});

	test("empty parts stay at zero", () => {
		expect(
			buildContextUsageBuckets({
				systemPrompt: "",
				contextFiles: [],
				skills: [],
				tools: [],
				messages: [],
			}),
		).toEqual({
			systemPrompt: 0,
			toolsDefinition: 0,
			rules: 0,
			skills: 0,
			mcpDynamic: 0,
			subagent: 0,
			summarized: 0,
			conversation: 0,
		});
	});

	test("context event runs shake pruning and returns pruned messages", () => {
		type Handler = (event: any, context: ExtensionContext) => any;
		const handlers = new Map<string, Handler>();
		const widgets: Array<{ key: string; lines: string[] }> = [];
		const pi = {
			on: (event: string, handler: Handler) => {
				handlers.set(event, handler);
				return () => {};
			},
			getActiveTools: () => [],
			getAllTools: () => [],
		} as unknown as ExtensionAPI;
		const context = {
			ui: {
				setWidget: (key: string, lines: string[]) => widgets.push({ key, lines }),
			},
			sessionManager: {
				getBranch: () => [],
			},
			model: { provider: "openai", id: "gpt-test", api: "responses" },
			getSystemPrompt: () => "system prompt",
		} as unknown as ExtensionContext;

		smeltContextUsageExtension(pi);
		const contextHandler = handlers.get("context");
		expect(contextHandler).toBeDefined();

		const messages = [
			{ role: "user", content: "read a.txt" },
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "read-1", name: "read", arguments: { path: "a.txt" } }],
			},
			{
				role: "toolResult",
				toolCallId: "read-1",
				toolName: "read",
				content: [{ type: "text", text: "stale content ".repeat(20) }],
			},
			{ role: "user", content: "write a.txt" },
			{
				role: "assistant",
				content: [{ type: "toolCall", id: "write-1", name: "write", arguments: { path: "a.txt" } }],
			},
			{
				role: "toolResult",
				toolCallId: "write-1",
				toolName: "write",
				content: [{ type: "text", text: "written" }],
			},
			{ role: "user", content: "what next?" },
		];

		const result = contextHandler!({ type: "context", messages }, context);
		expect(result).toBeDefined();
		expect(result.messages).toBeDefined();
		expect(result.messages[2].content[0].text).toContain("Stale read of \"a.txt\" elided by shake");
	});

	test("registers /shake and /handoff commands and intercepts slash input", async () => {
		type Handler = (event: any, context: ExtensionContext) => any;
		const handlers = new Map<string, Handler>();
		const commands = new Map<string, { description?: string; handler: (args: string, ctx: ExtensionContext) => any }>();
		const notifications: Array<{ msg: string; type: string }> = [];
		let compactedInstructions: string | undefined;

		const pi = {
			on: (event: string, handler: Handler) => {
				handlers.set(event, handler);
				return () => {};
			},
			registerCommand: (name: string, def: any) => {
				commands.set(name, def);
			},
			getActiveTools: () => [],
			getAllTools: () => [],
		} as unknown as ExtensionAPI;

		const branchEntries = [
			{
				type: "message",
				message: { role: "user", content: "read a.txt" },
			},
			{
				type: "message",
				message: {
					role: "assistant",
					content: [{ type: "toolCall", id: "c1", name: "read", arguments: { path: "a.txt" } }],
				},
			},
			{
				type: "message",
				message: {
					role: "toolResult",
					toolCallId: "c1",
					toolName: "read",
					content: [{ type: "text", text: "stale read content ".repeat(20) }],
				},
			},
			{
				type: "message",
				message: { role: "user", content: "now write a.txt" },
			},
			{
				type: "message",
				message: {
					role: "assistant",
					content: [{ type: "toolCall", id: "c2", name: "write", arguments: { path: "a.txt" } }],
				},
			},
			{
				type: "message",
				message: {
					role: "toolResult",
					toolCallId: "c2",
					toolName: "write",
					content: [{ type: "text", text: "written" }],
				},
			},
			{
				type: "message",
				message: { role: "user", content: "done" },
			},
		];

		const context = {
			hasUI: true,
			ui: {
				setWidget: () => {},
				notify: (msg: string, type: string) => notifications.push({ msg, type }),
			},
			sessionManager: {
				getBranch: () => branchEntries,
			},
			compact: (options: any) => {
				compactedInstructions = options?.customInstructions;
				options?.onComplete?.();
			},
			getSystemPrompt: () => "system prompt",
		} as unknown as ExtensionContext;

		smeltContextUsageExtension(pi);

		// 1. Verify commands registration
		expect(commands.has("shake")).toBe(true);
		expect(commands.has("handoff")).toBe(true);

		// 2. Test shake command execution
		const shakeCmd = commands.get("shake")!;
		await shakeCmd.handler("", context);
		expect(notifications.some((n) => n.msg.includes("Shake completed: pruned 1 item(s)"))).toBe(true);

		// 3. Test handoff command execution
		const handoffCmd = commands.get("handoff")!;
		await handoffCmd.handler("Focus on testing", context);
		expect(compactedInstructions).toBe("Focus on testing");
		expect(notifications.some((n) => n.msg.includes("Handoff compaction completed."))).toBe(true);

		// 4. Test input interception
		const inputHandler = handlers.get("input");
		expect(inputHandler).toBeDefined();

		const shakeInputRes = await inputHandler!({ text: "  /shake  " }, context);
		expect(shakeInputRes).toEqual({ action: "handled" });

		const handoffInputRes = await inputHandler!({ text: "/handoff Focus on release" }, context);
		expect(handoffInputRes).toEqual({ action: "handled" });
		expect(compactedInstructions).toBe("Focus on release");

		const normalInputRes = await inputHandler!({ text: "hello world" }, context);
		expect(normalInputRes).toEqual({ action: "continue" });
	});
});
