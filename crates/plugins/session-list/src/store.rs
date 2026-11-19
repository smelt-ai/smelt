//! 会话列表自己的库（`~/.smelt/plugin-data/smelt.session-list/store.sqlite3`）：侧栏的置顶、
//! 手动顺序、折叠，和行首头像用哪个。都是这台桌面自己的界面状态和偏好，运行时不存。
//!
//! 侧栏那部分启动时整份读一次（[`ListStore::sidebar`]），之后内存里那份是缓存，改的时候
//! 同一个事件里写库（见 `model::SessionList::set_pinned` 等）。都是小量写，在事件里同步写；
//! render 里不碰库。库打不开（权限、版本比插件新）就只在内存里用，日志里留一句。
//!
//! 行里的对话 id、项目根、工作集 id 都指着那台机器的工作区目录；目录里没了的由
//! [`ListStore::prune`] 清掉，移除的机器由 [`ListStore::forget_machine`] 清掉。
//!
//! 这些表原来在会话插件的库里（列表从会话插件拆出来之前）。新建这个库时从那边抄一次
//! （[`copy_from_sessions`]），之后两边各管各的。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use smelt_plugin_api::rusqlite::{self, OptionalExtension as _, Transaction, params};
use smelt_plugin_api::{PluginStore, RuntimeKey};

use crate::OWNER;
use crate::model::{AvatarChoice, Fold, KnownIds, Pin, ProjectKey, SidebarUi};

const MIGRATIONS: &[&str] = &["
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
"];

/// 按对话 id 记的表：目录里没了这段对话就清掉。
const CONVERSATION_TABLES: [&str; 2] = ["pinned_conversation", "conversation_order"];
/// 按项目根记的表。
const ROOT_TABLES: [&str; 3] = ["pinned_project", "project_order", "collapsed_project"];
/// 按工作集 id 记的表。
const WORKSET_TABLES: [&str; 2] = ["pinned_workset", "collapsed_workset"];

pub(crate) struct ListStore {
    store: Option<Arc<PluginStore>>,
}

impl ListStore {
    /// 打开本插件的库。打不开就是个什么都不记的空壳。
    pub(crate) fn open() -> Self {
        let store = smelt_plugin_api::plugin_store(OWNER).and_then(|store| {
            let created = store
                .read(|db| db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0)))?
                == 0;
            store.migrate(MIGRATIONS)?;
            if created && let Some(old) = sessions_store_path(store.path()) {
                copy_from_sessions(&store, &old);
            }
            Ok(store)
        });
        match store {
            Ok(store) => Self { store: Some(store) },
            Err(error) => {
                eprintln!("[session-list] 打不开会话列表的库，这次不记侧栏：{error}");
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

    /// 侧栏的置顶、顺序、折叠，所有机器的。启动时读一次。
    pub(crate) fn sidebar(&self) -> SidebarUi {
        let Some(store) = &self.store else {
            return SidebarUi::default();
        };
        let read = store.read(|db| {
            let mut ui = SidebarUi::default();
            for (machine, id) in pairs(
                db,
                "SELECT machine, conversation_id FROM pinned_conversation ORDER BY position",
            )? {
                ui.machine_entry(&machine).pinned_conversations.push(id);
            }
            for (machine, root) in pairs(
                db,
                "SELECT machine, root FROM pinned_project ORDER BY position",
            )? {
                ui.machine_entry(&machine).pinned_projects.push(root);
            }
            for (machine, id) in pairs(db, "SELECT machine, workset_id FROM pinned_workset")? {
                ui.machine_entry(&machine).pinned_worksets.insert(id);
            }
            for (machine, root) in pairs(
                db,
                "SELECT machine, root FROM project_order ORDER BY position",
            )? {
                ui.project_order.push(ProjectKey { machine, root });
            }
            for (machine, id) in pairs(
                db,
                "SELECT machine, conversation_id FROM conversation_order ORDER BY position",
            )? {
                ui.machine_entry(&machine).conversation_order.push(id);
            }
            for (machine, root) in pairs(db, "SELECT machine, root FROM collapsed_project")? {
                ui.machine_entry(&machine).collapsed_projects.insert(root);
            }
            for (machine, id) in pairs(db, "SELECT machine, workset_id FROM collapsed_workset")? {
                ui.machine_entry(&machine).collapsed_worksets.insert(id);
            }
            ui.conversations_collapsed = db
                .query_row(
                    "SELECT conversations_collapsed FROM sidebar WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(false);
            Ok(ui)
        });
        read.unwrap_or_else(|error| {
            eprintln!("[session-list] 读侧栏状态失败，按默认显示：{error}");
            SidebarUi::default()
        })
    }

    /// 置顶或取消。新置顶的排在已置顶的最后。
    pub(crate) fn set_pinned(&self, pin: &Pin, pinned: bool) {
        self.write("置顶", |tx| {
            let (table, column, machine, id, ordered) = match pin {
                Pin::Conversation(key) => (
                    "pinned_conversation",
                    "conversation_id",
                    &key.machine,
                    key.id.as_str(),
                    true,
                ),
                Pin::Project { machine, root } => {
                    ("pinned_project", "root", machine, root.as_str(), true)
                }
                Pin::Workset { machine, id } => {
                    ("pinned_workset", "workset_id", machine, id.as_str(), false)
                }
            };
            let machine = machine_column(machine);
            if !pinned {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE machine = ?1 AND {column} = ?2"),
                    params![machine, id],
                )?;
                return Ok(());
            }
            if ordered {
                tx.execute(
                    &format!(
                        "INSERT OR IGNORE INTO {table} (machine, {column}, position)
                         SELECT ?1, ?2, COALESCE(MAX(position) + 1, 0) FROM {table}
                         WHERE machine = ?1"
                    ),
                    params![machine, id],
                )?;
            } else {
                tx.execute(
                    &format!("INSERT OR IGNORE INTO {table} (machine, {column}) VALUES (?1, ?2)"),
                    params![machine, id],
                )?;
            }
            Ok(())
        });
    }

    /// 折起或展开。
    pub(crate) fn set_collapsed(&self, fold: &Fold, collapsed: bool) {
        self.write("折叠", |tx| {
            let (table, column, machine, id) = match fold {
                Fold::Conversations => {
                    tx.execute(
                        "INSERT INTO sidebar (id, conversations_collapsed) VALUES (1, ?1)
                         ON CONFLICT (id) DO UPDATE SET
                             conversations_collapsed = excluded.conversations_collapsed",
                        params![collapsed],
                    )?;
                    return Ok(());
                }
                Fold::Project { machine, root } => {
                    ("collapsed_project", "root", machine, root.as_str())
                }
                Fold::Workset { machine, id } => {
                    ("collapsed_workset", "workset_id", machine, id.as_str())
                }
            };
            let sql = if collapsed {
                format!("INSERT OR IGNORE INTO {table} (machine, {column}) VALUES (?1, ?2)")
            } else {
                format!("DELETE FROM {table} WHERE machine = ?1 AND {column} = ?2")
            };
            tx.execute(&sql, params![machine_column(machine), id])?;
            Ok(())
        });
    }

    /// 换掉项目的手动顺序（所有机器一份，整份）。位置跨机器排，读回来按位置就是侧栏里的顺序。
    pub(crate) fn set_project_order(&self, order: &[ProjectKey]) {
        self.write("项目顺序", |tx| {
            tx.execute("DELETE FROM project_order", [])?;
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO project_order (machine, root, position) VALUES (?1, ?2, ?3)",
            )?;
            for (position, key) in order.iter().enumerate() {
                insert.execute(params![
                    machine_column(&key.machine),
                    key.root,
                    position as i64
                ])?;
            }
            Ok(())
        });
    }

    /// 换掉那台机器上会话的手动顺序（整份）。
    pub(crate) fn set_conversation_order(&self, machine: &RuntimeKey, ids: &[String]) {
        self.write("会话顺序", |tx| {
            replace_order(tx, "conversation_order", "conversation_id", machine, ids)
        });
    }

    /// 行首头像用哪个。没有那一行是没选过，`view_id` 是空的是不要头像。
    pub(crate) fn avatar_choice(&self) -> AvatarChoice {
        let Some(store) = &self.store else {
            return AvatarChoice::Unset;
        };
        let read = store.read(|db| {
            db.query_row(
                "SELECT view_id FROM avatar_choice WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
        });
        match read {
            Ok(None) => AvatarChoice::Unset,
            Ok(Some(None)) => AvatarChoice::Icon,
            Ok(Some(Some(view))) => AvatarChoice::View(view),
            Err(error) => {
                eprintln!("[session-list] 读行首头像的选择失败：{error}");
                AvatarChoice::Unset
            }
        }
    }

    pub(crate) fn save_avatar_choice(&self, choice: &AvatarChoice) {
        self.write("行首头像", |tx| {
            let view = match choice {
                AvatarChoice::Unset => {
                    tx.execute("DELETE FROM avatar_choice", [])?;
                    return Ok(());
                }
                AvatarChoice::Icon => None,
                AvatarChoice::View(view) => Some(view.as_str()),
            };
            tx.execute(
                "INSERT INTO avatar_choice (id, view_id) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET view_id = excluded.view_id",
                params![view],
            )?;
            Ok(())
        });
    }

    /// 清掉那台机器上目录里已经没有的东西留下的行。`known` 是那台机器整份目录里还在的
    /// （见 `model::known_ids`）；目录还没收到时不要调。
    pub(crate) fn prune(&self, machine: &RuntimeKey, known: &KnownIds) {
        self.write("清理", |tx| {
            let machine = machine_column(machine);
            for table in CONVERSATION_TABLES {
                prune_table(tx, table, "conversation_id", machine, &known.conversations)?;
            }
            for table in ROOT_TABLES {
                prune_table(tx, table, "root", machine, &known.roots)?;
            }
            for table in WORKSET_TABLES {
                prune_table(tx, table, "workset_id", machine, &known.worksets)?;
            }
            Ok(())
        });
    }

    /// 机器移除了：它的行全删。
    pub(crate) fn forget_machine(&self, machine: &RuntimeKey) {
        self.write("清理", |tx| {
            let machine = machine_column(machine);
            for table in CONVERSATION_TABLES
                .into_iter()
                .chain(ROOT_TABLES)
                .chain(WORKSET_TABLES)
            {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE machine = ?1"),
                    params![machine],
                )?;
            }
            Ok(())
        });
    }

    fn write(&self, what: &str, run: impl FnOnce(&Transaction) -> rusqlite::Result<()>) {
        let Some(store) = &self.store else {
            return;
        };
        if let Err(error) = store.write(run) {
            eprintln!("[session-list] 记{what}失败：{error}");
        }
    }
}

/// 读两列（机器、键）的结果。
/// 会话插件那份旧库：和本库同在 `plugin-data` 下。没有就是 `None`。
// TODO(会话列表改 JS 之后)：用户都升过来了就删掉这次搬迁，连同会话插件库里那几张表。
fn sessions_store_path(own: &Path) -> Option<PathBuf> {
    let old = own
        .parent()?
        .parent()?
        .join("smelt.sessions")
        .join("store.sqlite3");
    old.is_file().then_some(old)
}

/// 新建的库从会话插件的旧库里抄一次侧栏的表。旧库是哪一版都行：那边没有的表跳过。抄不成
/// 只记一句，按空的用。
fn copy_from_sessions(store: &PluginStore, old: &Path) {
    let copied = store.read(|db| {
        db.execute("ATTACH DATABASE ?1 AS old", params![old.to_string_lossy()])?;
        let copied = copy_tables(db);
        let detached = db.execute_batch("DETACH DATABASE old");
        copied.and(detached)
    });
    if let Err(error) = copied {
        eprintln!("[session-list] 从会话插件的库搬侧栏状态失败，按空的用：{error}");
    }
}

fn copy_tables(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    let tx = db.unchecked_transaction()?;
    for table in CONVERSATION_TABLES
        .into_iter()
        .chain(ROOT_TABLES)
        .chain(WORKSET_TABLES)
        .chain(["sidebar", "avatar_choice"])
    {
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM old.sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get(0),
        )?;
        if exists {
            tx.execute(
                &format!("INSERT INTO main.{table} SELECT * FROM old.{table}"),
                [],
            )?;
        }
    }
    tx.commit()
}

fn pairs(db: &rusqlite::Connection, sql: &str) -> rusqlite::Result<Vec<(RuntimeKey, String)>> {
    let mut statement = db.prepare(sql)?;
    let rows = statement
        .query_map([], |row| {
            Ok((machine_of(row.get::<_, String>(0)?), row.get(1)?))
        })?
        .collect();
    rows
}

fn replace_order(
    tx: &Transaction,
    table: &str,
    column: &str,
    machine: &RuntimeKey,
    keys: &[String],
) -> rusqlite::Result<()> {
    let machine = machine_column(machine);
    tx.execute(
        &format!("DELETE FROM {table} WHERE machine = ?1"),
        params![machine],
    )?;
    let mut insert = tx.prepare(&format!(
        "INSERT OR IGNORE INTO {table} (machine, {column}, position) VALUES (?1, ?2, ?3)"
    ))?;
    for (position, key) in keys.iter().enumerate() {
        insert.execute(params![machine, key, position as i64])?;
    }
    Ok(())
}

fn prune_table(
    tx: &Transaction,
    table: &str,
    column: &str,
    machine: &str,
    keep: &HashSet<String>,
) -> rusqlite::Result<()> {
    let stored: Vec<String> = {
        let mut statement =
            tx.prepare(&format!("SELECT {column} FROM {table} WHERE machine = ?1"))?;
        statement
            .query_map(params![machine], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for key in stored.iter().filter(|key| !keep.contains(*key)) {
        tx.execute(
            &format!("DELETE FROM {table} WHERE machine = ?1 AND {column} = ?2"),
            params![machine, key],
        )?;
    }
    Ok(())
}

fn machine_column(machine: &RuntimeKey) -> &str {
    match machine {
        RuntimeKey::Local => "",
        RuntimeKey::Remote(id) => id,
    }
}

fn machine_of(column: String) -> RuntimeKey {
    if column.is_empty() {
        RuntimeKey::Local
    } else {
        RuntimeKey::Remote(column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ConversationKey;

    fn store() -> (tempfile::TempDir, ListStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ListStore::open_at(&dir.path().join("store.sqlite3"));
        (dir, store)
    }

    fn key(machine: RuntimeKey, id: &str) -> ConversationKey {
        ConversationKey {
            machine,
            id: id.into(),
        }
    }

    fn remote() -> RuntimeKey {
        RuntimeKey::Remote("box".into())
    }

    #[test]
    fn pins_keep_pin_order_per_machine() {
        let (_dir, store) = store();
        store.set_pinned(&Pin::Conversation(key(RuntimeKey::Local, "b")), true);
        store.set_pinned(&Pin::Conversation(key(RuntimeKey::Local, "a")), true);
        store.set_pinned(&Pin::Conversation(key(remote(), "c")), true);
        // 再置顶一次不改位置。
        store.set_pinned(&Pin::Conversation(key(RuntimeKey::Local, "b")), true);
        store.set_pinned(
            &Pin::Project {
                machine: RuntimeKey::Local,
                root: "/repo".into(),
            },
            true,
        );
        store.set_pinned(
            &Pin::Workset {
                machine: remote(),
                id: "w".into(),
            },
            true,
        );

        let ui = store.sidebar();
        let local = &ui.machines[&RuntimeKey::Local];
        assert_eq!(local.pinned_conversations, ["b", "a"]);
        assert_eq!(local.pinned_projects, ["/repo"]);
        let box_ui = &ui.machines[&remote()];
        assert_eq!(box_ui.pinned_conversations, ["c"]);
        assert!(box_ui.pinned_worksets.contains("w"));

        store.set_pinned(&Pin::Conversation(key(RuntimeKey::Local, "b")), false);
        store.set_pinned(&Pin::Conversation(key(RuntimeKey::Local, "d")), true);
        assert_eq!(
            store.sidebar().machines[&RuntimeKey::Local].pinned_conversations,
            ["a", "d"]
        );
    }

    #[test]
    fn order_is_replaced_whole_and_collapse_toggles() {
        let (_dir, store) = store();
        let project = |machine: RuntimeKey, root: &str| ProjectKey {
            machine,
            root: root.into(),
        };
        store.set_project_order(&[
            project(RuntimeKey::Local, "/b"),
            project(RuntimeKey::Local, "/a"),
        ]);
        store.set_project_order(&[
            project(RuntimeKey::Local, "/a"),
            project(remote(), "/r"),
            project(RuntimeKey::Local, "/c"),
        ]);
        store.set_conversation_order(&remote(), &["x".into(), "y".into()]);
        store.set_collapsed(
            &Fold::Project {
                machine: RuntimeKey::Local,
                root: "/a".into(),
            },
            true,
        );
        store.set_collapsed(
            &Fold::Workset {
                machine: remote(),
                id: "w".into(),
            },
            true,
        );
        store.set_collapsed(&Fold::Conversations, true);

        let ui = store.sidebar();
        let local = &ui.machines[&RuntimeKey::Local];
        // 整份换掉，跨机器的先后照存。
        assert_eq!(
            ui.project_order,
            [
                project(RuntimeKey::Local, "/a"),
                project(remote(), "/r"),
                project(RuntimeKey::Local, "/c"),
            ]
        );
        assert!(local.collapsed_projects.contains("/a"));
        assert_eq!(ui.machines[&remote()].conversation_order, ["x", "y"]);
        assert!(ui.machines[&remote()].collapsed_worksets.contains("w"));
        assert!(ui.conversations_collapsed);

        store.set_collapsed(&Fold::Conversations, false);
        store.set_collapsed(
            &Fold::Project {
                machine: RuntimeKey::Local,
                root: "/a".into(),
            },
            false,
        );
        let ui = store.sidebar();
        assert!(!ui.conversations_collapsed);
        // 本机什么都不剩时不留这一项。
        assert!(
            ui.machines
                .get(&RuntimeKey::Local)
                .is_none_or(|local| local.collapsed_projects.is_empty())
        );
    }

    #[test]
    fn avatar_choice_round_trips() {
        let (_dir, store) = store();
        assert_eq!(store.avatar_choice(), AvatarChoice::Unset);
        for choice in [
            AvatarChoice::View("someone.avatars/avatar".into()),
            AvatarChoice::Icon,
            AvatarChoice::View("smelt.buddies/avatar".into()),
            AvatarChoice::Unset,
        ] {
            store.save_avatar_choice(&choice);
            assert_eq!(store.avatar_choice(), choice);
        }
    }

    #[test]
    fn prune_drops_rows_the_directory_no_longer_has() {
        let (_dir, store) = store();
        for id in ["keep", "gone"] {
            store.set_pinned(&Pin::Conversation(key(RuntimeKey::Local, id)), true);
        }
        store.set_conversation_order(&RuntimeKey::Local, &["gone".into(), "keep".into()]);
        store.set_pinned(&Pin::Conversation(key(remote(), "gone")), true);
        store.set_collapsed(
            &Fold::Project {
                machine: RuntimeKey::Local,
                root: "/old".into(),
            },
            true,
        );
        store.set_collapsed(
            &Fold::Workset {
                machine: RuntimeKey::Local,
                id: "w-old".into(),
            },
            true,
        );

        let known = KnownIds {
            conversations: HashSet::from(["keep".to_string()]),
            roots: HashSet::new(),
            worksets: HashSet::new(),
        };
        store.prune(&RuntimeKey::Local, &known);

        let ui = store.sidebar();
        let local = &ui.machines[&RuntimeKey::Local];
        assert_eq!(local.pinned_conversations, ["keep"]);
        assert_eq!(local.conversation_order, ["keep"]);
        assert!(local.collapsed_projects.is_empty());
        assert!(local.collapsed_worksets.is_empty());
        // 别的机器不受影响。
        assert_eq!(ui.machines[&remote()].pinned_conversations, ["gone"]);

        store.forget_machine(&remote());
        assert!(!store.sidebar().machines.contains_key(&remote()));
    }

    #[test]
    fn a_new_store_copies_the_sessions_tables() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("smelt.sessions").join("store.sqlite3");
        let old = PluginStore::open(&old_path).unwrap();
        // 会话插件库的第一版：还没有头像那张表，另有不归列表的。
        old.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE pinned_conversation (machine TEXT NOT NULL,
                    conversation_id TEXT NOT NULL, position INTEGER NOT NULL,
                    PRIMARY KEY (machine, conversation_id));
                 CREATE TABLE sidebar (id INTEGER PRIMARY KEY,
                    conversations_collapsed INTEGER NOT NULL);
                 CREATE TABLE selection (id INTEGER PRIMARY KEY, machine TEXT,
                    conversation_id TEXT);
                 INSERT INTO pinned_conversation VALUES ('', 'c', 0);
                 INSERT INTO sidebar VALUES (1, 1);
                 INSERT INTO selection VALUES (1, '', 'c');",
            )
        })
        .unwrap();
        let own_path = dir.path().join("smelt.session-list").join("store.sqlite3");
        assert_eq!(sessions_store_path(&own_path), Some(old_path.clone()));
        let empty = tempfile::tempdir().unwrap();
        let alone = empty
            .path()
            .join("smelt.session-list")
            .join("store.sqlite3");
        assert_eq!(sessions_store_path(&alone), None);

        let own = PluginStore::open(&own_path).unwrap();
        own.migrate(MIGRATIONS).unwrap();
        copy_from_sessions(&own, &old_path);
        let store = ListStore {
            store: Some(Arc::new(own)),
        };
        let ui = store.sidebar();
        assert_eq!(ui.machines[&RuntimeKey::Local].pinned_conversations, ["c"]);
        assert!(ui.conversations_collapsed);
        assert_eq!(store.avatar_choice(), AvatarChoice::default());
    }
}
