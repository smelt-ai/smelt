export const SMELT_AGENT_INSTRUCTIONS_ENV = "SMELT_AGENT_INSTRUCTIONS";

/**
 * Consume the host-provided AgentDefinition instructions before Pi starts.
 * Keeping the value in one argv entry preserves whitespace/newlines, while removing the
 * environment variable prevents subsequently spawned tool processes from inheriting it.
 */
export function consumeAgentInstructions(
	environment: Record<string, string | undefined>,
	argv: string[],
): boolean {
	const instructions = environment[SMELT_AGENT_INSTRUCTIONS_ENV]?.trim();
	delete environment[SMELT_AGENT_INSTRUCTIONS_ENV];
	if (!instructions) return false;
	argv.push("--append-system-prompt", instructions);
	return true;
}
