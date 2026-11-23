// 技能插件（smelt.skills）：通过目标机器 smeltd 的 `skills.*` 控制 RPC 扫描与管理技能，
// 桌面与远程机器行为一致，UI 进程不直接碰本地磁盘。
import { computed, Scroll, signal, Text, View } from "nsh";
import { Button as BaseButton, Dialog, Input } from "nsh/base";
import { Menu } from "nsh/desktop";
import {
  activeContext,
  call,
  initHost,
  machineEntry,
  onEvent,
  onState,
  registerViews,
  revealPath,
  run,
  SmeltButton,
  SmeltIcon,
  stableMemo,
  state,
  t,
  text,
  viewShown,
} from "smelt/sdk";
import {
  brokenConfirmBody,
  describeAction,
  filterSkills,
  formatModified,
  joinNames,
  migrationNotice,
  needsRepair,
  placeLabel,
  prettyPath,
  scopeChoices,
  scopeRequestOf,
  skillChip,
  skillSummary,
} from "./model.js";

const VIEW_NAME = "main";
const VIEW_ID = "smelt.skills/main";
const PICK_PATHS_CMD = "smelt.app/pick-paths";

interface ScopeRequest {
  universal: boolean;
  labels: string[];
  project_scope: boolean;
}

interface PlanState {
  status: "idle" | "loading" | "ok" | "error";
  steps: string[];
  replaces: boolean;
  error: string | null;
}

interface NoticeState {
  error: boolean;
  text: string;
}

interface ScanData {
  home: string;
  trash: string;
  project: string | null;
  installed: string[];
  agents: Array<{
    label: string;
    has_user_dir: boolean;
    has_project_dir: boolean;
    reads_agents_dir: boolean;
  }>;
  skills: any[];
  broken: Array<{ path: string; project_scope: boolean }>;
}

const EMPTY_SCAN: ScanData = {
  home: "",
  trash: "",
  project: null,
  installed: [],
  agents: [],
  skills: [],
  broken: [],
};

const EMPTY_PLAN: PlanState = {
  status: "idle",
  steps: [],
  replaces: false,
  error: null,
};

// ---- 全局状态 ----

const currentTheme = stableMemo(() => state().theme);
const scanData = signal<ScanData>(EMPTY_SCAN);
const loading = signal(false);
const offline = signal(false);
const notice = signal<NoticeState | null>(null);
const query = signal("");

const expandedDir = signal<string | null>(null);
const editingScope = signal(false);
const scopeRequest = signal<ScopeRequest>({ universal: true, labels: [], project_scope: false });
const scopePlan = signal<PlanState>(EMPTY_PLAN);

const importing = signal(false);
const importPicking = signal(false);
const importSubmitting = signal(false);
const importSource = signal<string | null>(null);
const importInfo = signal<{ name: string; description: string; has_global: boolean } | null>(null);
const importError = signal<string | null>(null);
const importRequest = signal<ScopeRequest>({ universal: true, labels: [], project_scope: false });
const importPlan = signal<PlanState>(EMPTY_PLAN);

const keepChoices = signal<Record<string, string>>({});
const deleteDialog = signal<{ name: string; dir: string } | null>(null);
const pruneDialogOpen = signal(false);

let scanGen = 0;
let scopePlanToken = 0;
let importPlanToken = 0;
let lastShown = false;
let lastTargetKey = "";
let lastOnline = false;

function targetContext(): { machine: string | null; project: string | null } {
  const proj = activeContext().project;
  if (proj && typeof proj.root === "string" && proj.root.length > 0) {
    return { machine: proj.machine ?? null, project: proj.root };
  }
  return { machine: null, project: null };
}

function targetKeyOf(target: { machine: string | null; project: string | null }): string {
  return `${target.machine ?? ""}\0${target.project ?? ""}`;
}

function errMsg(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err ?? "");
}

function skillAt(dir: string | null): any | null {
  if (!dir) return null;
  return scanData().skills.find((s) => s.dir === dir) ?? null;
}

// ---- RPC 读盘与规划 ----

async function reload() {
  const { machine, project } = targetContext();
  const online = machineEntry(machine)?.online === true;
  if (!online) {
    offline.set(true);
    return;
  }
  offline.set(false);
  const gen = ++scanGen;
  loading.set(true);
  try {
    const res = await call(machine, "skills.scan", { project });
    if (gen !== scanGen) return;
    loading.set(false);
    applyScan(res);
  } catch (err) {
    if (gen !== scanGen) return;
    loading.set(false);
    notice.set({ error: true, text: errMsg(err) });
  }
}

function applyScan(next: ScanData) {
  const normalized: ScanData = {
    ...EMPTY_SCAN,
    ...(next ?? {}),
    installed: Array.isArray(next?.installed) ? next.installed : [],
    agents: Array.isArray(next?.agents) ? next.agents : [],
    skills: Array.isArray(next?.skills) ? next.skills : [],
    broken: Array.isArray(next?.broken) ? next.broken : [],
  };
  scanData.set(normalized);
  const exp = expandedDir();
  if (exp && !normalized.skills.some((s) => s.dir === exp)) {
    expandedDir.set(null);
    editingScope.set(false);
  }
  const liveDirs = new Set(normalized.skills.map((s) => s.dir));
  const curKeep = keepChoices();
  let keepChanged = false;
  const nextKeep: Record<string, string> = {};
  for (const [k, v] of Object.entries(curKeep)) {
    if (liveDirs.has(k)) nextKeep[k] = v;
    else keepChanged = true;
  }
  if (keepChanged) keepChoices.set(nextKeep);

  if (editingScope()) {
    beginScopeEdit();
  } else if (importing()) {
    void refreshPlan("import");
  }
}

function beginScopeEdit() {
  const skill = skillAt(expandedDir());
  if (!skill) return;
  scopeRequest.set(scopeRequestOf(skill));
  void refreshPlan("scope");
}

function setFormUniversal(kind: "scope" | "import", universal: boolean) {
  const sig = kind === "scope" ? scopeRequest : importRequest;
  sig.set({ ...sig(), universal });
  void refreshPlan(kind);
}

function setFormLevel(kind: "scope" | "import", projectScope: boolean) {
  const data = scanData();
  if (projectScope && !data.project) return;
  const allowed = new Set(
    scopeChoices(data.agents, data.installed, projectScope).map((c) => c.label),
  );
  const sig = kind === "scope" ? scopeRequest : importRequest;
  const cur = sig();
  sig.set({
    ...cur,
    project_scope: projectScope,
    labels: cur.labels.filter((label) => allowed.has(label)),
  });
  void refreshPlan(kind);
}

function toggleFormLabel(kind: "scope" | "import", label: string) {
  const sig = kind === "scope" ? scopeRequest : importRequest;
  const cur = sig();
  if (cur.universal) return;
  const has = cur.labels.includes(label);
  sig.set({
    ...cur,
    labels: has ? cur.labels.filter((l) => l !== label) : [...cur.labels, label],
  });
  void refreshPlan(kind);
}

async function refreshPlan(kind: "scope" | "import") {
  const { machine, project } = targetContext();
  const target = kind === "scope" ? expandedDir() : importSource();
  const planSig = kind === "scope" ? scopePlan : importPlan;
  const req = kind === "scope" ? scopeRequest() : importRequest();
  const token = kind === "scope" ? ++scopePlanToken : ++importPlanToken;
  if (!target) {
    planSig.set(EMPTY_PLAN);
    return;
  }
  planSig.set({ status: "loading", steps: [], replaces: false, error: null });
  try {
    const params =
      kind === "scope"
        ? { project, kind: "scope", dir: target, request: req }
        : { project, kind: "import", source: target, request: req };
    const res = await call(machine, "skills.plan", params);
    const activeToken = kind === "scope" ? scopePlanToken : importPlanToken;
    if (token !== activeToken) return;
    const home = scanData().home;
    const steps = (res?.actions ?? []).map((a: any) => describeAction(home, a, t));
    planSig.set({
      status: "ok",
      steps,
      replaces: Boolean(res?.replaces),
      error: null,
    });
  } catch (err) {
    const activeToken = kind === "scope" ? scopePlanToken : importPlanToken;
    if (token !== activeToken) return;
    planSig.set({
      status: "error",
      steps: [],
      replaces: false,
      error: errMsg(err),
    });
  }
}

// ---- RPC 写操作 ----

async function performApply(
  actionLabel: string,
  op: Record<string, unknown>,
  onSuccess?: (res: any) => NoticeState | null | void,
) {
  const { machine, project } = targetContext();
  try {
    const res = await call(machine, "skills.apply", { project, ...op });
    const custom = onSuccess?.(res);
    notice.set(
      custom ?? {
        error: false,
        text: t("notice.done", { action: actionLabel }),
      },
    );
  } catch (err) {
    notice.set({
      error: true,
      text: t("notice.failed", { action: actionLabel, error: errMsg(err) }),
    });
  }
  await reload();
}

function openImportDialog() {
  editingScope.set(false);
  importSource.set(null);
  importInfo.set(null);
  importError.set(null);
  importRequest.set({ universal: true, labels: [], project_scope: false });
  importPlan.set(EMPTY_PLAN);
  importing.set(true);
}

function closeImportDialog() {
  if (importSubmitting()) return;
  importing.set(false);
  importError.set(null);
}

function toggleScope() {
  const next = !editingScope();
  editingScope.set(next);
  if (next) {
    importing.set(false);
    beginScopeEdit();
  }
}

function toggleRow(dir: string) {
  expandedDir.set(expandedDir() === dir ? null : dir);
  editingScope.set(false);
}

async function pickImportFolder() {
  if (importPicking() || importSubmitting()) return;
  const { machine, project } = targetContext();
  importPicking.set(true);
  importError.set(null);
  try {
    const paths = await run(PICK_PATHS_CMD, {
      machine,
      title: t("import.pick_title"),
      confirm: t("import.pick_confirm"),
      files: false,
      multiple: false,
    });
    const picked = Array.isArray(paths) ? paths[0] : null;
    if (!picked || !importing()) return;
    const info = await call(machine, "skills.inspect_import", { project, source: picked });
    if (!importing()) return;
    importSource.set(picked);
    importInfo.set(info);
    await refreshPlan("import");
  } catch (err) {
    if (importing()) {
      importError.set(t("notice.pick_failed", { error: errMsg(err) }));
    }
  } finally {
    importPicking.set(false);
  }
}

async function submitImport() {
  if (importSubmitting()) return;
  const source = importSource();
  const info = importInfo();
  const plan = importPlan();
  if (!source || !info || plan.status !== "ok" || plan.steps.length === 0) return;
  const { machine, project } = targetContext();
  const actionLabel = t("action.import", { name: info.name });
  importSubmitting.set(true);
  importError.set(null);
  try {
    const res = await call(machine, "skills.apply", {
      project,
      op: "import",
      source,
      request: importRequest(),
    });
    importing.set(false);
    if (res?.destination) expandedDir.set(res.destination);
    notice.set({
      error: false,
      text: t("notice.done", { action: actionLabel }),
    });
    await reload();
  } catch (err) {
    importError.set(t("notice.failed", { action: actionLabel, error: errMsg(err) }));
  } finally {
    importSubmitting.set(false);
  }
}

async function applyScopeChange() {
  const skill = skillAt(expandedDir());
  const plan = scopePlan();
  if (!skill || plan.status !== "ok" || plan.steps.length === 0) return;
  await performApply(
    t("action.rescope", { name: skill.name }),
    { op: "scope", dir: skill.dir, request: scopeRequest() },
    (res) => {
      editingScope.set(false);
      if (res?.destination) expandedDir.set(res.destination);
    },
  );
}

async function repairSkill(skill: any) {
  const missing = (skill.coverage ?? [])
    .filter((row: any) => row.access === "missing")
    .map((row: any) => row.agent);
  await performApply(t("action.repair", { name: skill.name, agents: joinNames(missing, t) }), {
    op: "repair",
    dir: skill.dir,
  });
}

async function repairAllSkills() {
  await performApply(t("action.repair_all"), { op: "repair_all" });
}

async function trashSkill(name: string, dir: string) {
  await performApply(t("delete.action", { name }), { op: "trash", dir });
}

async function pruneBrokenLinks() {
  await performApply(t("broken.action"), { op: "prune_broken" });
}

async function resolveSkillConflict(skill: any) {
  const keep = keepChoices()[skill.dir] ?? skill.dir;
  await performApply(t("conflict.action", { name: skill.name }), {
    op: "resolve_conflict",
    dir: skill.dir,
    keep,
  });
}

async function migrateLegacySkills() {
  await performApply(t("migrate.action"), { op: "migrate_legacy" }, (res) => {
    if (res?.migration) {
      return migrationNotice(res.migration, t);
    }
    return null;
  });
}

async function openLocalTrash() {
  const { machine, project } = targetContext();
  if (machine !== null) return;
  try {
    const res = await call(null, "skills.apply", { project, op: "ensure_trash" });
    const dir = res?.destination || scanData().trash;
    if (dir) await revealPath(dir);
  } catch (err) {
    notice.set({ error: true, text: errMsg(err) });
  }
}

// ---- 事件驱动同步 ----

function syncEvents() {
  const target = targetContext();
  const nextTargetKey = targetKeyOf(target);
  const shown = viewShown(VIEW_ID);
  const appeared = shown && !lastShown;
  lastShown = shown;

  const online = machineEntry(target.machine)?.online === true;
  const becameOnline = online && !lastOnline;
  lastOnline = online;

  if (nextTargetKey !== lastTargetKey) {
    lastTargetKey = nextTargetKey;
    expandedDir.set(null);
    editingScope.set(false);
    importing.set(false);
    if (shown) void reload();
    return;
  }
  if (appeared || (shown && becameOnline)) {
    void reload();
  }
}

onState(() => {
  syncEvents();
});

onEvent((event: any) => {
  if (!event) return;
  if (event.type === "view_opened" && event.view === VIEW_ID) {
    lastShown = true;
    void reload();
    return;
  }
  if (event.type === "reconnected") {
    const { machine } = targetContext();
    if ((event.machine ?? null) === machine && viewShown(VIEW_ID)) {
      void reload();
    }
  }
});

initHost().then(() => {
  registerViews([
    {
      name: VIEW_NAME,
      export: VIEW_NAME,
      title: text("view.title"),
      icon: "icons/book-open.svg",
      draws_top_bar: false,
    },
  ]).catch((err) => console.error("[skills] registerViews 失败：", err));
  syncEvents();
});

// ---- UI 辅助组件 ----

function BadgePill(props: { label: string; tone?: "neutral" | "ok" | "need" | "bad" }) {
  const look = currentTheme;
  const colors = computed(() => {
    const p = look().palette;
    if (props.tone === "ok") return { bg: p.soft, fg: p.ok, border: p.hair };
    if (props.tone === "need") return { bg: p.soft, fg: p.need, border: p.hair };
    if (props.tone === "bad") return { bg: p.soft, fg: p.bad, border: p.bad };
    return { bg: p.soft, fg: p.muted, border: p.hair };
  });
  return (
    <View
      style={{
        paddingLeft: 6,
        paddingRight: 6,
        height: 18,
        borderRadius: look().radius.sm,
        backgroundColor: colors().bg,
        borderWidth: 1,
        borderColor: colors().border,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
      }}
    >
      <Text
        style={{
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
          color: colors().fg,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </View>
  );
}

function RadioItem(props: {
  label: string;
  checked: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  const look = currentTheme;
  return (
    <BaseButton
      disabled={props.disabled}
      onPress={() => {
        if (!props.disabled) props.onSelect();
      }}
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 6,
        opacity: props.disabled ? 0.45 : 1,
      }}
    >
      <View
        style={{
          width: 14,
          height: 14,
          borderRadius: 7,
          borderWidth: 1.5,
          borderColor: props.checked ? look().palette.ink : look().palette.hair_strong,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          flexShrink: 0,
        }}
      >
        <If when={props.checked}>
          <View
            style={{
              width: 6,
              height: 6,
              borderRadius: 3,
              backgroundColor: look().palette.ink,
            }}
          />
        </If>
      </View>
      <Text
        style={{
          fontSize: look().type.small.size,
          lineHeight: look().type.small.line_height,
          color: look().palette.text,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </BaseButton>
  );
}

function CheckItem(props: { label: string; checked: boolean; onToggle: () => void }) {
  const look = currentTheme;
  return (
    <BaseButton
      onPress={() => props.onToggle()}
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 6,
      }}
    >
      <View
        style={{
          width: 14,
          height: 14,
          borderRadius: 3,
          borderWidth: 1.5,
          borderColor: props.checked ? look().palette.ink : look().palette.hair_strong,
          backgroundColor: props.checked ? look().palette.ink : "transparent",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          flexShrink: 0,
        }}
      >
        <If when={props.checked}>
          <SmeltIcon path="icons/check.svg" size={10} color={look().palette.on_ink} />
        </If>
      </View>
      <Text
        style={{
          fontSize: look().type.small.size,
          lineHeight: look().type.small.line_height,
          color: look().palette.text,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </BaseButton>
  );
}

function PlanBlock(props: { plan: PlanState; idleText?: string }) {
  const look = currentTheme;
  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <If when={props.plan.status === "loading"}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("plan.computing")}
        </Text>
      </If>
      <If when={props.plan.status === "error"}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
          {props.plan.error ?? ""}
        </Text>
      </If>
      <If when={props.plan.status === "ok" && props.plan.steps.length === 0 && Boolean(props.idleText)}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {props.idleText ?? ""}
        </Text>
      </If>
      <If when={props.plan.status === "ok" && props.plan.steps.length > 0}>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
          {t("plan.title")}
        </Text>
        <Each of={props.plan.steps} key={(step: string) => step}>
          {(step: () => string) => (
            <Text
              style={{
                fontFamily: look().fonts?.mono,
                fontSize: 11,
                lineHeight: 16,
                color: look().palette.text,
              }}
            >
              {step()}
            </Text>
          )}
        </Each>
      </If>
    </View>
  );
}

function ScopeForm(props: { kind: "scope" | "import" }) {
  const look = currentTheme;
  const req = computed(() => (props.kind === "scope" ? scopeRequest() : importRequest()));
  const data = computed(() => scanData());
  const visibleAgents = computed(() => {
    const d = data();
    const r = req();
    return scopeChoices(d.agents, d.installed, r.project_scope).filter(
      (c) => c.installed || r.labels.includes(c.label),
    );
  });

  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
        <View style={{ width: 56, flexShrink: 0 }}>
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
            {t("scope.for")}
          </Text>
        </View>
        <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 12, flex: 1 }}>
          <RadioItem
            label={t("scope.universal_all")}
            checked={req().universal}
            onSelect={() => setFormUniversal(props.kind, true)}
          />
          <RadioItem
            label={t("scope.specific")}
            checked={!req().universal}
            onSelect={() => setFormUniversal(props.kind, false)}
          />
        </View>
      </View>

      <If when={!req().universal}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            flexWrap: "wrap",
            gap: 8,
            paddingLeft: 64,
          }}
        >
          <Each of={visibleAgents()} key={(c: any) => c.label}>
            {(choice: () => { label: string; installed: boolean }) => (
              <CheckItem
                label={
                  choice().installed
                    ? choice().label
                    : t("scope.not_installed", { agent: choice().label })
                }
                checked={req().labels.includes(choice().label)}
                onToggle={() => toggleFormLabel(props.kind, choice().label)}
              />
            )}
          </Each>
        </View>
      </If>

      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
        <View style={{ width: 56, flexShrink: 0 }}>
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
            {t("scope.level")}
          </Text>
        </View>
        <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 12, flex: 1 }}>
          <RadioItem
            label={t("scope.user")}
            checked={!req().project_scope}
            onSelect={() => setFormLevel(props.kind, false)}
          />
          <RadioItem
            label={
              data().project
                ? t("scope.project_at", { path: prettyPath(data().home, data().project!) })
                : t("scope.project_none")
            }
            checked={Boolean(data().project) && req().project_scope}
            disabled={!data().project}
            onSelect={() => setFormLevel(props.kind, true)}
          />
        </View>
      </View>
    </View>
  );
}

function ImportDialog() {
  const look = currentTheme;
  const info = computed(() => importInfo());
  const source = computed(() => importSource());
  const plan = computed(() => importPlan());
  const ready = computed(
    () => Boolean(source()) && plan().status === "ok" && plan().steps.length > 0,
  );
  const shadowsUser = computed(
    () => Boolean(info()?.has_global) && importRequest().project_scope,
  );

  return (
    <Dialog
      open={importing()}
      role="dialog"
      animation="fade"
      accessibilityLabel={t("import.title")}
      backdropClass="bg-black/40"
      onOpenChange={({ open }: { open: boolean }) => {
        if (!open && !importPicking() && !importSubmitting()) closeImportDialog();
      }}
      class="m-auto rounded-(--radius) border border-border bg-popover p-5"
      style={{ width: 520, gap: 14 }}
    >
      <If when={importing()}>
        <View style={{ display: "flex", flexDirection: "column", gap: 14, width: "100%" }}>
          {/* 标题栏 */}
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <Text
              style={{
                flex: 1,
                fontSize: look().type.ui_medium.size + 1,
                fontWeight: 600,
                color: look().palette.text,
              }}
            >
              {t("import.title")}
            </Text>
            <SmeltButton
              quiet={true}
              small={true}
              icon="icons/close.svg"
              disabled={importSubmitting()}
              accessibilityLabel={t("notice.close")}
              onPress={closeImportDialog}
            />
          </View>

          <Scroll style={{ maxHeight: 460, width: "100%" }}>
            <View style={{ display: "flex", flexDirection: "column", gap: 14, width: "100%" }}>
              {/* 来源 */}
              <View style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall_medium.size,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    color: look().palette.muted,
                  }}
                >
                  {t("import.source")}
                </Text>
                <If
                  when={Boolean(source() && info())}
                  else={
                    <View
                      style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: 8,
                        alignItems: "flex-start",
                        padding: 10,
                        borderWidth: 1,
                        borderColor: look().palette.hair,
                        borderRadius: look().radius.md,
                      }}
                    >
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                        {t("import.source_hint")}
                      </Text>
                      <SmeltButton
                        primary={true}
                        small={true}
                        loading={importPicking()}
                        disabled={importSubmitting()}
                        label={t("import.pick")}
                        onPress={() => void pickImportFolder()}
                      />
                    </View>
                  }
                >
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      gap: 4,
                      padding: 10,
                      borderWidth: 1,
                      borderColor: look().palette.hair,
                      borderRadius: look().radius.md,
                    }}
                  >
                    <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                      <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
                        <Text
                          style={{
                            fontSize: look().type.ui_medium.size,
                            fontWeight: look().type.ui_medium.weight,
                            color: look().palette.text,
                            numberOfLines: 1,
                          }}
                        >
                          {info()?.name ?? ""}
                        </Text>
                        <Text
                          style={{
                            fontFamily: look().fonts?.mono,
                            fontSize: 11,
                            color: look().palette.muted,
                            numberOfLines: 1,
                          }}
                        >
                          {prettyPath(scanData().home, source() ?? "")}
                        </Text>
                      </View>
                      <SmeltButton
                        small={true}
                        loading={importPicking()}
                        disabled={importSubmitting()}
                        label={t("import.repick")}
                        onPress={() => void pickImportFolder()}
                      />
                    </View>
                    <If when={Boolean(info()?.description)}>
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                        {info()?.description ?? ""}
                      </Text>
                    </If>
                  </View>
                </If>
              </View>

              {/* 装到哪里 */}
              <View style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall_medium.size,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    color: look().palette.muted,
                  }}
                >
                  {t("import.destination")}
                </Text>
                <ScopeForm kind="import" />
              </View>

              {/* 计划与提示 */}
              <View style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <If
                  when={Boolean(source())}
                  else={
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                      {t("import.plan_pending")}
                    </Text>
                  }
                >
                  <PlanBlock plan={plan()} />
                </If>
                <If when={plan().status === "ok" && plan().replaces}>
                  <View
                    style={{
                      padding: 8,
                      borderRadius: look().radius.md,
                      borderWidth: 1,
                      borderColor: look().palette.need,
                    }}
                  >
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.need }}>
                      {t("import.replaces", { trash: prettyPath(scanData().home, scanData().trash) })}
                    </Text>
                  </View>
                </If>
                <If when={shadowsUser()}>
                  <View
                    style={{
                      padding: 8,
                      borderRadius: look().radius.md,
                      borderWidth: 1,
                      borderColor: look().palette.need,
                    }}
                  >
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.need }}>
                      {t("import.shadows_user", { name: info()?.name ?? "" })}
                    </Text>
                  </View>
                </If>
                <If when={importError() != null}>
                  <View
                    style={{
                      padding: 8,
                      borderRadius: look().radius.md,
                      borderWidth: 1,
                      borderColor: look().palette.bad,
                    }}
                  >
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                      {importError() ?? ""}
                    </Text>
                  </View>
                </If>
              </View>
            </View>
          </Scroll>

          {/* 底部按钮行 */}
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              disabled={importSubmitting()}
              label={t("cancel")}
              onPress={closeImportDialog}
            />
            <SmeltButton
              primary={true}
              small={true}
              loading={importSubmitting()}
              disabled={!ready() || importSubmitting()}
              label={t("import.button")}
              onPress={() => void submitImport()}
            />
          </View>
        </View>
      </If>
    </Dialog>
  );
}

function SkillRow(props: { skill: any }) {
  const look = currentTheme;
  const s = () => props.skill;
  const open = computed(() => expandedDir() === s().dir);
  const chip = computed(() => skillChip(s(), t));
  const summary = computed(() => skillSummary(s(), t));
  const conflicted = computed(() => (s().conflicts?.length ?? 0) > 0);
  const copies = computed(() => [
    {
      name: s().name,
      description: s().description,
      dir: s().dir,
      placement: s().placement,
      modified_ms: s().modified_ms,
    },
    ...(s().conflicts ?? []),
  ]);
  const selectedKeep = computed(() => keepChoices()[s().dir] ?? s().dir);
  const missingAgents = computed(() =>
    (s().coverage ?? [])
      .filter((row: any) => row.access === "missing")
      .map((row: any) => row.agent),
  );
  const scopeWho = computed(() =>
    s().placement?.kind === "agent"
      ? t("scope.only", { agent: s().placement.label })
      : t("scope.universal_all"),
  );
  const scopeLevel = computed(() => (s().project_scope ? t("scope.project") : t("scope.user")));
  const scopeReady = computed(
    () => scopePlan().status === "ok" && scopePlan().steps.length > 0,
  );
  const isLocal = computed(() => targetContext().machine === null);

  return (
    <View style={{ display: "flex", flexDirection: "column" }}>
      <BaseButton
        onPress={() => toggleRow(s().dir)}
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 12,
          paddingLeft: 16,
          paddingRight: 16,
          paddingTop: 8,
          paddingBottom: 8,
          backgroundColor: open() ? look().palette.row_selected : "transparent",
        }}
        hoverStyle={{
          backgroundColor: open() ? look().palette.row_selected : look().palette.row_hover,
        }}
      >
        <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <Text
              style={{
                fontSize: look().type.ui_medium.size,
                lineHeight: look().type.ui_medium.line_height,
                fontWeight: look().type.ui_medium.weight,
                color: look().palette.text,
                numberOfLines: 1,
                flexShrink: 1,
              }}
            >
              {s().name}
            </Text>
            <BadgePill label={chip().label} tone={chip().tone as any} />
          </View>
          <If when={!open()}>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
                numberOfLines: 1,
              }}
            >
              {s().description || t("no_description")}
            </Text>
          </If>
        </View>
        <If
          when={summary().badge}
          else={
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                color:
                  summary().tone === "bad" ? look().palette.bad : look().palette.muted,
                flexShrink: 0,
              }}
            >
              {summary().label}
            </Text>
          }
        >
          <BadgePill label={summary().label} tone={summary().tone as any} />
        </If>
      </BaseButton>

      <If when={open()}>
        <View
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 12,
            paddingLeft: 16,
            paddingRight: 16,
            paddingTop: 6,
            paddingBottom: 12,
            borderBottomWidth: 1,
            borderBottomColor: look().palette.hair,
          }}
        >
          <If when={Boolean(s().description)}>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.text,
              }}
            >
              {s().description}
            </Text>
          </If>

          {/* 同名冲突 */}
          <If when={conflicted()}>
            <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                <Text
                  style={{
                    flex: 1,
                    fontSize: look().type.text_xsmall_medium.size,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    color: look().palette.muted,
                  }}
                >
                  {t("conflict.title", { name: s().name })}
                </Text>
                <SmeltButton
                  primary={true}
                  small={true}
                  label={t("conflict.keep")}
                  onPress={() => void resolveSkillConflict(s())}
                />
              </View>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("conflict.hint", {
                  count: copies().length,
                  trash: prettyPath(scanData().home, scanData().trash),
                })}
              </Text>
              <View
                style={{
                  display: "flex",
                  flexDirection: "column",
                  gap: 8,
                  padding: 10,
                  borderWidth: 1,
                  borderColor: look().palette.hair,
                  borderRadius: look().radius.lg,
                }}
              >
                <Each of={copies()} key={(c: any) => c.dir}>
                  {(copy: () => any) => (
                    <View style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                      <RadioItem
                        label={`${placeLabel(copy().placement, t)} · ${formatModified(copy().modified_ms, t)}`}
                        checked={selectedKeep() === copy().dir}
                        onSelect={() =>
                          keepChoices.set({ ...keepChoices(), [s().dir]: copy().dir })
                        }
                      />
                      <Text
                        style={{
                          paddingLeft: 20,
                          fontFamily: look().fonts?.mono,
                          fontSize: 11,
                          color: look().palette.muted,
                          numberOfLines: 1,
                        }}
                      >
                        {prettyPath(scanData().home, copy().dir)}
                      </Text>
                    </View>
                  )}
                </Each>
              </View>
            </View>
          </If>

          {/* 可用 agent */}
          <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
              <Text
                style={{
                  flex: 1,
                  fontSize: look().type.text_xsmall_medium.size,
                  fontWeight: look().type.text_xsmall_medium.weight,
                  color: look().palette.muted,
                }}
              >
                {t("coverage.title")}
              </Text>
              <If when={s().placement?.kind === "legacy"}>
                <SmeltButton
                  primary={true}
                  small={true}
                  label={t("migrate.button")}
                  onPress={() => void migrateLegacySkills()}
                />
              </If>
              <If when={s().placement?.kind !== "legacy" && missingAgents().length > 0}>
                <SmeltButton
                  primary={true}
                  small={true}
                  label={
                    missingAgents().length === 1
                      ? t("coverage.repair_one", { agent: missingAgents()[0] })
                      : t("coverage.repair_many")
                  }
                  onPress={() => void repairSkill(s())}
                />
              </If>
            </View>
            <If
              when={s().placement?.kind !== "legacy"}
              else={
                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                  {t("coverage.legacy")}
                </Text>
              }
            >
              <If
                when={(s().coverage?.length ?? 0) > 0}
                else={
                  <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                    {t("coverage.none")}
                  </Text>
                }
              >
                <View style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 4 }}>
                  <Each of={s().coverage ?? []} key={(row: any) => row.agent}>
                    {(row: () => { agent: string; access: string }) => (
                      <BadgePill
                        label={
                          row().access === "missing"
                            ? t("coverage.row_missing", { agent: row().agent })
                            : row().agent
                        }
                        tone={row().access === "missing" ? "need" : "ok"}
                      />
                    )}
                  </Each>
                </View>
              </If>
            </If>
          </View>

          {/* 作用范围 */}
          <If when={!conflicted()}>
            <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall_medium.size,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    color: look().palette.muted,
                    flexShrink: 0,
                  }}
                >
                  {t("scope.title")}
                </Text>
                <Text
                  style={{
                    flex: 1,
                    minWidth: 0,
                    fontSize: look().type.text_xsmall.size,
                    color: look().palette.text,
                    numberOfLines: 1,
                  }}
                >
                  {`${scopeWho()} · ${scopeLevel()}`}
                </Text>
                <SmeltButton
                  small={true}
                  label={editingScope() ? t("scope.hide") : t("scope.change")}
                  onPress={() => toggleScope()}
                />
              </View>

              <If when={editingScope()}>
                <View
                  style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: 10,
                    padding: 12,
                    borderWidth: 1,
                    borderColor: look().palette.hair,
                    borderRadius: look().radius.lg,
                  }}
                >
                  <ScopeForm kind="scope" />
                  <PlanBlock plan={scopePlan()} idleText={t("scope.unchanged")} />
                  <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
                    <SmeltButton small={true} label={t("cancel")} onPress={() => toggleScope()} />
                    <SmeltButton
                      primary={true}
                      small={true}
                      disabled={!scopeReady()}
                      label={t("scope.apply")}
                      onPress={() => void applyScopeChange()}
                    />
                  </View>
                </View>
              </If>
            </View>
          </If>

          {/* 底部路径与删除 */}
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              gap: 8,
              paddingTop: 8,
              borderTopWidth: 1,
              borderTopColor: look().palette.hair,
            }}
          >
            <BaseButton
              disabled={!isLocal()}
              onPress={() => {
                if (isLocal()) void revealPath(s().dir);
              }}
              style={{ flex: 1, minWidth: 0 }}
            >
              <Text
                style={{
                  fontFamily: look().fonts?.mono,
                  fontSize: 11,
                  color: look().palette.muted,
                  numberOfLines: 1,
                }}
              >
                {`${prettyPath(scanData().home, s().dir)} · ${formatModified(s().modified_ms, t)}`}
              </Text>
            </BaseButton>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("delete.button")}
              onPress={() => deleteDialog.set({ name: s().name, dir: s().dir })}
            />
          </View>
        </View>
      </If>
    </View>
  );
}

export function main() {
  const look = currentTheme;
  const shown = computed(() => filterSkills(scanData().skills, query()));
  const projectSkills = computed(() => shown().filter((s) => s.project_scope));
  const userSkills = computed(() => shown().filter((s) => !s.project_scope));
  const hasRepairable = computed(() => scanData().skills.some(needsRepair));
  const showBroken = computed(() => !query().trim() && scanData().broken.length > 0);

  const openMoreMenu = (e: any) => {
    const isLocal = targetContext().machine === null;
    const items: any[] = [
      {
        label: t("menu.refresh"),
        onSelect: () => {
          notice.set(null);
          void reload();
        },
      },
    ];
    if (isLocal) {
      items.push({
        label: t("menu.open_trash"),
        onSelect: () => void openLocalTrash(),
      });
    }
    if (hasRepairable()) {
      items.push({
        label: t("action.repair_all"),
        onSelect: () => void repairAllSkills(),
      });
    }
    Menu.popup(items, e?.x != null && e?.y != null ? { x: e.x, y: e.y } : undefined);
  };

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
      }}
    >
      {/* 顶栏：搜索、导入、更多 */}
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 8,
          paddingLeft: 16,
          paddingRight: 16,
          paddingTop: 8,
          paddingBottom: 8,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
          flexShrink: 0,
        }}
      >
        <View style={{ flex: 1, minWidth: 0 }}>
          <Input
            value={query()}
            placeholder={t("search.placeholder")}
            accessibilityLabel={t("search.label")}
            onChange={({ value }: { value: string }) => query.set(value)}
            style={{ width: "100%", height: 26, fontSize: look().type.small.size }}
          />
        </View>
        <SmeltButton
          primary={true}
          small={true}
          label={t("import.button")}
          onPress={openImportDialog}
        />
        <SmeltButton
          quiet={true}
          small={true}
          icon="icons/ellipsis.svg"
          accessibilityLabel={t("menu.more")}
          onPress={openMoreMenu}
        />
      </View>

      {/* 结果通知 */}
      <If when={notice() != null}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 8,
            paddingLeft: 16,
            paddingRight: 16,
            paddingTop: 8,
            flexShrink: 0,
          }}
        >
          <View
            style={{
              flex: 1,
              minWidth: 0,
              ...(notice()?.error
                ? {
                    paddingLeft: 10,
                    paddingRight: 10,
                    paddingTop: 6,
                    paddingBottom: 6,
                    borderRadius: look().radius.md,
                    borderWidth: 1,
                    borderColor: look().palette.bad,
                  }
                : {}),
            }}
          >
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: notice()?.error ? look().palette.bad : look().palette.muted,
              }}
            >
              {notice()?.text ?? ""}
            </Text>
          </View>
          <SmeltButton
            quiet={true}
            small={true}
            label={t("notice.close")}
            onPress={() => notice.set(null)}
          />
        </View>
      </If>

      <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
        <View style={{ display: "flex", flexDirection: "column", paddingBottom: 16 }}>
          <If when={offline()}>
            <View style={{ padding: 16 }}>
              <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                {t("offline")}
              </Text>
            </View>
          </If>

          <If when={!offline() && !loading() && scanData().skills.length === 0}>
            <View style={{ padding: 16, display: "flex", flexDirection: "column", gap: 4 }}>
              <Text
                style={{
                  fontSize: look().type.ui_medium.size,
                  fontWeight: look().type.ui_medium.weight,
                  color: look().palette.text,
                }}
              >
                {t("empty.none_title")}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("empty.none_body")}
              </Text>
            </View>
          </If>

          <If when={!offline() && scanData().skills.length > 0 && shown().length === 0}>
            <View style={{ padding: 16, display: "flex", flexDirection: "column", gap: 4 }}>
              <Text
                style={{
                  fontSize: look().type.ui_medium.size,
                  fontWeight: look().type.ui_medium.weight,
                  color: look().palette.text,
                }}
              >
                {t("empty.no_match_title")}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("empty.no_match_body", { query: query().trim() })}
              </Text>
            </View>
          </If>

          {/* 项目级分组 */}
          <If when={projectSkills().length > 0}>
            <View
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                paddingLeft: 16,
                paddingRight: 16,
                paddingTop: 12,
                paddingBottom: 4,
              }}
            >
              <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                {`${t("scope.project")} ${projectSkills().length}${
                  scanData().project ? ` · ${prettyPath(scanData().home, scanData().project!)}` : ""
                }`}
              </Text>
            </View>
            <Each of={projectSkills()} key={(s: any) => s.dir}>
              {(s: () => any) => <SkillRow skill={s()} />}
            </Each>
          </If>

          {/* 用户级分组 */}
          <If when={userSkills().length > 0}>
            <View
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                paddingLeft: 16,
                paddingRight: 16,
                paddingTop: 12,
                paddingBottom: 4,
              }}
            >
              <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                {`${t("scope.user")} ${userSkills().length}`}
              </Text>
            </View>
            <Each of={userSkills()} key={(s: any) => s.dir}>
              {(s: () => any) => <SkillRow skill={s()} />}
            </Each>
          </If>

          {/* 失效软链 */}
          <If when={showBroken()}>
            <View style={{ display: "flex", flexDirection: "column" }}>
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 6,
                  paddingLeft: 16,
                  paddingRight: 16,
                  paddingTop: 12,
                  paddingBottom: 4,
                }}
              >
                <Text
                  style={{
                    flex: 1,
                    fontSize: look().type.small.size,
                    color: look().palette.muted,
                  }}
                >
                  {`${t("broken.title")} ${scanData().broken.length}`}
                </Text>
                <SmeltButton
                  quiet={true}
                  small={true}
                  label={t("broken.button")}
                  onPress={() => pruneDialogOpen.set(true)}
                />
              </View>
              <Each of={scanData().broken} key={(b: any) => b.path}>
                {(b: () => { path: string }) => (
                  <Text
                    style={{
                      paddingLeft: 16,
                      paddingRight: 16,
                      paddingTop: 2,
                      paddingBottom: 2,
                      fontFamily: look().fonts?.mono,
                      fontSize: 11,
                      color: look().palette.muted,
                      numberOfLines: 1,
                    }}
                  >
                    {prettyPath(scanData().home, b().path)}
                  </Text>
                )}
              </Each>
            </View>
          </If>
        </View>
      </Scroll>

      {/* 导入弹窗 */}
      <ImportDialog />

      {/* 删除确认框 */}
      <Dialog
        open={deleteDialog() != null}
        role="alertdialog"
        animation="fade"
        accessibilityLabel={t("delete.title", { name: deleteDialog()?.name ?? "" })}
        backdropClass="bg-black/40"
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) deleteDialog.set(null);
        }}
        class="m-auto rounded-(--radius) border border-border bg-popover p-4"
        style={{ width: 420, gap: 12 }}
      >
        <If when={deleteDialog() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("delete.title", { name: deleteDialog()?.name ?? "" })}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {t("delete.body", { trash: prettyPath(scanData().home, scanData().trash) })}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("cancel")}
              onPress={() => deleteDialog.set(null)}
            />
            <SmeltButton
              danger={true}
              small={true}
              label={t("delete.confirm")}
              onPress={() => {
                const target = deleteDialog();
                deleteDialog.set(null);
                if (target) void trashSkill(target.name, target.dir);
              }}
            />
          </View>
        </If>
      </Dialog>

      {/* 清理失效链接确认框 */}
      <Dialog
        open={pruneDialogOpen()}
        role="alertdialog"
        animation="fade"
        accessibilityLabel={
          scanData().broken.length === 1
            ? t("broken.confirm_title.one")
            : t("broken.confirm_title.other", { count: scanData().broken.length })
        }
        backdropClass="bg-black/40"
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) pruneDialogOpen.set(false);
        }}
        class="m-auto rounded-(--radius) border border-border bg-popover p-4"
        style={{ width: 420, gap: 12 }}
      >
        <If when={pruneDialogOpen()}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {scanData().broken.length === 1
              ? t("broken.confirm_title.one")
              : t("broken.confirm_title.other", { count: scanData().broken.length })}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {brokenConfirmBody(scanData().home, scanData().broken, t)}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("cancel")}
              onPress={() => pruneDialogOpen.set(false)}
            />
            <SmeltButton
              primary={true}
              small={true}
              label={t("broken.confirm")}
              onPress={() => {
                pruneDialogOpen.set(false);
                void pruneBrokenLinks();
              }}
            />
          </View>
        </If>
      </Dialog>
    </View>
  );
}

export default main;
