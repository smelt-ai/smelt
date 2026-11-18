// 插件文案表的检查。在仓库根目录跑：node --test plugins/skills/tests/
// 1. 每个键英文、简体中文都有，插值的名字两边一样；
// 2. 代码里 `t("…")`、`tn("…")` 取的键都在表里（`tn` 要 `.one`、`.other` 两条）；
// 3. 表里的键都有代码在取，不留死条目。

import { test } from "node:test";
import assert from "node:assert/strict";
import * as fs from "node:fs";
import { registerHooks } from "node:module";

registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "smelt-plugin-api") {
      return { url: new URL("./smelt-plugin-api.mjs", import.meta.url).href, shortCircuit: true };
    }
    return next(specifier, context);
  },
});

const { CATALOG, t, tn } = await import("../i18n.js");
const ROOT = new URL("../", import.meta.url);

/// 插件目录下各个 .js 里用到的键：`t` 的、`tn` 的分开。
function usedKeys() {
  const plain = new Set();
  const counted = new Set();
  for (const name of fs.readdirSync(ROOT).filter((name) => name.endsWith(".js"))) {
    const source = fs.readFileSync(new URL(name, ROOT), "utf8");
    for (const [, fn, key] of source.matchAll(/(?<![\w.$])(tn?)\("([^"]+)"/g)) {
      (fn === "tn" ? counted : plain).add(key);
    }
  }
  return { plain, counted };
}

const placeholders = (text) => [...text.matchAll(/%\{(\w+)\}/g)].map(([, name]) => name).sort();

test("每个键两种语言都有，插值一样", () => {
  assert.deepEqual(Object.keys(CATALOG["zh-CN"]).sort(), Object.keys(CATALOG.en).sort());
  for (const [key, en] of Object.entries(CATALOG.en)) {
    assert.deepEqual(placeholders(CATALOG["zh-CN"][key]), placeholders(en), key);
  }
});

test("用到的键都在表里，表里的键都有人用", () => {
  const { plain, counted } = usedKeys();
  const wanted = new Set([...plain, ...[...counted].flatMap((key) => [`${key}.one`, `${key}.other`])]);
  for (const key of wanted) assert.ok(key in CATALOG.en, `表里没有 ${key}`);
  for (const key of Object.keys(CATALOG.en)) assert.ok(wanted.has(key), `${key} 没人用`);
});

test("插值和数量", () => {
  assert.equal(t("delete.title", { name: "lint" }), 'Delete "lint"?');
  assert.equal(tn("status.agents", 1), "1 agent");
  assert.equal(tn("status.agents", 3), "3 agents");
});
