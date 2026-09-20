/**
 * Wire protocol between the Smelt host and the Pi login helper.
 *
 * Pi's own login lives in its interactive TUI, so a GUI host has to drive
 * pi-ai's OAuth flows itself. The flows are callback-shaped (`notify` pushes
 * an authorization URL or device code, `prompt` asks for a pasted code), which
 * is why this is a bidirectional NDJSON stream rather than a single request.
 *
 * Events go out on stdout, commands come in on stdin, one JSON object per
 * line. The host ignores stdout lines that are not protocol objects: pi-ai and
 * its dependencies may write diagnostics there and a stray log line must not
 * abort a login.
 */

export interface AuthProviderSummary {
	id: string;
	/** Provider display name, e.g. "GitHub Copilot". */
	name: string;
	/** Login option label for the OAuth method, when the provider has one. */
	oauthName?: string;
	/** Api-key method label, when the provider accepts a stored key. */
	apiKeyName?: string;
	/** Whether OAuth access here is backed by a paid subscription. */
	subscription: boolean;
	supportsOauth: boolean;
	supportsApiKey: boolean;
	/** Credential type currently in effect, or `none`. */
	status: "oauth" | "api_key" | "none";
	/** Where the credential came from, e.g. "OAuth", "ANTHROPIC_API_KEY". */
	source?: string;
}

export interface AuthModelSummary {
	id: string;
	name: string;
	contextWindow?: number;
	maxTokens?: number;
}

export interface PromptOption {
	id: string;
	label: string;
	description?: string;
}

export type HostEvent =
	| { event: "providers"; providers: AuthProviderSummary[] }
	| { event: "models"; models: AuthModelSummary[] }
	| { event: "auth_url"; url: string; instructions?: string }
	| {
			event: "device_code";
			userCode: string;
			verificationUri: string;
			intervalSeconds?: number;
			expiresInSeconds?: number;
	  }
	| { event: "info"; message: string; links?: { url: string; label?: string }[] }
	| { event: "progress"; message: string }
	| {
			event: "prompt";
			id: string;
			promptType: "text" | "secret" | "select" | "manual_code";
			message: string;
			placeholder?: string;
			options?: PromptOption[];
	  }
	/** The flow stopped waiting for this prompt (a racing callback won). */
	| { event: "prompt_done"; id: string }
	| { event: "done" }
	| { event: "error"; message: string };

export type HostCommand =
	| { command: "prompt_response"; id: string; value: string }
	| { command: "cancel" };

/** Serialize one event as a protocol line, newline included. */
export function encodeEvent(event: HostEvent): string {
	return `${JSON.stringify(event)}\n`;
}

/**
 * Parse one stdin line into a command.
 *
 * Returns undefined for blank lines, malformed JSON, and unknown commands: a
 * host that speaks a newer protocol must not crash an in-flight login.
 */
export function decodeCommand(line: string): HostCommand | undefined {
	const trimmed = line.trim();
	if (!trimmed) return undefined;
	let parsed: unknown;
	try {
		parsed = JSON.parse(trimmed);
	} catch {
		return undefined;
	}
	if (typeof parsed !== "object" || parsed === null) return undefined;
	const value = parsed as Record<string, unknown>;
	if (value.command === "cancel") return { command: "cancel" };
	if (
		value.command === "prompt_response" &&
		typeof value.id === "string" &&
		typeof value.value === "string"
	) {
		return { command: "prompt_response", id: value.id, value: value.value };
	}
	return undefined;
}

/**
 * Split a growing stdin buffer into whole lines.
 *
 * Returns the lines and the trailing partial line, which the caller keeps for
 * the next chunk. A pasted OAuth redirect URL can exceed one read.
 */
export function takeLines(buffer: string): { lines: string[]; rest: string } {
	const parts = buffer.split("\n");
	const rest = parts.pop() ?? "";
	return { lines: parts, rest };
}
