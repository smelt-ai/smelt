// core.js 的磁盘逻辑测试。在仓库根目录跑：node --test plugins/skills/tests/
// 宿主模块换成同目录下的 Node 实现，每个用例在临时目录里造一个用户主目录。

import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { registerHooks } from "node:module";

const SHIMS = {
  "smelt-fs": new URL("./smelt-fs.mjs", import.meta.url).href,
  "smelt-workspace": new URL("./smelt-workspace.mjs", import.meta.url).href,
};

registerHooks({
  resolve(specifier, context, next) {
    const url = SHIMS[specifier];
    return url ? { url, shortCircuit: true } : next(specifier, context);
  },
});

const core = await import("../core.js");

/// 装了 Claude Code 的主目录。取真实路径：macOS 的临时目录本身经过 /var → /private/var。
function makeHome(t) {
  const home = fs.realpathSync(fs.mkdtempSync(join(tmpdir(), "smelt-skills-")));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  fs.mkdirSync(join(home, ".claude"));
  return home;
}

function writeSkill(dir, name) {
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(join(dir, "SKILL.md"), `---\nname: ${name}\ndescription: ${name} skill\n---\n`);
}

/// `.claude/skills` 整个目录软链到 `.agents/skills`，不是每个 skill 一条链接。
function linkClaudeRoot(home) {
  fs.mkdirSync(join(home, ".agents/skills"), { recursive: true });
  fs.symlinkSync("../.agents/skills", join(home, ".claude/skills"));
}

test("agent 目录整个软链到 .agents/skills：同一份 skill，不算冲突，Claude Code 经链接读到", async (t) => {
  const home = makeHome(t);
  linkClaudeRoot(home);
  writeSkill(join(home, ".agents/skills/demo"), "demo");

  const { skills } = await core.scan(home, null);

  assert.equal(skills.length, 1);
  const [skill] = skills;
  assert.equal(skill.dir, join(home, ".agents/skills/demo"));
  assert.deepEqual(skill.conflicts, []);
  assert.deepEqual(skill.bridges, ["Claude Code"]);
  assert.deepEqual(skill.missing_bridges, []);
  assert.deepEqual(
    skill.coverage.find((row) => row.label === "Claude Code"),
    { label: "Claude Code", state: "linked" },
  );
});

test("agent 目录整个软链时迁移旧位置：不往本体目录里再建兼容链接", async (t) => {
  const home = makeHome(t);
  linkClaudeRoot(home);
  writeSkill(join(home, ".smelt/skills/old"), "old");

  const result = await core.migrateLegacy(home, null);

  assert.deepEqual(result.skipped, []);
  assert.deepEqual(result.migrated, ["old"]);
  assert.equal(fs.lstatSync(join(home, ".agents/skills/old")).isDirectory(), true);
  assert.deepEqual(fs.readdirSync(join(home, ".agents/skills")), ["old"]);
});

test("agent 目录整个软链时删除引用型 skill：只删那一条链接，真身不动", async (t) => {
  const home = makeHome(t);
  linkClaudeRoot(home);
  writeSkill(join(home, "elsewhere/ext"), "ext");
  fs.symlinkSync(join(home, "elsewhere/ext"), join(home, ".agents/skills/ext"));

  const [skill] = (await core.scan(home, null)).skills;
  assert.equal(skill.dir, join(home, ".agents/skills/ext"));
  await core.trashSkill(home, skill);

  assert.equal(fs.existsSync(join(home, ".agents/skills/ext")), false);
  assert.equal(fs.existsSync(join(home, "elsewhere/ext/SKILL.md")), true);
});
