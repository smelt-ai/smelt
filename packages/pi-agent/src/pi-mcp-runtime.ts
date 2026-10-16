import { pathToFileURL } from "node:url";
import type { McpTransportFactory } from "@earendil-works/pi-coding-agent";

/**
 * Pi's own transport factory, the one its MCP extension uses when none is given.
 *
 * It is not exported from the package entry, but it is what decides how a `mcp.json` entry becomes
 * a connection: `~` expansion, `${VAR}` and `!cmd` values in `env` and `headers`, OAuth for HTTP
 * servers. Smelt routes stdio servers through the shared gateway and hands everything else back to
 * this factory, and the gateway starts each server with it, so both keep Pi's exact semantics
 * instead of a copy that drifts. Pi is pinned to an exact version; the import fails loudly (and the
 * tests with it) if a Pi upgrade moves the module.
 */
function piModuleUrl(relative: string): string {
	const resolved = import.meta.resolve("@earendil-works/pi-coding-agent");
	const entry = resolved.startsWith("file:") ? resolved : pathToFileURL(resolved).href;
	return new URL(relative, entry).href;
}

const runtime = (await import(piModuleUrl("./extensions/mcp/runtime.js"))) as {
	createDefaultTransport?: unknown;
};

if (typeof runtime.createDefaultTransport !== "function") {
	throw new Error("Pi's MCP runtime no longer exports createDefaultTransport");
}

export const createPiDefaultTransport = runtime.createDefaultTransport as McpTransportFactory;
