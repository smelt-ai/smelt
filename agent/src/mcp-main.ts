#!/usr/bin/env bun

/**
 * `pi mcp <args>` through Smelt's managed runtime.
 *
 * `list` and the other commands are forwarded to Pi's own `main` unchanged.
 *
 * `login <server>` is how the GUI signs in to remote MCP servers (Figma, …), and it speaks the
 * NDJSON protocol of `auth-protocol.ts` instead of printing for a terminal. It runs on the runtime
 * machine, which may have no desktop at all (a server, a container); the user approves access in a
 * browser in front of them, possibly on another device. So the helper never opens a browser: the
 * authorization URL goes to the host as an `auth_url` event, and the host opens or shows it where
 * the user is. When that browser cannot reach this machine's loopback callback, the user pastes
 * the address it was redirected to and the helper delivers it to the callback here.
 *
 * Pi still runs the whole OAuth flow (registration, PKCE, loopback callback, `mcp-auth.json`)
 * through `runMcpCommand`, the entry its `main` dispatches `mcp` to, with the host hooks it offers.
 *
 * Usage:
 *   bun src/mcp-main.ts login <server> [--timeout <seconds>]
 *   bun src/mcp-main.ts list
 */

import { get } from "node:http";
import { getAgentDir, main, SettingsManager } from "@earendil-works/pi-coding-agent";
// Not package exports: files inside the pinned package, like `pi-mcp-runtime.ts` does; the compile
// fails if an upgrade moves them. They are what `main` runs before dispatching `mcp`.
import {
	applyHttpProxySettings,
	configureHttpDispatcher,
} from "../node_modules/@earendil-works/pi-coding-agent/dist/core/http-dispatcher.js";
import { runMcpCommand } from "../node_modules/@earendil-works/pi-coding-agent/dist/extensions/mcp/cli.js";
import { decodeCommand, encodeEvent, type HostEvent, takeLines } from "./auth-protocol.ts";
import { callbackTarget } from "./mcp-login.ts";

process.title = "pi-mcp";
process.emitWarning = (() => {}) as typeof process.emitWarning;

function emit(event: HostEvent): void {
	process.stdout.write(encodeEvent(event));
}

/** Request the loopback callback on this machine; resolves to the HTTP status. */
function deliver(url: URL): Promise<number> {
	return new Promise((resolve, reject) => {
		// node:http, not fetch: Pi's dispatcher may send fetch through the configured proxy.
		const request = get(url, (response) => {
			response.resume();
			resolve(response.statusCode ?? 0);
		});
		request.on("error", reject);
		request.setTimeout(15_000, () => request.destroy(new Error("回调没有响应")));
	});
}

async function hostLogin(args: string[]): Promise<number> {
	const cwd = process.cwd();
	const agentDir = getAgentDir();
	applyHttpProxySettings(SettingsManager.create(cwd, agentDir, { projectTrusted: false }).getGlobalSettings().httpProxy);
	configureHttpDispatcher();

	let authorizationUrl: string | undefined;
	let promptId = 0;
	let promptOpen = false;
	const ask = () => {
		promptOpen = true;
		emit({
			event: "prompt",
			id: String(++promptId),
			promptType: "manual_code",
			message: "授权后浏览器打不开 127.0.0.1 的页面时（比如在手机或另一台电脑上授权），把地址栏里的完整地址粘贴到这里",
			placeholder: "http://127.0.0.1:…/callback?code=…&state=…",
		});
	};
	const withdraw = () => {
		if (!promptOpen) return;
		promptOpen = false;
		emit({ event: "prompt_done", id: String(promptId) });
	};

	const answer = async (value: string) => {
		promptOpen = false;
		if (!authorizationUrl) return;
		const target = callbackTarget(authorizationUrl, value);
		if ("error" in target) {
			emit({ event: "progress", message: target.error });
			ask();
			return;
		}
		emit({ event: "progress", message: "已收到地址，正在完成登录…" });
		try {
			const status = await deliver(target.url);
			if (status >= 400) {
				emit({ event: "progress", message: `回调没有接受这个地址（HTTP ${status}），请重新授权后再粘贴` });
				ask();
			}
		} catch (error) {
			emit({ event: "progress", message: `转交回调失败：${error instanceof Error ? error.message : String(error)}` });
			ask();
		}
	};

	let buffer = "";
	process.stdin.setEncoding("utf-8");
	process.stdin.on("data", (chunk: string) => {
		buffer += chunk;
		const { lines, rest } = takeLines(buffer);
		buffer = rest;
		for (const line of lines) {
			const command = decodeCommand(line);
			if (command?.command === "cancel") process.exit(1);
			if (command?.command === "prompt_response" && command.id === String(promptId)) void answer(command.value);
		}
	});
	// A host that went away must not leave the loopback callback server listening.
	process.stdin.on("end", () => process.exit(1));

	const errors: string[] = [];
	let lastLog: string | undefined;
	const code = await runMcpCommand(["login", ...args], {
		cwd,
		agentDir,
		openUrl: (url) => {
			authorizationUrl = url;
			emit({ event: "auth_url", url, instructions: "在浏览器里登录并同意授权，完成后回到这里。" });
			ask();
		},
		log: (line) => {
			// "Sign in to MCP server … in your browser:\n<url>": `auth_url` carries the URL, and Pi logs this
			// line before it calls `openUrl`.
			if (/\n\s*https?:\/\/\S+\s*$/.test(line)) return;
			lastLog = line;
			emit({ event: "progress", message: line });
		},
		error: (line) => errors.push(line),
	});
	withdraw();
	if (code === 0) {
		emit({ event: "done" });
	} else {
		emit({ event: "error", message: errors.join("\n") || lastLog || `登录失败（退出码 ${code}）` });
	}
	return code;
}

const [command, ...rest] = process.argv.slice(2);
if (command === "login") {
	let code = 1;
	try {
		code = await hostLogin(rest);
	} catch (error) {
		emit({ event: "error", message: error instanceof Error ? error.message : String(error) });
	}
	process.exit(code);
}
await main(["mcp", ...process.argv.slice(2)]);
// Connections opened by `list` may keep sockets alive; the command is done here.
process.exit(process.exitCode ?? 0);
