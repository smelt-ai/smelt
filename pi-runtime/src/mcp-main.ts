#!/usr/bin/env bun

/**
 * `pi mcp <args>` through Smelt's managed runtime.
 *
 * The GUI signs in to remote MCP servers (Figma, …) with `mcp-main.ts login <server>`. Pi's own
 * `main` dispatches `mcp` before any session starts and runs the whole OAuth flow (registration,
 * PKCE, loopback callback, `mcp-auth.json`), so this entry only forwards the arguments.
 *
 * Usage:
 *   bun src/mcp-main.ts login <server> [--timeout <seconds>]
 *   bun src/mcp-main.ts list
 */

import { main } from "@earendil-works/pi-coding-agent";

process.title = "pi-mcp";
process.emitWarning = (() => {}) as typeof process.emitWarning;
await main(["mcp", ...process.argv.slice(2)]);
// Connections opened by `login` / `list` may keep sockets alive; the command is done here.
process.exit(process.exitCode ?? 0);
