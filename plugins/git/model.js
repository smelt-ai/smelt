// 变更面板纯逻辑：PR 链接、改动树单链压缩、暂存分组、多仓写目标解析、写锁、
// status 状态机、统一补丁解析与行坐标映射、分支图列布局与 SVG 路径生成。
// 不依赖 NSH 或宿主全局，供 bun test 直接单测。

export const MIN_TREE_WIDTH = 190;
export const MAX_TREE_WIDTH = 320;
export const GIT_DISCOVERY_MIN_INTERVAL_MS = 15_000;
export const GIT_AUTOFETCH_INTERVAL_MS = 60_000;
export const GIT_STATUS_MAX_AUTO_RETRIES = 3;
export const LOG_LIMIT = 500;

export const GRAPH_ROW_H = 24;
const GRAPH_LANE_W = 14;
const GRAPH_DOT_R = 3.5;
const GRAPH_PAD = 8;

export function projectKey(machine, root) {
  return `${machine ?? ""}\0${root}`;
}

// ---- PR 链接 ----

function encodeBranch(branch) {
  const bytes = new TextEncoder().encode(String(branch ?? ""));
  let out = "";
  for (const byte of bytes) {
    const ok =
      (byte >= 0x41 && byte <= 0x5a) ||
      (byte >= 0x61 && byte <= 0x7a) ||
      (byte >= 0x30 && byte <= 0x39) ||
      byte === 0x2d ||
      byte === 0x5f ||
      byte === 0x2e ||
      byte === 0x7e ||
      byte === 0x2f;
    if (ok) {
      out += String.fromCharCode(byte);
    } else {
      out += `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
    }
  }
  return out;
}

function webLocation(remoteUrl) {
  const url = String(remoteUrl ?? "").trim();
  let host = "";
  let path = "";
  if (url.startsWith("https://") || url.startsWith("http://")) {
    const rest = url.replace(/^https?:\/\//, "");
    const slash = rest.indexOf("/");
    if (slash < 0) return null;
    const authority = rest.slice(0, slash);
    path = rest.slice(slash + 1);
    const at = authority.lastIndexOf("@");
    host = at >= 0 ? authority.slice(at + 1) : authority;
  } else if (url.startsWith("ssh://") || url.startsWith("git+ssh://")) {
    const rest = url.replace(/^(?:git\+)?ssh:\/\//, "");
    const slash = rest.indexOf("/");
    if (slash < 0) return null;
    const authority = rest.slice(0, slash);
    path = rest.slice(slash + 1);
    const at = authority.lastIndexOf("@");
    const hostPort = at >= 0 ? authority.slice(at + 1) : authority;
    const colon = hostPort.indexOf(":");
    host = colon >= 0 ? hostPort.slice(0, colon) : hostPort;
  } else {
    const colon = url.indexOf(":");
    if (colon < 0) return null;
    const authority = url.slice(0, colon);
    if (authority.includes("/")) return null;
    path = url.slice(colon + 1);
    const at = authority.lastIndexOf("@");
    host = at >= 0 ? authority.slice(at + 1) : authority;
  }
  path = path.replace(/^\/+|\/+$/g, "");
  if (path.endsWith(".git")) path = path.slice(0, -4);
  if (!host || !path.includes("/")) return null;
  return { host: host.toLowerCase(), path };
}

export function pullRequestUrl(remoteUrl, base, head) {
  const loc = webLocation(remoteUrl);
  if (!loc) return null;
  const encBase = encodeBranch(base);
  const encHead = encodeBranch(head);
  if (loc.host.includes("github")) {
    return `https://${loc.host}/${loc.path}/compare/${encBase}...${encHead}?expand=1`;
  }
  if (loc.host.includes("gitlab")) {
    return `https://${loc.host}/${loc.path}/-/merge_requests/new?merge_request%5Bsource_branch%5D=${encHead}&merge_request%5Btarget_branch%5D=${encBase}`;
  }
  return null;
}

export function pullRequestLink(repoInfoResult, tr) {
  const info = repoInfoResult?.info ?? null;
  if (!info) return { ok: false, error: tr("pull_request.not_repo") };
  const head = info.upstream_branch;
  if (!head) return { ok: false, error: tr("pull_request.not_pushed") };
  const base = info.default_branch;
  if (!base) return { ok: false, error: tr("pull_request.no_default_branch") };
  if (head === base) {
    return { ok: false, error: tr("pull_request.on_default_branch", { branch: base }) };
  }
  const remote = info.remote_url;
  if (!remote) return { ok: false, error: tr("pull_request.no_remote") };
  const url = pullRequestUrl(remote, base, head);
  if (!url) {
    return { ok: false, error: tr("pull_request.unknown_host", { remote }) };
  }
  return { ok: true, url };
}

// ---- 改动树 & 分组 ----

export function fullFilePath(root, path, aggregate) {
  if (aggregate) return null;
  if (path.startsWith("/")) return path;
  if (!root || root.endsWith("/")) return `${root}${path}`;
  if (!path) return `${root}/`;
  return `${root}/${path}`;
}

export function splitStagedAndChanged(files) {
  const staged = [];
  const changed = [];
  for (const item of files ?? []) {
    const code = Array.isArray(item) ? String(item[0] ?? "") : String(item?.code ?? "");
    const path = Array.isArray(item) ? String(item[1] ?? "") : String(item?.path ?? "");
    if (!path) continue;
    if (code === "??") {
      changed.push([code, path]);
      continue;
    }
    const x = code[0] ?? " ";
    const y = code[1] ?? " ";
    if (x !== " ") staged.push([code, path]);
    if (y !== " ") changed.push([code, path]);
  }
  return { staged, changed };
}

/** 构造改动目录树：单子目录链压缩成一行，目录排在文件前，跳过尾斜杠空文件名。 */
export function buildGitTree(files, collapsed = new Set()) {
  const makeNode = () => ({ dirs: new Map(), files: [] });
  const root = makeNode();

  for (const item of files ?? []) {
    const status = Array.isArray(item) ? String(item[0] ?? "") : String(item?.code ?? "");
    const path = Array.isArray(item) ? String(item[1] ?? "") : String(item?.path ?? "");
    const parts = path.split("/");
    const name = parts.pop();
    if (name === undefined) continue;
    let node = root;
    for (const part of parts) {
      if (!node.dirs.has(part)) node.dirs.set(part, makeNode());
      node = node.dirs.get(part);
    }
    if (name.length > 0) {
      node.files.push([name, status]);
    }
  }

  const rows = [];
  const walk = (node, prefix, depth) => {
    const dirNames = [...node.dirs.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    for (const name of dirNames) {
      let label = name;
      let path = prefix ? `${prefix}/${name}` : name;
      let cur = node.dirs.get(name);
      while (cur.files.length === 0 && cur.dirs.size === 1) {
        const [nextName, nextNode] = cur.dirs.entries().next().value;
        label = `${label}/${nextName}`;
        path = `${path}/${nextName}`;
        cur = nextNode;
      }
      const isCollapsed = collapsed.has(path);
      rows.push({ depth, name: label, path, status: null });
      if (!isCollapsed) {
        walk(cur, path, depth + 1);
      }
    }
    for (const [name, status] of node.files) {
      rows.push({
        depth,
        name,
        path: prefix ? `${prefix}/${name}` : name,
        status,
      });
    }
  };
  walk(root, "", 0);
  return rows;
}

// ---- 状态机 / 写锁 / 多仓落点 ----

export function gitStatusResponseAction(currentGen, requestGen, ok) {
  if (!ok) return "preserve_and_retry";
  return currentGen === requestGen ? "accept_current" : "accept_stale_and_refresh";
}

export function gitStatusRetryDelay(failureCount) {
  if (failureCount <= 0 || failureCount > GIT_STATUS_MAX_AUTO_RETRIES) return null;
  return 150 * (1 << (failureCount - 1));
}

export function gitEmptyListKind(status, refreshing, failureCount) {
  if (status && status.ok && (status.files?.length ?? 0) === 0) return "clean";
  if (refreshing) return failureCount > 0 ? "retrying" : "loading";
  if (failureCount > 0) return "failed";
  if (!status) return "loading";
  return "clean";
}

export function shouldCloseAggregateDiff(status, aggregate) {
  return Boolean(aggregate && status && status.ok && (status.files?.length ?? 0) === 0);
}

export function gitIndexOpInFlight(pending) {
  return Boolean(pending && !pending.completed);
}

export function clearConfirmedGitIndexOps(pendingMap, rootKey) {
  for (const [k, op] of [...pendingMap.entries()]) {
    if (k.startsWith(`${rootKey}\0`) && op.completed) {
      pendingMap.delete(k);
    }
  }
}

export function reserveGitWrite(writesMap, key, op) {
  if (writesMap.has(key)) return false;
  writesMap.set(key, op);
  return true;
}

export function releaseGitWrite(writesMap, key, op) {
  if (writesMap.get(key) !== op) return false;
  writesMap.delete(key);
  return true;
}

export function repoNeedsAttention(hasChanges, ahead, behind, stashCount) {
  return Boolean(hasChanges || ahead > 0 || behind > 0 || stashCount > 0);
}

export function resolveGitWriteTarget(projectRoot, selected, repoSet, fallback) {
  const repos = repoSet?.repos ?? [];
  if (selected && (repos.length === 0 || repos.some((r) => r.root === selected))) {
    return selected;
  }
  if (fallback) return fallback;
  if (repos.length > 0 && !repos.some((r) => r.root === projectRoot)) {
    return repos[0].root;
  }
  return projectRoot;
}

export function needsFetch(cached) {
  return !cached || Boolean(cached.stale);
}

export function gitDiscoveryWait(cached, nowMs) {
  if (!cached) return 0;
  if (!cached.stale) return null;
  return Math.max(0, GIT_DISCOVERY_MIN_INTERVAL_MS - Math.max(0, nowMs - cached.fetchedAt));
}

export function gitAutofetchWait(lastMs, nowMs) {
  if (lastMs == null) return 0;
  return Math.max(0, GIT_AUTOFETCH_INTERVAL_MS - Math.max(0, nowMs - lastMs));
}

export function commitButtonBusyKey(op) {
  if (op === "commit") return "busy.commit";
  if (op === "commit_and_push") return "busy.commit_and_push";
  if (op === "push") return "busy.push";
  return "commit.commit_and_push";
}

export function repoKindGlyph(kind) {
  if (kind === "root") return "◆";
  if (kind === "submodule") return "◇";
  return "◌";
}

export function seenChange(status, path) {
  const found = (status?.files ?? []).find(([, p]) => p === path);
  if (!found) return null;
  const [xy, filePath] = found;
  const hashes = status?.worktree_hashes ?? {};
  return {
    path: filePath,
    xy,
    worktree_hash: Object.prototype.hasOwnProperty.call(hashes, filePath) ? hashes[filePath] : null,
  };
}

export function seenChanges(status) {
  const hashes = status?.worktree_hashes ?? {};
  return (status?.files ?? []).map(([xy, path]) => ({
    path,
    xy,
    worktree_hash: Object.prototype.hasOwnProperty.call(hashes, path) ? hashes[path] : null,
  }));
}

// ---- Diff 视图（补丁本身由原生 DiffView 解析，这里只有改动列表上的事实） ----

/** 改动列表里点的那个路径对应的一项（重命名的认新旧两个路径，未跟踪目录不带末尾的 `/`）。 */
export function diffEntryFor(entries, requested) {
  const want = String(requested ?? "").replace(/\/+$/, "");
  return (entries ?? []).find((e) => e.path === want || e.old_path === want) ?? null;
}

/**
 * 不在汇总补丁里的一项为什么不在：`too_large` 超过查看上限、`directory` 未跟踪目录（多半是
 * 嵌套仓库），其余是单个较大或汇总放不下，能单独打开。
 */
export function diffLeftOutReason(entry) {
  const kind = entry?.body?.kind;
  return kind === "too_large" || kind === "directory" ? kind : "separate";
}

export function formatSize(bytes) {
  const n = Number(bytes ?? 0);
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)} GB`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MB`;
  return `${Math.ceil(n / 1024)} KB`;
}

export function diffFeedbackText(headerText, start, end, selected, comment) {
  const lo = Math.min(start, end);
  const hi = Math.max(start, end);
  let msg = `${headerText}\n`;
  msg += lo === hi ? `L${lo}\n` : `L${lo}-L${hi}\n`;
  if (selected && selected.length > 0) {
    msg += selected;
    if (!selected.endsWith("\n")) msg += "\n";
  }
  if (comment && comment.length > 0) {
    msg += `\n${comment}\n`;
  }
  return msg;
}

// ---- 提交历史分支图布局 & SVG 路径 ----

export function layoutGraph(commits) {
  const lanes = [];
  const rows = [];

  for (const c of commits ?? []) {
    const before = [...lanes];
    const claimed = [];
    for (let i = 0; i < lanes.length; i += 1) {
      if (lanes[i] === c.hash) claimed.push(i);
    }
    let node;
    if (claimed.length > 0) {
      node = claimed[0];
    } else {
      const free = lanes.findIndex((l) => l == null);
      if (free >= 0) {
        node = free;
      } else {
        node = lanes.length;
        lanes.push(null);
      }
    }
    for (let k = 1; k < claimed.length; k += 1) {
      lanes[claimed[k]] = null;
    }
    lanes[node] = null;

    const outLanes = [];
    const parents = c.parents ?? [];
    for (let n = 0; n < parents.length; n += 1) {
      const p = parents[n];
      let i;
      if (n === 0) {
        i = node;
      } else {
        const existing = lanes.findIndex((l) => l === p);
        if (existing >= 0) {
          outLanes.push(existing);
          continue;
        }
        const free = lanes.findIndex((l) => l == null);
        if (free >= 0) {
          i = free;
        } else {
          i = lanes.length;
          lanes.push(null);
        }
      }
      lanes[i] = p;
      outLanes.push(i);
    }

    while (lanes.length > 0 && lanes[lanes.length - 1] == null) {
      lanes.pop();
    }

    const edges = [];
    for (const i of claimed) edges.push({ kind: "in", from: i });
    for (const i of outLanes) edges.push({ kind: "out", to: i });
    for (let i = 0; i < before.length; i += 1) {
      const sha = before[i];
      if (sha == null || claimed.includes(i)) continue;
      const j = lanes.findIndex((l) => l === sha);
      if (j >= 0) edges.push({ kind: "through", from: i, to: j });
    }

    const width = Math.max(lanes.length, before.length, node + 1);
    rows.push({ node, edges, lanes: width });
  }
  return rows;
}

export function graphWidth(rows) {
  let max = 1;
  for (const r of rows ?? []) {
    if (r.lanes > max) max = r.lanes;
  }
  return GRAPH_PAD * 2 + Math.max(3, max) * GRAPH_LANE_W;
}

/** 一行分支图的 SVG。`laneColors` 是主题的分类色（`palette.chart`），按列号取模。 */
export function commitGraphRowSvg(row, width, rowH, laneColors) {
  const colorOf = (lane) => laneColors[lane % laneColors.length];
  const laneX = (lane) => GRAPH_PAD + lane * GRAPH_LANE_W + GRAPH_LANE_W / 2;
  const mid = rowH / 2;
  const nodeX = laneX(row.node);
  const parts = [];

  for (const edge of row.edges) {
    let x1;
    let y1;
    let x2;
    let y2;
    let color;
    if (edge.kind === "through") {
      x1 = laneX(edge.from);
      y1 = 0;
      x2 = laneX(edge.to);
      y2 = rowH;
      color = colorOf(edge.from);
    } else if (edge.kind === "in") {
      x1 = laneX(edge.from);
      y1 = 0;
      x2 = nodeX;
      y2 = mid;
      color = colorOf(edge.from);
    } else {
      x1 = nodeX;
      y1 = mid;
      x2 = laneX(edge.to);
      y2 = rowH;
      color = colorOf(edge.to);
    }
    const d =
      Math.abs(x1 - x2) < 0.5
        ? `M${x1} ${y1} L${x2} ${y2}`
        : `M${x1} ${y1} Q${x1} ${y2} ${x2} ${y2}`;
    parts.push(`<path d="${d}" fill="none" stroke="${color}" stroke-width="1.5" stroke-linecap="round"/>`);
  }

  parts.push(`<circle cx="${nodeX}" cy="${mid}" r="${GRAPH_DOT_R}" fill="${colorOf(row.node)}"/>`);
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${rowH}" width="${width}" height="${rowH}">${parts.join("")}</svg>`;
}

export function formatCommitTime(unixSec, now = new Date()) {
  if (!Number.isFinite(unixSec) || unixSec <= 0) return "";
  const d = new Date(unixSec * 1000);
  const pad2 = (n) => String(n).padStart(2, "0");
  if (d.getFullYear() === now.getFullYear()) {
    return `${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  }
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}
