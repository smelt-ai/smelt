#!/usr/bin/env bun

/**
 * The shared MCP gateway process (see `mcp-gateway.ts`). Started detached by the first Pi
 * conversation that needs a stdio server (`mcp-gateway-client.ts`); exits on its own once no
 * conversation is connected and no server is running.
 */

import { randomBytes } from "node:crypto";
import { watch } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname } from "node:path";
import {
	gatewayDir,
	removeGatewayStateIfOwned,
	writeGatewayState,
} from "./mcp-gateway-client.ts";
import { type ConfigWatcher, GATEWAY_PROTOCOL, McpGateway } from "./mcp-gateway.ts";
import { createPiDefaultTransport } from "./pi-mcp-runtime.ts";

const MINUTE = 60_000;

function duration(name: string, fallback: number): number {
	const value = Number(process.env[name]);
	return Number.isFinite(value) && value > 0 ? value : fallback;
}

/** Stop a server that had no request for this long. Its next request starts it again. */
const UPSTREAM_IDLE_MS = duration("SMELT_MCP_GATEWAY_SERVER_IDLE_MS", 10 * MINUTE);
/** Forget a session whose client neither streams nor asks for this long (a crashed conversation). */
const SESSION_IDLE_MS = duration("SMELT_MCP_GATEWAY_SESSION_IDLE_MS", 30 * MINUTE);
/** Exit after this long with nothing to serve. */
const GATEWAY_IDLE_MS = duration("SMELT_MCP_GATEWAY_IDLE_MS", 5 * MINUTE);
/** `setTimeout` takes at most this many milliseconds; a later deadline is reached by re-arming. */
const MAX_TIMER_MS = 2 ** 31 - 1;
/** Editors and atomic writes touch the file several times per save; act once it settles. */
const CONFIG_SETTLE_MS = 200;

process.title = "smelt-mcp-gateway";
process.emitWarning = (() => {}) as typeof process.emitWarning;

const dir = process.env.SMELT_MCP_GATEWAY_DIR?.trim() || gatewayDir();
delete process.env.SMELT_MCP_GATEWAY_DIR;

function log(line: string): void {
	process.stderr.write(`${new Date().toISOString()} ${line}\n`);
}

/**
 * Watch the directory rather than the file: saving by rename (atomic writes, most editors) replaces
 * the file, and a watch on the old file would see nothing after the first save.
 */
const watchConfig: ConfigWatcher = (path, onChange) => {
	const name = basename(path);
	let timer: ReturnType<typeof setTimeout> | undefined;
	const watcher = watch(dirname(path), (_event, file) => {
		if (file !== null && file !== name) return;
		clearTimeout(timer);
		timer = setTimeout(onChange, CONFIG_SETTLE_MS);
	});
	watcher.on("error", (error) => log(`watching ${path} failed: ${error.message}`));
	return () => {
		clearTimeout(timer);
		watcher.close();
	};
};

const token = randomBytes(32).toString("hex");
const gateway = new McpGateway({
	token,
	// Servers run from the home directory: they are shared, so no one conversation's directory is theirs.
	createTransport: (spec) =>
		createPiDefaultTransport(
			{
				name: spec.name,
				config: {
					command: spec.command,
					...(spec.args === undefined ? {} : { args: spec.args }),
					...(spec.env === undefined ? {} : { env: spec.env }),
					...(spec.cwd === undefined ? {} : { cwd: spec.cwd }),
				},
				source: "smelt-mcp-gateway",
			},
			homedir(),
			undefined,
		),
	upstreamIdleMs: UPSTREAM_IDLE_MS,
	sessionIdleMs: SESSION_IDLE_MS,
	idleExitMs: GATEWAY_IDLE_MS,
	onActivity: () => armDeadline(),
	watchConfig,
	log,
});

const server = Bun.serve({
	hostname: "127.0.0.1",
	port: 0,
	// Tool calls and server-to-client streams stay open far longer than Bun's default 10s.
	idleTimeout: 0,
	fetch: (request) => gateway.handle(request),
});

// Written once listening: the conversation that started the gateway, and any waiting for it, take the
// state file as the signal that it is ready (`mcp-gateway-client.ts`).
writeGatewayState(dir, { protocol: GATEWAY_PROTOCOL, pid: process.pid, port: server.port ?? 0, token });
log(`listening on 127.0.0.1:${server.port} (pid ${process.pid})`);

let shuttingDown = false;
async function shutdown(reason: string): Promise<never> {
	if (!shuttingDown) {
		shuttingDown = true;
		log(`exiting: ${reason}`);
		removeGatewayStateIfOwned(dir, process.pid);
		await Promise.race([gateway.close(), new Promise((resolve) => setTimeout(resolve, 5_000))]);
		server.stop(true);
	}
	process.exit(0);
}

/**
 * One timer for the nearest deadline (a server or session going idle, or the gateway's own idle exit),
 * re-armed whenever activity moves it. Nothing runs while no deadline is pending.
 */
let deadlineTimer: ReturnType<typeof setTimeout> | undefined;
function armDeadline(): void {
	clearTimeout(deadlineTimer);
	deadlineTimer = undefined;
	if (shuttingDown) return;
	const at = gateway.nextDeadline();
	if (at === undefined) return;
	deadlineTimer = setTimeout(() => void onDeadline(), Math.min(Math.max(0, at - Date.now()), MAX_TIMER_MS));
}

async function onDeadline(): Promise<void> {
	deadlineTimer = undefined;
	// The sweep reports its changes, which arms the timer for the next deadline.
	await gateway.sweep();
	if (gateway.idleExitDue()) void shutdown("idle");
}

armDeadline();

process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));
process.on("SIGHUP", () => {});
