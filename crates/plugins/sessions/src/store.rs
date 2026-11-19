//! 会话插件自己的库（`~/.smelt/plugin-data/smelt.sessions/store.sqlite3`）：每段会话的分屏布局，
//! 和上次选中的会话。都是这台桌面自己的界面状态，运行时不存。
//!
//! 分屏布局在建会话视图时按需读，选中启动时读一次。都是小量写，在事件里同步写；render 里
//! 不碰库。库打不开（权限、版本比插件新）就只在内存里用，日志里留一句。
//!
//! 行里的对话 id 指着那台机器的工作区目录；目录里没了的由 [`SessionsStore::prune`] 清掉，
//! 移除的机器由 [`SessionsStore::forget_machine`] 清掉。
//!
//! 第一版里的置顶、顺序、折叠、行首头像那几张表（`pinned_*`、`*_order`、`collapsed_*`、
//! `sidebar`、`avatar_choice`）归会话列表插件了，它第一次建库时从这里抄一份过去
//! （`smelt.session-list` 的 `store.rs`）。这边不再读写它们。
// TODO(会话列表改 JS 之后)：那时列表的库早已抄好，加一版迁移删掉这几张表。

use std::collections::HashSet;
use std::sync::Arc;

use smelt_plugin_api::rusqlite::{self, OptionalExtension as _, Transaction, params};
use smelt_plugin_api::{PluginStore, RuntimeKey};

use crate::OWNER;
use crate::model::ConversationKey;

const MIGRATIONS: &[&str] = &[
    "
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
",
    "
    CREATE TABLE avatar_choice (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        view_id TEXT
    );
",
];

/// 按对话 id 记的表：目录里没了这段对话就清掉。
const CONVERSATION_TABLES: [&str; 2] = ["session_layout", "selection"];

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
                eprintln!("[sessions] 打不开会话的库，这次不记分屏和选中：{error}");
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

    /// 清掉那台机器上目录里已经没有的对话留下的行。`known` 是那台机器整份目录里还在的对话
    /// id；目录还没收到时不要调。
    pub(crate) fn prune(&self, machine: &RuntimeKey, known: &HashSet<String>) {
        self.write("清理", |tx| {
            let machine = machine_column(machine);
            for table in CONVERSATION_TABLES {
                prune_table(tx, table, "conversation_id", machine, known)?;
            }
            Ok(())
        });
    }

    /// 机器移除了：它的行全删。
    pub(crate) fn forget_machine(&self, machine: &RuntimeKey) {
        self.write("清理", |tx| {
            let machine = machine_column(machine);
            for table in CONVERSATION_TABLES {
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
            store.save_layout(&key(RuntimeKey::Local, id), &SavedLayout::default());
        }
        store.save_layout(&key(remote(), "gone"), &SavedLayout::default());
        store.save_selection(Some(&key(RuntimeKey::Local, "gone")));

        store.prune(&RuntimeKey::Local, &HashSet::from(["keep".to_string()]));

        assert_eq!(store.layout(&key(RuntimeKey::Local, "gone")), None);
        assert!(store.layout(&key(RuntimeKey::Local, "keep")).is_some());
        assert_eq!(store.selection(), None);
        // 别的机器不受影响。
        assert!(store.layout(&key(remote(), "gone")).is_some());

        store.forget_machine(&remote());
        assert_eq!(store.layout(&key(remote(), "gone")), None);
    }
}
