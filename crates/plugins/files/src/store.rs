//! 文件视图自己的库（`~/.smelt/plugin-data/smelt.files.sqlite3`）：树的宽度和开关、钉住的
//! 目录，以及每个项目的选中、打开的文件、展开的目录、收起的根。
//!
//! 都是小量写，在事件里同步写；读只在建视图和换到一个没停放过的项目时。库打不开（权限、
//! 版本比插件新）就只在内存里用，日志里留一句。

use std::collections::HashSet;
use std::sync::Arc;

use smelt_plugin_api::rusqlite::{self, OptionalExtension as _, params};
use smelt_plugin_api::{PluginStore, RuntimeKey};

use crate::{COMMAND_OWNER, MAX_TREE_WIDTH, MIN_TREE_WIDTH, ProjectRef};

const MIGRATIONS: &[&str] = &["
    CREATE TABLE settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        tree_width REAL NOT NULL,
        tree_open INTEGER NOT NULL
    );
    CREATE TABLE pinned_root (
        path TEXT PRIMARY KEY,
        position INTEGER NOT NULL
    );
    CREATE TABLE project (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        selected TEXT,
        open_file TEXT,
        PRIMARY KEY (machine, root)
    );
    CREATE TABLE expanded_dir (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        path TEXT NOT NULL,
        PRIMARY KEY (machine, root, path)
    );
    CREATE TABLE collapsed_root (
        machine TEXT NOT NULL,
        root TEXT NOT NULL,
        path TEXT NOT NULL,
        PRIMARY KEY (machine, root, path)
    );
"];

/// 不分项目的那部分。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub(crate) tree_width: f32,
    pub(crate) tree_open: bool,
    /// 钉进文件树的本机目录，按钉的先后。
    pub(crate) pinned: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tree_width: MIN_TREE_WIDTH,
            tree_open: true,
            pinned: Vec::new(),
        }
    }
}

/// 一个项目记下的现场。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SavedProject {
    pub(crate) selected: Option<String>,
    pub(crate) open_file: Option<String>,
    pub(crate) expanded: HashSet<String>,
    pub(crate) collapsed_roots: HashSet<String>,
}

pub(crate) struct FilesStore {
    store: Option<Arc<PluginStore>>,
}

impl FilesStore {
    /// 打开本插件的库。打不开就是个什么都不记的空壳。
    pub(crate) fn open() -> Self {
        let store = smelt_plugin_api::plugin_store(COMMAND_OWNER).and_then(|store| {
            store.migrate(MIGRATIONS)?;
            Ok(store)
        });
        match store {
            Ok(store) => Self { store: Some(store) },
            Err(error) => {
                eprintln!("[files] 打不开文件视图的库，这次不记现场：{error}");
                Self { store: None }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn open_at(path: &std::path::Path) -> Self {
        let store = PluginStore::open(path).unwrap();
        store.migrate(MIGRATIONS).unwrap();
        Self {
            store: Some(Arc::new(store)),
        }
    }

    pub(crate) fn settings(&self) -> Settings {
        let Some(store) = &self.store else {
            return Settings::default();
        };
        let read = store.read(|db| {
            let layout = db
                .query_row(
                    "SELECT tree_width, tree_open FROM settings WHERE id = 1",
                    [],
                    |row| Ok((row.get::<_, f64>(0)?, row.get::<_, bool>(1)?)),
                )
                .optional()?;
            let mut pinned = db.prepare("SELECT path FROM pinned_root ORDER BY position")?;
            let pinned = pinned
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok((layout, pinned))
        });
        match read {
            Ok((layout, pinned)) => {
                let defaults = Settings::default();
                let (tree_width, tree_open) = layout
                    .map(|(width, open)| (width as f32, open))
                    .unwrap_or((defaults.tree_width, defaults.tree_open));
                Settings {
                    tree_width: tree_width.clamp(MIN_TREE_WIDTH, MAX_TREE_WIDTH),
                    tree_open,
                    pinned,
                }
            }
            Err(error) => {
                eprintln!("[files] 读设置失败：{error}");
                Settings::default()
            }
        }
    }

    pub(crate) fn save_tree_layout(&self, tree_width: f32, tree_open: bool) {
        self.write("记树的宽度", |tx| {
            tx.execute(
                "INSERT INTO settings (id, tree_width, tree_open) VALUES (1, ?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET tree_width = ?1, tree_open = ?2",
                params![f64::from(tree_width), tree_open],
            )?;
            Ok(())
        });
    }

    pub(crate) fn save_pinned(&self, pinned: &[String]) {
        self.write("记钉住的目录", |tx| {
            tx.execute("DELETE FROM pinned_root", [])?;
            let mut insert =
                tx.prepare("INSERT INTO pinned_root (path, position) VALUES (?1, ?2)")?;
            for (position, path) in pinned.iter().enumerate() {
                insert.execute(params![path, position as i64])?;
            }
            Ok(())
        });
    }

    pub(crate) fn project(&self, project: &ProjectRef) -> SavedProject {
        let Some(store) = &self.store else {
            return SavedProject::default();
        };
        let machine = machine_column(&project.runtime);
        let root = project.root.as_str();
        let read = store.read(|db| {
            let (selected, open_file) = db
                .query_row(
                    "SELECT selected, open_file FROM project WHERE machine = ?1 AND root = ?2",
                    params![machine, root],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .unwrap_or((None, None));
            let paths = |table: &str| -> rusqlite::Result<HashSet<String>> {
                let mut query = db.prepare(&format!(
                    "SELECT path FROM {table} WHERE machine = ?1 AND root = ?2"
                ))?;
                query
                    .query_map(params![machine, root], |row| row.get(0))?
                    .collect()
            };
            Ok(SavedProject {
                selected,
                open_file,
                expanded: paths("expanded_dir")?,
                collapsed_roots: paths("collapsed_root")?,
            })
        });
        read.unwrap_or_else(|error| {
            eprintln!("[files] 读 {} 的现场失败：{error}", project.root);
            SavedProject::default()
        })
    }

    /// 整份替换一个项目的现场。
    pub(crate) fn save_project(&self, project: &ProjectRef, saved: &SavedProject) {
        let machine = machine_column(&project.runtime);
        let root = project.root.as_str();
        self.write("记项目现场", |tx| {
            tx.execute(
                "INSERT INTO project (machine, root, selected, open_file) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (machine, root) DO UPDATE SET selected = ?3, open_file = ?4",
                params![machine, root, saved.selected, saved.open_file],
            )?;
            for (table, paths) in [
                ("expanded_dir", &saved.expanded),
                ("collapsed_root", &saved.collapsed_roots),
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE machine = ?1 AND root = ?2"),
                    params![machine, root],
                )?;
                let mut insert = tx.prepare(&format!(
                    "INSERT INTO {table} (machine, root, path) VALUES (?1, ?2, ?3)"
                ))?;
                for path in paths {
                    insert.execute(params![machine, root, path])?;
                }
            }
            Ok(())
        });
    }

    fn write(&self, what: &str, run: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<()>) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(error) = store.write(run) {
            eprintln!("[files] {what}失败：{error}");
        }
    }
}

/// 机器在表里怎么写：本机是空串，远程机器是它的 id（不会是空的）。
fn machine_column(machine: &RuntimeKey) -> &str {
    match machine {
        RuntimeKey::Local => "",
        RuntimeKey::Remote(id) => id,
    }
}
