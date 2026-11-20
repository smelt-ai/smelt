// 会话列表自己的库（~/.smelt/plugin-data/smelt.session-list/store.sqlite3）：侧栏的置顶、手动
// 顺序、折叠，和行首头像用哪个。都是这台桌面自己的界面状态和偏好，运行时不存。
//
// 侧栏那部分启动时整份读一次（read_sidebar），之后内存里那份是缓存，改的时候同一个事件里写库。
// 写是排队的：一条写完才发下一条，先改的先落库。库打不开（权限、版本比插件新）就只在内存里用，
// 日志里留一句。render 里不碰库。
//
// 行里的对话 id、项目根、工作集 id 都指着那台机器的工作区目录；目录里没了的由 prune 清掉，移除
// 的机器由 forget_machine 清掉。本机在 machine 列里是 ""。
//
// 这些表原来在会话插件的库里（列表从会话插件拆出来之前）。新建这个库时从那边抄一次
// （copy_from_sessions），之后两边各管各的。
// TODO：用户都升过来了就删掉这次搬迁，连同会话插件库里那几张表。

import { machine_key, machine_of_key, new_sidebar_ui } from "./model.js";
import { message } from "./util.js";

/** 迁移步骤：同原来 Rust 版的库，第 1 版。只往后加，不改已有的。 */
export const MIGRATIONS = [
  `
    CREATE TABLE pinned_conversation (
        machine TEXT NOT NULL,
        conversation_id TEXT NOT NULL,
        position INTEGER NOT NULL,
        PRIMARY KEY (machine, conversation_id)
    );
    CREATE TABLE pinned_project (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        position INTEGER NOT NULL,
        PRIMARY KEY (machine, root)
    );
    CREATE TABLE pinned_workset (
        machine TEXT NOT NULL,
        workset_id TEXT NOT NULL,
        PRIMARY KEY (machine, workset_id)
    );
    CREATE TABLE project_order (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        position INTEGER NOT NULL,
        PRIMARY KEY (machine, root)
    );
    CREATE TABLE conversation_order (
        machine TEXT NOT NULL,
        conversation_id TEXT NOT NULL,
        position INTEGER NOT NULL,
        PRIMARY KEY (machine, conversation_id)
    );
    CREATE TABLE collapsed_project (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        PRIMARY KEY (machine, root)
    );
    CREATE TABLE collapsed_workset (
        machine TEXT NOT NULL,
        workset_id TEXT NOT NULL,
        PRIMARY KEY (machine, workset_id)
    );
    CREATE TABLE sidebar (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        conversations_collapsed INTEGER NOT NULL CHECK (conversations_collapsed IN (0, 1))
    );
    CREATE TABLE avatar_choice (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        view_id TEXT
    );
`,
];

/** 按对话 id 记的表：目录里没了这段对话就清掉。 */
const CONVERSATION_TABLES = ["pinned_conversation", "conversation_order"];
/** 按项目根记的表。 */
const ROOT_TABLES = ["pinned_project", "project_order", "collapsed_project"];
/** 按工作集 id 记的表。 */
const WORKSET_TABLES = ["pinned_workset", "collapsed_workset"];
const KEYED_TABLES = [...CONVERSATION_TABLES, ...ROOT_TABLES, ...WORKSET_TABLES];

const PIN_TABLE = {
  conversation: ["pinned_conversation", "conversation_id", true],
  project: ["pinned_project", "root", true],
  workset: ["pinned_workset", "workset_id", false],
};

/**
 * 本插件的库。db 是宿主的库函数 { query, execute, write, migrate }（store_query 这些），测试里
 * 换成 Node 的 SQLite。
 */
export class ListStore {
  constructor(db) {
    this.db = db;
    /** 库打不开时为 false：什么都不记。 */
    this.ready = false;
    this.queue = Promise.resolve();
  }

  /**
   * 迁到最新版；新建的库从会话插件的旧库（old_path，没有也行）抄一次侧栏的表。打不开就是个
   * 什么都不记的空壳。
   */
  async open(old_path) {
    try {
      const [{ user_version }] = await this.db.query("PRAGMA user_version");
      await this.db.migrate(MIGRATIONS);
      this.ready = true;
      if (user_version === 0 && old_path) await copy_from_sessions(this.db, old_path);
    } catch (error) {
      console.error(`[session-list] 打不开会话列表的库，这次不记侧栏：${message(error)}`);
    }
  }

  /** 侧栏的置顶、顺序、折叠，所有机器的。启动时读一次。 */
  async read_sidebar() {
    const ui = new_sidebar_ui();
    if (!this.ready) return ui;
    try {
      const entry = (machine) => {
        let found = ui.machines.get(machine);
        if (!found) {
          found = {
            pinned_conversations: [],
            pinned_projects: [],
            pinned_worksets: new Set(),
            conversation_order: [],
            collapsed_projects: new Set(),
            collapsed_worksets: new Set(),
          };
          ui.machines.set(machine, found);
        }
        return found;
      };
      const q = (sql) => this.db.query(sql);
      for (const row of await q("SELECT machine, conversation_id FROM pinned_conversation ORDER BY position")) {
        entry(row.machine).pinned_conversations.push(row.conversation_id);
      }
      for (const row of await q("SELECT machine, root FROM pinned_project ORDER BY position")) {
        entry(row.machine).pinned_projects.push(row.root);
      }
      for (const row of await q("SELECT machine, workset_id FROM pinned_workset")) {
        entry(row.machine).pinned_worksets.add(row.workset_id);
      }
      for (const row of await q("SELECT machine, root FROM project_order ORDER BY position")) {
        ui.project_order.push({ machine: machine_of_key(row.machine), root: row.root });
      }
      for (const row of await q("SELECT machine, conversation_id FROM conversation_order ORDER BY position")) {
        entry(row.machine).conversation_order.push(row.conversation_id);
      }
      for (const row of await q("SELECT machine, root FROM collapsed_project")) {
        entry(row.machine).collapsed_projects.add(row.root);
      }
      for (const row of await q("SELECT machine, workset_id FROM collapsed_workset")) {
        entry(row.machine).collapsed_worksets.add(row.workset_id);
      }
      const [sidebar] = await q("SELECT conversations_collapsed FROM sidebar WHERE id = 1");
      ui.conversations_collapsed = !!sidebar?.conversations_collapsed;
      return ui;
    } catch (error) {
      console.error(`[session-list] 读侧栏状态失败，按默认显示：${message(error)}`);
      return new_sidebar_ui();
    }
  }

  /** 行首头像用哪个。没有那一行是没选过，view_id 是空的是不要头像。 */
  async avatar_choice() {
    if (!this.ready) return { kind: "unset" };
    try {
      const [row] = await this.db.query("SELECT view_id FROM avatar_choice WHERE id = 1");
      if (!row) return { kind: "unset" };
      return row.view_id == null ? { kind: "icon" } : { kind: "view", id: row.view_id };
    } catch (error) {
      console.error(`[session-list] 读行首头像的选择失败：${message(error)}`);
      return { kind: "unset" };
    }
  }

  /** 置顶或取消。新置顶的排在已置顶的最后。 */
  set_pinned(pin, pinned) {
    const [table, column, ordered] = PIN_TABLE[pin.kind];
    const key = pin.kind === "project" ? pin.root : pin.id;
    const machine = machine_key(pin.machine);
    let sql;
    if (!pinned) sql = `DELETE FROM ${table} WHERE machine = ?1 AND ${column} = ?2`;
    else if (ordered) {
      sql = `INSERT OR IGNORE INTO ${table} (machine, ${column}, position)
             SELECT ?1, ?2, COALESCE(MAX(position) + 1, 0) FROM ${table}
             WHERE machine = ?1`;
    } else sql = `INSERT OR IGNORE INTO ${table} (machine, ${column}) VALUES (?1, ?2)`;
    this.write("置顶", [{ sql, params: [machine, key] }]);
  }

  /** 折起或展开。 */
  set_collapsed(fold, collapsed) {
    if (fold.kind === "conversations") {
      this.write("折叠", [
        {
          sql: `INSERT INTO sidebar (id, conversations_collapsed) VALUES (1, ?1)
                ON CONFLICT (id) DO UPDATE SET
                    conversations_collapsed = excluded.conversations_collapsed`,
          params: [collapsed ? 1 : 0],
        },
      ]);
      return;
    }
    const [table, column, key] =
      fold.kind === "project" ? ["collapsed_project", "root", fold.root] : ["collapsed_workset", "workset_id", fold.id];
    const sql = collapsed
      ? `INSERT OR IGNORE INTO ${table} (machine, ${column}) VALUES (?1, ?2)`
      : `DELETE FROM ${table} WHERE machine = ?1 AND ${column} = ?2`;
    this.write("折叠", [{ sql, params: [machine_key(fold.machine), key] }]);
  }

  /** 换掉项目的手动顺序（所有机器一份，整份）。位置跨机器排，读回来按位置就是侧栏里的顺序。 */
  set_project_order(order) {
    this.write("项目顺序", [
      { sql: "DELETE FROM project_order" },
      ...order.map((key, position) => ({
        sql: "INSERT OR IGNORE INTO project_order (machine, root, position) VALUES (?1, ?2, ?3)",
        params: [machine_key(key.machine), key.root, position],
      })),
    ]);
  }

  /** 换掉那台机器上会话的手动顺序（整份）。 */
  set_conversation_order(machine, ids) {
    const key = machine_key(machine);
    this.write("会话顺序", [
      { sql: "DELETE FROM conversation_order WHERE machine = ?1", params: [key] },
      ...ids.map((id, position) => ({
        sql: "INSERT OR IGNORE INTO conversation_order (machine, conversation_id, position) VALUES (?1, ?2, ?3)",
        params: [key, id, position],
      })),
    ]);
  }

  save_avatar_choice(choice) {
    if (choice.kind === "unset") {
      this.write("行首头像", [{ sql: "DELETE FROM avatar_choice" }]);
      return;
    }
    this.write("行首头像", [
      {
        sql: `INSERT INTO avatar_choice (id, view_id) VALUES (1, ?1)
              ON CONFLICT (id) DO UPDATE SET view_id = excluded.view_id`,
        params: [choice.kind === "view" ? choice.id : null],
      },
    ]);
  }

  /**
   * 清掉那台机器上目录里已经没有的东西留下的行。known 是那台机器整份目录里还在的
   * （model.known_ids）；目录还没收到时不要调。
   */
  prune(machine, known) {
    const key = machine_key(machine);
    const keep = (table, column, set) => {
      const kept = [...set];
      const placeholders = kept.map((_, ix) => `?${ix + 2}`).join(", ");
      return {
        sql: `DELETE FROM ${table} WHERE machine = ?1${kept.length ? ` AND ${column} NOT IN (${placeholders})` : ""}`,
        params: [key, ...kept],
      };
    };
    this.write("清理", [
      ...CONVERSATION_TABLES.map((table) => keep(table, "conversation_id", known.conversations)),
      ...ROOT_TABLES.map((table) => keep(table, "root", known.roots)),
      ...WORKSET_TABLES.map((table) => keep(table, "workset_id", known.worksets)),
    ]);
  }

  /** 机器移除了：它的行全删。 */
  forget_machine(machine) {
    const key = machine_key(machine);
    this.write(
      "清理",
      KEYED_TABLES.map((table) => ({ sql: `DELETE FROM ${table} WHERE machine = ?1`, params: [key] })),
    );
  }

  /** 排进写的队列：一条写完才写下一条。失败只记一句。 */
  write(what, statements) {
    if (!this.ready) return this.queue;
    this.queue = this.queue
      .then(() => this.db.write(statements))
      .catch((error) => console.error(`[session-list] 记${what}失败：${message(error)}`));
    return this.queue;
  }

  /** 等排着的写都落库（测试用）。 */
  flush() {
    return this.queue;
  }
}

/** SQLite 的只读 URI：文件不在就打不开，不会新建一个空的。 */
export function read_only_uri(path) {
  let normalized = path.replaceAll("\\", "/");
  if (/^[A-Za-z]:\//.test(normalized)) normalized = `/${normalized}`;
  const encoded = normalized.replace(/[%?#]/g, (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`);
  return `file:${encoded}?mode=ro`;
}

/**
 * 新建的库从会话插件的旧库里抄一次侧栏的表。旧库是哪一版都行：那边没有的表跳过。没有旧库、
 * 抄不成只记一句，按空的用。ATTACH 不能在事务里，所以挂、卸各自单发；抄在一个事务里。
 */
async function copy_from_sessions(db, old_path) {
  try {
    await db.execute("ATTACH DATABASE ?1 AS old", [read_only_uri(old_path)]);
  } catch {
    return; // 没有旧库：会话插件从来没记过侧栏。
  }
  try {
    const present = new Set(
      (await db.query("SELECT name FROM old.sqlite_master WHERE type = 'table'")).map((row) => row.name),
    );
    const tables = [...KEYED_TABLES, "sidebar", "avatar_choice"].filter((table) => present.has(table));
    if (tables.length) {
      await db.write(tables.map((table) => ({ sql: `INSERT INTO main.${table} SELECT * FROM old.${table}` })));
    }
  } catch (error) {
    console.error(`[session-list] 从会话插件的库搬侧栏状态失败，按空的用：${message(error)}`);
  } finally {
    await db.execute("DETACH DATABASE old").catch(() => {});
  }
}

