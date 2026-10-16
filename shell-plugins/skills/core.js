// 技能的磁盘逻辑：扫描、修复兼容链接、移入回收目录、清理失效链接。
//
// 移植自 plugins/skills/bin/main.ts 的一个子集（可行性原型）。设计依据见
// plugins/skills/DESIGN.md：`.agents/skills` 是本体，agent 目录里的软链是派生的
// 兼容桥，软链一律是「引用」而不是本体。
//
// 文件操作全部经 smelt 借出的 `smelt-fs`，授权范围见 smelt-grant.json。
// 原型里不做的：作用范围迁移、导入、冲突处理、旧目录迁移、暂存 + 原子替换。

import { join, basename } from "path";
import { list, lstat, stat, realpath, read_text, symlink, rename, unlink, mkdir } from "smelt-fs";
import { AGENTS } from "./agents.js";

const UNIVERSAL = ".agents/skills";
const LEGACY = ".smelt/skills";
const TRASH = ".smelt/trash/skills";

/// 调用一次可能因越权或 IO 失败而抛错的操作，把结果和失败分开。
function attempt(body) {
  try {
    return { ok: true, value: body() };
  } catch (error) {
    return { ok: false, error };
  }
}

export function message(error) {
  return error && error.message ? error.message : String(error);
}

function agentRelative(agent, projectScope) {
  return (projectScope ? agent.project_dir : agent.user_dir) ?? undefined;
}

function readsAgentsDir(agent, projectScope) {
  const value = agent.reads_agents_dir;
  if (typeof value === "object" && value !== null) {
    return projectScope ? value.project === true : value.user === true;
  }
  return value === true;
}

/// 「这台机器上在不在用这个 agent」只看用户主目录里的配置目录。
function isInstalled(agent, home) {
  const result = attempt(() => stat(join(home, agent.home)));
  return result.ok && result.value?.kind === "dir";
}

/// 需要补软链的 agent：在用、有自己的目录、自己不读 .agents/skills。
function bridgeAgents(home, projectScope) {
  return AGENTS.filter(
    (agent) =>
      agentRelative(agent, projectScope) !== undefined &&
      !readsAgentsDir(agent, projectScope) &&
      isInstalled(agent, home),
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

function readCopy(path, placement, agent) {
  const text = attempt(() => read_text(join(path, "SKILL.md")));
  if (!text.ok || typeof text.value !== "string") return undefined;
  const frontmatter = parseFrontmatter(text.value);
  const name = (frontmatter.name ?? basename(path)).trim();
  if (name.length === 0) return undefined;
  const info = attempt(() => stat(join(path, "SKILL.md")));
  return {
    name,
    description: frontmatter.description ?? "",
    dir: path,
    placement,
    agent,
    modified_ms: info.ok && info.value ? info.value.mtime_ms : 0,
  };
}

function scanRoot(root, placement, agent, projectScope, copies, links, broken) {
  const entries = attempt(() => list(root));
  if (!entries.ok) return;
  for (const entry of entries.value) {
    // 点开头的是暂存/隐藏目录，不是 skill。
    if (entry.name.startsWith(".")) continue;
    const path = join(root, entry.name);
    // 软链一律按「引用」处理，不管它躺在哪个目录。
    if (entry.kind === "symlink") {
      const target = attempt(() => stat(join(path, "SKILL.md")));
      // 指向授权范围外：读不了，也不能当成失效链接去清理。
      if (!target.ok) continue;
      if (target.value?.kind !== "file") {
        broken.push({ path, project_scope: projectScope });
        continue;
      }
      links.push({ agent, placement, at: path, target: realpath(path) });
      continue;
    }
    if (entry.kind !== "dir") continue;
    const copy = readCopy(path, placement, agent);
    if (copy !== undefined) copies.push(copy);
  }
}

function placementRank(copy) {
  if (copy.placement === "universal") return 0;
  if (copy.placement === "legacy") return 1;
  return 2 + AGENTS.findIndex((agent) => agent.label === copy.agent);
}

function canonical(path) {
  const result = attempt(() => realpath(path));
  return result.ok && typeof result.value === "string" ? result.value : path;
}

function scanScope(home, base, projectScope, output, broken) {
  const copies = [];
  const links = [];
  scanRoot(join(base, UNIVERSAL), "universal", null, projectScope, copies, links, broken);
  scanRoot(join(base, LEGACY), "legacy", null, projectScope, copies, links, broken);
  for (const agent of AGENTS) {
    const relative = agentRelative(agent, projectScope);
    if (relative !== undefined) {
      scanRoot(join(base, relative), "agent", agent.label, projectScope, copies, links, broken);
    }
  }

  // 指向本 base 之外的软链，本身就是一份独立存在的 skill。
  const known = new Set(copies.map((copy) => canonical(copy.dir)));
  for (const link of links) {
    if (known.has(link.target)) continue;
    const copy = readCopy(link.at, link.placement, link.agent);
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

  const needBridge = bridgeAgents(home, projectScope);
  for (const [name, bucket] of grouped) {
    bucket.sort((left, right) => placementRank(left) - placementRank(right));
    const primary = bucket[0];
    const canonicalPrimary = canonical(primary.dir);
    const linked = links
      .filter((link) => link.target === canonicalPrimary && link.at !== primary.dir)
      .map((link) => link.agent);
    const bridges = AGENTS.filter((agent) => linked.includes(agent.label)).map((agent) => agent.label);
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
      conflicts: bucket.slice(1),
    });
  }
}

/// 整份状态：用户级 + 项目级（项目根就是主目录时只扫一次）。
export function scan(home, projectRoot) {
  const skills = [];
  const broken = [];
  scanScope(home, home, false, skills, broken);
  if (projectRoot && canonical(projectRoot) !== canonical(home)) {
    scanScope(home, projectRoot, true, skills, broken);
  }
  skills.sort(
    (left, right) =>
      Number(right.project_scope) - Number(left.project_scope) ||
      left.name.localeCompare(right.name),
  );
  return { skills, broken };
}

/// 在 `at` 放一条指向 `target` 的兼容链接。实体目录绝不覆盖。
function writeLink(at, target) {
  const existing = lstat(at);
  if (existing !== null) {
    if (existing.kind !== "symlink") throw new Error(`${at} 是实体目录，不能被兼容链接覆盖`);
    if (canonical(at) === canonical(target)) return;
    unlink(at);
  }
  symlink(target, at);
}

export function repairBridges(home, skill) {
  if (skill.placement === "agent") throw new Error("agent 专属 skill 没有兼容链接需要修复");
  for (const agent of bridgeAgents(home, skill.project_scope)) {
    if (skill.bridges.includes(agent.label)) continue;
    const root = join(skill.base, agentRelative(agent, skill.project_scope));
    mkdir(root);
    writeLink(join(root, skill.name), skill.dir);
  }
}

/// 所有根目录里指向 `target` 的软链。删本体时要一并清掉，否则留下悬空链接。
function collectReferences(skill) {
  const wanted = canonical(skill.dir);
  const roots = [
    join(skill.base, UNIVERSAL),
    join(skill.base, LEGACY),
    ...AGENTS.map((agent) => agentRelative(agent, skill.project_scope))
      .filter((relative) => relative !== undefined)
      .map((relative) => join(skill.base, relative)),
  ];
  const found = [];
  for (const root of roots) {
    const entries = attempt(() => list(root));
    if (!entries.ok) continue;
    for (const entry of entries.value) {
      const path = join(root, entry.name);
      if (entry.kind === "symlink" && path !== skill.dir && canonical(path) === wanted) {
        found.push(path);
      }
    }
  }
  return found;
}

function stamp() {
  return new Date().toISOString().replace(/:/g, "-").replace(/\..*$/, "");
}

/// 删除 = 移进 `~/.smelt/trash/skills/<时间>/`，可以手动找回。
export function trashSkill(home, skill) {
  for (const at of collectReferences(skill)) unlink(at);
  // 本体本身是软链（指向 base 之外的那种）时，删掉的只是这条引用。
  if (lstat(skill.dir)?.kind === "symlink") {
    unlink(skill.dir);
    return;
  }
  const root = join(home, TRASH, stamp());
  mkdir(root);
  let destination = join(root, basename(skill.dir));
  for (let sequence = 1; lstat(destination) !== null; sequence += 1) {
    destination = join(root, `${basename(skill.dir)}-${sequence}`);
  }
  rename(skill.dir, destination);
}

/// 只删这些链接本身，不动任何真实目录。
export function pruneLinks(broken) {
  for (const link of broken) unlink(link.path);
}
