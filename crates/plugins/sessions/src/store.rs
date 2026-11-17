//! 会话插件自己的库（`~/.smelt/plugin-data/smelt.sessions.sqlite3`）：侧栏的置顶、手动顺序、
//! 折叠，每段会话的分屏布局，和上次选中的会话。都是这台桌面自己的界面状态，运行时不存。
//!
//! 侧栏那部分启动时整份读一次（[`SessionsStore::sidebar`]），之后内存里那份是缓存，改的时候
//! 同一个事件里写库（见 `model::Sessions::set_pinned` 等）。分屏布局在建会话视图时按需读。
//! 都是小量写，在事件里同步写；render 里不碰库。库打不开（权限、版本比插件新）就只在内存里
//! 用，日志里留一句。
//!
//! 行里的对话 id、项目根、工作集 id 都指着那台机器的工作区目录；目录里没了的由
//! [`SessionsStore::prune`] 清掉，移除的机器由 [`SessionsStore::forget_machine`] 清掉。

use std::collections::HashSet;
use std::sync::Arc;

use smelt_plugin_api::rusqlite::{self, OptionalExtension as _, Transaction, params};
use smelt_plugin_api::{PluginStore, RuntimeKey};

use crate::OWNER;
use crate::model::{ConversationKey, Fold, KnownIds, Pin, SidebarUi};

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
"];

/// 按对话 id 记的表：目录里没了这段对话就清掉。
const CONVERSATION_TABLES: [&str; 3] = [
    "pinned_conversation",
    "conversation_order",
    "session_layout",
];
/// 按项目根记的表。
const ROOT_TABLES: [&str; 3] = ["pinned_project", "project_order", "collapsed_project"];
/// 按工作集 id 记的表。
const WORKSET_TABLES: [&str; 2] = ["pinned_workset", "collapsed_workset"];

/// 一段会话存下的分屏：眼下是哪一格，和分屏怎么摆。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SavedLayout {
    /// 上次眼下的那一格（守护会话 id，对话和终端都算）。
    pub(crate) active_pane: Option<String>,
    /// 分屏的存档：`split_layout` 定的、带版本号的 JSON，这里只原样存取。
    pub(crate) layout: Option<String>,
}

pub(crate) struct SessionsStore {
    store: Option<Arc<PluginStore>>,
}

impl SessionsStore {
    /// 打开本插件的库。打不开就是个什么都不记的空壳。
    pub(crate) fn open() -> Self {
        let store = smelt_plugin_api::plugin_store(OWNER).and_then(|store| {
            store.migrate(MIGRATIONS)?;
            Ok(store)
        });
        match store {
            Ok(store) => Self { store: Some(store) },
            Err(error) => {
                eprintln!("[sessions] 打不开会话的库，这次不记侧栏和分屏：{error}");
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
                ui.machine_entry(&machine).project_order.push(root);
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
            eprintln!("[sessions] 读侧栏状态失败，按默认显示：{error}");
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

    /// 换掉那台机器上项目的手动顺序（整份）。
    pub(crate) fn set_project_order(&self, machine: &RuntimeKey, roots: &[String]) {
        self.write("项目顺序", |tx| {
            replace_order(tx, "project_order", "root", machine, roots)
        });
    }

    /// 换掉那台机器上会话的手动顺序（整份）。
    pub(crate) fn set_conversation_order(&self, machine: &RuntimeKey, ids: &[String]) {
        self.write("会话顺序", |tx| {
            replace_order(tx, "conversation_order", "conversation_id", machine, ids)
        });
    }

    /// 那段会话存下的分屏；没存过、库打不开是 `None`。建会话视图时读，不在 render 里。
    pub(crate) fn layout(&self, key: &ConversationKey) -> Option<SavedLayout> {
        let store = self.store.as_ref()?;
        let read = store.read(|db| {
            db.query_row(
                "SELECT active_pane_id, layout FROM session_layout
                 WHERE machine = ?1 AND conversation_id = ?2",
                params![machine_column(&key.machine), key.id],
                |row| {
                    Ok(SavedLayout {
                        active_pane: row.get(0)?,
                        layout: row.get(1)?,
                    })
                },
            )
            .optional()
        });
        read.unwrap_or_else(|error| {
            eprintln!("[sessions] 读分屏失败，按默认摆：{error}");
            None
        })
    }

    pub(crate) fn save_layout(&self, key: &ConversationKey, layout: &SavedLayout) {
        self.write("分屏", |tx| {
            tx.execute(
                "INSERT INTO session_layout (machine, conversation_id, active_pane_id, layout)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (machine, conversation_id) DO UPDATE SET
                     active_pane_id = excluded.active_pane_id,
                     layout = excluded.layout",
                params![
                    machine_column(&key.machine),
                    key.id,
                    layout.active_pane,
                    layout.layout
                ],
            )?;
            Ok(())
        });
    }

    /// 上次选中的会话。
    pub(crate) fn selection(&self) -> Option<ConversationKey> {
        let store = self.store.as_ref()?;
        let read = store.read(|db| {
            db.query_row(
                "SELECT machine, conversation_id FROM selection WHERE id = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
        });
        match read {
            Ok(row) => row.map(|(machine, id)| ConversationKey {
                machine: machine_of(machine),
                id,
            }),
            Err(error) => {
                eprintln!("[sessions] 读上次的选中失败：{error}");
                None
            }
        }
    }

    pub(crate) fn save_selection(&self, selection: Option<&ConversationKey>) {
        self.write("选中", |tx| {
            match selection {
                Some(key) => tx.execute(
                    "INSERT INTO selection (id, machine, conversation_id) VALUES (1, ?1, ?2)
                     ON CONFLICT (id) DO UPDATE SET
                         machine = excluded.machine,
                         conversation_id = excluded.conversation_id",
                    params![machine_column(&key.machine), key.id],
                )?,
                None => tx.execute("DELETE FROM selection", [])?,
            };
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
            prune_table(
                tx,
                "selection",
                "conversation_id",
                machine,
                &known.conversations,
            )?;
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
                .chain(["selection"])
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
            eprintln!("[sessions] 记{what}失败：{error}");
        }
    }
}

/// 读两列（机器、键）的结果。
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

    fn store() -> (tempfile::TempDir, SessionsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionsStore::open_at(&dir.path().join("sessions.sqlite3"));
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
        store.set_project_order(&RuntimeKey::Local, &["/b".into(), "/a".into()]);
        store.set_project_order(&RuntimeKey::Local, &["/a".into(), "/c".into()]);
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
        assert_eq!(local.project_order, ["/a", "/c"]);
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
        assert!(
            ui.machines[&RuntimeKey::Local]
                .collapsed_projects
                .is_empty()
        );
    }

    #[test]
    fn layout_and_selection_round_trip() {
        let (_dir, store) = store();
        let conversation = key(remote(), "c");
        assert_eq!(store.layout(&conversation), None);
        assert_eq!(store.selection(), None);

        let layout = SavedLayout {
            active_pane: Some("t2".into()),
            layout: Some(r#"{"v":1}"#.into()),
        };
        store.save_layout(&conversation, &layout);
        assert_eq!(store.layout(&conversation), Some(layout));
        // 同一台机器的同名对话 id 不串到本机。
        assert_eq!(store.layout(&key(RuntimeKey::Local, "c")), None);

        store.save_selection(Some(&conversation));
        assert_eq!(store.selection(), Some(conversation.clone()));
        store.save_selection(Some(&key(RuntimeKey::Local, "d")));
        assert_eq!(store.selection(), Some(key(RuntimeKey::Local, "d")));
        store.save_selection(None);
        assert_eq!(store.selection(), None);
    }

    #[test]
    fn prune_drops_rows_the_directory_no_longer_has() {
        let (_dir, store) = store();
        for id in ["keep", "gone"] {
            store.set_pinned(&Pin::Conversation(key(RuntimeKey::Local, id)), true);
            store.save_layout(&key(RuntimeKey::Local, id), &SavedLayout::default());
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
        store.save_selection(Some(&key(RuntimeKey::Local, "gone")));

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
        assert_eq!(store.layout(&key(RuntimeKey::Local, "gone")), None);
        assert!(store.layout(&key(RuntimeKey::Local, "keep")).is_some());
        assert_eq!(store.selection(), None);
        // 别的机器不受影响。
        assert_eq!(ui.machines[&remote()].pinned_conversations, ["gone"]);

        store.forget_machine(&remote());
        assert!(!store.sidebar().machines.contains_key(&remote()));
    }
}
