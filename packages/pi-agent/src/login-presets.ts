/**
 * Login prompts the host can answer on the user's behalf.
 *
 * A login flow may open with a question that has one right answer for almost
 * everyone — "GitHub Enterprise domain (blank for github.com)", "browser or
 * device code?". Pi's TUI asks them because a terminal has no better option; a
 * GUI that can open a browser should not stop the user at an empty text box
 * whose answer is "leave it empty".
 *
 * Two layers, so adding a provider never means touching the flow driver:
 *   - `GENERIC_RULES` are provider-independent heuristics (prefer the browser
 *     branch when the host has a browser).
 *   - `PROVIDER_PRESETS` are ordered per-provider answers, consumed strictly in
 *     order. A prompt that does not match the next preset disables the rest for
 *     that login: the flow changed shape, and guessing past that point would
 *     answer a question we have not read.
 *
 * Anything the user genuinely has to supply — a pasted code, a secret — is
 * never preset.
 */

/** The subset of an `AuthPrompt` a preset decides on. */
export interface PresetPrompt {
	type: "text" | "secret" | "select" | "manual_code";
	message: string;
	options?: readonly { id: string; label: string; description?: string }[];
}

export interface PresetAnswer {
	value: string;
	/** Shown to the user so an auto-answered step is visible, not silent. */
	reason: string;
}

interface ProviderPreset {
	type: "text" | "select";
	/**
	 * Substring the prompt must contain. A preset answer is only safe on the
	 * question it was written for: if the wording changes, matching by position
	 * alone would hand "leave it blank" to some other question. Failing to
	 * match just asks the user, which is the harmless direction.
	 */
	messageIncludes: string;
	/** For `select`, the option id to pick; for `text`, the literal answer. */
	value: string;
	reason: string;
}

const PROVIDER_PRESETS: Record<string, readonly ProviderPreset[]> = {
	"github-copilot": [
		{
			type: "text",
			messageIncludes: "GitHub Enterprise",
			value: "",
			reason: "GitHub 企业版域名留空，按 github.com 登录",
		},
	],
};

/** Option ids that mean "do it in a browser" across providers. */
const BROWSER_OPTION_IDS = new Set(["browser"]);

const GENERIC_RULES: readonly ((prompt: PresetPrompt) => PresetAnswer | undefined)[] = [
	// A GUI host opens the URL itself, so the browser branch is strictly better
	// than typing a device code — and it is what the flows call the default.
	(prompt) => {
		if (prompt.type !== "select") return undefined;
		const option = prompt.options?.find((candidate) => BROWSER_OPTION_IDS.has(candidate.id));
		return option && { value: option.id, reason: "用浏览器完成登录" };
	},
];

/**
 * Consumes provider presets in order; generic rules stay available regardless.
 */
export class LoginPresets {
	private index = 0;
	private derailed = false;

	constructor(
		private readonly providerId: string,
		private readonly enabled: boolean,
	) {}

	answer(prompt: PresetPrompt): PresetAnswer | undefined {
		if (!this.enabled) return undefined;
		const preset = this.nextProviderPreset(prompt);
		if (preset) return preset;
		for (const rule of GENERIC_RULES) {
			const answer = rule(prompt);
			if (answer) return answer;
		}
		return undefined;
	}

	private nextProviderPreset(prompt: PresetPrompt): PresetAnswer | undefined {
		if (this.derailed) return undefined;
		const preset = PROVIDER_PRESETS[this.providerId]?.[this.index];
		if (!preset) return undefined;
		if (!matches(preset, prompt)) {
			this.derailed = true;
			return undefined;
		}
		this.index += 1;
		return { value: preset.value, reason: preset.reason };
	}
}

function matches(preset: ProviderPreset, prompt: PresetPrompt): boolean {
	if (preset.type !== prompt.type) return false;
	if (!prompt.message.includes(preset.messageIncludes)) return false;
	if (preset.type !== "select") return true;
	return prompt.options?.some((option) => option.id === preset.value) === true;
}
