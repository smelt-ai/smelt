//! 原生 provider 身份及混合历史的引用。正文仍由 provider 的原生存档持有。

use crate::{StoreError, WorkspaceAcpRecord};
use rusqlite::{Connection, Transaction, params};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderHistory {
    pub threads: Vec<ProviderThreadRecord>,
    pub segments: Vec<ProviderSegmentRecord>,
    pub handoffs: Vec<ContextHandoffRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderThreadRecord {
    pub provider_key: String,
    pub agent: String,
    pub profile_id: Option<String>,
    pub agent_definition_id: Option<String>,
    pub history_session_id: Option<String>,
    /// 已覆盖的 segment 数量，半开上界。
    pub covered_segment: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSegmentRecord {
    pub position: i64,
    pub provider_key: String,
    pub history_session_id: String,
    pub first_turn: i64,
    pub end_turn: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextHandoffRecord {
    pub delivery_id: String,
    pub provider_key: String,
    pub history_session_id: Option<String>,
    pub through_segment: i64,
    pub status: String,
}

pub(crate) fn read(connection: &Connection, sid: &str) -> Result<ProviderHistory, StoreError> {
    let mut threads = connection.prepare(
        "SELECT provider_key, agent, profile_id, agent_definition_id, history_session_id, covered_segment
         FROM acp_provider_thread WHERE session_id = ?1 ORDER BY provider_key",
    )?;
    let threads = threads
        .query_map([sid], |row| {
            Ok(ProviderThreadRecord {
                provider_key: row.get(0)?,
                agent: row.get(1)?,
                profile_id: row.get(2)?,
                agent_definition_id: row.get(3)?,
                history_session_id: row.get(4)?,
                covered_segment: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut segments = connection.prepare(
        "SELECT position, provider_key, history_session_id, first_turn, end_turn
         FROM acp_provider_segment WHERE session_id = ?1 ORDER BY position",
    )?;
    let segments = segments
        .query_map([sid], |row| {
            Ok(ProviderSegmentRecord {
                position: row.get(0)?,
                provider_key: row.get(1)?,
                history_session_id: row.get(2)?,
                first_turn: row.get(3)?,
                end_turn: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut handoffs = connection.prepare(
        "SELECT delivery_id, provider_key, history_session_id, through_segment, status
         FROM acp_context_handoff WHERE session_id = ?1 ORDER BY delivery_id",
    )?;
    let handoffs = handoffs
        .query_map([sid], |row| {
            Ok(ContextHandoffRecord {
                delivery_id: row.get(0)?,
                provider_key: row.get(1)?,
                history_session_id: row.get(2)?,
                through_segment: row.get(3)?,
                status: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ProviderHistory {
        threads,
        segments,
        handoffs,
    })
}

pub(crate) fn write(
    tx: &Transaction<'_>,
    sid: &str,
    history: &ProviderHistory,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM acp_provider_thread WHERE session_id = ?1",
        [sid],
    )?;
    for t in &history.threads {
        tx.execute(
            "INSERT INTO acp_provider_thread VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                sid,
                t.provider_key,
                t.agent,
                t.profile_id,
                t.agent_definition_id,
                t.history_session_id,
                t.covered_segment
            ],
        )?;
    }
    for s in &history.segments {
        tx.execute(
            "INSERT INTO acp_provider_segment VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                sid,
                s.position,
                s.provider_key,
                s.history_session_id,
                s.first_turn,
                s.end_turn
            ],
        )?;
    }
    for h in &history.handoffs {
        handoff(tx, sid, h)?;
    }
    Ok(())
}

pub(crate) fn handoff(
    tx: &Transaction<'_>,
    sid: &str,
    h: &ContextHandoffRecord,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO acp_context_handoff VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id, delivery_id) DO UPDATE SET status = excluded.status",
        params![
            sid,
            h.delivery_id,
            h.provider_key,
            h.history_session_id,
            h.through_segment,
            h.status
        ],
    )?;
    if h.status == "delivered" {
        tx.execute(
            "UPDATE acp_provider_thread SET covered_segment = MAX(covered_segment, ?3)
             WHERE session_id = ?1 AND provider_key = ?2 AND history_session_id IS ?4",
            params![sid, h.provider_key, h.through_segment, h.history_session_id],
        )?;
    }
    Ok(())
}

pub(crate) fn switch(
    tx: &Transaction<'_>,
    sid: &str,
    expected: &str,
    active: &WorkspaceAcpRecord,
    history: &ProviderHistory,
) -> Result<(), StoreError> {
    let old: (String, Option<String>, Option<String>) = tx.query_row(
        "SELECT agent, profile_id, agent_definition_id FROM acp_detail WHERE session_id = ?1",
        [sid],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let key = old
        .2
        .map(|id| format!("agent:{id}"))
        .or_else(|| old.1.map(|id| format!("profile:{id}")))
        .unwrap_or(old.0);
    if key != expected {
        return Err(
            std::io::Error::other("The active provider changed; refresh before switching").into(),
        );
    }
    write(tx, sid, history)?;
    tx.execute(
        "UPDATE acp_detail SET agent = ?2, profile_id = ?3, agent_definition_id = ?4,
         history_session_id = ?5, launch_command = ?6, pending_prompt = NULL,
         pending_agent_preset = ?7 WHERE session_id = ?1",
        params![
            sid,
            active.agent,
            active.profile_id,
            active.agent_definition_id,
            active.history_session_id,
            active.launch_command,
            active.pending_agent_preset
        ],
    )?;
    tx.execute("DELETE FROM acp_launch_env WHERE session_id = ?1", [sid])?;
    for (name, value) in &active.launch_env {
        tx.execute(
            "INSERT INTO acp_launch_env VALUES (?1, ?2, ?3)",
            params![sid, name, value],
        )?;
    }
    tx.execute("DELETE FROM acp_config_value WHERE session_id = ?1", [sid])?;
    for (i, (key, value)) in active.config_values.iter().enumerate() {
        tx.execute(
            "INSERT INTO acp_config_value VALUES (?1, ?2, ?3, ?4)",
            params![sid, i as i64, key, value],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(crate::schema::CREATE_SCHEMA)
            .unwrap();
        connection.execute_batch("PRAGMA foreign_keys = ON;
            INSERT INTO session_group(id, position) VALUES ('g', 0);
            INSERT INTO session(id, group_id, kind) VALUES ('s', 'g', 'acp');
            INSERT INTO acp_detail(session_id, agent, launch_command) VALUES ('s', 'codex', 'codex');").unwrap();
        connection
    }

    fn history() -> ProviderHistory {
        ProviderHistory {
            threads: vec![ProviderThreadRecord {
                provider_key: "codex".into(),
                agent: "codex".into(),
                profile_id: None,
                agent_definition_id: None,
                history_session_id: Some("a".into()),
                covered_segment: 0,
            }],
            segments: vec![ProviderSegmentRecord {
                position: 0,
                provider_key: "codex".into(),
                history_session_id: "a".into(),
                first_turn: 0,
                end_turn: 2,
            }],
            handoffs: vec![],
        }
    }

    #[test]
    fn references_round_trip_and_follow_conversation_deletion() {
        let mut c = fixture();
        let tx = c.transaction().unwrap();
        write(&tx, "s", &history()).unwrap();
        tx.commit().unwrap();
        assert_eq!(read(&c, "s").unwrap(), history());
        c.execute("DELETE FROM session_group", []).unwrap();
        assert_eq!(read(&c, "s").unwrap(), ProviderHistory::default());
    }

    #[test]
    fn invalid_segment_rolls_back_the_whole_switch() {
        let mut c = fixture();
        let mut invalid = history();
        invalid.segments[0].end_turn = 0;
        let tx = c.transaction().unwrap();
        assert!(switch(&tx, "s", "codex", &WorkspaceAcpRecord::default(), &invalid).is_err());
        drop(tx);
        assert_eq!(read(&c, "s").unwrap(), ProviderHistory::default());
        assert_eq!(
            c.query_row("SELECT agent FROM acp_detail", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "codex"
        );
    }

    #[test]
    fn stale_switch_is_rejected_without_writing() {
        let mut c = fixture();
        let tx = c.transaction().unwrap();
        assert!(
            switch(
                &tx,
                "s",
                "claude",
                &WorkspaceAcpRecord::default(),
                &history()
            )
            .is_err()
        );
        assert!(read(&tx, "s").unwrap().threads.is_empty());
    }

    #[test]
    fn coverage_only_advances_for_confirmed_delivery_to_the_same_native_session() {
        let mut c = fixture();
        let tx = c.transaction().unwrap();
        write(&tx, "s", &history()).unwrap();
        let mut h = ContextHandoffRecord {
            delivery_id: "d".into(),
            provider_key: "codex".into(),
            history_session_id: Some("a".into()),
            through_segment: 1,
            status: "unknown".into(),
        };
        handoff(&tx, "s", &h).unwrap();
        assert_eq!(read(&tx, "s").unwrap().threads[0].covered_segment, 0);
        h.status = "delivered".into();
        handoff(&tx, "s", &h).unwrap();
        assert_eq!(read(&tx, "s").unwrap().threads[0].covered_segment, 1);
    }

    fn old_database(path: &std::path::Path, malformed: bool) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            crate::schema::CREATE_SCHEMA
                .split("CREATE TABLE acp_provider_thread")
                .next()
                .unwrap(),
        )
        .unwrap();
        c.execute_batch("INSERT INTO session_group(id, position, custom_title) VALUES ('g', 0, 'keep me');
            INSERT INTO session(id, group_id, kind) VALUES ('s', 'g', 'acp');
            INSERT INTO acp_detail(session_id, agent, launch_command) VALUES ('s', 'codex', 'codex');
            PRAGMA user_version = 40;").unwrap();
        if malformed {
            c.execute_batch("ALTER TABLE acp_detail ADD COLUMN unexpected TEXT")
                .unwrap();
        }
    }

    #[test]
    fn v40_migration_preserves_rows_and_a_complete_v40_backup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("smelt.sqlite3");
        old_database(&path, false);
        let store = crate::Store::open(&path).unwrap();
        assert_eq!(crate::Store::schema_version(&path), Some(41));
        assert_eq!(
            store.workspace_directory().unwrap().1.sessions[0]
                .custom_title
                .as_deref(),
            Some("keep me")
        );
        assert_eq!(
            store.provider_history("s").unwrap(),
            ProviderHistory::default()
        );
        assert!(
            store
                .remember_context_handoff(
                    "missing",
                    &ContextHandoffRecord {
                        delivery_id: "d".into(),
                        provider_key: "codex".into(),
                        history_session_id: None,
                        through_segment: 0,
                        status: "prepared".into(),
                    }
                )
                .is_err()
        );
        let backup = Connection::open(temp.path().join("smelt.sqlite3.bak-v40")).unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            40
        );
        assert_eq!(
            backup
                .query_row("SELECT custom_title FROM session_group", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "keep me"
        );
        assert_eq!(
            backup
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            backup
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name = 'acp_provider_thread'",
                    [],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn a_failed_v41_shape_check_rolls_back_tables_and_version() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("smelt.sqlite3");
        old_database(&path, true);
        assert!(crate::Store::open(&path).is_err());
        let c = Connection::open(&path).unwrap();
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            40
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'acp_provider_thread'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            c.query_row("SELECT custom_title FROM session_group", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "keep me"
        );
    }
    #[test]
    fn directory_rewrites_preserve_provider_references_and_reject_stale_switches() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("smelt.sqlite3");
        old_database(&path, false);
        let store = crate::Store::open(path).unwrap();
        let mut state = history();
        state.threads.push(ProviderThreadRecord {
            provider_key: "claude".into(),
            agent: "claude".into(),
            profile_id: None,
            agent_definition_id: None,
            history_session_id: None,
            covered_segment: 0,
        });
        let target = WorkspaceAcpRecord {
            agent: "claude".into(),
            launch_command: "claude-acp".into(),
            ..Default::default()
        };
        store
            .switch_provider("s", "codex", &target, &state)
            .unwrap();
        assert!(
            store
                .switch_provider("s", "codex", &target, &state)
                .is_err()
        );
        let before = store.provider_history("s").unwrap();
        let record = store.workspace_directory().unwrap().1.sessions.remove(0);
        store.update_workspace_conversation(&record).unwrap();
        assert_eq!(store.provider_history("s").unwrap(), before);
    }
}
