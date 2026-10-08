export const SMELT_AGENT_PLUGIN_ARGS_ENV = "SMELT_AGENT_PLUGIN_ARGS";

let consumedPluginArgs: string[] = [];

/** The selected resource argv is reused only by Smelt-managed Pi subagents. */
export function getConsumedPluginArgs(): string[] {
	return [...consumedPluginArgs];
}

/**
 * Consume the host-provided Pi plugin argv before Pi starts.
 *
 * The host sends a JSON string array rather than a pre-joined command line so that skill paths
 * containing spaces survive intact. Removing the environment variable prevents arbitrary tool
 * subprocesses from inheriting it; managed subagents can explicitly reuse the in-memory argv.
 *
 * An empty selection still yields `--no-skills --no-extensions`: the host decides that a product
 * agent loads only what it checked, and dropping the flags would silently restore full discovery.
 */
export function consumePluginArgs(
	environment: Record<string, string | undefined>,
	argv: string[],
): boolean {
	consumedPluginArgs = [];
	const raw = environment[SMELT_AGENT_PLUGIN_ARGS_ENV]?.trim();
	delete environment[SMELT_AGENT_PLUGIN_ARGS_ENV];
	if (!raw) return false;
	let parsed: unknown;
	try {
		parsed = JSON.parse(raw);
	} catch {
		return false;
	}
	if (!Array.isArray(parsed)) return false;
	const args = parsed.filter(
		(entry): entry is string => typeof entry === "string" && entry.length > 0,
	);
	if (args.length === 0) return false;
	consumedPluginArgs = args;
	argv.push(...args);
	return true;
}
