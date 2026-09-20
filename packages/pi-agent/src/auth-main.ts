#!/usr/bin/env bun

/**
 * Pi credential helper: list providers, run a login flow, drop a credential.
 *
 * Pi only exposes `/login` inside its interactive TUI and its RPC protocol has
 * no auth methods, so a GUI host cannot reuse either. This entry drives the
 * same pi-ai code Pi drives (`Models.login`), and writes the same
 * `~/.pi/agent/auth.json`, with the interaction turned into the NDJSON
 * protocol in `auth-protocol.ts`.
 *
 * Usage:
 *   bun src/auth-main.ts list
 *   bun src/auth-main.ts login <providerId> [--type oauth|api_key] [--ask-all]
 *   bun src/auth-main.ts logout <providerId>
 */

import { homedir } from "node:os";
import { join } from "node:path";
import { builtinModels } from "@earendil-works/pi-ai/providers/all";
import { registerBunOAuthFlows } from "@earendil-works/pi-ai/bun-oauth";
import {
	type AuthModelSummary,
	type AuthProviderSummary,
	type HostEvent,
	decodeCommand,
	encodeEvent,
	takeLines,
} from "./auth-protocol.ts";
import { AuthJsonCredentialStore } from "./auth-store.ts";
import { LoginPresets, type PresetPrompt } from "./login-presets.ts";

/** Same resolution as Pi's `getAgentDir()`, so both read one auth.json. */
function agentDir(): string {
	const override = process.env.PI_CODING_AGENT_DIR?.trim();
	if (override) {
		return override.startsWith("~") ? join(homedir(), override.slice(1)) : override;
	}
	return join(homedir(), ".pi", "agent");
}

function emit(event: HostEvent): void {
	process.stdout.write(encodeEvent(event));
}

function message(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function createModels() {
	// Static registration: the lazy loaders import flow modules through a
	// computed specifier, which resolves from `node_modules` but not from a
	// compiled bundle. Registering up front makes both layouts behave the same.
	registerBunOAuthFlows();
	return builtinModels({ credentials: new AuthJsonCredentialStore(join(agentDir(), "auth.json")) });
}

async function listProviders(): Promise<void> {
	const models = createModels();
	const providers = await Promise.all(
		models.getProviders().map(async (provider): Promise<AuthProviderSummary> => {
			const oauth = provider.auth.oauth;
			const apiKey = provider.auth.apiKey;
			// A provider without stored or ambient credentials simply reports
			// nothing; a failing check must not take down the whole list.
			const check = await models.checkAuth(provider.id).catch(() => undefined);
			return {
				id: provider.id,
				name: provider.name,
				oauthName: oauth?.loginLabel ?? oauth?.name,
				apiKeyName: apiKey?.name,
				subscription: oauth?.isSubscription === true,
				supportsOauth: oauth !== undefined,
				supportsApiKey: apiKey !== undefined,
				status: check?.type ?? "none",
				source: check?.source,
			};
		}),
	);
	providers.sort((left, right) => left.id.localeCompare(right.id));
	emit({ event: "providers", providers });
}

/**
 * Models the provider can actually serve with its current credential.
 *
 * `getAvailable` applies the provider's own filtering — GitHub Copilot only
 * serves the models the account enabled — so an unfiltered catalog would offer
 * defaults that fail on first use. It needs auth, so an unauthenticated
 * provider falls back to the static catalog.
 */
async function listModels(providerId: string): Promise<void> {
	const models = createModels();
	const available = await models.getAvailable(providerId).catch(() => []);
	const catalog = available.length > 0 ? available : models.getModels(providerId);
	const summaries: AuthModelSummary[] = catalog.map((model) => ({
		id: model.id,
		name: model.name,
		contextWindow: model.contextWindow,
		maxTokens: model.maxTokens,
	}));
	emit({ event: "models", models: summaries });
}

/**
 * Host-driven interaction: prompts become events the host answers by id.
 *
 * Prompts carry their own abort signal because flows race a pasted code
 * against a loopback callback. When the callback wins, the pending prompt is
 * withdrawn — the host has to be told, or its dialog keeps asking for a code
 * that is no longer needed.
 */
function createInteraction(
	signal: AbortSignal,
	pending: Map<string, PendingPrompt>,
	presets: LoginPresets,
) {
	let nextId = 0;
	return {
		signal,
		notify(event: Record<string, unknown>): void {
			switch (event.type) {
				case "auth_url":
					emit({
						event: "auth_url",
						url: String(event.url),
						instructions: event.instructions as string | undefined,
					});
					break;
				case "device_code":
					emit({
						event: "device_code",
						userCode: String(event.userCode),
						verificationUri: String(event.verificationUri),
						intervalSeconds: event.intervalSeconds as number | undefined,
						expiresInSeconds: event.expiresInSeconds as number | undefined,
					});
					break;
				case "info":
					emit({
						event: "info",
						message: String(event.message),
						links: event.links as { url: string; label?: string }[] | undefined,
					});
					break;
				case "progress":
					emit({ event: "progress", message: String(event.message) });
					break;
			}
		},
		prompt(prompt: Record<string, unknown>): Promise<string> {
			// Steps with one right answer are answered here, but never silently:
			// a login that skips a question the user did not see must still say
			// what it decided.
			const preset = presets.answer(prompt as unknown as PresetPrompt);
			if (preset) {
				emit({
					event: "progress",
					message: `已代答「${String(prompt.message)}」：${preset.reason}`,
				});
				return Promise.resolve(preset.value);
			}
			const id = String(++nextId);
			return new Promise<string>((resolve, reject) => {
				const promptSignal = prompt.signal as AbortSignal | undefined;
				const settle = () => {
					pending.delete(id);
					promptSignal?.removeEventListener("abort", onAbort);
					signal.removeEventListener("abort", onAbort);
					emit({ event: "prompt_done", id });
				};
				const onAbort = () => {
					settle();
					reject(new Error("prompt aborted"));
				};
				pending.set(id, {
					resolve: (value) => {
						settle();
						resolve(value);
					},
				});
				promptSignal?.addEventListener("abort", onAbort, { once: true });
				signal.addEventListener("abort", onAbort, { once: true });
				if (promptSignal?.aborted || signal.aborted) {
					onAbort();
					return;
				}
				emit({
					event: "prompt",
					id,
					promptType: prompt.type as "text" | "secret" | "select" | "manual_code",
					message: String(prompt.message),
					placeholder: prompt.placeholder as string | undefined,
					options: prompt.options as { id: string; label: string; description?: string }[] | undefined,
				});
			});
		},
	};
}

interface PendingPrompt {
	resolve(value: string): void;
}

/** Feed stdin commands into the running flow until the process exits. */
function readCommands(controller: AbortController, pending: Map<string, PendingPrompt>): void {
	let buffer = "";
	process.stdin.setEncoding("utf-8");
	process.stdin.on("data", (chunk: string) => {
		buffer += chunk;
		const { lines, rest } = takeLines(buffer);
		buffer = rest;
		for (const line of lines) {
			const command = decodeCommand(line);
			if (!command) continue;
			if (command.command === "cancel") {
				controller.abort();
				continue;
			}
			pending.get(command.id)?.resolve(command.value);
		}
	});
	// A host that dies mid-login must not leave a loopback callback server and
	// a polling device-code loop behind.
	process.stdin.on("end", () => controller.abort());
}

async function login(
	providerId: string,
	type: "oauth" | "api_key",
	askAll: boolean,
): Promise<void> {
	const models = createModels();
	const controller = new AbortController();
	const pending = new Map<string, PendingPrompt>();
	readCommands(controller, pending);
	const presets = new LoginPresets(providerId, !askAll);
	await models.login(
		providerId,
		type,
		createInteraction(controller.signal, pending, presets) as never,
	);
	emit({ event: "done" });
}

async function logout(providerId: string): Promise<void> {
	await createModels().logout(providerId);
	emit({ event: "done" });
}

async function run(argv: string[]): Promise<void> {
	const [command, providerId] = argv;
	if (command === "list") return listProviders();
	if (!providerId) throw new Error(`用法：${command ?? "<command>"} <providerId>`);
	if (command === "logout") return logout(providerId);
	if (command === "models") return listModels(providerId);
	if (command === "login") {
		const typeIndex = argv.indexOf("--type");
		const type = typeIndex >= 0 ? argv[typeIndex + 1] : "oauth";
		if (type !== "oauth" && type !== "api_key") throw new Error(`未知登录方式：${type}`);
		return login(providerId, type, argv.includes("--ask-all"));
	}
	throw new Error(`未知命令：${command ?? ""}`);
}

try {
	await run(process.argv.slice(2));
	process.exit(0);
} catch (error) {
	emit({ event: "error", message: message(error) });
	process.exit(1);
}
