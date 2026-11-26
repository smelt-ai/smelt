// 文件面板（NSH A2 独立 VM 版本）：
// - 左侧：面包屑 + 脏点 + 错误 + 本地 HTML「浏览器打开」+ Markdown 预览/源码切换 + 树收起按钮，
//         主体用 1st-class Rust <SmeltEditor>、<SmeltMarkdown>、<Image data:... />
// - 右侧：<ResizablePanelGroup> 无 JS 往返拖动分栏，搜索框 + 钉目录按钮 + 键盘可控文件树 / 搜索结果树
// - 全事件驱动：project.watch / project_changed / reconnected / window_activated / active_context，无轮询。
import { computed, effect, Image, Scroll, signal, Text, View } from "nsh";
import {
  Input,
  MenuItem,
  MenuSeparator,
  ResizablePanel,
  ResizablePanelGroup,
} from "nsh/base";
import { Menu } from "nsh/desktop";
import {
  activeContext,
  bootTask,
  call,
  callDetail,
  clipboardWrite,
  currentTheme,
  DIALOG_SURFACE_CLASS,
  initHost,
  localOs,
  MENU_ITEM_CLASS,
  onCommand,
  onEvent,
  onSlices,
  openUrl,
  pathFileName,
  pathJoin,
  pathParent,
  pathStartsWith,
  pathStripPrefix,
  pathStyle,
  registerCommands,
  registerFileOpener,
  registerViews,
  revealPath,
  run,
  SmeltButton,
  SmeltContextMenu,
  SmeltDialog,
  SmeltDisclosureChevron,
  SmeltEditor,
  SmeltIcon,
  SmeltMarkdown,
  SmeltTooltip,
  stableMemo,
  store,
  t,
  text,
  viewShown,
  windowActive,
} from "smelt/sdk";

import {
  ancestorsUnderRoot,
  breadcrumbSegments,
  buildSearchTree,
  collectFileTreeRows,
  DEFAULT_TREE_WIDTH,
  editorLanguageForPath,
  fileTreeRowId,
  fileTreeRowPath,
  flattenSearchTree,
  isHtmlPath,
  isMarkdownPath,
  MAX_TREE_WIDTH,
  MIN_TREE_WIDTH,
  nameLikelyTruncated,
  pathToFileUrl,
  projectKey,
  treeEntries,
  treeRoots,
} from "./model.js";
import { FileContentCache } from "./cache.js";
import { clampTreeWidth, createFilesStore } from "./store.js";

const VIEW_ID = "smelt.files/main";
const OPEN_VIEW_CMD = "smelt.app/open-view";
const PICK_PATHS_CMD = "smelt.app/pick-paths";
const NOTIFY_CMD = "smelt.app/notify";
// 约定：smelt.sessions 的 SEND_TEXT（权威定义在 sessions contract）。
const SEND_TEXT_CMD = "smelt.sessions/send-text";

const SEARCH_DEBOUNCE_MS = 180;
/** 远程拉大文件、大图（经中继最大 20 MB）比一般调用慢，和手机端一样给两分钟。 */
const FILE_OPEN_TIMEOUT_MS = 120_000;
const PREVIEW_DEBOUNCE_MS = 80;

interface ProjectRef {
  machine: string | null;
  root: string;
}

interface FileRef {
  machine: string | null;
  path: string;
}

interface DirListing {
  entries: Array<{ name: string; is_dir: boolean }>;
  stale: boolean;
}

interface GitCacheEntry {
  value: { files: Array<[string, string]> };
  stale: boolean;
}

/** 读文件到哪一步：读完前、读不出来都画在内容区，不让预览或编辑器显示成一片空白。 */
type OpenFileStatus =
  | { kind: "loading" }
  | { kind: "ready" }
  | { kind: "unreadable"; message: string; canRetry: boolean };

interface OpenFileState {
  id: number;
  machine: string | null;
  path: string;
  status: OpenFileStatus;
  /** 点过「解锁编辑」或已经改过：项目外也能改。在不在项目里随根目录实时算，不存。 */
  unlocked: boolean;
  content: string;
  savedContent: string;
  savedVersion: string | null;
  imageDataUri: string | null;
  saveError: string | null;
  readable: boolean;
  conflictPending: boolean;
  reloadInflight: boolean;
  reloadAgain: boolean;
  preview: boolean;
  previewSource: string;
  pendingGotoLine: number | null;
}

interface ParkedProjectUi {
  openFile: OpenFileState | null;
  selected: string | null;
  expanded: Set<string>;
  collapsedRoots: Set<string>;
  pendingReveal: string | null;
}

interface SearchHit {
  path: string;
  rel: string;
  line: [number, string] | null;
}

interface SearchState {
  machine: string | null;
  root: string;
  query: string;
  done: boolean;
  hits: SearchHit[];
  truncated: boolean;
}

interface UnsavedSwitchDialog {
  currentId: number;
  currentName: string;
  target: FileRef;
  targetName: string;
  line: number | null;
}

interface DeleteDialog {
  path: string;
  name: string;
  isDir: boolean;
}

const filesStore = createFilesStore(store);

// ---- 模块级响应式状态（与命令处理器、视图渲染共用） ----

const currentProject = signal<ProjectRef | null>(null);
const treeWidth = signal<number>(DEFAULT_TREE_WIDTH);
const treeOpen = signal<boolean>(true);
const pinnedRoots = signal<string[]>([]);
const expandedDirs = signal<Set<string>>(new Set());
const collapsedRoots = signal<Set<string>>(new Set());
const selectedPath = signal<string | null>(null);
const openFile = signal<OpenFileState | null>(null);
const filterQuery = signal<string>("");
const searchState = signal<SearchState | null>(null);
const unsavedDialog = signal<UnsavedSwitchDialog | null>(null);
const deleteDialog = signal<DeleteDialog | null>(null);
const cacheVersion = signal<number>(0);

// 非渲染热数据（通过 cacheVersion 触发重算）
const dirCache = new Map<string, Map<string, DirListing>>();
const dirInflight = new Set<string>();
const dirGeneration = new Map<string, number>();

const gitMarks = new Map<string, GitCacheEntry>();
const gitInflight = new Set<string>();
const gitGeneration = new Map<string, number>();

const watches = new Set<string>();
const parked = new Map<string, ParkedProjectUi>();
let focusEditorOnMount = false;
/** 打开过的文本 / 图片内容，跨项目、跨切换复用。 */
const fileCache = new FileContentCache();

let nextFileId = 1;
let pendingReveal: string | null = null;
let pendingSwitchAfterSave: { fromId: number; target: FileRef; line: number | null } | null = null;
let searchTimer: ReturnType<typeof setTimeout> | null = null;
let searchSeq = 0;
let previewTimer: ReturnType<typeof setTimeout> | null = null;
let activeEditorApi: any = null;
let activeEditorFileId = 0;
let settingsLoaded = false;
let projectSwitchSeq = 0;
let lastWindowActive = true;

function bumpCache() {
  cacheVersion.set(cacheVersion() + 1);
}

function treeMachine(): string | null {
  return currentProject()?.machine ?? null;
}

function styleOpsFor(machine: string | null) {
  const st = pathStyle(machine);
  return {
    isWindows: st === "windows",
    join: (base: string, child: string) => pathJoin(st, base, child),
    fileName: (p: string) => pathFileName(st, p),
    startsWith: (p: string, base: string) => pathStartsWith(st, p, base),
    stripPrefix: (p: string, base: string) => pathStripPrefix(st, p, base),
  };
}

function currentRoots(): string[] {
  return treeRoots(currentProject(), pinnedRoots());
}

function isDirty(of: OpenFileState | null): boolean {
  if (!of || !of.readable || of.status.kind !== "ready") return false;
  return of.content !== of.savedContent;
}

/** 文件在不在当前项目（含钉住的目录）里：只有树那台机器上的、落在某个根下的才算。 */
function isInsideProject(machine: string | null, path: string): boolean {
  if (machine !== treeMachine()) return false;
  const ops = styleOpsFor(machine);
  return currentRoots().some((root) => ops.startsWith(path, root));
}

/** 项目外的文件默认只读防误改；钉住或移走目录后跟着变，解锁过就一直能改。 */
function isReadOnly(of: OpenFileState): boolean {
  return !of.unlocked && !isInsideProject(of.machine, of.path);
}

function getDirMap(machine: string | null): Map<string, DirListing> {
  const key = machine ?? "";
  let map = dirCache.get(key);
  if (!map) {
    map = new Map();
    dirCache.set(key, map);
  }
  return map;
}

function notifyError(body: string) {
  void run(NOTIFY_CMD, { subtitle: t("notify.error"), body }).catch(() => {});
}

function findOpenFileById(id: number): { current: boolean; file: OpenFileState } | null {
  const cur = openFile();
  if (cur && cur.id === id) return { current: true, file: cur };
  for (const ui of parked.values()) {
    if (ui.openFile && ui.openFile.id === id) {
      return { current: false, file: ui.openFile };
    }
  }
  return null;
}

function patchOpenFileById(id: number, mutator: (prev: OpenFileState) => OpenFileState): boolean {
  const cur = openFile();
  if (cur && cur.id === id) {
    openFile.set(mutator(cur));
    return true;
  }
  for (const [k, ui] of parked.entries()) {
    if (ui.openFile && ui.openFile.id === id) {
      parked.set(k, { ...ui, openFile: mutator(ui.openFile) });
      return true;
    }
  }
  return false;
}

// ---- 持久化 ----

function persistCurrentProject() {
  const proj = currentProject();
  if (!proj) return;
  void filesStore.saveProject(proj.machine, proj.root, {
    selected: selectedPath(),
    openFile: openFile()?.path ?? null,
    expanded: expandedDirs(),
    collapsedRoots: collapsedRoots(),
  });
}

function persistTreeLayout() {
  void filesStore.saveTreeLayout(treeWidth(), treeOpen());
}

function persistPinned() {
  void filesStore.savePinned(pinnedRoots());
}

// ---- 监听与失效 ----

const failedWatches = new Set<string>();
const failedDirs = new Set<string>();
const failedGitMarks = new Set<string>();

function clearFailedWhere(set: Set<string>, predicate: (machine: string | null, path: string) => boolean) {
  for (const k of [...set]) {
    const sep = k.indexOf("\0");
    const m = sep > 0 ? k.slice(0, sep) : null;
    const p = sep >= 0 ? k.slice(sep + 1) : k;
    if (predicate(m, p)) set.delete(k);
  }
}

function ensureWatch(machine: string | null, root: string) {
  const key = projectKey(machine, root);
  if (watches.has(key) || failedWatches.has(key)) return;
  watches.add(key);
  void call(machine, "project.watch", { root }).catch((error) => {
    watches.delete(key);
    failedWatches.add(key);
    console.error(`[files] project.watch(${root}) failed:`, error);
  });
}

function markDirsStaleWhere(predicate: (machine: string | null, dir: string) => boolean) {
  clearFailedWhere(failedDirs, predicate);
  for (const [mKey, map] of dirCache.entries()) {
    const m = mKey === "" ? null : mKey;
    for (const [dir, listing] of map.entries()) {
      if (predicate(m, dir)) {
        const k = projectKey(m, dir);
        dirGeneration.set(k, (dirGeneration.get(k) ?? 0) + 1);
        listing.stale = true;
      }
    }
  }
  for (const k of dirInflight) {
    const sep = k.indexOf("\0");
    const m = sep > 0 ? k.slice(0, sep) : null;
    const dir = sep >= 0 ? k.slice(sep + 1) : k;
    if (predicate(m, dir)) {
      dirGeneration.set(k, (dirGeneration.get(k) ?? 0) + 1);
    }
  }
}

function markGitMarksStaleWhere(predicate: (machine: string | null, root: string) => boolean) {
  clearFailedWhere(failedGitMarks, predicate);
  const matched = new Set<string>();
  for (const k of gitMarks.keys()) matched.add(k);
  for (const k of gitInflight) matched.add(k);
  for (const k of matched) {
    const sep = k.indexOf("\0");
    const m = sep > 0 ? k.slice(0, sep) : null;
    const root = sep >= 0 ? k.slice(sep + 1) : k;
    if (!predicate(m, root)) continue;
    gitGeneration.set(k, (gitGeneration.get(k) ?? 0) + 1);
    const cached = gitMarks.get(k);
    if (cached) cached.stale = true;
  }
}

function markEverythingStale(machineFilter: string | null | undefined) {
  const matchesMachine = (m: string | null) => machineFilter === undefined || m === machineFilter;
  clearFailedWhere(failedWatches, (m) => matchesMachine(m));
  for (const key of watches) {
    const sep = key.indexOf("\0");
    const m = sep > 0 ? key.slice(0, sep) : null;
    const root = sep >= 0 ? key.slice(sep + 1) : key;
    if (matchesMachine(m)) {
      onOpenFileProjectChanged(m, root);
    }
  }
  markGitMarksStaleWhere((m) => matchesMachine(m));
  markDirsStaleWhere((m) => matchesMachine(m));
}

// ---- 目录与 Git 状态抓取 ----

function treeRootFor(machine: string | null, path: string): string | null {
  if (machine !== treeMachine()) return null;
  const ops = styleOpsFor(machine);
  let best: string | null = null;
  for (const root of currentRoots()) {
    if (ops.startsWith(path, root) && (best == null || root.length > best.length)) {
      best = root;
    }
  }
  return best;
}

function ensureDir(machine: string | null, dir: string) {
  const key = projectKey(machine, dir);
  const map = getDirMap(machine);
  const existing = map.get(dir);
  if ((existing && !existing.stale) || dirInflight.has(key) || failedDirs.has(key)) return;
  dirInflight.add(key);
  const gen = dirGeneration.get(key) ?? 0;

  void callDetail(machine, "fs.read_dir", { path: dir })
    .then((outcome) => {
      dirInflight.delete(key);
      // 目录没了、没权限读：当空目录显示。连不上之类的失败等重连后再取。
      const unreadable =
        !outcome.ok && (outcome.code === "not_found" || outcome.code === "permission_denied");
      if (!outcome.ok && !unreadable) {
        failedDirs.add(key);
        return;
      }
      failedDirs.delete(key);
      const entries = outcome.ok ? treeEntries(outcome.value?.entries) : [];
      const stale = (dirGeneration.get(key) ?? 0) !== gen;
      getDirMap(machine).set(dir, { entries, stale });
      bumpCache();
      tryFlushReveal();
      if (stale && viewShown(VIEW_ID)) {
        refreshVisible();
      }
    })
    .catch(() => {
      dirInflight.delete(key);
      failedDirs.add(key);
    });
}

function ensureGitStatus(machine: string | null, root: string) {
  const key = projectKey(machine, root);
  const existing = gitMarks.get(key);
  if ((existing && !existing.stale) || gitInflight.has(key) || failedGitMarks.has(key)) return;
  gitInflight.add(key);
  const gen = gitGeneration.get(key) ?? 0;

  void call(machine, "git.status", { root })
    .then((result: any) => {
      gitInflight.delete(key);
      failedGitMarks.delete(key);
      const stale = (gitGeneration.get(key) ?? 0) !== gen;
      const files = Array.isArray(result?.files) ? result.files : [];
      gitMarks.set(key, { value: { files }, stale });
      bumpCache();
      if (stale && viewShown(VIEW_ID)) {
        ensureGitStatus(machine, root);
      }
    })
    .catch(() => {
      gitInflight.delete(key);
      failedGitMarks.add(key);
    });
}

function refreshVisible() {
  if (!viewShown(VIEW_ID)) return;
  const machine = treeMachine();
  const roots = currentRoots();
  const ops = styleOpsFor(machine);
  const collapsed = collapsedRoots();
  const expanded = expandedDirs();

  for (const root of roots) {
    ensureWatch(machine, root);
    if (roots.length > 1 && collapsed.has(root)) continue;
    ensureDir(machine, root);
    ensureGitStatus(machine, root);
    for (const dir of expanded) {
      if (dir !== root && ops.startsWith(dir, root)) {
        ensureDir(machine, dir);
      }
    }
  }
}

// ---- 文件树定位与交互 ----

function currentTreeRows() {
  cacheVersion();
  const machine = treeMachine();
  const roots = currentRoots();
  const ops = styleOpsFor(machine);
  const map = dirCache.get(machine ?? "") ?? new Map();
  return collectFileTreeRows(
    ops,
    {
      roots,
      pinned: pinnedRoots(),
      collapsed: collapsedRoots(),
    },
    expandedDirs(),
    map,
    (root: string) => gitMarks.get(projectKey(machine, root))?.value ?? null,
  );
}

function revealInTree(path: string) {
  const machine = treeMachine();
  const root = treeRootFor(machine, path);
  if (!root) return;
  const ops = styleOpsFor(machine);
  const nextCollapsed = new Set(collapsedRoots());
  const wasCollapsed = nextCollapsed.delete(root);
  if (wasCollapsed) collapsedRoots.set(nextCollapsed);

  const ancestors = ancestorsUnderRoot(ops, path, root);
  const nextExpanded = new Set(expandedDirs());
  let changed = wasCollapsed;
  for (const dir of ancestors) {
    if (!nextExpanded.has(dir)) {
      nextExpanded.add(dir);
      changed = true;
    }
  }
  if (changed) {
    expandedDirs.set(nextExpanded);
    persistCurrentProject();
  }
  selectedPath.set(path);
  pendingReveal = path;
  refreshVisible();
  tryFlushReveal();
}

function revealFromBreadcrumb(path: string, isDir: boolean) {
  if (!treeOpen()) {
    treeOpen.set(true);
    persistTreeLayout();
  }
  if (isDir) {
    const nextExpanded = new Set(expandedDirs());
    if (!nextExpanded.has(path)) {
      nextExpanded.add(path);
      expandedDirs.set(nextExpanded);
    }
  }
  revealInTree(path);
}

function tryFlushReveal() {
  if (!pendingReveal) return;
  const rows = currentTreeRows();
  if (rows.some((r) => fileTreeRowPath(r) === pendingReveal)) {
    selectedPath.set(pendingReveal);
    pendingReveal = null;
  }
}

function toggleRootCollapsed(root: string) {
  const next = new Set(collapsedRoots());
  if (next.has(root)) {
    next.delete(root);
  } else {
    next.add(root);
  }
  collapsedRoots.set(next);
  persistCurrentProject();
  refreshVisible();
}

function toggleExpandDir(dir: string) {
  const next = new Set(expandedDirs());
  if (next.has(dir)) {
    next.delete(dir);
  } else {
    next.add(dir);
  }
  expandedDirs.set(next);
  persistCurrentProject();
  refreshVisible();
}

function unpinRoot(root: string) {
  const next = pinnedRoots().filter((r) => r !== root);
  pinnedRoots.set(next);
  persistPinned();
  refreshVisible();
}

async function pickPinnedRoots() {
  if (treeMachine() != null) return;
  try {
    const picked = await run(PICK_PATHS_CMD, {
      machine: null,
      title: t("pin.pick_title"),
      confirm: t("pin.pick_confirm"),
      files: false,
      multiple: true,
    });
    if (!Array.isArray(picked) || picked.length === 0) return;
    const next = [...pinnedRoots()];
    let added = false;
    for (const p of picked) {
      if (typeof p === "string" && p && !next.includes(p)) {
        next.push(p);
        added = true;
      }
    }
    if (added) {
      pinnedRoots.set(next);
      persistPinned();
      refreshVisible();
    }
  } catch (error: any) {
    notifyError(error?.message ?? String(error));
  }
}

// ---- 文件打开 / 保存 / 外部变更重读 / 预览 ----

function schedulePreviewRefresh() {
  const cur = openFile();
  if (!cur || !cur.preview || !isMarkdownPath(cur.path)) return;
  if (previewTimer) clearTimeout(previewTimer);
  const fileId = cur.id;
  previewTimer = setTimeout(() => {
    previewTimer = null;
    patchOpenFileById(fileId, (of) =>
      of.preview && of.previewSource !== of.content ? { ...of, previewSource: of.content } : of,
    );
  }, PREVIEW_DEBOUNCE_MS);
}

function setFilePreview(nextPreview: boolean) {
  const cur = openFile();
  if (!cur || !isMarkdownPath(cur.path)) return;
  openFile.set({
    ...cur,
    preview: nextPreview,
    previewSource: nextPreview ? cur.content : cur.previewSource,
  });
}

function jumpEditorToLine(line1Based: number) {
  if (!activeEditorApi || line1Based <= 0) return;
  const zeroBased = Math.max(0, Math.floor(line1Based) - 1);
  try {
    activeEditorApi.goToLine(zeroBased);
  } catch {
    // 编辑器行数尚未挂载时忽略。
  }
}

function viewFileOn(machine: string | null, path: string, line: number | null = null) {
  const cur = openFile();
  if (cur && isDirty(cur)) {
    if (cur.machine === machine && cur.path === path) {
      if (line != null) {
        if (cur.preview) {
          setFilePreview(false);
        }
        jumpEditorToLine(line);
      }
      return;
    }
    const curOps = styleOpsFor(cur.machine);
    const targetOps = styleOpsFor(machine);
    unsavedDialog.set({
      currentId: cur.id,
      currentName: curOps.fileName(cur.path) ?? cur.path,
      target: { machine, path },
      targetName: targetOps.fileName(path) ?? path,
      line,
    });
    return;
  }
  openFileNow(machine, path, line);
}

/**
 * 真正打开。项目外的文件默认只读。缓存里有就先秒开，再带版本问运行时变没变；没有就显示加载中。
 * `gotoLine` 从 1 数：带行号（`#L10`、搜索命中）直接落到源码行，不进预览。
 */
function openFileNow(machine: string | null, path: string, gotoLine: number | null = null) {
  if (machine === treeMachine()) {
    revealInTree(path);
  }
  const id = nextFileId++;
  const defaultPreview = isMarkdownPath(path) && gotoLine == null;
  if (previewTimer) {
    clearTimeout(previewTimer);
    previewTimer = null;
  }
  pendingSwitchAfterSave = null;
  unsavedDialog.set(null);

  const cached = fileCache.get(machine, path);
  const hadCached = cached != null;
  openFile.set({
    id,
    machine,
    path,
    status: hadCached ? { kind: "ready" } : { kind: "loading" },
    unlocked: false,
    content: cached?.kind === "text" ? cached.text : "",
    savedContent: cached?.kind === "text" ? cached.text : "",
    savedVersion: cached?.version ?? null,
    imageDataUri: cached?.kind === "image" ? cached.dataUri : null,
    saveError: null,
    readable: cached?.kind === "text",
    conflictPending: false,
    reloadInflight: false,
    reloadAgain: false,
    preview: defaultPreview,
    previewSource: cached?.kind === "text" ? cached.text : "",
    pendingGotoLine: gotoLine,
  });
  persistCurrentProject();

  const unreadable = (message: string, canRetry = false) => {
    fileCache.remove(machine, path);
    const found = findOpenFileById(id);
    // 从缓存秒开后已经改了：不换掉编辑器，改动留着、仍算未保存，由用户决定存回去还是放弃。
    if (found && isDirty(found.file)) {
      patchOpenFileById(id, (of) => ({ ...of, saveError: message }));
      return;
    }
    patchOpenFileById(id, (of) => ({
      ...of,
      status: { kind: "unreadable", message, canRetry },
      imageDataUri: null,
      readable: false,
    }));
  };

  void callDetail(
    machine,
    "file.open",
    { path, if_none_match: cached?.version ?? undefined },
    FILE_OPEN_TIMEOUT_MS,
  ).then((outcome) => {
    if (!findOpenFileById(id)) return;
    if (!outcome.ok) {
      const message = t("unreadable.error", { error: outcome.message });
      // 运行时说读不了（没有这个文件、没权限）：缓存作废。连不上：有缓存就接着看缓存。
      if (outcome.code != null) {
        unreadable(message, true);
      } else if (!hadCached) {
        patchOpenFileById(id, (of) => ({
          ...of,
          status: { kind: "unreadable", message, canRetry: true },
          readable: false,
        }));
      }
      return;
    }
    const content = outcome.value?.content;
    if (!content || typeof content !== "object") {
      unreadable(t("unreadable.error", { error: "invalid response" }), true);
      return;
    }
    if (content.kind === "not_modified") return;
    if (content.kind === "text") {
      const body = String(content.text ?? "");
      const version = typeof content.version === "string" ? content.version : null;
      if (version != null) fileCache.put(machine, path, { kind: "text", text: body, version });
      const found = findOpenFileById(id);
      if (!found) return;
      if (hadCached && isDirty(found.file)) {
        patchOpenFileById(id, (of) => ({ ...of, saveError: t("save.changed_elsewhere") }));
        return;
      }
      if (hadCached && found.file.savedVersion === version && found.file.readable) return;
      patchOpenFileById(id, (of) => ({
        ...of,
        status: { kind: "ready" },
        content: body,
        savedContent: body,
        savedVersion: version,
        previewSource: body,
        imageDataUri: null,
        readable: true,
        saveError: null,
      }));
      const cur = openFile();
      if (!hadCached && cur && cur.id === id && gotoLine != null) {
        queueMicrotask(() => {
          if (activeEditorFileId === id) {
            jumpEditorToLine(gotoLine);
            patchOpenFileById(id, (of) => ({ ...of, pendingGotoLine: null }));
          }
        });
      }
      return;
    }
    if (content.kind === "image") {
      // 格式由运行时按内容认（`mime`），不按扩展名猜；旧运行时不带就当预览不了。
      const mime = typeof content.mime === "string" ? content.mime : "";
      const b64 = typeof content.data === "string" ? content.data : "";
      if (!mime || !b64) {
        unreadable(t("unreadable.image"));
        return;
      }
      const dataUri = `data:${mime};base64,${b64}`;
      const version = typeof content.version === "string" ? content.version : null;
      fileCache.put(machine, path, { kind: "image", dataUri, version });
      patchOpenFileById(id, (of) => ({
        ...of,
        status: { kind: "ready" },
        imageDataUri: dataUri,
        savedVersion: version,
        readable: false,
      }));
      return;
    }
    if (content.kind === "too_large") {
      const sizeMb = (Number(content.size ?? 0) / (1024 * 1024)).toFixed(1);
      const limitMb = Math.floor(Number(content.limit ?? 0) / (1024 * 1024));
      unreadable(t("unreadable.too_large", { size: sizeMb, limit: limitMb }));
      return;
    }
    unreadable(t("unreadable.binary"));
  });
}

/** 项目外的文件点「解锁编辑」：能改能存，切回源码并把焦点给编辑器。 */
function unlockOpenFile() {
  const cur = openFile();
  if (!cur || !isReadOnly(cur)) return;
  const wasPreview = cur.preview;
  openFile.set({ ...cur, unlocked: true, saveError: null, preview: false });
  // 从预览切回时编辑器要等下一次挂上，在 editorRef 里再给焦点。
  if (wasPreview || !activeEditorApi) {
    focusEditorOnMount = true;
  } else {
    activeEditorApi.focus?.();
  }
}

/** 读失败点重试：不看缓存，重新向运行时拉。 */
function retryOpenFile() {
  const cur = openFile();
  if (!cur) return;
  fileCache.remove(cur.machine, cur.path);
  openFileNow(cur.machine, cur.path, null);
}

async function saveOpenFile() {
  const cur = openFile();
  if (!cur) return;
  // 图片和还在读的文件没有可存的文本。
  if (cur.imageDataUri != null || cur.status.kind === "loading") return;
  if (isReadOnly(cur)) {
    openFile.set({ ...cur, saveError: t("save.read_only") });
    pendingSwitchAfterSave = null;
    return;
  }
  if (!cur.readable) {
    openFile.set({ ...cur, saveError: t("save.unreadable") });
    pendingSwitchAfterSave = null;
    return;
  }
  const id = cur.id;
  const content = cur.content;
  const baseVersion = cur.conflictPending ? null : cur.savedVersion;

  const outcome = await callDetail<{ version: string }>(cur.machine, "file.save", {
    path: cur.path,
    text: content,
    base_version: baseVersion,
  });

  const switchTarget =
    pendingSwitchAfterSave && pendingSwitchAfterSave.fromId === id
      ? pendingSwitchAfterSave
      : null;
  if (switchTarget) pendingSwitchAfterSave = null;

  if (!findOpenFileById(id)) return;

  if (outcome.ok) {
    const version =
      typeof outcome.value?.version === "string" ? outcome.value.version : cur.savedVersion;
    if (version != null) {
      fileCache.put(cur.machine, cur.path, { kind: "text", text: content, version });
    }
    patchOpenFileById(id, (of) => ({
      ...of,
      savedContent: content,
      savedVersion: version,
      saveError: null,
      conflictPending: false,
    }));
    if (switchTarget && openFile()?.id === id) {
      openFileNow(switchTarget.target.machine, switchTarget.target.path, switchTarget.line);
    }
  } else if (outcome.code === "conflict") {
    patchOpenFileById(id, (of) => ({
      ...of,
      conflictPending: true,
      saveError: t("save.conflict"),
    }));
  } else {
    patchOpenFileById(id, (of) => ({
      ...of,
      saveError: t("save.failed", { error: outcome.message }),
    }));
  }
}

function onOpenFileProjectChanged(machine: string | null, root: string) {
  const cur = openFile();
  if (!cur || cur.machine !== machine || !cur.readable) return;
  const ops = styleOpsFor(machine);
  if (!ops.startsWith(cur.path, root)) return;
  if (cur.reloadInflight) {
    openFile.set({ ...cur, reloadAgain: true });
    return;
  }
  reloadOpenFile();
}

function reloadOpenFile() {
  const cur = openFile();
  if (!cur || !cur.readable || cur.status.kind !== "ready") return;
  const id = cur.id;
  const machine = cur.machine;
  const path = cur.path;
  openFile.set({ ...cur, reloadInflight: true, reloadAgain: false });

  void call(
    machine,
    "file.open",
    { path, if_none_match: cur.savedVersion ?? undefined },
    FILE_OPEN_TIMEOUT_MS,
  )
    .then((result: any) => {
      const found = findOpenFileById(id);
      if (!found) return;
      const again = found.file.reloadAgain;
      const content = result?.content;
      if (content?.kind === "text" && typeof content.version === "string") {
        fileCache.put(machine, path, {
          kind: "text",
          text: String(content.text ?? ""),
          version: content.version,
        });
      }
      patchOpenFileById(id, (of) => {
        const next: OpenFileState = { ...of, reloadInflight: false, reloadAgain: false };
        if (
          content?.kind === "text" &&
          typeof content.version === "string" &&
          of.savedVersion !== content.version
        ) {
          if (isDirty(of)) {
            next.saveError = t("save.changed_elsewhere");
          } else {
            const textBody = String(content.text ?? "");
            next.content = textBody;
            next.savedContent = textBody;
            next.savedVersion = content.version;
            next.previewSource = textBody;
          }
        }
        return next;
      });
      if (again && openFile()?.id === id) {
        reloadOpenFile();
      }
    })
    .catch(() => {
      patchOpenFileById(id, (of) => ({ ...of, reloadInflight: false, reloadAgain: false }));
    });
}

function openFileInBrowser() {
  const cur = openFile();
  if (!cur || cur.machine != null) return;
  const url = pathToFileUrl(cur.path);
  if (!url) {
    openFile.set({ ...cur, saveError: t("browser.bad_path") });
    return;
  }
  void openUrl(url).catch((error: any) => {
    const latest = openFile();
    if (latest && latest.id === cur.id) {
      openFile.set({ ...latest, saveError: error?.message ?? String(error) });
    }
  });
}

// ---- 发送到会话 / 删除 ----

function sendToSession(message: string) {
  void run(SEND_TEXT_CMD, { text: message }).catch((error: any) => {
    notifyError(t("send.failed", { error: error?.message ?? String(error) }));
  });
}

function sendPathToSession(path: string) {
  const machine = treeMachine();
  const ops = styleOpsFor(machine);
  const root = treeRootFor(machine, path);
  const rel = root ? ops.stripPrefix(path, root) : null;
  const display = rel && rel.length > 0 ? rel : path;
  sendToSession(`@${display} `);
}

function sendEditorSelectionToSession() {
  const cur = openFile();
  if (!cur || !activeEditorApi) return;
  const selected = String(activeEditorApi.selectedText?.() ?? "");
  if (!selected.trim()) return;
  const proj = currentProject();
  const ops = styleOpsFor(cur.machine);
  const rel =
    proj && proj.machine === cur.machine ? (ops.stripPrefix(cur.path, proj.root) ?? cur.path) : cur.path;
  sendToSession(t("send.selection", { path: rel, selected }));
}

function startDelete(path: string, isDir: boolean) {
  const ops = styleOpsFor(treeMachine());
  const name = ops.fileName(path) ?? path;
  deleteDialog.set({ path, name, isDir });
}

async function confirmDelete() {
  const dlg = deleteDialog();
  if (!dlg) return;
  deleteDialog.set(null);
  const machine = treeMachine();
  const root = treeRootFor(machine, dlg.path);
  // 项目根本身不从文件树里删。
  if (!root || dlg.path === root) return;
  const ops = styleOpsFor(machine);
  try {
    await call(machine, "fs.delete", { path: dlg.path, trash: true });
    fileCache.removeUnder(machine, dlg.path, (path, base) => ops.startsWith(path, base));
    // 清掉已删除路径对应的打开文件、选中和展开缓存。
    const cur = openFile();
    if (cur && cur.machine === machine && ops.startsWith(cur.path, dlg.path)) {
      openFile.set(null);
    }
    if (selectedPath() && ops.startsWith(selectedPath()!, dlg.path)) {
      selectedPath.set(null);
    }
    const nextExpanded = new Set<string>();
    for (const dir of expandedDirs()) {
      if (!ops.startsWith(dir, dlg.path)) nextExpanded.add(dir);
    }
    expandedDirs.set(nextExpanded);
    markDirsStaleWhere((m, dir) => m === machine && ops.startsWith(dir, root));
    markGitMarksStaleWhere((m, r) => m === machine && r === root);
    persistCurrentProject();
    refreshVisible();
  } catch (error: any) {
    notifyError(t("delete.failed", { error: error?.message ?? String(error) }));
  }
}

// ---- 搜索 ----

function scheduleSearch(query: string) {
  if (searchTimer) {
    clearTimeout(searchTimer);
    searchTimer = null;
  }
  const trimmed = query.trim();
  const proj = currentProject();
  if (!trimmed || !proj) {
    searchSeq += 1;
    searchState.set(null);
    return;
  }
  const seq = ++searchSeq;
  searchState.set({
    machine: proj.machine,
    root: proj.root,
    query: trimmed,
    done: false,
    hits: [],
    truncated: false,
  });
  searchTimer = setTimeout(() => {
    searchTimer = null;
    void call(proj.machine, "project.search", { root: proj.root, query: trimmed })
      .then((result: any) => {
        if (seq !== searchSeq) return;
        const hits: SearchHit[] = Array.isArray(result?.hits)
          ? result.hits.map((h: any) => ({
              path: String(h.path ?? ""),
              rel: String(h.rel ?? ""),
              line: Array.isArray(h.line) ? [Number(h.line[0]), String(h.line[1] ?? "")] : null,
            }))
          : [];
        searchState.set({
          machine: proj.machine,
          root: proj.root,
          query: trimmed,
          done: true,
          hits,
          truncated: Boolean(result?.truncated),
        });
      })
      .catch(() => {
        if (seq !== searchSeq) return;
        searchState.set({
          machine: proj.machine,
          root: proj.root,
          query: trimmed,
          done: true,
          hits: [],
          truncated: false,
        });
      });
  }, SEARCH_DEBOUNCE_MS);
}

// ---- 切换项目（停放 / 恢复） ----

async function syncProjectFromContext() {
  const ctxProj = activeContext()?.project;
  const next: ProjectRef | null =
    ctxProj && typeof ctxProj.root === "string" && ctxProj.root.length > 0
      ? { machine: ctxProj.machine ?? null, root: ctxProj.root }
      : null;
  const cur = currentProject();
  if (cur?.machine === next?.machine && cur?.root === next?.root) return;

  // 停放当前项目 UI 现场
  if (cur) {
    parked.set(projectKey(cur.machine, cur.root), {
      openFile: openFile(),
      selected: selectedPath(),
      expanded: new Set(expandedDirs()),
      collapsedRoots: new Set(collapsedRoots()),
      pendingReveal,
    });
  }

  const switchId = ++projectSwitchSeq;
  currentProject.set(next);
  filterQuery.set("");
  searchState.set(null);
  unsavedDialog.set(null);
  pendingSwitchAfterSave = null;
  pendingReveal = null;

  if (!next) {
    openFile.set(null);
    selectedPath.set(null);
    expandedDirs.set(new Set());
    collapsedRoots.set(new Set());
    refreshVisible();
    return;
  }

  const key = projectKey(next.machine, next.root);
  const restored = parked.get(key);
  if (restored) {
    parked.delete(key);
    openFile.set(restored.openFile);
    selectedPath.set(restored.selected);
    expandedDirs.set(restored.expanded);
    collapsedRoots.set(restored.collapsedRoots);
    pendingReveal = restored.pendingReveal;
    refreshVisible();
    if (restored.openFile?.readable) {
      reloadOpenFile();
    }
    return;
  }

  // 从 SQLite 恢复该项目现场
  openFile.set(null);
  selectedPath.set(null);
  expandedDirs.set(new Set());
  collapsedRoots.set(new Set());
  const saved = await filesStore.loadProject(next.machine, next.root);
  if (switchId !== projectSwitchSeq) return;
  selectedPath.set(saved.selected);
  expandedDirs.set(saved.expanded);
  collapsedRoots.set(saved.collapsedRoots);
  if (saved.openFile) {
    openFileNow(next.machine, saved.openFile, null);
  }
  refreshVisible();
}

// ---- 树键盘导航 ----

function handleTreeKeyDown(e: any) {
  const key = e?.key;
  if (
    key !== "ArrowUp" &&
    key !== "ArrowDown" &&
    key !== "ArrowLeft" &&
    key !== "ArrowRight" &&
    key !== "Enter"
  ) {
    return;
  }
  const rows = currentTreeRows();
  if (rows.length === 0) return;
  const curSel = selectedPath();
  const curIx = curSel != null ? rows.findIndex((r) => fileTreeRowPath(r) === curSel) : -1;

  if (key === "ArrowUp") {
    let nextIx = curIx > 0 ? curIx - 1 : rows.length - 1;
    while (nextIx >= 0 && fileTreeRowPath(rows[nextIx]) == null) nextIx -= 1;
    if (nextIx >= 0) selectedPath.set(fileTreeRowPath(rows[nextIx]));
    return;
  }
  if (key === "ArrowDown") {
    let nextIx = curIx >= 0 && curIx + 1 < rows.length ? curIx + 1 : 0;
    while (nextIx < rows.length && fileTreeRowPath(rows[nextIx]) == null) nextIx += 1;
    if (nextIx < rows.length) selectedPath.set(fileTreeRowPath(rows[nextIx]));
    return;
  }
  if (curIx < 0) return;
  const row = rows[curIx];
  const path = fileTreeRowPath(row);
  if (!path) return;

  if (key === "ArrowLeft") {
    if (row.kind === "root_header" && row.open) {
      toggleRootCollapsed(path);
    } else if (row.kind === "entry" && row.isDir && row.expanded) {
      toggleExpandDir(path);
    } else {
      // 向上找父级行（depth 更小）
      const depth = row.kind === "entry" ? row.depth : 0;
      for (let i = curIx - 1; i >= 0; i -= 1) {
        const candidate = rows[i];
        const cDepth = candidate.kind === "root_header" ? 0 : candidate.kind === "entry" ? candidate.depth : depth;
        if (cDepth < depth && fileTreeRowPath(candidate) != null) {
          selectedPath.set(fileTreeRowPath(candidate));
          break;
        }
      }
    }
    return;
  }
  if (key === "ArrowRight") {
    if (row.kind === "root_header") {
      if (!row.open) toggleRootCollapsed(path);
      else if (curIx + 1 < rows.length && fileTreeRowPath(rows[curIx + 1])) {
        selectedPath.set(fileTreeRowPath(rows[curIx + 1]));
      }
    } else if (row.kind === "entry" && row.isDir) {
      if (!row.expanded) toggleExpandDir(path);
      else if (curIx + 1 < rows.length && fileTreeRowPath(rows[curIx + 1])) {
        selectedPath.set(fileTreeRowPath(rows[curIx + 1]));
      }
    } else if (row.kind === "entry") {
      viewFileOn(treeMachine(), path, null);
    }
    return;
  }
  if (key === "Enter") {
    if (row.kind === "root_header") {
      toggleRootCollapsed(path);
    } else if (row.kind === "entry" && row.isDir) {
      toggleExpandDir(path);
    } else if (row.kind === "entry") {
      viewFileOn(treeMachine(), path, null);
    }
  }
}

// ---- 启动注册与事件监听 ----

bootTask(async () => {
  await initHost(["window_active", "active_context", "machines_meta"]);
  await registerViews([
    {
      name: "main",
      title: text("title"),
      icon: "icons/folder-tree.svg",
      export: "default",
    },
  ]);
  await registerCommands(
    [
      {
        name: "save",
        title: text("command.save"),
        key: "cmd-s",
        context: "Workspace",
        when_view_shown: VIEW_ID,
        palette: true,
        requires_args: false,
        args: [],
      },
      {
        name: "open",
        title: text("command.open"),
        context: "Workspace",
        palette: false,
        requires_args: true,
        args: ["path", "machine", "line"],
      },
    ],
    text("title"),
  );
  await registerFileOpener("open");

  if (!settingsLoaded) {
    settingsLoaded = true;
    const settings = await filesStore.loadSettings();
    treeWidth.set(clampTreeWidth(settings.treeWidth));
    treeOpen.set(settings.treeOpen);
    pinnedRoots.set(settings.pinned);
  }
  await syncProjectFromContext();
  refreshVisible();
});

onCommand(async ({ name, args }) => {
  if (name === "save") {
    await saveOpenFile();
    return null;
  }
  if (name === "open") {
    const filePath = typeof args?.path === "string" && args.path.length > 0 ? args.path : null;
    if (!filePath) {
      throw new Error(t("error.bad_open_args", { args: JSON.stringify(args ?? null) }));
    }
    const machine = typeof args?.machine === "string" ? args.machine : null;
    const line =
      typeof args?.line === "number" && Number.isFinite(args.line) && args.line > 0
        ? Math.floor(args.line)
        : null;
    await run(OPEN_VIEW_CMD, { view: VIEW_ID, args: null });
    viewFileOn(machine, filePath, line);
    return null;
  }
  throw new Error(`unknown command: ${name}`);
});

onSlices(["window_active", "active_context"], () => {
  lastWindowActive = windowActive();
  void syncProjectFromContext();
});

onEvent(
  (event) => {
    if (!event || typeof event !== "object") return;
    if (event.type === "project_changed") {
      const machine = event.machine ?? null;
      const root = typeof event.root === "string" ? event.root : "";
      if (!root || !watches.has(projectKey(machine, root))) return;
      const ops = styleOpsFor(machine);
      markDirsStaleWhere((m, dir) => m === machine && ops.startsWith(dir, root));
      onOpenFileProjectChanged(machine, root);
      markGitMarksStaleWhere((m, r) => m === machine && ops.startsWith(r, root));
      refreshVisible();
      return;
    }
    if (event.type === "reconnected") {
      const machine = event.machine ?? null;
      markEverythingStale(machine);
      for (const key of [...watches]) {
        const sep = key.indexOf("\0");
        const m = sep > 0 ? key.slice(0, sep) : null;
        if (m === machine) watches.delete(key);
      }
      refreshVisible();
      return;
    }
    if (event.type === "window_activated") {
      markEverythingStale(undefined);
      refreshVisible();
      return;
    }
    if (event.type === "active_context_changed") {
      void syncProjectFromContext();
      return;
    }
    if (event.type === "shown_views_changed") {
      if (viewShown(VIEW_ID)) {
        refreshVisible();
      }
    }
  },
  [
    "project_changed",
    "reconnected",
    "window_activated",
    "active_context_changed",
    "shown_views_changed",
  ],
);

// ---- 子组件 ----


function MenuRow(props: {
  label: string;
  iconPath?: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  const look = currentTheme;
  return (
    <MenuItem
      closeOnSelect={true}
      disabled={props.disabled}
      class={MENU_ITEM_CLASS}
      style={{ height: 28, gap: 8 }}
      onSelect={props.onSelect}
    >
      <If when={Boolean(props.iconPath)}>
        <SmeltIcon
          path={props.iconPath}
          size={14}
          color={props.danger ? look().palette.bad : look().palette.muted}
        />
      </If>
      <Text
        style={{
          fontSize: look().type.ui_compact.size,
          lineHeight: look().type.ui_compact.line_height,
          color: props.danger ? look().palette.bad : look().palette.text,
        }}
      >
        {props.label}
      </Text>
    </MenuItem>
  );
}

function gitMarkColor(mark: string, palette: any): string {
  if (mark === "D") return palette.bad;
  if (mark === "A") return palette.ok;
  if (mark === "U") return palette.ok;
  return palette.need;
}

function Placeholder(props: { text: string }) {
  const look = currentTheme;
  return (
    <View
      style={{
        flex: 1,
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        paddingLeft: 16,
        paddingRight: 16,
      }}
    >
      <Text
        style={{
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.muted,
          textAlign: "center",
        }}
      >
        {props.text}
      </Text>
    </View>
  );
}

/** 读不出来：居中说明，传输或运行时出错时给重试。 */
function UnreadableBody() {
  const look = currentTheme;
  const status = computed(() => {
    const st = openFile()?.status;
    return st?.kind === "unreadable" ? st : null;
  });
  return (
    <View
      style={{
        flex: 1,
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 12,
        paddingLeft: 16,
        paddingRight: 16,
      }}
    >
      <Text
        style={{
          maxWidth: 420,
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.muted,
          textAlign: "center",
        }}
      >
        {status()?.message ?? ""}
      </Text>
      <If when={Boolean(status()?.canRetry)}>
        <SmeltButton small={true} label={t("retry")} onPress={retryOpenFile} />
      </If>
    </View>
  );
}

// ---- 主视图 ----

export function Main() {
  const look = currentTheme;
  const isMac = localOs() === "darwin";

  const rows = computed(() => currentTreeRows());
  const roots = computed(() => currentRoots());
  const singleRootLoading = computed(() => {
    cacheVersion();
    const r = roots();
    return r.length === 1 && !dirCache.get(treeMachine() ?? "")?.has(r[0]);
  });
  const isLocalTree = computed(() => treeMachine() == null);
  const hasQuery = computed(() => filterQuery().trim().length > 0);

  const searchRows = computed(() => {
    const st = searchState();
    if (!st) return [];
    const tree = buildSearchTree(st.hits);
    return flattenSearchTree(tree, st.hits);
  });

  const searchStatusText = computed(() => {
    const st = searchState();
    if (!st) return "";
    if (!st.done) return t("search.running");
    if (st.hits.length === 0) return t("search.none");
    if (st.truncated) return t("search.hits_truncated", { count: st.hits.length });
    return t("search.hits", { count: st.hits.length });
  });

  const breadcrumbs = computed(() => {
    const of = openFile();
    if (!of) return [];
    const r = of.machine === treeMachine() ? roots() : [];
    const segs = breadcrumbSegments(styleOpsFor(of.machine), of.path, r);
    const lastIx = segs.length - 1;
    return segs.map((seg, ix) => ({ ...seg, isLast: ix === lastIx }));
  });

  // 项目外的文件、或者只剩一段（看不出在哪）时，最后一段悬停给出完整路径。
  const showFullPathTip = computed(() => {
    const of = openFile();
    return of != null && (!isInsideProject(of.machine, of.path) || breadcrumbs().length === 1);
  });

  const openFileReady = computed(() => openFile()?.status.kind === "ready");

  const canPreviewMarkdown = computed(() => {
    const of = openFile();
    return of != null && openFileReady() && isMarkdownPath(of.path);
  });

  const canOpenInBrowser = computed(() => {
    const of = openFile();
    return of != null && openFileReady() && of.machine == null && isHtmlPath(of.path);
  });

  const canUnlock = computed(() => {
    const of = openFile();
    return of != null && openFileReady() && of.readable && isReadOnly(of);
  });

  // 预览里的相对链接按文件所在目录、在文件那台机器上解析。
  const previewCwd = computed(() => {
    const of = openFile();
    return of ? pathParent(pathStyle(of.machine), of.path) : null;
  });

  const showingMarkdownPreview = computed(() => {
    const of = openFile();
    return of != null && of.preview && isMarkdownPath(of.path);
  });

  const openFileDirty = computed(() => isDirty(openFile()));

  const isShown = computed(() => viewShown(VIEW_ID));
  effect(() => {
    if (isShown()) {
      refreshVisible();
    }
  });

  function handleEditorContextMenu(e: { x: number; y: number }) {
    const api = activeEditorApi;
    const hasSelection = Boolean(api && String(api.selectedText?.() ?? "").trim().length > 0);
    const cur = openFile();
    const readOnly = !cur || !cur.readable || isReadOnly(cur);
    void Menu.popup(
      [
        {
          label: t("edit.cut"),
          disabled: readOnly || !hasSelection,
          onSelect: () => api?.run?.("cut"),
        },
        {
          label: t("edit.copy"),
          disabled: !hasSelection,
          onSelect: () => api?.run?.("copy"),
        },
        {
          label: t("edit.paste"),
          disabled: readOnly,
          onSelect: () => api?.run?.("paste"),
        },
        {
          label: t("edit.select_all"),
          onSelect: () => api?.run?.("selectAll"),
        },
        { type: "separator" },
        {
          label: t("send.selection_menu"),
          disabled: !hasSelection,
          onSelect: () => sendEditorSelectionToSession(),
        },
      ],
      { x: e.x, y: e.y },
    );
  }

  return (
    <View
      style={{
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        flexDirection: "column",
        overflow: "hidden",
      }}
    >
      {/* 路径面包屑栏（仅在打开文件时显示） */}
      <If when={openFile() != null}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 8,
            paddingLeft: 12,
            paddingRight: 12,
            paddingTop: 6,
            paddingBottom: 6,
            borderBottomWidth: 1,
            borderBottomColor: look().palette.hair,
            flexShrink: 0,
          }}
        >
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              gap: 4,
              minWidth: 0,
              flex: 1,
              overflow: "hidden",
            }}
          >
            <Each of={breadcrumbs()} key={(seg: any) => seg.path}>
              {(seg: () => { label: string; path: string; isLast: boolean }) => {
                const isLast = () => seg().isLast;
                const onTreeMachine = () => openFile()?.machine === treeMachine();
                return (
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 4,
                      flexShrink: isLast() ? 1 : 0,
                      minWidth: 0,
                    }}
                  >
                    <View
                      class="rounded hover:bg-accent"
                      onPress={() => {
                        if (!onTreeMachine()) return;
                        revealFromBreadcrumb(seg().path, !isLast());
                      }}
                      tooltip={
                        isLast() && showFullPathTip() ? (
                          <SmeltTooltip>
                            <Text
                              style={{
                                fontSize: look().type.text_xsmall.size,
                                color: look().palette.text,
                              }}
                            >
                              {openFile()?.path ?? ""}
                            </Text>
                          </SmeltTooltip>
                        ) : undefined
                      }
                      style={{
                        paddingLeft: 6,
                        paddingRight: 6,
                        paddingTop: 4,
                        paddingBottom: 4,
                        minHeight: 26,
                        display: "flex",
                        alignItems: "center",
                        cursor: onTreeMachine() ? "pointer" : "default",
                      }}
                    >
                      <Text
                        style={{
                          fontSize: look().type.ui.size,
                          lineHeight: look().type.ui.line_height,
                          fontWeight: isLast()
                            ? look().type.ui_medium.weight
                            : look().type.ui.weight,
                          color: isLast() ? look().palette.text : look().palette.muted,
                          numberOfLines: 1,
                        }}
                      >
                        {seg().label}
                      </Text>
                    </View>
                    <If when={!isLast()}>
                      <Text
                        style={{
                          fontSize: look().type.ui.size,
                          color: look().palette.muted,
                        }}
                      >
                        {">"}
                      </Text>
                    </If>
                  </View>
                );
              }}
            </Each>
            <If when={openFileDirty()}>
              <View
                style={{
                  width: 6,
                  height: 6,
                  borderRadius: 3,
                  backgroundColor: look().palette.need,
                  flexShrink: 0,
                  marginLeft: 4,
                }}
              />
            </If>
            <If when={Boolean(openFile()?.saveError)}>
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                  color: look().palette.bad,
                  marginLeft: 6,
                  flexShrink: 1,
                  numberOfLines: 1,
                }}
              >
                {openFile()?.saveError ?? ""}
              </Text>
            </If>
          </View>

          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4, flexShrink: 0 }}>
            <If when={canOpenInBrowser()}>
              <SmeltButton
                quiet={true}
                small={true}
                label={t("browser.open")}
                onPress={openFileInBrowser}
              />
            </If>
            <If when={canUnlock()}>
              <SmeltButton
                quiet={true}
                small={true}
                icon="icons/pencil.svg"
                label={t("readonly.edit")}
                tooltip={t("readonly.tooltip")}
                onPress={unlockOpenFile}
              />
            </If>
            <If when={canPreviewMarkdown()}>
              <SmeltButton
                quiet={true}
                small={true}
                label={showingMarkdownPreview() ? t("preview.show_source") : t("preview.show_preview")}
                onPress={() => setFilePreview(!showingMarkdownPreview())}
              />
            </If>
            <SmeltButton
              quiet={true}
              small={true}
              icon={treeOpen() ? "icons/panel-right.svg" : "icons/panel-left.svg"}
              tooltip={treeOpen() ? t("tree.collapse") : t("tree.expand")}
              onPress={() => {
                treeOpen.set(!treeOpen());
                persistTreeLayout();
                refreshVisible();
              }}
            />
          </View>
        </View>
      </If>

      {/* 主体：左内容 + 右文件树（Rust 一等 ResizablePanelGroup，拖动零 JS 往返） */}
      <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "row" }}>
        <ResizablePanelGroup
          axis="horizontal"
          handleClass="bg-border group-hover:bg-muted-foreground/50 group-active:bg-muted-foreground/70"
        >
          {/* 左侧：文件主体（编辑器 / Markdown 预览 / 图片 / 占位） */}
          <ResizablePanel minSize={120}>
            <View
              style={{
                flex: 1,
                width: "100%",
                height: "100%",
                minWidth: 0,
                minHeight: 0,
                display: "flex",
                flexDirection: "column",
                overflow: "hidden",
              }}
            >
              <If when={openFile() != null} else={<Placeholder text={t("title")} />}>
                <If when={openFile()?.status.kind !== "loading"} else={<Placeholder text={t("loading")} />}>
                  <If when={openFile()?.status.kind !== "unreadable"} else={<UnreadableBody />}>
                    <If
                      when={Boolean(openFile()?.imageDataUri)}
                      else={
                        <If
                          when={showingMarkdownPreview()}
                          else={
                            <SmeltEditor
                              editorRef={(api: any) => {
                                activeEditorApi = api;
                                const cur = openFile();
                                activeEditorFileId = cur?.id ?? 0;
                                if (api && focusEditorOnMount) {
                                  focusEditorOnMount = false;
                                  api.focus?.();
                                }
                                if (api && cur?.pendingGotoLine != null) {
                                  jumpEditorToLine(cur.pendingGotoLine);
                                  patchOpenFileById(cur.id, (of) => ({ ...of, pendingGotoLine: null }));
                                }
                              }}
                              value={openFile()?.content ?? ""}
                              language={openFile() ? editorLanguageForPath(openFile()!.path) : "text"}
                              readOnly={!openFile()?.readable || isReadOnly(openFile()!)}
                              lineNumbers={true}
                              softWrap={false}
                              accessibilityLabel={openFile()?.path ?? ""}
                              onChange={({ value }: { value: string }) => {
                                const cur = openFile();
                                if (!cur || !cur.readable || isReadOnly(cur)) return;
                                if (cur.content === value) return;
                                // 改过就算解锁：之后移走根目录也不会把没存的改动锁住。
                                openFile.set({ ...cur, content: value, unlocked: true });
                                schedulePreviewRefresh();
                              }}
                              onContextMenu={handleEditorContextMenu}
                            />
                          }
                        >
                          <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
                            <View style={{ padding: 14, width: "100%", minWidth: 0 }}>
                              <SmeltMarkdown
                                markdown={openFile()?.previewSource ?? ""}
                                cwd={previewCwd()}
                                machine={openFile()?.machine ?? null}
                              />
                            </View>
                          </Scroll>
                        </If>
                      }
                    >
                      <View
                        style={{
                          flex: 1,
                          width: "100%",
                          height: "100%",
                          padding: 16,
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "center",
                          overflow: "hidden",
                        }}
                      >
                        <Image
                          src={openFile()?.imageDataUri ?? ""}
                          style={{ width: "100%", height: "100%", objectFit: "contain" }}
                        />
                      </View>
                    </If>
                  </If>
                </If>
              </If>
            </View>
          </ResizablePanel>

          {/* 右侧：搜索栏 + 文件树 / 搜索结果 */}
          <ResizablePanel
            fixed={true}
            visible={treeOpen()}
            size={treeWidth()}
            minSize={MIN_TREE_WIDTH}
            maxSize={MAX_TREE_WIDTH}
            onResize={({ size }: { size: number }) => {
              const clamped = clampTreeWidth(size);
              treeWidth.set(clamped);
              persistTreeLayout();
            }}
          >
            <View
              style={{
                flex: 1,
                width: "100%",
                height: "100%",
                minWidth: 0,
                minHeight: 0,
                display: "flex",
                flexDirection: "column",
                overflow: "hidden",
              }}
            >
              {/* 搜索框 + 钉住目录按钮 */}
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 4,
                  paddingLeft: 8,
                  paddingRight: 8,
                  paddingTop: 6,
                  paddingBottom: 6,
                  borderBottomWidth: 1,
                  borderBottomColor: look().palette.hair,
                  flexShrink: 0,
                }}
              >
                <Input
                  value={filterQuery()}
                  placeholder={t("search.placeholder")}
                  accessibilityLabel={t("search.placeholder")}
                  onChange={({ value }: { value: string }) => {
                    filterQuery.set(value);
                    scheduleSearch(value);
                  }}
                  onKeyPress={(e: any) => {
                    if (e?.key === "Escape") {
                      filterQuery.set("");
                      scheduleSearch("");
                    }
                  }}
                  class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
                  style={{
                    flex: 1,
                    minWidth: 0,
                    height: 26,
                    fontSize: look().type.small.size,
                    lineHeight: look().type.small.line_height,
                  }}
                />
                <If when={isLocalTree()}>
                  <SmeltButton
                    quiet={true}
                    small={true}
                    icon="icons/plus.svg"
                    tooltip={t("pin.add")}
                    onPress={() => void pickPinnedRoots()}
                  />
                </If>
              </View>

              {/* 搜索结果 OR 文件树 */}
              <If
                when={hasQuery()}
                else={
                  <If
                    when={roots().length > 0}
                    else={<Placeholder text={t("tree.no_project")} />}
                  >
                    <If
                      when={!singleRootLoading()}
                      else={<Placeholder text={t("loading")} />}
                    >
                      <View
                        focusable={true}
                        onKeyDown={handleTreeKeyDown}
                        style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}
                      >
                        <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
                          <View style={{ display: "flex", flexDirection: "column", paddingTop: 2, paddingBottom: 4 }}>
                            <Each of={rows()} key={(row: any) => fileTreeRowId(row)}>
                              {(row: () => any) => {
                                const isLoading = () => row().kind === "loading";
                                const isRoot = () => row().kind === "root_header";
                                const rowPath = () => fileTreeRowPath(row()) ?? "";
                                const isSelected = () =>
                                  selectedPath() != null && selectedPath() === rowPath();
                                const isOpen = () =>
                                  row().kind === "entry" &&
                                  !row().isDir &&
                                  openFile()?.path === row().path &&
                                  openFile()?.machine === treeMachine();

                                return (
                                  <If
                                    when={isLoading()}
                                    else={
                                      <If
                                        when={isRoot()}
                                        else={
                                          /* 普通目录 / 文件行 */
                                          <SmeltContextMenu
                                            popupStyle={{ minWidth: 168 }}
                                            trigger={
                                              <View
                                                class="hover:bg-accent"
                                                onPress={() => {
                                                  const r = row();
                                                  selectedPath.set(r.path);
                                                  if (r.isDir) {
                                                    toggleExpandDir(r.path);
                                                  } else {
                                                    viewFileOn(treeMachine(), r.path, null);
                                                  }
                                                }}
                                                tooltip={
                                                  nameLikelyTruncated(row().name, row().depth, treeWidth()) ? (
                                                    <SmeltTooltip>
                                                      <Text
                                                        style={{
                                                          fontSize: look().type.text_xsmall.size,
                                                          color: look().palette.text,
                                                        }}
                                                      >
                                                        {row().name}
                                                      </Text>
                                                    </SmeltTooltip>
                                                  ) : undefined
                                                }
                                                style={{
                                                  width: "100%",
                                                  display: "flex",
                                                  flexDirection: "row",
                                                  alignItems: "center",
                                                  gap: 4,
                                                  height: 24,
                                                  paddingLeft:
                                                    8 + row().depth * 14 - (isSelected() ? 2 : 0),
                                                  paddingRight: 8,
                                                  borderLeftWidth: isSelected() ? 2 : 0,
                                                  borderLeftColor: look().palette.ink,
                                                  backgroundColor: isOpen()
                                                    ? look().palette.row_selected
                                                    : "transparent",
                                                  cursor: "pointer",
                                                  overflow: "hidden",
                                                }}
                                              >
                                                <If
                                                  when={row().isDir}
                                                  else={<View style={{ width: 14, flexShrink: 0 }} />}
                                                >
                                                  <SmeltDisclosureChevron
                                                    open={row().expanded}
                                                    size={12}
                                                    color={look().palette.muted}
                                                  />
                                                </If>
                                                <SmeltIcon
                                                  path={
                                                    row().isDir
                                                      ? row().expanded
                                                        ? "icons/folder-open.svg"
                                                        : "icons/folder.svg"
                                                      : "icons/file.svg"
                                                  }
                                                  size={14}
                                                  color={
                                                    row().isDir
                                                      ? look().palette.text
                                                      : look().palette.muted
                                                  }
                                                />
                                                <Text
                                                  style={{
                                                    flex: 1,
                                                    minWidth: 0,
                                                    fontSize: look().type.ui.size,
                                                    lineHeight: look().type.ui.line_height,
                                                    color: row().isDir
                                                      ? look().palette.text
                                                      : look().palette.muted,
                                                    numberOfLines: 1,
                                                  }}
                                                >
                                                  {row().name}
                                                </Text>
                                                <If when={Boolean(row().gitMark)}>
                                                  <Text
                                                    style={{
                                                      flexShrink: 0,
                                                      fontSize: look().type.text_xsmall.size,
                                                      fontWeight: 600,
                                                      color: gitMarkColor(
                                                        row().gitMark ?? "M",
                                                        look().palette,
                                                      ),
                                                    }}
                                                  >
                                                    {row().gitMark ?? ""}
                                                  </Text>
                                                </If>
                                              </View>
                                            }
                                          >
                                            <MenuRow
                                              label={t("send.path_menu")}
                                              onSelect={() => sendPathToSession(row().path)}
                                            />
                                            <MenuRow
                                              label={t("copy_path")}
                                              onSelect={() => void clipboardWrite(row().path)}
                                            />
                                            <If when={isLocalTree()}>
                                              <MenuRow
                                                label={isMac ? t("reveal.finder") : t("reveal.file_manager")}
                                                onSelect={() => void revealPath(row().path)}
                                              />
                                            </If>
                                            <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />
                                            <MenuRow
                                              label={
                                                row().isDir
                                                  ? t("delete.folder_menu")
                                                  : t("delete.file_menu")
                                              }
                                              iconPath="icons/trash.svg"
                                              danger={true}
                                              onSelect={() => startDelete(row().path, Boolean(row().isDir))}
                                            />
                                          </SmeltContextMenu>
                                        }
                                      >
                                        {/* 多根时的根标题行 */}
                                        <If
                                          when={Boolean(row().pinned)}
                                          else={
                                            <View
                                              class="hover:bg-accent"
                                              onPress={() => {
                                                selectedPath.set(row().path);
                                                toggleRootCollapsed(row().path);
                                              }}
                                              style={{
                                                display: "flex",
                                                flexDirection: "row",
                                                alignItems: "center",
                                                gap: 4,
                                                height: 24,
                                                paddingLeft: isSelected() ? 6 : 8,
                                                paddingRight: 8,
                                                borderLeftWidth: isSelected() ? 2 : 0,
                                                borderLeftColor: look().palette.ink,
                                                cursor: "pointer",
                                                overflow: "hidden",
                                              }}
                                            >
                                              <SmeltDisclosureChevron
                                                open={row().open}
                                                size={12}
                                                color={look().palette.muted}
                                              />
                                              <SmeltIcon
                                                path="icons/folder-open.svg"
                                                size={14}
                                                color={look().palette.text}
                                              />
                                              <Text
                                                style={{
                                                  flex: 1,
                                                  minWidth: 0,
                                                  fontSize: look().type.ui_medium.size,
                                                  fontWeight: look().type.ui_medium.weight,
                                                  color: look().palette.text,
                                                  numberOfLines: 1,
                                                }}
                                              >
                                                {styleOpsFor(treeMachine()).fileName(row().path) ??
                                                  row().path}
                                              </Text>
                                            </View>
                                          }
                                        >
                                          <SmeltContextMenu
                                            popupStyle={{ minWidth: 168 }}
                                            trigger={
                                              <View
                                                class="hover:bg-accent"
                                                onPress={() => {
                                                  selectedPath.set(row().path);
                                                  toggleRootCollapsed(row().path);
                                                }}
                                                style={{
                                                  width: "100%",
                                                  display: "flex",
                                                  flexDirection: "row",
                                                  alignItems: "center",
                                                  gap: 4,
                                                  height: 24,
                                                  paddingLeft: isSelected() ? 6 : 8,
                                                  paddingRight: 8,
                                                  borderLeftWidth: isSelected() ? 2 : 0,
                                                  borderLeftColor: look().palette.ink,
                                                  cursor: "pointer",
                                                  overflow: "hidden",
                                                }}
                                              >
                                                <SmeltDisclosureChevron
                                                  open={row().open}
                                                  size={12}
                                                  color={look().palette.muted}
                                                />
                                                <SmeltIcon
                                                  path="icons/folder-open.svg"
                                                  size={14}
                                                  color={look().palette.text}
                                                />
                                                <Text
                                                  style={{
                                                    flex: 1,
                                                    minWidth: 0,
                                                    fontSize: look().type.ui_medium.size,
                                                    fontWeight: look().type.ui_medium.weight,
                                                    color: look().palette.text,
                                                    numberOfLines: 1,
                                                  }}
                                                >
                                                  {styleOpsFor(treeMachine()).fileName(row().path) ??
                                                    row().path}
                                                </Text>
                                              </View>
                                            }
                                          >
                                            <MenuRow
                                              label={t("pin.remove")}
                                              onSelect={() => unpinRoot(row().path)}
                                            />
                                          </SmeltContextMenu>
                                        </If>
                                      </If>
                                    }
                                  >
                                    <View
                                      style={{
                                        height: 24,
                                        paddingLeft: 8 + row().depth * 14,
                                        paddingRight: 8,
                                        display: "flex",
                                        flexDirection: "row",
                                        alignItems: "center",
                                      }}
                                    >
                                      <Text
                                        style={{
                                          fontSize: look().type.text_xsmall.size,
                                          color: look().palette.muted,
                                        }}
                                      >
                                        {t("loading")}
                                      </Text>
                                    </View>
                                  </If>
                                );
                              }}
                            </Each>
                          </View>
                        </Scroll>
                      </View>
                    </If>
                  </If>
                }
              >
                {/* 搜索结果视图 */}
                <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}>
                  <View style={{ paddingLeft: 8, paddingRight: 8, paddingTop: 4, paddingBottom: 4, flexShrink: 0 }}>
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                        color: look().palette.muted,
                      }}
                    >
                      {searchStatusText()}
                    </Text>
                  </View>
                  <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
                    <View style={{ display: "flex", flexDirection: "column", paddingBottom: 4 }}>
                      <Each of={searchRows()} key={(r: any) => r.key}>
                        {(row: () => any) => (
                          <If
                            when={row().kind === "dir"}
                            else={
                              <View
                                class="hover:bg-accent"
                                onPress={() =>
                                  viewFileOn(
                                    searchState()?.machine ?? treeMachine(),
                                    row().path,
                                    row().line,
                                  )
                                }
                                style={{
                                  display: "flex",
                                  flexDirection: "column",
                                  paddingLeft: 8 + row().depth * 12,
                                  paddingRight: 8,
                                  paddingTop: 3,
                                  paddingBottom: 3,
                                  cursor: "pointer",
                                }}
                              >
                                <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
                                  <SmeltIcon path="icons/file.svg" size={13} color={look().palette.muted} />
                                  <Text
                                    style={{
                                      flex: 1,
                                      minWidth: 0,
                                      fontSize: look().type.ui.size,
                                      lineHeight: look().type.ui.line_height,
                                      color: look().palette.text,
                                      numberOfLines: 1,
                                    }}
                                  >
                                    {row().name}
                                  </Text>
                                  <If when={row().line != null}>
                                    <Text
                                      style={{
                                        fontSize: look().type.text_xsmall.size,
                                        color: look().palette.faint,
                                        flexShrink: 0,
                                      }}
                                    >
                                      {`:${row().line}`}
                                    </Text>
                                  </If>
                                </View>
                                <If when={Boolean(row().snippet)}>
                                  <Text
                                    style={{
                                      paddingLeft: 17,
                                      fontSize: look().type.text_xsmall.size,
                                      lineHeight: look().type.text_xsmall.line_height,
                                      color: look().palette.muted,
                                      numberOfLines: 1,
                                    }}
                                  >
                                    {row().snippet ?? ""}
                                  </Text>
                                </If>
                              </View>
                            }
                          >
                            <View
                              style={{
                                display: "flex",
                                flexDirection: "row",
                                alignItems: "center",
                                gap: 4,
                                height: 22,
                                paddingLeft: 8 + row().depth * 12,
                                paddingRight: 8,
                              }}
                            >
                              <SmeltIcon path="icons/folder-open.svg" size={13} color={look().palette.muted} />
                              <Text
                                style={{
                                  flex: 1,
                                  minWidth: 0,
                                  fontSize: look().type.ui.size,
                                  color: look().palette.muted,
                                  numberOfLines: 1,
                                }}
                              >
                                {row().name}
                              </Text>
                            </View>
                          </If>
                        )}
                      </Each>
                    </View>
                  </Scroll>
                </View>
              </If>
            </View>
          </ResizablePanel>
        </ResizablePanelGroup>
      </View>

      {/* 未保存切换确认框 */}
      <SmeltDialog
        open={unsavedDialog() != null}
        role="alertdialog"
        accessibilityLabel={t("switch.title", { name: unsavedDialog()?.currentName ?? "" })}
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) unsavedDialog.set(null);
        }}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
        style={{ width: 440, gap: 12 }}
      >
        <If when={unsavedDialog() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("switch.title", { name: unsavedDialog()?.currentName ?? "" })}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {t("switch.description", { name: unsavedDialog()?.targetName ?? "" })}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("cancel")}
              onPress={() => unsavedDialog.set(null)}
            />
            <SmeltButton
              small={true}
              label={t("switch.discard")}
              onPress={() => {
                const dlg = unsavedDialog();
                unsavedDialog.set(null);
                if (dlg) openFileNow(dlg.target.machine, dlg.target.path, dlg.line);
              }}
            />
            <SmeltButton
              primary={true}
              small={true}
              label={t("switch.save")}
              onPress={() => {
                const dlg = unsavedDialog();
                unsavedDialog.set(null);
                if (!dlg) return;
                pendingSwitchAfterSave = {
                  fromId: dlg.currentId,
                  target: dlg.target,
                  line: dlg.line,
                };
                void saveOpenFile();
              }}
            />
          </View>
        </If>
      </SmeltDialog>

      {/* 删除确认框 */}
      <SmeltDialog
        open={deleteDialog() != null}
        role="alertdialog"
        accessibilityLabel={
          deleteDialog()?.isDir ? t("delete.folder_confirm") : t("delete.file_confirm")
        }
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) deleteDialog.set(null);
        }}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
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
            {deleteDialog()?.isDir ? t("delete.folder_confirm") : t("delete.file_confirm")}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {deleteDialog()?.isDir
              ? t("delete.folder_description", { name: deleteDialog()?.name ?? "" })
              : t("delete.file_description", { name: deleteDialog()?.name ?? "" })}
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
              label={t("delete.action")}
              onPress={() => void confirmDelete()}
            />
          </View>
        </If>
      </SmeltDialog>
    </View>
  );
}

export default Main;
