import assert from "node:assert/strict";
import test from "node:test";

import { suggested_name, valid_task } from "../repositories.js";
import { fake_host, list } from "./fakes.mjs";
import { t } from "./helpers.mjs";

const settle = () => new Promise(setImmediate);
const repo = (name, project_root = "/repo") => ({ name, root: `${project_root}/${name}`, project_root, managed_link: null });

/** 运行时那边的仓库目录：discover 回现有的，bind / clone 加一个，remove 去掉一个。 */
function runtime(host, names) {
  const repositories = names.map((name) => repo(name));
  host.replies["workspace.project.repository.action"] = ({ action }) => {
    if (action.kind === "bind" || action.kind === "clone") repositories.push(repo(action.name));
    if (action.kind === "remove") repositories.splice(repositories.findIndex((r) => r.name === action.name), 1);
    return { repositories: [...repositories, repo("other", "/elsewhere")], worksets: [] };
  };
}

test("从 Git URL 或目录推仓库名", () => {
  assert.equal(suggested_name("git@example.com:team/api.git"), "api");
  assert.equal(suggested_name("https://example.com/team/web.git"), "web");
  assert.equal(suggested_name("/Users/someone/my repo/"), "my repo");
  assert.equal(suggested_name("https://token@example.com"), "");
  assert.equal(suggested_name("https://token@example.com/team/api.git?token=private"), "api");
});

test("任务名是一层目录名", () => {
  assert.ok(valid_task("add-search"));
  assert.ok(valid_task("搜索功能"));
  for (const bad of ["", ".", "..", "a/b", "a\\b", "a\nb"]) assert.ok(!valid_task(bad), JSON.stringify(bad));
});

test("打开先探查，全勾上；之后纳入的勾上，人取消的留着", async () => {
  const host = fake_host();
  runtime(host, ["api", "web"]);
  const sessions = list(host);
  sessions.open_repositories(null, "/repo", "manage");
  const dialog = sessions.repositories;
  assert.equal(dialog.busy, true);
  await settle();
  assert.equal(dialog.busy, false);
  assert.deepEqual(dialog.repositories.map((r) => r.name), ["api", "web"]);
  assert.deepEqual([...dialog.selected], ["api", "web"]);

  dialog.toggle("web", false);
  dialog.switch("clone");
  assert.equal(dialog.back(), "manage");
  dialog.url.type("https://example.com/team/docs.git");
  assert.equal(dialog.repository_name(), "docs");
  dialog.url.emit("submit");
  await settle();
  assert.equal(dialog.mode, "manage");
  assert.deepEqual([...dialog.selected].sort(), ["api", "docs"]);
  assert.deepEqual(host.calls.at(-1)[2].action, { kind: "clone", url: "https://example.com/team/docs.git", name: "docs" });
});

test("纳入要先选目录", async () => {
  const host = fake_host();
  runtime(host, []);
  const sessions = list(host);
  sessions.open_repositories(null, "/repo", "manage");
  const dialog = sessions.repositories;
  await settle();
  dialog.switch("bind");
  dialog.submit();
  assert.equal(dialog.error, t("repo.choose_first"));
  host.answers = { "smelt.app/pick-paths": ["/src/lib"] };
  await dialog.choose_folder();
  assert.equal(dialog.source, "/src/lib");
  dialog.submit();
  await settle();
  assert.deepEqual(host.calls.at(-1)[2].action, { kind: "bind", source: "/src/lib", name: "lib" });
});

test("建工作树：写入跑着时框关不掉；建好了报初始化失败、告诉打开它的，再关框", async () => {
  const host = fake_host();
  runtime(host, ["web", "api"]);
  let finish;
  host.replies["workspace.workset.create_with_branch"] = () => new Promise((resolve) => (finish = resolve));
  const sessions = list(host);
  const created = [];
  sessions.open_repositories(null, "/repo", "create", (task) => created.push(task));
  const dialog = sessions.repositories;
  await settle();
  assert.equal(dialog.back(), null);

  dialog.task.set_value("a/b");
  dialog.submit();
  assert.equal(dialog.error, t("repo.invalid_task"));

  dialog.task.set_value(" search ");
  dialog.submit();
  assert.deepEqual(host.calls.at(-1), [
    null,
    "workspace.workset.create_with_branch",
    { project_root: "/repo", task: "search", branch_prefix: "feat", branch: null, repository_names: ["api", "web"] },
  ]);
  dialog.dismiss();
  dialog.switch("manage");
  assert.equal(sessions.repositories, dialog);
  assert.equal(dialog.mode, "create");

  finish({ setup: [{ repository: "web", success: false, output: "npm failed" }] });
  await settle();
  assert.deepEqual(created, ["search"]);
  assert.equal(sessions.repositories, null);
  assert.equal(host.runs.filter(([command]) => command === "smelt.app/notify").length, 1);
  // 输入框等到下一次不在绘制里时才放。
  assert.equal(dialog.task.released, false);
  sessions.open_repositories(null, "/repo", "manage");
  assert.ok(dialog.fields().every((field) => field.released));
});
