// 能力库插件（smelt.capabilities）的纯领域逻辑与规则：
// 零 NSH / 宿主依赖，可直接由 `bun test` 运行验证。

// ---- 常量 ----

/** pi-ai 的 KnownApi，顺序即界面里的排列顺序：常用的在前。 */
export const PROVIDER_API_PROTOCOLS = [
  "openai-completions",
  "openai-responses",
  "anthropic-messages",
  "google-generative-ai",
  "azure-openai-responses",
  "openai-codex-responses",
  "google-vertex",
  "bedrock-converse-stream",
  "mistral-conversations",
  "pi-messages",
];

export const DEFAULT_PROVIDER_API = "openai-completions";

/** Pi 认的推理强度，从低到高。 */
export const PI_THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/** 设置页搜索关键词。 */
export const MODELS_SETTINGS_KEYWORDS = [
  "模型",
  "默认模型",
  "服务商",
  "provider",
  "api key",
  "登录",
  "订阅",
  "思考",
  "thinking",
  "pi",
];

export const MEMORY_SETTINGS_KEYWORDS = [
  "记忆",
  "检索",
  "向量",
  "嵌入",
  "embedding",
  "重排",
  "rerank",
  "memory",
];

/** 记忆检索在运行时 settings 表中的键名（与 smelt_model::memory_settings 一致）。 */
export const MEMORY_KEYS = {
  EMBEDDING_BASE_URL: "memory.embedding_base_url",
  EMBEDDING_MODEL: "memory.embedding_model",
  EMBEDDING_API_KEY: "memory.embedding_api_key",
  RERANK_MODEL_PROVIDER: "memory.rerank_model_provider",
  RERANK_MODEL_ID: "memory.rerank_model_id",
};

// ---- 通用辅助 ----

export function machineKey(machine) {
  return typeof machine === "string" && machine.length > 0 ? machine : "";
}

export function normalizeMachine(machine) {
  return typeof machine === "string" && machine.length > 0 ? machine : null;
}

export function nonEmptyOr(value, fallback) {
  return typeof value === "string" && value.trim().length > 0 ? value : fallback;
}

export function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

/**
 * 解包 Rust `Result<T, String>` 的 serde JSON 表示（`{ Ok: ... }` / `{ Err: ... }`）。
 */
export function unwrapRustResult(raw) {
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    if (Object.prototype.hasOwnProperty.call(raw, "Ok")) {
      return { ok: true, value: raw.Ok };
    }
    if (Object.prototype.hasOwnProperty.call(raw, "Err")) {
      return { ok: false, error: String(raw.Err ?? "") };
    }
  }
  return { ok: false, error: "Invalid result payload" };
}

/** 规整 `pi.config.get` / 修改方法返回的 `PiConfig` 快照。 */
export function normalizePiConfig(raw) {
  const modelRes = unwrapRustResult(raw?.model_settings);
  const mcpRes = unwrapRustResult(raw?.mcp_servers);
  const paths = raw?.paths && typeof raw.paths === "object" ? raw.paths : {};

  return {
    modelSettings: modelRes.ok
      ? { ok: true, value: normalizeModelSettings(modelRes.value) }
      : { ok: false, error: modelRes.error },
    mcpServers: mcpRes.ok
      ? { ok: true, value: Array.isArray(mcpRes.value) ? mcpRes.value.map(normalizeMcpServer) : [] }
      : { ok: false, error: mcpRes.error },
    plugins: Array.isArray(raw?.plugins) ? raw.plugins.map(normalizePlugin) : [],
    modelChoiceGroups: Array.isArray(raw?.model_choice_groups)
      ? raw.model_choice_groups.map(normalizeModelChoiceGroup)
      : [],
    autoProviderIds: Array.isArray(raw?.auto_provider_ids)
      ? raw.auto_provider_ids.filter((id) => typeof id === "string")
      : [],
    paths: {
      settings: typeof paths.settings === "string" ? paths.settings : "",
      models: typeof paths.models === "string" ? paths.models : "",
      auth: typeof paths.auth === "string" ? paths.auth : "",
      mcp: typeof paths.mcp === "string" ? paths.mcp : "",
    },
  };
}

function normalizeModelSettings(raw) {
  const obj = raw && typeof raw === "object" ? raw : {};
  const dm = obj.defaultModel;
  const defaultModel =
    dm && typeof dm === "object" && typeof dm.provider === "string" && typeof dm.model === "string"
      ? { provider: dm.provider, model: dm.model }
      : null;
  const thinkingLevel =
    typeof obj.thinkingLevel === "string" && obj.thinkingLevel.length > 0
      ? obj.thinkingLevel
      : null;
  const customProviders = Array.isArray(obj.customProviders)
    ? obj.customProviders.map((p) => ({
        id: typeof p?.id === "string" ? p.id : "",
        displayName: typeof p?.displayName === "string" ? p.displayName : "",
        api: typeof p?.api === "string" ? p.api : "",
        baseUrl: typeof p?.baseUrl === "string" ? p.baseUrl : "",
        credentialConfigured: Boolean(p?.credentialConfigured),
        models: Array.isArray(p?.models)
          ? p.models.map((m) => ({
              id: typeof m?.id === "string" ? m.id : "",
              name: typeof m?.name === "string" ? m.name : "",
              contextWindow: typeof m?.contextWindow === "number" ? m.contextWindow : null,
              maxTokens: typeof m?.maxTokens === "number" ? m.maxTokens : null,
            }))
          : [],
      }))
    : [];
  const endpointOverrides =
    obj.endpointOverrides && typeof obj.endpointOverrides === "object" && !Array.isArray(obj.endpointOverrides)
      ? { ...obj.endpointOverrides }
      : {};
  return { defaultModel, thinkingLevel, customProviders, endpointOverrides };
}

function normalizeMcpServer(raw) {
  const tr = raw?.transport;
  let transport = { type: "unknown" };
  if (tr && typeof tr === "object") {
    if (tr.type === "http" && typeof tr.url === "string") {
      transport = { type: "http", url: tr.url };
    } else if (tr.type === "stdio" && typeof tr.command === "string") {
      transport = {
        type: "stdio",
        command: tr.command,
        args: Array.isArray(tr.args) ? tr.args.map(String) : [],
      };
    }
  }
  return {
    name: typeof raw?.name === "string" ? raw.name : "",
    transport,
    enabled: Boolean(raw?.enabled),
    description: typeof raw?.description === "string" ? raw.description : null,
    signed_in: Boolean(raw?.signed_in),
  };
}

function normalizePlugin(raw) {
  return {
    id: typeof raw?.id === "string" ? raw.id : "",
    kind: raw?.kind === "extension" ? "extension" : "skill",
    name: typeof raw?.name === "string" ? raw.name : "",
    description: typeof raw?.description === "string" ? raw.description : "",
    path: typeof raw?.path === "string" ? raw.path : "",
    origin: typeof raw?.origin === "string" ? raw.origin : "",
    broken: typeof raw?.broken === "string" ? raw.broken : null,
    builtin: Boolean(raw?.builtin),
    legacy_location: Boolean(raw?.legacy_location),
    duplicates: Array.isArray(raw?.duplicates) ? raw.duplicates.map(String) : [],
  };
}

function normalizeModelChoiceGroup(raw) {
  return {
    provider: typeof raw?.provider === "string" ? raw.provider : "",
    provider_name: typeof raw?.provider_name === "string" ? raw.provider_name : "",
    models: Array.isArray(raw?.models)
      ? raw.models.map((m) => ({
          provider: typeof m?.provider === "string" ? m.provider : "",
          provider_name: typeof m?.provider_name === "string" ? m.provider_name : "",
          id: typeof m?.id === "string" ? m.id : "",
          name: typeof m?.name === "string" ? m.name : "",
        }))
      : [],
  };
}

export function normalizeAuthProvider(raw) {
  const status =
    raw?.status === "oauth" || raw?.status === "api_key" || raw?.status === "none"
      ? raw.status
      : "none";
  return {
    id: typeof raw?.id === "string" ? raw.id : "",
    name: typeof raw?.name === "string" ? raw.name : "",
    oauthName: typeof raw?.oauthName === "string" ? raw.oauthName : null,
    apiKeyName: typeof raw?.apiKeyName === "string" ? raw.apiKeyName : null,
    subscription: Boolean(raw?.subscription),
    supportsOauth: Boolean(raw?.supportsOauth),
    supportsApiKey: Boolean(raw?.supportsApiKey),
    status,
    source: typeof raw?.source === "string" ? raw.source : null,
    models: Array.isArray(raw?.models) ? raw.models.map(normalizeDiscoveredModel) : [],
  };
}

export function normalizeDiscoveredModel(raw) {
  return {
    id: typeof raw?.id === "string" ? raw.id : "",
    name: typeof raw?.name === "string" ? raw.name : null,
    contextWindow: typeof raw?.contextWindow === "number" ? raw.contextWindow : null,
    maxTokens: typeof raw?.maxTokens === "number" ? raw.maxTokens : null,
  };
}

// ---- Provider 协议、校验与模型目录 ----

export function normalizeProviderApi(api) {
  const trimmed = typeof api === "string" ? api.trim() : "";
  return PROVIDER_API_PROTOCOLS.includes(trimmed) ? trimmed : DEFAULT_PROVIDER_API;
}

export function discoveredModelLabel(model) {
  const name = typeof model?.name === "string" ? model.name.trim() : "";
  return name.length > 0 ? name : String(model?.id ?? "");
}

export function modelChoiceShortLabel(choice) {
  const name = typeof choice?.name === "string" ? choice.name.trim() : "";
  return name.length > 0 ? name : String(choice?.id ?? "");
}

export function thinkingLevelLabel(level, t) {
  if (PI_THINKING_LEVELS.includes(level)) {
    return t(`config.thinking_level.${level}`);
  }
  return String(level ?? "");
}

/** Provider API 地址：必须是完整的 HTTP(S) URL。 */
export function validateApiBaseUrl(baseUrl, t) {
  const raw = typeof baseUrl === "string" ? baseUrl.trim() : "";
  try {
    const parsed = new URL(raw);
    if ((parsed.protocol === "http:" || parsed.protocol === "https:") && parsed.host.length > 0) {
      return { ok: true };
    }
  } catch {
    // fall through
  }
  return { ok: false, error: t("pi_model.base_url_invalid") };
}

/** 自定义 Provider ID：以 ASCII 字母开头，只含字母、数字与非相邻/非首尾的短连字符。 */
export function validateCustomProviderId(id, t) {
  const value = typeof id === "string" ? id : "";
  if (value.length === 0) {
    return { ok: false, error: t("pi_model.provider_id_required") };
  }
  const firstCode = value.charCodeAt(0);
  const startsAlpha =
    (firstCode >= 65 && firstCode <= 90) || (firstCode >= 97 && firstCode <= 122);
  const parts = value.split("-");
  const valid =
    startsAlpha && parts.every((part) => part.length > 0 && /^[A-Za-z0-9]+$/.test(part));
  if (valid) {
    return { ok: true };
  }
  return { ok: false, error: t("pi_model.provider_id_invalid", { id: value }) };
}

export function parseOptionalUint(raw) {
  const trimmed = typeof raw === "string" ? raw.trim() : "";
  if (!/^\d+$/.test(trimmed)) return null;
  const num = Number(trimmed);
  return Number.isSafeInteger(num) ? num : null;
}

export function authStatusConfigured(status) {
  return status === "oauth" || status === "api_key";
}

/**
 * 默认模型下拉分组（与 Rust `render/models.rs::model_groups` 语义完全一致）。
 * 自定义服务商来自 models.json；内置服务商来自凭据助手（仅已登录）；未就绪时退回快照目录；
 * 当前默认模型不在目录中时也会补进去。
 */
export function buildModelGroups(custom, builtin, fallback, current) {
  const customList = Array.isArray(custom) ? custom : [];
  const customIds = new Set(customList.map((p) => p.id));

  const groups = customList
    .filter((p) => Array.isArray(p.models) && p.models.length > 0)
    .map((p) => ({
      provider: p.id,
      name: nonEmptyOr(p.displayName, p.id),
      models: p.models.map((m) => ({
        id: m.id,
        name: nonEmptyOr(m.name, m.id),
      })),
    }));

  if (Array.isArray(builtin)) {
    for (const p of builtin) {
      if (!authStatusConfigured(p.status) || customIds.has(p.id)) continue;
      if (!Array.isArray(p.models) || p.models.length === 0) continue;
      groups.push({
        provider: p.id,
        name: p.name,
        models: p.models.map((m) => ({
          id: m.id,
          name: discoveredModelLabel(m),
        })),
      });
    }
  } else if (Array.isArray(fallback)) {
    for (const g of fallback) {
      if (customIds.has(g.provider)) continue;
      groups.push({
        provider: g.provider,
        name: g.provider_name,
        models: (g.models ?? []).map((c) => ({
          id: c.id,
          name: modelChoiceShortLabel(c),
        })),
      });
    }
  }

  if (current && current.provider && current.model) {
    const listed = groups.some(
      (g) => g.provider === current.provider && g.models.some((m) => m.id === current.model),
    );
    if (!listed) {
      const entry = { id: current.model, name: current.model };
      const existing = groups.find((g) => g.provider === current.provider);
      if (existing) {
        existing.models.push(entry);
      } else {
        groups.push({
          provider: current.provider,
          name: current.provider,
          models: [entry],
        });
      }
    }
  }

  return groups;
}

export function formatModelLabel(groups, modelRef) {
  if (!modelRef) return "";
  const group = (groups ?? []).find((g) => g.provider === modelRef.provider);
  const provider = group ? group.name : modelRef.provider;
  const found = group?.models.find((m) => m.id === modelRef.model);
  const name = found ? found.name : modelRef.model;
  return `${provider} / ${name}`;
}

export function isDefaultProviderSignedOut(custom, builtin, modelRef) {
  if (!modelRef || !Array.isArray(builtin)) return false;
  if ((custom ?? []).some((p) => p.id === modelRef.provider)) return false;
  return builtin.some((p) => p.id === modelRef.provider && !authStatusConfigured(p.status));
}

/**
 * 合并内置与自定义服务商，返回 `{ entries, hidden }`（与 Rust `merge_providers` 一致）。
 * 排序：默认模型所用服务商 → 已配置 / 自定义 → 其余，同档保持原序。
 */
export function mergeProviders(builtin, custom, defaultProvider, showAll) {
  const customList = Array.isArray(custom) ? custom : [];
  const builtinList = Array.isArray(builtin) ? builtin : [];
  const customIds = new Set(customList.map((p) => p.id));

  let hidden = 0;
  const entries = customList.map((provider) => ({
    kind: "custom",
    id: provider.id,
    configured: true,
    provider,
  }));

  for (const provider of builtinList) {
    if (customIds.has(provider.id)) continue;
    const configured = authStatusConfigured(provider.status);
    const visible =
      Boolean(showAll) || provider.supportsOauth || configured || provider.id === defaultProvider;
    if (visible) {
      entries.push({
        kind: "builtin",
        id: provider.id,
        configured,
        provider,
      });
    } else {
      hidden += 1;
    }
  }

  const rank = (entry) => {
    if (entry.id === defaultProvider) return 0;
    if (entry.configured) return 1;
    return 2;
  };
  entries.sort((a, b) => rank(a) - rank(b));

  return { entries, hidden };
}

export function authStatusNote(provider, t) {
  if (provider.status === "oauth") return t("auth.status.oauth");
  if (provider.status === "api_key") {
    return provider.source
      ? t("auth.status.api_key_from", { source: provider.source })
      : t("auth.status.api_key");
  }
  if (provider.subscription) return t("auth.status.none_subscription");
  return t("auth.status.none");
}

export function customProviderNote(provider, isAuto, t) {
  let note = t("provider.model_count", { count: (provider.models ?? []).length });
  if (isAuto) {
    note = `${note} · ${t("provider.auto_synced")}`;
  }
  if (!provider.credentialConfigured) {
    note = `${note} · ${t("provider.no_key")}`;
  }
  return note;
}

export function providerHasCredential(providerId, customProviders, builtinProviders) {
  const custom = (customProviders ?? []).find((p) => p.id === providerId);
  if (custom) return Boolean(custom.credentialConfigured);
  if (Array.isArray(builtinProviders)) {
    const builtin = builtinProviders.find((p) => p.id === providerId);
    if (builtin) return authStatusConfigured(builtin.status);
  }
  return null;
}

export function buildSaveDefaultModelParams({ provider, model, baseUrl = "", apiKey = null, resetBaseUrl = false }) {
  return {
    config: {
      provider,
      model,
      baseUrl,
      thinkingLevel: "",
    },
    api_key: apiKey && apiKey.length > 0 ? apiKey : null,
    reset_base_url: Boolean(resetBaseUrl),
  };
}

export function validateManualModelDraft(draft, customProviders, builtinProviders, t) {
  const provider = String(draft?.provider ?? "").trim();
  if (!provider) {
    return { ok: false, error: t("model_editor.provider_required") };
  }
  const model = String(draft?.model ?? "").trim();
  if (!model) {
    return { ok: false, error: t("model_editor.model_required") };
  }
  const baseUrl = String(draft?.baseUrl ?? "").trim();
  if (baseUrl.length > 0) {
    const urlCheck = validateApiBaseUrl(baseUrl, t);
    if (!urlCheck.ok) return urlCheck;
  }
  const apiKey = String(draft?.apiKey ?? "").trim();
  if (apiKey.length === 0 && providerHasCredential(provider, customProviders, builtinProviders) === false) {
    return { ok: false, error: t("model_editor.key_required", { provider }) };
  }
  return {
    ok: true,
    params: buildSaveDefaultModelParams({
      provider,
      model,
      baseUrl,
      apiKey: apiKey.length > 0 ? apiKey : null,
      resetBaseUrl: true,
    }),
  };
}

let nextRowKey = 1;
export function emptyCustomModelRow(model = null) {
  return {
    key: `row-${nextRowKey++}`,
    id: model?.id ?? "",
    name: model?.name ?? "",
    contextWindow: model?.contextWindow != null ? String(model.contextWindow) : "",
    maxTokens: model?.maxTokens != null ? String(model.maxTokens) : "",
  };
}

export function buildCustomProviderDraft(existing, isAuto) {
  const models = isAuto
    ? []
    : (existing?.models ?? []).map((m) => emptyCustomModelRow(m));
  if (models.length === 0 && !isAuto) {
    models.push(emptyCustomModelRow(null));
  }
  return {
    previousId: existing ? existing.id : null,
    id: existing?.id ?? "",
    displayName: existing?.displayName ?? "",
    apiKey: "",
    baseUrl: existing?.baseUrl ?? "",
    api: existing?.api ? existing.api : DEFAULT_PROVIDER_API,
    models,
    discovery: null,
    credentialConfigured: Boolean(existing?.credentialConfigured),
  };
}

/** 把端点发现的模型填进草稿列表：已存在则跳过，优先复用第一行空白行。 */
export function adoptDiscoveredModelRows(rows, model) {
  const list = Array.isArray(rows) ? rows : [];
  if (list.some((row) => String(row.id ?? "").trim() === model.id)) {
    return list;
  }
  const row = emptyCustomModelRow({
    id: model.id,
    name: model.name ?? "",
    contextWindow: model.contextWindow ?? null,
    maxTokens: model.maxTokens ?? null,
  });
  const blankIdx = list.findIndex((r) => String(r.id ?? "").trim().length === 0);
  if (blankIdx >= 0) {
    const next = list.slice();
    next[blankIdx] = row;
    return next;
  }
  return [...list, row];
}

export function validateAndBuildSaveCustomProviderParams(draft, setDefault, t) {
  const id = String(draft?.id ?? "").trim();
  const idCheck = validateCustomProviderId(id, t);
  if (!idCheck.ok) {
    return { ok: false, error: idCheck.error };
  }
  const baseUrl = String(draft?.baseUrl ?? "").trim();
  if (baseUrl.length === 0) {
    return { ok: false, error: t("provider.base_url_required") };
  }
  const urlCheck = validateApiBaseUrl(baseUrl, t);
  if (!urlCheck.ok) {
    return { ok: false, error: urlCheck.error };
  }

  const models = [];
  for (const row of draft?.models ?? []) {
    const modelId = String(row?.id ?? "").trim();
    if (!modelId) continue;
    const name = String(row?.name ?? "").trim();
    models.push({
      id: modelId,
      name: name.length > 0 ? name : modelId,
      contextWindow: parseOptionalUint(row?.contextWindow ?? ""),
      maxTokens: parseOptionalUint(row?.maxTokens ?? ""),
    });
  }

  const displayNameRaw = String(draft?.displayName ?? "").trim();
  const displayName = displayNameRaw.length > 0 ? displayNameRaw : id;
  const apiKey = String(draft?.apiKey ?? "").trim();
  const fromEndpoint = models.length === 0;

  return {
    ok: true,
    fromEndpoint,
    params: {
      previous_id: draft?.previousId ?? null,
      provider: {
        id,
        displayName,
        api: draft?.api || DEFAULT_PROVIDER_API,
        baseUrl,
        credentialConfigured: Boolean(draft?.credentialConfigured) || apiKey.length > 0,
        models,
      },
      api_key: apiKey.length > 0 ? apiKey : null,
      set_default: Boolean(setDefault),
    },
  };
}

// ---- 登录事件流（Provider / MCP 共用） ----

export function loginNoteText(note, t) {
  if (!note || typeof note !== "object") return null;
  switch (note.kind) {
    case "paste_callback":
      return t("pi_login.paste_callback");
    case "sign_in_in_browser":
      return t("pi_login.sign_in_in_browser");
    case "callback_received":
      return t("pi_login.callback_received");
    case "callback_rejected":
      return t("pi_login.callback_rejected", { status: note.status ?? "" });
    case "callback_failed":
      return t("pi_login.callback_failed", { error: note.error ?? "" });
    case "no_redirect":
      return t("pi_login.no_redirect");
    case "not_full_address":
      return t("pi_login.not_full_address");
    case "wrong_callback":
      return t("pi_login.wrong_callback", { expected: note.expected ?? "" });
    case "no_authorization_result":
      return t("pi_login.no_authorization_result");
    case "login_failed":
      return t("pi_login.login_failed", { code: note.code ?? "" });
    case "answered_github_dot_com":
      return t("pi_login.answered_github_dot_com", { question: note.question ?? "" });
    case "answered_browser":
      return t("pi_login.answered_browser", { question: note.question ?? "" });
    case "helper_exited":
      return note.detail
        ? t("pi_login.helper_exited_with", { detail: note.detail })
        : t("pi_login.helper_exited");
    default:
      return null;
  }
}

export function loginText(message, note, t) {
  return loginNoteText(note, t) ?? String(message ?? "");
}

export function createLoginView(target) {
  return {
    target,
    loginId: null,
    messages: [],
    authUrl: null,
    instructions: null,
    deviceCode: null,
    prompt: null,
    inputValue: "",
    error: null,
  };
}

/**
 * 纯函数归约一条 `PiLoginEvent`。
 * 返回 `{ view, openUrl, doneTarget }`：`view === null` 表示登录成功面板收起。
 */
export function applyLoginEvent(view, event, t) {
  if (!view || !event || typeof event !== "object") {
    return { view, openUrl: null, doneTarget: null };
  }
  switch (event.event) {
    case "auth_url": {
      const url = typeof event.url === "string" ? event.url : null;
      const localized = loginNoteText(event.note, t);
      const instructions =
        localized ?? (typeof event.instructions === "string" ? event.instructions : null);
      return {
        view: { ...view, authUrl: url, instructions },
        openUrl: url,
        doneTarget: null,
      };
    }
    case "device_code": {
      const userCode = String(event.userCode ?? event.user_code ?? "");
      const verificationUri = String(event.verificationUri ?? event.verification_uri ?? "");
      return {
        view: {
          ...view,
          deviceCode: { userCode, verificationUri },
        },
        openUrl: verificationUri || null,
        doneTarget: null,
      };
    }
    case "info": {
      const nextMessages = [...view.messages];
      if (typeof event.message === "string" && event.message.length > 0) {
        nextMessages.push(event.message);
      }
      if (Array.isArray(event.links)) {
        for (const link of event.links) {
          if (typeof link?.url === "string" && link.url.length > 0) {
            nextMessages.push(link.url);
          }
        }
      }
      return { view: { ...view, messages: nextMessages }, openUrl: null, doneTarget: null };
    }
    case "progress": {
      const text = loginText(event.message ?? "", event.note, t);
      return {
        view: { ...view, messages: [...view.messages, text] },
        openUrl: null,
        doneTarget: null,
      };
    }
    case "prompt": {
      const kind = event.promptType ?? event.prompt_type ?? "text";
      const prompt = {
        id: String(event.id ?? ""),
        kind,
        message: loginText(event.message ?? "", event.note, t),
        placeholder: typeof event.placeholder === "string" ? event.placeholder : null,
        options: Array.isArray(event.options)
          ? event.options.map((opt) => ({
              id: String(opt?.id ?? ""),
              label: String(opt?.label ?? ""),
              description: typeof opt?.description === "string" ? opt.description : null,
            }))
          : [],
      };
      return {
        view: { ...view, prompt, inputValue: "" },
        openUrl: null,
        doneTarget: null,
      };
    }
    case "prompt_done": {
      if (view.prompt && view.prompt.id === event.id) {
        return { view: { ...view, prompt: null, inputValue: "" }, openUrl: null, doneTarget: null };
      }
      return { view, openUrl: null, doneTarget: null };
    }
    case "done": {
      return { view: null, openUrl: null, doneTarget: view.target };
    }
    case "error": {
      const message = loginText(event.message ?? "", event.note, t);
      return {
        view: {
          ...view,
          prompt: null,
          authUrl: null,
          deviceCode: null,
          inputValue: "",
          error: message,
        },
        openUrl: null,
        doneTarget: null,
      };
    }
    default:
      return { view, openUrl: null, doneTarget: null };
  }
}

// ---- MCP ----

export function isMcpServerHttp(server) {
  return server?.transport?.type === "http";
}

export function formatMcpTransportDetail(transport, t) {
  if (!transport || typeof transport !== "object") {
    return t("mcp.missing_transport");
  }
  if (transport.type === "http") {
    return String(transport.url ?? "");
  }
  if (transport.type === "stdio") {
    const args = Array.isArray(transport.args) ? transport.args.join(" ").trim() : "";
    if (args.length === 0) {
      return t("mcp.local_command_bare", { command: transport.command ?? "" });
    }
    return t("mcp.local_command", { command: transport.command ?? "", args });
  }
  return t("mcp.missing_transport");
}

// ---- 技能与扩展（Pi 插件） ----

export function isPluginUsable(plugin) {
  return plugin?.broken == null;
}

export function pathBasename(rawPath) {
  if (typeof rawPath !== "string" || rawPath.length === 0) return null;
  const trimmed = rawPath.replace(/[\\/]+$/, "");
  if (trimmed.length === 0) return null;
  const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
}

export function pluginLocation(plugin, t) {
  if (plugin?.builtin) {
    return t("plugins.origin_builtin", { origin: plugin.origin ?? "" });
  }
  const base = pathBasename(plugin?.path ?? "");
  return base ? `${plugin.origin}/${base}` : String(plugin?.origin ?? "");
}

/**
 * 推导目标机器上的扫描目录：优先根据运行时报上来的 `paths.settings` 推导（避免远程机器
 * 显示本机路径），未拿到快照时退回通用 `~` 写法。
 */
export function derivePluginRoots(kind, paths) {
  const settingsPath = typeof paths?.settings === "string" ? paths.settings.trim() : "";
  if (settingsPath.length > 0) {
    const sep = settingsPath.includes("\\") && !settingsPath.includes("/") ? "\\" : "/";
    const lastSep = Math.max(settingsPath.lastIndexOf("/"), settingsPath.lastIndexOf("\\"));
    if (lastSep > 0) {
      const piAgentDir = settingsPath.slice(0, lastSep);
      if (kind === "extension") {
        return [`${piAgentDir}${sep}extensions`];
      }
      const suffix = `${sep}.pi${sep}agent`;
      const roots = [];
      if (piAgentDir.endsWith(suffix)) {
        const home = piAgentDir.slice(0, piAgentDir.length - suffix.length);
        roots.push(`${home}${sep}.agents${sep}skills`);
      } else {
        roots.push("~/.agents/skills");
      }
      roots.push(`${piAgentDir}${sep}skills`);
      return roots;
    }
  }
  return kind === "extension"
    ? ["~/.pi/agent/extensions"]
    : ["~/.agents/skills", "~/.pi/agent/skills"];
}

export function summarizePlugins(plugins, kind, t) {
  const items = (plugins ?? []).filter((p) => p.kind === kind);
  const usable = items.filter(isPluginUsable).length;
  const broken = items.length - usable;
  const summary =
    broken === 0
      ? t(`plugins.summary.${kind}`, { usable })
      : t(`plugins.summary_broken.${kind}`, { usable, broken });
  return { items, usable, broken, summary };
}

// ---- 记忆检索设置 ----

export function parseMemorySettings(result) {
  const settings = result?.settings && typeof result.settings === "object" ? result.settings : {};
  const str = (key) => (typeof settings[key] === "string" ? settings[key] : "");
  const baseUrl = str(MEMORY_KEYS.EMBEDDING_BASE_URL);
  const model = str(MEMORY_KEYS.EMBEDDING_MODEL);
  const apiKeySet = str(MEMORY_KEYS.EMBEDDING_API_KEY).length > 0;
  const provider = str(MEMORY_KEYS.RERANK_MODEL_PROVIDER);
  const id = str(MEMORY_KEYS.RERANK_MODEL_ID);
  return {
    saved: {
      baseUrl,
      model,
      apiKeySet,
      configured: baseUrl.length > 0 && model.length > 0,
    },
    rerankModel: provider.length > 0 && id.length > 0 ? { provider, id } : null,
  };
}

export function memoryKeyPlaceholder(saved, t) {
  return saved?.apiKeySet ? t("memory.key_set") : t("memory.key_empty");
}

export function memoryStatusLine(saved, loading, t) {
  if (loading) return t("memory.loading");
  if (saved?.configured) {
    return t("memory.status_configured", { model: saved.model, url: saved.baseUrl });
  }
  return t("memory.status_unconfigured");
}

/**
 * 把表单翻成要写入的键值补丁。地址排第一个：它是唯一会被拒的项，拒了就什么都没写。
 * 地址与模型要么都填要么都空；密钥留空表示保持不变，不放进补丁。
 */
export function buildMemorySavePatch(baseUrl, model, apiKey, t) {
  const url = String(baseUrl ?? "").trim();
  const mdl = String(model ?? "").trim();
  const key = String(apiKey ?? "").trim();
  if ((url.length === 0) !== (mdl.length === 0)) {
    return { ok: false, error: t("memory.need_both") };
  }
  const settings = {
    [MEMORY_KEYS.EMBEDDING_BASE_URL]: url,
    [MEMORY_KEYS.EMBEDDING_MODEL]: mdl,
  };
  if (key.length > 0) {
    settings[MEMORY_KEYS.EMBEDDING_API_KEY] = key;
  }
  return { ok: true, settings };
}

export function buildMemoryTestParams(baseUrl, model, apiKey, t) {
  const url = String(baseUrl ?? "").trim();
  const mdl = String(model ?? "").trim();
  if (url.length === 0 || mdl.length === 0) {
    return { ok: false, error: t("memory.need_both_to_test") };
  }
  const key = String(apiKey ?? "").trim();
  return {
    ok: true,
    params: {
      base_url: url,
      model: mdl,
      api_key: key.length > 0 ? key : null,
    },
  };
}

export function buildRerankPatch(model) {
  return {
    settings: {
      [MEMORY_KEYS.RERANK_MODEL_PROVIDER]: model?.provider ?? "",
      [MEMORY_KEYS.RERANK_MODEL_ID]: model?.id ?? "",
    },
  };
}

export function rerankModelLabel(model, t) {
  if (model && model.provider && model.id) {
    return `${model.provider} / ${model.id}`;
  }
  return t("memory.rerank_auto");
}

// ---- 智能体关联（smelt.agents 对等协作） ----

export function parseMachineAgentSettings(payload) {
  if (!payload || typeof payload !== "object") return null;
  const machine = normalizeMachine(payload.machine);
  const state = payload.state;
  if (state === "ready" && payload.settings && typeof payload.settings === "object") {
    return { machine, state: "ready", settings: payload.settings };
  }
  if (state === "unavailable" || state === "absent") {
    return { machine, state };
  }
  return null;
}

export function agentUsesItem(agent, usable) {
  if (!agent || !usable) return false;
  const list = usable.kind === "mcp" ? agent.mcp_servers : agent.plugins;
  return Array.isArray(list) && list.includes(usable.id);
}

export function agentUserNames(settings, usable) {
  if (!settings || !Array.isArray(settings.agents)) return [];
  return settings.agents
    .filter((agent) => agentUsesItem(agent, usable))
    .map((agent) => String(agent.name ?? ""));
}

export function formatUsedByLine(settings, usable, t) {
  if (!settings) return null;
  const names = agentUserNames(settings, usable);
  if (names.length === 0) {
    return t("plugins.unused");
  }
  return t("plugins.used_by", { names: names.join(t("plugins.user_separator")) });
}

/**
 * 把 `settings` 里指定智能体对 `usable` 的勾选改成 `on`；智能体不在或本来就是这样时返回 `null`。
 */
export function withAgentUse(settings, usable, agentId, on) {
  if (!settings || !Array.isArray(settings.agents) || !usable) return null;
  const idx = settings.agents.findIndex((a) => a.id === agentId);
  if (idx < 0) return null;
  const current = settings.agents[idx];
  const currentlyOn = agentUsesItem(current, usable);
  if (currentlyOn === Boolean(on)) return null;

  const after = cloneJson(settings);
  const target = after.agents[idx];
  const field = usable.kind === "mcp" ? "mcp_servers" : "plugins";
  const list = Array.isArray(target[field]) ? target[field] : [];
  if (on) {
    target[field] = [...list, usable.id];
  } else {
    target[field] = list.filter((id) => id !== usable.id);
  }
  return after;
}
