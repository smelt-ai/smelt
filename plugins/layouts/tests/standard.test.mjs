import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

function views(panel) {
  const children = Array.isArray(panel?.children) ? panel.children : [];
  if (children.length === 0) return [panel?.panel_name ?? ""];
  return children.flatMap(views);
}

function stackViews(stack) {
  const out = [];
  for (const item of stack.items ?? []) {
    if (item.view) out.push(item.view);
    else if (item.stack) out.push(...stackViews(item.stack));
  }
  return out;
}

describe("smelt.layouts standard layout", () => {
  const root = join(import.meta.dir, "..");
  const manifest = JSON.parse(readFileSync(join(root, "smelt-plugin.json"), "utf8"));
  const state = JSON.parse(readFileSync(join(root, "layouts", "standard.json"), "utf8"));

  test("清单以纯声明方式登记首页与默认布局，不需要 JS 入口", () => {
    expect(manifest.id).toBe("smelt.layouts");
    expect(manifest.entry).toBeUndefined();
    expect(manifest.pages).toEqual([
      {
        name: "workbench",
        title_key: "page.workbench",
        home: true,
        index: "smelt.sessions/session",
      },
    ]);
    expect(manifest.layouts).toEqual([
      {
        page: "smelt.layouts/workbench",
        name: "standard",
        title_key: "layout.standard",
        file: "layouts/standard.json",
        materials: { left: "sidebar" },
      },
    ]);
  });

  test("默认布局把侧栏摆在左停靠区一摞、右侧面板组开着、出口在中间", () => {
    expect(state.version).toBe(1);
    expect(views(state.center)).toEqual(["smelt/outlet"]);
    expect(state.left_dock.open).toBe(true);
    const side = state.left_dock.panel.children;
    expect(side.length).toBe(1);
    expect(side[0].panel_name).toBe("smelt/stack");
    const stack = side[0].info.panel;
    expect(stackViews(stack)).toEqual([
      "smelt.launcher/nav",
      "smelt.session-list/main",
      "smelt.launcher/settings",
      "smelt.archive/button",
      "smelt.quota/bar",
    ]);
    expect(stack.axis).toBe("vertical");
    expect(stack.items.map((item) => Boolean(item.fit))).toEqual([true, false, true]);
    const bottom = stack.items[2].stack;
    expect(bottom.axis).toBe("horizontal");
    expect(bottom.items.map((item) => Boolean(item.fit))).toEqual([true, true, false]);
    expect(state.right_dock.open).toBe(true);
    expect(views(state.right_dock.panel)).toEqual([
      "smelt.files/main",
      "smelt.git/main",
      "smelt.history/main",
      "smelt.skills/main",
    ]);
    expect(state.bottom_dock ?? null).toBeNull();
  });
});
