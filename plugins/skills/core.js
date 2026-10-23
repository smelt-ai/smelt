// 技能的磁盘逻辑。设计依据见同目录的 DESIGN.md：`.agents/skills` 是本体，
// agent 目录里的软链是派生的兼容桥，软链一律是引用，不是本体。
//
// 读内容、列目录、建删目录用官方 `fs/promises`（宿主把授权根设成用户主目录）。
// 作用于软链本身的操作（lstat/rename/unlink）和官方没有的建软链、解析真实路径、
// 整树复制、原子交换来自 smelt 的通用模块 `smelt-fs`。官方的是异步的，所以这里
// 的入口都是 async：面板在事件里 await，结果缓存在视图上，render 只读缓存。

import { join, basename, dirname } from "path";
import * as fs from "fs/promises";
import { lstat, realpath, symlink, rename, unlink, exchange, copy as copyTree } from "smelt-fs";
import { pick_directory } from "smelt-workspace";
import { AGENTS } from "./agents.js";

const UNIVERSAL = ".agents/skills";
const LEGACY = ".smelt/skills";
const TRASH = ".smelt/trash/skills";

/// 跑一次可能因越权或 IO 失败而出错的操作，把结果和失败分开。
/// 越权是同步抛出的，IO 失败是 Promise 拒绝，这里两种都接住。
async function attempt(body) {
  try {
    return { ok: true, value: await body() };
  } catch (error) {
    return { ok: false, error };
  }
}

export function message(error) {
  return error && error.message ? error.message : String(error);
}

/// 软链解开后的真实路径；不存在或越权时退回原路径。
function canonical(path) {
  try {
    return realpath(path) ?? path;
  } catch {
    return path;
  }
}

/// 目录里的条目。官方 readdir 已按名字排好、但分不出软链，类型取自不跟随软链的 lstat。
async function list(dir) {
  const names = await fs.readdir(dir);
  return names.map((name) => ({ name, kind: lstat(join(dir, name))?.kind ?? "other" }));
}

function agentRelative(agent, projectScope) {
  return (projectScope ? agent.project_dir : agent.user_dir) ?? undefined;
}

/// 这台机器上在用的 agent：只看用户主目录里有没有它的配置目录。
async function installedAgents(home) {
  const installed = new Set();
  for (const agent of AGENTS) {
    const info = await attempt(() => fs.stat(join(home, agent.home)));
    if (info.ok && info.value.isDirectory()) installed.add(agent.label);
  }
  return installed;
}

/// 需要补软链的 agent：在用、有自己的目录、自己不读 .agents/skills。
function bridgeAgents(installed, projectScope) {
  return AGENTS.filter(
    (agent) =>
      agentRelative(agent, projectScope) !== undefined &&
      !agent.reads_agents_dir &&
      installed.has(agent.label),
  );
}

function parseFrontmatter(text) {
  const lines = text.split(/\r\n|\n/);
  if (lines[0]?.trim() !== "---") return {};
  const out = {};
  let pending;
  let buffer = "";
  const finish = () => {
    if (pending !== undefined) out[pending] = unquote(buffer.trim());
    pending = undefined;
    buffer = "";
  };
  for (const line of lines.slice(1)) {
    if (line.trim() === "---") break;
    if (line.startsWith(" ") || line.startsWith("\t")) {
      if (pending !== undefined) buffer += ` ${line.trim()}`;
      continue;
    }
    finish();
    const separator = line.indexOf(":");
    if (separator < 0) continue;
    const key = line.slice(0, separator).trim();
    if (key === "name" || key === "description") {
      pending = key;
      buffer = line.slice(separator + 1).trim();
    }
  }
  finish();
  return out;
}

function unquote(value) {
  if (
    value.length >= 2 &&
    ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'")))
  ) {
    return value.slice(1, -1);
  }
  return value;
}

async function readCopy(path, placement, agent) {
  const skillMd = join(path, "SKILL.md");
  const text = await attempt(() => fs.readFile(skillMd, "utf8"));
  if (!text.ok) return undefined;
  const frontmatter = parseFrontmatter(text.value);
  const name = (frontmatter.name ?? basename(path)).trim();
  if (name.length === 0) return undefined;
  const info = await attempt(() => fs.stat(skillMd));
  return {
    name,
    description: frontmatter.description ?? "",
    dir: path,
    placement,
    agent,
    modified_ms: info.ok ? info.value.mtimeMs : 0,
  };
}

async function scanRoot(root, placement, agent, projectScope, copies, links, broken) {
  const entries = await attempt(() => list(root));
  if (!entries.ok) return;
  for (const entry of entries.value) {
    // 点开头的是暂存/隐藏目录，不是 skill。
    if (entry.name.startsWith(".")) continue;
    const path = join(root, entry.name);
    // 软链一律按「引用」处理，不管它躺在哪个目录。
    if (entry.kind === "symlink") {
      let target;
      try {
        target = realpath(path);
      } catch {
        // 指向主目录外：读不了，也不能当成失效链接去清理。
        continue;
      }
      const skillMd = target === null ? undefined : await attempt(() => fs.stat(join(target, "SKILL.md")));
      if (skillMd === undefined || !skillMd.ok || !skillMd.value.isFile()) {
        broken.push({ path, project_scope: projectScope });
        continue;
      }
      links.push({ agent, placement, at: path, target });
      continue;
    }
    if (entry.kind !== "dir") continue;
    const copy = await readCopy(path, placement, agent);
    if (copy !== undefined) copies.push(copy);
  }
}

/// 一个层级里要扫的 skill 根目录，顺序同 `placementRank`。根目录本身是软链、解开后就是
/// 前面某个根目录时（如 `.claude/skills -> ../.agents/skills`），里面是同一批 skill，
/// 不另算一处存放，只在被指向的那个根目录上记下这个 agent 经整个目录读到它们。
function skillRoots(base, projectScope) {
  const candidates = [
    { path: join(base, UNIVERSAL), placement: "universal", agent: null },
    { path: join(base, LEGACY), placement: "legacy", agent: null },
    ...AGENTS.filter((agent) => agentRelative(agent, projectScope) !== undefined).map((agent) => ({
      path: join(base, agentRelative(agent, projectScope)),
      placement: "agent",
      agent: agent.label,
    })),
  ];
  const roots = [];
  for (const candidate of candidates) {
    const real = canonical(candidate.path);
    const target = roots.find((root) => root.real === real);
    if (target === undefined) roots.push({ ...candidate, real, aliases: [] });
    else if (candidate.agent !== null) target.aliases.push(candidate.agent);
  }
  return roots;
}

function placementRank(copy) {
  if (copy.placement === "universal") return 0;
  if (copy.placement === "legacy") return 1;
  return 2 + AGENTS.findIndex((agent) => agent.label === copy.agent);
}

/// 在用的 agent 里，哪些读得到这份 skill，以及怎么读到的：
/// `reads` 自己读 .agents/skills，`linked` 经兼容链接，`owner` 本体就在它的目录，
/// `missing` 该有兼容链接却没有。旧位置（smelt 私有目录）谁都读不到，是空表。
function coverageOf(primary, bridges, installed, projectScope) {
  if (primary.placement === "legacy") return [];
  const rows = [];
  for (const agent of AGENTS) {
    if (!installed.has(agent.label)) continue;
    let state;
    if (primary.placement === "agent") {
      if (agent.label === primary.agent) state = "owner";
      else if (bridges.includes(agent.label)) state = "linked";
      else continue;
    } else if (agent.reads_agents_dir) {
      state = "reads";
    } else if (bridges.includes(agent.label)) {
      state = "linked";
    } else if (agentRelative(agent, projectScope) !== undefined) {
      state = "missing";
    } else {
      continue;
    }
    rows.push({ label: agent.label, state });
  }
  return rows;
}

async function scanScope(installed, base, projectScope, output, broken) {
  const copies = [];
  const links = [];
  const roots = skillRoots(base, projectScope);
  for (const root of roots) {
    await scanRoot(root.path, root.placement, root.agent, projectScope, copies, links, broken);
  }

  // 指向本 base 之外的软链，本身就是一份独立存在的 skill。
  const known = new Set(copies.map((copy) => canonical(copy.dir)));
  for (const link of links) {
    if (known.has(link.target)) continue;
    const copy = await readCopy(link.at, link.placement, link.agent);
    if (copy !== undefined) {
      copies.push(copy);
      known.add(canonical(link.at));
    }
  }

  const grouped = new Map();
  for (const copy of copies) {
    const bucket = grouped.get(copy.name);
    if (bucket === undefined) grouped.set(copy.name, [copy]);
    else bucket.push(copy);
  }

  const needBridge = bridgeAgents(installed, projectScope);
  for (const [name, bucket] of grouped) {
    bucket.sort((left, right) => placementRank(left) - placementRank(right));
    const primary = bucket[0];
    const canonicalPrimary = canonical(primary.dir);
    const linked = links
      .filter((link) => link.target === canonicalPrimary && link.at !== primary.dir)
      .map((link) => link.agent);
    const aliases = roots.find((root) => root.path === dirname(primary.dir))?.aliases ?? [];
    const bridges = AGENTS.filter((agent) => linked.includes(agent.label) || aliases.includes(agent.label)).map(
      (agent) => agent.label,
    );
    const missing =
      primary.placement === "agent"
        ? []
        : needBridge.filter((agent) => !bridges.includes(agent.label)).map((agent) => agent.label);
    output.push({
      name,
      description: primary.description,
      project_scope: projectScope,
      base,
      dir: primary.dir,
      placement: primary.placement,
      agent: primary.agent,
      bridges,
      missing_bridges: missing,
      coverage: coverageOf(primary, bridges, installed, projectScope),
      conflicts: bucket.slice(1),
      modified_ms: primary.modified_ms,
    });
  }
}

/// 整份状态：用户级 + 项目级（项目根就是主目录时只扫一次）。
/// `installed` 是在用的 agent，给作用范围的选项用，免得每次打开表单再扫一遍。
export async function scan(home, projectRoot) {
  const installed = await installedAgents(home);
  const skills = [];
  const broken = [];
  await scanScope(installed, home, false, skills, broken);
  if (projectRoot && canonical(projectRoot) !== canonical(home)) {
    await scanScope(installed, projectRoot, true, skills, broken);
  }
  skills.sort(
    (left, right) =>
      Number(right.project_scope) - Number(left.project_scope) ||
      left.name.localeCompare(right.name),
  );
  return { skills, broken, installed: [...installed] };
}

/// 在 `at` 放一条指向 `target` 的兼容链接。实体目录绝不覆盖。
async function writeLink(at, target) {
  await fs.mkdir(dirname(at), { recursive: true });
  const existing = lstat(at);
  if (existing !== null) {
    if (existing.kind !== "symlink") throw new Error(`${at} 是实体目录，不能被兼容链接覆盖`);
    if (canonical(at) === canonical(target)) return;
    unlink(at);
  }
  symlink(target, at);
}

export async function repairBridges(home, skill) {
  if (skill.placement === "agent") throw new Error("agent 专属 skill 没有兼容链接需要修复");
  for (const agent of bridgeAgents(await installedAgents(home), skill.project_scope)) {
    if (skill.bridges.includes(agent.label)) continue;
    const root = join(skill.base, agentRelative(agent, skill.project_scope));
    await writeLink(join(root, skill.name), skill.dir);
  }
}

/// 这个 base 里指向 `target` 的软链。删本体或搬走时要一并清掉，否则留下悬空链接。
async function referencesTo(base, projectScope, target) {
  const wanted = canonical(target);
  const found = [];
  for (const { path: root } of skillRoots(base, projectScope)) {
    const entries = await attempt(() => list(root));
    if (!entries.ok) continue;
    for (const entry of entries.value) {
      const path = join(root, entry.name);
      if (entry.kind === "symlink" && path !== target && canonical(path) === wanted) {
        found.push(path);
      }
    }
  }
  return found;
}

function stamp() {
  return new Date().toISOString().replace(/:/g, "-").replace(/\..*$/, "");
}

/// 把 `from` 移进回收目录 `root`，重名时加序号。回收目录里的东西可以手动找回。
async function moveInto(root, from, name = basename(from)) {
  await fs.mkdir(root, { recursive: true });
  let destination = join(root, name);
  for (let sequence = 1; lstat(destination) !== null; sequence += 1) {
    destination = join(root, `${name}-${sequence}`);
  }
  rename(from, destination);
}

/// 删除 = 移进 `~/.smelt/trash/skills/<时间>/`，可以手动找回。
export async function trashSkill(home, skill) {
  for (const at of await referencesTo(skill.base, skill.project_scope, skill.dir)) unlink(at);
  // 本体本身是软链（指向 base 之外的那种）时，删掉的只是这条引用。
  if (lstat(skill.dir)?.kind === "symlink") {
    unlink(skill.dir);
    return;
  }
  await moveInto(join(home, TRASH, stamp()), skill.dir);
}

/// 只删这些链接本身，不动任何真实目录。
export function pruneLinks(broken) {
  for (const link of broken) unlink(link.path);
}

function pretty(home, path) {
  if (home && (path === home || path.startsWith(`${home}/`))) return `~${path.slice(home.length)}`;
  return path;
}

function scopeBase(home, projectRoot, projectScope) {
  if (projectScope) {
    if (!projectRoot) throw new Error("项目未打开，无法使用项目级 skill");
    return projectRoot;
  }
  if (!home) throw new Error("无法定位用户主目录");
  return home;
}

function validateSkillName(value) {
  const name = value.trim();
  if (name.length === 0) throw new Error("名称不能为空");
  if (name.length > 64) throw new Error("名称过长（最多 64 个字符）");
  if (!/^[A-Za-z0-9_-]+$/.test(name)) throw new Error("只能包含字母、数字、连字符(-)和下划线(_)");
  if (name.startsWith("-") || name.startsWith("_")) throw new Error("不能以 - 或 _ 开头");
  return name;
}

/// 这个层级里能单独放一份 skill 的 agent。没装过的排在后面，勾选时界面要标明。
/// `installed` 取自 `scan` 的结果，这里只做内存里的筛选和排序。
export function scopeChoices(installed, projectScope) {
  const present = new Set(installed);
  return AGENTS.filter((agent) => agentRelative(agent, projectScope) !== undefined)
    .map((agent) => ({ label: agent.label, installed: present.has(agent.label) }))
    .sort((left, right) => Number(right.installed) - Number(left.installed));
}

function normalizeRequest(request) {
  const universal = request.universal !== false;
  const selected = new Set(request.labels ?? []);
  const labels = AGENTS.filter((agent) => selected.has(agent.label)).map((agent) => agent.label);
  if (!universal && labels.length === 0) throw new Error("至少选择一个 agent，或者改用「通用」");
  return { universal, labels, project_scope: request.project_scope === true };
}

function agentRootOrReject(base, agent, projectScope) {
  const relative = agentRelative(agent, projectScope);
  if (relative === undefined) {
    throw new Error(`${agent.label} 只读取 ${UNIVERSAL}，没有可以单独存放的目录，请改用「通用」`);
  }
  return join(base, relative);
}

function destinationFor(base, name, request) {
  if (request.universal) return join(base, UNIVERSAL, name);
  const primary = AGENTS.find((agent) => agent.label === request.labels[0]);
  if (primary === undefined) throw new Error("未知的 agent");
  return join(agentRootOrReject(base, primary, request.project_scope), name);
}

/// 本体落定之后，还要被哪些路径引用。通用只给「在用、且自己不读 .agents」的 agent 打桥；
/// 指定 agent 时，第一个是本体，其余是用户明确勾上的链接。整个 skills 目录就软链到
/// 本体所在目录的 agent 已经读得到，不再建：那条链接会落进本体自己的目录。
function referencesFor(installed, base, name, request) {
  const wanted = request.universal
    ? bridgeAgents(installed, request.project_scope)
    : AGENTS.filter((agent) => request.labels.includes(agent.label) && agent.label !== request.labels[0]);
  const destinationRoot = canonical(dirname(destinationFor(base, name, request)));
  return wanted
    .map((agent) => join(agentRootOrReject(base, agent, request.project_scope), name))
    .filter((at) => canonical(dirname(at)) !== destinationRoot);
}

async function locate(home, projectRoot, dir) {
  if (!dir) throw new Error("缺少 skill 目录");
  const entry = (await scan(home, projectRoot)).skills.find((candidate) => candidate.dir === dir);
  if (entry === undefined) throw new Error("这个 skill 已经不在了，请刷新");
  return entry;
}

/// 改作用范围要做的事。预览和执行都走这里，面板上看到的就是会落盘的。
async function planScopeChange(entry, home, projectRoot, rawRequest) {
  if (entry.conflicts.length > 0) throw new Error("这个 skill 有同名副本，请先处理冲突");
  const request = normalizeRequest(rawRequest);
  const destinationBase = scopeBase(home, projectRoot, request.project_scope);
  const destination = destinationFor(destinationBase, entry.name, request);
  const occupant = lstat(destination);
  // 目标位置常常正是一条指回本体的兼容链接：realpath 会相等，但那不代表
  // 本体已经在目标位置上。只有实体目录才算「已经在这」。
  const settled =
    destination === entry.dir ||
    (occupant !== null && occupant.kind !== "symlink" && canonical(destination) === canonical(entry.dir));
  const actions = [];

  const stale = new Set([
    ...(await referencesTo(entry.base, entry.project_scope, entry.dir)),
    ...(await referencesTo(destinationBase, request.project_scope, entry.dir)),
  ]);
  const wanted = referencesFor(await installedAgents(home), destinationBase, entry.name, request);
  const wantedSet = new Set(wanted);
  for (const path of stale) {
    if (path !== destination && !wantedSet.has(path)) actions.push({ kind: "unlink", at: path });
  }

  if (!settled) {
    if (occupant !== null) {
      if (occupant.kind !== "symlink") throw new Error(`目标位置已有同名 skill：${pretty(home, destination)}`);
      actions.push({ kind: "unlink", at: destination });
    }
    actions.push({ kind: "move", from: entry.dir, to: destination });
  }

  for (const path of wanted) {
    // 已经指对了的链接不用重建，预览里也就不该出现这一步。
    if (settled && stale.has(path)) continue;
    actions.push({ kind: "link", at: path, to: destination });
  }
  return actions;
}

/// 路径落在哪个 agent 的 skills 目录里。预览按 agent 说话，用户不用懂「链接」。
function agentOfPath(path) {
  const agent = AGENTS.find((agent) =>
    [agent.user_dir, agent.project_dir].some((dir) => dir !== null && path.includes(`/${dir}/`)),
  );
  return agent ? agent.label : null;
}

function linkStep(home, at) {
  const agent = agentOfPath(at);
  return agent ? `让 ${agent} 能读到：${pretty(home, at)}` : `建立链接 ${pretty(home, at)}`;
}

function describe(home, action) {
  switch (action.kind) {
    case "move":
      return `移动 ${pretty(home, action.from)} → ${pretty(home, action.to)}`;
    case "link":
      return linkStep(home, action.at);
    case "unlink": {
      const agent = agentOfPath(action.at);
      return agent ? `${agent} 不再读：${pretty(home, action.at)}` : `移除链接 ${pretty(home, action.at)}`;
    }
    case "trash":
      return `移入回收目录 ${pretty(home, action.from)}`;
    default:
      return "未知操作";
  }
}

async function execute(home, actions) {
  let trashRoot;
  for (const action of actions) {
    switch (action.kind) {
      case "unlink":
        if (lstat(action.at)?.kind === "symlink") unlink(action.at);
        break;
      case "move":
        await fs.mkdir(dirname(action.to), { recursive: true });
        rename(action.from, action.to);
        break;
      case "link":
        await writeLink(action.at, action.to);
        break;
      case "trash":
        // 同一次操作里移走的东西放进同一个时间戳目录。
        trashRoot ??= join(home, TRASH, stamp());
        await moveInto(trashRoot, action.from);
        break;
      default:
        throw new Error("未知操作");
    }
  }
}

/// 将要执行的步骤，给面板预览。空数组表示已经是这个状态。
export async function planScope(home, projectRoot, dir, request) {
  const entry = await locate(home, projectRoot, dir);
  return (await planScopeChange(entry, home, projectRoot, request)).map((action) => describe(home, action));
}

/// 返回 skill 落定后的目录，界面据此跟到新位置。
export async function applyScope(home, projectRoot, dir, request) {
  const entry = await locate(home, projectRoot, dir);
  const normalized = normalizeRequest(request);
  await execute(home, await planScopeChange(entry, home, projectRoot, request));
  return destinationFor(scopeBase(home, projectRoot, normalized.project_scope), entry.name, normalized);
}

/// 留下 `keepDir` 那一份，其余连同指向它们的链接一起进回收目录。不合并内容。
export async function resolveConflict(home, projectRoot, dir, keepDir) {
  const entry = await locate(home, projectRoot, dir);
  if (entry.conflicts.length === 0) throw new Error("没有需要处理的同名副本");
  const copies = [entry, ...entry.conflicts];
  const keep = copies.find((copy) => copy.dir === keepDir);
  if (keep === undefined) throw new Error("选择的副本不存在");
  const actions = [];
  for (const copy of copies) {
    if (copy.dir === keep.dir) continue;
    for (const at of await referencesTo(entry.base, entry.project_scope, copy.dir)) {
      actions.push({ kind: "unlink", at });
    }
    actions.push({ kind: "trash", from: copy.dir });
  }
  await execute(home, actions);
}

async function removeEmptyDir(path) {
  if (lstat(path)?.kind !== "dir") return;
  const entries = await attempt(() => list(path));
  if (!entries.ok || entries.value.length > 0) return;
  await attempt(() => fs.rmdir(path));
}

/// 把还躺在 `.smelt/skills` 里的 skill 迁到同层级的 `.agents/skills`。
/// 同名冲突迁不动，记进 skipped，其余继续。迁空了才收掉旧目录。
export async function migrateLegacy(home, projectRoot) {
  const migrated = [];
  const skipped = [];
  const failed = new Set();
  for (let guard = 0; ; guard += 1) {
    if (guard === 1000) throw new Error("迁移次数异常，已停止");
    const entry = (await scan(home, projectRoot)).skills.find(
      (candidate) => candidate.placement === "legacy" && !failed.has(candidate.dir),
    );
    if (entry === undefined) break;
    try {
      await execute(
        home,
        await planScopeChange(entry, home, projectRoot, {
          universal: true,
          labels: [],
          project_scope: entry.project_scope,
        }),
      );
      migrated.push(entry.name);
    } catch {
      // 一个迁不动（多半是同名冲突）不该拖住其余的。
      failed.add(entry.dir);
      skipped.push(entry.name);
    }
  }
  // 同名副本归在主条目的 conflicts 里，上面的循环看不到。照实报出来，让用户先选留哪份。
  for (const entry of (await scan(home, projectRoot)).skills) {
    const blocked = entry.conflicts.some((copy) => copy.placement === "legacy");
    if (blocked && !skipped.includes(entry.name)) skipped.push(entry.name);
  }
  for (const base of [home, projectRoot].filter((base) => base)) await removeEmptyDir(join(base, LEGACY));

  const lines = [];
  if (migrated.length) lines.push(`已迁移 ${migrated.length} 个到 .agents/skills：${migrated.join("、")}`);
  if (skipped.length) {
    lines.push(
      `${skipped.length} 个迁不动：.agents/skills 里已有同名的 skill。请先处理冲突再迁：${skipped.join("、")}`,
    );
  }
  if (lines.length === 0) lines.push("没有需要迁移的 skill。");
  return { text: lines.join(" "), level: skipped.length > 0 ? "error" : "info", migrated, skipped };
}

/// 插件只能碰用户主目录里的文件，主目录外的目录在这里先拦下，给一句看得懂的话。
function requireInsideHome(home, source) {
  if (source !== home && !source.startsWith(`${home}/`)) {
    throw new Error(`只能导入用户主目录里的目录：${source}`);
  }
}

/// 所选目录的 SKILL.md：名字（校验过）和描述。
async function readSource(source) {
  const text = await attempt(() => fs.readFile(join(source, "SKILL.md"), "utf8"));
  if (!text.ok) throw new Error(`所选目录中没有可读的 SKILL.md：${message(text.error)}`);
  const frontmatter = parseFrontmatter(text.value);
  return {
    name: validateSkillName((frontmatter.name ?? basename(source)).trim()),
    description: frontmatter.description ?? "",
  };
}

async function findNamed(home, base, projectScope, name) {
  const skills = [];
  await scanScope(await installedAgents(home), base, projectScope, skills, []);
  return skills.find((skill) => skill.name === name);
}

/// 选中的目录叫什么、用户级是否已经有另一份同名 skill。
/// 源本身就是那份用户级 skill 时不算「已有」，否则把自己再导入一次也会弹出选择。
export async function inspectImport(home, source) {
  requireInsideHome(home, source);
  const { name, description } = await readSource(source);
  const existing = await findNamed(home, home, false, name);
  const sourceReal = canonical(source);
  const same =
    existing !== undefined &&
    [existing.dir, ...existing.conflicts.map((copy) => copy.dir)].some((dir) => canonical(dir) === sourceReal);
  return { name, description, has_global: existing !== undefined && !same };
}

/// 按这份作用范围导入会做什么，给导入页预览。`replaces` 表示目标已有同名目录，
/// 导入会换掉它（旧版本进回收目录）。
export async function previewImport(home, projectRoot, rawRequest, source) {
  requireInsideHome(home, source);
  const { name } = await readSource(source);
  const request = normalizeRequest(rawRequest);
  const base = scopeBase(home, projectRoot, request.project_scope);
  const destination = destinationFor(base, name, request);
  const occupant = lstat(destination);
  if (occupant !== null && occupant.kind !== "dir") {
    throw new Error(`同名目标不是可替换的 skill 目录：${pretty(home, destination)}`);
  }
  const links = referencesFor(await installedAgents(home), base, name, request);
  return {
    steps: [`复制到 ${pretty(home, destination)}`, ...links.map((at) => linkStep(home, at))],
    replaces: occupant !== null,
  };
}

/// 用户选一个目录。取消时返回 null。
export function pickSkillDirectory() {
  return pick_directory("选择要导入的 skill 目录（需含 SKILL.md）");
}

/// 返回装好后的目录，界面导入完直接打开它的详情。
export async function importSkill(home, projectRoot, rawRequest, source) {
  requireInsideHome(home, source);
  const { name } = await readSource(source);
  const request = normalizeRequest(rawRequest);
  const base = scopeBase(home, projectRoot, request.project_scope);
  const destination = destinationFor(base, name, request);
  await installTree(home, source, destination);
  const installed = await installedAgents(home);
  await execute(
    home,
    referencesFor(installed, base, name, request).map((at) => ({ kind: "link", at, to: destination })),
  );
  return destination;
}

/// 把选中的目录装成 `destination`。先复制到同目录下的暂存区：点开头，扫描会跳过；
/// 和目标同卷，交换才是原子的。目标已有实体目录时原子交换两边，已经指着目标路径
/// 的兼容链接全程不悬空，换下来的旧版本移进回收目录。复制跳过软链：软链是别人的
/// 引用，不是 skill 的内容。
async function installTree(home, source, destination) {
  const occupant = lstat(destination);
  if (occupant !== null && occupant.kind !== "dir") {
    throw new Error(`同名目标不是可替换的 skill 目录：${pretty(home, destination)}`);
  }
  if (occupant !== null && canonical(source) === canonical(destination)) {
    throw new Error("所选目录就是要安装的位置，不用再导入");
  }
  const staging = join(dirname(destination), ".staging");
  await fs.mkdir(staging, { recursive: true });
  const staged = join(staging, `import-${Date.now()}`);
  await copyTree(source, staged, { symlinks: "skip" });
  const skillMd = await attempt(() => fs.stat(join(staged, "SKILL.md")));
  if (!skillMd.ok || !skillMd.value.isFile()) {
    await moveInto(join(home, TRASH, stamp()), staged, basename(destination));
    throw new Error("复制后缺少 SKILL.md");
  }
  if (occupant === null) {
    rename(staged, destination);
  } else {
    exchange(staged, destination);
    await moveInto(join(home, TRASH, stamp()), staged, basename(destination));
  }
  await attempt(() => fs.rmdir(staging));
}
