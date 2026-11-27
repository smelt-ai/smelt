// 变更面板（NSH A2 独立 VM 版本）：
// - 顶部：更改 / 历史 分段切换 + 作用范围（全部/已暂存/未暂存）或仓库选择与同步菜单
// - 更改页：左 1st-class Rust <SmeltDiffView>（原生 multi-file patch / split & unified / 块操作 / 行选批注发送会话）
//           + 右 <ResizablePanelGroup> 多仓分组改动树 & 每仓独立提交框
// - 历史页：三栏 <ResizablePanelGroup>（分支树 + <SmeltVector> 矢量贝塞尔分支图/提交列表 + 提交详情/单文件 Diff）
// - 全事件驱动（project.watch / project_changed / reconnected / window_activated），仅前台可见时 60s 自动 fetch。
import { computed, effect, Overlay, Scroll, signal, Text, View } from "nsh";
import {
  Input,
  MenuItem,
  MenuSeparator,
  ResizablePanel,
  ResizablePanelGroup,
} from "nsh/base";
import {
  activeContext,
  bootTask,
  call,
  callWithBlobs,
  clipboardWrite,
  currentTheme,
  DIALOG_SURFACE_CLASS,
  initHost,
  localOs,
  MENU_ITEM_CLASS,
  onCommand,
  onEvent,
  onSlices,
  openFile,
  openUrl,
  pathFileName,
  pathJoin,
  pathStartsWith,
  pathStyle,
  registerCommands,
  registerViews,
  revealPath,
  run,
  SmeltButton,
  SmeltContextMenu,
  SmeltDialog,
  SmeltDiffView,
  SmeltDisclosureChevron,
  SmeltIcon,
  SmeltMenu,
  SmeltTooltip,
  SmeltVector,
  stableMemo,
  t,
  text,
  viewShown,
  windowActive,
} from "smelt/sdk";

import {
  buildGitTree,
  clearConfirmedGitIndexOps,
  commitButtonBusyKey,
  commitGraphRowSvg,
  diffEntryFor,
  diffFeedbackText,
  diffLeftOutReason,
  formatCommitTime,
  formatSize,
  fullFilePath,
  GIT_AUTOFETCH_INTERVAL_MS,
  gitAutofetchWait,
  gitDiscoveryWait,
  gitEmptyListKind,
  gitIndexOpInFlight,
  gitStatusResponseAction,
  gitStatusRetryDelay,
  GRAPH_ROW_H,
  graphWidth,
  layoutGraph,
  LOG_LIMIT,
  MAX_TREE_WIDTH,
  MIN_TREE_WIDTH,
  needsFetch,
  projectKey,
  pullRequestLink,
  releaseGitWrite,
  repoKindGlyph,
  repoNeedsAttention,
  reserveGitWrite,
  resolveGitWriteTarget,
  seenChange,
  seenChanges,
  shouldCloseAggregateDiff,
  splitStagedAndChanged,
} from "./model.js";

const VIEW_ID = "smelt.git/main";
const SET_VIEW_BADGE_CMD = "smelt.app/set-view-badge";
const NOTIFY_CMD = "smelt.app/notify";
// 约定：smelt.sessions 的 SEND_TEXT（权威定义在 sessions contract）。
const SEND_TEXT_CMD = "smelt.sessions/send-text";

const NETWORK_TIMEOUT_MS = 600_000;

type DiffScope = "all" | "staged" | "unstaged";
type GitTab = "changes" | "log";
type LogScope = "head" | "all" | { branch: string };
type HunkOps = "none" | "stage_discard" | "unstage";

interface ProjectRef {
  machine: string | null;
  root: string;
}

interface DiscoveredRepo {
  root: string;
  rel_path: string;
  kind: "root" | "submodule" | "nested";
}

interface RepoSet {
  repos: DiscoveredRepo[];
  truncated: boolean;
}

interface GitStatusData {
  ok: boolean;
  branch: string;
  upstream: string | null;
  ahead: number;
  behind: number;
  files: Array<[string, string]>;
  worktree_hashes: Record<string, string>;
  stash_count: number;
  insertions: number;
  deletions: number;
}

interface BranchList {
  local: string[];
  remote: string[];
}

interface CommitNode {
  hash: string;
  short: string;
  author: string;
  time: number;
  subject: string;
  parents: string[];
  refs: string[];
}

interface GitDiffState {
  root: ProjectRef;
  /** 单文件视图的文件；汇总视图为空。 */
  path: string;
  oldPath: string | null;
  untracked: boolean;
  aggregate: boolean;
  scope: DiffScope;
  /** 回复到了（成功或失败）。 */
  loaded: boolean;
  /** 补丁 Blob 的地址，正文不进插件的堆；没有可显示的为 null。换状态时放掉（`setGitDiff`）。 */
  patchUrl: string | null;
  /** 代替补丁的一句话：无差异、太大、出错。 */
  message: string | null;
  /** 汇总视图的改动列表（`in_patch` 的在补丁里，其余单独打开）。 */
  entries: any[];
  worktreeFile: string | null;
  patchable: boolean;
  hasStaged: boolean;
}

interface DangerDialogState {
  title: string;
  body: string;
  warning: string;
  okLabel: string;
  onConfirm: () => Promise<boolean> | boolean;
}

interface CommentRange {
  side: "old" | "new";
  start: number;
  end: number;
}

interface ParkedUi {
  tab: GitTab;
  diff: GitDiffState | null;
  diffSplit: boolean;
  diffScope: DiffScope;
  pendingDiffFile: string | null;
  collapsedDiffFiles: Set<string>;
  treeCollapsed: Set<string>;
}

// ---- 模块级响应式状态 ----

const currentProject = signal<ProjectRef | null>(null);
const gitTab = signal<GitTab>("changes");
const diffScope = signal<DiffScope>("all");
const diffSplit = signal<boolean>(false);
const treeWidth = signal<number>(MIN_TREE_WIDTH);
const activeRepoOverride = signal<ProjectRef | null>(null);
const gitDiff = signal<GitDiffState | null>(null);
const commentRange = signal<CommentRange | null>(null);
const commentOpen = signal<boolean>(false);
const commentText = signal<string>("");
const dangerDialog = signal<DangerDialogState | null>(null);

// 日志页状态
const logScope = signal<LogScope>("head");
const logCommits = signal<CommitNode[]>([]);
const logGraphRows = signal<any[]>([]);
const logLoading = signal<boolean>(false);
const logSelectedHash = signal<string | null>(null);
const logDetailFiles = signal<Array<[string, string]>>([]);
const logDetailLoading = signal<boolean>(false);
const logDetailFile = signal<string | null>(null);
// 提交里那个文件的补丁 Blob 地址，或代替它的一句话；都为 null 时在读取。
const logDetailUrl = signal<string | null>(null);
const logDetailMessage = signal<string | null>(null);
let logLoadedRootKey: string | null = null;

// 版本信号：驱动依赖 Map/Set 的 computed 重算
const stateVersion = signal<number>(0);

// 缓存表（按 projectKey 索引）
const repoCache = new Map<string, { value: RepoSet; stale: boolean; fetchedAt: number }>();
const repoInflight = new Map<string, boolean>();
const repoThrottleTimers = new Map<string, ReturnType<typeof setTimeout>>();

const statusCache = new Map<string, { value: GitStatusData; stale: boolean; fetchedAt: number }>();
const statusGeneration = new Map<string, number>();
const statusInflight = new Set<string>();
const statusFailures = new Map<string, number>();
const statusRetryTimers = new Map<string, ReturnType<typeof setTimeout>>();

const branchCache = new Map<string, { value: BranchList; stale: boolean; fetchedAt: number }>();
const branchGeneration = new Map<string, number>();
const branchInflight = new Set<string>();

const commitMessages = new Map<string, ReturnType<typeof signal<string>>>();

function commitMessageSignal(repoKey: string) {
  let sig = commitMessages.get(repoKey);
  if (!sig) {
    sig = signal<string>("");
    commitMessages.set(repoKey, sig);
  }
  return sig;
}
const commitErrors = new Map<string, string>();
const gitWrites = new Map<string, string>();
const indexPending = new Map<string, { completed: boolean }>();
const repoCollapsed = new Set<string>();
const treeCollapsed = new Set<string>();
const collapsedDiffFiles = new Set<string>();
const watches = new Set<string>();
const failedWatches = new Set<string>();
const parked = new Map<string, ParkedUi>();
const autofetchAt = new Map<string, number>();

let diffGen = 0;
let pendingDiffFile: string | null = null;
let changesDiffApi: any = null;
let autofetchTimer: ReturnType<typeof setTimeout> | null = null;
let lastBadgeLabel: string | null | undefined = undefined;
let lastWindowActive = true;

function bump() {
  stateVersion.set(stateVersion() + 1);
  syncBadge();
}

function notifyError(body: string) {
  console.error("[git]", body);
  void run(NOTIFY_CMD, { subtitle: t("notify.error"), body }).catch(() => {});
}

function sameProject(a: ProjectRef | null, b: ProjectRef | null): boolean {
  return a?.machine === b?.machine && a?.root === b?.root;
}

function projectRepoList(project: ProjectRef): DiscoveredRepo[] {
  const cached = repoCache.get(projectKey(project.machine, project.root));
  if (cached && cached.value.repos.length > 0) return cached.value.repos;
  return [{ root: project.root, rel_path: "", kind: "root" }];
}

function projectRepoRoots(project: ProjectRef): ProjectRef[] {
  return projectRepoList(project).map((r) => ({ machine: project.machine, root: r.root }));
}

function firstRepoWithChanges(project: ProjectRef): string | null {
  const cached = repoCache.get(projectKey(project.machine, project.root));
  if (!cached) return null;
  for (const repo of cached.value.repos) {
    const st = statusCache.get(projectKey(project.machine, repo.root));
    if (st && st.value.ok && st.value.files.length > 0) return repo.root;
  }
  return null;
}

function activeRepoFor(project: ProjectRef): ProjectRef {
  const override = activeRepoOverride();
  const selected = override && override.machine === project.machine ? override.root : null;
  const cached = repoCache.get(projectKey(project.machine, project.root))?.value ?? null;
  const targetRoot = resolveGitWriteTarget(
    project.root,
    selected,
    cached,
    firstRepoWithChanges(project),
  );
  return { machine: project.machine, root: targetRoot };
}

function syncBadge() {
  const proj = currentProject();
  let label: string | null = null;
  if (proj) {
    let count = 0;
    for (const r of projectRepoRoots(proj)) {
      const st = statusCache.get(projectKey(r.machine, r.root));
      if (st?.value.ok) count += st.value.files.length;
    }
    if (count > 0) label = String(count);
  }
  if (label === lastBadgeLabel) return;
  lastBadgeLabel = label;
  void run(SET_VIEW_BADGE_CMD, { view: VIEW_ID, label }).catch(() => {
    if (lastBadgeLabel === label) lastBadgeLabel = undefined;
  });
}

// ---- 监听与失效 ----

function ensureWatch(project: ProjectRef) {
  const k = projectKey(project.machine, project.root);
  if (watches.has(k) || failedWatches.has(k)) return;
  watches.add(k);
  void call(project.machine, "project.watch", { root: project.root }).catch((err) => {
    watches.delete(k);
    failedWatches.add(k);
    console.error(`[git] project.watch(${project.root}) failed:`, err);
  });
}

function markStatusDirty(repoKey: string) {
  statusGeneration.set(repoKey, (statusGeneration.get(repoKey) ?? 0) + 1);
  const c = statusCache.get(repoKey);
  if (c) c.stale = true;
}

function markBranchesStale(repoKey: string) {
  branchGeneration.set(repoKey, (branchGeneration.get(repoKey) ?? 0) + 1);
  const c = branchCache.get(repoKey);
  if (c) c.stale = true;
}

function splitProjectKey(k: string): [string | null, string] {
  const sep = k.indexOf("\0");
  return [sep > 0 ? k.slice(0, sep) : null, sep >= 0 ? k.slice(sep + 1) : k];
}

function markCachesStaleWhere(predicate: (machine: string | null, root: string) => boolean) {
  for (const k of new Set([...statusCache.keys(), ...statusInflight])) {
    const [m, r] = splitProjectKey(k);
    if (predicate(m, r)) markStatusDirty(k);
  }
  for (const k of new Set([...branchCache.keys(), ...branchInflight])) {
    const [m, r] = splitProjectKey(k);
    if (predicate(m, r)) markBranchesStale(k);
  }
  for (const [k, c] of repoCache.entries()) {
    const [m, r] = splitProjectKey(k);
    if (predicate(m, r)) c.stale = true;
  }
  for (const k of repoInflight.keys()) {
    const [m, r] = splitProjectKey(k);
    if (predicate(m, r)) repoInflight.set(k, true);
  }
}

function invalidateRepo(repo: ProjectRef) {
  const k = projectKey(repo.machine, repo.root);
  markStatusDirty(k);
  markBranchesStale(k);
  refreshVisible();
}

// ---- 仓库发现 / Status / Branches ----

function discoverRepos(project: ProjectRef) {
  const k = projectKey(project.machine, project.root);
  if (repoInflight.has(k)) return;
  const wait = gitDiscoveryWait(repoCache.get(k) ?? null, Date.now());
  if (wait == null) return;
  repoInflight.set(k, false);

  if (wait > 0) {
    // 限频等待（非轮询）：仅在事件标旧后等满最小间隔再触发一次发现。
    const prevTimer = repoThrottleTimers.get(k);
    if (prevTimer != null) clearTimeout(prevTimer);
    const timer = setTimeout(() => {
      repoThrottleTimers.delete(k);
      repoInflight.delete(k);
      refreshVisible();
    }, wait);
    repoThrottleTimers.set(k, timer);
    return;
  }

  void call(project.machine, "git.discover", { root: project.root })
    .then((res: any) => {
      const changed = repoInflight.get(k) ?? false;
      repoInflight.delete(k);
      repoCache.set(k, {
        value: {
          repos: Array.isArray(res?.repos) ? res.repos : [],
          truncated: Boolean(res?.truncated),
        },
        stale: changed,
        fetchedAt: Date.now(),
      });
      bump();
      refreshVisible();
    })
    .catch((err) => {
      const changed = repoInflight.get(k) ?? false;
      repoInflight.delete(k);
      console.error(`[git] discover(${project.root}) failed:`, err);
      const existing = repoCache.get(k);
      if (existing) {
        existing.stale = changed;
        existing.fetchedAt = changed ? 0 : Date.now();
      } else {
        repoCache.set(k, {
          value: { repos: [], truncated: false },
          stale: changed,
          fetchedAt: 0,
        });
      }
      bump();
    });
}

function ensureStatus(repo: ProjectRef) {
  const k = projectKey(repo.machine, repo.root);
  if (!needsFetch(statusCache.get(k) ?? null) || statusInflight.has(k)) return;
  const reqGen = statusGeneration.get(k) ?? 0;
  statusInflight.add(k);

  void call(repo.machine, "git.status", { root: repo.root })
    .then((raw: any) => {
      const data: GitStatusData = {
        ok: Boolean(raw?.ok),
        branch: String(raw?.branch ?? ""),
        upstream: typeof raw?.upstream === "string" ? raw.upstream : null,
        ahead: Number(raw?.ahead ?? 0),
        behind: Number(raw?.behind ?? 0),
        files: Array.isArray(raw?.files) ? raw.files : [],
        worktree_hashes: raw?.worktree_hashes && typeof raw.worktree_hashes === "object" ? raw.worktree_hashes : {},
        stash_count: Number(raw?.stash_count ?? 0),
        insertions: Number(raw?.insertions ?? 0),
        deletions: Number(raw?.deletions ?? 0),
      };
      handleStatusResult(repo, k, reqGen, data);
    })
    .catch(() => {
      handleStatusResult(repo, k, reqGen, {
        ok: false,
        branch: "",
        upstream: null,
        ahead: 0,
        behind: 0,
        files: [],
        worktree_hashes: {},
        stash_count: 0,
        insertions: 0,
        deletions: 0,
      });
    });
}

function handleStatusResult(repo: ProjectRef, k: string, reqGen: number, data: GitStatusData) {
  const curGen = statusGeneration.get(k) ?? 0;
  const action = gitStatusResponseAction(curGen, reqGen, data.ok);

  if (action === "accept_current") {
    statusInflight.delete(k);
    statusFailures.delete(k);
    statusCache.set(k, { value: data, stale: false, fetchedAt: Date.now() });
    clearConfirmedGitIndexOps(indexPending, k);
  } else if (action === "accept_stale_and_refresh") {
    statusInflight.delete(k);
    statusFailures.delete(k);
    statusCache.set(k, { value: data, stale: true, fetchedAt: Date.now() });
    ensureStatus(repo);
  } else {
    const changed = curGen !== reqGen;
    const count = (statusFailures.get(k) ?? 0) + 1;
    statusFailures.set(k, count);
    const existing = statusCache.get(k);
    if (existing) {
      existing.stale = changed;
    } else {
      statusCache.set(k, { value: data, stale: changed, fetchedAt: Date.now() });
    }
    const delay = gitStatusRetryDelay(count);
    if (delay != null) {
      const prevTimer = statusRetryTimers.get(k);
      if (prevTimer != null) clearTimeout(prevTimer);
      const timer = setTimeout(() => {
        statusRetryTimers.delete(k);
        statusInflight.delete(k);
        const c = statusCache.get(k);
        if (c) c.stale = true;
        ensureStatus(repo);
        bump();
      }, delay);
      statusRetryTimers.set(k, timer);
    } else {
      statusInflight.delete(k);
      if (changed) ensureStatus(repo);
    }
  }
  bump();
  preparePanel();
}

function ensureBranches(repo: ProjectRef) {
  const k = projectKey(repo.machine, repo.root);
  if (!needsFetch(branchCache.get(k) ?? null) || branchInflight.has(k)) return;
  const reqGen = branchGeneration.get(k) ?? 0;
  branchInflight.add(k);

  void call(repo.machine, "git.branches", { root: repo.root })
    .then((res: any) => {
      branchInflight.delete(k);
      if ((branchGeneration.get(k) ?? 0) !== reqGen) {
        ensureBranches(repo);
        return;
      }
      branchCache.set(k, {
        value: {
          local: Array.isArray(res?.local) ? res.local : [],
          remote: Array.isArray(res?.remote) ? res.remote : [],
        },
        stale: false,
        fetchedAt: Date.now(),
      });
      bump();
    })
    .catch((err) => {
      branchInflight.delete(k);
      console.error(`[git] branches(${repo.root}) failed:`, err);
      const existing = branchCache.get(k);
      if (existing) {
        existing.stale = false;
      } else {
        branchCache.set(k, { value: { local: [], remote: [] }, stale: false, fetchedAt: Date.now() });
      }
      bump();
    });
}

// ---- 前台可见自动 fetch（60s） ----

function scheduleAutofetch() {
  if (autofetchTimer != null) return;
  if (!viewShown(VIEW_ID) || !windowActive()) return;
  const proj = currentProject();
  if (!proj) return;
  const k = projectKey(proj.machine, proj.root);
  const wait = gitAutofetchWait(autofetchAt.get(k) ?? null, Date.now());
  if (wait <= 0) {
    autofetchAt.set(k, Date.now());
    runFetch(activeRepoFor(proj), true);
    scheduleNextAutofetch(GIT_AUTOFETCH_INTERVAL_MS);
  } else {
    scheduleNextAutofetch(wait);
  }
}

function scheduleNextAutofetch(ms: number) {
  if (autofetchTimer != null) return;
  // 轮询例外：远端仓库变了没有事件可等；周期 60 秒，仅在视图露着且窗口在前台时续排，离开后最多醒一次即停。
  autofetchTimer = setTimeout(() => {
    autofetchTimer = null;
    scheduleAutofetch();
  }, ms);
}

// ---- 写操作调度 ----

async function runRepoWrite(
  repo: ProjectRef,
  op: string,
  work: () => Promise<void>,
  onDone: (err: string | null) => void,
) {
  const k = projectKey(repo.machine, repo.root);
  if (!reserveGitWrite(gitWrites, k, op)) return false;
  bump();
  let err: string | null = null;
  try {
    await work();
  } catch (e: any) {
    err = e?.message ?? String(e);
  }
  releaseGitWrite(gitWrites, k, op);
  onDone(err);
  bump();
  return true;
}

function setActiveRepo(repo: ProjectRef) {
  const cur = activeRepoOverride();
  if (sameProject(cur, repo)) return;
  activeRepoOverride.set(repo);
  resetDiffView();
  refreshVisible();
  bump();
}

function runFetch(repo: ProjectRef, silent: boolean) {
  void runRepoWrite(
    repo,
    "fetch",
    () => call(repo.machine, "git.fetch", { root: repo.root }, NETWORK_TIMEOUT_MS),
    (err) => {
      if (!err) invalidateRepo(repo);
      else if (!silent) notifyError(err);
    },
  );
}

function runPull(repo: ProjectRef) {
  void runRepoWrite(
    repo,
    "pull",
    () => call(repo.machine, "git.pull", { root: repo.root }, NETWORK_TIMEOUT_MS),
    (err) => {
      if (!err) {
        invalidateRepo(repo);
        reloadLog(repo);
      } else {
        notifyError(err);
      }
    },
  );
}

function runPushOnly(repo: ProjectRef) {
  const k = projectKey(repo.machine, repo.root);
  const branch = statusCache.get(k)?.value.branch ?? "";
  commitErrors.delete(k);
  void runRepoWrite(
    repo,
    "push",
    () => call(repo.machine, "git.push", { root: repo.root, branch }, NETWORK_TIMEOUT_MS),
    (err) => {
      if (!err) invalidateRepo(repo);
      else commitErrors.set(k, err);
    },
  );
}

function runCommit(repo: ProjectRef, push: boolean) {
  const k = projectKey(repo.machine, repo.root);
  const message = commitMessageSignal(k)().trim();
  if (!message) return;
  const branch = statusCache.get(k)?.value.branch ?? "";
  const op = push ? "commit_and_push" : "commit";
  commitErrors.delete(k);
  void runRepoWrite(
    repo,
    op,
    () =>
      call(
        repo.machine,
        "git.commit",
        { root: repo.root, message, push, branch },
        NETWORK_TIMEOUT_MS,
      ),
    (err) => {
      if (!err) {
        commitMessageSignal(k).set("");
        invalidateRepo(repo);
      } else {
        commitErrors.set(k, err);
      }
    },
  );
}

function runStashPush(repo: ProjectRef) {
  void runRepoWrite(
    repo,
    "stash",
    () => call(repo.machine, "git.stash_push", { root: repo.root }),
    (err) => {
      if (!err) invalidateRepo(repo);
      else notifyError(err);
    },
  );
}

function runStashPop(repo: ProjectRef) {
  void runRepoWrite(
    repo,
    "stash_pop",
    () => call(repo.machine, "git.stash_pop", { root: repo.root }),
    (err) => {
      if (!err) invalidateRepo(repo);
      else notifyError(err);
    },
  );
}

function runCheckout(repo: ProjectRef, branch: string) {
  void runRepoWrite(
    repo,
    "checkout",
    () => call(repo.machine, "git.checkout", { root: repo.root, branch }),
    (err) => {
      if (!err) invalidateRepo(repo);
      else notifyError(err);
    },
  );
}

function runMerge(repo: ProjectRef, branch: string) {
  void runRepoWrite(
    repo,
    "merge",
    () => call(repo.machine, "git.merge", { root: repo.root, branch }),
    (err) => {
      if (!err) {
        invalidateRepo(repo);
        reloadLog(repo);
      } else {
        notifyError(err);
      }
    },
  );
}

function startDeleteBranch(repo: ProjectRef, branch: string, remote: boolean) {
  dangerDialog.set({
    title: remote ? t("branch.delete_remote_title") : t("branch.delete_title"),
    body: remote
      ? t("branch.delete_remote_body", { branch })
      : t("branch.delete_body", { branch }),
    warning: remote ? t("branch.delete_remote_warning") : t("branch.delete_warning"),
    okLabel: t("branch.delete_ok"),
    onConfirm: async () => {
      const k = projectKey(repo.machine, repo.root);
      if (gitWrites.has(k)) return false;
      void runRepoWrite(
        repo,
        remote ? "delete_remote_branch" : "delete_branch",
        () =>
          call(
            repo.machine,
            "git.branch_delete",
            { root: repo.root, branch, remote },
            NETWORK_TIMEOUT_MS,
          ),
        (err) => {
          if (!err) {
            invalidateRepo(repo);
            reloadLog(repo);
          } else {
            notifyError(err);
          }
        },
      );
      return true;
    },
  });
}

function stageFile(repo: ProjectRef, path: string, stage: boolean) {
  const rk = projectKey(repo.machine, repo.root);
  const pk = `${rk}\0${path}`;
  void runRepoWrite(
    repo,
    "index_file",
    () => {
      indexPending.set(pk, { completed: false });
      bump();
      return call(repo.machine, stage ? "git.stage" : "git.unstage", {
        root: repo.root,
        paths: [path],
      });
    },
    (err) => {
      if (!err) {
        markStatusDirty(rk);
        const cur = indexPending.get(pk);
        if (cur) cur.completed = true;
        ensureStatus(repo);
      } else {
        indexPending.delete(pk);
        notifyError(err);
      }
    },
  );
}

function stagePaths(repo: ProjectRef, paths: string[], stage: boolean) {
  if (paths.length === 0) return;
  const rk = projectKey(repo.machine, repo.root);
  void runRepoWrite(
    repo,
    stage ? "stage" : "unstage",
    () =>
      call(repo.machine, stage ? "git.stage" : "git.unstage", {
        root: repo.root,
        paths,
      }),
    (err) => {
      if (err) {
        notifyError(t(stage ? "stage.stage_failed" : "stage.unstage_failed", { error: err }));
      }
      for (const k of [...indexPending.keys()]) {
        if (k.startsWith(`${rk}\0`)) indexPending.delete(k);
      }
      invalidateRepo(repo);
      ensureStatus(repo);
    },
  );
}

function startDiscardFile(repo: ProjectRef, path: string) {
  const rk = projectKey(repo.machine, repo.root);
  const seen = seenChange(statusCache.get(rk)?.value ?? null, path);
  if (!seen) {
    notifyError(t("discard.gone", { path }));
    return;
  }
  const untracked = seen.xy === "??";
  dangerDialog.set({
    title: untracked ? t("discard.file_untracked_title") : t("discard.file_title"),
    body: untracked
      ? t("discard.file_untracked_body", { path })
      : t("discard.file_body", { path }),
    warning: t("discard.no_reflog"),
    okLabel: untracked ? t("discard.file_untracked_ok") : t("discard.file_ok"),
    onConfirm: () => {
      if (gitWrites.has(rk)) return false;
      void runRepoWrite(
        repo,
        "discard_file",
        () => call(repo.machine, "git.discard_file", { root: repo.root, seen }),
        (err) => {
          invalidateRepo(repo);
          if (!err) {
            if (gitDiff()?.path === path) resetDiffView();
          } else {
            notifyError(err);
          }
        },
      );
      return true;
    },
  });
}

function startDiscardAll(repo: ProjectRef) {
  const rk = projectKey(repo.machine, repo.root);
  const st = statusCache.get(rk)?.value ?? null;
  if (!st) {
    notifyError(t("discard.not_loaded"));
    return;
  }
  const seen = seenChanges(st);
  dangerDialog.set({
    title: t("discard.all_title"),
    body: t("discard.all_body"),
    warning: t("discard.no_reflog"),
    okLabel: t("discard.all_ok"),
    onConfirm: () => {
      if (gitWrites.has(rk)) return false;
      void runRepoWrite(
        repo,
        "discard_changes",
        () => call(repo.machine, "git.discard_all", { root: repo.root, seen }),
        (err) => {
          invalidateRepo(repo);
          if (!err) resetDiffView();
          else notifyError(err);
        },
      );
      return true;
    },
  });
}

async function openPullRequest(repo: ProjectRef) {
  try {
    const info = await call(repo.machine, "git.repo_info", { root: repo.root });
    const res = pullRequestLink(info, t);
    if (res.ok && res.url) {
      await openUrl(res.url);
    } else {
      notifyError(res.error ?? t("pull_request.not_repo"));
    }
  } catch (err: any) {
    notifyError(err?.message ?? String(err));
  }
}

// ---- Diff 视图管理 / 块操作 / 批注评论 ----

function clearComment() {
  commentRange.set(null);
  commentOpen.set(false);
  commentText.set("");
  try {
    changesDiffApi?.setSelectedLines?.(null);
  } catch {
    // 忽略
  }
}

function resetDiffView() {
  diffGen += 1;
  pendingDiffFile = null;
  setGitDiff(null);
  clearComment();
}

/** 换掉当前的 diff 状态，放掉旧状态的补丁 Blob。停放起来的状态（切项目）不经这里。 */
function setGitDiff(next: GitDiffState | null) {
  const old = gitDiff()?.patchUrl;
  if (old && old !== next?.patchUrl) URL.revokeObjectURL(old);
  gitDiff.set(next);
}

function diffState(
  repo: ProjectRef,
  path: string,
  aggregate: boolean,
  scope: DiffScope,
  rest: Partial<GitDiffState> = {},
): GitDiffState {
  return {
    root: repo,
    path,
    oldPath: null,
    untracked: false,
    aggregate,
    scope,
    loaded: false,
    patchUrl: null,
    message: null,
    entries: [],
    worktreeFile: null,
    patchable: false,
    hasStaged: false,
    ...rest,
  };
}

/** 有内容的 Blob 换成地址交给 DiffView，空的为 null。 */
function blobUrl(blob: any): string | null {
  return blob instanceof Blob && blob.size > 0 ? URL.createObjectURL(blob) : null;
}

/** 一个 `GitDiffText`（正文以 Blob 交来）：要显示的补丁地址，或代替它的一句话。 */
function diffShown(diff: any, empty: string): { url: string | null; message: string | null } {
  if (diff?.kind === "too_large") {
    return { url: null, message: t("diff.too_large", { size: formatSize(diff.size) }) };
  }
  const url = blobUrl(diff?.text);
  return { url, message: url ? null : empty };
}

function hunkOpsFor(diff: GitDiffState | null): HunkOps {
  if (!diff || diff.aggregate || !diff.patchable) return "none";
  if (diff.scope === "unstaged") return "stage_discard";
  if (diff.scope === "staged") return "unstage";
  return !diff.hasStaged ? "stage_discard" : "none";
}

function openDiff(repo: ProjectRef, path: string, untracked: boolean, oldPath: string | null = null) {
  setActiveRepo(repo);
  const scope = diffScope();
  pendingDiffFile = null;
  const gen = ++diffGen;
  clearComment();
  gitTab.set("changes");
  const file = { oldPath, untracked };
  setGitDiff(diffState(repo, path, false, scope, file));

  void callWithBlobs(
    repo.machine,
    "git.diff.file",
    { root: repo.root, path, old_path: oldPath, untracked, scope },
    ["/diff/text"],
  )
    .then((res: any) => {
      if (gen !== diffGen) return;
      const shown = diffShown(res?.diff, t("diff.empty"));
      setGitDiff(
        diffState(repo, path, false, scope, {
          ...file,
          loaded: true,
          patchUrl: shown.url,
          message: shown.message,
          worktreeFile: typeof res?.worktree_file === "string" ? res.worktree_file : null,
          patchable: !untracked && !res?.is_gitlink && shown.url != null,
          hasStaged: Boolean(res?.has_staged),
        }),
      );
    })
    .catch((err: any) => {
      if (gen !== diffGen) return;
      const message = t("diff.run_failed", { error: err?.message ?? String(err) });
      setGitDiff(diffState(repo, path, false, scope, { ...file, loaded: true, message }));
    });
}

function openEntryAlone(repo: ProjectRef, entry: any) {
  openDiff(repo, entry.path, entry.status === "untracked", entry.old_path ?? null);
}

function openAllDiffs(repo: ProjectRef, keepPending = false) {
  if (!keepPending) pendingDiffFile = null;
  const scope = diffScope();
  const rk = projectKey(repo.machine, repo.root);
  const statusFiles = statusCache.get(rk)?.value.files ?? [];
  const untrackedPaths =
    scope === "all" || scope === "unstaged"
      ? statusFiles.filter(([code]) => code === "??").map(([, p]) => p)
      : [];
  const gen = ++diffGen;
  clearComment();
  setGitDiff(diffState(repo, "", true, scope));

  void callWithBlobs(
    repo.machine,
    "git.diff.files",
    { root: repo.root, scope, untracked_paths: untrackedPaths },
    ["/patch"],
  )
    .then((res: any) => {
      if (gen !== diffGen) return;
      const entries: any[] = Array.isArray(res?.files) ? res.files : [];
      const patchUrl = blobUrl(res?.patch);
      setGitDiff(
        diffState(repo, "", true, scope, {
          loaded: true,
          patchUrl,
          entries,
          message: patchUrl || entries.length > 0 ? null : t("diff.empty"),
        }),
      );
      queueMicrotask(() => flushPendingDiffFile());
    })
    .catch((err: any) => {
      if (gen !== diffGen) return;
      const message = t("diff.run_failed", { error: err?.message ?? String(err) });
      setGitDiff(diffState(repo, "", true, scope, { loaded: true, message }));
    });
}

function openAggregateFile(repo: ProjectRef, path: string) {
  if (!sameProject(activeRepoOverride(), repo)) {
    activeRepoOverride.set(repo);
    resetDiffView();
    refreshVisible();
  }
  pendingDiffFile = path;
  const cur = gitDiff();
  const sameScope = cur && sameProject(cur.root, repo) && cur.aggregate && cur.scope === diffScope();
  const listed = sameScope && cur.loaded && diffEntryFor(cur.entries, path) != null;
  const loading = sameScope && !cur.loaded;

  if (!listed && !loading) {
    openAllDiffs(repo, true);
  }
  flushPendingDiffFile();
}

/** 汇总视图到了以后跳到点过的文件：在补丁里的滚过去，不在的单独打开。 */
function flushPendingDiffFile() {
  const cur = gitDiff();
  if (!pendingDiffFile || !cur || !cur.aggregate || !cur.loaded) return;
  const entry = diffEntryFor(cur.entries, pendingDiffFile);
  if (!entry) return;
  if (!entry.in_patch) {
    openEntryAlone(cur.root, entry);
    return;
  }
  if (!changesDiffApi) return;
  pendingDiffFile = null;
  collapsedDiffFiles.delete(entry.path);
  changesDiffApi.expandFile?.(entry.path);
  changesDiffApi.scrollToFile?.(entry.path);
}

/** 第 `gap` 块单独成补丁交给 `git apply`；块的原文由 DiffView 从它读到的补丁里切出来。 */
async function applyHunk(repo: ProjectRef, gap: number, action: "stage" | "unstage" | "discard") {
  const cur = gitDiff();
  const patch: string | null = cur?.patchable ? await changesDiffApi?.hunkPatch?.(gap) : null;
  if (!cur || !patch) {
    notifyError(t("hunk.unsupported"));
    return false;
  }
  if (gitDiff() !== cur) return false;
  const op =
    action === "stage" ? "stage_hunk" : action === "unstage" ? "unstage_hunk" : "discard_hunk";

  void runRepoWrite(
    repo,
    op,
    () => call(repo.machine, "git.apply_patch", { root: repo.root, action, patch }),
    (err) => {
      if (!err) {
        invalidateRepo(repo);
        openDiff(repo, cur.path, cur.untracked, cur.oldPath);
      } else {
        notifyError(t("hunk.apply_failed", { error: err }));
      }
    },
  );
  return true;
}

function startDiscardHunk(repo: ProjectRef, gap: number) {
  const filePath = gitDiff()?.path ?? "";
  dangerDialog.set({
    title: t("discard.hunk_title"),
    body: t("discard.hunk_body", { path: filePath, index: gap + 1 }),
    warning: t("discard.hunk_warning"),
    okLabel: t("discard.hunk_ok"),
    onConfirm: () => {
      const rk = projectKey(repo.machine, repo.root);
      if (gitWrites.has(rk)) return false;
      return applyHunk(repo, gap, "discard");
    },
  });
}

function jumpHunk(forward: boolean) {
  if (!changesDiffApi) return;
  try {
    if (forward) changesDiffApi.nextHunk?.();
    else changesDiffApi.previousHunk?.();
  } catch {
    // 忽略
  }
}

/** 补丁里的文件：有一个展开的就全收起，否则全展开。 */
function toggleAllAggregateFiles() {
  const cur = gitDiff();
  if (!cur || !cur.aggregate || !changesDiffApi) return;
  const paths = cur.entries.filter((e) => e.in_patch).map((e) => String(e.path));
  if (paths.length === 0) return;
  const collapse = paths.some((p) => !collapsedDiffFiles.has(p));
  for (const p of paths) {
    if (collapse) {
      collapsedDiffFiles.add(p);
      changesDiffApi.collapseFile?.(p);
    } else {
      collapsedDiffFiles.delete(p);
      changesDiffApi.expandFile?.(p);
    }
  }
  bump();
}

function setScope(next: DiffScope) {
  if (diffScope() === next) return;
  diffScope.set(next);
  const cur = gitDiff();
  if (cur) {
    if (cur.aggregate) openAllDiffs(cur.root);
    else openDiff(cur.root, cur.path, cur.untracked, cur.oldPath);
  }
}

/** 选中行在各自文件里的行号（DiffView 给的是拼起来的行）。 */
async function commentFileLines(range: CommentRange) {
  const [start, end] = await Promise.all([
    changesDiffApi?.fileLine?.(range.side, range.start),
    changesDiffApi?.fileLine?.(range.side, range.end),
  ]);
  return {
    path: String(start?.path ?? ""),
    start: Number(start?.number ?? range.start + 1),
    end: Number(end?.number ?? range.end + 1),
  };
}

async function sendDiffComment() {
  const range = commentRange();
  const cur = gitDiff();
  if (!range || !cur) return;
  try {
    const [selected, lines] = await Promise.all([
      changesDiffApi?.selectedText?.(),
      commentFileLines(range),
    ]);
    const headerText = t("comment.feedback_header", { path: lines.path || cur.path });
    const msg = diffFeedbackText(
      headerText,
      lines.start,
      lines.end,
      String(selected ?? ""),
      commentText().trim(),
    );
    await run(SEND_TEXT_CMD, { text: msg });
    clearComment();
  } catch (err: any) {
    notifyError(err?.message ?? String(err));
  }
}

// ---- 日志页 ----

function ensureLog(repo: ProjectRef) {
  const k = projectKey(repo.machine, repo.root);
  if (logLoading() || logLoadedRootKey === k) return;
  reloadLog(repo);
}

function reloadLog(repo: ProjectRef) {
  const k = projectKey(repo.machine, repo.root);
  logLoadedRootKey = k;
  logLoading.set(true);
  const scope = logScope();

  void call(repo.machine, "git.log", { root: repo.root, scope, limit: LOG_LIMIT })
    .then((res: any) => {
      if (logLoadedRootKey !== k) return;
      const commits: CommitNode[] = Array.isArray(res?.commits) ? res.commits : [];
      logCommits.set(commits);
      logGraphRows.set(layoutGraph(commits));
      logLoading.set(false);
      logSelectedHash.set(null);
      logDetailFiles.set([]);
      logDetailFile.set(null);
      setLogDetail(null, null);
    })
    .catch(() => {
      if (logLoadedRootKey !== k) return;
      logCommits.set([]);
      logGraphRows.set([]);
      logLoading.set(false);
    });
}

function setLogScope(repo: ProjectRef, next: LogScope) {
  if (JSON.stringify(logScope()) === JSON.stringify(next)) return;
  logScope.set(next);
  reloadLog(repo);
}

function selectCommit(repo: ProjectRef, hash: string) {
  logSelectedHash.set(hash);
  logDetailLoading.set(true);
  logDetailFiles.set([]);
  logDetailFile.set(null);
  setLogDetail(null, null);

  void call(repo.machine, "git.commit.files", { root: repo.root, hash })
    .then((res: any) => {
      if (logSelectedHash() !== hash) return;
      logDetailFiles.set(Array.isArray(res?.files) ? res.files : []);
      logDetailLoading.set(false);
    })
    .catch(() => {
      if (logSelectedHash() !== hash) return;
      logDetailFiles.set([]);
      logDetailLoading.set(false);
    });
}

function selectCommitFile(repo: ProjectRef, path: string) {
  const hash = logSelectedHash();
  if (!hash) return;
  logDetailFile.set(path);
  setLogDetail(null, null);

  void callWithBlobs(repo.machine, "git.commit.file_diff", { root: repo.root, hash, path }, ["/text"])
    .then((res: any) => {
      if (logSelectedHash() !== hash || logDetailFile() !== path) return;
      const shown = diffShown(res, t("log.no_diff"));
      setLogDetail(shown.url, shown.message);
    })
    .catch((err: any) => {
      if (logSelectedHash() !== hash || logDetailFile() !== path) return;
      setLogDetail(null, t("log.show_failed", { error: err?.message ?? String(err) }));
    });
}

/** 换掉提交里那个文件的补丁，放掉旧的 Blob。 */
function setLogDetail(url: string | null, message: string | null) {
  const old = logDetailUrl();
  if (old && old !== url) URL.revokeObjectURL(old);
  logDetailUrl.set(url);
  logDetailMessage.set(message);
}

// ---- 切换项目与刷新 ----

function syncProjectFromContext() {
  const ctx = activeContext()?.project;
  const next: ProjectRef | null =
    ctx && typeof ctx.root === "string" && ctx.root.length > 0
      ? { machine: ctx.machine ?? null, root: ctx.root }
      : null;
  const cur = currentProject();
  if (sameProject(cur, next)) return;

  if (cur) {
    parked.set(projectKey(cur.machine, cur.root), {
      tab: gitTab(),
      diff: gitDiff(),
      diffSplit: diffSplit(),
      diffScope: diffScope(),
      pendingDiffFile,
      collapsedDiffFiles: new Set(collapsedDiffFiles),
      treeCollapsed: new Set(treeCollapsed),
    });
  }

  currentProject.set(next);
  activeRepoOverride.set(null);
  clearComment();
  diffGen += 1;

  if (next) {
    const ui = parked.get(projectKey(next.machine, next.root));
    if (ui) {
      parked.delete(projectKey(next.machine, next.root));
      gitTab.set(ui.tab);
      diffSplit.set(ui.diffSplit);
      diffScope.set(ui.diffScope);
      pendingDiffFile = ui.pendingDiffFile;
      collapsedDiffFiles.clear();
      for (const p of ui.collapsedDiffFiles) collapsedDiffFiles.add(p);
      treeCollapsed.clear();
      for (const p of ui.treeCollapsed) treeCollapsed.add(p);
      gitDiff.set(ui.diff?.loaded ? ui.diff : null);
    } else {
      gitTab.set("changes");
      diffSplit.set(false);
      diffScope.set("all");
      pendingDiffFile = null;
      collapsedDiffFiles.clear();
      treeCollapsed.clear();
      gitDiff.set(null);
    }
  } else {
    gitDiff.set(null);
    pendingDiffFile = null;
  }

  bump();
  refreshVisible();
}

function preparePanel() {
  const proj = currentProject();
  if (!proj || !viewShown(VIEW_ID)) return;
  const active = activeRepoFor(proj);
  const cur = gitDiff();
  if (cur && !sameProject(cur.root, active)) {
    resetDiffView();
  }
  if (gitTab() === "changes") {
    const st = statusCache.get(projectKey(active.machine, active.root))?.value ?? null;
    const hasChanges = Boolean(st && st.ok && st.files.length > 0);
    if (shouldCloseAggregateDiff(st, Boolean(gitDiff()?.aggregate))) {
      resetDiffView();
    } else if (gitDiff() == null && hasChanges) {
      openAllDiffs(active);
    }
    if (pendingDiffFile) {
      queueMicrotask(() => flushPendingDiffFile());
    }
  } else {
    ensureLog(active);
  }
}

function refreshVisible() {
  const proj = currentProject();
  if (!proj) {
    syncBadge();
    return;
  }
  ensureWatch(proj);
  discoverRepos(proj);
  for (const r of projectRepoRoots(proj)) {
    ensureStatus(r);
  }
  if (!viewShown(VIEW_ID)) return;
  if (windowActive()) {
    scheduleAutofetch();
  }
  const active = activeRepoFor(proj);
  ensureBranches(active);
  preparePanel();
}

// ---- 启动注册与事件监听 ----

bootTask(async () => {
  await initHost(["window_active", "active_context", "machines_meta"]);
  await registerViews([
    {
      name: "main",
      title: text("title"),
      icon: "icons/git-branch.svg",
      export: "default",
    },
  ]);
  await registerCommands(
    [
      {
        name: "prev-hunk",
        title: text("command.prev_hunk"),
        key: "shift-f7",
        context: "Workspace",
        when_view_shown: VIEW_ID,
        requires_args: false,
        args: [],
      },
      {
        name: "next-hunk",
        title: text("command.next_hunk"),
        key: "f7",
        context: "Workspace",
        when_view_shown: VIEW_ID,
        requires_args: false,
        args: [],
      },
      {
        name: "open-pull-request",
        title: text("command.open_pull_request"),
        context: "Workspace",
        requires_args: true,
        args: ["machine", "root"],
      },
    ],
    text("title"),
  );
  syncProjectFromContext();
  refreshVisible();
});

onCommand(async ({ name, args }) => {
  if (name === "prev-hunk") {
    jumpHunk(false);
    return null;
  }
  if (name === "next-hunk") {
    jumpHunk(true);
    return null;
  }
  if (name === "open-pull-request") {
    const root = typeof args?.root === "string" && args.root.length > 0 ? args.root : null;
    if (!root) {
      throw new Error(t("error.pull_request_args", { error: "missing root" }));
    }
    const machine = typeof args?.machine === "string" ? args.machine : null;
    const info = await call(machine, "git.repo_info", { root });
    const res = pullRequestLink(info, t);
    if (!res.ok || !res.url) {
      throw new Error(res.error ?? t("pull_request.not_repo"));
    }
    await openUrl(res.url);
    return null;
  }
  throw new Error(`unknown command: ${name}`);
});

onSlices(["window_active", "active_context"], () => {
  lastWindowActive = windowActive();
  syncProjectFromContext();
});

onEvent(
  (event) => {
    if (!event || typeof event !== "object") return;
    if (event.type === "project_changed") {
      const machine = event.machine ?? null;
      const root = typeof event.root === "string" ? event.root : "";
      if (!root || !watches.has(projectKey(machine, root))) return;
      const st = pathStyle(machine);
      markCachesStaleWhere((m, r) => m === machine && pathStartsWith(st, r, root));
      refreshVisible();
      return;
    }
    if (event.type === "reconnected") {
      const machine = event.machine ?? null;
      markCachesStaleWhere((m) => m === machine);
      for (const [k, c] of repoCache.entries()) {
        const [m] = splitProjectKey(k);
        if (m === machine) c.fetchedAt = 0;
      }
      for (const [k, timer] of [...repoThrottleTimers.entries()]) {
        const [m] = splitProjectKey(k);
        if (m === machine) {
          clearTimeout(timer);
          repoThrottleTimers.delete(k);
          repoInflight.delete(k);
        }
      }
      for (const k of [...statusFailures.keys()]) {
        const [m] = splitProjectKey(k);
        if (m === machine) statusFailures.delete(k);
      }
      for (const [k, timer] of [...statusRetryTimers.entries()]) {
        const [m] = splitProjectKey(k);
        if (m === machine) {
          clearTimeout(timer);
          statusRetryTimers.delete(k);
          statusInflight.delete(k);
        }
      }
      for (const k of [...watches]) {
        const [m] = splitProjectKey(k);
        if (m === machine) watches.delete(k);
      }
      for (const k of [...failedWatches]) {
        const [m] = splitProjectKey(k);
        if (m === machine) failedWatches.delete(k);
      }
      refreshVisible();
      return;
    }
    if (event.type === "window_activated") {
      failedWatches.clear();
      markCachesStaleWhere(() => true);
      refreshVisible();
      return;
    }
    if (event.type === "active_context_changed") {
      syncProjectFromContext();
      return;
    }
    if (event.type === "shown_views_changed" || event.type === "machines_changed") {
      refreshVisible();
    }
  },
  [
    "project_changed",
    "reconnected",
    "window_activated",
    "active_context_changed",
    "shown_views_changed",
    "machines_changed",
  ],
);

// ---- 主题与共用组件 ----


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

function Placeholder(props: { text: string; subtext?: string }) {
  const look = currentTheme;
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
        gap: 4,
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
      <If when={Boolean(props.subtext)}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.faint,
            textAlign: "center",
          }}
        >
          {props.subtext ?? ""}
        </Text>
      </If>
    </View>
  );
}

function scopeLabel(scope: DiffScope): string {
  if (scope === "staged") return t("scope.staged");
  if (scope === "unstaged") return t("scope.unstaged");
  return t("scope.all");
}

function busyLabel(op: string): string {
  const key = op === "index_file" ? "busy.stage" : `busy.${op}`;
  return t(key);
}

function OpsMenu(props: {
  repo: ProjectRef;
  ahead: number;
  behind: number;
  stashCount: number;
  hasChanges: boolean;
  upstream: boolean;
  triggerLabel: string;
  triggerColor: string;
}) {
  const look = currentTheme;
  const runOn = (fn: (repo: ProjectRef) => void) => {
    setActiveRepo(props.repo);
    fn(props.repo);
  };
  return (
    <SmeltMenu
      placement="bottom"
      align="end"
      popupStyle={{ minWidth: 168 }}
      trigger={
        <View
          class="rounded hover:bg-accent"
          style={{
            paddingLeft: 6,
            paddingRight: 6,
            height: 22,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 3,
            cursor: "pointer",
            flexShrink: 0,
          }}
        >
          <Text
            style={{
              fontFamily: look().fonts?.mono,
              fontSize: look().type.text_xsmall.size,
              color: props.triggerColor,
            }}
          >
            {props.triggerLabel}
          </Text>
        </View>
      }
    >
      <If when={props.ahead > 0}>
        <MenuRow label={t("ops.push", { ahead: props.ahead })} onSelect={() => runOn(runPushOnly)} />
      </If>
      <If when={props.ahead === 0 && props.upstream}>
        <MenuRow
          label={t("command.open_pull_request")}
          onSelect={() => runOn((r) => void openPullRequest(r))}
        />
      </If>
      <MenuRow label={t("ops.fetch")} onSelect={() => runOn((r) => runFetch(r, false))} />
      <MenuRow
        label={props.behind > 0 ? t("ops.pull_behind", { behind: props.behind }) : t("ops.pull")}
        onSelect={() => runOn(runPull)}
      />
      <If when={props.hasChanges}>
        <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />
        <MenuRow label={t("ops.stash")} onSelect={() => runOn(runStashPush)} />
      </If>
      <If when={props.stashCount > 0}>
        <MenuRow
          label={t("ops.stash_pop", { count: props.stashCount })}
          onSelect={() => runOn(runStashPop)}
        />
      </If>
      <If when={props.hasChanges}>
        <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />
        <MenuRow
          label={t("discard.all_menu")}
          iconPath="icons/trash.svg"
          danger={true}
          onSelect={() => runOn(startDiscardAll)}
        />
      </If>
    </SmeltMenu>
  );
}

// ---- 主视图 ----

export function Main() {
  const look = currentTheme;
  const isMac = localOs() === "darwin";

  const activeRepo = computed(() => {
    stateVersion();
    const proj = currentProject();
    return proj ? activeRepoFor(proj) : null;
  });

  const repos = computed(() => {
    stateVersion();
    const proj = currentProject();
    if (!proj) return [];
    return projectRepoList(proj);
  });

  const reposTruncated = computed(() => {
    stateVersion();
    const proj = currentProject();
    if (!proj) return false;
    return Boolean(repoCache.get(projectKey(proj.machine, proj.root))?.value.truncated);
  });

  const activeStatus = computed(() => {
    stateVersion();
    const repo = activeRepo();
    if (!repo) return null;
    return statusCache.get(projectKey(repo.machine, repo.root))?.value ?? null;
  });

  const activeBusy = computed(() => {
    stateVersion();
    const repo = activeRepo();
    if (!repo) return null;
    return gitWrites.get(projectKey(repo.machine, repo.root)) ?? null;
  });

  const activeBranches = computed(() => {
    stateVersion();
    const repo = activeRepo();
    if (!repo) return { local: [], remote: [] };
    return branchCache.get(projectKey(repo.machine, repo.root))?.value ?? { local: [], remote: [] };
  });

  const repoLabelOf = (repo: DiscoveredRepo): string => {
    const proj = currentProject();
    if (!proj) return repo.root;
    if (!repo.rel_path) {
      return pathFileName(pathStyle(proj.machine), proj.root) ?? proj.root;
    }
    return repo.rel_path;
  };

  // 构建右侧更改树（扁平化行数组）
  const changeListModel = computed(() => {
    stateVersion();
    const proj = currentProject();
    const active = activeRepo();
    if (!proj || !active) return { rows: [] as any[], totalChanges: 0 };
    const list = repos();
    const showHeaders = list.length > 1;
    const rootGitlinks = new Set(list.filter((r) => r.kind === "submodule").map((r) => r.rel_path));
    const rows: any[] = [];
    let totalChanges = 0;
    let anythingToDo = false;

    for (const repo of list) {
      const repoRef: ProjectRef = { machine: proj.machine, root: repo.root };
      const rk = projectKey(proj.machine, repo.root);
      const st = statusCache.get(rk)?.value ?? null;
      const { staged, changed } = st ? splitStagedAndChanged(st.files) : { staged: [], changed: [] };
      totalChanges += staged.length + changed.length;
      const isActive = sameProject(repoRef, active);
      const hasChanges = staged.length > 0 || changed.length > 0;
      if (st && repoNeedsAttention(hasChanges, st.ahead, st.behind, st.stash_count)) {
        anythingToDo = true;
      }
      const collapsed = repoCollapsed.has(rk);
      if (showHeaders) {
        rows.push({
          kind: "repo_header",
          key: `repo:${repo.root}`,
          repo: repoRef,
          label: repoLabelOf(repo),
          branch: st?.branch || "…",
          repoKind: repo.kind,
          active: isActive,
          collapsed,
          ahead: st?.ahead ?? 0,
          behind: st?.behind ?? 0,
          stashCount: st?.stash_count ?? 0,
          hasChanges,
          upstream: Boolean(st?.upstream),
        });
      }
      if (showHeaders && collapsed) continue;

      if (hasChanges || isActive) {
        rows.push({
          kind: "commit_box",
          key: `commit:${repo.root}`,
          repo: repoRef,
          repoKey: rk,
          branch: st?.branch ?? "",
          hasStaged: staged.length > 0,
          writeOp: gitWrites.get(rk) ?? null,
          ahead: st?.ahead ?? 0,
          error: commitErrors.get(rk) ?? null,
          depth: showHeaders ? 1 : 0,
        });
      }

      const depth = showHeaders ? 1 : 0;
      const gitlinks = !repo.rel_path ? rootGitlinks : new Set<string>();
      const pushGroup = (groupKey: string, title: string, items: Array<[string, string]>, isStaged: boolean) => {
        rows.push({
          kind: "group_header",
          key: `hdr:${groupKey}:${repo.root}`,
          repo: repoRef,
          title,
          depth,
          staged: isStaged,
          paths: items.map(([, p]) => p),
        });
        for (const tRow of buildGitTree(items, treeCollapsed)) {
          rows.push({
            kind: tRow.status == null ? "tree_dir" : "tree_file",
            key: `tree:${groupKey}:${repo.root}:${tRow.path}`,
            repo: repoRef,
            repoKey: rk,
            isStaged,
            depth: depth + tRow.depth,
            name: tRow.name,
            path: tRow.path,
            status: tRow.status,
            collapsed: tRow.status == null && treeCollapsed.has(tRow.path),
            gitlink: gitlinks.has(tRow.path),
            busy:
              tRow.status != null &&
              gitIndexOpInFlight(indexPending.get(`${rk}\0${tRow.path}`) ?? null),
          });
        }
      };
      if (staged.length > 0) {
        pushGroup("staged", t("changes.staged", { count: staged.length }), staged, true);
      }
      if (changed.length > 0) {
        pushGroup("changed", t("changes.unstaged", { count: changed.length }), changed, false);
      }
    }

    if (totalChanges === 0 && !anythingToDo) {
      return { rows: [], totalChanges: 0 };
    }
    return { rows, totalChanges };
  });

  const emptyListKind = computed(() => {
    stateVersion();
    const repo = activeRepo();
    if (!repo) return "loading";
    const rk = projectKey(repo.machine, repo.root);
    return gitEmptyListKind(
      statusCache.get(rk)?.value ?? null,
      statusInflight.has(rk),
      statusFailures.get(rk) ?? 0,
    );
  });

  const annotationsList = computed(() => {
    const r = commentRange();
    if (!commentOpen() || !r) return [];
    return [{ id: "diff-comment", side: r.side, line: r.end }];
  });

  // 选中行在文件里的行号要问 DiffView（异步），到了再填标题。
  const commentLines = signal<{ start: number; end: number } | null>(null);
  effect(() => {
    const r = commentRange();
    commentLines.set(null);
    if (!r) return;
    void commentFileLines(r).then((lines) => {
      if (commentRange() === r) commentLines.set(lines);
    });
  });
  const commentRangeTitle = computed(() => {
    const l = commentLines();
    if (!l) return "";
    const lo = Math.min(l.start, l.end);
    const hi = Math.max(l.start, l.end);
    return lo === hi
      ? t("comment.range_one", { line: lo })
      : t("comment.range", { start: lo, end: hi });
  });

  const allAggregateCollapsed = computed(() => {
    stateVersion();
    const cur = gitDiff();
    const paths = cur?.aggregate ? cur.entries.filter((e) => e.in_patch).map((e) => e.path) : [];
    return paths.length > 0 && paths.every((p) => collapsedDiffFiles.has(p));
  });

  const diffLeftOut = computed(() => (gitDiff()?.entries ?? []).filter((e) => !e.in_patch));

  const logGraphW = computed(() => graphWidth(logGraphRows()));
  const logSummary = computed(() => {
    const sc = logScope();
    const head = activeStatus()?.branch || null;
    const scopeName =
      sc === "head"
        ? head
          ? t("log.head_named", { branch: head })
          : t("log.head")
        : sc === "all"
          ? t("log.all_branches")
          : sc.branch;
    return t("log.summary", { scope: scopeName, count: logCommits().length });
  });

  const selectedCommitObj = computed(() => {
    const h = logSelectedHash();
    if (!h) return null;
    return logCommits().find((c) => c.hash === h) ?? null;
  });

  const isShown = computed(() => viewShown(VIEW_ID));
  effect(() => {
    if (isShown()) {
      refreshVisible();
    }
  });

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
      <If
        when={currentProject() != null}
        else={<Placeholder text={t("no_project.title")} subtext={t("no_project.hint")} />}
      >
        {/* 顶栏：更改 / 历史 分段切换 + 作用范围 / 仓库选择 / 同步菜单 */}
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
              gap: 2,
              padding: 2,
              borderRadius: look().radius.md,
              backgroundColor: look().palette.soft,
            }}
          >
            <SmeltButton
              quiet={true}
              small={true}
              selected={gitTab() === "changes"}
              label={t("tab.changes")}
              onPress={() => {
                if (gitTab() === "changes") return;
                gitTab.set("changes");
                resetDiffView();
                refreshVisible();
              }}
            />
            <SmeltButton
              quiet={true}
              small={true}
              selected={gitTab() === "log"}
              label={t("tab.history")}
              onPress={() => {
                if (gitTab() === "log") return;
                gitTab.set("log");
                refreshVisible();
              }}
            />
          </View>

          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6 }}>
            <If when={gitTab() === "changes"}>
              <SmeltMenu
                placement="bottom"
                align="end"
                popupStyle={{ minWidth: 140 }}
                trigger={
                  <View
                    class="rounded hover:bg-accent"
                    style={{
                      paddingLeft: 8,
                      paddingRight: 6,
                      height: 24,
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 4,
                      cursor: "pointer",
                    }}
                  >
                    <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                      {scopeLabel(diffScope())}
                    </Text>
                    <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
                  </View>
                }
              >
                <MenuRow label={t("scope.all")} onSelect={() => setScope("all")} />
                <MenuRow label={t("scope.staged")} onSelect={() => setScope("staged")} />
                <MenuRow label={t("scope.unstaged")} onSelect={() => setScope("unstaged")} />
              </SmeltMenu>
            </If>

            <If when={gitTab() === "log" && repos().length > 1}>
              <SmeltMenu
                placement="bottom"
                align="end"
                popupStyle={{ minWidth: 168 }}
                trigger={
                  <View
                    class="rounded hover:bg-accent"
                    style={{
                      paddingLeft: 8,
                      paddingRight: 6,
                      height: 24,
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 4,
                      cursor: "pointer",
                    }}
                  >
                    <Text style={{ fontSize: look().type.small.size, color: look().palette.text }}>
                      {(() => {
                        const ar = activeRepo();
                        const found = repos().find((r) => r.root === ar?.root);
                        return found ? repoLabelOf(found) : (ar?.root ?? "");
                      })()}
                    </Text>
                    <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
                  </View>
                }
              >
                <Each of={repos()} key={(r: DiscoveredRepo) => r.root}>
                  {(r: () => DiscoveredRepo) => (
                    <MenuRow
                      label={repoLabelOf(r())}
                      onSelect={() => {
                        const proj = currentProject();
                        if (proj) setActiveRepo({ machine: proj.machine, root: r().root });
                      }}
                    />
                  )}
                </Each>
              </SmeltMenu>
            </If>

            <If when={gitTab() === "log" && activeRepo() != null}>
              <OpsMenu
                repo={activeRepo()!}
                ahead={activeStatus()?.ahead ?? 0}
                behind={activeStatus()?.behind ?? 0}
                stashCount={activeStatus()?.stash_count ?? 0}
                hasChanges={(activeStatus()?.files.length ?? 0) > 0}
                upstream={Boolean(activeStatus()?.upstream)}
                triggerLabel={
                  activeBusy()
                    ? busyLabel(activeBusy()!)
                    : `↑${activeStatus()?.ahead ?? 0} ↓${activeStatus()?.behind ?? 0}`
                }
                triggerColor={
                  activeBusy() || (activeStatus()?.ahead ?? 0) + (activeStatus()?.behind ?? 0) > 0
                    ? look().palette.text
                    : look().palette.faint
                }
              />
            </If>
          </View>
        </View>

        {/* 主体：更改页 OR 历史页 */}
        <If
          when={gitTab() === "changes"}
          else={
            /* ---- 历史页（三栏 ResizablePanelGroup） ---- */
            <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "row" }}>
              <ResizablePanelGroup
                axis="horizontal"
                handleClass="bg-border group-hover:bg-muted-foreground/50 group-active:bg-muted-foreground/70"
              >
                {/* 左栏：分支树 */}
                <ResizablePanel size={190} minSize={130} maxSize={320}>
                  <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
                    <View style={{ display: "flex", flexDirection: "column", padding: 6, gap: 2 }}>
                      <View
                        class="rounded hover:bg-accent"
                        onPress={() => activeRepo() && setLogScope(activeRepo()!, "head")}
                        style={{
                          paddingLeft: 8,
                          paddingRight: 8,
                          height: 24,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          backgroundColor:
                            logScope() === "head" ? look().palette.row_selected : "transparent",
                          cursor: "pointer",
                        }}
                      >
                        <Text
                          style={{
                            fontSize: look().type.small.size,
                            color: look().palette.text,
                            numberOfLines: 1,
                          }}
                        >
                          {activeStatus()?.branch
                            ? t("log.head_named", { branch: activeStatus()!.branch })
                            : t("log.head")}
                        </Text>
                      </View>
                      <View
                        class="rounded hover:bg-accent"
                        onPress={() => activeRepo() && setLogScope(activeRepo()!, "all")}
                        style={{
                          paddingLeft: 8,
                          paddingRight: 8,
                          height: 24,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          backgroundColor:
                            logScope() === "all" ? look().palette.row_selected : "transparent",
                          cursor: "pointer",
                        }}
                      >
                        <Text
                          style={{
                            fontSize: look().type.small.size,
                            color: look().palette.text,
                            numberOfLines: 1,
                          }}
                        >
                          {t("log.all_branches")}
                        </Text>
                      </View>

                      {/* 本地分支 */}
                      <If when={activeBranches().local.length > 0}>
                        <Text
                          style={{
                            paddingLeft: 8,
                            paddingTop: 6,
                            paddingBottom: 2,
                            fontSize: look().type.text_xsmall.size,
                            color: look().palette.muted,
                          }}
                        >
                          {t("branch.local")}
                        </Text>
                        <Each of={activeBranches().local} key={(b: string) => `local:${b}`}>
                          {(b: () => string) => {
                            const isHead = () => activeStatus()?.branch === b();
                            const selected = () => {
                              const sc = logScope();
                              return typeof sc === "object" && sc.branch === b();
                            };
                            return (
                              <SmeltContextMenu
                                popupStyle={{ minWidth: 168 }}
                                disabled={isHead()}
                                trigger={
                                  <View
                                    class="rounded hover:bg-accent"
                                    onPress={() =>
                                      activeRepo() && setLogScope(activeRepo()!, { branch: b() })
                                    }
                                    style={{
                                      width: "100%",
                                      paddingLeft: 12,
                                      paddingRight: 8,
                                      height: 24,
                                      display: "flex",
                                      flexDirection: "row",
                                      alignItems: "center",
                                      gap: 4,
                                      backgroundColor: selected()
                                        ? look().palette.row_selected
                                        : "transparent",
                                      cursor: "pointer",
                                    }}
                                  >
                                    <Text
                                      style={{
                                        width: 8,
                                        fontSize: look().type.text_xsmall.size,
                                        color: look().palette.ok,
                                      }}
                                    >
                                      {isHead() ? "●" : ""}
                                    </Text>
                                    <Text
                                      style={{
                                        flex: 1,
                                        minWidth: 0,
                                        fontSize: look().type.small.size,
                                        color: look().palette.text,
                                        numberOfLines: 1,
                                      }}
                                    >
                                      {b()}
                                    </Text>
                                  </View>
                                }
                              >
                                <MenuRow
                                  label={t("branch.checkout")}
                                  onSelect={() => activeRepo() && runCheckout(activeRepo()!, b())}
                                />
                                <MenuRow
                                  label={t("branch.merge")}
                                  onSelect={() => activeRepo() && runMerge(activeRepo()!, b())}
                                />
                                <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />
                                <MenuRow
                                  label={t("branch.delete")}
                                  iconPath="icons/trash.svg"
                                  danger={true}
                                  onSelect={() =>
                                    activeRepo() && startDeleteBranch(activeRepo()!, b(), false)
                                  }
                                />
                              </SmeltContextMenu>
                            );
                          }}
                        </Each>
                      </If>

                      {/* 远程分支 */}
                      <If when={activeBranches().remote.length > 0}>
                        <Text
                          style={{
                            paddingLeft: 8,
                            paddingTop: 6,
                            paddingBottom: 2,
                            fontSize: look().type.text_xsmall.size,
                            color: look().palette.muted,
                          }}
                        >
                          {t("branch.remote")}
                        </Text>
                        <Each of={activeBranches().remote} key={(b: string) => `remote:${b}`}>
                          {(b: () => string) => {
                            const selected = () => {
                              const sc = logScope();
                              return typeof sc === "object" && sc.branch === b();
                            };
                            return (
                              <SmeltContextMenu
                                popupStyle={{ minWidth: 168 }}
                                trigger={
                                  <View
                                    class="rounded hover:bg-accent"
                                    onPress={() =>
                                      activeRepo() && setLogScope(activeRepo()!, { branch: b() })
                                    }
                                    style={{
                                      width: "100%",
                                      paddingLeft: 12,
                                      paddingRight: 8,
                                      height: 24,
                                      display: "flex",
                                      flexDirection: "row",
                                      alignItems: "center",
                                      gap: 4,
                                      backgroundColor: selected()
                                        ? look().palette.row_selected
                                        : "transparent",
                                      cursor: "pointer",
                                    }}
                                  >
                                    <View style={{ width: 8 }} />
                                    <Text
                                      style={{
                                        flex: 1,
                                        minWidth: 0,
                                        fontSize: look().type.small.size,
                                        color: look().palette.muted,
                                        numberOfLines: 1,
                                      }}
                                    >
                                      {b()}
                                    </Text>
                                  </View>
                                }
                              >
                                <MenuRow
                                  label={t("branch.checkout")}
                                  onSelect={() => {
                                    const repo = activeRepo();
                                    if (!repo) return;
                                    const slash = b().indexOf("/");
                                    const short = slash >= 0 ? b().slice(slash + 1) : b();
                                    runCheckout(repo, short);
                                  }}
                                />
                                <MenuRow
                                  label={t("branch.merge")}
                                  onSelect={() => activeRepo() && runMerge(activeRepo()!, b())}
                                />
                                <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />
                                <MenuRow
                                  label={t("branch.delete_remote")}
                                  iconPath="icons/trash.svg"
                                  danger={true}
                                  onSelect={() =>
                                    activeRepo() && startDeleteBranch(activeRepo()!, b(), true)
                                  }
                                />
                              </SmeltContextMenu>
                            );
                          }}
                        </Each>
                      </If>
                    </View>
                  </Scroll>
                </ResizablePanel>

                {/* 中栏：提交列表 + 矢量分支图 */}
                <ResizablePanel minSize={200}>
                  <View
                    style={{
                      flex: 1,
                      width: "100%",
                      height: "100%",
                      minWidth: 0,
                      minHeight: 0,
                      display: "flex",
                      flexDirection: "column",
                    }}
                  >
                    <View
                      style={{
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        justifyContent: "space-between",
                        gap: 8,
                        paddingLeft: 12,
                        paddingRight: 12,
                        paddingTop: 4,
                        paddingBottom: 4,
                        borderBottomWidth: 1,
                        borderBottomColor: look().palette.hair,
                        flexShrink: 0,
                      }}
                    >
                      <Text
                        style={{
                          flex: 1,
                          minWidth: 0,
                          fontSize: look().type.small.size,
                          color: look().palette.muted,
                          numberOfLines: 1,
                        }}
                      >
                        {logSummary()}
                      </Text>
                      <SmeltButton
                        quiet={true}
                        small={true}
                        label={t("log.reload")}
                        onPress={() => activeRepo() && reloadLog(activeRepo()!)}
                      />
                    </View>

                    <If
                      when={logCommits().length > 0}
                      else={<Placeholder text={logLoading() ? t("log.loading") : t("log.empty")} />}
                    >
                      <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
                        <View style={{ display: "flex", flexDirection: "column" }}>
                          <Each
                            of={logCommits().map((commit, ix) => ({
                              commit,
                              graph: logGraphRows()[ix],
                            }))}
                            key={(item: any) => item.commit.hash}
                          >
                            {(item: () => { commit: CommitNode; graph: any }) => {
                              const sel = () => logSelectedHash() === item().commit.hash;
                              return (
                                <View
                                  class="hover:bg-accent"
                                  onPress={() =>
                                    activeRepo() && selectCommit(activeRepo()!, item().commit.hash)
                                  }
                                  style={{
                                    display: "flex",
                                    flexDirection: "row",
                                    alignItems: "center",
                                    height: GRAPH_ROW_H,
                                    paddingRight: 12,
                                    gap: 8,
                                    backgroundColor: sel()
                                      ? look().palette.row_selected
                                      : "transparent",
                                    cursor: "pointer",
                                    overflow: "hidden",
                                  }}
                                >
                                  <If when={item().graph != null}>
                                    <SmeltVector
                                      markup={commitGraphRowSvg(
                                        item().graph,
                                        logGraphW(),
                                        GRAPH_ROW_H,
                                        look().palette.chart,
                                      )}
                                      width={logGraphW()}
                                      height={GRAPH_ROW_H}
                                    />
                                  </If>
                                  <Each
                                    of={(item().commit.refs ?? []).slice(0, 2)}
                                    key={(r: string) => r}
                                  >
                                    {(r: () => string) => (
                                      <View
                                        style={{
                                          paddingLeft: 5,
                                          paddingRight: 5,
                                          height: 18,
                                          borderRadius: 4,
                                          backgroundColor: look().palette.soft,
                                          display: "flex",
                                          alignItems: "center",
                                          justifyContent: "center",
                                          maxWidth: 110,
                                          flexShrink: 0,
                                          overflow: "hidden",
                                        }}
                                      >
                                        <Text
                                          style={{
                                            fontSize: look().type.text_xsmall.size,
                                            color: look().palette.text,
                                            numberOfLines: 1,
                                          }}
                                        >
                                          {r()}
                                        </Text>
                                      </View>
                                    )}
                                  </Each>
                                  <Text
                                    style={{
                                      flex: 1,
                                      minWidth: 0,
                                      fontSize: look().type.ui.size,
                                      color: look().palette.text,
                                      numberOfLines: 1,
                                    }}
                                  >
                                    {item().commit.subject}
                                  </Text>
                                  <Text
                                    style={{
                                      width: 96,
                                      flexShrink: 0,
                                      fontSize: look().type.text_xsmall.size,
                                      color: look().palette.muted,
                                      numberOfLines: 1,
                                    }}
                                  >
                                    {item().commit.author}
                                  </Text>
                                  <Text
                                    style={{
                                      width: 88,
                                      flexShrink: 0,
                                      textAlign: "right",
                                      fontSize: look().type.text_xsmall.size,
                                      color: look().palette.muted,
                                      numberOfLines: 1,
                                    }}
                                  >
                                    {formatCommitTime(item().commit.time)}
                                  </Text>
                                </View>
                              );
                            }}
                          </Each>
                        </View>
                      </Scroll>
                    </If>
                  </View>
                </ResizablePanel>

                {/* 右栏：提交详情 & 单文件 Diff */}
                <ResizablePanel size={360} minSize={220}>
                  <View
                    style={{
                      flex: 1,
                      width: "100%",
                      height: "100%",
                      minWidth: 0,
                      minHeight: 0,
                      display: "flex",
                      flexDirection: "column",
                    }}
                  >
                    <If
                      when={selectedCommitObj() != null}
                      else={<Placeholder text={t("log.pick_commit")} />}
                    >
                      <View
                        style={{
                          paddingLeft: 12,
                          paddingRight: 12,
                          paddingTop: 8,
                          paddingBottom: 8,
                          gap: 4,
                          borderBottomWidth: 1,
                          borderBottomColor: look().palette.hair,
                          flexShrink: 0,
                        }}
                      >
                        <Text
                          style={{
                            fontSize: look().type.ui.size,
                            color: look().palette.text,
                            numberOfLines: 1,
                          }}
                        >
                          {selectedCommitObj()?.subject ?? ""}
                        </Text>
                        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                          <Text
                            style={{
                              flex: 1,
                              minWidth: 0,
                              fontSize: look().type.text_xsmall.size,
                              color: look().palette.muted,
                              numberOfLines: 1,
                            }}
                          >
                            {selectedCommitObj()?.author ?? ""}
                          </Text>
                          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                            {formatCommitTime(selectedCommitObj()?.time ?? 0)}
                          </Text>
                          <Text
                            style={{
                              fontFamily: look().fonts?.mono,
                              fontSize: look().type.text_xsmall.size,
                              color: look().palette.muted,
                            }}
                          >
                            {selectedCommitObj()?.short ?? ""}
                          </Text>
                        </View>
                      </View>

                      <If
                        when={!logDetailLoading()}
                        else={<Placeholder text={t("log.loading_commit")} />}
                      >
                        <If
                          when={logDetailFiles().length > 0}
                          else={<Placeholder text={t("log.no_files")} />}
                        >
                          <Scroll
                            style={{
                              maxHeight: 160,
                              flexShrink: 0,
                              borderBottomWidth: 1,
                              borderBottomColor: look().palette.hair,
                            }}
                          >
                            <View style={{ padding: 4, display: "flex", flexDirection: "column" }}>
                              <Each
                                of={logDetailFiles().map(([st, p]) => ({ st, path: p }))}
                                key={(f: any) => f.path}
                              >
                                {(f: () => { st: string; path: string }) => (
                                  <View
                                    class="rounded hover:bg-accent"
                                    onPress={() =>
                                      activeRepo() && selectCommitFile(activeRepo()!, f().path)
                                    }
                                    style={{
                                      display: "flex",
                                      flexDirection: "row",
                                      alignItems: "center",
                                      gap: 8,
                                      paddingLeft: 8,
                                      paddingRight: 8,
                                      height: 22,
                                      backgroundColor:
                                        logDetailFile() === f().path
                                          ? look().palette.row_selected
                                          : "transparent",
                                      cursor: "pointer",
                                    }}
                                  >
                                    <Text
                                      style={{
                                        width: 14,
                                        fontSize: look().type.text_xsmall.size,
                                        color: look().palette.muted,
                                      }}
                                    >
                                      {f().st}
                                    </Text>
                                    <Text
                                      style={{
                                        flex: 1,
                                        minWidth: 0,
                                        fontSize: look().type.ui.size,
                                        color: look().palette.text,
                                        numberOfLines: 1,
                                      }}
                                    >
                                      {f().path}
                                    </Text>
                                  </View>
                                )}
                              </Each>
                            </View>
                          </Scroll>

                          <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}>
                            <If
                              when={logDetailFile() != null}
                              else={<Placeholder text={t("log.pick_file")} />}
                            >
                              <If
                                when={logDetailUrl() != null}
                                else={
                                  <Placeholder text={logDetailMessage() ?? t("log.loading_file")} />
                                }
                              >
                                <SmeltDiffView patchUrl={logDetailUrl() ?? ""} layout="unified" />
                              </If>
                            </If>
                          </View>
                        </If>
                      </If>
                    </If>
                  </View>
                </ResizablePanel>
              </ResizablePanelGroup>
            </View>
          }
        >
          {/* ---- 更改页（左 Diff + 右改动树与提交框） ---- */}
          <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "row" }}>
            <ResizablePanelGroup
              axis="horizontal"
              handleClass="bg-border group-hover:bg-muted-foreground/50 group-active:bg-muted-foreground/70"
            >
              {/* 左侧：Diff 视图 */}
              <ResizablePanel minSize={180}>
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
                  <If
                    when={gitDiff() != null}
                    else={
                      <Placeholder
                        text={
                          changeListModel().totalChanges > 0
                            ? t("changes.pick_file")
                            : t("changes.no_files")
                        }
                      />
                    }
                  >
                    <If
                      when={Boolean(gitDiff()?.loaded)}
                      else={<Placeholder text={t("diff.loading")} />}
                    >
                      <If
                        when={gitDiff()?.message == null}
                        else={
                          <View style={{ flex: 1, padding: 16 }}>
                            <Text
                              style={{
                                fontSize: look().type.ui.size,
                                lineHeight: look().type.ui.line_height,
                                color: look().palette.muted,
                              }}
                            >
                              {gitDiff()?.message ?? ""}
                            </Text>
                          </View>
                        }
                      >
                        {/* Diff 工具栏 */}
                        <View
                          style={{
                            display: "flex",
                            flexDirection: "row",
                            alignItems: "center",
                            justifyContent: "space-between",
                            gap: 8,
                            paddingLeft: 12,
                            paddingRight: 12,
                            paddingTop: 4,
                            paddingBottom: 4,
                            borderBottomWidth: 1,
                            borderBottomColor: look().palette.hair,
                            flexShrink: 0,
                          }}
                        >
                          <Text
                            style={{
                              flex: 1,
                              minWidth: 0,
                              fontSize: look().type.small.size,
                              color: look().palette.muted,
                              numberOfLines: 1,
                            }}
                          >
                            {gitDiff()?.aggregate
                              ? t("diff.all_changes")
                              : (gitDiff()?.path.split("/").pop() ?? gitDiff()?.path ?? "")}
                          </Text>
                          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
                            <If when={Boolean(gitDiff()?.worktreeFile)}>
                              <SmeltButton
                                quiet={true}
                                small={true}
                                label={t("diff.open_in_file")}
                                onPress={() => {
                                  const d = gitDiff();
                                  if (d?.worktreeFile) {
                                    void openFile({
                                      machine: d.root.machine,
                                      path: d.worktreeFile,
                                    }).catch((err: any) => notifyError(err?.message ?? String(err)));
                                  }
                                }}
                              />
                            </If>
                            <If when={Boolean(gitDiff()?.aggregate)}>
                              <SmeltButton
                                quiet={true}
                                small={true}
                                label={
                                  allAggregateCollapsed()
                                    ? t("diff.expand_all")
                                    : t("diff.collapse_all")
                                }
                                onPress={toggleAllAggregateFiles}
                              />
                            </If>
                            <SmeltButton
                              quiet={true}
                              small={true}
                              selected={diffSplit()}
                              label={t("diff.split")}
                              tooltip={t("diff.split_tooltip")}
                              onPress={() => diffSplit.set(!diffSplit())}
                            />
                          </View>
                        </View>

                        {/* 汇总补丁放不下的文件：说明原因，能单独看的点开 */}
                        <If when={diffLeftOut().length > 0}>
                          <Scroll
                            style={{
                              // 有补丁时只占顶上一条，没有时就是整页
                              ...(gitDiff()?.patchUrl ? { flexShrink: 0, maxHeight: 132 } : { flex: 1 }),
                              borderBottomWidth: 1,
                              borderBottomColor: look().palette.hair,
                            }}
                          >
                            <View style={{ display: "flex", flexDirection: "column", paddingTop: 4, paddingBottom: 4 }}>
                              <Text
                                style={{
                                  paddingLeft: 12,
                                  paddingRight: 12,
                                  height: 22,
                                  fontSize: look().type.small.size,
                                  color: look().palette.muted,
                                }}
                              >
                                {t("diff.left_out", { count: diffLeftOut().length })}
                              </Text>
                              <Each of={diffLeftOut()} key={(e: any) => e.path}>
                                {(entry: () => any) => {
                                  const reason = computed(() => diffLeftOutReason(entry()));
                                  return (
                                    <View
                                      class={reason() === "separate" ? "hover:bg-accent" : undefined}
                                      onPress={() => {
                                        const cur = gitDiff();
                                        if (cur && reason() === "separate") openEntryAlone(cur.root, entry());
                                      }}
                                      style={{
                                        height: 22,
                                        paddingLeft: 12,
                                        paddingRight: 12,
                                        display: "flex",
                                        flexDirection: "row",
                                        alignItems: "center",
                                        gap: 8,
                                        ...(reason() === "separate" ? { cursor: "pointer" } : {}),
                                      }}
                                    >
                                      <Text
                                        style={{
                                          flex: 1,
                                          minWidth: 0,
                                          fontFamily: look().fonts?.mono,
                                          fontSize: look().type.text_xsmall.size,
                                          color: look().palette.text,
                                          numberOfLines: 1,
                                        }}
                                      >
                                        {entry().path}
                                      </Text>
                                      <Text
                                        style={{
                                          flexShrink: 0,
                                          fontSize: look().type.small.size,
                                          color: look().palette.muted,
                                        }}
                                      >
                                        {reason() === "directory"
                                          ? t("diff.left_out_directory")
                                          : t(
                                              reason() === "too_large"
                                                ? "diff.left_out_too_large"
                                                : "diff.left_out_separate",
                                              { size: formatSize(entry().size) },
                                            )}
                                      </Text>
                                    </View>
                                  );
                                }}
                              </Each>
                            </View>
                          </Scroll>
                        </If>

                        <If when={gitDiff()?.patchUrl != null}>
                        <SmeltDiffView
                          diffRef={(api: any) => {
                            changesDiffApi = api;
                            if (api && pendingDiffFile) {
                              queueMicrotask(() => flushPendingDiffFile());
                            }
                          }}
                          patchUrl={gitDiff()?.patchUrl ?? ""}
                          layout={diffSplit() ? "split" : "unified"}
                          annotations={annotationsList()}
                          onFileToggle={({ file }: { file: any }) => {
                            if (!file?.path) return;
                            if (file.collapsed) collapsedDiffFiles.add(file.path);
                            else collapsedDiffFiles.delete(file.path);
                            bump();
                          }}
                          onLineSelectionChange={({ range }) => {
                            if (range) {
                              commentRange.set(range);
                            } else if (!commentOpen()) {
                              commentRange.set(null);
                            }
                          }}
                          renderFileHeader={(file: any) => {
                            const ops = computed(() => hunkOpsFor(gitDiff()));
                            const fullPath = computed(() => {
                              const cur = gitDiff();
                              return cur && file?.path && file.status !== "deleted"
                                ? fullFilePath(cur.root.root, file.path, false)
                                : null;
                            });
                            return (
                              <View
                                style={{
                                  flex: 1,
                                  height: "100%",
                                  paddingLeft: 10,
                                  paddingRight: 10,
                                  display: "flex",
                                  flexDirection: "row",
                                  alignItems: "center",
                                  gap: 6,
                                  backgroundColor: look().palette.surface,
                                  borderBottomWidth: 1,
                                  borderBottomColor: look().palette.hair,
                                }}
                              >
                                <View
                                  onPress={() => changesDiffApi?.toggleFile?.(file.path)}
                                  style={{
                                    flex: 1,
                                    minWidth: 0,
                                    display: "flex",
                                    flexDirection: "row",
                                    alignItems: "center",
                                    gap: 6,
                                    cursor: "pointer",
                                  }}
                                >
                                  <SmeltDisclosureChevron
                                    open={!file.collapsed}
                                    size={12}
                                    color={look().palette.muted}
                                  />
                                  <Text
                                    style={{
                                      flex: 1,
                                      minWidth: 0,
                                      fontFamily: look().fonts?.mono,
                                      fontSize: look().type.text_xsmall.size,
                                      color: look().palette.text,
                                      numberOfLines: 1,
                                    }}
                                  >
                                    {file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}
                                  </Text>
                                  <If when={Number(file.added ?? 0) > 0}>
                                    <Text
                                      style={{
                                        fontFamily: look().fonts?.mono,
                                        fontSize: look().type.text_xsmall.size,
                                        color: look().palette.ok,
                                      }}
                                    >
                                      {`+${file.added}`}
                                    </Text>
                                  </If>
                                  <If when={Number(file.deleted ?? 0) > 0}>
                                    <Text
                                      style={{
                                        fontFamily: look().fonts?.mono,
                                        fontSize: look().type.text_xsmall.size,
                                        color: look().palette.bad,
                                      }}
                                    >
                                      {`-${file.deleted}`}
                                    </Text>
                                  </If>
                                </View>
                                <If when={!gitDiff()?.aggregate && ops() !== "none"}>
                                  <SmeltButton
                                    quiet={true}
                                    small={true}
                                    label={ops() === "unstage" ? t("stage.unstage") : t("stage.stage")}
                                    onPress={() => {
                                      const cur = gitDiff();
                                      if (!cur) return;
                                      stageFile(cur.root, cur.path, ops() !== "unstage");
                                    }}
                                  />
                                </If>
                                <If when={Boolean(fullPath())}>
                                  <SmeltButton
                                    quiet={true}
                                    small={true}
                                    label={t("diff.open")}
                                    onPress={() => {
                                      const cur = gitDiff();
                                      const path = fullPath();
                                      if (cur && path) {
                                        void openFile({
                                          machine: cur.root.machine,
                                          path,
                                        }).catch((err: any) =>
                                          notifyError(err?.message ?? String(err)),
                                        );
                                      }
                                    }}
                                  />
                                </If>
                              </View>
                            );
                          }}
                          renderSeparator={(sep: any) => {
                            const ops = computed(() => hunkOpsFor(gitDiff()));
                            const hunkIx = Number(sep?.gap ?? 0);
                            return (
                              <View
                                style={{
                                  flex: 1,
                                  height: "100%",
                                  paddingLeft: 12,
                                  paddingRight: 8,
                                  display: "flex",
                                  flexDirection: "row",
                                  alignItems: "center",
                                  justifyContent: "space-between",
                                  gap: 8,
                                  backgroundColor: look().palette.surface,
                                }}
                              >
                                <Text
                                  style={{
                                    flex: 1,
                                    minWidth: 0,
                                    fontFamily: look().fonts?.mono,
                                    fontSize: look().type.text_xsmall.size,
                                    color: look().palette.muted,
                                    numberOfLines: 1,
                                  }}
                                >
                                  {sep?.header ? `@@ ${sep.header}` : "@@"}
                                </Text>
                                <If when={!gitDiff()?.aggregate && ops() === "stage_discard"}>
                                  <View style={{ display: "flex", flexDirection: "row", gap: 4 }}>
                                    <SmeltButton
                                      quiet={true}
                                      small={true}
                                      label={t("hunk.stage")}
                                      onPress={() => {
                                        const cur = gitDiff();
                                        if (cur) void applyHunk(cur.root, hunkIx, "stage");
                                      }}
                                    />
                                    <SmeltButton
                                      quiet={true}
                                      danger={true}
                                      small={true}
                                      label={t("hunk.discard")}
                                      onPress={() => {
                                        const cur = gitDiff();
                                        if (cur) startDiscardHunk(cur.root, hunkIx);
                                      }}
                                    />
                                  </View>
                                </If>
                                <If when={!gitDiff()?.aggregate && ops() === "unstage"}>
                                  <SmeltButton
                                    quiet={true}
                                    small={true}
                                    label={t("hunk.unstage")}
                                    onPress={() => {
                                      const cur = gitDiff();
                                      if (cur) void applyHunk(cur.root, hunkIx, "unstage");
                                    }}
                                  />
                                </If>
                              </View>
                            );
                          }}
                          renderAddAnnotation={(range: any) => (
                            <SmeltButton
                              primary={true}
                              small={true}
                              icon="icons/plus.svg"
                              onPress={() => {
                                commentRange.set(range);
                                commentOpen.set(true);
                              }}
                              style={{ width: 18, height: 18, minHeight: 18, borderRadius: 4 }}
                            />
                          )}
                          renderAnnotation={() => (
                            <View
                              style={{
                                margin: 8,
                                padding: 10,
                                gap: 8,
                                borderRadius: look().radius.md,
                                borderWidth: 1,
                                borderColor: look().palette.hair,
                                backgroundColor: look().palette.surface,
                                display: "flex",
                                flexDirection: "column",
                              }}
                            >
                              <View
                                style={{
                                  display: "flex",
                                  flexDirection: "row",
                                  alignItems: "center",
                                  justifyContent: "space-between",
                                  gap: 8,
                                }}
                              >
                                <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6 }}>
                                  <SmeltIcon path="icons/bot.svg" size={14} color={look().palette.text} />
                                  <Text
                                    style={{
                                      fontSize: look().type.small_medium.size,
                                      fontWeight: look().type.small_medium.weight,
                                      color: look().palette.text,
                                    }}
                                  >
                                    {t("comment.title")}
                                  </Text>
                                </View>
                                <Text
                                  style={{
                                    fontSize: look().type.text_xsmall.size,
                                    color: look().palette.muted,
                                  }}
                                >
                                  {commentRangeTitle()}
                                </Text>
                              </View>
                              <Input
                                ref={(el: any) => {
                                  el?.focus?.();
                                }}
                                multiline={true}
                                autoFocus={true}
                                value={commentText()}
                                placeholder={t("comment.placeholder")}
                                onChange={({ value }: { value: string }) => commentText.set(value)}
                                onKeyPress={(e: any) => {
                                  if (e?.key === "Escape") clearComment();
                                }}
                                class="rounded-(--radius) border border-input bg-card p-2 text-foreground"
                                style={{
                                  minHeight: 52,
                                  fontSize: look().type.small.size,
                                  lineHeight: look().type.small.line_height,
                                }}
                              />
                              <Overlay
                                onCancel={clearComment}
                                style={{ width: 0, height: 0, pointerEvents: "none" }}
                              />
                              <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
                                <SmeltButton
                                  quiet={true}
                                  small={true}
                                  label={t("comment.cancel")}
                                  onPress={clearComment}
                                />
                                <SmeltButton
                                  primary={true}
                                  small={true}
                                  label={t("comment.send")}
                                  onPress={sendDiffComment}
                                />
                              </View>
                            </View>
                          )}
                        />
                        </If>
                      </If>
                    </If>
                  </If>
                </View>
              </ResizablePanel>

              {/* 右侧：改动列表 + 提交框 */}
              <ResizablePanel
                fixed={true}
                size={treeWidth()}
                minSize={MIN_TREE_WIDTH}
                maxSize={MAX_TREE_WIDTH}
                onResize={({ size }: { size: number }) => {
                  treeWidth.set(Math.max(MIN_TREE_WIDTH, Math.min(MAX_TREE_WIDTH, size)));
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
                    paddingTop: 8,
                    overflow: "hidden",
                  }}
                >
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      justifyContent: "space-between",
                      paddingLeft: 12,
                      paddingRight: 8,
                      paddingBottom: 4,
                      flexShrink: 0,
                    }}
                  >
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall_medium.size,
                        fontWeight: look().type.text_xsmall_medium.weight,
                        color: look().palette.faint,
                      }}
                    >
                      {t("title")}
                    </Text>
                    {/* 单仓工作区也在标题右侧给出同步菜单入口 */}
                    <If when={repos().length <= 1 && activeRepo() != null}>
                      <OpsMenu
                        repo={activeRepo()!}
                        ahead={activeStatus()?.ahead ?? 0}
                        behind={activeStatus()?.behind ?? 0}
                        stashCount={activeStatus()?.stash_count ?? 0}
                        hasChanges={(activeStatus()?.files.length ?? 0) > 0}
                        upstream={Boolean(activeStatus()?.upstream)}
                        triggerLabel={
                          activeBusy()
                            ? busyLabel(activeBusy()!)
                            : (activeStatus()?.ahead ?? 0) + (activeStatus()?.behind ?? 0) > 0
                              ? `↑${activeStatus()?.ahead ?? 0} ↓${activeStatus()?.behind ?? 0}`
                              : "⋯"
                        }
                        triggerColor={
                          activeBusy() || (activeStatus()?.ahead ?? 0) + (activeStatus()?.behind ?? 0) > 0
                            ? look().palette.text
                            : look().palette.faint
                        }
                      />
                    </If>
                  </View>

                  <If
                    when={changeListModel().rows.length > 0}
                    else={
                      <Placeholder
                        text={t(`changes.${emptyListKind()}`)}
                        subtext={
                          emptyListKind() === "clean" ? t("changes.nothing_to_commit") : undefined
                        }
                      />
                    }
                  >
                    <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
                      <View style={{ display: "flex", flexDirection: "column", paddingBottom: 8 }}>
                        <Each of={changeListModel().rows} key={(row: any) => row.key}>
                          {(row: () => any) => (
                            <If
                              when={row().kind === "repo_header"}
                              else={
                                <If
                                  when={row().kind === "commit_box"}
                                  else={
                                    <If
                                      when={row().kind === "group_header"}
                                      else={
                                        <If
                                          when={row().kind === "tree_dir"}
                                          else={
                                            /* 改动文件行 */
                                            <SmeltContextMenu
                                              popupStyle={{ minWidth: 168 }}
                                              trigger={
                                                <View
                                                  class="hover:bg-accent"
                                                  onPress={() =>
                                                    openAggregateFile(row().repo, row().path)
                                                  }
                                                  style={{
                                                    width: "100%",
                                                    display: "flex",
                                                    flexDirection: "row",
                                                    alignItems: "center",
                                                    gap: 4,
                                                    height: 24,
                                                    marginLeft: 4,
                                                    marginRight: 4,
                                                    paddingLeft: 10 + row().depth * 12,
                                                    paddingRight: 8,
                                                    borderRadius: look().radius.row,
                                                    cursor: "pointer",
                                                    overflow: "hidden",
                                                  }}
                                                >
                                                  <If when={Boolean(row().gitlink)}>
                                                    <Text
                                                      style={{
                                                        fontSize: look().type.text_xsmall.size,
                                                        color: look().palette.faint,
                                                      }}
                                                    >
                                                      {repoKindGlyph("submodule")}
                                                    </Text>
                                                  </If>
                                                  <Text
                                                    style={{
                                                      flex: 1,
                                                      minWidth: 0,
                                                      fontFamily: look().fonts?.mono,
                                                      fontSize: look().type.text_xsmall.size,
                                                      color: look().palette.text,
                                                      numberOfLines: 1,
                                                    }}
                                                  >
                                                    {row().name}
                                                  </Text>
                                                  <If when={Boolean(row().gitlink)}>
                                                    <Text
                                                      style={{
                                                        fontSize: look().type.text_xsmall.size,
                                                        color: look().palette.faint,
                                                      }}
                                                    >
                                                      {t("file.submodule")}
                                                    </Text>
                                                  </If>
                                                  <SmeltButton
                                                    quiet={true}
                                                    small={true}
                                                    disabled={Boolean(row().busy)}
                                                    icon={
                                                      row().isStaged
                                                        ? "icons/minus.svg"
                                                        : "icons/plus.svg"
                                                    }
                                                    tooltip={
                                                      row().isStaged
                                                        ? t("stage.unstage")
                                                        : t("stage.stage")
                                                    }
                                                    onPress={() =>
                                                      stageFile(row().repo, row().path, !row().isStaged)
                                                    }
                                                  />
                                                  <Text
                                                    style={{
                                                      fontFamily: look().fonts?.mono,
                                                      fontSize: look().type.text_xsmall.size,
                                                      flexShrink: 0,
                                                      color: (() => {
                                                        const code = String(row().status ?? " M");
                                                        const ch = code.trim()[0] ?? "M";
                                                        if (code === "??" || ch === "A") {
                                                          return look().palette.ok;
                                                        }
                                                        if (ch === "D") return look().palette.bad;
                                                        return look().palette.need;
                                                      })(),
                                                    }}
                                                  >
                                                    {String(row().status ?? " M").trim()[0] ?? "M"}
                                                  </Text>
                                                </View>
                                              }
                                            >
                                              <MenuRow
                                                label={t("file.open_diff")}
                                                onSelect={() =>
                                                  openDiff(
                                                    row().repo,
                                                    row().path,
                                                    row().status === "??",
                                                  )
                                                }
                                              />
                                              <MenuRow
                                                label={t("file.copy_path")}
                                                onSelect={() => {
                                                  const full = pathJoin(
                                                    pathStyle(row().repo.machine),
                                                    row().repo.root,
                                                    row().path,
                                                  );
                                                  void clipboardWrite(full);
                                                }}
                                              />
                                              <If when={row().repo.machine == null}>
                                                <MenuRow
                                                  label={
                                                    isMac
                                                      ? t("file.reveal")
                                                      : t("file.reveal_file_manager")
                                                  }
                                                  onSelect={() => {
                                                    const full = pathJoin(
                                                      pathStyle(null),
                                                      row().repo.root,
                                                      row().path,
                                                    );
                                                    void revealPath(full);
                                                  }}
                                                />
                                              </If>
                                              <MenuSeparator
                                                class="my-1 bg-border"
                                                style={{ height: 1 }}
                                              />
                                              <MenuRow
                                                label={
                                                  row().status === "??"
                                                    ? t("discard.file_untracked_ok")
                                                    : t("discard.file_ok")
                                                }
                                                iconPath="icons/trash.svg"
                                                danger={true}
                                                onSelect={() =>
                                                  startDiscardFile(row().repo, row().path)
                                                }
                                              />
                                            </SmeltContextMenu>
                                          }
                                        >
                                          {/* 压缩目录行 */}
                                          <View
                                            class="hover:bg-accent"
                                            onPress={() => {
                                              if (treeCollapsed.has(row().path)) {
                                                treeCollapsed.delete(row().path);
                                              } else {
                                                treeCollapsed.add(row().path);
                                              }
                                              bump();
                                            }}
                                            style={{
                                              display: "flex",
                                              flexDirection: "row",
                                              alignItems: "center",
                                              gap: 4,
                                              height: 24,
                                              marginLeft: 4,
                                              marginRight: 4,
                                              paddingLeft: 10 + row().depth * 12,
                                              paddingRight: 8,
                                              borderRadius: look().radius.row,
                                              cursor: "pointer",
                                            }}
                                          >
                                            <SmeltDisclosureChevron
                                              open={!row().collapsed}
                                              size={10}
                                              color={look().palette.muted}
                                            />
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
                                            <SmeltButton
                                              quiet={true}
                                              small={true}
                                              icon={
                                                row().isStaged
                                                  ? "icons/minus.svg"
                                                  : "icons/plus.svg"
                                              }
                                              tooltip={
                                                row().isStaged
                                                  ? t("stage.unstage")
                                                  : t("stage.stage")
                                              }
                                              onPress={() =>
                                                stagePaths(row().repo, [row().path], !row().isStaged)
                                              }
                                            />
                                          </View>
                                        </If>
                                      }
                                    >
                                      {/* 分组标题（已暂存 / 更改） */}
                                      <View
                                        style={{
                                          display: "flex",
                                          flexDirection: "row",
                                          alignItems: "center",
                                          height: 24,
                                          paddingLeft: 12 + row().depth * 10,
                                          paddingRight: 10,
                                        }}
                                      >
                                        <Text
                                          style={{
                                            flex: 1,
                                            minWidth: 0,
                                            fontSize: look().type.ui_medium.size,
                                            fontWeight: look().type.ui_medium.weight,
                                            color: look().palette.faint,
                                            numberOfLines: 1,
                                          }}
                                        >
                                          {row().title}
                                        </Text>
                                        <SmeltButton
                                          quiet={true}
                                          small={true}
                                          icon={
                                            row().staged ? "icons/minus.svg" : "icons/plus.svg"
                                          }
                                          tooltip={
                                            row().staged ? t("stage.unstage") : t("stage.stage")
                                          }
                                          onPress={() =>
                                            stagePaths(row().repo, row().paths, !row().staged)
                                          }
                                        />
                                      </View>
                                    </If>
                                  }
                                >
                                  {/* 每仓独立提交框 */}
                                  <View
                                    style={{
                                      display: "flex",
                                      flexDirection: "column",
                                      gap: 6,
                                      paddingLeft: 12 + row().depth * 10,
                                      paddingRight: 8,
                                      paddingTop: 4,
                                      paddingBottom: 6,
                                    }}
                                  >
                                    <Input
                                      multiline={true}
                                      value={commitMessageSignal(row().repoKey)()}
                                      placeholder={
                                        row().branch
                                          ? t("commit.placeholder_branch", { branch: row().branch })
                                          : t("commit.placeholder")
                                      }
                                      onChange={({ value }: { value: string }) => {
                                        commitMessageSignal(row().repoKey).set(value);
                                      }}
                                      class="rounded-(--radius) border border-input bg-card p-2 text-foreground"
                                      style={{
                                        minHeight: 52,
                                        maxHeight: 120,
                                        fontSize: look().type.small.size,
                                        lineHeight: look().type.small.line_height,
                                      }}
                                    />
                                    <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
                                      <SmeltButton
                                        primary={true}
                                        small={true}
                                        fill={true}
                                        disabled={
                                          !(
                                            commitMessageSignal(row().repoKey)().trim().length > 0 &&
                                            row().hasStaged &&
                                            !row().writeOp
                                          )
                                        }
                                        label={t(commitButtonBusyKey(row().writeOp))}
                                        onPress={() => {
                                          setActiveRepo(row().repo);
                                          runCommit(row().repo, true);
                                        }}
                                      />
                                      <SmeltButton
                                        quiet={true}
                                        small={true}
                                        disabled={
                                          !(
                                            commitMessageSignal(row().repoKey)().trim().length > 0 &&
                                            row().hasStaged &&
                                            !row().writeOp
                                          )
                                        }
                                        label={t("commit.commit_only")}
                                        onPress={() => {
                                          setActiveRepo(row().repo);
                                          runCommit(row().repo, false);
                                        }}
                                      />
                                      <SmeltButton
                                        quiet={true}
                                        small={true}
                                        disabled={!(row().ahead > 0 && !row().writeOp)}
                                        label={`↑${row().ahead}`}
                                        onPress={() => {
                                          setActiveRepo(row().repo);
                                          runPushOnly(row().repo);
                                        }}
                                      />
                                    </View>
                                    <If
                                      when={
                                        commitMessageSignal(row().repoKey)().trim().length > 0 &&
                                        !row().hasStaged
                                      }
                                    >
                                      <Text
                                        style={{
                                          fontSize: look().type.text_xsmall.size,
                                          color: look().palette.faint,
                                        }}
                                      >
                                        {t("commit.stage_first")}
                                      </Text>
                                    </If>
                                    <If when={Boolean(row().error)}>
                                      <View
                                        style={{
                                          padding: 6,
                                          borderRadius: look().radius.sm,
                                          backgroundColor: look().palette.surface,
                                        }}
                                      >
                                        <Text
                                          style={{
                                            fontFamily: look().fonts?.mono,
                                            fontSize: look().type.text_xsmall.size,
                                            color: look().palette.bad,
                                          }}
                                        >
                                          {row().error ?? ""}
                                        </Text>
                                      </View>
                                    </If>
                                  </View>
                                </If>
                              }
                            >
                              {/* 多仓标题行 */}
                              <View
                                class="hover:bg-accent"
                                onPress={() => setActiveRepo(row().repo)}
                                style={{
                                  display: "flex",
                                  flexDirection: "row",
                                  alignItems: "center",
                                  gap: 4,
                                  height: 24,
                                  marginLeft: 4,
                                  marginRight: 4,
                                  paddingLeft: 4,
                                  paddingRight: 4,
                                  borderRadius: look().radius.row,
                                  backgroundColor: row().active
                                    ? look().palette.soft
                                    : "transparent",
                                  cursor: "pointer",
                                }}
                              >
                                <SmeltButton
                                  quiet={true}
                                  small={true}
                                  icon={
                                    row().collapsed
                                      ? "icons/chevron-right.svg"
                                      : "icons/chevron-down.svg"
                                  }
                                  tooltip={row().collapsed ? t("repo.expand") : t("repo.collapse")}
                                  onPress={() => {
                                    const rk = projectKey(row().repo.machine, row().repo.root);
                                    if (repoCollapsed.has(rk)) repoCollapsed.delete(rk);
                                    else repoCollapsed.add(rk);
                                    bump();
                                  }}
                                />
                                <Text
                                  style={{
                                    fontSize: look().type.text_xsmall.size,
                                    color: row().active
                                      ? look().palette.text
                                      : look().palette.faint,
                                  }}
                                >
                                  {repoKindGlyph(row().repoKind)}
                                </Text>
                                <Text
                                  style={{
                                    flex: 1,
                                    minWidth: 0,
                                    fontSize: look().type.text_xsmall.size,
                                    fontWeight: 600,
                                    color: look().palette.text,
                                    numberOfLines: 1,
                                  }}
                                >
                                  {row().label}
                                </Text>
                                <Text
                                  tooltip={
                                    <SmeltTooltip>
                                      <Text
                                        style={{
                                          fontSize: look().type.text_xsmall.size,
                                          color: look().palette.text,
                                        }}
                                      >
                                        {row().branch}
                                      </Text>
                                    </SmeltTooltip>
                                  }
                                  style={{
                                    maxWidth: 80,
                                    fontSize: look().type.text_xsmall.size,
                                    color: look().palette.faint,
                                    numberOfLines: 1,
                                  }}
                                >
                                  {row().branch}
                                </Text>
                                <OpsMenu
                                  repo={row().repo}
                                  ahead={row().ahead}
                                  behind={row().behind}
                                  stashCount={row().stashCount}
                                  hasChanges={row().hasChanges}
                                  upstream={row().upstream}
                                  triggerLabel={
                                    row().ahead + row().behind > 0
                                      ? `↑${row().ahead} ↓${row().behind}`
                                      : "⋯"
                                  }
                                  triggerColor={
                                    row().ahead + row().behind > 0
                                      ? look().palette.text
                                      : look().palette.faint
                                  }
                                />
                              </View>
                            </If>
                          )}
                        </Each>
                      </View>
                    </Scroll>
                  </If>

                  <If when={reposTruncated()}>
                    <View style={{ paddingLeft: 12, paddingRight: 12, paddingTop: 4, paddingBottom: 4, flexShrink: 0 }}>
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                        {t("changes.repos_truncated")}
                      </Text>
                    </View>
                  </If>
                </View>
              </ResizablePanel>
            </ResizablePanelGroup>
          </View>
        </If>
      </If>

      {/* 危险操作确认框（丢弃文件 / 丢弃全部 / 丢弃块 / 删除分支） */}
      <SmeltDialog
        open={dangerDialog() != null}
        role="alertdialog"
        accessibilityLabel={dangerDialog()?.title ?? ""}
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) dangerDialog.set(null);
        }}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
        style={{ width: 440, gap: 10 }}
      >
        <If when={dangerDialog() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {dangerDialog()?.title ?? ""}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {dangerDialog()?.body ?? ""}
          </Text>
          <If when={Boolean(dangerDialog()?.warning)}>
            <Text
              style={{
                color: look().palette.bad,
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
              }}
            >
              {dangerDialog()?.warning ?? ""}
            </Text>
          </If>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8, marginTop: 4 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("cancel")}
              onPress={() => dangerDialog.set(null)}
            />
            <SmeltButton
              danger={true}
              small={true}
              label={dangerDialog()?.okLabel ?? ""}
              onPress={async () => {
                const dlg = dangerDialog();
                if (!dlg) return;
                const close = await dlg.onConfirm();
                if (close) dangerDialog.set(null);
              }}
            />
          </View>
        </If>
      </SmeltDialog>
    </View>
  );
}

export default Main;
