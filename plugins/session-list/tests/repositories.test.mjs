import { expect, test } from "bun:test";

import { suggested_name, valid_task } from "../repositories.js";
import { fake_host, list } from "./fakes.mjs";
import { t } from "./helpers.mjs";

const settle = () => new Promise(setImmediate);
const repo = (name, project_root = "/repo") => ({
  name,
  root: `${project_root}/${name}`,
  project_root,
  managed_link: null,
});

/** 运行时那边的仓库目录：discover 回现有的，bind / clone 加一个，remove 去掉一个。 */
function runtime(host, names) {
  const repositories = names.map((name) => repo(name));
  host.replies["workspace.project.repository.action"] = ({ action }) => {
    if (action.kind === "bind" || action.kind === "clone") repositories.push(repo(action.name));
    if (action.kind === "remove")
      repositories.splice(
        repositories.findIndex((r) => r.name === action.name),
        1,
      );
    return { repositories: [...repositories, repo("other", "/elsewhere")], worksets: [] };
  };
}

test("从 Git URL 或目录推仓库名", () => {
  expect(suggested_name("git@example.com:team/api.git")).toBe("api");
  expect(suggested_name("https://example.com/team/web.git")).toBe("web");
  expect(suggested_name("/Users/someone/my repo/")).toBe("my repo");
  expect(suggested_name("https://token@example.com")).toBe("");
  expect(suggested_name("https://token@example.com/team/api.git?token=private")).toBe("api");
});

test("任务名是一层目录名", () => {
  expect(valid_task("add-search")).toBeTruthy();
  expect(valid_task("搜索功能")).toBeTruthy();
  for (const bad of ["", ".", "..", "a/b", "a\\b", "a\nb"]) expect(valid_task(bad), JSON.stringify(bad)).toBeFalsy();
});

test("打开先探查，全勾上；之后纳入的勾上，人取消的留着", async () => {
  const host = fake_host();
  runtime(host, ["api", "web"]);
  const sessions = list(host);
  sessions.open_repositories(null, "/repo", "manage");
  const dialog = sessions.repositories;
  expect(dialog.busy).toBe(true);
  await settle();
  expect(dialog.busy).toBe(false);
  expect(dialog.repositories.map((r) => r.name)).toStrictEqual(["api", "web"]);
  expect([...dialog.selected]).toStrictEqual(["api", "web"]);

  dialog.toggle("web", false);
  dialog.switch("clone");
  expect(dialog.back()).toBe("manage");
  dialog.url.type("https://example.com/team/docs.git");
  expect(dialog.repository_name()).toBe("docs");
  dialog.url.emit("submit");
  await settle();
  expect(dialog.mode).toBe("manage");
  expect([...dialog.selected].sort()).toStrictEqual(["api", "docs"]);
  expect(host.calls.at(-1)[2].action).toStrictEqual({
    kind: "clone",
    url: "https://example.com/team/docs.git",
    name: "docs",
  });
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
  expect(dialog.error).toBe(t("repo.choose_first"));
  host.answers = { "smelt.app/pick-paths": ["/src/lib"] };
  await dialog.choose_folder();
  expect(dialog.source).toBe("/src/lib");
  dialog.submit();
  await settle();
  expect(host.calls.at(-1)[2].action).toStrictEqual({ kind: "bind", source: "/src/lib", name: "lib" });
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
  expect(dialog.back()).toBe(null);

  dialog.task.set_value("a/b");
  dialog.submit();
  expect(dialog.error).toBe(t("repo.invalid_task"));

  dialog.task.set_value(" search ");
  dialog.submit();
  expect(host.calls.at(-1)).toStrictEqual([
    null,
    "workspace.workset.create_with_branch",
    { project_root: "/repo", task: "search", branch_prefix: "feat", branch: null, repository_names: ["api", "web"] },
  ]);
  dialog.dismiss();
  dialog.switch("manage");
  expect(sessions.repositories).toBe(dialog);
  expect(dialog.mode).toBe("create");

  finish({ setup: [{ repository: "web", success: false, output: "npm failed" }] });
  await settle();
  expect(created).toStrictEqual(["search"]);
  expect(sessions.repositories).toBe(null);
  expect(host.runs.filter(([command]) => command === "smelt.app/notify").length).toBe(1);
  // 输入框等到下一次不在绘制里时才放。
  expect(dialog.task.released).toBe(false);
  sessions.open_repositories(null, "/repo", "manage");
  expect(dialog.fields().every((field) => field.released)).toBeTruthy();
});
