import { describe, expect, test } from "bun:test";
import { LoginPresets, type PresetPrompt } from "../src/login-presets.ts";

const enterprise: PresetPrompt = {
	type: "text",
	message: "GitHub Enterprise URL/domain (blank for github.com)",
};
const method: PresetPrompt = {
	type: "select",
	message: "Select OpenAI Codex login method:",
	options: [
		{ id: "browser", label: "Browser login (default)" },
		{ id: "device_code", label: "Device code login (headless)" },
	],
};

describe("login presets", () => {
	test("answer a step whose only sane answer is a default", () => {
		const presets = new LoginPresets("github-copilot", true);
		const answer = presets.answer(enterprise);
		expect(answer?.value).toBe("");
		expect(answer?.reason).toContain("github.com");
	});

	test("a GUI host takes the browser branch without asking", () => {
		// The rule is provider-independent: any flow offering a `browser`
		// option gets it, because the host can open the URL itself.
		expect(new LoginPresets("openai-codex", true).answer(method)?.value).toBe("browser");
		expect(new LoginPresets("whatever-else", true).answer(method)?.value).toBe("browser");
	});

	test("a select without a browser branch is left to the user", () => {
		const presets = new LoginPresets("openai-codex", true);
		expect(
			presets.answer({
				type: "select",
				message: "Pick a workspace",
				options: [{ id: "team-a", label: "Team A" }],
			}),
		).toBeUndefined();
	});

	test("secrets and pasted codes are never answered for the user", () => {
		const presets = new LoginPresets("github-copilot", true);
		expect(presets.answer({ type: "manual_code", message: "Paste the code" })).toBeUndefined();
		expect(presets.answer({ type: "secret", message: "API key" })).toBeUndefined();
	});

	test("presets are consumed in order and stop once the flow diverges", () => {
		const presets = new LoginPresets("github-copilot", true);
		// A flow that starts with a question we did not expect must not have
		// the enterprise-domain answer handed to it.
		expect(presets.answer({ type: "text", message: "Region" })).toBeUndefined();
		expect(presets.answer(enterprise)).toBeUndefined();
	});

	test("--ask-all hands every prompt back to the user", () => {
		const presets = new LoginPresets("github-copilot", false);
		expect(presets.answer(enterprise)).toBeUndefined();
		expect(presets.answer(method)).toBeUndefined();
	});
});
