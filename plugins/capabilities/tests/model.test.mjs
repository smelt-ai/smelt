import { describe, expect, test } from "bun:test";
import {
  adoptDiscoveredModelRows,
  agentUserNames,
  agentUsesItem,
  applyLoginEvent,
  authStatusConfigured,
  authStatusNote,
  buildCustomProviderDraft,
  buildMemorySavePatch,
  buildMemoryTestParams,
  buildModelGroups,
  buildRerankPatch,
  buildSaveDefaultModelParams,
  createLoginView,
  customProviderNote,
  DEFAULT_PROVIDER_API,
  derivePluginRoots,
  discoveredModelLabel,
  formatMcpTransportDetail,
  formatModelLabel,
  formatUsedByLine,
  isDefaultProviderSignedOut,
  isMcpServerHttp,
  loginNoteText,
  loginText,
  MEMORY_KEYS,
  memoryKeyPlaceholder,
  memoryStatusLine,
  mergeProviders,
  normalizeAuthProvider,
  normalizePiConfig,
  normalizeProviderApi,
  parseMachineAgentSettings,
  parseMemorySettings,
  parseOptionalUint,
  PI_THINKING_LEVELS,
  pluginLocation,
  PROVIDER_API_PROTOCOLS,
  providerHasCredential,
  rerankModelLabel,
  summarizePlugins,
  thinkingLevelLabel,
  unwrapRustResult,
  validateAndBuildSaveCustomProviderParams,
  validateManualModelDraft,
  withAgentUse,
} from "../model.js";

// 简单的测试翻译函数：支持 %{var} 插值与常用文案
const EN_DICT = {
  "auth.status.oauth": "Signed in",
  "auth.status.api_key": "API key",
  "auth.status.api_key_from": "API key · %{source}",
  "auth.status.none": "Not set up",
  "auth.status.none_subscription": "Not signed in · subscription available",
  "config.thinking_level.off": "Off",
  "config.thinking_level.minimal": "Minimal",
  "config.thinking_level.low": "Low",
  "config.thinking_level.medium": "Medium",
  "config.thinking_level.high": "High",
  "config.thinking_level.xhigh": "Very high",
  "config.thinking_level.max": "Max",
  "mcp.local_command": "Local command · %{command} %{args}",
  "mcp.local_command_bare": "Local command · %{command}",
  "mcp.missing_transport": "Missing url or command",
  "memory.key_set": "Set",
  "memory.key_empty": "Optional; local services usually don't need one",
  "memory.loading": "Loading…",
  "memory.need_both": "Fill in both the service address and the model, or leave both empty to skip vector search",
  "memory.need_both_to_test": "Fill in the service address and the model before testing",
  "memory.rerank_auto": "Automatic (the cheapest model from the current conversation's provider)",
  "memory.status_configured": "Set up: %{model} @ %{url}",
  "memory.status_unconfigured": "Not set up: keyword search + model reranking only",
  "model_editor.key_required": "%{provider} has no credentials yet: enter an API key, or sign in below",
  "model_editor.model_required": "Enter a model ID",
  "model_editor.provider_required": "Enter a provider ID",
  "pi_login.answered_browser": "Answered “%{question}” for you: sign in with the browser",
  "pi_login.answered_github_dot_com": "Answered “%{question}” for you: left the GitHub Enterprise domain blank to sign in to github.com",
  "pi_login.callback_failed": "Couldn't pass the address to the callback: %{error}",
  "pi_login.callback_received": "Got the address, finishing sign-in…",
  "pi_login.callback_rejected": "The callback didn't accept that address (HTTP %{status}).",
  "pi_login.helper_exited": "The Pi credential helper exited without a result",
  "pi_login.helper_exited_with": "The Pi credential helper exited without a result: %{detail}",
  "pi_login.login_failed": "Sign-in failed (exit code %{code})",
  "pi_login.no_authorization_result": "No code",
  "pi_login.no_redirect": "No redirect",
  "pi_login.not_full_address": "Not full address",
  "pi_login.paste_callback": "Paste callback URL",
  "pi_login.sign_in_in_browser": "Sign in in browser",
  "pi_login.wrong_callback": "Wrong callback, expected %{expected}",
  "pi_model.base_url_invalid": "The API URL must be a full HTTP(S) URL",
  "pi_model.provider_id_invalid": "Provider ID \"%{id}\" is invalid",
  "pi_model.provider_id_required": "Enter a provider ID",
  "plugins.origin_builtin": "Built into %{origin}",
  "plugins.summary.extension": "%{usable} usable extensions installed.",
  "plugins.summary.skill": "%{usable} usable skills installed.",
  "plugins.summary_broken.extension": "%{usable} usable, %{broken} broken extensions.",
  "plugins.summary_broken.skill": "%{usable} usable, %{broken} broken skills.",
  "plugins.unused": "Not used by any agent",
  "plugins.used_by": "Used by %{names}",
  "plugins.user_separator": ", ",
  "provider.auto_synced": "synced from the endpoint",
  "provider.base_url_required": "Enter the API address",
  "provider.model_count": "%{count} models",
  "provider.no_key": "no API key",
};

function t(key, params = {}) {
  let out = EN_DICT[key] ?? key;
  for (const [k, v] of Object.entries(params)) {
    out = out.replaceAll(`%{${k}}`, String(v ?? ""));
  }
  return out;
}

describe("PiConfig & Rust Result parsing", () => {
  test("unwrapRustResult handles Ok, Err and invalid payloads", () => {
    expect(unwrapRustResult({ Ok: { a: 1 } })).toEqual({ ok: true, value: { a: 1 } });
    expect(unwrapRustResult({ Err: "broken json" })).toEqual({ ok: false, error: "broken json" });
    expect(unwrapRustResult(null).ok).toBe(false);
  });

  test("normalizePiConfig normalizes camelCase model settings and snake_case MCP/plugins", () => {
    const cfg = normalizePiConfig({
      model_settings: {
        Ok: {
          defaultModel: { provider: "anthropic", model: "claude-opus" },
          thinkingLevel: "high",
          customProviders: [
            {
              id: "gw",
              displayName: "Gateway",
              api: "openai-responses",
              baseUrl: "https://gw.example/v1",
              credentialConfigured: true,
              models: [{ id: "m1", name: "Model 1", contextWindow: 128000, maxTokens: 4096 }],
            },
          ],
          endpointOverrides: { anthropic: "https://proxy.local" },
        },
      },
      mcp_servers: {
        Ok: [
          {
            name: "figma",
            transport: { type: "http", url: "https://mcp.figma.com/mcp" },
            enabled: true,
            description: "Figma",
            signed_in: true,
          },
        ],
      },
      plugins: [
        {
          id: "skill:charts",
          kind: "skill",
          name: "charts",
          description: "Draw charts",
          path: "/Users/me/.agents/skills/charts",
          origin: "~/.agents/skills",
          broken: null,
          builtin: false,
          legacy_location: false,
          duplicates: [],
        },
      ],
      model_choice_groups: [],
      auto_provider_ids: ["gw"],
      paths: {
        settings: "/Users/me/.pi/agent/settings.json",
        models: "/Users/me/.pi/agent/models.json",
        auth: "/Users/me/.pi/agent/auth.json",
        mcp: "/Users/me/.pi/agent/mcp.json",
      },
    });

    expect(cfg.modelSettings.ok).toBe(true);
    expect(cfg.modelSettings.value.defaultModel).toEqual({
      provider: "anthropic",
      model: "claude-opus",
    });
    expect(cfg.modelSettings.value.thinkingLevel).toBe("high");
    expect(cfg.modelSettings.value.customProviders[0].models[0].contextWindow).toBe(128000);
    expect(cfg.mcpServers.ok).toBe(true);
    expect(cfg.mcpServers.value[0].transport).toEqual({
      type: "http",
      url: "https://mcp.figma.com/mcp",
    });
    expect(cfg.autoProviderIds).toEqual(["gw"]);
    expect(cfg.paths.mcp).toBe("/Users/me/.pi/agent/mcp.json");
  });
});

describe("Provider protocols, validation & discovery", () => {
  test("openai-responses is offered and unknown protocols fall back to default", () => {
    expect(PROVIDER_API_PROTOCOLS).toContain("openai-responses");
    expect(normalizeProviderApi("openai-responses")).toBe("openai-responses");
    expect(normalizeProviderApi("  anthropic-messages ")).toBe("anthropic-messages");
    expect(normalizeProviderApi("")).toBe(DEFAULT_PROVIDER_API);
    expect(normalizeProviderApi("openai-response")).toBe(DEFAULT_PROVIDER_API);
  });

  test("discoveredModelLabel falls back to id when name is empty or whitespace", () => {
    expect(discoveredModelLabel({ id: "gpt-5", name: null })).toBe("gpt-5");
    expect(discoveredModelLabel({ id: "gpt-5", name: "   " })).toBe("gpt-5");
    expect(discoveredModelLabel({ id: "gpt-5", name: "GPT 5" })).toBe("GPT 5");
  });

  test("parseOptionalUint parses clean integers and rejects garbage", () => {
    expect(parseOptionalUint("128000")).toBe(128000);
    expect(parseOptionalUint(" 4096 ")).toBe(4096);
    expect(parseOptionalUint("")).toBe(null);
    expect(parseOptionalUint("12abc")).toBe(null);
    expect(parseOptionalUint("-5")).toBe(null);
  });

  test("thinkingLevelLabel covers all 7 levels and passes through unknown", () => {
    expect(PI_THINKING_LEVELS.length).toBe(7);
    expect(thinkingLevelLabel("xhigh", t)).toBe("Very high");
    expect(thinkingLevelLabel("custom-level", t)).toBe("custom-level");
  });
});

describe("Provider merging & model choice grouping (Rust render/models.rs parity)", () => {
  const builtin = (id, supportsOauth, status, extra = {}) =>
    normalizeAuthProvider({
      id,
      name: extra.name ?? id,
      supportsOauth,
      supportsApiKey: extra.supportsApiKey ?? true,
      status,
      models: extra.models ?? [],
      subscription: extra.subscription ?? false,
      source: extra.source ?? null,
    });

  const custom = (id, extra = {}) => ({
    id,
    displayName: extra.displayName ?? "",
    api: extra.api ?? DEFAULT_PROVIDER_API,
    baseUrl: extra.baseUrl ?? "https://gw.example/v1",
    credentialConfigured: extra.credentialConfigured ?? false,
    models: extra.models ?? [],
  });

  test("merged providers put default then configured/custom first", () => {
    const builtins = [
      builtin("copilot", true, "none"),
      builtin("openai", false, "api_key"),
      builtin("anthropic", true, "oauth"),
    ];
    const customs = [custom("gateway")];
    const { entries, hidden } = mergeProviders(builtins, customs, "anthropic", false);
    expect(entries.map((e) => e.id)).toEqual(["anthropic", "gateway", "openai", "copilot"]);
    expect(hidden).toBe(0);
  });

  test("rarely used builtins fold until showAll", () => {
    const builtins = [builtin("groq", false, "none"), builtin("copilot", true, "none")];
    const collapsed = mergeProviders(builtins, [], "", false);
    expect(collapsed.entries.map((e) => e.id)).toEqual(["copilot"]);
    expect(collapsed.hidden).toBe(1);

    const expanded = mergeProviders(builtins, [], "", true);
    expect(expanded.entries.map((e) => e.id)).toEqual(["groq", "copilot"]);
    expect(expanded.hidden).toBe(0);
  });

  test("default builtin stays visible even when folded", () => {
    const builtins = [builtin("groq", false, "none")];
    const { entries, hidden } = mergeProviders(builtins, [], "groq", false);
    expect(entries.map((e) => e.id)).toEqual(["groq"]);
    expect(hidden).toBe(0);
  });

  test("custom provider shadows builtin with same id", () => {
    const builtins = [builtin("gateway", false, "api_key")];
    const customs = [custom("gateway")];
    const { entries } = mergeProviders(builtins, customs, "", false);
    expect(entries.length).toBe(1);
    expect(entries[0].kind).toBe("custom");
  });

  test("modelGroups list custom and signed-in builtins", () => {
    const customs = [
      custom("gw", { displayName: "Gateway", models: [{ id: "m1", name: "" }] }),
    ];
    const builtins = [
      builtin("anthropic", true, "oauth", {
        name: "Anthropic",
        models: [{ id: "claude-x" }],
      }),
      builtin("openai", false, "none", { models: [{ id: "gpt-x" }] }),
    ];
    const groups = buildModelGroups(customs, builtins, [], null);
    expect(groups).toEqual([
      { provider: "gw", name: "Gateway", models: [{ id: "m1", name: "m1" }] },
      { provider: "anthropic", name: "Anthropic", models: [{ id: "claude-x", name: "claude-x" }] },
    ]);
  });

  test("modelGroups fall back when builtin not loaded and keep current default", () => {
    const fallback = [
      {
        provider: "deepseek",
        provider_name: "deepseek",
        models: [{ provider: "deepseek", provider_name: "deepseek", id: "deepseek-chat", name: "deepseek-chat" }],
      },
    ];
    const groups = buildModelGroups([], null, fallback, {
      provider: "deepseek",
      model: "deepseek-new",
    });
    expect(groups[0].models).toEqual([
      { id: "deepseek-chat", name: "deepseek-chat" },
      { id: "deepseek-new", name: "deepseek-new" },
    ]);

    const extra = buildModelGroups([], [], fallback, { provider: "x", model: "y" });
    expect(formatModelLabel(extra, { provider: "x", model: "y" })).toBe("x / y");
  });

  test("isDefaultProviderSignedOut detects signed-out builtin but ignores custom", () => {
    const builtins = [builtin("anthropic", true, "none")];
    expect(
      isDefaultProviderSignedOut([], builtins, { provider: "anthropic", model: "claude" }),
    ).toBe(true);
    expect(
      isDefaultProviderSignedOut([custom("anthropic")], builtins, {
        provider: "anthropic",
        model: "claude",
      }),
    ).toBe(false);
  });

  test("authStatusNote & customProviderNote format accurately", () => {
    expect(authStatusConfigured("oauth")).toBe(true);
    expect(authStatusConfigured("none")).toBe(false);
    expect(authStatusNote(builtin("a", true, "oauth"), t)).toBe("Signed in");
    expect(authStatusNote(builtin("a", false, "api_key", { source: "ENV_KEY" }), t)).toBe(
      "API key · ENV_KEY",
    );
    expect(authStatusNote(builtin("a", true, "none", { subscription: true }), t)).toBe(
      "Not signed in · subscription available",
    );
    expect(
      customProviderNote(custom("gw", { models: [{ id: "m1" }, { id: "m2" }] }), true, t),
    ).toBe("2 models · synced from the endpoint · no API key");
  });
});

describe("Manual model & custom provider editors", () => {
  // 规则本身是运行时那份（smelt-model 里有测试）；这里只看表单把哪种问题写成哪句字。
  const rules = (problems = {}) => ({
    customProviderIdProblem: (id) => problems.id?.[id] ?? null,
    apiBaseUrlProblem: (url) => problems.url?.[url] ?? null,
  });

  test("validateManualModelDraft requires credentials for unconfigured known provider", () => {
    const builtins = [normalizeAuthProvider({ id: "openai", name: "OpenAI", status: "none" })];
    const fail = validateManualModelDraft(
      { provider: "openai", model: "gpt-5", baseUrl: "", apiKey: "" },
      [],
      builtins,
      t,
      rules(),
    );
    expect(fail.ok).toBe(false);
    expect(fail.error).toContain("openai has no credentials yet");

    const ok = validateManualModelDraft(
      { provider: "openai", model: "gpt-5", baseUrl: "", apiKey: "sk-123" },
      [],
      builtins,
      t,
      rules(),
    );
    expect(ok.ok).toBe(true);
    expect(ok.params).toEqual({
      config: { provider: "openai", model: "gpt-5", baseUrl: "", thinkingLevel: "" },
      api_key: "sk-123",
      reset_base_url: true,
    });
  });

  test("providerHasCredential checks custom then builtin", () => {
    expect(
      providerHasCredential(
        "gw",
        [{ id: "gw", credentialConfigured: true }],
        [{ id: "gw", status: "none" }],
      ),
    ).toBe(true);
    expect(providerHasCredential("unknown", [], [])).toBe(null);
  });

  test("buildCustomProviderDraft keeps auto provider models empty and adopts discovered models into blank row", () => {
    const autoDraft = buildCustomProviderDraft(
      { id: "gw", displayName: "GW", baseUrl: "https://x/v1", api: "openai-completions", models: [{ id: "m1" }] },
      true,
    );
    expect(autoDraft.models).toEqual([]);

    const manualDraft = buildCustomProviderDraft(null, false);
    expect(manualDraft.models.length).toBe(1);
    const adopted = adoptDiscoveredModelRows(manualDraft.models, {
      id: "gpt-5",
      name: "GPT-5",
      contextWindow: 128000,
      maxTokens: 16384,
    });
    expect(adopted.length).toBe(1);
    expect(adopted[0].id).toBe("gpt-5");
    expect(adopted[0].contextWindow).toBe("128000");

    // Duplicate adoption is ignored
    const dup = adoptDiscoveredModelRows(adopted, { id: "gpt-5", name: "GPT-5" });
    expect(dup.length).toBe(1);
  });

  test("validateAndBuildSaveCustomProviderParams builds camelCase provider payload and auto mode", () => {
    const draft = buildCustomProviderDraft(null, true);
    draft.id = "My-GW";
    draft.baseUrl = "https://gw.example/v1";
    draft.apiKey = "sk-key";

    const res = validateAndBuildSaveCustomProviderParams(draft, true, t, rules());
    expect(res.ok).toBe(true);
    expect(res.fromEndpoint).toBe(true);
    expect(res.params).toEqual({
      previous_id: null,
      provider: {
        id: "My-GW",
        displayName: "My-GW",
        api: "openai-completions",
        baseUrl: "https://gw.example/v1",
        credentialConfigured: true,
        models: [],
      },
      api_key: "sk-key",
      set_default: true,
    });
  });

  test("provider editors turn the runtime's problem kinds into their own text", () => {
    const draft = buildCustomProviderDraft(null, true);
    draft.id = "bad_id";
    draft.baseUrl = "ftp://gw";
    const badId = validateAndBuildSaveCustomProviderParams(draft, false, t, rules({ id: { bad_id: "id_invalid" } }));
    expect(badId).toEqual({ ok: false, error: t("pi_model.provider_id_invalid", { id: "bad_id" }) });

    draft.id = "gw";
    const badUrl = validateAndBuildSaveCustomProviderParams(
      draft,
      false,
      t,
      rules({ url: { "ftp://gw": "base_url_invalid" } }),
    );
    expect(badUrl).toEqual({ ok: false, error: t("pi_model.base_url_invalid") });

    const manual = validateManualModelDraft(
      { provider: "gw", model: "m", baseUrl: "ftp://gw", apiKey: "k" },
      [],
      null,
      t,
      rules({ url: { "ftp://gw": "base_url_invalid" } }),
    );
    expect(manual).toEqual({ ok: false, error: t("pi_model.base_url_invalid") });
  });

  test("buildSaveDefaultModelParams uses camelCase inside config and snake_case outside", () => {
    expect(buildSaveDefaultModelParams({ provider: "anthropic", model: "claude" })).toEqual({
      config: { provider: "anthropic", model: "claude", baseUrl: "", thinkingLevel: "" },
      api_key: null,
      reset_base_url: false,
    });
  });
});

describe("OAuth / API key login event reducer", () => {
  test("loginNoteText localizes known notes and falls back for unknown", () => {
    expect(loginNoteText({ kind: "callback_rejected", status: 400 }, t)).toContain("400");
    expect(loginNoteText({ kind: "helper_exited", detail: "boom" }, t)).toContain("boom");
    expect(loginNoteText({ kind: "helper_exited", detail: null }, t)).toBe(
      "The Pi credential helper exited without a result",
    );
    expect(loginNoteText({ kind: "future_kind" }, t)).toBe(null);
    expect(loginText("raw fallback", { kind: "future_kind" }, t)).toBe("raw fallback");
  });

  test("applyLoginEvent handles auth_url, device_code, prompt, prompt_done, error, done", () => {
    let view = createLoginView({ kind: "provider", id: "copilot", name: "Copilot" });

    let step = applyLoginEvent(
      view,
      { event: "auth_url", url: "https://auth.example", note: { kind: "sign_in_in_browser" } },
      t,
    );
    view = step.view;
    expect(step.openUrl).toBe("https://auth.example");
    expect(view.instructions).toBe("Sign in in browser");

    step = applyLoginEvent(
      view,
      { event: "device_code", userCode: "ABCD-1234", verificationUri: "https://github.com/login/device" },
      t,
    );
    view = step.view;
    expect(step.openUrl).toBe("https://github.com/login/device");
    expect(view.deviceCode).toEqual({
      userCode: "ABCD-1234",
      verificationUri: "https://github.com/login/device",
    });

    step = applyLoginEvent(
      view,
      { event: "prompt", id: "p1", promptType: "secret", message: "API key", placeholder: "sk-..." },
      t,
    );
    view = step.view;
    expect(view.prompt?.kind).toBe("secret");

    step = applyLoginEvent(view, { event: "prompt_done", id: "p1" }, t);
    view = step.view;
    expect(view.prompt).toBe(null);

    step = applyLoginEvent(view, { event: "error", message: "denied" }, t);
    expect(step.view.error).toBe("denied");
    expect(step.view.authUrl).toBe(null);
    expect(step.view.deviceCode).toBe(null);

    const done = applyLoginEvent(view, { event: "done" }, t);
    expect(done.view).toBe(null);
    expect(done.doneTarget).toEqual({ kind: "provider", id: "copilot", name: "Copilot" });
  });
});

describe("MCP transport formatting", () => {
  test("formatMcpTransportDetail and isMcpServerHttp", () => {
    expect(isMcpServerHttp({ transport: { type: "http", url: "https://a" } })).toBe(true);
    expect(isMcpServerHttp({ transport: { type: "stdio", command: "bun", args: [] } })).toBe(false);
    expect(formatMcpTransportDetail({ type: "http", url: "https://mcp.figma.com/mcp" }, t)).toBe(
      "https://mcp.figma.com/mcp",
    );
    expect(formatMcpTransportDetail({ type: "stdio", command: "smelt-agent", args: ["mcp-server", "playwright"] }, t)).toBe(
      "Local command · smelt-agent mcp-server playwright",
    );
    expect(formatMcpTransportDetail({ type: "stdio", command: "smelt-agent", args: [] }, t)).toBe(
      "Local command · smelt-agent",
    );
    expect(formatMcpTransportDetail({ type: "unknown" }, t)).toBe("Missing url or command");
  });
});

describe("Plugins (Skills & Extensions) roots, location & summary", () => {
  test("derivePluginRoots derives target machine paths on POSIX and Windows", () => {
    expect(
      derivePluginRoots("skill", { settings: "/home/dev/.pi/agent/settings.json" }),
    ).toEqual(["/home/dev/.agents/skills", "/home/dev/.pi/agent/skills"]);
    expect(
      derivePluginRoots("extension", { settings: "/home/dev/.pi/agent/settings.json" }),
    ).toEqual(["/home/dev/.pi/agent/extensions"]);
    expect(
      derivePluginRoots("skill", { settings: "C:\\Users\\alice\\.pi\\agent\\settings.json" }),
    ).toEqual(["C:\\Users\\alice\\.agents\\skills", "C:\\Users\\alice\\.pi\\agent\\skills"]);
    expect(derivePluginRoots("skill", null)).toEqual(["~/.agents/skills", "~/.pi/agent/skills"]);
  });

  test("pluginLocation and summarizePlugins", () => {
    expect(pluginLocation({ builtin: true, origin: "smelt-agent", path: "/x/a" }, t)).toBe(
      "Built into smelt-agent",
    );
    expect(
      pluginLocation({ builtin: false, origin: "~/.agents/skills", path: "/Users/me/.agents/skills/charts" }, t),
    ).toBe("~/.agents/skills/charts");

    const sum = summarizePlugins(
      [
        { id: "skill:a", kind: "skill", broken: null },
        { id: "skill:b", kind: "skill", broken: "dead symlink" },
        { id: "extension:c", kind: "extension", broken: null },
      ],
      "skill",
      t,
    );
    expect(sum.usable).toBe(1);
    expect(sum.broken).toBe(1);
    expect(sum.summary).toBe("1 usable, 1 broken skills.");
  });
});

describe("Memory settings (Rust memory.rs unit tests parity)", () => {
  test("status_line needs both url and model", () => {
    const unconfigured = "Not set up: keyword search + model reranking only";
    expect(memoryStatusLine({ baseUrl: "", model: "", configured: false }, false, t)).toBe(
      unconfigured,
    );
    expect(
      memoryStatusLine({ baseUrl: "http://localhost:11434/v1", model: "", configured: false }, false, t),
    ).toBe(unconfigured);
    expect(
      memoryStatusLine(
        { baseUrl: "http://localhost:11434/v1", model: "bge-m3", configured: true },
        false,
        t,
      ),
    ).toBe("Set up: bge-m3 @ http://localhost:11434/v1");
    expect(memoryStatusLine(null, true, t)).toBe("Loading…");
  });

  test("empty key field keeps the stored key and url comes first in patch", () => {
    const p1 = buildMemorySavePatch(" http://localhost:11434/v1 ", "bge-m3", "  ", t);
    expect(p1).toEqual({
      ok: true,
      settings: {
        [MEMORY_KEYS.EMBEDDING_BASE_URL]: "http://localhost:11434/v1",
        [MEMORY_KEYS.EMBEDDING_MODEL]: "bge-m3",
      },
    });
    const p2 = buildMemorySavePatch("https://api.example.com/v1", "m", "sk-new", t);
    expect(p2.ok).toBe(true);
    expect(Object.keys(p2.settings)[0]).toBe(MEMORY_KEYS.EMBEDDING_BASE_URL);
    expect(p2.settings[MEMORY_KEYS.EMBEDDING_API_KEY]).toBe("sk-new");
  });

  test("testing uses form values and leaves stored key to runtime", () => {
    const p1 = buildMemoryTestParams(" http://new/v1 ", "bge-m3", "", t);
    expect(p1).toEqual({
      ok: true,
      params: { base_url: "http://new/v1", model: "bge-m3", api_key: null },
    });
    const p2 = buildMemoryTestParams("http://new/v1", "m", "sk-typed", t);
    expect(p2.ok && p2.params.api_key).toBe("sk-typed");
    expect(buildMemoryTestParams("http://new/v1", "", "", t).ok).toBe(false);
  });

  test("saved settings only reveal whether a key is set", () => {
    const parsed = parseMemorySettings({
      settings: {
        [MEMORY_KEYS.EMBEDDING_BASE_URL]: "http://x/v1",
        [MEMORY_KEYS.EMBEDDING_MODEL]: "bge-m3",
        [MEMORY_KEYS.EMBEDDING_API_KEY]: "set",
        [MEMORY_KEYS.RERANK_MODEL_PROVIDER]: "",
        [MEMORY_KEYS.RERANK_MODEL_ID]: "",
      },
    });
    expect(parsed.saved.apiKeySet).toBe(true);
    expect(parsed.saved.configured).toBe(true);
    expect(parsed.rerankModel).toBe(null);
    expect(memoryKeyPlaceholder(parsed.saved, t)).toBe("Set");
  });

  test("rerank model label says auto when unset and formats provider / id", () => {
    expect(rerankModelLabel(null, t).startsWith("Automatic")).toBe(true);
    expect(rerankModelLabel({ provider: "github-copilot", id: "gpt-6-luna" }, t)).toBe(
      "github-copilot / gpt-6-luna",
    );
    expect(buildRerankPatch(null)).toEqual({
      settings: {
        [MEMORY_KEYS.RERANK_MODEL_PROVIDER]: "",
        [MEMORY_KEYS.RERANK_MODEL_ID]: "",
      },
    });
  });

  test("half-filled form is rejected, both empty clears", () => {
    expect(buildMemorySavePatch("http://localhost:11434/v1", "", "", t).ok).toBe(false);
    expect(buildMemorySavePatch("", "bge-m3", "", t).ok).toBe(false);
    const cleared = buildMemorySavePatch("", "", "", t);
    expect(cleared.ok).toBe(true);
    expect(Object.keys(cleared.settings).length).toBe(2);
  });
});

describe("Agent usage & smelt.agents peer contract (Rust agents.rs unit tests parity)", () => {
  const makeAgent = (id, plugins, mcp) => ({
    id,
    name: id,
    plugins: [...plugins],
    mcp_servers: [...mcp],
  });

  test("withAgentUse toggles only that agent and list", () => {
    const settings = {
      agents: [
        makeAgent("a", ["skill:x"], []),
        makeAgent("b", ["skill:x"], ["github"]),
      ],
    };
    const skill = { kind: "plugin", id: "skill:y" };
    const afterAdd = withAgentUse(settings, skill, "a", true);
    expect(afterAdd.agents[0].plugins).toEqual(["skill:x", "skill:y"]);
    expect(afterAdd.agents[1]).toEqual(settings.agents[1]);
    // Original settings must not be mutated
    expect(settings.agents[0].plugins).toEqual(["skill:x"]);

    const mcp = { kind: "mcp", id: "github" };
    const afterRemove = withAgentUse(settings, mcp, "b", false);
    expect(afterRemove.agents[1].mcp_servers).toEqual([]);
    expect(afterRemove.agents[1].plugins).toEqual(["skill:x"]);
  });

  test("withAgentUse returns null when already so or agent missing", () => {
    const settings = { agents: [makeAgent("a", ["skill:x"], [])] };
    const skill = { kind: "plugin", id: "skill:x" };
    expect(withAgentUse(settings, skill, "a", true)).toBe(null);
    expect(withAgentUse(settings, skill, "gone", false)).toBe(null);
  });

  test("agentUsesItem, agentUserNames and formatUsedByLine", () => {
    const settings = {
      agents: [
        makeAgent("alpha", ["skill:x"], ["figma"]),
        makeAgent("beta", ["skill:x"], []),
      ],
    };
    expect(agentUsesItem(settings.agents[0], { kind: "mcp", id: "figma" })).toBe(true);
    expect(agentUserNames(settings, { kind: "plugin", id: "skill:x" })).toEqual(["alpha", "beta"]);
    expect(formatUsedByLine(settings, { kind: "plugin", id: "skill:x" }, t)).toBe(
      "Used by alpha, beta",
    );
    expect(formatUsedByLine(settings, { kind: "mcp", id: "none" }, t)).toBe(
      "Not used by any agent",
    );
    expect(formatUsedByLine(null, { kind: "mcp", id: "figma" }, t)).toBe(null);
  });

  test("parseMachineAgentSettings parses ready, unavailable and absent shapes", () => {
    const ready = parseMachineAgentSettings({
      machine: "box",
      state: "ready",
      settings: { agents: [] },
    });
    expect(ready).toEqual({ machine: "box", state: "ready", settings: { agents: [] } });

    const unavailable = parseMachineAgentSettings({
      machine: null,
      state: "unavailable",
      reason: "runtime too old",
    });
    expect(unavailable).toEqual({ machine: null, state: "unavailable" });

    const absent = parseMachineAgentSettings({ machine: "old", state: "absent" });
    expect(absent).toEqual({ machine: "old", state: "absent" });
  });
});
