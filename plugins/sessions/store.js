// 会话插件的 SQLite 持久化（~/.smelt/plugin-data/smelt.sessions/store.sqlite3）。
// 严格沿用已发布表结构与迁移序列（含历史两步迁移），确保老库零迁移误差打开且不破坏 session-list 首次导入。
import { machineFromColumn, machineKey } from "./model.js";

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
    CREATE TABLE session_layout (
        machine TEXT NOT NULL,
        conversation_id TEXT NOT NULL,
        active_pane_id TEXT,
        layout TEXT CHECK (layout IS NULL OR json_valid(layout)),
        PRIMARY KEY (machine, conversation_id)
    );
    CREATE TABLE selection (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        machine TEXT NOT NULL,
        conversation_id TEXT NOT NULL
    );
  `,
  `
    CREATE TABLE avatar_choice (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        view_id TEXT
    );
  `,
];

const CONVERSATION_TABLES = ["session_layout", "selection"];

export function createSessionsStore(backend) {
  let available = false;

  return {
    async open() {
      try {
        await backend.migrate(MIGRATIONS);
        available = true;
      } catch (error) {
        console.error("[sessions] 打不开会话的库，这次不记分屏和选中：", error);
        available = false;
      }
    },

    async readSelection() {
      if (!available) return null;
      try {
        const rows = await backend.query(
          "SELECT machine, conversation_id FROM selection WHERE id = 1",
          [],
        );
        const row = rows?.[0];
        if (!row) return null;
        return {
          machine: machineFromColumn(row.machine),
          id: String(row.conversation_id),
        };
      } catch (error) {
        console.error("[sessions] 读上次选中失败：", error);
        return null;
      }
    },

    async saveSelection(key) {
      if (!available) return;
      try {
        if (key) {
          await backend.execute(
            `INSERT INTO selection (id, machine, conversation_id) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET
               machine = excluded.machine,
               conversation_id = excluded.conversation_id`,
            [machineKey(key.machine), key.id],
          );
        } else {
          await backend.execute("DELETE FROM selection", []);
        }
      } catch (error) {
        console.error("[sessions] 记选中失败：", error);
      }
    },

    async readLayout(key) {
      if (!available) return null;
      try {
        const rows = await backend.query(
          "SELECT active_pane_id, layout FROM session_layout WHERE machine = ?1 AND conversation_id = ?2",
          [machineKey(key.machine), key.id],
        );
        const row = rows?.[0];
        if (!row) return null;
        return {
          active_pane: row.active_pane_id ?? null,
          layout: row.layout ?? null,
        };
      } catch (error) {
        console.error("[sessions] 读分屏失败：", error);
        return null;
      }
    },

    async saveLayout(key, layout) {
      if (!available) return;
      try {
        await backend.execute(
          `INSERT INTO session_layout (machine, conversation_id, active_pane_id, layout)
           VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT (machine, conversation_id) DO UPDATE SET
             active_pane_id = excluded.active_pane_id,
             layout = excluded.layout`,
          [
            machineKey(key.machine),
            key.id,
            layout.active_pane ?? null,
            layout.layout ?? null,
          ],
        );
      } catch (error) {
        console.error("[sessions] 记分屏失败：", error);
      }
    },

    async prune(machine, knownIds) {
      if (!available) return;
      const m = machineKey(machine);
      const keep = new Set(knownIds ?? []);
      try {
        const statements = [];
        for (const table of CONVERSATION_TABLES) {
          const rows = await backend.query(
            `SELECT conversation_id FROM ${table} WHERE machine = ?1`,
            [m],
          );
          for (const row of rows ?? []) {
            if (!keep.has(row.conversation_id)) {
              statements.push({
                sql: `DELETE FROM ${table} WHERE machine = ?1 AND conversation_id = ?2`,
                params: [m, row.conversation_id],
              });
            }
          }
        }
        if (statements.length > 0) {
          await backend.write(statements);
        }
      } catch (error) {
        console.error("[sessions] 清理失败：", error);
      }
    },

    async forgetMachine(machine) {
      if (!available) return;
      const m = machineKey(machine);
      try {
        await backend.write(
          CONVERSATION_TABLES.map((table) => ({
            sql: `DELETE FROM ${table} WHERE machine = ?1`,
            params: [m],
          })),
        );
      } catch (error) {
        console.error("[sessions] 清理已移除机器失败：", error);
      }
    },
  };
}
