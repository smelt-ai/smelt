// 能力库插件状态管理：
// - 全局 AgentSettingsCache（对等订阅 smelt.agents，零轮询）；
// - 三个视图（main / models / memory）各自独立的 PageController（与 Rust CapabilitiesPage 同构）；
// - 全部 pi.* / system.describe / settings.* / memory.test_embedding RPC 调用。

import { signal } from "nsh";
import {
  call,
  command,
  currentTheme,
  machineMeta,
  machineMetaEntry,
  openUrl,
  run,
  stableMemo,
  t,
  viewShown,
  waitOnline,
} from "smelt/sdk";
import {
  AGENT_SETTINGS,
  AGENT_SETTINGS_CHANGED,
  EDIT_AGENT_SETTINGS,
} from "../contract.js";
import {
  adoptDiscoveredModelRows,
  applyLoginEvent,
  buildCustomProviderDraft,
  buildMemorySavePatch,
  buildMemoryTestParams,
  buildRerankPatch,
  buildSaveDefaultModelParams,
  createLoginView,
  emptyCustomModelRow,
  MEMORY_KEYS,
  machineKey,
  modelsEndpoint,
  normalizeAuthProvider,
  normalizeDiscoveredModel,
  normalizeMachine,
  normalizePiConfig,
  normalizeProviderApi,
  parseMachineAgentSettings,
  parseMemorySettings,
  validateAndBuildSaveCustomProviderParams,
  validateManualModelDraft,
  withAgentUse,
} from "../model.js";

export type CapabilitiesTab = "models" | "skills" | "extensions" | "mcp" | "memory";

export type PiConfigFileKind = "settings" | "models" | "credentials" | "mcp";

export interface ConfirmRequest {
  title: string;
  description: string;
  action: string;
  danger: boolean;
  onConfirm: () => void;
}

export interface ManualModelDraft {
  provider: string;
  model: string;
  baseUrl: string;
  apiKey: string;
}

export interface CustomModelRowDraft {
  key: string;
  id: string;
  name: string;
  contextWindow: string;
  maxTokens: string;
}

export type DiscoveryState =
  | null
  | { status: "loading" }
  | { status: "ready"; models: any[] }
  | { status: "failed"; error: string };

export interface CustomProviderDraft {
  previousId: string | null;
  id: string;
  displayName: string;
  apiKey: string;
  baseUrl: string;
  api: string;
  models: CustomModelRowDraft[];
  discovery: DiscoveryState;
  credentialConfigured: boolean;
}

export type AuthProvidersState =
  | null
  | { status: "loading" }
  | { status: "ready"; providers: any[] }
  | { status: "failed"; error: string };

export interface LoginTarget {
  kind: "provider" | "mcp";
  id?: string;
  name: string;
}

export interface LoginViewState {
  target: LoginTarget;
  loginId: string | null;
  messages: string[];
  authUrl: string | null;
  instructions: string | null;
  deviceCode: { userCode: string; verificationUri: string } | null;
  prompt: {
    id: string;
    kind: string;
    message: string;
    placeholder: string | null;
    options: Array<{ id: string; label: string; description: string | null }>;
  } | null;
  inputValue: string;
  error: string | null;
}

const CALL_TIMEOUT_MS = 30_000;
const AUTH_CALL_TIMEOUT_MS = 180_000;
const MEMORY_TEST_TIMEOUT_MS = 20_000;
const REFRESH_AUTO_TIMEOUT_MS = 300_000;

export { currentTheme };

export function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

export function notifySuccess(body: string) {
  void run("smelt.app/notify", { subtitle: t("notify.done"), body }).catch(() => {});
}

export function notifyError(body: string) {
  void run("smelt.app/notify", { subtitle: t("notify.error"), body }).catch(() => {});
}

// ---- 全局智能体配置缓存（smelt.agents 对等协作） ----

const agentSettingsMap = signal<Record<string, any>>({});
let agentFetchState: "idle" | "loading" | "done" = "idle";
const agentTouched = new Set<string>();

export function agentSettingsOn(machine: string | null): any | null {
  return agentSettingsMap()[machineKey(machine)] ?? null;
}

export function onAgentCommandsChanged() {
  const registered = command(AGENT_SETTINGS) != null;
  if (registered && agentFetchState === "idle") {
    void loadAllAgentSettings();
  } else if (!registered && agentFetchState === "done") {
    agentFetchState = "idle";
    agentTouched.clear();
    agentSettingsMap.set({});
  }
}

async function loadAllAgentSettings() {
  agentFetchState = "loading";
  agentTouched.clear();
  try {
    const list = await run(AGENT_SETTINGS, null);
    const next = { ...agentSettingsMap() };
    if (Array.isArray(list)) {
      for (const raw of list) {
        const parsed = parseMachineAgentSettings(raw);
        if (!parsed) continue;
        const key = machineKey(parsed.machine);
        if (agentTouched.has(key)) continue;
        if (parsed.state === "ready") {
          next[key] = parsed.settings;
        }
      }
    }
    agentTouched.clear();
    agentFetchState = "done";
    agentSettingsMap.set(next);
  } catch (error) {
    console.warn("[capabilities] 取智能体配置失败：", errText(error));
    agentFetchState = "idle";
  }
}

export function applyAgentSettingsEvent(payload: unknown) {
  const parsed = parseMachineAgentSettings(payload);
  if (!parsed) return;
  const key = machineKey(parsed.machine);
  if (agentFetchState === "loading") {
    agentTouched.add(key);
  }
  const next = { ...agentSettingsMap() };
  if (parsed.state === "ready") {
    next[key] = parsed.settings;
  } else {
    delete next[key];
  }
  agentSettingsMap.set(next);
}

// ---- 启动期一次性自动刷新（事件驱动等待本机在线，零轮询） ----

let refreshAutoStarted = false;

export function startRefreshAutoProvidersOnceOnline() {
  if (refreshAutoStarted) return;
  refreshAutoStarted = true;
  void (async () => {
    const online = await waitOnline(null, 0);
    if (!online) return;
    try {
      await call(null, "pi.providers.refresh_auto", {}, REFRESH_AUTO_TIMEOUT_MS);
    } catch (error) {
      console.warn("[capabilities] 刷新自动模型目录失败：", errText(error));
    }
  })();
}

// ---- 视图级页面控制器（main / models / memory 各持有一份，与 Rust CapabilitiesPage 同构） ----

export function createPageController(viewId: string, tabs: readonly CapabilitiesTab[]) {
  const machine = signal<string | null>(null);
  const tab = signal<CapabilitiesTab>(tabs[0]);

  const piConfig = signal<ReturnType<typeof normalizePiConfig> | null>(null);
  const piConfigError = signal<string | null>(null);
  const remoteDesktop = signal(false);

  const authProviders = signal<AuthProvidersState>(null);
  const authShowAll = signal(false);
  const authError = signal<string | null>(null);
  const defaultError = signal<string | null>(null);

  const modelEditor = signal<ManualModelDraft | null>(null);
  const customProviderEditor = signal<CustomProviderDraft | null>(null);
  const modelEditorError = signal<string | null>(null);
  const modelEditorSaving = signal(false);

  const loginView = signal<LoginViewState | null>(null);
  const mcpError = signal<string | null>(null);
  const pluginError = signal<string | null>(null);

  const agentEditPending = signal(false);
  const agentEditError = signal<string | null>(null);
  const confirmDialog = signal<ConfirmRequest | null>(null);

  // 记忆设置表单状态（与当前选中的 machine 绑定）
  const memoryLoading = signal(false);
  const memorySaving = signal(false);
  const memorySaved = signal({ baseUrl: "", model: "", apiKeySet: false, configured: false });
  const memoryRerankModel = signal<{ provider: string; id: string } | null>(null);
  const memoryBaseUrl = signal("");
  const memoryModel = signal("");
  const memoryApiKey = signal("");
  const memoryError = signal<string | null>(null);
  const memoryNotice = signal<string | null>(null);
  const memoryTesting = signal(false);
  const memoryTestResult = signal<{ ok: boolean; text: string } | null>(null);

  let epoch = 0;
  let configGen = 0;
  let memoryGen = 0;
  let loginSeq = 0;
  let initialized = false;
  let wasShown = false;
  let justOpened = false;

  function currentMachineName(): string {
    const m = machine();
    return machineMetaEntry(m)?.name ?? "";
  }

  function pendingText(): string {
    const err = piConfigError();
    if (err) return err;
    return machine() === null ? t("config.loading") : t("config.loading_remote");
  }

  function adoptConfig(raw: any) {
    configGen += 1;
    piConfig.set(normalizePiConfig(raw));
    piConfigError.set(null);
  }

  function refreshPiLibrary() {
    const callEpoch = epoch;
    const gen = ++configGen;
    const target = machine();
    void call(target, "pi.config.get", {}, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch || configGen !== gen) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch || configGen !== gen) return;
        const msg = errText(error);
        console.warn("[capabilities] 读取 Pi 配置失败：", msg);
        piConfigError.set(msg);
      },
    );
  }

  function refreshRemoteDesktop() {
    const callEpoch = epoch;
    const target = machine();
    void call(target, "system.describe", {}, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        remoteDesktop.set(res?.daemon?.desktop === true);
      },
      () => {
        // 离线或旧运行时保留原值
      },
    );
  }

  function refreshAuthProviders() {
    if (authProviders()?.status === "loading") return;
    const callEpoch = epoch;
    const target = machine();
    authError.set(null);
    authProviders.set({ status: "loading" });
    void call(target, "pi.auth.providers", {}, AUTH_CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        const list = Array.isArray(res?.providers)
          ? res.providers.map(normalizeAuthProvider)
          : [];
        authProviders.set({ status: "ready", providers: list });
      },
      (error) => {
        if (epoch !== callEpoch) return;
        authProviders.set({ status: "failed", error: errText(error) });
      },
    );
  }

  function loadTab() {
    const current = tab();
    if (current === "models" && authProviders() === null) {
      refreshAuthProviders();
    } else if (current === "memory") {
      refreshMemorySettings(true);
    }
  }

  function reload() {
    refreshPiLibrary();
    refreshRemoteDesktop();
    loadTab();
  }

  function selectTab(next: CapabilitiesTab) {
    if (!tabs.includes(next) || tab() === next) return;
    tab.set(next);
    loadTab();
  }

  function cancelLogin() {
    const current = loginView();
    loginSeq += 1;
    loginView.set(null);
    if (current?.loginId) {
      const target = machine();
      void call(target, "pi.auth.login_cancel", { login_id: current.loginId }, CALL_TIMEOUT_MS).catch(
        () => {},
      );
    }
  }

  function resetMemoryState() {
    memoryGen += 1;
    memoryLoading.set(false);
    memorySaving.set(false);
    memorySaved.set({ baseUrl: "", model: "", apiKeySet: false, configured: false });
    memoryRerankModel.set(null);
    memoryBaseUrl.set("");
    memoryModel.set("");
    memoryApiKey.set("");
    memoryError.set(null);
    memoryNotice.set(null);
    memoryTesting.set(false);
    memoryTestResult.set(null);
  }

  function selectMachine(nextRaw: string | null) {
    const next = normalizeMachine(nextRaw);
    if (machine() === next) return;
    cancelLogin();
    machine.set(next);
    epoch += 1;
    configGen += 1;
    piConfig.set(null);
    piConfigError.set(null);
    modelEditor.set(null);
    customProviderEditor.set(null);
    modelEditorError.set(null);
    modelEditorSaving.set(false);
    authProviders.set(null);
    authError.set(null);
    defaultError.set(null);
    mcpError.set(null);
    pluginError.set(null);
    remoteDesktop.set(false);
    agentEditPending.set(false);
    agentEditError.set(null);
    confirmDialog.set(null);
    resetMemoryState();
    reload();
  }

  function applyOpenArgs(args: any, forceReload: boolean) {
    if (args && typeof args === "object") {
      if (typeof args.tab === "string" && tabs.includes(args.tab as CapabilitiesTab)) {
        tab.set(args.tab as CapabilitiesTab);
      }
      if (Object.prototype.hasOwnProperty.call(args, "machine")) {
        let next = normalizeMachine(args.machine);
        if (next !== null && !machineMetaEntry(next)) {
          next = null;
        }
        if (next !== machine()) {
          selectMachine(next);
          return;
        }
      }
    }
    if (machine() !== null && !machineMetaEntry(machine())) {
      selectMachine(null);
      return;
    }
    if (forceReload) {
      reload();
    }
  }

  function ensureMounted(initialArgs?: any) {
    if (initialized) return;
    initialized = true;
    wasShown = true;
    applyOpenArgs(initialArgs, true);
  }

  function onOpen(args: any) {
    initialized = true;
    wasShown = true;
    justOpened = true;
    applyOpenArgs(args, true);
  }

  function onShownChanged() {
    const shown = viewShown(viewId);
    const appeared = shown && !wasShown;
    wasShown = shown;
    if (justOpened) {
      justOpened = false;
      return;
    }
    if (appeared && initialized) {
      reload();
    }
  }

  function onMachinesChanged() {
    if (machine() !== null && !machineMetaEntry(machine())) {
      selectMachine(null);
    }
  }

  function onPiConfigChanged(eventMachine: string | null, message: any) {
    if (!initialized) return;
    if (normalizeMachine(eventMachine) !== machine()) return;
    if (message?.kind === "unavailable") return;
    refreshPiLibrary();
  }

  function onReconnected(eventMachine: string | null) {
    if (!initialized) return;
    if (normalizeMachine(eventMachine) !== machine()) return;
    refreshPiLibrary();
    refreshRemoteDesktop();
    if (tab() === "memory") {
      refreshMemorySettings(false);
    }
  }

  // ---- 模型页操作 ----

  function setDefaultModel(provider: string, model: string) {
    defaultError.set(null);
    const callEpoch = epoch;
    const params = buildSaveDefaultModelParams({ provider, model, resetBaseUrl: false });
    void call(machine(), "pi.models.save_default", params, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        defaultError.set(errText(error));
      },
    );
  }

  function clearDefaultModel() {
    defaultError.set(null);
    const callEpoch = epoch;
    void call(machine(), "pi.models.clear_default", {}, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        defaultError.set(errText(error));
      },
    );
  }

  function setThinkingLevel(level: string) {
    defaultError.set(null);
    const callEpoch = epoch;
    void call(machine(), "pi.models.set_thinking_level", { level }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        defaultError.set(errText(error));
      },
    );
  }

  function openManualModelEditor() {
    const cfg = piConfig();
    if (!cfg || !cfg.modelSettings.ok) {
      defaultError.set(cfg ? cfg.modelSettings.error : pendingText());
      return;
    }
    const settings = cfg.modelSettings.value;
    defaultError.set(null);
    modelEditorError.set(null);
    customProviderEditor.set(null);
    const provider = settings.defaultModel?.provider ?? "";
    const model = settings.defaultModel?.model ?? "";
    const baseUrl = settings.endpointOverrides?.[provider] ?? "";
    modelEditor.set({ provider, model, baseUrl, apiKey: "" });
  }

  function updateManualModel(patch: Partial<ManualModelDraft>) {
    const cur = modelEditor();
    if (!cur) return;
    modelEditor.set({ ...cur, ...patch });
  }

  function cancelManualModelEditor() {
    modelEditor.set(null);
    modelEditorError.set(null);
    modelEditorSaving.set(false);
  }

  function saveManualModel() {
    const draft = modelEditor();
    if (!draft || modelEditorSaving()) return;
    const cfg = piConfig();
    const customProviders = cfg?.modelSettings.ok ? cfg.modelSettings.value.customProviders : [];
    const authState = authProviders();
    const builtinProviders = authState?.status === "ready" ? authState.providers : null;
    const check = validateManualModelDraft(draft, customProviders, builtinProviders, t);
    if (!check.ok) {
      modelEditorError.set(check.error);
      return;
    }
    const callEpoch = epoch;
    modelEditorSaving.set(true);
    modelEditorError.set(null);
    void call(machine(), "pi.models.save_default", check.params, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        modelEditorSaving.set(false);
        modelEditor.set(null);
        modelEditorError.set(null);
        adoptConfig(res);
        refreshAuthProviders();
      },
      (error) => {
        if (epoch !== callEpoch) return;
        modelEditorSaving.set(false);
        modelEditorError.set(errText(error));
      },
    );
  }

  function openCustomProviderEditor(providerId: string | null) {
    const cfg = piConfig();
    if (!cfg || !cfg.modelSettings.ok) {
      authError.set(cfg ? cfg.modelSettings.error : pendingText());
      return;
    }
    authError.set(null);
    modelEditor.set(null);
    modelEditorError.set(null);
    modelEditorSaving.set(false);
    const existing = providerId
      ? cfg.modelSettings.value.customProviders.find((p: any) => p.id === providerId) ?? null
      : null;
    const isAuto = existing ? cfg.autoProviderIds.includes(existing.id) : false;
    customProviderEditor.set(buildCustomProviderDraft(existing, isAuto));
  }

  function updateCustomProvider(patch: Partial<CustomProviderDraft>) {
    const cur = customProviderEditor();
    if (!cur) return;
    customProviderEditor.set({ ...cur, ...patch });
  }

  function setCustomProviderApi(api: string) {
    updateCustomProvider({ api: normalizeProviderApi(api) });
  }

  function addCustomModelRow() {
    const cur = customProviderEditor();
    if (!cur) return;
    customProviderEditor.set({ ...cur, models: [...cur.models, emptyCustomModelRow(null)] });
  }

  function updateCustomModelRow(key: string, patch: Partial<CustomModelRowDraft>) {
    const cur = customProviderEditor();
    if (!cur) return;
    customProviderEditor.set({
      ...cur,
      models: cur.models.map((row) => (row.key === key ? { ...row, ...patch } : row)),
    });
  }

  function removeCustomModelRow(key: string) {
    const cur = customProviderEditor();
    if (!cur) return;
    customProviderEditor.set({
      ...cur,
      models: cur.models.filter((row) => row.key !== key),
    });
  }

  function discoverCustomModels() {
    const cur = customProviderEditor();
    if (!cur || cur.discovery?.status === "loading") return;
    const baseUrl = cur.baseUrl.trim();
    const check = modelsEndpoint(baseUrl, t);
    if (!check.ok) {
      customProviderEditor.set({ ...cur, discovery: { status: "failed", error: check.error } });
      return;
    }
    const callEpoch = epoch;
    customProviderEditor.set({ ...cur, discovery: { status: "loading" } });
    const params = {
      base_url: baseUrl,
      api: cur.api,
      api_key: cur.apiKey.trim(),
      saved_provider_id: cur.previousId,
    };
    void call(machine(), "pi.providers.discover_models", params, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        const latest = customProviderEditor();
        if (!latest) return;
        const models = Array.isArray(res?.models)
          ? res.models.map(normalizeDiscoveredModel)
          : [];
        customProviderEditor.set({ ...latest, discovery: { status: "ready", models } });
      },
      (error) => {
        if (epoch !== callEpoch) return;
        const latest = customProviderEditor();
        if (!latest) return;
        customProviderEditor.set({
          ...latest,
          discovery: { status: "failed", error: errText(error) },
        });
      },
    );
  }

  function adoptDiscoveredModel(model: any) {
    const cur = customProviderEditor();
    if (!cur) return;
    customProviderEditor.set({
      ...cur,
      models: adoptDiscoveredModelRows(cur.models, model),
    });
  }

  function cancelCustomProviderEditor() {
    customProviderEditor.set(null);
    modelEditorError.set(null);
    modelEditorSaving.set(false);
  }

  function saveCustomProvider(setDefault: boolean) {
    const cur = customProviderEditor();
    if (!cur || modelEditorSaving()) return;
    const check = validateAndBuildSaveCustomProviderParams(cur, setDefault, t);
    if (!check.ok) {
      if (check.target === "discovery") {
        customProviderEditor.set({ ...cur, discovery: { status: "failed", error: check.error } });
      } else {
        modelEditorError.set(check.error);
      }
      return;
    }
    const callEpoch = epoch;
    modelEditorSaving.set(true);
    modelEditorError.set(null);
    if (check.fromEndpoint) {
      customProviderEditor.set({ ...cur, discovery: { status: "loading" } });
    }
    void call(machine(), "pi.providers.save_custom", check.params, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        modelEditorSaving.set(false);
        customProviderEditor.set(null);
        modelEditorError.set(null);
        adoptConfig(res?.config ?? res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        modelEditorSaving.set(false);
        const msg = errText(error);
        if (check.fromEndpoint) {
          const latest = customProviderEditor();
          if (latest) {
            customProviderEditor.set({ ...latest, discovery: { status: "failed", error: msg } });
          }
        } else {
          modelEditorError.set(msg);
        }
      },
    );
  }

  function removeCustomProvider(providerId: string) {
    authError.set(null);
    const callEpoch = epoch;
    void call(
      machine(),
      "pi.providers.remove_custom",
      { provider_id: providerId },
      CALL_TIMEOUT_MS,
    ).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        authError.set(errText(error));
      },
    );
  }

  function requestRemoveCustomProvider(providerId: string, name: string) {
    authError.set(null);
    confirmDialog.set({
      title: t("provider.remove_title", { name }),
      description: t("provider.remove_question"),
      action: t("provider.remove_confirm"),
      danger: true,
      onConfirm: () => removeCustomProvider(providerId),
    });
  }

  function openConfigFile(file: PiConfigFileKind) {
    if (machine() !== null) {
      notifyError(t("config.remote_file", { name: currentMachineName() }));
      return;
    }
    const callEpoch = epoch;
    void call(null, "pi.config.ensure_file", { file }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        const path = res?.path;
        if (!path) return;
        void run("smelt.app/open-in-text-editor", { path }).catch((error) => {
          notifyError(errText(error));
        });
      },
      (error) => {
        if (epoch !== callEpoch) return;
        notifyError(errText(error));
      },
    );
  }

  function refreshSettingsAndAuth() {
    refreshPiLibrary();
    refreshAuthProviders();
  }

  // ---- 登录会话（Provider + MCP 共用） ----

  function reportLoginError(target: LoginTarget, message: string) {
    if (target.kind === "provider") authError.set(message);
    else mcpError.set(message);
  }

  function runLogin(target: LoginTarget, startRpc: () => Promise<any>) {
    cancelLogin();
    authError.set(null);
    if (target.kind === "mcp") mcpError.set(null);

    const callEpoch = epoch;
    const seq = ++loginSeq;
    const targetMachine = machine();
    loginView.set(createLoginView(target));

    void (async () => {
      let loginId = "";
      try {
        const started = await startRpc();
        loginId = typeof started?.login_id === "string" ? started.login_id : "";
      } catch (error) {
        if (epoch !== callEpoch || seq !== loginSeq) return;
        const cur = loginView();
        if (cur) {
          loginView.set({ ...cur, error: errText(error) });
        } else {
          reportLoginError(target, errText(error));
        }
        return;
      }

      if (!loginId) {
        if (epoch === callEpoch && seq === loginSeq) {
          const cur = loginView();
          if (cur) loginView.set({ ...cur, error: "Missing login_id from runtime" });
        }
        return;
      }

      if (epoch !== callEpoch || seq !== loginSeq || !loginView()) {
        // 面板已被关掉或换机器：撤掉运行时刚起的登录会话。
        void call(targetMachine, "pi.auth.login_cancel", { login_id: loginId }, CALL_TIMEOUT_MS).catch(
          () => {},
        );
        return;
      }

      loginView.set({ ...loginView()!, loginId });

      // 长轮询事件流（服务端挂起最多 20s 等新事件，非定时器轮询）。
      let after = 0;
      for (;;) {
        if (epoch !== callEpoch || seq !== loginSeq) return;
        let batch: { events: any[]; finished: boolean };
        try {
          const res = await call(
            targetMachine,
            "pi.auth.login_events",
            { login_id: loginId, after },
            CALL_TIMEOUT_MS,
          );
          batch = {
            events: Array.isArray(res?.events) ? res.events : [],
            finished: Boolean(res?.finished),
          };
        } catch (error) {
          batch = {
            events: [{ event: "error", message: errText(error) }],
            finished: true,
          };
        }
        if (epoch !== callEpoch || seq !== loginSeq) return;

        after += batch.events.length;
        for (const ev of batch.events) {
          const cur = loginView();
          if (!cur || cur.loginId !== loginId) return;
          const reduced = applyLoginEvent(cur, ev, t);
          loginView.set(reduced.view);
          if (reduced.openUrl) {
            void openUrl(reduced.openUrl).catch(() => {});
          }
          if (reduced.doneTarget) {
            refreshPiLibrary();
            if (reduced.doneTarget.kind === "provider") {
              refreshAuthProviders();
            } else {
              notifySuccess(t("auth.mcp_signed_in", { name: reduced.doneTarget.name }));
            }
            return;
          }
        }
        if (batch.finished) return;
      }
    })();
  }

  function startProviderLogin(
    providerId: string,
    providerName: string,
    method: "oauth" | "api_key",
    askAll: boolean,
  ) {
    const targetMachine = machine();
    runLogin({ kind: "provider", id: providerId, name: providerName }, () =>
      call(
        targetMachine,
        "pi.auth.login_start",
        { provider_id: providerId, method, ask_all: askAll },
        AUTH_CALL_TIMEOUT_MS,
      ),
    );
  }

  function startMcpLogin(serverName: string) {
    const targetMachine = machine();
    runLogin({ kind: "mcp", name: serverName }, () =>
      call(targetMachine, "pi.mcp.login_start", { name: serverName }, AUTH_CALL_TIMEOUT_MS),
    );
  }

  function setLoginInputValue(value: string) {
    const cur = loginView();
    if (!cur) return;
    loginView.set({ ...cur, inputValue: value });
  }

  function sendLoginAnswer(loginId: string, promptId: string, value: string) {
    const callEpoch = epoch;
    void call(
      machine(),
      "pi.auth.login_answer",
      { login_id: loginId, prompt_id: promptId, value },
      CALL_TIMEOUT_MS,
    ).catch((error) => {
      if (epoch !== callEpoch) return;
      const cur = loginView();
      if (cur && cur.loginId === loginId) {
        loginView.set({
          ...cur,
          messages: [...cur.messages, t("auth.submit_failed", { error: errText(error) })],
        });
      } else {
        authError.set(errText(error));
      }
    });
  }

  function submitLoginPrompt() {
    const cur = loginView();
    if (!cur || !cur.prompt || !cur.loginId || cur.prompt.kind === "select") return;
    const value = cur.inputValue.trim();
    const promptId = cur.prompt.id;
    const loginId = cur.loginId;
    loginView.set({ ...cur, prompt: null, inputValue: "" });
    sendLoginAnswer(loginId, promptId, value);
  }

  function chooseLoginOption(optionId: string) {
    const cur = loginView();
    if (!cur || !cur.prompt || !cur.loginId) return;
    const promptId = cur.prompt.id;
    const loginId = cur.loginId;
    loginView.set({ ...cur, prompt: null, inputValue: "" });
    sendLoginAnswer(loginId, promptId, optionId);
  }

  function logoutProvider(providerId: string) {
    authError.set(null);
    const callEpoch = epoch;
    void call(machine(), "pi.auth.logout", { provider_id: providerId }, AUTH_CALL_TIMEOUT_MS).then(
      () => {
        if (epoch !== callEpoch) return;
        refreshPiLibrary();
        refreshAuthProviders();
      },
      (error) => {
        if (epoch !== callEpoch) return;
        authError.set(errText(error));
        refreshAuthProviders();
      },
    );
  }

  function requestSignOutProvider(providerId: string, name: string) {
    authError.set(null);
    confirmDialog.set({
      title: t("auth.sign_out_title", { name }),
      description: t("auth.sign_out_question", { name }),
      action: t("auth.sign_out"),
      danger: true,
      onConfirm: () => logoutProvider(providerId),
    });
  }

  function toggleAuthShowAll() {
    authShowAll.set(!authShowAll());
  }

  // ---- MCP 操作 ----

  function addMcpPreset(id: string) {
    mcpError.set(null);
    const callEpoch = epoch;
    void call(machine(), "pi.mcp.add_preset", { id }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        mcpError.set(errText(error));
      },
    );
  }

  function setMcpServerEnabled(name: string, enabled: boolean) {
    mcpError.set(null);
    const callEpoch = epoch;
    void call(machine(), "pi.mcp.set_enabled", { name, enabled }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        mcpError.set(errText(error));
      },
    );
  }

  function removeMcpServer(name: string) {
    mcpError.set(null);
    const callEpoch = epoch;
    void call(machine(), "pi.mcp.remove", { name }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        mcpError.set(errText(error));
      },
    );
  }

  function requestRemoveMcpServer(name: string) {
    confirmDialog.set({
      title: t("mcp.remove_title", { name }),
      description: t("mcp.remove_body"),
      action: t("mcp.remove"),
      danger: true,
      onConfirm: () => removeMcpServer(name),
    });
  }

  // ---- 插件（技能 / 扩展）操作 ----

  function refreshPlugins() {
    pluginError.set(null);
    refreshPiLibrary();
  }

  function deletePlugin(id: string) {
    const callEpoch = epoch;
    void call(machine(), "pi.plugins.remove", { id }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        pluginError.set(null);
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        pluginError.set(errText(error));
        refreshPiLibrary();
      },
    );
  }

  function requestDeletePlugin(plugin: { id: string; name: string }) {
    pluginError.set(null);
    confirmDialog.set({
      title: t("plugins.delete_title", { name: plugin.name || plugin.id }),
      description: t("plugins.delete_description"),
      action: t("plugins.confirm_delete"),
      danger: true,
      onConfirm: () => deletePlugin(plugin.id),
    });
  }

  function importPlugin(kind: "skill" | "extension") {
    const targetMachine = machine();
    const callEpoch = epoch;
    void run("smelt.app/pick-paths", {
      machine: targetMachine,
      title: t("plugins.import_title"),
      confirm: t("plugins.import_confirm"),
      files: kind === "extension",
      multiple: false,
    }).then(
      (paths) => {
        if (epoch !== callEpoch) return;
        const source = Array.isArray(paths) && typeof paths[0] === "string" ? paths[0] : null;
        if (!source) return;
        pluginError.set(null);
        void call(targetMachine, "pi.plugins.import", { kind, source }, CALL_TIMEOUT_MS).then(
          (res) => {
            if (epoch !== callEpoch) return;
            adoptConfig(res);
          },
          (error) => {
            if (epoch !== callEpoch) return;
            pluginError.set(errText(error));
          },
        );
      },
      (error) => {
        if (epoch !== callEpoch) return;
        notifyError(errText(error));
      },
    );
  }

  function adoptPlugin(id: string, path: string) {
    pluginError.set(null);
    const callEpoch = epoch;
    void call(machine(), "pi.plugins.adopt", { id, path }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        adoptConfig(res);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        pluginError.set(errText(error));
      },
    );
  }

  // ---- 智能体勾选（给智能体用） ----

  function setAgentUse(usable: { kind: "plugin" | "mcp"; id: string }, agentId: string, on: boolean) {
    if (agentEditPending()) return;
    const targetMachine = machine();
    const before = agentSettingsOn(targetMachine);
    if (!before) return;
    const after = withAgentUse(before, usable, agentId, on);
    if (!after) return;

    const callEpoch = epoch;
    agentEditPending.set(true);
    agentEditError.set(null);
    void run(EDIT_AGENT_SETTINGS, { machine: targetMachine, before, after }).then(
      (updated) => {
        if (updated && typeof updated === "object") {
          const next = { ...agentSettingsMap() };
          next[machineKey(targetMachine)] = updated;
          agentSettingsMap.set(next);
        }
        if (epoch !== callEpoch) return;
        agentEditPending.set(false);
        agentEditError.set(null);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        agentEditPending.set(false);
        agentEditError.set(errText(error));
      },
    );
  }

  // ---- 记忆检索设置 ----

  function applyMemorySettingsResult(
    raw: any,
    notice: string | null,
    refillInputs: boolean,
  ) {
    const parsed = parseMemorySettings(raw);
    memorySaved.set(parsed.saved);
    memoryRerankModel.set(parsed.rerankModel);
    memoryError.set(null);
    memoryNotice.set(notice);
    if (refillInputs) {
      memoryBaseUrl.set(parsed.saved.baseUrl);
      memoryModel.set(parsed.saved.model);
      memoryApiKey.set("");
    }
  }

  function refreshMemorySettings(refillInputs: boolean) {
    const callEpoch = epoch;
    const gen = ++memoryGen;
    memoryLoading.set(true);
    void call(machine(), "settings.get", {}, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch || memoryGen !== gen) return;
        memoryLoading.set(false);
        applyMemorySettingsResult(res, null, refillInputs);
      },
      (error) => {
        if (epoch !== callEpoch || memoryGen !== gen) return;
        memoryLoading.set(false);
        memoryError.set(errText(error));
        memoryNotice.set(null);
      },
    );
  }

  function setRerankModel(model: { provider: string; id: string } | null) {
    const callEpoch = epoch;
    const patch = buildRerankPatch(model);
    void call(machine(), "settings.update", patch, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        applyMemorySettingsResult(res, t("memory.rerank_updated"), false);
      },
      (error) => {
        if (epoch !== callEpoch) return;
        memoryError.set(errText(error));
        memoryNotice.set(null);
      },
    );
  }

  function submitMemoryPatch(settings: Record<string, string>, notice: string) {
    if (memorySaving()) return;
    const callEpoch = epoch;
    const gen = ++memoryGen;
    memorySaving.set(true);
    memoryError.set(null);
    void call(machine(), "settings.update", { settings }, CALL_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch || memoryGen !== gen) return;
        memorySaving.set(false);
        applyMemorySettingsResult(res, notice, true);
      },
      (error) => {
        if (epoch !== callEpoch || memoryGen !== gen) return;
        memorySaving.set(false);
        memoryError.set(errText(error));
        memoryNotice.set(null);
      },
    );
  }

  function saveMemorySettings() {
    const built = buildMemorySavePatch(memoryBaseUrl(), memoryModel(), memoryApiKey(), t);
    if (!built.ok) {
      memoryError.set(built.error);
      memoryNotice.set(null);
      return;
    }
    submitMemoryPatch(built.settings, t("memory.saved"));
  }

  function clearMemoryApiKey() {
    submitMemoryPatch({ [MEMORY_KEYS.EMBEDDING_API_KEY]: "" }, t("memory.key_cleared"));
  }

  function clearMemoryAll() {
    submitMemoryPatch(
      {
        [MEMORY_KEYS.EMBEDDING_BASE_URL]: "",
        [MEMORY_KEYS.EMBEDDING_MODEL]: "",
        [MEMORY_KEYS.EMBEDDING_API_KEY]: "",
      },
      t("memory.cleared"),
    );
  }

  function testMemoryEmbedding() {
    if (memoryTesting()) return;
    const built = buildMemoryTestParams(memoryBaseUrl(), memoryModel(), memoryApiKey(), t);
    if (!built.ok) {
      memoryTestResult.set({ ok: false, text: built.error });
      return;
    }
    const callEpoch = epoch;
    memoryTesting.set(true);
    memoryTestResult.set(null);
    void call(machine(), "memory.test_embedding", built.params, MEMORY_TEST_TIMEOUT_MS).then(
      (res) => {
        if (epoch !== callEpoch) return;
        memoryTesting.set(false);
        memoryTestResult.set({
          ok: true,
          text: t("memory.test_ok", {
            dimensions: res?.dimensions ?? 0,
            ms: res?.elapsed_ms ?? 0,
          }),
        });
      },
      (error) => {
        if (epoch !== callEpoch) return;
        memoryTesting.set(false);
        memoryTestResult.set({ ok: false, text: errText(error) });
      },
    );
  }

  return {
    viewId,
    tabs,
    machine,
    tab,
    piConfig,
    piConfigError,
    remoteDesktop,
    authProviders,
    authShowAll,
    authError,
    defaultError,
    modelEditor,
    customProviderEditor,
    modelEditorError,
    modelEditorSaving,
    loginView,
    mcpError,
    pluginError,
    agentEditPending,
    agentEditError,
    confirmDialog,
    memoryLoading,
    memorySaving,
    memorySaved,
    memoryRerankModel,
    memoryBaseUrl,
    memoryModel,
    memoryApiKey,
    memoryError,
    memoryNotice,
    memoryTesting,
    memoryTestResult,
    currentMachineName,
    pendingText,
    ensureMounted,
    onOpen,
    onShownChanged,
    onMachinesChanged,
    onPiConfigChanged,
    onReconnected,
    selectMachine,
    selectTab,
    reload,
    setDefaultModel,
    clearDefaultModel,
    setThinkingLevel,
    openManualModelEditor,
    updateManualModel,
    cancelManualModelEditor,
    saveManualModel,
    openCustomProviderEditor,
    updateCustomProvider,
    setCustomProviderApi,
    addCustomModelRow,
    updateCustomModelRow,
    removeCustomModelRow,
    discoverCustomModels,
    adoptDiscoveredModel,
    cancelCustomProviderEditor,
    saveCustomProvider,
    requestRemoveCustomProvider,
    openConfigFile,
    refreshSettingsAndAuth,
    refreshAuthProviders,
    toggleAuthShowAll,
    startProviderLogin,
    startMcpLogin,
    setLoginInputValue,
    submitLoginPrompt,
    chooseLoginOption,
    cancelLogin,
    requestSignOutProvider,
    addMcpPreset,
    setMcpServerEnabled,
    requestRemoveMcpServer,
    refreshPlugins,
    requestDeletePlugin,
    importPlugin,
    adoptPlugin,
    setAgentUse,
    refreshMemorySettings,
    setRerankModel,
    saveMemorySettings,
    clearMemoryApiKey,
    clearMemoryAll,
    testMemoryEmbedding,
    dismissConfirm: () => confirmDialog.set(null),
  };
}

export type PageController = ReturnType<typeof createPageController>;

export function allMachineList(): any[] {
  return machineMeta();
}
