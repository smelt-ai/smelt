import { createHash, randomUUID, timingSafeEqual } from "node:crypto";
import { readFileSync } from "node:fs";
import { isAbsolute } from "node:path";
import {
	type InitializeResult,
	isJsonRpcNotification,
	isJsonRpcRequest,
	isJsonRpcResponse,
	JSON_RPC_ERROR_CODES,
	type JsonRpcId,
	type JsonRpcMessage,
	type JsonRpcRequest,
	McpAbortError,
	McpClient,
	McpError,
	type McpTransport,
} from "@earendil-works/pi-mcp";

/**
 * One process per stdio MCP server for the whole machine.
 *
 * MCP's stdio transport pairs one client with one server process, so every Pi conversation used to
 * start its own copy of every checked server. The gateway owns a single copy of each server and
 * serves it to all conversations over loopback streamable HTTP:
 *
 * - Each client gets its own MCP session (`Mcp-Session-Id`). Its `initialize` is answered from the
 *   result the server gave the gateway, so a new conversation does not start the server.
 * - Requests are forwarded through one `McpClient` per server, which owns the upstream request ids,
 *   progress tokens, and cancellation; replies go back to the session that asked.
 * - List results are cached until the server announces a change, so connecting a conversation
 *   (`tools/list`, `resources/list`, …) does not wake a server that was stopped for being idle.
 * - A server with no request in flight for `upstreamIdleMs` is stopped. The next request starts it
 *   again.
 * - Only the latest configuration of a server runs. A server is the entry `name` of the `mcp.json`
 *   at `source`; the gateway watches that file, and when the entry changes or goes away, the old
 *   process is retired: its sessions are forgotten (their next request gets 404, and Pi reconnects
 *   with the file as it is now), requests already running finish, then the process is stopped.
 *
 * The server sees one client: the gateway. It does not get the client capabilities (roots,
 * sampling, elicitation) of any one conversation, since a reply could not be routed back to the
 * right one. Per-conversation state inside a server is shared, which is the point of sharing it.
 */

/** Bumped when the HTTP surface between Pi and the gateway changes. Part of the state file name. */
export const GATEWAY_PROTOCOL = 2;

export const GATEWAY_CLIENT_INFO = { name: "smelt-mcp-gateway", version: String(GATEWAY_PROTOCOL) };

/** The fields of a stdio server entry that decide which process runs. */
export interface StdioServerSpec {
	/** Absolute path of the `mcp.json` the entry is read from; with `name`, it identifies the server. */
	source: string;
	name: string;
	command: string;
	args?: string[];
	env?: Record<string, string>;
	cwd?: string;
}

export type UpstreamTransportFactory = (spec: StdioServerSpec) => McpTransport;

/** Call `onChange` when the file at `path` may have changed; returns a function that stops watching. */
export type ConfigWatcher = (path: string, onChange: () => void) => () => void;

export interface McpGatewayOptions {
	/** Bearer token every request must carry. */
	token: string;
	createTransport: UpstreamTransportFactory;
	/** Stop a server after this long without a request in flight. */
	upstreamIdleMs: number;
	/** Forget a session after this long without a request and without an open stream. */
	sessionIdleMs: number;
	/** Timeout of the gateway's own `initialize` with a server. */
	initializeTimeoutMs?: number;
	/** Watch a server's `mcp.json`, to retire the server when its entry changes. Not watched without it. */
	watchConfig?: ConfigWatcher;
	/** Read the stdio servers of an `mcp.json`; defaults to `readStdioServerSpecs`. */
	readConfig?: (path: string) => Map<string, StdioServerSpec>;
	now?: () => number;
	log?: (line: string) => void;
}

/** Results that only change when the server announces it with a `list_changed` notification. */
const CACHED_LISTS: Record<string, string> = {
	"tools/list": "notifications/tools/list_changed",
	"resources/list": "notifications/resources/list_changed",
	"resources/templates/list": "notifications/resources/list_changed",
	"prompts/list": "notifications/prompts/list_changed",
};

/** Notifications every session of a server gets. */
const BROADCAST_NOTIFICATIONS = [
	"notifications/tools/list_changed",
	"notifications/resources/list_changed",
	"notifications/prompts/list_changed",
	"notifications/resources/updated",
];

const SSE_KEEPALIVE_MS = 15_000;
const STDERR_TAIL_CHARS = 2_000;
const REQUEST_CANCELLED = -32800;

/**
 * Stable id of a server spec: the same entry of the same `mcp.json` with the same command, arguments,
 * environment, and directory shares a process.
 */
export function upstreamKey(spec: StdioServerSpec): string {
	const env = Object.entries(spec.env ?? {}).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
	const canonical = JSON.stringify([
		spec.source,
		spec.name,
		spec.command,
		spec.args ?? [],
		env,
		spec.cwd ?? null,
	]);
	return createHash("sha256").update(canonical).digest("hex").slice(0, 32);
}

/** A stdio server spec from untrusted JSON, or an error message. */
export function parseStdioServerSpec(value: unknown): StdioServerSpec | string {
	if (!isRecord(value)) return "server spec must be an object";
	const { source, name, command, args, env, cwd } = value;
	if (typeof source !== "string" || !isAbsolute(source)) return "source must be an absolute path";
	if (typeof name !== "string" || name.length === 0) return "name must be a non-empty string";
	if (typeof command !== "string" || command.length === 0) return "command must be a non-empty string";
	if (args !== undefined && !(Array.isArray(args) && args.every((arg) => typeof arg === "string"))) {
		return "args must be an array of strings";
	}
	if (env !== undefined && !(isRecord(env) && Object.values(env).every((item) => typeof item === "string"))) {
		return "env must map names to strings";
	}
	if (cwd !== undefined && typeof cwd !== "string") return "cwd must be a string";
	return {
		source,
		name,
		command,
		...(args === undefined ? {} : { args: [...(args as string[])] }),
		...(env === undefined ? {} : { env: { ...(env as Record<string, string>) } }),
		...(cwd === undefined ? {} : { cwd }),
	};
}

/**
 * The enabled stdio servers of the `mcp.json` at `path`, by name. A missing file has none; a file
 * that cannot be read or parsed throws, so a half-written file is not taken for an empty one.
 * Entries are taken as Pi takes them: HTTP entries (`url`) and invalid ones are left out.
 */
export function readStdioServerSpecs(path: string): Map<string, StdioServerSpec> {
	let text: string;
	try {
		text = readFileSync(path, "utf8");
	} catch (error) {
		if ((error as { code?: unknown }).code === "ENOENT") return new Map();
		throw error;
	}
	return stdioServerSpecs(JSON.parse(text), path);
}

/** The enabled stdio servers of a parsed `mcp.json` read from `source`. */
export function stdioServerSpecs(document: unknown, source: string): Map<string, StdioServerSpec> {
	const specs = new Map<string, StdioServerSpec>();
	if (!isRecord(document) || !isRecord(document.mcpServers)) return specs;
	for (const [name, entry] of Object.entries(document.mcpServers)) {
		if (!isRecord(entry) || entry.enabled === false || "url" in entry) continue;
		const spec = parseStdioServerSpec({
			source,
			name,
			command: entry.command,
			args: entry.args,
			env: entry.env,
			cwd: entry.cwd,
		});
		if (typeof spec !== "string") specs.set(name, spec);
	}
	return specs;
}

class Session {
	readonly streams = new Set<ReadableStreamDefaultController<Uint8Array>>();
	/** Requests of this session still running upstream, by the id the client gave them. */
	readonly pending = new Map<string, AbortController>();
	lastSeen: number;

	constructor(
		readonly id: string,
		readonly upstream: Upstream,
		now: number,
	) {
		this.lastSeen = now;
	}

	send(message: JsonRpcMessage): void {
		const frame = new TextEncoder().encode(`event: message\ndata: ${JSON.stringify(message)}\n\n`);
		for (const stream of this.streams) {
			try {
				stream.enqueue(frame);
			} catch {
				this.streams.delete(stream);
			}
		}
	}

	close(): void {
		for (const controller of this.pending.values()) controller.abort("session closed");
		this.pending.clear();
		this.closeStreams();
	}

	/** End the server-to-client streams; requests still running keep going and answer on their POST. */
	closeStreams(): void {
		for (const stream of this.streams) {
			try {
				stream.close();
			} catch {}
		}
		this.streams.clear();
	}
}

class Upstream {
	readonly key: string;
	readonly sessions = new Set<Session>();
	/** What the server answered the gateway's `initialize`; kept while the server is stopped. */
	init?: InitializeResult;
	private client?: McpClient;
	private starting?: Promise<McpClient>;
	private readonly lists = new Map<string, unknown>();
	/** Bumped by every `list_changed`, so a list answered before the change is not cached after it. */
	private listGeneration = 0;
	inflight = 0;
	lastUsed: number;
	/** Replaced by a newer configuration: takes no new sessions and stops once its requests finish. */
	retired = false;

	constructor(
		readonly spec: StdioServerSpec,
		private readonly gateway: McpGateway,
	) {
		this.key = upstreamKey(spec);
		this.lastUsed = gateway.now();
	}

	/** Name and short key in log lines, so two configurations of one server can be told apart. */
	get label(): string {
		return `${this.spec.name} ${this.key.slice(0, 8)}`;
	}

	get running(): boolean {
		return this.client !== undefined || this.starting !== undefined;
	}

	getClient(): Promise<McpClient> {
		if (this.client?.connectionState === "connected") return Promise.resolve(this.client);
		if (this.retired) return Promise.reject(new Error(`MCP server "${this.spec.name}" was reconfigured; retry`));
		this.starting ??= this.start().finally(() => {
			this.starting = undefined;
		});
		return this.starting;
	}

	private async start(): Promise<McpClient> {
		const client = new McpClient({
			...GATEWAY_CLIENT_INFO,
			requestTimeoutMs: this.gateway.options.initializeTimeoutMs ?? 60_000,
		});
		const label = this.label;
		for (const notification of BROADCAST_NOTIFICATIONS) {
			client.onNotification(notification, (params) => {
				this.dropLists(notification);
				this.broadcast({
					jsonrpc: "2.0",
					method: notification,
					...(isRecord(params) ? { params } : {}),
				});
			});
		}
		client.onNotification("notifications/message", (params) => {
			this.gateway.log(`[${label}] ${JSON.stringify(params)}`);
		});
		client.onError((error) => this.gateway.log(`[${label}] ${error.message}`));
		client.onClose(() => {
			if (this.client === client) this.client = undefined;
			this.gateway.log(`[${label}] stopped`);
		});
		let transport: McpTransport | undefined;
		try {
			transport = this.gateway.options.createTransport(this.spec);
			this.init = await client.connect(transport);
		} catch (error) {
			const stderr = stderrTail(transport);
			throw new Error(`${errorMessage(error)}${stderr ? `\n${stderr}` : ""}`);
		}
		this.client = client;
		this.gateway.log(`[${label}] started`);
		if (this.retired) {
			// Reconfigured while starting: the requests waiting for it fail and Pi retries on the new one.
			await this.stop();
			throw new Error(`MCP server "${this.spec.name}" was reconfigured; retry`);
		}
		return client;
	}

	async stop(): Promise<void> {
		const client = this.client;
		this.client = undefined;
		await client?.close().catch(() => undefined);
	}

	async initialize(): Promise<InitializeResult> {
		if (this.init) return this.init;
		await this.getClient();
		if (!this.init) throw new Error(`MCP server "${this.spec.name}" did not initialize`);
		return this.init;
	}

	async forward(session: Session, message: JsonRpcRequest): Promise<JsonRpcMessage> {
		const params = isRecord(message.params) ? message.params : undefined;
		const listKey = message.method in CACHED_LISTS ? `${message.method} ${JSON.stringify(params ?? {})}` : undefined;
		if (listKey !== undefined && this.lists.has(listKey)) {
			return { jsonrpc: "2.0", id: message.id, result: this.lists.get(listKey) as Record<string, unknown> };
		}
		const requestKey = idKey(message.id);
		const controller = new AbortController();
		session.pending.set(requestKey, controller);
		this.inflight++;
		this.lastUsed = this.gateway.now();
		const { params: forwarded, progressToken } = takeProgressToken(params);
		const generation = this.listGeneration;
		try {
			const client = await this.getClient();
			const result = await client.request<Record<string, unknown>>(message.method, forwarded, {
				signal: controller.signal,
				// The client set its own timeout and cancels when it runs out; the gateway does not add one.
				timeoutMs: 0,
				...(progressToken === undefined
					? {}
					: {
							onProgress: (progress) =>
								session.send({
									jsonrpc: "2.0",
									method: "notifications/progress",
									params: { ...progress, progressToken },
								}),
						}),
			});
			if (listKey !== undefined && generation === this.listGeneration) this.lists.set(listKey, result);
			return { jsonrpc: "2.0", id: message.id, result: result ?? {} };
		} catch (error) {
			return { jsonrpc: "2.0", id: message.id, error: errorObject(error) };
		} finally {
			session.pending.delete(requestKey);
			this.inflight--;
			this.lastUsed = this.gateway.now();
			if (this.retired && this.inflight === 0) void this.stop();
		}
	}

	private dropLists(notification: string): void {
		this.listGeneration++;
		for (const key of [...this.lists.keys()]) {
			if (CACHED_LISTS[key.slice(0, key.indexOf(" "))] === notification) this.lists.delete(key);
		}
	}

	private broadcast(message: JsonRpcMessage): void {
		for (const session of this.sessions) session.send(message);
	}
}

export class McpGateway {
	private readonly upstreams = new Map<string, Upstream>();
	private readonly sessions = new Map<string, Session>();
	/** Stop functions of the watched `mcp.json` files, by path. */
	private readonly watchers = new Map<string, () => void>();

	constructor(readonly options: McpGatewayOptions) {}

	now(): number {
		return this.options.now?.() ?? Date.now();
	}

	log(line: string): void {
		this.options.log?.(line);
	}

	/**
	 * Register a server spec and return the key of its endpoint, `/mcp/<key>`. Clients read the spec
	 * from `mcp.json` when they connect, so a new spec for a server is its latest configuration and
	 * replaces the old one.
	 */
	register(spec: StdioServerSpec): string {
		const key = upstreamKey(spec);
		if (this.upstreams.has(key)) return key;
		for (const other of [...this.upstreams.values()]) {
			if (other.spec.source === spec.source && other.spec.name === spec.name) this.retire(other, "reconfigured");
		}
		this.upstreams.set(key, new Upstream(spec, this));
		this.watch(spec.source);
		return key;
	}

	/**
	 * Compare the servers read from `source` with the file: retire the ones whose entry changed or is
	 * gone. A file that cannot be read keeps everything, so a half-written file does not stop servers.
	 */
	reconcile(source: string): void {
		let specs: Map<string, StdioServerSpec>;
		try {
			specs = (this.options.readConfig ?? readStdioServerSpecs)(source);
		} catch (error) {
			this.log(`reading ${source} failed, keeping its servers: ${errorMessage(error)}`);
			return;
		}
		for (const upstream of [...this.upstreams.values()]) {
			if (upstream.spec.source !== source) continue;
			const current = specs.get(upstream.spec.name);
			if (!current) this.retire(upstream, "removed or disabled in mcp.json");
			else if (upstreamKey(current) !== upstream.key) this.retire(upstream, "reconfigured");
		}
		this.unwatchUnused();
	}

	/**
	 * Take an old configuration out of service. Its sessions are forgotten, so their next request gets
	 * 404 and the client reconnects with the latest configuration; requests already running finish
	 * first, then the process stops.
	 */
	private retire(upstream: Upstream, reason: string): void {
		if (upstream.retired) return;
		upstream.retired = true;
		this.upstreams.delete(upstream.key);
		for (const session of upstream.sessions) {
			this.sessions.delete(session.id);
			session.closeStreams();
		}
		upstream.sessions.clear();
		this.log(`[${upstream.label}] ${reason}, retiring`);
		if (upstream.inflight === 0) void upstream.stop();
	}

	private watch(source: string): void {
		const watchConfig = this.options.watchConfig;
		if (!watchConfig || this.watchers.has(source)) return;
		try {
			this.watchers.set(
				source,
				watchConfig(source, () => this.reconcile(source)),
			);
		} catch (error) {
			this.log(`watching ${source} failed: ${errorMessage(error)}`);
		}
	}

	/** Stop watching files that no registered server comes from. */
	private unwatchUnused(): void {
		const used = new Set([...this.upstreams.values()].map((upstream) => upstream.spec.source));
		for (const [source, stop] of [...this.watchers]) {
			if (used.has(source)) continue;
			stop();
			this.watchers.delete(source);
		}
	}

	/** Servers and sessions, for `/health` and tests. */
	status(): {
		servers: Array<{ source: string; name: string; key: string; running: boolean; sessions: number }>;
		sessions: number;
	} {
		return {
			servers: [...this.upstreams.values()].map((upstream) => ({
				source: upstream.spec.source,
				name: upstream.spec.name,
				key: upstream.key,
				running: upstream.running,
				sessions: upstream.sessions.size,
			})),
			sessions: this.sessions.size,
		};
	}

	/** Nothing to serve: no session and no running server. */
	isIdle(): boolean {
		return this.sessions.size === 0 && [...this.upstreams.values()].every((upstream) => !upstream.running);
	}

	/** Stop idle servers and forget sessions whose client went away. */
	async sweep(): Promise<void> {
		const now = this.now();
		for (const session of [...this.sessions.values()]) {
			if (session.streams.size > 0 || session.pending.size > 0) continue;
			if (now - session.lastSeen >= this.options.sessionIdleMs) this.dropSession(session);
		}
		const stops: Promise<void>[] = [];
		for (const upstream of [...this.upstreams.values()]) {
			if (upstream.inflight > 0 || now - upstream.lastUsed < this.options.upstreamIdleMs) continue;
			if (upstream.running) {
				this.log(`[${upstream.label}] idle for ${Math.round((now - upstream.lastUsed) / 1000)}s, stopping`);
				stops.push(upstream.stop());
			} else if (upstream.sessions.size === 0) {
				this.upstreams.delete(upstream.key);
			}
		}
		this.unwatchUnused();
		await Promise.all(stops);
	}

	async close(): Promise<void> {
		for (const session of [...this.sessions.values()]) this.dropSession(session);
		await Promise.all([...this.upstreams.values()].map((upstream) => upstream.stop()));
		this.upstreams.clear();
		this.unwatchUnused();
	}

	async handle(request: Request): Promise<Response> {
		// Browsers send Origin; the gateway only serves local processes (DNS rebinding protection).
		if (request.headers.get("origin")) return text(403, "forbidden");
		if (!this.authorized(request.headers.get("authorization"))) return text(401, "unauthorized");
		const url = new URL(request.url);
		if (url.pathname === "/health" && request.method === "GET") {
			return Response.json({ ok: true, protocol: GATEWAY_PROTOCOL, pid: process.pid, ...this.status() });
		}
		if (url.pathname === "/servers" && request.method === "POST") {
			const spec = parseStdioServerSpec(await request.json().catch(() => undefined));
			if (typeof spec === "string") return text(400, spec);
			return Response.json({ key: this.register(spec) });
		}
		const match = /^\/mcp\/([0-9a-f]{32})$/.exec(url.pathname);
		const upstream = match ? this.upstreams.get(match[1]!) : undefined;
		if (!upstream) return text(404, "unknown MCP server");
		switch (request.method) {
			case "POST":
				return this.post(upstream, request);
			case "GET":
				return this.openStream(upstream, request);
			case "DELETE": {
				const session = this.sessionOf(upstream, request);
				if (session) this.dropSession(session);
				return new Response(null, { status: 204 });
			}
			default:
				return text(405, "method not allowed");
		}
	}

	private authorized(header: string | null): boolean {
		const expected = Buffer.from(`Bearer ${this.options.token}`);
		const actual = Buffer.from(header ?? "");
		return actual.length === expected.length && timingSafeEqual(actual, expected);
	}

	private sessionOf(upstream: Upstream, request: Request): Session | undefined {
		const id = request.headers.get("mcp-session-id");
		const session = id ? this.sessions.get(id) : undefined;
		return session?.upstream === upstream ? session : undefined;
	}

	private dropSession(session: Session): void {
		session.close();
		this.sessions.delete(session.id);
		session.upstream.sessions.delete(session);
	}

	private async post(upstream: Upstream, request: Request): Promise<Response> {
		let message: unknown;
		try {
			message = await request.json();
		} catch {
			return jsonRpcError(400, null, JSON_RPC_ERROR_CODES.parseError, "Parse error");
		}
		if (isJsonRpcRequest(message as JsonRpcMessage) && (message as JsonRpcRequest).method === "initialize") {
			return this.initialize(upstream, message as JsonRpcRequest);
		}
		const session = this.sessionOf(upstream, request);
		if (!session) {
			// 404 with a session id tells the client to start a new session; without one it is a bad request.
			return request.headers.get("mcp-session-id") ? text(404, "session not found") : text(400, "missing session");
		}
		session.lastSeen = this.now();
		const rpc = message as JsonRpcMessage;
		if (isJsonRpcRequest(rpc)) {
			if (rpc.method === "ping") return Response.json({ jsonrpc: "2.0", id: rpc.id, result: {} });
			const abort = () => session.pending.get(idKey(rpc.id))?.abort("client disconnected");
			request.signal.addEventListener("abort", abort, { once: true });
			try {
				return Response.json(await upstream.forward(session, rpc));
			} finally {
				request.signal.removeEventListener("abort", abort);
			}
		}
		if (isJsonRpcNotification(rpc)) {
			if (rpc.method === "notifications/cancelled" && isRecord(rpc.params) && isJsonRpcId(rpc.params.requestId)) {
				const reason = typeof rpc.params.reason === "string" ? rpc.params.reason : "cancelled";
				session.pending.get(idKey(rpc.params.requestId))?.abort(reason);
			}
			// `initialized` and `roots/list_changed` belong to this session only; the server already has its own.
			return new Response(null, { status: 202 });
		}
		if (isJsonRpcResponse(rpc)) return new Response(null, { status: 202 });
		return jsonRpcError(400, null, JSON_RPC_ERROR_CODES.invalidRequest, "Invalid request");
	}

	private async initialize(upstream: Upstream, message: JsonRpcRequest): Promise<Response> {
		let init: InitializeResult;
		try {
			init = await upstream.initialize();
		} catch (error) {
			return Response.json({ jsonrpc: "2.0", id: message.id, error: errorObject(error) });
		}
		const session = new Session(randomUUID(), upstream, this.now());
		// Retired while it started: no session on it; the client gets the latest configuration on retry.
		if (upstream.retired) return text(404, "MCP server was reconfigured");
		this.sessions.set(session.id, session);
		upstream.sessions.add(session);
		return Response.json({ jsonrpc: "2.0", id: message.id, result: init }, { headers: { "mcp-session-id": session.id } });
	}

	private openStream(upstream: Upstream, request: Request): Response {
		if (!(request.headers.get("accept") ?? "").includes("text/event-stream")) {
			return text(406, "the stream needs Accept: text/event-stream");
		}
		const session = this.sessionOf(upstream, request);
		if (!session) return text(request.headers.get("mcp-session-id") ? 404 : 400, "session not found");
		let controller: ReadableStreamDefaultController<Uint8Array> | undefined;
		let keepalive: ReturnType<typeof setInterval> | undefined;
		const detach = () => {
			if (keepalive) clearInterval(keepalive);
			if (controller) session.streams.delete(controller);
			session.lastSeen = this.now();
		};
		const stream = new ReadableStream<Uint8Array>({
			start: (opened) => {
				controller = opened;
				session.streams.add(opened);
				opened.enqueue(new TextEncoder().encode(": connected\n\n"));
				keepalive = setInterval(() => {
					try {
						opened.enqueue(new TextEncoder().encode(": keepalive\n\n"));
					} catch {
						detach();
					}
				}, SSE_KEEPALIVE_MS);
			},
			cancel: detach,
		});
		request.signal.addEventListener("abort", detach, { once: true });
		return new Response(stream, {
			headers: {
				"content-type": "text/event-stream",
				"cache-control": "no-cache",
				"mcp-session-id": session.id,
			},
		});
	}
}

/** Progress tokens belong to the client; the gateway's `McpClient` sets its own and maps progress back. */
function takeProgressToken(params: Record<string, unknown> | undefined): {
	params: Record<string, unknown> | undefined;
	progressToken: string | number | undefined;
} {
	if (!params || !isRecord(params._meta) || !isJsonRpcId(params._meta.progressToken)) {
		return { params, progressToken: undefined };
	}
	const { progressToken, ...meta } = params._meta;
	const { _meta: _, ...rest } = params;
	return {
		params: Object.keys(meta).length > 0 ? { ...rest, _meta: meta } : rest,
		progressToken: progressToken as string | number,
	};
}

function errorObject(error: unknown): { code: number; message: string; data?: unknown } {
	if (error instanceof McpError) {
		return { code: error.code, message: error.message, ...(error.data === undefined ? {} : { data: error.data }) };
	}
	if (error instanceof McpAbortError) return { code: REQUEST_CANCELLED, message: "Request cancelled" };
	return { code: JSON_RPC_ERROR_CODES.internalError, message: errorMessage(error) };
}

function stderrTail(transport: McpTransport | undefined): string | undefined {
	const stderr = (transport as { stderr?: unknown } | undefined)?.stderr;
	return typeof stderr === "string" ? stderr.trim().slice(-STDERR_TAIL_CHARS) || undefined : undefined;
}

function idKey(id: JsonRpcId): string {
	return `${typeof id}:${String(id)}`;
}

function isJsonRpcId(value: unknown): value is JsonRpcId {
	return typeof value === "string" || (typeof value === "number" && Number.isFinite(value));
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function text(status: number, body: string): Response {
	return new Response(body, { status, headers: { "content-type": "text/plain; charset=utf-8" } });
}

function jsonRpcError(status: number, id: JsonRpcId | null, code: number, message: string): Response {
	return Response.json({ jsonrpc: "2.0", id, error: { code, message } }, { status });
}
