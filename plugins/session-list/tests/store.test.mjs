import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { ListStore, read_only_uri } from "../store.js";
import { open_db } from "./sqlite.mjs";

async function store() {
  const dir = mkdtempSync(join(tmpdir(), "session-list-"));
  const list = new ListStore(open_db(join(dir, "store.sqlite3")));
  await list.open(join(dir, "nope", "store.sqlite3"));
  assert.ok(list.ready);
  return list;
}

const conversation = (machine, id) => ({ kind: "conversation", machine, id });

test("置顶按先后、分机器记", async () => {
  const list = await store();
  list.set_pinned(conversation(null, "b"), true);
  list.set_pinned(conversation(null, "a"), true);
  list.set_pinned(conversation("box", "c"), true);
  // 再置顶一次不改位置。
  list.set_pinned(conversation(null, "b"), true);
  list.set_pinned({ kind: "project", machine: null, root: "/repo" }, true);
  list.set_pinned({ kind: "workset", machine: "box", id: "w" }, true);
  await list.flush();
  let ui = await list.read_sidebar();
  assert.deepEqual(ui.machines.get("").pinned_conversations, ["b", "a"]);
  assert.deepEqual(ui.machines.get("box").pinned_conversations, ["c"]);
  assert.deepEqual(ui.machines.get("").pinned_projects, ["/repo"]);
  assert.ok(ui.machines.get("box").pinned_worksets.has("w"));
  list.set_pinned(conversation(null, "b"), false);
  await list.flush();
  ui = await list.read_sidebar();
  assert.deepEqual(ui.machines.get("").pinned_conversations, ["a"]);
});

test("顺序整份换，折叠来回切", async () => {
  const list = await store();
  const project = (machine, root) => ({ machine, root });
  list.set_project_order([project(null, "/b"), project(null, "/a")]);
  list.set_project_order([project(null, "/a"), project("box", "/r"), project(null, "/c")]);
  list.set_conversation_order("box", ["x", "y"]);
  list.set_collapsed({ kind: "project", machine: null, root: "/a" }, true);
  list.set_collapsed({ kind: "workset", machine: "box", id: "w" }, true);
  list.set_collapsed({ kind: "conversations" }, true);
  await list.flush();
  let ui = await list.read_sidebar();
  // 整份换掉，跨机器的先后照存。
  assert.deepEqual(ui.project_order, [project(null, "/a"), project("box", "/r"), project(null, "/c")]);
  assert.ok(ui.machines.get("").collapsed_projects.has("/a"));
  assert.deepEqual(ui.machines.get("box").conversation_order, ["x", "y"]);
  assert.ok(ui.machines.get("box").collapsed_worksets.has("w"));
  assert.ok(ui.conversations_collapsed);

  list.set_collapsed({ kind: "conversations" }, false);
  list.set_collapsed({ kind: "project", machine: null, root: "/a" }, false);
  await list.flush();
  ui = await list.read_sidebar();
  assert.ok(!ui.conversations_collapsed);
  // 本机什么都不剩时不留这一项。
  assert.ok(!ui.machines.has("") || ui.machines.get("").collapsed_projects.size === 0);
});

test("行首头像的选择存得回来", async () => {
  const list = await store();
  assert.deepEqual(await list.avatar_choice(), { kind: "unset" });
  for (const choice of [
    { kind: "view", id: "someone.avatars/avatar" },
    { kind: "icon" },
    { kind: "view", id: "smelt.buddies/avatar" },
    { kind: "unset" },
  ]) {
    list.save_avatar_choice(choice);
    await list.flush();
    assert.deepEqual(await list.avatar_choice(), choice);
  }
});

test("清掉目录里已经没有的", async () => {
  const list = await store();
  for (const id of ["keep", "gone"]) list.set_pinned(conversation(null, id), true);
  list.set_conversation_order(null, ["gone", "keep"]);
  list.set_pinned(conversation("box", "gone"), true);
  list.set_collapsed({ kind: "project", machine: null, root: "/old" }, true);
  list.set_collapsed({ kind: "workset", machine: null, id: "w-old" }, true);
  list.prune(null, { conversations: new Set(["keep"]), roots: new Set(), worksets: new Set() });
  await list.flush();
  let ui = await list.read_sidebar();
  const local = ui.machines.get("");
  assert.deepEqual(local.pinned_conversations, ["keep"]);
  assert.deepEqual(local.conversation_order, ["keep"]);
  assert.equal(local.collapsed_projects.size, 0);
  assert.equal(local.collapsed_worksets.size, 0);
  // 别的机器不受影响。
  assert.deepEqual(ui.machines.get("box").pinned_conversations, ["gone"]);

  list.forget_machine("box");
  await list.flush();
  ui = await list.read_sidebar();
  assert.ok(!ui.machines.has("box"));
});

test("新建的库从会话插件的旧库抄一次", async () => {
  const dir = mkdtempSync(join(tmpdir(), "session-list-"));
  mkdirSync(join(dir, "smelt.sessions"));
  const old_path = join(dir, "smelt.sessions", "store.sqlite3");
  const old = open_db(old_path);
  // 会话插件库的第一版：还没有头像那张表，另有不归列表的。
  old.raw.exec(`
    CREATE TABLE pinned_conversation (machine TEXT NOT NULL,
       conversation_id TEXT NOT NULL, position INTEGER NOT NULL,
       PRIMARY KEY (machine, conversation_id));
    CREATE TABLE sidebar (id INTEGER PRIMARY KEY, conversations_collapsed INTEGER NOT NULL);
    CREATE TABLE selection (id INTEGER PRIMARY KEY, machine TEXT, conversation_id TEXT);
    INSERT INTO pinned_conversation VALUES ('', 'c', 0);
    INSERT INTO sidebar VALUES (1, 1);
    INSERT INTO selection VALUES (1, '', 'c');`);
  old.raw.close();
  mkdirSync(join(dir, "smelt.session-list"));
  const db = open_db(join(dir, "smelt.session-list", "store.sqlite3"));
  const list = new ListStore(db);
  await list.open(old_path);
  const ui = await list.read_sidebar();
  assert.deepEqual(ui.machines.get("").pinned_conversations, ["c"]);
  assert.ok(ui.conversations_collapsed);
  assert.deepEqual(await list.avatar_choice(), { kind: "unset" });
  // 挂上的旧库卸掉了；再开一次不再抄。
  assert.deepEqual(await db.query("PRAGMA database_list").then((rows) => rows.map((row) => row.name)), ["main"]);
});

test("库比插件新：不记，只在内存里用", async () => {
  const dir = mkdtempSync(join(tmpdir(), "session-list-"));
  const db = open_db(join(dir, "store.sqlite3"));
  db.raw.exec("PRAGMA user_version = 9");
  const list = new ListStore(db);
  const error = console.error;
  console.error = () => {};
  try {
    await list.open(null);
  } finally {
    console.error = error;
  }
  assert.ok(!list.ready);
  list.set_pinned(conversation(null, "a"), true);
  assert.equal((await list.read_sidebar()).machines.size, 0);
});

test("只读 URI 转义路径里的特殊字符", () => {
  assert.equal(read_only_uri("/a b/c?#%.db"), "file:/a b/c%3F%23%25.db?mode=ro");
  assert.equal(read_only_uri("C:\\x\\s.db"), "file:/C:/x/s.db?mode=ro");
});
