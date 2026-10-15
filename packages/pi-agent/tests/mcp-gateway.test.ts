import { afterEach, describe, expect, test } from "bun:test";
import type { McpServerEntry } from "@earendil-works/pi-coding-agent";
import {
	type JsonRpcMessage,
	LATEST_PROTOCOL_VERSION,
	McpClient,
	McpSessionExpiredError,
	type McpTransport,
	StreamableHttpTransport,
} from "@earendil-works/pi-mcp";
import { createInMemoryTransportPair, type InMemoryTransport } from "@earendil-works/pi-mcp/testing";
import { type GatewayEndpoint, GatewayTransport, gatewayEnvironment, sharedTransportFactory, stdioServerSpec } from "../src/mcp-gateway-client.ts";
import { McpGateway, parseStdioServerSpec, type StdioServerSpec, upstreamKey } from "../src/mcp-gateway.ts";
import { createPiDefaultTransport } from "../src/pi-mcp-runtime.ts";

const SPEC: StdioServerSpec = { name: "rea", command: "bun", args: ["x", "rea-agents", "mcp"] };

/** A scripted MCP server behind an in-memory transport; every transport is one "process". */
function fakeServer() {
	const stats = { spawned: 0, closed: 0, toolsList: 0, calls: 0, cancelled: [] as unknown[] };
	let current: InMemoryTransport | undefined;
	const waiting = new Map<string | number, () => void>();
	const factory = (): McpTransport => {
		stats.spawned++;
		const { client, server } = createInMemoryTransportPair();
		current = server;
		server.onClose(() => stats.closed++);
		server.onMessage((message: JsonRpcMessage) => {
			if (!("method" in message)) return;
			if (!("id" in message)) {
				if (message.method === "notifications/cancelled") {
					stats.cancelled.push(message.params);
					const id = (message.params as { requestId: string | number }).requestId;
					waiting.get(id)?.();
				}
				return;
			}
			const reply = (result: Record<string, unknown>) =>
				void server.send({ jsonrpc: "2.0", id: message.id, result });
			const params = (message.params ?? {}) as {
				name?: string;
				arguments?: unknown;
				_meta?: { progressToken?: string | number };
			};
			switch (message.method) {
				case "initialize":
					reply({
						protocolVersion: LATEST_PROTOCOL_VERSION,
						capabilities: { tools: { listChanged: true } },
						serverInfo: { name: "fake", version: "1" },
						instructions: "fake server",
					});
					return;
				case "tools/list":
					stats.toolsList++;
					reply({ tools: [{ name: "echo", inputSchema: { type: "object" } }] });
					return;
				case "tools/call":
					stats.calls++;
					if (params.name === "slow") {
						waiting.set(message.id, () => reply({ content: [] }));
						return;
					}
					if (params._meta?.progressToken !== undefined) {
						void server.send({
							jsonrpc: "2.0",
							method: "notifications/progress",
							params: { progressToken: params._meta.progressToken, progress: 1, total: 2 },
						});
						// Progress travels on the session stream, the reply on the POST; let progress land first.
						setTimeout(
							() => reply({ content: [{ type: "text", text: JSON.stringify(params.arguments ?? null) }] }),
							50,
						);
						return;
					}
					reply({ content: [{ type: "text", text: JSON.stringify(params.arguments ?? null) }] });
					return;
				default:
					void server.send({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "nope" } });
			}
		});
		void server.start();
		return client;
	};
	return {
		stats,
		factory,
		notify: (method: string) => current?.send({ jsonrpc: "2.0", method }),
	};
}

const servers: Array<{ stop: (force?: boolean) => void }> = [];
const clients: McpClient[] = [];

afterEach(async () => {
	await Promise.all(clients.splice(0).map((client) => client.close().catch(() => undefined)));
	for (const server of servers.splice(0)) server.stop(true);
});

function serve(gateway: McpGateway): string {
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, idleTimeout: 0, fetch: (r) => gateway.handle(r) });
	servers.push(server);
	return `http://127.0.0.1:${server.port}`;
}

function setup() {
	const fake = fakeServer();
	let clock = 1_000;
	const gateway = new McpGateway({
		token: "secret",
		createTransport: fake.factory,
		upstreamIdleMs: 60_000,
		sessionIdleMs: 120_000,
		now: () => clock,
	});
	const base = serve(gateway);
	const key = gateway.register(SPEC);
	const connect = async () => {
		const client = new McpClient({ name: "test", version: "1", requestTimeoutMs: 5_000 });
		clients.push(client);
		await client.connect(
			new StreamableHttpTransport({ url: `${base}/mcp/${key}`, headers: { authorization: "Bearer secret" } }),
		);
		return client;
	};
	return { fake, gateway, base, key, connect, advance: (ms: number) => (clock += ms) };
}

async function until(check: () => boolean, ms = 2_000): Promise<void> {
	const deadline = Date.now() + ms;
	while (!check()) {
		if (Date.now() > deadline) throw new Error("condition not reached");
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
}

describe("shared MCP gateway", () => {
	test("serves many clients from one server process", async () => {
		const { fake, connect } = setup();
		const first = await connect();
		const second = await connect();
		expect(fake.stats.spawned).toBe(1);
		expect(first.instructions).toBe("fake server");

		expect((await first.listTools()).map((tool) => tool.name)).toEqual(["echo"]);
		expect((await second.listTools()).map((tool) => tool.name)).toEqual(["echo"]);
		expect(fake.stats.toolsList).toBe(1);

		const [a, b] = await Promise.all([first.callTool("echo", { from: "a" }), second.callTool("echo", { from: "b" })]);
		expect(a.content).toEqual([{ type: "text", text: '{"from":"a"}' }]);
		expect(b.content).toEqual([{ type: "text", text: '{"from":"b"}' }]);
		expect(fake.stats.spawned).toBe(1);
	});

	test("routes progress to the client that asked", async () => {
		const { connect } = setup();
		const first = await connect();
		const second = await connect();
		const firstProgress: unknown[] = [];
		const secondProgress: unknown[] = [];
		second.onNotification("notifications/progress", (params) => secondProgress.push(params));
		// The server-to-client streams open right after `initialized`.
		await new Promise((resolve) => setTimeout(resolve, 100));
		await first.callTool("echo", {}, { onProgress: (progress) => firstProgress.push(progress) });
		expect(firstProgress).toHaveLength(1);
		expect(firstProgress[0]).toMatchObject({ progress: 1, total: 2 });
		expect(secondProgress).toEqual([]);
	});

	test("forwards cancellation to the server", async () => {
		const { fake, connect } = setup();
		const client = await connect();
		const controller = new AbortController();
		const call = client.callTool("slow", {}, { signal: controller.signal });
		await until(() => fake.stats.calls === 1);
		controller.abort();
		await expect(call).rejects.toThrow();
		await until(() => fake.stats.cancelled.length === 1);
	});

	test("broadcasts list changes and drops the cached list", async () => {
		const { fake, connect } = setup();
		const first = await connect();
		const second = await connect();
		await first.listTools();
		const seen: string[] = [];
		first.onNotification("notifications/tools/list_changed", () => seen.push("first"));
		second.onNotification("notifications/tools/list_changed", () => seen.push("second"));
		// The server-to-client streams open right after `initialized`.
		await new Promise((resolve) => setTimeout(resolve, 100));
		await fake.notify("notifications/tools/list_changed");
		await until(() => seen.length === 2);
		expect(seen.sort()).toEqual(["first", "second"]);
		await second.listTools();
		expect(fake.stats.toolsList).toBe(2);
	});

	test("stops an idle server and starts it again on the next call only", async () => {
		const { fake, gateway, connect, advance } = setup();
		const first = await connect();
		await first.listTools();
		advance(60_000);
		await gateway.sweep();
		expect(fake.stats.closed).toBe(1);
		expect(gateway.status().servers[0]?.running).toBe(false);

		// A new conversation connects and lists tools without waking the server.
		const second = await connect();
		await second.listTools();
		expect(fake.stats.spawned).toBe(1);

		await second.callTool("echo", { again: true });
		expect(fake.stats.spawned).toBe(2);
	});

	test("does not stop a server with a request in flight", async () => {
		const { fake, gateway, connect, advance } = setup();
		const client = await connect();
		const controller = new AbortController();
		const call = client.callTool("slow", {}, { signal: controller.signal });
		await until(() => fake.stats.calls === 1);
		advance(600_000);
		await gateway.sweep();
		expect(fake.stats.closed).toBe(0);
		controller.abort();
		await expect(call).rejects.toThrow();
	});

	test("forgets sessions of clients that went away, then is idle", async () => {
		const { gateway, connect, advance } = setup();
		const client = await connect();
		await client.close();
		await until(() => gateway.status().sessions === 0);
		advance(60_000);
		await gateway.sweep();
		expect(gateway.isIdle()).toBe(true);
	});

	test("rejects requests without the token or from a browser", async () => {
		const { base, key } = setup();
		expect((await fetch(`${base}/health`)).status).toBe(401);
		expect(
			(await fetch(`${base}/health`, { headers: { authorization: "Bearer secret", origin: "https://evil.test" } }))
				.status,
		).toBe(403);
		const unknown = await fetch(`${base}/mcp/${"0".repeat(32)}`, {
			method: "POST",
			headers: { authorization: "Bearer secret", "content-type": "application/json" },
			body: "{}",
		});
		expect(unknown.status).toBe(404);
		const noSession = await fetch(`${base}/mcp/${key}`, {
			method: "POST",
			headers: { authorization: "Bearer secret", "content-type": "application/json" },
			body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" }),
		});
		expect(noSession.status).toBe(400);
	});

	test("registers servers over HTTP by spec", async () => {
		const { base } = setup();
		const response = await fetch(`${base}/servers`, {
			method: "POST",
			headers: { authorization: "Bearer secret", "content-type": "application/json" },
			body: JSON.stringify(SPEC),
		});
		expect(await response.json()).toEqual({ key: upstreamKey(SPEC) });
		const bad = await fetch(`${base}/servers`, {
			method: "POST",
			headers: { authorization: "Bearer secret", "content-type": "application/json" },
			body: JSON.stringify({ name: "x" }),
		});
		expect(bad.status).toBe(400);
	});
});

describe("server specs", () => {
	test("share a key only when the process would be the same", () => {
		expect(upstreamKey(SPEC)).toBe(upstreamKey({ ...SPEC, args: [...(SPEC.args ?? [])] }));
		expect(upstreamKey({ ...SPEC, env: { A: "1", B: "2" } })).toBe(upstreamKey({ ...SPEC, env: { B: "2", A: "1" } }));
		expect(upstreamKey(SPEC)).not.toBe(upstreamKey({ ...SPEC, args: ["other"] }));
		expect(upstreamKey(SPEC)).not.toBe(upstreamKey({ ...SPEC, cwd: "/tmp" }));
	});

	test("are validated", () => {
		expect(parseStdioServerSpec(SPEC)).toEqual(SPEC);
		expect(typeof parseStdioServerSpec({ name: "x", command: "y", args: [1] })).toBe("string");
		expect(typeof parseStdioServerSpec({ name: "x", command: "y", env: { A: 1 } })).toBe("string");
		expect(typeof parseStdioServerSpec(null)).toBe("string");
	});

	test("come from stdio entries only; HTTP servers keep Pi's transport", () => {
		const stdio: McpServerEntry = {
			name: "rea",
			config: { command: "bun", args: ["x"], env: { A: "1" }, exposure: "codemode", description: "d" },
			source: "test",
		};
		const http: McpServerEntry = { name: "figma", config: { url: "https://mcp.figma.com/mcp" }, source: "test" };
		expect(stdioServerSpec(stdio)).toEqual({ name: "rea", command: "bun", args: ["x"], env: { A: "1" } });
		expect(stdioServerSpec(http)).toBeUndefined();

		const routed: string[] = [];
		const marker = createInMemoryTransportPair().client;
		const factory = sharedTransportFactory(
			(entry) => {
				routed.push(`direct:${entry.name}`);
				return marker;
			},
			(spec) => {
				routed.push(`gateway:${spec.name}`);
				return marker;
			},
		);
		factory(stdio, "/work", undefined);
		factory(http, "/work", undefined);
		expect(routed).toEqual(["gateway:rea", "direct:figma"]);
	});

	test("Pi's default transport factory is reachable", () => {
		expect(typeof createPiDefaultTransport).toBe("function");
	});

	test("the gateway does not inherit one conversation's Smelt variables", () => {
		expect(
			gatewayEnvironment({ PATH: "/bin", SMELT_HOME: "/h", SMELT_SESSION_ID: "s", SMELT_AGENT_TOKEN: "t" }),
		).toEqual({ PATH: "/bin", SMELT_HOME: "/h" });
	});
});

describe("gateway transport", () => {
	test("connects a client through the gateway it locates", async () => {
		const fake = fakeServer();
		const gateway = new McpGateway({
			token: "secret",
			createTransport: fake.factory,
			upstreamIdleMs: 60_000,
			sessionIdleMs: 60_000,
		});
		const base = serve(gateway);
		const endpoint: GatewayEndpoint = { base, token: "secret", pid: process.pid };
		const client = new McpClient({ name: "test", version: "1", requestTimeoutMs: 5_000 });
		clients.push(client);
		await client.connect(new GatewayTransport(SPEC, async () => endpoint));
		const result = await client.callTool("echo", { ok: 1 });
		expect(result.content).toEqual([{ type: "text", text: '{"ok":1}' }]);
		expect(gateway.status().servers).toEqual([{ name: "rea", key: upstreamKey(SPEC), running: true, sessions: 1 }]);
	});

	test("reports a vanished gateway as an expired session, so Pi retries on a new connection", async () => {
		const fake = fakeServer();
		const gateway = new McpGateway({
			token: "secret",
			createTransport: fake.factory,
			upstreamIdleMs: 60_000,
			sessionIdleMs: 60_000,
		});
		const server = Bun.serve({ hostname: "127.0.0.1", port: 0, idleTimeout: 0, fetch: (r) => gateway.handle(r) });
		const endpoint: GatewayEndpoint = { base: `http://127.0.0.1:${server.port}`, token: "secret", pid: process.pid };
		const transport = new GatewayTransport(SPEC, async () => endpoint);
		const client = new McpClient({ name: "test", version: "1", requestTimeoutMs: 5_000 });
		clients.push(client);
		await client.connect(transport);
		server.stop(true);
		await gateway.close();
		await expect(client.callTool("echo", {})).rejects.toBeInstanceOf(McpSessionExpiredError);
	});
});
