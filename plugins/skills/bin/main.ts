// 技能面板的插件进程。
//
// 数据模型只有一条规矩：**磁盘就是真相**，作用范围由「skill 躺在哪个目录」表达，
// 不存任何 sidecar 元数据。
//
//   <base>/.agents/skills/<name>     通用（Agent Skills 开放标准，所有 agent 认）
//   <base>/.claude/skills/<name> …   某个 agent 专属
//   <base>/.smelt/skills/<name>      旧版 smelt 私有目录，只读 + 引导迁移
//
// base 是用户主目录（用户级）或当前项目根（项目级）。
// 对还不会自己读 .agents/skills 的 agent，从它自己的目录软链回通用目录——
// 这类链接是**派生态**，每次扫描重算，用户不需要理解它。

import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  renameSync,
  rmdirSync,
  rmSync,
  statSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, relative } from "node:path";
import { dlopen } from "bun:ffi";

const PLUGIN_ID = "com.smelt.skills";
const MANIFEST_URL = new URL("../plugin.json", import.meta.url);
const AGENTS_URL = new URL("../assets/agents.json", import.meta.url);
const STAGING_ATTEMPTS = 32;

/// 通用目录：跨 agent 的开放标准位置，也是 smelt 的 canonical 位置。
const UNIVERSAL_RELATIVE = ".agents/skills";
/// 旧版 smelt 私有目录。只读出来引导迁移，不再往里写新东西。
const LEGACY_RELATIVE = ".smelt/skills";
/// 用户覆盖 agent 能力表的位置（相对用户主目录）。
const AGENTS_OVERRIDE_RELATIVE = ".smelt/skills-agents.json";
/// 冲突处理不删数据，落到回收目录（相对用户主目录）。
const TRASH_RELATIVE = ".smelt/trash/skills";

type InvocationRequest = {
  invocation_id: string;
  contribution_id: string;
  operation: string;
  payload: unknown;
  deadline_ms: number;
};

type SharedPluginContext = {
  pluginId: string;
  dataDir: string;
};

type AgentTarget = {
  label: string;
  /// 判断「这台机器在不在用它」的配置根，例如 `.claude`、`.config/opencode`。
  /// 单独一列而不是从 skills 目录里截：两者经常对不上，
  /// 而且有的 agent 根本没有自己的 skills 目录。
  home: string;
  /// 它自有的 skills 目录；undefined 表示它没有自己的目录，只认 .agents/skills。
  userDir: string | undefined;
  projectDir: string | undefined;
  /// 会不会自己读 .agents/skills。按作用范围分开记——Antigravity 项目级读，
  /// 用户级只读 ~/.gemini/config/skills，一个 bool 表达不了。
  readsUser: boolean;
  readsProject: boolean;
};

/// universal = .agents/skills；agent = 某个 agent 自己的目录；legacy = .smelt/skills。
type Placement = "universal" | "agent" | "legacy";

type SkillCopy = {
  name: string;
  description: string;
  dir: string;
  placement: Placement;
  agent: string | null;
  modifiedMs: number;
};

/// 一条指向别处的软链。`agent` 为 null 表示它躺在通用目录或旧目录里
/// （那两处也可能有软链，早期版本就往 .agents/skills 里放过）。
type AgentLink = {
  agent: string | null;
  placement: Placement;
  at: string;
  target: string;
};

type SkillEntry = {
  name: string;
  description: string;
  projectScope: boolean;
  base: string;
  dir: string;
  placement: Placement;
  agent: string | null;
  /// 这个 skill 实际对哪些 agent 可见（专属目录本体 + 指向它的链接）。
  agents: string[];
  /// 当前存在的兼容链接。
  bridges: string[];
  /// 需要但缺失的兼容链接。
  missingBridges: string[];
  /// 同名的其它独立副本，永不自动合并内容。
  conflicts: SkillCopy[];
  modifiedMs: number;
};

type ManifestIdentity = {
  id: string;
  version: string;
};

/// 计划里的一步文件操作。plan 和 apply 走同一份数据，
/// 保证「预览里写的」就是「真会做的」。
type Action =
  | { kind: "move"; from: string; to: string }
  | { kind: "link"; at: string; to: string }
  | { kind: "unlink"; at: string }
  | { kind: "trash"; from: string };

class InvocationFailure extends Error {
  readonly code = "rejected";
  readonly retryable = false;

  constructor(message: string) {
    super(message);
    this.name = "InvocationFailure";
  }
}

const manifest = readManifest();
let stagingSequence = 0;

function readManifest(): ManifestIdentity {
  const value: unknown = JSON.parse(readFileSync(MANIFEST_URL, "utf8"));
  if (
    !isRecord(value) ||
    value.id !== PLUGIN_ID ||
    typeof value.version !== "string"
  ) {
    throw new Error("skills plugin manifest identity is invalid");
  }
  return { id: PLUGIN_ID, version: value.version };
}

// ── agent 能力表 ──────────────────────────────────────────────────────────

/// agent 名单来自配置而不是代码：某个 agent 哪天原生支持 .agents/skills，
/// 把它的 reads_agents_dir 改成 true，兼容链接就会自动收敛掉。
function readAgentTargets(): AgentTarget[] {
  const home = userHome();
  const override = home === undefined ? undefined : join(home, AGENTS_OVERRIDE_RELATIVE);
  if (override !== undefined && isFile(override)) {
    try {
      return parseAgentTargets(JSON.parse(readFileSync(override, "utf8")));
    } catch (error) {
      console.error(`[skills] ${override} 无效，回退到内置能力表：${errorMessage(error)}`);
    }
  }
  return parseAgentTargets(JSON.parse(readFileSync(AGENTS_URL, "utf8")));
}

function parseAgentTargets(value: unknown): AgentTarget[] {
  if (!isRecord(value) || !Array.isArray(value.agents)) {
    throw new Error("agent 能力表缺少 agents 数组");
  }
  const targets: AgentTarget[] = [];
  for (const raw of value.agents) {
    if (!isRecord(raw)) {
      continue;
    }
    const label = typeof raw.label === "string" ? raw.label.trim() : "";
    const home = typeof raw.home === "string" ? raw.home : "";
    const userDir = optionalDir(raw.user_dir);
    const projectDir = optionalDir(raw.project_dir);
    const reads = parseReadsAgentsDir(raw.reads_agents_dir);
    if (
      label.length === 0 ||
      !isSafeRelative(home) ||
      userDir === INVALID_DIR ||
      projectDir === INVALID_DIR ||
      // 既没有自己的目录、又不读通用目录 = 这个 agent 根本够不着任何 skill，
      // 留着只会在界面上骗人。
      (userDir === undefined && !reads.user && projectDir === undefined && !reads.project)
    ) {
      console.error(`[skills] 忽略无效的 agent 定义：${JSON.stringify(raw)}`);
      continue;
    }
    if (targets.some((target) => target.label === label)) {
      console.error(`[skills] 忽略重复的 agent：${label}`);
      continue;
    }
    targets.push({
      label,
      home,
      userDir,
      projectDir,
      readsUser: reads.user,
      readsProject: reads.project,
    });
  }
  if (targets.length === 0) {
    throw new Error("agent 能力表里没有可用的 agent");
  }
  return targets;
}

/// `null`/缺省 = 没有自有目录；字符串必须是老实的相对路径。
const INVALID_DIR = "\u0000invalid";

function optionalDir(value: unknown): string | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  return typeof value === "string" && isSafeRelative(value) ? value : INVALID_DIR;
}

function parseReadsAgentsDir(value: unknown): { user: boolean; project: boolean } {
  if (isRecord(value)) {
    return { user: value.user === true, project: value.project === true };
  }
  return { user: value === true, project: value === true };
}

/// 目录必须是老实的相对路径：能力表可被用户覆盖，不能让它指到任意位置。
function isSafeRelative(value: string): boolean {
  if (value.length === 0 || isAbsolute(value)) {
    return false;
  }
  return value.split("/").every((segment) => segment.length > 0 && segment !== "." && segment !== "..");
}

const AGENT_TARGETS = readAgentTargets();

function agentByLabel(label: string): AgentTarget | undefined {
  return AGENT_TARGETS.find((target) => target.label === label);
}

function agentRelative(target: AgentTarget, projectScope: boolean): string | undefined {
  return projectScope ? target.projectDir : target.userDir;
}

/// undefined = 这个 agent 在该作用范围里没有自己的目录（只认 .agents/skills）。
/// 调用方必须照实处理，不能兜底造一个路径出来。
function agentRoot(base: string, target: AgentTarget, projectScope: boolean): string | undefined {
  const relative = agentRelative(target, projectScope);
  return relative === undefined ? undefined : join(base, relative);
}

function readsAgentsDir(target: AgentTarget, projectScope: boolean): boolean {
  return projectScope ? target.readsProject : target.readsUser;
}

/// 「这台机器上在不在用这个 agent」——看它的配置目录存不存在。
/// 判定只看用户主目录：项目里的 `.claude/` 往往是用过之后才出现的，
/// 拿项目来判断会让第一个项目级 skill 一条桥都打不出来。
///
/// 没有这道闸，能力表一长，建一个通用 skill 就会在家目录里凭空造出
/// 十几个没人读的 `~/.xxx/skills`。
function isInstalled(target: AgentTarget): boolean {
  const home = userHome();
  return home !== undefined && isDirectory(join(home, target.home));
}

/// 需要 smelt 补软链的 agent：在用、有自己的目录、且自己不读 .agents/skills 的那些。
/// 按证据，这一档只剩 Claude Code、Kiro，以及用户级的 Antigravity——
/// 其余 agent 直接读 .agents/skills，多打的链接只会让它们把同一个 skill 数两遍。
function bridgeAgents(projectScope: boolean): AgentTarget[] {
  return AGENT_TARGETS.filter(
    (target) =>
      agentRelative(target, projectScope) !== undefined &&
      !readsAgentsDir(target, projectScope) &&
      isInstalled(target),
  );
}

// ── 基础工具 ──────────────────────────────────────────────────────────────

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function errorCode(error: unknown): string | undefined {
  return isRecord(error) && typeof error.code === "string" ? error.code : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function text(params: Record<string, unknown>, key: string): string {
  const value = params[key];
  return typeof value === "string" ? value : "";
}

function optionalText(params: Record<string, unknown>, key: string): string | undefined {
  const value = text(params, key);
  return value.length === 0 ? undefined : value;
}

function flag(params: Record<string, unknown>, key: string): boolean {
  return params[key] === true;
}

/// 缺省为 true 的开关：作用范围默认是「通用」。
function flagOrTrue(params: Record<string, unknown>, key: string): boolean {
  return params[key] !== false;
}

function index(params: Record<string, unknown>, key: string): number {
  const value = params[key];
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

function labels(params: Record<string, unknown>, key: string): string[] {
  const values = params[key];
  if (!Array.isArray(values)) {
    return [];
  }
  // 保持能力表里的顺序，主副本落点才是确定的。
  return AGENT_TARGETS.filter((target) => values.includes(target.label)).map(
    (target) => target.label,
  );
}

function metadata(path: string) {
  try {
    return statSync(path);
  } catch {
    return undefined;
  }
}

function linkMetadata(path: string) {
  try {
    return lstatSync(path);
  } catch {
    return undefined;
  }
}

function isDirectory(path: string): boolean {
  return metadata(path)?.isDirectory() === true;
}

function isFile(path: string): boolean {
  const value = metadata(path);
  return value !== undefined && !value.isDirectory();
}

function readDirectory(path: string): string[] {
  try {
    return readdirSync(path);
  } catch {
    return [];
  }
}

function canonicalize(path: string): string {
  try {
    return realpathSync(path);
  } catch {
    return path;
  }
}

function samePath(left: string, right: string): boolean {
  try {
    return realpathSync(left) === realpathSync(right);
  } catch {
    return left === right;
  }
}

function modifiedAt(path: string): number {
  return metadata(path)?.mtimeMs ?? 0;
}

function userHome(): string | undefined {
  const home = homedir();
  return typeof home === "string" && home.length > 0 ? home : undefined;
}

/// 面板里展示路径时把主目录收成 `~`，长路径才读得下去。
function pretty(path: string): string {
  const home = userHome();
  if (home !== undefined && (path === home || path.startsWith(`${home}/`))) {
    return `~${path.slice(home.length)}`;
  }
  return path;
}

function compareNames(left: string, right: string): number {
  return left.localeCompare(right, "zh-Hans-CN");
}

function validateSkillName(value: string): string {
  const name = value.trim();
  if (name.length === 0) {
    throw new InvocationFailure("名称不能为空");
  }
  if (name.length > 64) {
    throw new InvocationFailure("名称过长（最多 64 个字符）");
  }
  if (!/^[A-Za-z0-9_-]+$/.test(name)) {
    throw new InvocationFailure("只能包含字母、数字、连字符(-)和下划线(_)");
  }
  if (name.startsWith("-") || name.startsWith("_")) {
    throw new InvocationFailure("不能以 - 或 _ 开头");
  }
  return name;
}

function scopeBase(projectRoot: string | undefined, projectScope: boolean): string {
  if (projectScope) {
    if (projectRoot === undefined) {
      throw new InvocationFailure("项目未打开，无法使用项目级 skill");
    }
    return projectRoot;
  }
  const home = userHome();
  if (home === undefined) {
    throw new InvocationFailure("无法定位用户主目录");
  }
  return home;
}

function quoteScalar(value: string): string {
  return `"${value.replaceAll("\\", "\\\\").replaceAll("\"", "\\\"").replaceAll("\n", "\\n")}"`;
}

// ── 扫描 ──────────────────────────────────────────────────────────────────

function universalRoot(base: string): string {
  return join(base, UNIVERSAL_RELATIVE);
}

function legacyRoot(base: string): string {
  return join(base, LEGACY_RELATIVE);
}

function readCopy(
  path: string,
  placement: Placement,
  agent: string | null,
): SkillCopy | undefined {
  let markdown: string;
  try {
    markdown = readFileSync(join(path, "SKILL.md"), "utf8");
  } catch {
    return undefined;
  }
  const frontmatter = parseFrontmatter(markdown);
  const name = (frontmatter.name ?? basename(path)).trim();
  if (name.length === 0) {
    return undefined;
  }
  return {
    name,
    description: frontmatter.description ?? "",
    dir: path,
    placement,
    agent,
    modifiedMs: modifiedAt(join(path, "SKILL.md")),
  };
}

/// 扫一个目录：通用/旧目录里的一切都算「实体副本」，
/// agent 目录里的软链先记成链接，稍后再判断它到底指向谁。
/// 一条指向已消失目标的软链。带上作用域，「只清理这个项目」才做得到。
type BrokenLink = {
  path: string;
  projectScope: boolean;
};

function scanRoot(
  root: string,
  placement: Placement,
  agent: string | null,
  projectScope: boolean,
  copies: SkillCopy[],
  links: AgentLink[],
  broken: BrokenLink[],
): void {
  for (const name of readDirectory(root)) {
    // 点开头的是暂存/隐藏目录，不是 skill；导入过程中的半成品也在这里被挡掉。
    if (name.startsWith(".")) {
      continue;
    }
    const path = join(root, name);
    const info = linkMetadata(path);
    if (info === undefined) {
      continue;
    }
    // 软链一律按「引用」处理，不管它躺在哪个目录。
    //
    // 这里曾经写成 `info.isSymbolicLink() && agent !== null`，于是通用目录和旧目录
    // 里的软链落到下面的 isDirectory 分支（它会跟随软链）被当成本体。后果很重：
    // 同名处理时用户选中那条软链保留、真身被移进回收目录，链接立刻悬空，
    // skill 直接消失。
    if (info.isSymbolicLink()) {
      if (!isFile(join(path, "SKILL.md"))) {
        // 断链既不是 skill 也没法用。收集起来交给「清理失效链接」——
        // 只是跳过的话，它会永远躺在磁盘上，面板既看不见也够不着。
        broken.push({ path, projectScope });
        continue;
      }
      links.push({ agent, placement, at: path, target: canonicalize(path) });
      continue;
    }
    if (!isDirectory(path)) {
      continue;
    }
    const copy = readCopy(path, placement, agent);
    if (copy !== undefined) {
      copies.push(copy);
    }
  }
}

function placementRank(copy: SkillCopy): number {
  if (copy.placement === "universal") {
    return 0;
  }
  if (copy.placement === "legacy") {
    return 1;
  }
  return 2 + AGENT_TARGETS.findIndex((target) => target.label === copy.agent);
}

function scanScope(
  base: string,
  projectScope: boolean,
  output: SkillEntry[],
  broken: BrokenLink[],
): void {
  const copies: SkillCopy[] = [];
  const links: AgentLink[] = [];
  scanRoot(universalRoot(base), "universal", null, projectScope, copies, links, broken);
  scanRoot(legacyRoot(base), "legacy", null, projectScope, copies, links, broken);
  for (const target of AGENT_TARGETS) {
    const root = agentRoot(base, target, projectScope);
    if (root !== undefined) {
      scanRoot(root, "agent", target.label, projectScope, copies, links, broken);
    }
  }

  // 指向本 base 之外（或指向已消失副本）的软链，本身就是一份独立存在的 skill，
  // 不能因为「不认识」就在面板里消失。
  const known = new Set(copies.map((copy) => canonicalize(copy.dir)));
  for (const link of links) {
    if (known.has(link.target)) {
      continue;
    }
    const copy = readCopy(link.at, link.placement, link.agent);
    if (copy !== undefined) {
      copies.push(copy);
      known.add(canonicalize(link.at));
    }
  }

  const grouped = new Map<string, SkillCopy[]>();
  for (const copy of copies) {
    const bucket = grouped.get(copy.name);
    if (bucket === undefined) {
      grouped.set(copy.name, [copy]);
    } else {
      bucket.push(copy);
    }
  }

  for (const [name, bucket] of grouped) {
    bucket.sort((left, right) => placementRank(left) - placementRank(right));
    const primary = bucket[0];
    const canonicalPrimary = canonicalize(primary.dir);
    const linkedAgents = links
      .filter((link) => link.target === canonicalPrimary && link.at !== primary.dir)
      .map((link) => link.agent);
    const bridges = AGENT_TARGETS.filter((target) => linkedAgents.includes(target.label)).map(
      (target) => target.label,
    );
    // 「对谁可见」按实际磁盘算：原生读通用目录的 agent 直接可见，
    // 其余靠兼容链接，缺链接就是真的看不到，不能粉饰。
    const visible = AGENT_TARGETS.filter((target) => {
      if (bridges.includes(target.label)) {
        return true;
      }
      if (primary.placement === "agent") {
        return target.label === primary.agent;
      }
      return primary.placement === "universal" && readsAgentsDir(target, projectScope);
    }).map((target) => target.label);
    const missingBridges =
      primary.placement === "agent"
        ? []
        : bridgeAgents(projectScope)
            .filter((target) => !bridges.includes(target.label))
            .map((target) => target.label);
    output.push({
      name,
      description: primary.description,
      projectScope,
      base,
      dir: primary.dir,
      placement: primary.placement,
      agent: primary.agent,
      agents: visible,
      bridges,
      missingBridges,
      conflicts: bucket.slice(1),
      modifiedMs: primary.modifiedMs,
    });
  }
}

function scanSkills(projectRoot: string | undefined, broken: BrokenLink[] = []): SkillEntry[] {
  const skills: SkillEntry[] = [];
  const home = userHome();
  if (home !== undefined) {
    scanScope(home, false, skills, broken);
  }
  // 项目根就是主目录时两个作用域重叠，扫一次即可，否则同一份会被算成冲突。
  if (projectRoot !== undefined && (home === undefined || !samePath(projectRoot, home))) {
    scanScope(projectRoot, true, skills, broken);
  }
  skills.sort(
    (left, right) =>
      Number(right.projectScope) - Number(left.projectScope) || compareNames(left.name, right.name),
  );
  return skills;
}

function locate(projectRoot: string | undefined, directory: string): SkillEntry {
  if (directory.length === 0) {
    throw new InvocationFailure("缺少 skill 目录");
  }
  const entry = scanSkills(projectRoot).find((candidate) => candidate.dir === directory);
  if (entry === undefined) {
    throw new InvocationFailure("这个 skill 已经不在了，请刷新");
  }
  return entry;
}

// ── 文件原语 ──────────────────────────────────────────────────────────────

function stagedPath(root: string, name: string, purpose: string): string {
  const sequence = stagingSequence;
  stagingSequence += 1;
  return join(root, `.${name}-${purpose}-${process.pid}-${sequence}`);
}

function createStagedSkillDirectory(root: string, name: string): string {
  for (let attempt = 0; attempt < STAGING_ATTEMPTS; attempt += 1) {
    const directory = stagedPath(root, name, "import");
    try {
      mkdirSync(directory);
      return directory;
    } catch (error) {
      if (errorCode(error) !== "EEXIST") {
        throw error;
      }
    }
  }
  throw new Error("无法创建唯一的临时路径");
}

function createStagedAgentLink(root: string, name: string, target: string): string {
  for (let attempt = 0; attempt < STAGING_ATTEMPTS; attempt += 1) {
    const link = stagedPath(root, name, "link");
    try {
      symlinkSync(target, link);
      return link;
    } catch (error) {
      if (errorCode(error) !== "EEXIST") {
        throw error;
      }
    }
  }
  throw new Error("无法创建唯一的临时链接");
}

function openLinuxLibc() {
  let muslLibrary = "libc.musl-x86_64.so.1";
  if (process.arch === "arm64") {
    muslLibrary = "libc.musl-aarch64.so.1";
  } else if (process.arch === "arm") {
    muslLibrary = "libc.musl-armv7.so.1";
  } else if (process.arch === "riscv64") {
    muslLibrary = "libc.musl-riscv64.so.1";
  }
  let lastError: unknown;
  for (const libraryPath of ["libc.so.6", muslLibrary]) {
    try {
      return dlopen(libraryPath, {
        renameat2: {
          args: ["i32", "cstring", "i32", "cstring", "u32"],
          returns: "i32",
        },
      });
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError;
}

function exchangePaths(staged: string, destination: string): void {
  // Keep the legacy all-or-nothing replacement contract; plain rename cannot exchange paths.
  if (process.platform === "darwin") {
    const library = dlopen("/usr/lib/libSystem.B.dylib", {
      renamex_np: {
        args: ["cstring", "cstring", "u32"],
        returns: "i32",
      },
    });
    try {
      if (library.symbols.renamex_np(staged, destination, 2) !== 0) {
        throw new Error("原子交换 skill 路径失败");
      }
    } finally {
      library.close();
    }
    return;
  }
  if (process.platform === "linux") {
    const library = openLinuxLibc();
    try {
      if (library.symbols.renameat2(-100, staged, -100, destination, 2) !== 0) {
        throw new Error("原子交换 skill 路径失败");
      }
    } finally {
      library.close();
    }
    return;
  }
  throw new Error("当前平台不支持无中断替换同名 skill");
}

function installStagedEntry(staged: string, destination: string, replacing: boolean): string | null {
  if (!replacing) {
    renameSync(staged, destination);
    return null;
  }
  exchangePaths(staged, destination);
  return staged;
}

function removeSkillEntry(path: string): void {
  const value = linkMetadata(path);
  if (value === undefined) {
    throw new Error("文件不存在");
  }
  if (value.isSymbolicLink() || !value.isDirectory()) {
    unlinkSync(path);
  } else {
    rmSync(path, { recursive: true });
  }
}

function copyDirectoryRecursive(source: string, destination: string): void {
  mkdirSync(destination, { recursive: true });
  for (const name of readdirSync(source)) {
    const from = join(source, name);
    const to = join(destination, name);
    const entry = linkMetadata(from);
    if (entry === undefined || entry.isSymbolicLink()) {
      continue;
    }
    if (entry.isDirectory()) {
      copyDirectoryRecursive(from, to);
    } else {
      copyFileSync(from, to);
    }
  }
}

/// 同盘用 rename（原子），跨盘退化成复制再删。
function movePath(from: string, to: string): void {
  mkdirSync(dirname(to), { recursive: true });
  try {
    renameSync(from, to);
    return;
  } catch (error) {
    if (errorCode(error) !== "EXDEV") {
      throw error;
    }
  }
  copyDirectoryRecursive(from, to);
  rmSync(from, { recursive: true });
}

function writeLink(at: string, target: string): void {
  const root = dirname(at);
  mkdirSync(root, { recursive: true });
  const existing = linkMetadata(at);
  if (existing !== undefined) {
    if (!existing.isSymbolicLink()) {
      throw new InvocationFailure(`${pretty(at)} 是实体目录，不能被兼容链接覆盖`);
    }
    if (canonicalize(at) === canonicalize(target)) {
      return;
    }
  }
  const staged = createStagedAgentLink(root, basename(at), target);
  try {
    if (existing !== undefined) {
      unlinkSync(at);
    }
    renameSync(staged, at);
  } catch (error) {
    try {
      unlinkSync(staged);
    } catch {
      // 收尽力而为：临时链接留下来也不会被当成 skill。
    }
    throw new InvocationFailure(`创建链接 ${pretty(at)} 失败：${errorMessage(error)}`);
  }
}

function trashRoot(): string {
  const home = userHome();
  if (home === undefined) {
    throw new InvocationFailure("无法定位用户主目录");
  }
  const stamp = new Date().toISOString().replaceAll(":", "-").replace(/\..*$/, "");
  const root = join(home, TRASH_RELATIVE, stamp);
  mkdirSync(root, { recursive: true });
  return root;
}

function uniqueTrashPath(root: string, name: string): string {
  let candidate = join(root, name);
  let sequence = 1;
  while (existsSync(candidate)) {
    candidate = join(root, `${name}-${sequence}`);
    sequence += 1;
  }
  return candidate;
}

// ── 计划：所有「改作用范围」的操作都走同一条路 ────────────────────────────

function collectReferences(base: string, projectScope: boolean, target: string): string[] {
  const canonical = canonicalize(target);
  const found: string[] = [];
  // 通用目录和旧目录也要扫：漏掉它们，删本体时留在那儿的软链就成了悬空链接。
  const roots = [
    universalRoot(base),
    legacyRoot(base),
    ...AGENT_TARGETS.map((agent) => agentRoot(base, agent, projectScope)),
  ];
  for (const root of roots) {
    if (root === undefined) {
      continue;
    }
    for (const name of readDirectory(root)) {
      const path = join(root, name);
      if (path === target) {
        continue;
      }
      if (linkMetadata(path)?.isSymbolicLink() === true && canonicalize(path) === canonical) {
        found.push(path);
      }
    }
  }
  return found;
}

type ScopeRequest = {
  universal: boolean;
  labels: string[];
  projectScope: boolean;
};

function readScopeRequest(
  params: Record<string, unknown>,
  fallbackProjectScope: boolean,
): ScopeRequest {
  const universal = flagOrTrue(params, "universal");
  const selected = labels(params, "labels");
  if (!universal && selected.length === 0) {
    throw new InvocationFailure("至少选择一个 agent，或者改用「通用」");
  }
  return {
    universal,
    labels: selected,
    projectScope: params.project_scope === undefined ? fallbackProjectScope : flag(params, "project_scope"),
  };
}

function destinationFor(
  base: string,
  name: string,
  request: ScopeRequest,
): string {
  if (request.universal) {
    return join(universalRoot(base), name);
  }
  const primary = agentByLabel(request.labels[0]);
  if (primary === undefined) {
    throw new InvocationFailure("未知的 agent");
  }
  return join(agentRootOrReject(base, primary, request.projectScope), name);
}

/// 有些 agent（Codex、Grok）压根没有自己的 skills 目录，只认 .agents/skills，
/// 所以「只给它一个人用」这件事对它们不成立——直说，不要凭空造个目录糊弄过去。
function agentRootOrReject(base: string, target: AgentTarget, projectScope: boolean): string {
  const root = agentRoot(base, target, projectScope);
  if (root === undefined) {
    throw new InvocationFailure(
      `${target.label} 只读取 ${UNIVERSAL_RELATIVE}，没有可以单独存放的目录，请改用「通用」`,
    );
  }
  return root;
}

/// 目标位置确定后，本体应该被哪些路径引用。
function referencesFor(base: string, name: string, request: ScopeRequest): string[] {
  const wanted = request.universal
    ? bridgeAgents(request.projectScope)
    : AGENT_TARGETS.filter(
        (target) => request.labels.includes(target.label) && target.label !== request.labels[0],
      );
  return wanted.map((target) =>
    join(agentRootOrReject(base, target, request.projectScope), name),
  );
}

function planScopeChange(
  entry: SkillEntry,
  projectRoot: string | undefined,
  request: ScopeRequest,
): Action[] {
  if (entry.conflicts.length > 0) {
    throw new InvocationFailure("这个 skill 有同名副本，请先处理冲突");
  }
  const destinationBase = scopeBase(projectRoot, request.projectScope);
  const destination = destinationFor(destinationBase, entry.name, request);
  const occupant = linkMetadata(destination);
  // 目标位置常常正是一条指回本体的兼容链接：realpath 会相等，但那不代表
  // 本体已经在目标位置上。只有实体目录才算「已经在这」。
  const settled =
    destination === entry.dir ||
    (occupant !== undefined &&
      !occupant.isSymbolicLink() &&
      canonicalize(destination) === canonicalize(entry.dir));
  const actions: Action[] = [];

  // 先拆掉所有旧引用（含换作用域时留在原 base 的），再谈搬家。
  const stale = new Set<string>([
    ...collectReferences(entry.base, entry.projectScope, entry.dir),
    ...collectReferences(destinationBase, request.projectScope, entry.dir),
  ]);
  const wanted = referencesFor(destinationBase, entry.name, request);
  const wantedSet = new Set(wanted);
  for (const path of stale) {
    if (path !== destination && !wantedSet.has(path)) {
      actions.push({ kind: "unlink", at: path });
    }
  }

  if (!settled) {
    if (occupant !== undefined) {
      if (!occupant.isSymbolicLink()) {
        throw new InvocationFailure(`目标位置已有同名 skill：${pretty(destination)}`);
      }
      actions.push({ kind: "unlink", at: destination });
    }
    actions.push({ kind: "move", from: entry.dir, to: destination });
  }

  for (const path of wanted) {
    // 已经指对了的链接不用重建，预览里也就不该出现这一步。
    if (settled && stale.has(path)) {
      continue;
    }
    actions.push({ kind: "link", at: path, to: destination });
  }
  return actions;
}

function describe(action: Action): string {
  switch (action.kind) {
    case "move":
      return `移动 ${pretty(action.from)} → ${pretty(action.to)}`;
    case "link":
      return `建立兼容链接 ${pretty(action.at)}`;
    case "unlink":
      return `移除链接 ${pretty(action.at)}`;
    case "trash":
      return `移入回收目录 ${pretty(action.from)}`;
  }
}

function execute(actions: Action[]): void {
  let trash: string | undefined;
  for (const action of actions) {
    switch (action.kind) {
      case "unlink":
        if (linkMetadata(action.at)?.isSymbolicLink() === true) {
          try {
            unlinkSync(action.at);
          } catch (error) {
            console.error(`[skills] 移除链接 ${action.at} 失败：${errorMessage(error)}`);
          }
        }
        break;
      case "move":
        try {
          movePath(action.from, action.to);
        } catch (error) {
          throw new InvocationFailure(
            `移动到 ${pretty(action.to)} 失败：${errorMessage(error)}`,
          );
        }
        break;
      case "link":
        writeLink(action.at, action.to);
        break;
      case "trash": {
        trash ??= trashRoot();
        const destination = uniqueTrashPath(trash, basename(action.from));
        try {
          movePath(action.from, destination);
        } catch (error) {
          throw new InvocationFailure(
            `移入回收目录失败：${pretty(action.from)}：${errorMessage(error)}`,
          );
        }
        break;
      }
    }
  }
}

// ── 操作 ──────────────────────────────────────────────────────────────────

function createSkill(
  projectRoot: string | undefined,
  request: ScopeRequest,
  rawName: string,
  rawDescription: string,
): string {
  const name = validateSkillName(rawName);
  const base = scopeBase(projectRoot, request.projectScope);
  const directory = destinationFor(base, name, request);
  if (existsSync(directory)) {
    throw new InvocationFailure(`已存在同名 skill：${pretty(directory)}`);
  }
  try {
    mkdirSync(directory, { recursive: true });
  } catch (error) {
    throw new InvocationFailure(`创建目录失败：${errorMessage(error)}`);
  }
  const description = rawDescription.trim();
  const content =
    `---\nname: ${quoteScalar(name)}\ndescription: ${quoteScalar(description)}\n---\n\n` +
    `# ${name}\n\n在这里写这个 skill 的具体使用说明和步骤。\n`;
  try {
    writeFileSync(join(directory, "SKILL.md"), content);
  } catch (error) {
    throw new InvocationFailure(`写入 SKILL.md 失败：${errorMessage(error)}`);
  }
  execute(referencesFor(base, name, request).map((at) => ({ kind: "link", at, to: directory })));
  return directory;
}

function updateSkill(entry: SkillEntry, rawName: string, rawDescription: string): string {
  const name = validateSkillName(rawName);
  const markdownPath = join(entry.dir, "SKILL.md");
  let textValue: string;
  try {
    textValue = readFileSync(markdownPath, "utf8");
  } catch (error) {
    throw new InvocationFailure(`读取 SKILL.md 失败：${errorMessage(error)}`);
  }
  try {
    writeFileSync(markdownPath, rewriteFrontmatter(textValue, name, rawDescription.trim()));
  } catch (error) {
    throw new InvocationFailure(`写入 SKILL.md 失败：${errorMessage(error)}`);
  }
  const currentName = basename(entry.dir);
  if (currentName === name) {
    return entry.dir;
  }
  const newDirectory = join(dirname(entry.dir), name);
  if (existsSync(newDirectory)) {
    throw new InvocationFailure(`已存在同名 skill：${pretty(newDirectory)}`);
  }
  const references = collectReferences(entry.base, entry.projectScope, entry.dir);
  try {
    renameSync(entry.dir, newDirectory);
  } catch (error) {
    throw new InvocationFailure(`重命名目录失败：${errorMessage(error)}`);
  }
  // 改名后旧链接名字也不对了：拆掉重建，链接名跟着目录名走。
  execute([
    ...references.map((at): Action => ({ kind: "unlink", at })),
    ...references.map((at): Action => ({
      kind: "link",
      at: join(dirname(at), name),
      to: newDirectory,
    })),
  ]);
  return newDirectory;
}

/// `all` = 连同同名副本一起删。默认只删这一份：两份副本内容可能不同，
/// 替用户做主是危险的。但只删一份时列表里那一行不会消失（副本顶上来），
/// 所以调用方必须先把这件事讲清楚，否则看起来就像「点了没反应」。
function deleteSkill(entry: SkillEntry, all: boolean): void {
  const targets = all ? [entry.dir, ...entry.conflicts.map((copy) => copy.dir)] : [entry.dir];
  for (const dir of targets) {
    if (!isFile(join(dir, "SKILL.md"))) {
      throw new InvocationFailure("目录中没有 SKILL.md，拒绝删除");
    }
  }
  for (const dir of targets) {
    execute(
      collectReferences(entry.base, entry.projectScope, dir).map((at): Action => ({
        kind: "unlink",
        at,
      })),
    );
    try {
      removeSkillEntry(dir);
    } catch (error) {
      throw new InvocationFailure(`删除失败：${errorMessage(error)}`);
    }
  }
}

function repairBridges(entry: SkillEntry): void {
  if (entry.placement === "agent") {
    throw new InvocationFailure("agent 专属 skill 没有兼容链接需要修复");
  }
  const request: ScopeRequest = {
    universal: true,
    labels: [],
    projectScope: entry.projectScope,
  };
  execute(
    referencesFor(entry.base, entry.name, request).map((at): Action => ({
      kind: "link",
      at,
      to: entry.dir,
    })),
  );
}

function resolveConflict(entry: SkillEntry, keepIndex: number): void {
  if (entry.conflicts.length === 0) {
    throw new InvocationFailure("没有需要处理的同名副本");
  }
  const copies: SkillCopy[] = [
    {
      name: entry.name,
      description: entry.description,
      dir: entry.dir,
      placement: entry.placement,
      agent: entry.agent,
      modifiedMs: entry.modifiedMs,
    },
    ...entry.conflicts,
  ];
  const keep = copies[keepIndex];
  if (keep === undefined) {
    throw new InvocationFailure("选择的副本不存在");
  }
  const actions: Action[] = [];
  for (const copy of copies) {
    if (copy.dir === keep.dir) {
      continue;
    }
    for (const at of collectReferences(entry.base, entry.projectScope, copy.dir)) {
      actions.push({ kind: "unlink", at });
    }
    // 冲突永不自动合并内容，落盘的东西一律进回收目录，可人工找回。
    actions.push({ kind: "trash", from: copy.dir });
  }
  execute(actions);
}

function migrateLegacy(
  projectRoot: string | undefined,
  onlyProjectScope: boolean | undefined,
): { migrated: string[]; skipped: string[] } {
  const migrated: string[] = [];
  const skipped: string[] = [];
  const failed = new Set<string>();
  // 每迁一个就重扫：搬完一个之后其余条目的路径与冲突关系都可能变了。
  for (;;) {
    const entry = scanSkills(projectRoot).find(
      (candidate) =>
        candidate.placement === "legacy" &&
        !failed.has(candidate.dir) &&
        (onlyProjectScope === undefined || candidate.projectScope === onlyProjectScope),
    );
    if (entry === undefined) {
      break;
    }
    try {
      execute(
        planScopeChange(entry, projectRoot, {
          universal: true,
          labels: [],
          projectScope: entry.projectScope,
        }),
      );
      migrated.push(entry.name);
    } catch (error) {
      // 一个迁不动（多半是同名冲突）不该拖住其余的，记下来继续。
      console.error(`[skills] 迁移 ${entry.name} 失败：${errorMessage(error)}`);
      failed.add(entry.dir);
      skipped.push(entry.name);
    }
  }
  // 同名副本会被归到 primary 的 conflicts 里，placement 不是 "legacy"，
  // 上面那个循环永远找不到它们。横幅计数却把它们算了进去——口径不一致的结果就是
  // 「点了迁移什么都没发生」。这里照实报出来，交给用户走冲突处理。
  for (const entry of scanSkills(projectRoot)) {
    if (onlyProjectScope !== undefined && entry.projectScope !== onlyProjectScope) {
      continue;
    }
    const blocked = entry.conflicts.some((copy) => copy.placement === "legacy");
    if (blocked && !skipped.includes(entry.name)) {
      skipped.push(entry.name);
    }
  }

  // 迁空了就把私有目录也收掉，别留一个空壳继续误导人。
  const bases = [userHome(), projectRoot].filter(
    (base): base is string => base !== undefined,
  );
  for (const base of bases) {
    try {
      rmdirSync(legacyRoot(base));
    } catch {
      // 还有东西没迁走（或本来就没有这个目录），留着即可。
    }
  }
  return { migrated, skipped };
}

function skillNameFromDirectory(source: string): string {
  let markdown: string;
  try {
    markdown = readFileSync(join(source, "SKILL.md"), "utf8");
  } catch {
    throw new InvocationFailure("所选目录中没有 SKILL.md");
  }
  return validateSkillName((parseFrontmatter(markdown).name ?? basename(source)).trim());
}

function scopeContainsSkill(base: string, projectScope: boolean, name: string): boolean {
  const entries: SkillEntry[] = [];
  scanScope(base, projectScope, entries, []);
  return entries.some((entry) => entry.name === name);
}

/// 导入：复制到暂存目录后再原子安装，替换同名 skill 时链接始终有效。
function importSkill(
  projectRoot: string | undefined,
  request: ScopeRequest,
  source: string,
): string {
  const name = skillNameFromDirectory(source);
  const base = scopeBase(projectRoot, request.projectScope);
  const destination = destinationFor(base, name, request);
  const root = dirname(destination);
  try {
    mkdirSync(root, { recursive: true });
  } catch (error) {
    throw new InvocationFailure(`创建目标目录失败：${errorMessage(error)}`);
  }
  const stagingRoot = join(root, ".staging");
  try {
    mkdirSync(stagingRoot, { recursive: true });
  } catch (error) {
    throw new InvocationFailure(`创建导入暂存区失败：${errorMessage(error)}`);
  }
  const destinationInfo = linkMetadata(destination);
  if (
    destinationInfo !== undefined &&
    (destinationInfo.isSymbolicLink() || !destinationInfo.isDirectory())
  ) {
    throw new InvocationFailure(`同名目标不是可替换的 skill 目录：${pretty(destination)}`);
  }
  const replacing = destinationInfo !== undefined;
  let staged: string;
  try {
    staged = createStagedSkillDirectory(stagingRoot, name);
  } catch (error) {
    throw new InvocationFailure(`创建导入临时目录失败：${errorMessage(error)}`);
  }
  try {
    copyDirectoryRecursive(source, staged);
  } catch (error) {
    try {
      rmSync(staged, { recursive: true });
    } catch {
      // 导入失败必须保住原有 skill，暂存清理尽力而为。
    }
    throw new InvocationFailure(`复制目录失败：${errorMessage(error)}`);
  }
  if (!isFile(join(staged, "SKILL.md"))) {
    try {
      rmSync(staged, { recursive: true });
    } catch {
      // 同上。
    }
    throw new InvocationFailure("复制后缺少 SKILL.md");
  }
  let oldDirectory: string | null;
  try {
    oldDirectory = installStagedEntry(staged, destination, replacing);
  } catch (error) {
    try {
      rmSync(staged, { recursive: true });
    } catch {
      // 同上。
    }
    throw new InvocationFailure(`安装导入的 skill 失败：${errorMessage(error)}`);
  }
  if (oldDirectory !== null) {
    try {
      rmSync(oldDirectory, { recursive: true });
    } catch (error) {
      console.error(`[skills] 导入后清理旧版本 ${oldDirectory} 失败：${errorMessage(error)}`);
    }
  }
  try {
    // 暂存区是实现细节，别留在 .agents/skills 这种公共目录里当垃圾。
    rmdirSync(stagingRoot);
  } catch {
    // 还有并发导入在用就留着，下次自然会清掉。
  }
  execute(referencesFor(base, name, request).map((at): Action => ({ kind: "link", at, to: destination })));
  return destination;
}

// ── frontmatter ───────────────────────────────────────────────────────────

function textLines(value: string): string[] {
  const lines = value.split(/\r\n|\n/);
  if (lines.length > 1 && lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines;
}

function parseFrontmatter(textValue: string): { name?: string; description?: string } {
  const lines = textLines(textValue);
  if (lines[0]?.trim() !== "---") {
    return {};
  }
  let name: string | undefined;
  let description: string | undefined;
  let pending: "name" | "description" | undefined;
  let buffer = "";
  const finishPending = () => {
    if (pending === undefined) {
      return;
    }
    if (pending === "name") {
      name = unquote(buffer.trim());
    } else {
      description = unquote(buffer.trim());
    }
    pending = undefined;
    buffer = "";
  };
  for (const line of lines.slice(1)) {
    if (line.trim() === "---") {
      break;
    }
    if (line.startsWith(" ") || line.startsWith("\t")) {
      if (pending !== undefined) {
        buffer += ` ${line.trim()}`;
      }
      continue;
    }
    finishPending();
    const separator = line.indexOf(":");
    if (separator < 0) {
      continue;
    }
    const key = line.slice(0, separator).trim();
    if (key === "name" || key === "description") {
      pending = key;
      buffer = line.slice(separator + 1).trim();
    }
  }
  finishPending();
  return { name, description };
}

function unquote(value: string): string {
  const trimmed = value.trim();
  if (
    trimmed.length >= 2 &&
    ((trimmed.startsWith("\"") && trimmed.endsWith("\"")) ||
      (trimmed.startsWith("'") && trimmed.endsWith("'")))
  ) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

function rewriteFrontmatter(textValue: string, name: string, description: string): string {
  const header = `---\nname: ${quoteScalar(name)}\ndescription: ${quoteScalar(description)}\n`;
  const lines = textLines(textValue);
  if (lines[0]?.trim() !== "---") {
    return `${header}---\n\n${textValue}`;
  }
  const endIndex = lines.findIndex((line, position) => position > 0 && line.trim() === "---");
  if (endIndex < 0) {
    return `${header}---\n\n${textValue}`;
  }
  const kept: string[] = [];
  for (let position = 1; position < endIndex; ) {
    const line = lines[position];
    const separator = line.indexOf(":");
    const key = separator < 0 || line.startsWith(" ") || line.startsWith("\t")
      ? undefined
      : line.slice(0, separator).trim();
    if (key === "name" || key === "description") {
      position += 1;
      while (
        position < endIndex &&
        (lines[position].startsWith(" ") || lines[position].startsWith("\t"))
      ) {
        position += 1;
      }
      continue;
    }
    kept.push(line);
    position += 1;
  }
  let output = header;
  for (const line of kept) {
    output += `${line}\n`;
  }
  output += "---\n";
  const body = lines.slice(endIndex + 1).join("\n").replace(/^\n+/, "");
  if (body.length > 0) {
    output += `\n${body}`;
    if (!output.endsWith("\n")) {
      output += "\n";
    }
  }
  return output;
}

// ── 序列化与分发 ──────────────────────────────────────────────────────────

function copyJson(copy: SkillCopy): Record<string, unknown> {
  return {
    dir: copy.dir,
    display_dir: pretty(copy.dir),
    description: copy.description,
    placement: copy.placement,
    agent: copy.agent,
    modified_ms: copy.modifiedMs,
  };
}

function entryJson(entry: SkillEntry): Record<string, unknown> {
  return {
    name: entry.name,
    description: entry.description,
    project_scope: entry.projectScope,
    dir: entry.dir,
    display_dir: pretty(entry.dir),
    base: entry.base,
    placement: entry.placement,
    agent: entry.agent,
    agents: entry.agents,
    bridges: entry.bridges,
    missing_bridges: entry.missingBridges,
    modified_ms: entry.modifiedMs,
    conflicts: entry.conflicts.map(copyJson),
  };
}

function state(projectRoot: string | undefined): Record<string, unknown> {
  const broken: BrokenLink[] = [];
  const skills = scanSkills(projectRoot, broken);
  return {
    project_root: projectRoot ?? null,
    agents: AGENT_TARGETS.map((target) => ({
      label: target.label,
      installed: isInstalled(target),
      user_dir: target.userDir ?? null,
      project_dir: target.projectDir ?? null,
      reads_agents_dir: { user: target.readsUser, project: target.readsProject },
    })),
    universal_dir: UNIVERSAL_RELATIVE,
    // 指向已消失目标的软链。它们不属于任何 skill，面板够不着，
    // 只能单独报出来让用户一键清掉。
    broken_links: broken.map((link) => ({
      path: link.path,
      display_path: pretty(link.path),
      project_scope: link.projectScope,
    })),
    // 冲突里藏着的旧副本同样要计数，否则横幅会漏报。
    legacy_count: skills.filter(
      (entry) =>
        entry.placement === "legacy" ||
        entry.conflicts.some((copy) => copy.placement === "legacy"),
    ).length,
    skills: skills.map(entryJson),
  };
}

function handle(request: InvocationRequest): Record<string, unknown> {
  if (request.operation !== "panel.message") {
    throw new InvocationFailure("unsupported invocation operation");
  }
  const params = isRecord(request.payload) ? request.payload : {};
  const projectRoot = optionalText(params, "project_root");
  const operation = text(params, "op");
  let result: Record<string, unknown>;

  switch (operation) {
    case "state":
      result = state(projectRoot);
      break;
    case "create":
      createSkill(
        projectRoot,
        readScopeRequest(params, false),
        text(params, "name"),
        text(params, "description"),
      );
      result = state(projectRoot);
      break;
    case "update": {
      const entry = locate(projectRoot, text(params, "dir"));
      updateSkill(entry, text(params, "name"), text(params, "description"));
      result = state(projectRoot);
      break;
    }
    case "delete": {
      const entry = locate(projectRoot, text(params, "dir"));
      deleteSkill(entry, flag(params, "all"));
      result = state(projectRoot);
      break;
    }
    case "scope.plan": {
      const entry = locate(projectRoot, text(params, "dir"));
      const actions = planScopeChange(entry, projectRoot, readScopeRequest(params, entry.projectScope));
      result = {
        steps: actions.map(describe),
        // 只改链接不动本体时如实说明，用户不会以为文件要搬家。
        moves: actions.some((action) => action.kind === "move"),
      };
      break;
    }
    case "scope.apply": {
      const entry = locate(projectRoot, text(params, "dir"));
      execute(planScopeChange(entry, projectRoot, readScopeRequest(params, entry.projectScope)));
      result = state(projectRoot);
      break;
    }
    case "links.prune": {
      // 和 legacy.migrate 一样：不带 project_scope 就两个层级一起清。
      const scope = params.project_scope === undefined ? undefined : flag(params, "project_scope");
      const broken: BrokenLink[] = [];
      scanSkills(projectRoot, broken);
      const doomed = broken.filter(
        (link) => scope === undefined || link.projectScope === scope,
      );
      execute(doomed.map((link): Action => ({ kind: "unlink", at: link.path })));
      result = { ...state(projectRoot), pruned: doomed.map((link) => pretty(link.path)) };
      break;
    }
    case "bridges.repair": {
      const entry = locate(projectRoot, text(params, "dir"));
      repairBridges(entry);
      result = state(projectRoot);
      break;
    }
    case "conflicts.resolve": {
      const entry = locate(projectRoot, text(params, "dir"));
      resolveConflict(entry, index(params, "keep_index"));
      result = state(projectRoot);
      break;
    }
    case "legacy.migrate": {
      // 不带 project_scope 就是两个层级一起迁；带了就只迁那一层。
      const scope = params.project_scope === undefined ? undefined : flag(params, "project_scope");
      const outcome = migrateLegacy(projectRoot, scope);
      result = { ...state(projectRoot), migrated: outcome.migrated, skipped: outcome.skipped };
      break;
    }
    case "import.inspect": {
      const source = text(params, "source");
      const name = skillNameFromDirectory(source);
      const home = userHome();
      result = {
        name,
        has_global: home !== undefined && scopeContainsSkill(home, false, name),
      };
      break;
    }
    case "import":
      importSkill(projectRoot, readScopeRequest(params, false), text(params, "source"));
      result = state(projectRoot);
      break;
    default:
      throw new InvocationFailure(`未知的面板 op：${operation}`);
  }
  return result;
}

const sharedPlugin = {
  invoke(request: InvocationRequest, context: SharedPluginContext) {
    if (context.pluginId !== manifest.id) {
      throw new Error("shared bun host supplied a mismatched skills plugin identity");
    }
    try {
      const operation = isRecord(request.payload) ? text(request.payload, "op") : "";
      return {
        op: operation,
        data: handle(request),
        version: manifest.version,
      };
    } catch (error) {
      if (error instanceof InvocationFailure) {
        throw error;
      }
      throw new InvocationFailure(errorMessage(error));
    }
  },
};

export default sharedPlugin;
