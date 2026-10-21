import { spawn } from "node:child_process";
import {
	closeSync,
	mkdirSync,
	openSync,
	readFileSync,
	renameSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { McpServerEntry, McpTransportFactory } from "@earendil-works/pi-coding-agent";
import {
	type JsonRpcMessage,
	McpConnectionClosedError,
	McpHttpError,
	McpSessionExpiredError,
	type McpTransport,
	StreamableHttpTransport,
} from "@earendil-works/pi-mcp";
import lockfile from "proper-lockfile";
import { GATEWAY_PROTOCOL, readStdioServerSpecs, type StdioServerSpec } from "./mcp-gateway.ts";
import { mcpServerEnvironment } from "./pi-mcp-runtime.ts";

/**
 * Pi side of the shared MCP gateway: find it, start it when it is not running, and connect stdio
 * servers through it.
 *
 * The gateway is a detached process with its own session, so ending a conversation (which kills
 * the conversation's process group) does not take it down. It records where it listens in a state
 * file; whoever finds no healthy gateway starts one under a lock, so concurrent conversations end
 * up with the same one. A gateway that died or was replaced is found again on the next connect.
 */

export interface GatewayState {
	protocol: number;
	pid: number;
	port: number;
	token: string;
}

export interface GatewayEndpoint {
	base: string;
	token: string;
	pid: number;
}

const SPAWN_TIMEOUT_MS = 15_000;
const HEALTH_TIMEOUT_MS = 1_000;
const LOG_ROTATE_BYTES = 1024 * 1024;

export function gatewayDir(environment: Record<string, string | undefined> = process.env): string {
	const home = environment.SMELT_HOME?.trim() || join(homedir(), ".smelt");
	return join(home, "run", "mcp-gateway");
}

export function gatewayStatePath(dir: string): string {
	return join(dir, `gateway-v${GATEWAY_PROTOCOL}.json`);
}

export function readGatewayState(dir: string): GatewayState | undefined {
	try {
		const value = JSON.parse(readFileSync(gatewayStatePath(dir), "utf8")) as Partial<GatewayState>;
		if (
			value.protocol === GATEWAY_PROTOCOL &&
			typeof value.pid === "number" &&
			typeof value.port === "number" &&
			typeof value.token === "string"
		) {
			return value as GatewayState;
		}
	} catch {}
	return undefined;
}

/** Atomic and private: the token in it grants access to every shared server. */
export function writeGatewayState(dir: string, state: GatewayState): void {
	mkdirSync(dir, { recursive: true, mode: 0o700 });
	const path = gatewayStatePath(dir);
	const temporary = `${path}.${state.pid}.tmp`;
	writeFileSync(temporary, JSON.stringify(state), { mode: 0o600 });
	renameSync(temporary, path);
}

export function removeGatewayStateIfOwned(dir: string, pid: number): void {
	if (readGatewayState(dir)?.pid === pid) rmSync(gatewayStatePath(dir), { force: true });
}

export function endpointOf(state: GatewayState): GatewayEndpoint {
	return { base: `http://127.0.0.1:${state.port}`, token: state.token, pid: state.pid };
}

async function healthy(endpoint: GatewayEndpoint): Promise<boolean> {
	try {
		const response = await fetch(`${endpoint.base}/health`, {
			headers: { authorization: `Bearer ${endpoint.token}` },
			signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS),
		});
		if (!response.ok) return false;
		const body = (await response.json()) as { pid?: unknown; protocol?: unknown };
		return body.pid === endpoint.pid && body.protocol === GATEWAY_PROTOCOL;
	} catch {
		return false;
	}
}

async function runningGateway(dir: string): Promise<GatewayEndpoint | undefined> {
	const state = readGatewayState(dir);
	if (!state) return undefined;
	const endpoint = endpointOf(state);
	return (await healthy(endpoint)) ? endpoint : undefined;
}

function spawnGateway(dir: string): number {
	const log = join(dir, "gateway.log");
	try {
		if (statSync(log).size > LOG_ROTATE_BYTES) renameSync(log, `${log}.1`);
	} catch {}
	const output = openSync(log, "a", 0o600);
	try {
		// 编译后的 smelt-pi 认子命令 `gateway`。源码模式没有这条子命令：
		// `src/main.ts` 会在前面塞上 `--mode rpc`，所以这里直接跑网关入口。
		const gatewayArgs = import.meta.url.includes("$bunfs")
			? ["gateway"]
			: [fileURLToPath(new URL("./mcp-gateway-main.ts", import.meta.url))];
		const child = spawn(process.execPath, gatewayArgs, {
			// Own session: the conversation's process group is killed when it ends.
			detached: true,
			stdio: ["ignore", output, output],
			cwd: homedir(),
			// 网关和它起的共享 server 不属于哪一条会话：拉起网关的那条会话的 Smelt 变量不能带进去。
			env: { ...mcpServerEnvironment(process.env), SMELT_MCP_GATEWAY_DIR: dir },
		});
		child.unref();
		if (child.pid === undefined) throw new Error("MCP gateway did not start");
		return child.pid;
	} finally {
		closeSync(output);
	}
}

async function startGateway(dir: string): Promise<GatewayEndpoint> {
	mkdirSync(dir, { recursive: true, mode: 0o700 });
	const existing = await runningGateway(dir);
	if (existing) return existing;
	const release = await lockfile.lock(dir, {
		realpath: false,
		lockfilePath: join(dir, "spawn.lock"),
		stale: SPAWN_TIMEOUT_MS + 5_000,
		retries: { retries: 200, minTimeout: 50, maxTimeout: 200 },
	});
	try {
		const raced = await runningGateway(dir);
		if (raced) return raced;
		const pid = spawnGateway(dir);
		const deadline = Date.now() + SPAWN_TIMEOUT_MS;
		while (Date.now() < deadline) {
			await new Promise((resolve) => setTimeout(resolve, 50));
			const state = readGatewayState(dir);
			if (state?.pid === pid && (await healthy(endpointOf(state)))) return endpointOf(state);
		}
		throw new Error(`MCP gateway did not come up in ${SPAWN_TIMEOUT_MS / 1000}s; see ${join(dir, "gateway.log")}`);
	} finally {
		await release().catch(() => undefined);
	}
}

let current: Promise<GatewayEndpoint> | undefined;
/** What `current` resolved to, so a dead gateway can be forgotten synchronously. */
let currentEndpoint: GatewayEndpoint | undefined;

/** The running gateway, started when there is none. Shared by all servers of this process. */
export function ensureGateway(dir = gatewayDir()): Promise<GatewayEndpoint> {
	if (current) return current;
	const attempt = startGateway(dir);
	current = attempt;
	attempt.then(
		(endpoint) => {
			if (current === attempt) currentEndpoint = endpoint;
		},
		() => {
			if (current === attempt) current = undefined;
		},
	);
	return attempt;
}

/** The gateway at `endpoint` stopped answering; the next connect looks again. */
export function forgetGateway(endpoint: GatewayEndpoint | undefined): void {
	if (!endpoint || currentEndpoint?.pid !== endpoint.pid) return;
	current = undefined;
	currentEndpoint = undefined;
}

/** Connection refused or reset before anything was sent: the request did not reach a gateway. */
function isUnreachable(error: unknown): boolean {
	const codes = [
		(error as { code?: unknown } | undefined)?.code,
		(error as { cause?: { code?: unknown } } | undefined)?.cause?.code,
	];
	return codes.some((code) => code === "ConnectionRefused" || code === "ECONNREFUSED");
}

async function registerServer(endpoint: GatewayEndpoint, spec: StdioServerSpec): Promise<string> {
	const response = await fetch(`${endpoint.base}/servers`, {
		method: "POST",
		headers: { authorization: `Bearer ${endpoint.token}`, "content-type": "application/json" },
		body: JSON.stringify(spec),
		signal: AbortSignal.timeout(5_000),
	});
	if (!response.ok) {
		throw new Error(`MCP gateway rejected server "${spec.name}": ${(await response.text()).slice(0, 500)}`);
	}
	const { key } = (await response.json()) as { key?: unknown };
	if (typeof key !== "string") throw new Error("MCP gateway answered without a server key");
	return key;
}

/**
 * The current spec of a server, read from its `mcp.json` again. Pi keeps the entry it read when the
 * conversation started; reading the file on every connect is what makes a conversation that
 * reconnects (for example after the gateway retired the old configuration) use the latest one.
 */
export function currentStdioServerSpec(spec: StdioServerSpec): StdioServerSpec {
	const current = readStdioServerSpecs(spec.source).get(spec.name);
	if (!current) throw new Error(`MCP server "${spec.name}" is no longer enabled in ${spec.source}`);
	return current;
}

/**
 * A stdio server reached through the gateway. To Pi it looks like any transport; it finds (or
 * starts) the gateway in `start()`, so a gateway that went away is replaced on reconnect.
 *
 * A request that could not reach the gateway (refused, or rejected because the gateway was
 * replaced) is reported as an expired session: Pi then retries it once on a new connection, which is
 * safe because the server never saw it.
 */
export class GatewayTransport implements McpTransport {
	private inner?: StreamableHttpTransport;
	private endpoint?: GatewayEndpoint;
	private closed = false;
	private protocolVersion?: string;
	private readonly messageListeners = new Set<(message: JsonRpcMessage) => void>();
	private readonly errorListeners = new Set<(error: Error) => void>();
	private readonly closeListeners = new Set<() => void>();
	private closeEmitted = false;

	constructor(
		public spec: StdioServerSpec,
		private readonly locate: () => Promise<GatewayEndpoint> = ensureGateway,
		private readonly resolve: (spec: StdioServerSpec) => StdioServerSpec = currentStdioServerSpec,
	) {}

	async start(): Promise<void> {
		if (this.closed) throw new McpConnectionClosedError();
		this.spec = this.resolve(this.spec);
		let endpoint = await this.locate();
		let key: string;
		try {
			key = await registerServer(endpoint, this.spec);
		} catch (error) {
			if (!isUnreachable(error)) throw error;
			forgetGateway(endpoint);
			endpoint = await this.locate();
			key = await registerServer(endpoint, this.spec);
		}
		if (this.closed) throw new McpConnectionClosedError();
		this.endpoint = endpoint;
		const inner = new StreamableHttpTransport({
			url: `${endpoint.base}/mcp/${key}`,
			headers: { authorization: `Bearer ${endpoint.token}` },
		});
		inner.onMessage((message) => {
			for (const listener of this.messageListeners) listener(message);
		});
		inner.onError((error) => {
			for (const listener of this.errorListeners) listener(error);
		});
		inner.onClose(() => this.emitClose());
		if (this.protocolVersion) inner.setProtocolVersion(this.protocolVersion);
		this.inner = inner;
		await inner.start();
	}

	async send(message: JsonRpcMessage): Promise<void> {
		const inner = this.inner;
		if (!inner || this.closed) throw new McpConnectionClosedError();
		try {
			await inner.send(message);
		} catch (error) {
			if (isUnreachable(error) || (error instanceof McpHttpError && error.status === 401)) {
				forgetGateway(this.endpoint);
				throw new McpSessionExpiredError(`MCP gateway for "${this.spec.name}" is gone`);
			}
			throw error;
		}
	}

	async close(): Promise<void> {
		if (this.closed) return;
		this.closed = true;
		if (this.inner) await this.inner.close();
		this.emitClose();
	}

	setProtocolVersion(version: string): void {
		this.protocolVersion = version;
		this.inner?.setProtocolVersion(version);
	}

	onMessage(listener: (message: JsonRpcMessage) => void): () => void {
		this.messageListeners.add(listener);
		return () => this.messageListeners.delete(listener);
	}

	onError(listener: (error: Error) => void): () => void {
		this.errorListeners.add(listener);
		return () => this.errorListeners.delete(listener);
	}

	onClose(listener: () => void): () => void {
		this.closeListeners.add(listener);
		return () => this.closeListeners.delete(listener);
	}

	private emitClose(): void {
		if (this.closeEmitted) return;
		this.closeEmitted = true;
		for (const listener of this.closeListeners) listener();
	}
}

/**
 * The transport-relevant part of a stdio entry read from the `mcp.json` at `source`. Other fields
 * only matter to the client.
 */
export function stdioServerSpec(entry: McpServerEntry, source: string): StdioServerSpec | undefined {
	const config = entry.config;
	if ("url" in config) return undefined;
	return {
		source,
		name: entry.name,
		command: config.command,
		...(config.args === undefined ? {} : { args: [...config.args] }),
		...(config.env === undefined ? {} : { env: { ...config.env } }),
		...(config.cwd === undefined ? {} : { cwd: config.cwd }),
	};
}

/** Stdio servers of the `mcp.json` at `source` through the shared gateway; HTTP servers as Pi connects them. */
export function sharedTransportFactory(
	fallback: McpTransportFactory,
	source: string,
	connect: (spec: StdioServerSpec) => McpTransport = (spec) => new GatewayTransport(spec),
): McpTransportFactory {
	return (entry, cwd, authProvider) => {
		const spec = stdioServerSpec(entry, source);
		return spec ? connect(spec) : fallback(entry, cwd, authProvider);
	};
}
