// 文件视图自己的 SQLite 库（~/.smelt/plugin-data/smelt.files/store.sqlite3）：
// 与原 Rust 版保持同一张表结构，老数据无缝继续用。

import { DEFAULT_TREE_WIDTH, MAX_TREE_WIDTH, MIN_TREE_WIDTH } from "./model.js";

const MIGRATIONS = [
  `
    CREATE TABLE IF NOT EXISTS settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        tree_width REAL NOT NULL,
        tree_open INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS pinned_root (
        path TEXT PRIMARY KEY,
        position INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS project (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        selected TEXT,
        open_file TEXT,
        PRIMARY KEY (machine, root)
    );
    CREATE TABLE IF NOT EXISTS expanded_dir (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        path TEXT NOT NULL,
        PRIMARY KEY (machine, root, path)
    );
    CREATE TABLE IF NOT EXISTS collapsed_root (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        path TEXT NOT NULL,
        PRIMARY KEY (machine, root, path)
    );
  `,
];

export function machineColumn(machine) {
  return machine ?? "";
}

export function clampTreeWidth(width) {
  const num = Number(width);
  if (!Number.isFinite(num)) return DEFAULT_TREE_WIDTH;
  return Math.max(MIN_TREE_WIDTH, Math.min(MAX_TREE_WIDTH, num));
}

export function createFilesStore(storeApi) {
  let ready = null;

  function ensureMigrated() {
    if (!ready) {
      ready = storeApi.migrate(MIGRATIONS).catch((error) => {
        console.error("[files] migrate store failed:", error);
        return false;
      });
    }
    return ready;
  }

  return {
    async loadSettings() {
      const ok = await ensureMigrated();
      if (ok === false) {
        return { treeWidth: DEFAULT_TREE_WIDTH, treeOpen: true, pinned: [] };
      }
      try {
        const [layoutRows, pinnedRows] = await Promise.all([
          storeApi.query("SELECT tree_width, tree_open FROM settings WHERE id = 1"),
          storeApi.query("SELECT path FROM pinned_root ORDER BY position"),
        ]);
        const layout = layoutRows?.[0];
        const treeWidth = layout ? clampTreeWidth(layout.tree_width) : DEFAULT_TREE_WIDTH;
        const treeOpen = layout ? Boolean(layout.tree_open) : true;
        const pinned = (pinnedRows ?? [])
          .map((row) => row?.path)
          .filter((p) => typeof p === "string" && p.length > 0);
        return { treeWidth, treeOpen, pinned };
      } catch (error) {
        console.error("[files] loadSettings failed:", error);
        return { treeWidth: DEFAULT_TREE_WIDTH, treeOpen: true, pinned: [] };
      }
    },

    async saveTreeLayout(treeWidth, treeOpen) {
      const ok = await ensureMigrated();
      if (ok === false) return;
      try {
        await storeApi.write([
          {
            sql: `INSERT INTO settings (id, tree_width, tree_open) VALUES (1, ?1, ?2)
                  ON CONFLICT (id) DO UPDATE SET tree_width = ?1, tree_open = ?2`,
            params: [clampTreeWidth(treeWidth), treeOpen ? 1 : 0],
          },
        ]);
      } catch (error) {
        console.error("[files] saveTreeLayout failed:", error);
      }
    },

    async savePinned(pinned) {
      const ok = await ensureMigrated();
      if (ok === false) return;
      try {
        const statements = [{ sql: "DELETE FROM pinned_root", params: [] }];
        for (let i = 0; i < pinned.length; i += 1) {
          statements.push({
            sql: "INSERT INTO pinned_root (path, position) VALUES (?1, ?2)",
            params: [pinned[i], i],
          });
        }
        await storeApi.write(statements);
      } catch (error) {
        console.error("[files] savePinned failed:", error);
      }
    },

    async loadProject(machine, root) {
      const ok = await ensureMigrated();
      if (ok === false) {
        return { selected: null, openFile: null, expanded: new Set(), collapsedRoots: new Set() };
      }
      const m = machineColumn(machine);
      try {
        const [projRows, expandedRows, collapsedRows] = await Promise.all([
          storeApi.query(
            "SELECT selected, open_file FROM project WHERE machine = ?1 AND root = ?2",
            [m, root],
          ),
          storeApi.query(
            "SELECT path FROM expanded_dir WHERE machine = ?1 AND root = ?2",
            [m, root],
          ),
          storeApi.query(
            "SELECT path FROM collapsed_root WHERE machine = ?1 AND root = ?2",
            [m, root],
          ),
        ]);
        const row = projRows?.[0];
        return {
          selected: typeof row?.selected === "string" ? row.selected : null,
          openFile: typeof row?.open_file === "string" ? row.open_file : null,
          expanded: new Set((expandedRows ?? []).map((r) => r.path).filter(Boolean)),
          collapsedRoots: new Set((collapsedRows ?? []).map((r) => r.path).filter(Boolean)),
        };
      } catch (error) {
        console.error("[files] loadProject failed:", error);
        return { selected: null, openFile: null, expanded: new Set(), collapsedRoots: new Set() };
      }
    },

    async saveProject(machine, root, saved) {
      const ok = await ensureMigrated();
      if (ok === false) return;
      const m = machineColumn(machine);
      const statements = [
        {
          sql: `INSERT INTO project (machine, root, selected, open_file) VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT (machine, root) DO UPDATE SET selected = ?3, open_file = ?4`,
          params: [m, root, saved.selected ?? null, saved.openFile ?? null],
        },
        {
          sql: "DELETE FROM expanded_dir WHERE machine = ?1 AND root = ?2",
          params: [m, root],
        },
        {
          sql: "DELETE FROM collapsed_root WHERE machine = ?1 AND root = ?2",
          params: [m, root],
        },
      ];
      for (const path of saved.expanded ?? []) {
        statements.push({
          sql: "INSERT INTO expanded_dir (machine, root, path) VALUES (?1, ?2, ?3)",
          params: [m, root, path],
        });
      }
      for (const path of saved.collapsedRoots ?? []) {
        statements.push({
          sql: "INSERT INTO collapsed_root (machine, root, path) VALUES (?1, ?2, ?3)",
          params: [m, root, path],
        });
      }
      try {
        await storeApi.write(statements);
      } catch (error) {
        console.error("[files] saveProject failed:", error);
      }
    },
  };
}
