// 文件视图纯逻辑：路径/语言/图片 MIME/HTML file URL、目录树展平、面包屑、搜索树、git 状态简标。
// 不依赖 NSH 或宿主全局，供 bun test 直接单测。

export const MIN_TREE_WIDTH = 190;
export const MAX_TREE_WIDTH = 320;
export const DEFAULT_TREE_WIDTH = 190;

const IMAGE_MIME_BY_EXT = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  webp: "image/webp",
  gif: "image/gif",
  bmp: "image/bmp",
  tif: "image/tiff",
  tiff: "image/tiff",
  svg: "image/svg+xml",
};

/** 从路径末尾取文件名（兼容 POSIX 与 Windows 分隔符）。 */
export function lastPathSegment(path) {
  const trimmed = String(path ?? "").replace(/[/\\]+$/, "");
  if (!trimmed) return "";
  const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
}

/** 取文件扩展名（不含点、小写）；点文件或无扩展名返回 null。 */
export function pathExtLower(path) {
  const name = lastPathSegment(path);
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return null;
  return name.slice(dot + 1).toLowerCase();
}

export function isPreviewableImage(path) {
  const ext = pathExtLower(path);
  return ext != null && Object.prototype.hasOwnProperty.call(IMAGE_MIME_BY_EXT, ext);
}

export function imageMimeForPath(path) {
  const ext = pathExtLower(path);
  return ext ? (IMAGE_MIME_BY_EXT[ext] ?? null) : null;
}

/** 扩展名或无扩展文件名（如 Makefile / Dockerfile）→ 编辑器语言标识。 */
export function editorLanguageForPath(path) {
  const ext = pathExtLower(path);
  if (ext) return ext;
  const name = lastPathSegment(path);
  return name ? name.toLowerCase() : "text";
}

export function isMarkdownPath(path) {
  const ext = pathExtLower(path);
  return ext === "md" || ext === "markdown";
}

export function isHtmlPath(path) {
  const ext = pathExtLower(path);
  return ext === "html" || ext === "htm";
}

/** 把本机绝对路径转成 RFC 8089 `file://` URL（不支持相对路径）。 */
export function pathToFileUrl(path) {
  if (typeof path !== "string" || path.length === 0) return null;
  // Windows 盘符路径：C:\a\b.html 或 C:/a/b.html
  if (/^[a-zA-Z]:[/\\]/.test(path)) {
    const drive = path.slice(0, 2);
    const rest = path
      .slice(2)
      .replace(/\\/g, "/")
      .split("/")
      .map((seg) => encodeURIComponent(seg))
      .join("/");
    return `file:///${drive}${rest}`;
  }
  // POSIX 绝对路径：/a/b.html
  if (path.startsWith("/")) {
    const encoded = path
      .split("/")
      .map((seg) => encodeURIComponent(seg))
      .join("/");
    return `file://${encoded}`;
  }
  return null;
}

/** 文件树不显示的目录项。 */
export const HIDDEN_TREE_ENTRIES = [".git", "node_modules", "target", ".DS_Store"];

/**
 * `fs.read_dir` 的原样结果 → 文件树的一层：滤掉 {@link HIDDEN_TREE_ENTRIES}，目录在前、同类按名字
 * 不区分大小写排。指向目录的符号链接当目录，能展开。
 */
export function treeEntries(raw) {
  const entries = [];
  for (const item of Array.isArray(raw) ? raw : []) {
    const name = String(item?.name ?? "");
    if (!name || HIDDEN_TREE_ENTRIES.includes(name)) continue;
    const kind = item.kind === "symlink" ? item.target_kind : item.kind;
    entries.push({ name, is_dir: kind === "dir", key: name.toLowerCase() });
  }
  entries.sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.key < b.key ? -1 : a.key > b.key ? 1 : 0;
  });
  return entries.map(({ name, is_dir }) => ({ name, is_dir }));
}

/** git status porcelain 两位码 → 简标（M / A / D / U）。 */
export function gitStatusMark(code) {
  const pair = String(code ?? "").slice(0, 2);
  if (pair.includes("D")) return "D";
  if (pair.includes("A")) return "A";
  if (pair.includes("?")) return "U";
  for (const ch of pair) {
    if (ch !== " " && ch !== "?") return "M";
  }
  return null;
}

export function gitMarkIndex(files) {
  const marks = new Map();
  for (const entry of files ?? []) {
    const code = Array.isArray(entry) ? entry[0] : entry?.code;
    const path = Array.isArray(entry) ? entry[1] : entry?.path;
    if (typeof path !== "string") continue;
    const mark = gitStatusMark(code);
    if (mark) marks.set(path, mark);
  }
  return marks;
}

/** 树里挂出的根：当前项目优先，本机时附加钉住的目录（去空去重）。 */
export function treeRoots(project, pinnedRoots) {
  const roots = [];
  if (project?.root) {
    roots.push(project.root);
  }
  const isLocal = !project || project.machine == null;
  if (isLocal) {
    for (const p of pinnedRoots ?? []) {
      if (p && !roots.includes(p)) roots.push(p);
    }
  }
  return roots;
}

export function projectKey(machine, root) {
  return `${machine ?? ""}\0${root}`;
}

export function machineKey(machine) {
  return machine ?? "";
}

function walkDirCached(styleOps, dir, dirCache, expanded, depth, out) {
  const listing = dirCache?.get(dir);
  if (!listing) return;
  for (const item of listing.entries) {
    const name = item.name;
    const isDir = Boolean(item.is_dir);
    const path = styleOps.join(dir, name);
    const isExpanded = expanded.has(path);
    out.push({ depth, name, isDir, path, expanded: isExpanded });
    if (isDir && isExpanded) {
      walkDirCached(styleOps, path, dirCache, expanded, depth + 1, out);
    }
  }
}

/** 展平当前可见文件树。单根不插根标题行；多根有可折叠的根标题行。 */
export function collectFileTreeRows(styleOps, treeState, expanded, dirCache, getGitStatus) {
  const roots = treeState.roots ?? [];
  const pinned = treeState.pinned ?? [];
  const collapsed = treeState.collapsed ?? new Set();
  const multi = roots.length > 1;
  const rows = [];

  for (const root of roots) {
    const git = getGitStatus ? getGitStatus(root) : null;
    const marks = git ? gitMarkIndex(git.files) : null;
    const open = !collapsed.has(root);
    if (multi) {
      rows.push({
        kind: "root_header",
        path: root,
        open,
        pinned: pinned.includes(root),
      });
      if (!open) continue;
    }
    if (!dirCache?.has(root)) {
      rows.push({
        kind: "loading",
        depth: multi ? 1 : 0,
        root,
      });
      continue;
    }
    const flat = [];
    walkDirCached(styleOps, root, dirCache, expanded, multi ? 1 : 0, flat);
    for (const entry of flat) {
      let gitMark = null;
      if (!entry.isDir && marks) {
        const rel = styleOps.stripPrefix(entry.path, root);
        if (rel != null) {
          gitMark = marks.get(rel) ?? null;
        }
      }
      rows.push({
        kind: "entry",
        depth: entry.depth,
        name: entry.name,
        isDir: entry.isDir,
        path: entry.path,
        expanded: entry.expanded,
        gitMark,
      });
    }
  }
  return rows;
}

/** 行的稳定 key：与路径绑定，不随上方目录展开错位。 */
export function fileTreeRowId(row) {
  if (row.kind === "loading") return `root-loading:${row.root}`;
  if (row.kind === "root_header") return `root:${row.path}`;
  return `file:${row.path}`;
}

export function fileTreeRowPath(row) {
  return row.kind === "loading" ? null : row.path;
}

/** 估算文件名在给定列宽下是否会被截断，用于决定是否显示 Tooltip。 */
export function nameLikelyTruncated(name, depth, panelW) {
  const chrome = 56 + depth * 14;
  const textW = Math.max(0, panelW - chrome);
  const maxChars = Math.floor(textW / 7.5);
  return [...String(name ?? "")].length > maxChars;
}

/** 面包屑分段：「项目名 > 中间目录 > 文件名」，多根取最长匹配根。 */
export function breadcrumbSegments(styleOps, path, roots) {
  let matched = null;
  for (const root of roots ?? []) {
    if (styleOps.startsWith(path, root)) {
      if (matched == null || root.length > matched.length) {
        matched = root;
      }
    }
  }
  if (matched == null) {
    return [{ label: styleOps.fileName(path) ?? path, path }];
  }
  const segs = [{ label: styleOps.fileName(matched) ?? matched, path: matched }];
  let acc = matched;
  const rel = styleOps.stripPrefix(path, matched) ?? "";
  const sepRe = styleOps.isWindows ? /[/\\]+/ : /\/+/;
  for (const part of rel.split(sepRe).filter(Boolean)) {
    acc = styleOps.join(acc, part);
    segs.push({ label: part, path: acc });
  }
  return segs;
}

/** 列出 `path` 在 `root` 之下所有需要展开的祖先目录（不含 `path` 自身、不含 `root` 自身，从浅到深）。 */
export function ancestorsUnderRoot(styleOps, path, root) {
  if (!styleOps.startsWith(path, root) || path === root) return [];
  const rel = styleOps.stripPrefix(path, root) ?? "";
  const sepRe = styleOps.isWindows ? /[/\\]+/ : /\/+/;
  const parts = rel.split(sepRe).filter(Boolean);
  const dirs = [];
  let acc = root;
  for (let i = 0; i + 1 < parts.length; i += 1) {
    acc = styleOps.join(acc, parts[i]);
    dirs.push(acc);
  }
  return dirs;
}

/** 按相对路径构造搜索结果目录树（目录按字典序排列）。 */
export function buildSearchTree(hits) {
  const root = { dirs: new Map(), files: [] };
  for (let ix = 0; ix < (hits?.length ?? 0); ix += 1) {
    const rel = String(hits[ix].rel ?? "");
    const parts = rel.split("/").filter(Boolean);
    let dir = root;
    for (let i = 0; i < parts.length; i += 1) {
      const part = parts[i];
      if (i + 1 < parts.length) {
        if (!dir.dirs.has(part)) {
          dir.dirs.set(part, { dirs: new Map(), files: [] });
        }
        dir = dir.dirs.get(part);
      } else {
        dir.files.push(ix);
      }
    }
  }
  return root;
}

/** 展平搜索结果树为可渲染行（目录名按字典序，先目录后文件）。 */
export function flattenSearchTree(tree, hits) {
  const rows = [];
  const walk = (node, depth, prefix) => {
    const dirNames = [...node.dirs.keys()].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    for (const name of dirNames) {
      const key = prefix ? `${prefix}/${name}` : name;
      rows.push({ kind: "dir", key: `dir:${key}`, depth, name });
      walk(node.dirs.get(name), depth + 1, key);
    }
    for (const hitIndex of node.files) {
      const hit = hits[hitIndex];
      const name = hit.rel.split("/").filter(Boolean).pop() ?? hit.rel;
      rows.push({
        kind: "file",
        key: `hit:${hit.path}:${hit.line ? hit.line[0] : 0}`,
        depth,
        name,
        path: hit.path,
        line: hit.line ? hit.line[0] : null,
        snippet: hit.line ? String(hit.line[1] ?? "").trim() : null,
      });
    }
  };
  walk(tree, 0, "");
  return rows;
}
