//! 群组会话独立运行时存储（`~/.smelt/groups.sqlite3`）。
//!
//! 设计见 `docs/agent-groups.md` §3：
//! - 由 `smeltd` 独占写入，独立于 `smelt.sqlite3`（不动核心 `STORE_SCHEMA_VERSION`）；
//! - 5 张极简表：`agent_group`、`agent_group_member`、`agent_group_message`、
//!   `agent_group_message_mention`、`remote_group_member_grant`；
//! - 遵循 Commit-before-Publish：写消息、递增 `next_seq` 与写入全部 mention 行在同一原子事务完成。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{
    DatabasePresence, OpenSource, StoreError, database_presence, missing_database_error,
    orphan_sidecar_error,
};

pub const GROUPS_DATABASE_FILE_NAME: &str = "groups.sqlite3";
pub const GROUP_STORE_SCHEMA_VERSION: u32 = 1;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const CREATE_GROUPS_SCHEMA: &str = "
CREATE TABLE agent_group (
    id                      TEXT PRIMARY KEY,
    title                   TEXT NOT NULL,
    description             TEXT NOT NULL DEFAULT '',
    workset_id              TEXT,
    mode                    TEXT NOT NULL DEFAULT 'mention' CHECK (mode IN ('mention', 'coordinator')),
    coordinator_member_id   TEXT,
    allow_worker_cross_talk INTEGER NOT NULL DEFAULT 1 CHECK (allow_worker_cross_talk IN (0, 1)),
    max_chain_hops          INTEGER NOT NULL DEFAULT 10 CHECK (max_chain_hops > 0),
    max_chain_depth         INTEGER NOT NULL DEFAULT 6 CHECK (max_chain_depth > 0),
    max_pair_ping_pong      INTEGER NOT NULL DEFAULT 3 CHECK (max_pair_ping_pong > 0),
    auto_bridge_mentions    INTEGER NOT NULL DEFAULT 0 CHECK (auto_bridge_mentions IN (0, 1)),
    paused                  INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    next_seq                INTEGER NOT NULL DEFAULT 1 CHECK (next_seq >= 1),
    archived_at_ms          INTEGER,
    created_at_ms           INTEGER NOT NULL,
    updated_at_ms           INTEGER NOT NULL
);

CREATE TABLE agent_group_member (
    id                       TEXT PRIMARY KEY,
    group_id                 TEXT NOT NULL REFERENCES agent_group(id) ON DELETE CASCADE,
    alias                    TEXT NOT NULL,
    display_name             TEXT NOT NULL,
    ownership                TEXT NOT NULL DEFAULT 'group_owned' CHECK (ownership IN ('group_owned', 'linked')),
    runtime_id               TEXT,
    session_id               TEXT NOT NULL,
    role_kind                TEXT NOT NULL DEFAULT 'worker' CHECK (role_kind IN ('coordinator', 'worker', 'reviewer', 'observer')),
    role_title               TEXT NOT NULL DEFAULT '',
    role_prompt              TEXT NOT NULL DEFAULT '',
    workspace_mode           TEXT NOT NULL DEFAULT 'shared_readonly' CHECK (workspace_mode IN ('isolated_worktree', 'shared_readonly', 'shared_leased')),
    tool_restriction_profile TEXT NOT NULL DEFAULT 'default',
    briefing_sent            INTEGER NOT NULL DEFAULT 0 CHECK (briefing_sent IN (0, 1)),
    last_delivered_seq       INTEGER NOT NULL DEFAULT 0 CHECK (last_delivered_seq >= 0),
    last_read_seq            INTEGER NOT NULL DEFAULT 0 CHECK (last_read_seq >= 0),
    muted                    INTEGER NOT NULL DEFAULT 0 CHECK (muted IN (0, 1)),
    removed_at_ms            INTEGER,
    created_at_ms            INTEGER NOT NULL,
    UNIQUE(group_id, alias)
);
CREATE INDEX idx_agm_session ON agent_group_member(runtime_id, session_id);

CREATE TABLE agent_group_message (
    id                   TEXT PRIMARY KEY,
    group_id             TEXT NOT NULL REFERENCES agent_group(id) ON DELETE CASCADE,
    seq                  INTEGER NOT NULL CHECK (seq >= 1),
    sender_type          TEXT NOT NULL CHECK (sender_type IN ('user', 'member', 'system')),
    sender_member_id     TEXT REFERENCES agent_group_member(id) ON DELETE SET NULL,
    kind                 TEXT NOT NULL DEFAULT 'chat' CHECK (kind IN ('chat', 'task', 'decision', 'blind_prompt', 'blind_submission', 'system')),
    content              TEXT NOT NULL,
    attachments_json     BLOB NOT NULL DEFAULT X'5B5D',
    reply_to_seq         INTEGER,
    thread_root_seq      INTEGER,
    blind_deadline_at_ms INTEGER,
    blind_revealed_at_ms INTEGER,
    task_status          TEXT CHECK (task_status IS NULL OR task_status IN ('open', 'in_progress', 'done', 'blocked')),
    chain_id             TEXT,
    chain_depth          INTEGER NOT NULL DEFAULT 0 CHECK (chain_depth >= 0),
    auto_bridged         INTEGER NOT NULL DEFAULT 0 CHECK (auto_bridged IN (0, 1)),
    pinned               INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    idempotency_key      TEXT,
    created_at_ms        INTEGER NOT NULL,
    UNIQUE(group_id, seq),
    UNIQUE(group_id, idempotency_key)
);
CREATE INDEX idx_agmsg_thread ON agent_group_message(group_id, thread_root_seq, seq);
CREATE INDEX idx_agmsg_blind ON agent_group_message(group_id, reply_to_seq) WHERE kind = 'blind_submission';
CREATE INDEX idx_agmsg_open_tasks ON agent_group_message(group_id, task_status) WHERE kind = 'task';

CREATE TABLE agent_group_message_mention (
    message_id       TEXT NOT NULL REFERENCES agent_group_message(id) ON DELETE CASCADE,
    group_id         TEXT NOT NULL,
    target_member_id TEXT NOT NULL REFERENCES agent_group_member(id) ON DELETE CASCADE,
    mention_kind     TEXT NOT NULL DEFAULT 'direct' CHECK (mention_kind IN ('direct', 'all', 'coordinator_default', 'blind_invite')),
    delivery_status  TEXT NOT NULL DEFAULT 'queued' CHECK (delivery_status IN ('queued', 'offline_pending', 'pending_approval', 'delivered', 'submitted', 'coalesced', 'skipped', 'rejected', 'failed')),
    delivery_id      TEXT,
    status_reason    TEXT,
    updated_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (message_id, target_member_id)
);
CREATE INDEX idx_agmm_pending ON agent_group_message_mention(group_id, delivery_status)
    WHERE delivery_status IN ('queued', 'offline_pending', 'pending_approval');

CREATE TABLE remote_group_member_grant (
    host_runtime_id          TEXT NOT NULL,
    group_id                 TEXT NOT NULL,
    member_id                TEXT NOT NULL,
    local_session_id         TEXT NOT NULL,
    allow_remote_full_access INTEGER NOT NULL DEFAULT 0 CHECK (allow_remote_full_access IN (0, 1)),
    created_at_ms            INTEGER NOT NULL,
    PRIMARY KEY (host_runtime_id, group_id, member_id)
);
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupRecord {
    pub id: String,
    pub title: String,
    pub description: String,
    pub workset_id: Option<String>,
    pub mode: String,
    pub coordinator_member_id: Option<String>,
    pub allow_worker_cross_talk: bool,
    pub max_chain_hops: u32,
    pub max_chain_depth: u32,
    pub max_pair_ping_pong: u32,
    pub auto_bridge_mentions: bool,
    pub paused: bool,
    pub next_seq: i64,
    pub archived_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMemberRecord {
    pub id: String,
    pub group_id: String,
    pub alias: String,
    pub display_name: String,
    pub ownership: String,
    pub runtime_id: Option<String>,
    pub session_id: String,
    pub role_kind: String,
    pub role_title: String,
    pub role_prompt: String,
    pub workspace_mode: String,
    pub tool_restriction_profile: String,
    pub briefing_sent: bool,
    pub last_delivered_seq: i64,
    pub last_read_seq: i64,
    pub muted: bool,
    pub removed_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMessageRecord {
    pub id: String,
    pub group_id: String,
    pub seq: i64,
    pub sender_type: String,
    pub sender_member_id: Option<String>,
    pub kind: String,
    pub content: String,
    pub attachments_json: Vec<u8>,
    pub reply_to_seq: Option<i64>,
    pub thread_root_seq: Option<i64>,
    pub blind_deadline_at_ms: Option<i64>,
    pub blind_revealed_at_ms: Option<i64>,
    pub task_status: Option<String>,
    pub chain_id: Option<String>,
    pub chain_depth: u32,
    pub auto_bridged: bool,
    pub pinned: bool,
    pub idempotency_key: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMentionRecord {
    pub message_id: String,
    pub group_id: String,
    pub target_member_id: String,
    pub mention_kind: String,
    pub delivery_status: String,
    pub delivery_id: Option<String>,
    pub status_reason: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteGroupMemberGrantRecord {
    pub host_runtime_id: String,
    pub group_id: String,
    pub member_id: String,
    pub local_session_id: String,
    pub allow_remote_full_access: bool,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct NewGroupMessage {
    pub id: String,
    pub group_id: String,
    pub sender_type: String,
    pub sender_member_id: Option<String>,
    pub kind: String,
    pub content: String,
    pub attachments_json: Vec<u8>,
    pub reply_to_seq: Option<i64>,
    pub thread_root_seq: Option<i64>,
    pub blind_deadline_at_ms: Option<i64>,
    pub blind_revealed_at_ms: Option<i64>,
    pub task_status: Option<String>,
    pub chain_id: Option<String>,
    pub chain_depth: u32,
    pub auto_bridged: bool,
    pub pinned: bool,
    pub idempotency_key: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct NewGroupMention {
    pub target_member_id: String,
    pub mention_kind: String,
    pub delivery_status: String,
    pub delivery_id: Option<String>,
    pub status_reason: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default)]
pub struct GroupMessageQuery {
    pub after_seq: Option<i64>,
    pub before_seq: Option<i64>,
    pub thread_root_seq: Option<i64>,
    pub limit: usize,
    /// `Some(member_id)` 时强制过滤其他成员未揭晓的 `blind_submission`；`None` 表示人类视角可见全部。
    pub viewer_member_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct GroupUpdatePatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub workset_id: Option<Option<String>>,
    pub mode: Option<String>,
    pub coordinator_member_id: Option<Option<String>>,
    pub allow_worker_cross_talk: Option<bool>,
    pub max_chain_hops: Option<u32>,
    pub max_chain_depth: Option<u32>,
    pub max_pair_ping_pong: Option<u32>,
    pub auto_bridge_mentions: Option<bool>,
    pub paused: Option<bool>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default)]
pub struct GroupMemberUpdatePatch {
    pub alias: Option<String>,
    pub display_name: Option<String>,
    pub role_kind: Option<String>,
    pub role_title: Option<String>,
    pub role_prompt: Option<String>,
    pub workspace_mode: Option<String>,
    pub tool_restriction_profile: Option<String>,
    pub muted: Option<bool>,
}

#[derive(Clone)]
pub struct GroupStore {
    path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for GroupStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl GroupStore {
    /// 打开已有群组库；文件缺失或仅有孤儿 sidecar 时拒绝建空库。
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        match database_presence(&path) {
            DatabasePresence::Absent => return Err(missing_database_error(&path)),
            DatabasePresence::OrphanSidecars => return Err(orphan_sidecar_error(&path)),
            DatabasePresence::Ready => {}
        }
        open_connection(&path, false)
    }

    /// 已有则打开，主文件与 sidecar 都不在才新建；orphan sidecar 拒绝建空库。
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let create = match database_presence(&path) {
            DatabasePresence::Absent => true,
            DatabasePresence::OrphanSidecars => return Err(orphan_sidecar_error(&path)),
            DatabasePresence::Ready => false,
        };
        open_connection(&path, create)
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| StoreError::ConnectionPoisoned {
                path: self.path.clone(),
            })?;
        f(&mut guard)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn checkpoint_wal_truncate(&self) -> Result<(), StoreError> {
        self.with_conn(|conn| {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(StoreError::from)
        })
    }

    // ---- Group CRUD ----

    pub fn create_group(&self, group: &GroupRecord) -> Result<(), StoreError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agent_group (
                    id, title, description, workset_id, mode, coordinator_member_id,
                    allow_worker_cross_talk, max_chain_hops, max_chain_depth, max_pair_ping_pong,
                    auto_bridge_mentions, paused, next_seq, archived_at_ms, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    group.id,
                    group.title,
                    group.description,
                    group.workset_id,
                    group.mode,
                    group.coordinator_member_id,
                    i64::from(group.allow_worker_cross_talk),
                    i64::from(group.max_chain_hops),
                    i64::from(group.max_chain_depth),
                    i64::from(group.max_pair_ping_pong),
                    i64::from(group.auto_bridge_mentions),
                    i64::from(group.paused),
                    group.next_seq,
                    group.archived_at_ms,
                    group.created_at_ms,
                    group.updated_at_ms,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_group(&self, group_id: &str) -> Result<Option<GroupRecord>, StoreError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, title, description, workset_id, mode, coordinator_member_id,
                        allow_worker_cross_talk, max_chain_hops, max_chain_depth, max_pair_ping_pong,
                        auto_bridge_mentions, paused, next_seq, archived_at_ms, created_at_ms, updated_at_ms
                 FROM agent_group WHERE id = ?1",
                params![group_id],
                row_to_group,
            )
            .optional()
            .map_err(StoreError::from)
        })
    }

    pub fn list_groups(&self, include_archived: bool) -> Result<Vec<GroupRecord>, StoreError> {
        self.with_conn(|conn| {
            let sql = if include_archived {
                "SELECT id, title, description, workset_id, mode, coordinator_member_id,
                        allow_worker_cross_talk, max_chain_hops, max_chain_depth, max_pair_ping_pong,
                        auto_bridge_mentions, paused, next_seq, archived_at_ms, created_at_ms, updated_at_ms
                 FROM agent_group ORDER BY updated_at_ms DESC, created_at_ms DESC"
            } else {
                "SELECT id, title, description, workset_id, mode, coordinator_member_id,
                        allow_worker_cross_talk, max_chain_hops, max_chain_depth, max_pair_ping_pong,
                        auto_bridge_mentions, paused, next_seq, archived_at_ms, created_at_ms, updated_at_ms
                 FROM agent_group WHERE archived_at_ms IS NULL ORDER BY updated_at_ms DESC, created_at_ms DESC"
            };
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([], row_to_group)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn update_group(
        &self,
        group_id: &str,
        patch: &GroupUpdatePatch,
    ) -> Result<Option<GroupRecord>, StoreError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let Some(mut group) = tx
                .query_row(
                    "SELECT id, title, description, workset_id, mode, coordinator_member_id,
                            allow_worker_cross_talk, max_chain_hops, max_chain_depth, max_pair_ping_pong,
                            auto_bridge_mentions, paused, next_seq, archived_at_ms, created_at_ms, updated_at_ms
                     FROM agent_group WHERE id = ?1",
                    params![group_id],
                    row_to_group,
                )
                .optional()?
            else {
                return Ok(None);
            };
            if let Some(title) = &patch.title {
                group.title = title.clone();
            }
            if let Some(description) = &patch.description {
                group.description = description.clone();
            }
            if let Some(workset_id) = &patch.workset_id {
                group.workset_id = workset_id.clone();
            }
            if let Some(mode) = &patch.mode {
                group.mode = mode.clone();
            }
            if let Some(coord) = &patch.coordinator_member_id {
                group.coordinator_member_id = coord.clone();
            }
            if let Some(v) = patch.allow_worker_cross_talk {
                group.allow_worker_cross_talk = v;
            }
            if let Some(v) = patch.max_chain_hops {
                group.max_chain_hops = v;
            }
            if let Some(v) = patch.max_chain_depth {
                group.max_chain_depth = v;
            }
            if let Some(v) = patch.max_pair_ping_pong {
                group.max_pair_ping_pong = v;
            }
            if let Some(v) = patch.auto_bridge_mentions {
                group.auto_bridge_mentions = v;
            }
            if let Some(v) = patch.paused {
                group.paused = v;
            }
            group.updated_at_ms = patch.updated_at_ms;
            tx.execute(
                "UPDATE agent_group
                 SET title = ?2, description = ?3, workset_id = ?4, mode = ?5,
                     coordinator_member_id = ?6, allow_worker_cross_talk = ?7,
                     max_chain_hops = ?8, max_chain_depth = ?9, max_pair_ping_pong = ?10,
                     auto_bridge_mentions = ?11, paused = ?12, updated_at_ms = ?13
                 WHERE id = ?1",
                params![
                    group.id,
                    group.title,
                    group.description,
                    group.workset_id,
                    group.mode,
                    group.coordinator_member_id,
                    i64::from(group.allow_worker_cross_talk),
                    i64::from(group.max_chain_hops),
                    i64::from(group.max_chain_depth),
                    i64::from(group.max_pair_ping_pong),
                    i64::from(group.auto_bridge_mentions),
                    i64::from(group.paused),
                    group.updated_at_ms,
                ],
            )?;
            tx.commit()?;
            Ok(Some(group))
        })
    }

    pub fn set_group_archived(
        &self,
        group_id: &str,
        archived_at_ms: Option<i64>,
        updated_at_ms: i64,
    ) -> Result<bool, StoreError> {
        self.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE agent_group SET archived_at_ms = ?2, updated_at_ms = ?3 WHERE id = ?1",
                params![group_id, archived_at_ms, updated_at_ms],
            )?;
            Ok(changed > 0)
        })
    }

    pub fn delete_group(&self, group_id: &str) -> Result<bool, StoreError> {
        self.with_conn(|conn| {
            let changed =
                conn.execute("DELETE FROM agent_group WHERE id = ?1", params![group_id])?;
            Ok(changed > 0)
        })
    }

    // ---- Member CRUD ----

    pub fn add_member(&self, member: &GroupMemberRecord) -> Result<(), StoreError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agent_group_member (
                    id, group_id, alias, display_name, ownership, runtime_id, session_id,
                    role_kind, role_title, role_prompt, workspace_mode, tool_restriction_profile,
                    briefing_sent, last_delivered_seq, last_read_seq, muted, removed_at_ms, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                params![
                    member.id,
                    member.group_id,
                    member.alias,
                    member.display_name,
                    member.ownership,
                    member.runtime_id,
                    member.session_id,
                    member.role_kind,
                    member.role_title,
                    member.role_prompt,
                    member.workspace_mode,
                    member.tool_restriction_profile,
                    i64::from(member.briefing_sent),
                    member.last_delivered_seq,
                    member.last_read_seq,
                    i64::from(member.muted),
                    member.removed_at_ms,
                    member.created_at_ms,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_member(&self, member_id: &str) -> Result<Option<GroupMemberRecord>, StoreError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, group_id, alias, display_name, ownership, runtime_id, session_id,
                        role_kind, role_title, role_prompt, workspace_mode, tool_restriction_profile,
                        briefing_sent, last_delivered_seq, last_read_seq, muted, removed_at_ms, created_at_ms
                 FROM agent_group_member WHERE id = ?1",
                params![member_id],
                row_to_member,
            )
            .optional()
            .map_err(StoreError::from)
        })
    }

    pub fn list_members(
        &self,
        group_id: &str,
        include_removed: bool,
    ) -> Result<Vec<GroupMemberRecord>, StoreError> {
        self.with_conn(|conn| {
            let sql = if include_removed {
                "SELECT id, group_id, alias, display_name, ownership, runtime_id, session_id,
                        role_kind, role_title, role_prompt, workspace_mode, tool_restriction_profile,
                        briefing_sent, last_delivered_seq, last_read_seq, muted, removed_at_ms, created_at_ms
                 FROM agent_group_member WHERE group_id = ?1 ORDER BY created_at_ms ASC, id ASC"
            } else {
                "SELECT id, group_id, alias, display_name, ownership, runtime_id, session_id,
                        role_kind, role_title, role_prompt, workspace_mode, tool_restriction_profile,
                        briefing_sent, last_delivered_seq, last_read_seq, muted, removed_at_ms, created_at_ms
                 FROM agent_group_member WHERE group_id = ?1 AND removed_at_ms IS NULL ORDER BY created_at_ms ASC, id ASC"
            };
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params![group_id], row_to_member)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// 反查某个活跃会话归属的所有未移除且未归档群成员（供 MCP 动态工具暴露和来源认证使用）。
    pub fn find_active_members_by_session(
        &self,
        runtime_id: Option<&str>,
        session_id: &str,
    ) -> Result<Vec<GroupMemberRecord>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.group_id, m.alias, m.display_name, m.ownership, m.runtime_id, m.session_id,
                        m.role_kind, m.role_title, m.role_prompt, m.workspace_mode, m.tool_restriction_profile,
                        m.briefing_sent, m.last_delivered_seq, m.last_read_seq, m.muted, m.removed_at_ms, m.created_at_ms
                 FROM agent_group_member m
                 JOIN agent_group g ON g.id = m.group_id
                 WHERE m.session_id = ?1
                   AND ((?2 IS NULL AND m.runtime_id IS NULL) OR m.runtime_id = ?2)
                   AND m.removed_at_ms IS NULL
                   AND g.archived_at_ms IS NULL
                 ORDER BY m.created_at_ms ASC",
            )?;
            let rows = stmt.query_map(params![session_id, runtime_id], row_to_member)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn update_member(
        &self,
        member_id: &str,
        patch: &GroupMemberUpdatePatch,
    ) -> Result<Option<GroupMemberRecord>, StoreError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let Some(mut member) = tx
                .query_row(
                    "SELECT id, group_id, alias, display_name, ownership, runtime_id, session_id,
                            role_kind, role_title, role_prompt, workspace_mode, tool_restriction_profile,
                            briefing_sent, last_delivered_seq, last_read_seq, muted, removed_at_ms, created_at_ms
                     FROM agent_group_member WHERE id = ?1",
                    params![member_id],
                    row_to_member,
                )
                .optional()?
            else {
                return Ok(None);
            };
            if let Some(v) = &patch.alias {
                member.alias = v.clone();
            }
            if let Some(v) = &patch.display_name {
                member.display_name = v.clone();
            }
            if let Some(v) = &patch.role_kind {
                member.role_kind = v.clone();
            }
            if let Some(v) = &patch.role_title {
                member.role_title = v.clone();
            }
            if let Some(v) = &patch.role_prompt {
                member.role_prompt = v.clone();
            }
            if let Some(v) = &patch.workspace_mode {
                member.workspace_mode = v.clone();
            }
            if let Some(v) = &patch.tool_restriction_profile {
                member.tool_restriction_profile = v.clone();
            }
            if let Some(v) = patch.muted {
                member.muted = v;
            }
            tx.execute(
                "UPDATE agent_group_member
                 SET alias = ?2, display_name = ?3, role_kind = ?4, role_title = ?5,
                     role_prompt = ?6, workspace_mode = ?7, tool_restriction_profile = ?8, muted = ?9
                 WHERE id = ?1",
                params![
                    member.id,
                    member.alias,
                    member.display_name,
                    member.role_kind,
                    member.role_title,
                    member.role_prompt,
                    member.workspace_mode,
                    member.tool_restriction_profile,
                    i64::from(member.muted),
                ],
            )?;
            tx.commit()?;
            Ok(Some(member))
        })
    }

    pub fn remove_member(&self, member_id: &str, removed_at_ms: i64) -> Result<bool, StoreError> {
        self.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE agent_group_member SET removed_at_ms = ?2 WHERE id = ?1 AND removed_at_ms IS NULL",
                params![member_id, removed_at_ms],
            )?;
            Ok(changed > 0)
        })
    }

    pub fn advance_member_cursors(
        &self,
        member_id: &str,
        delivered_seq: Option<i64>,
        read_seq: Option<i64>,
        mark_briefing_sent: bool,
    ) -> Result<(), StoreError> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE agent_group_member
                 SET last_delivered_seq = CASE WHEN ?2 IS NOT NULL AND ?2 > last_delivered_seq THEN ?2 ELSE last_delivered_seq END,
                     last_read_seq = CASE WHEN ?3 IS NOT NULL AND ?3 > last_read_seq THEN ?3 ELSE last_read_seq END,
                     briefing_sent = CASE WHEN ?4 = 1 THEN 1 ELSE briefing_sent END
                 WHERE id = ?1",
                params![member_id, delivered_seq, read_seq, i64::from(mark_briefing_sent)],
            )?;
            Ok(())
        })
    }

    // ---- Messages & Mentions (Commit-before-Publish 原子提交) ----

    /// 在单个事务里幂等检查 `idempotency_key`、分配 `seq`、写入消息与全部 mention 行。
    /// 返回 `(message, mentions, is_replay)`。
    pub fn append_message_with_mentions(
        &self,
        msg: &NewGroupMessage,
        mentions: &[NewGroupMention],
    ) -> Result<(GroupMessageRecord, Vec<GroupMentionRecord>, bool), StoreError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            if let Some(key) = msg.idempotency_key.as_deref()
                && let Some(existing) = tx
                    .query_row(
                        "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                                attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                                blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                                pinned, idempotency_key, created_at_ms
                         FROM agent_group_message
                         WHERE group_id = ?1 AND idempotency_key = ?2",
                        params![msg.group_id, key],
                        row_to_message,
                    )
                    .optional()?
            {
                let existing_mentions = query_mentions_in_tx(&tx, &existing.id)?;
                return Ok((existing, existing_mentions, true));
            }

            let seq: i64 = tx.query_row(
                "SELECT next_seq FROM agent_group WHERE id = ?1",
                params![msg.group_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE agent_group SET next_seq = ?2, updated_at_ms = ?3 WHERE id = ?1",
                params![msg.group_id, seq + 1, msg.created_at_ms],
            )?;
            tx.execute(
                "INSERT INTO agent_group_message (
                    id, group_id, seq, sender_type, sender_member_id, kind, content,
                    attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                    blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                    pinned, idempotency_key, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![
                    msg.id,
                    msg.group_id,
                    seq,
                    msg.sender_type,
                    msg.sender_member_id,
                    msg.kind,
                    msg.content,
                    msg.attachments_json,
                    msg.reply_to_seq,
                    msg.thread_root_seq,
                    msg.blind_deadline_at_ms,
                    msg.blind_revealed_at_ms,
                    msg.task_status,
                    msg.chain_id,
                    i64::from(msg.chain_depth),
                    i64::from(msg.auto_bridged),
                    i64::from(msg.pinned),
                    msg.idempotency_key,
                    msg.created_at_ms,
                ],
            )?;

            let mut stored_mentions = Vec::with_capacity(mentions.len());
            for mention in mentions {
                tx.execute(
                    "INSERT INTO agent_group_message_mention (
                        message_id, group_id, target_member_id, mention_kind,
                        delivery_status, delivery_id, status_reason, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        msg.id,
                        msg.group_id,
                        mention.target_member_id,
                        mention.mention_kind,
                        mention.delivery_status,
                        mention.delivery_id,
                        mention.status_reason,
                        mention.updated_at_ms,
                    ],
                )?;
                stored_mentions.push(GroupMentionRecord {
                    message_id: msg.id.clone(),
                    group_id: msg.group_id.clone(),
                    target_member_id: mention.target_member_id.clone(),
                    mention_kind: mention.mention_kind.clone(),
                    delivery_status: mention.delivery_status.clone(),
                    delivery_id: mention.delivery_id.clone(),
                    status_reason: mention.status_reason.clone(),
                    updated_at_ms: mention.updated_at_ms,
                });
            }

            tx.commit()?;
            let stored = GroupMessageRecord {
                id: msg.id.clone(),
                group_id: msg.group_id.clone(),
                seq,
                sender_type: msg.sender_type.clone(),
                sender_member_id: msg.sender_member_id.clone(),
                kind: msg.kind.clone(),
                content: msg.content.clone(),
                attachments_json: msg.attachments_json.clone(),
                reply_to_seq: msg.reply_to_seq,
                thread_root_seq: msg.thread_root_seq,
                blind_deadline_at_ms: msg.blind_deadline_at_ms,
                blind_revealed_at_ms: msg.blind_revealed_at_ms,
                task_status: msg.task_status.clone(),
                chain_id: msg.chain_id.clone(),
                chain_depth: msg.chain_depth,
                auto_bridged: msg.auto_bridged,
                pinned: msg.pinned,
                idempotency_key: msg.idempotency_key.clone(),
                created_at_ms: msg.created_at_ms,
            };
            Ok((stored, stored_mentions, false))
        })
    }

    pub fn get_message_by_seq(
        &self,
        group_id: &str,
        seq: i64,
    ) -> Result<Option<GroupMessageRecord>, StoreError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                        attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                        blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                        pinned, idempotency_key, created_at_ms
                 FROM agent_group_message WHERE group_id = ?1 AND seq = ?2",
                params![group_id, seq],
                row_to_message,
            )
            .optional()
            .map_err(StoreError::from)
        })
    }

    pub fn list_messages(
        &self,
        group_id: &str,
        query: &GroupMessageQuery,
    ) -> Result<(Vec<GroupMessageRecord>, bool), StoreError> {
        self.with_conn(|conn| {
            let limit = query.limit.clamp(1, 200);
            let fetch_limit = (limit + 1) as i64;
            let mut stmt = conn.prepare(
                "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                        attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                        blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                        pinned, idempotency_key, created_at_ms
                 FROM agent_group_message
                 WHERE group_id = ?1
                   AND (?2 IS NULL OR seq > ?2)
                   AND (?3 IS NULL OR seq < ?3)
                   AND (?4 IS NULL OR thread_root_seq = ?4)
                   AND (?5 IS NULL OR kind != 'blind_submission' OR blind_revealed_at_ms IS NOT NULL OR sender_member_id = ?5)
                 ORDER BY seq ASC
                 LIMIT ?6",
            )?;
            let rows = stmt.query_map(
                params![
                    group_id,
                    query.after_seq,
                    query.before_seq,
                    query.thread_root_seq,
                    query.viewer_member_id,
                    fetch_limit,
                ],
                row_to_message,
            )?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            let has_more = items.len() > limit;
            if has_more {
                items.truncate(limit);
            }
            Ok((items, has_more))
        })
    }

    pub fn list_mentions_for_message(
        &self,
        message_id: &str,
    ) -> Result<Vec<GroupMentionRecord>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT message_id, group_id, target_member_id, mention_kind,
                        delivery_status, delivery_id, status_reason, updated_at_ms
                 FROM agent_group_message_mention
                 WHERE message_id = ?1
                 ORDER BY target_member_id ASC",
            )?;
            let rows = stmt.query_map(params![message_id], row_to_mention)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn update_mention_status(
        &self,
        message_id: &str,
        target_member_id: &str,
        delivery_status: &str,
        delivery_id: Option<&str>,
        status_reason: Option<&str>,
        updated_at_ms: i64,
    ) -> Result<bool, StoreError> {
        self.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE agent_group_message_mention
                 SET delivery_status = ?3,
                     delivery_id = COALESCE(?4, delivery_id),
                     status_reason = ?5,
                     updated_at_ms = ?6
                 WHERE message_id = ?1 AND target_member_id = ?2",
                params![
                    message_id,
                    target_member_id,
                    delivery_status,
                    delivery_id,
                    status_reason,
                    updated_at_ms,
                ],
            )?;
            Ok(changed > 0)
        })
    }

    /// 启动自愈 `resume()` 扫描：列出所有未终结投递（`queued` / `offline_pending` / `pending_approval`）。
    pub fn list_pending_mentions(
        &self,
        group_id: Option<&str>,
    ) -> Result<Vec<GroupMentionRecord>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.message_id, m.group_id, m.target_member_id, m.mention_kind,
                        m.delivery_status, m.delivery_id, m.status_reason, m.updated_at_ms
                 FROM agent_group_message_mention m
                 JOIN agent_group g ON g.id = m.group_id
                 WHERE m.delivery_status IN ('queued', 'offline_pending', 'pending_approval')
                   AND g.archived_at_ms IS NULL
                   AND (?1 IS NULL OR m.group_id = ?1)
                 ORDER BY m.updated_at_ms ASC",
            )?;
            let rows = stmt.query_map(params![group_id], row_to_mention)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// 原子揭晓盲评轮次：把 `blind_prompt` 和它全部 `blind_submission` 设为已揭晓并返回所有答卷。
    pub fn reveal_blind_prompt(
        &self,
        group_id: &str,
        prompt_seq: i64,
        revealed_at_ms: i64,
    ) -> Result<Vec<GroupMessageRecord>, StoreError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE agent_group_message
                 SET blind_revealed_at_ms = COALESCE(blind_revealed_at_ms, ?3)
                 WHERE group_id = ?1 AND seq = ?2 AND kind = 'blind_prompt'",
                params![group_id, prompt_seq, revealed_at_ms],
            )?;
            tx.execute(
                "UPDATE agent_group_message
                 SET blind_revealed_at_ms = COALESCE(blind_revealed_at_ms, ?3)
                 WHERE group_id = ?1 AND reply_to_seq = ?2 AND kind = 'blind_submission'",
                params![group_id, prompt_seq, revealed_at_ms],
            )?;
            let submissions = {
                let mut stmt = tx.prepare(
                    "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                            attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                            blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                            pinned, idempotency_key, created_at_ms
                     FROM agent_group_message
                     WHERE group_id = ?1 AND reply_to_seq = ?2 AND kind = 'blind_submission'
                     ORDER BY seq ASC",
                )?;
                let rows = stmt.query_map(params![group_id, prompt_seq], row_to_message)?;
                let mut items = Vec::new();
                for row in rows {
                    items.push(row?);
                }
                items
            };
            tx.commit()?;
            Ok(submissions)
        })
    }

    pub fn update_task_status(
        &self,
        group_id: &str,
        seq: i64,
        task_status: &str,
    ) -> Result<Option<GroupMessageRecord>, StoreError> {
        self.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE agent_group_message
                 SET task_status = ?3
                 WHERE group_id = ?1 AND seq = ?2 AND kind = 'task'",
                params![group_id, seq, task_status],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            conn.query_row(
                "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                        attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                        blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                        pinned, idempotency_key, created_at_ms
                 FROM agent_group_message WHERE group_id = ?1 AND seq = ?2",
                params![group_id, seq],
                row_to_message,
            )
            .optional()
            .map_err(StoreError::from)
        })
    }

    pub fn list_open_tasks(&self, group_id: &str) -> Result<Vec<GroupMessageRecord>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                        attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                        blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                        pinned, idempotency_key, created_at_ms
                 FROM agent_group_message
                 WHERE group_id = ?1 AND kind = 'task'
                   AND (task_status IS NULL OR task_status != 'done')
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![group_id], row_to_message)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// 取群内最新一条消息（用于侧栏预览与按最后活跃时间排序）。
    pub fn latest_message(&self, group_id: &str) -> Result<Option<GroupMessageRecord>, StoreError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                        attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                        blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                        pinned, idempotency_key, created_at_ms
                 FROM agent_group_message
                 WHERE group_id = ?1
                 ORDER BY seq DESC
                 LIMIT 1",
                params![group_id],
                row_to_message,
            )
            .optional()
            .map_err(StoreError::from)
        })
    }

    /// 置顶或取消置顶群内某条消息。
    pub fn set_message_pinned(
        &self,
        group_id: &str,
        seq: i64,
        pinned: bool,
    ) -> Result<Option<GroupMessageRecord>, StoreError> {
        self.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE agent_group_message
                 SET pinned = ?3
                 WHERE group_id = ?1 AND seq = ?2",
                params![group_id, seq, i64::from(pinned)],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            conn.query_row(
                "SELECT id, group_id, seq, sender_type, sender_member_id, kind, content,
                        attachments_json, reply_to_seq, thread_root_seq, blind_deadline_at_ms,
                        blind_revealed_at_ms, task_status, chain_id, chain_depth, auto_bridged,
                        pinned, idempotency_key, created_at_ms
                 FROM agent_group_message WHERE group_id = ?1 AND seq = ?2",
                params![group_id, seq],
                row_to_message,
            )
            .optional()
            .map_err(StoreError::from)
        })
    }

    /// 列出本机所有属于未归档群组的 `group_owned` 成员会话 ID，供 `session-list` 隐藏群私有会话。
    pub fn list_group_owned_session_ids(&self) -> Result<Vec<String>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT m.session_id
                 FROM agent_group_member m
                 JOIN agent_group g ON g.id = m.group_id
                 WHERE m.ownership = 'group_owned'
                   AND m.runtime_id IS NULL
                   AND m.removed_at_ms IS NULL
                   AND g.archived_at_ms IS NULL
                 ORDER BY m.session_id ASC",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// 按 `artifact_id` 在群内最近消息附件里查找其来源 `runtime_id`（用于跨机拉取已发布产物）。
    pub fn find_attachment_runtime_id(
        &self,
        group_id: &str,
        artifact_id: &str,
    ) -> Result<Option<String>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT attachments_json
                 FROM agent_group_message
                 WHERE group_id = ?1 AND length(attachments_json) > 2
                 ORDER BY seq DESC
                 LIMIT 200",
            )?;
            let rows = stmt.query_map(params![group_id], |row| row.get::<_, Vec<u8>>(0))?;
            for row in rows {
                let bytes = row?;
                if let Ok(items) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes) {
                    for item in items {
                        if item.get("id").and_then(serde_json::Value::as_str) == Some(artifact_id) {
                            return Ok(item
                                .get("runtime_id")
                                .and_then(serde_json::Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(ToOwned::to_owned));
                        }
                    }
                }
            }
            Ok(None)
        })
    }

    // ---- Remote Zero-Trust Member Grants ----

    pub fn upsert_remote_grant(
        &self,
        grant: &RemoteGroupMemberGrantRecord,
    ) -> Result<(), StoreError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO remote_group_member_grant (
                    host_runtime_id, group_id, member_id, local_session_id,
                    allow_remote_full_access, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(host_runtime_id, group_id, member_id) DO UPDATE SET
                    local_session_id = excluded.local_session_id,
                    allow_remote_full_access = excluded.allow_remote_full_access",
                params![
                    grant.host_runtime_id,
                    grant.group_id,
                    grant.member_id,
                    grant.local_session_id,
                    i64::from(grant.allow_remote_full_access),
                    grant.created_at_ms,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_remote_grant(
        &self,
        host_runtime_id: &str,
        group_id: &str,
        member_id: &str,
    ) -> Result<Option<RemoteGroupMemberGrantRecord>, StoreError> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT host_runtime_id, group_id, member_id, local_session_id,
                        allow_remote_full_access, created_at_ms
                 FROM remote_group_member_grant
                 WHERE host_runtime_id = ?1 AND group_id = ?2 AND member_id = ?3",
                params![host_runtime_id, group_id, member_id],
                row_to_remote_grant,
            )
            .optional()
            .map_err(StoreError::from)
        })
    }

    pub fn list_remote_grants(
        &self,
        host_runtime_id: Option<&str>,
    ) -> Result<Vec<RemoteGroupMemberGrantRecord>, StoreError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT host_runtime_id, group_id, member_id, local_session_id,
                        allow_remote_full_access, created_at_ms
                 FROM remote_group_member_grant
                 WHERE (?1 IS NULL OR host_runtime_id = ?1)
                 ORDER BY created_at_ms ASC",
            )?;
            let rows = stmt.query_map(params![host_runtime_id], row_to_remote_grant)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn revoke_remote_grant(
        &self,
        host_runtime_id: &str,
        group_id: &str,
        member_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_conn(|conn| {
            let changed = conn.execute(
                "DELETE FROM remote_group_member_grant
                 WHERE host_runtime_id = ?1 AND group_id = ?2 AND member_id = ?3",
                params![host_runtime_id, group_id, member_id],
            )?;
            Ok(changed > 0)
        })
    }
}

fn open_connection(path: &Path, create: bool) -> Result<GroupStore, StoreError> {
    if create
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| StoreError::OpenFailed {
            path: path.to_path_buf(),
            source: OpenSource::from(error),
        })?;
    }
    let flags = if create {
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let mut conn =
        Connection::open_with_flags(path, flags).map_err(|e| StoreError::OpenFailed {
            path: path.to_path_buf(),
            source: OpenSource::from(e),
        })?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    let journal: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL;", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::WalNotEnabled { mode });
        }
    }
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 0 {
        if create {
            let tx = conn.transaction()?;
            tx.execute_batch(CREATE_GROUPS_SCHEMA)?;
            tx.pragma_update(None, "user_version", GROUP_STORE_SCHEMA_VERSION)?;
            tx.commit()?;
        } else {
            return Err(StoreError::SchemaUnrecognized { version: 0 });
        }
    } else if version > GROUP_STORE_SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            observed: version,
            supported: GROUP_STORE_SCHEMA_VERSION,
        });
    }
    tighten_permissions(path);
    Ok(GroupStore {
        path: path.to_path_buf(),
        conn: Arc::new(Mutex::new(conn)),
    })
}

#[cfg(unix)]
fn tighten_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", path.to_string_lossy())),
    ] {
        if candidate.exists() {
            let _ = std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path) {}

fn query_mentions_in_tx(
    tx: &rusqlite::Transaction<'_>,
    message_id: &str,
) -> Result<Vec<GroupMentionRecord>, rusqlite::Error> {
    let mut stmt = tx.prepare(
        "SELECT message_id, group_id, target_member_id, mention_kind,
                delivery_status, delivery_id, status_reason, updated_at_ms
         FROM agent_group_message_mention
         WHERE message_id = ?1
         ORDER BY target_member_id ASC",
    )?;
    let rows = stmt.query_map(params![message_id], row_to_mention)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn row_to_group(row: &rusqlite::Row<'_>) -> Result<GroupRecord, rusqlite::Error> {
    Ok(GroupRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        workset_id: row.get(3)?,
        mode: row.get(4)?,
        coordinator_member_id: row.get(5)?,
        allow_worker_cross_talk: row.get::<_, i64>(6)? != 0,
        max_chain_hops: row.get::<_, i64>(7)? as u32,
        max_chain_depth: row.get::<_, i64>(8)? as u32,
        max_pair_ping_pong: row.get::<_, i64>(9)? as u32,
        auto_bridge_mentions: row.get::<_, i64>(10)? != 0,
        paused: row.get::<_, i64>(11)? != 0,
        next_seq: row.get(12)?,
        archived_at_ms: row.get(13)?,
        created_at_ms: row.get(14)?,
        updated_at_ms: row.get(15)?,
    })
}

fn row_to_member(row: &rusqlite::Row<'_>) -> Result<GroupMemberRecord, rusqlite::Error> {
    Ok(GroupMemberRecord {
        id: row.get(0)?,
        group_id: row.get(1)?,
        alias: row.get(2)?,
        display_name: row.get(3)?,
        ownership: row.get(4)?,
        runtime_id: row.get(5)?,
        session_id: row.get(6)?,
        role_kind: row.get(7)?,
        role_title: row.get(8)?,
        role_prompt: row.get(9)?,
        workspace_mode: row.get(10)?,
        tool_restriction_profile: row.get(11)?,
        briefing_sent: row.get::<_, i64>(12)? != 0,
        last_delivered_seq: row.get(13)?,
        last_read_seq: row.get(14)?,
        muted: row.get::<_, i64>(15)? != 0,
        removed_at_ms: row.get(16)?,
        created_at_ms: row.get(17)?,
    })
}

fn row_to_message(row: &rusqlite::Row<'_>) -> Result<GroupMessageRecord, rusqlite::Error> {
    Ok(GroupMessageRecord {
        id: row.get(0)?,
        group_id: row.get(1)?,
        seq: row.get(2)?,
        sender_type: row.get(3)?,
        sender_member_id: row.get(4)?,
        kind: row.get(5)?,
        content: row.get(6)?,
        attachments_json: row.get(7)?,
        reply_to_seq: row.get(8)?,
        thread_root_seq: row.get(9)?,
        blind_deadline_at_ms: row.get(10)?,
        blind_revealed_at_ms: row.get(11)?,
        task_status: row.get(12)?,
        chain_id: row.get(13)?,
        chain_depth: row.get::<_, i64>(14)? as u32,
        auto_bridged: row.get::<_, i64>(15)? != 0,
        pinned: row.get::<_, i64>(16)? != 0,
        idempotency_key: row.get(17)?,
        created_at_ms: row.get(18)?,
    })
}

fn row_to_mention(row: &rusqlite::Row<'_>) -> Result<GroupMentionRecord, rusqlite::Error> {
    Ok(GroupMentionRecord {
        message_id: row.get(0)?,
        group_id: row.get(1)?,
        target_member_id: row.get(2)?,
        mention_kind: row.get(3)?,
        delivery_status: row.get(4)?,
        delivery_id: row.get(5)?,
        status_reason: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn row_to_remote_grant(
    row: &rusqlite::Row<'_>,
) -> Result<RemoteGroupMemberGrantRecord, rusqlite::Error> {
    Ok(RemoteGroupMemberGrantRecord {
        host_runtime_id: row.get(0)?,
        group_id: row.get(1)?,
        member_id: row.get(2)?,
        local_session_id: row.get(3)?,
        allow_remote_full_access: row.get::<_, i64>(4)? != 0,
        created_at_ms: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (PathBuf, GroupStore) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "smelt-group-store-test-{}-{}-{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(GROUPS_DATABASE_FILE_NAME);
        let store = GroupStore::open_or_create(&path).unwrap();
        (path, store)
    }

    fn sample_group(id: &str) -> GroupRecord {
        GroupRecord {
            id: id.to_string(),
            title: "Auth 重构".to_string(),
            description: String::new(),
            workset_id: Some("ws-1".to_string()),
            mode: "mention".to_string(),
            coordinator_member_id: None,
            allow_worker_cross_talk: true,
            max_chain_hops: 10,
            max_chain_depth: 6,
            max_pair_ping_pong: 3,
            auto_bridge_mentions: false,
            paused: false,
            next_seq: 1,
            archived_at_ms: None,
            created_at_ms: 1000,
            updated_at_ms: 1000,
        }
    }

    fn sample_member(id: &str, group_id: &str, alias: &str, session_id: &str) -> GroupMemberRecord {
        GroupMemberRecord {
            id: id.to_string(),
            group_id: group_id.to_string(),
            alias: alias.to_string(),
            display_name: alias.to_string(),
            ownership: "group_owned".to_string(),
            runtime_id: None,
            session_id: session_id.to_string(),
            role_kind: "worker".to_string(),
            role_title: String::new(),
            role_prompt: String::new(),
            workspace_mode: "shared_readonly".to_string(),
            tool_restriction_profile: "default".to_string(),
            briefing_sent: false,
            last_delivered_seq: 0,
            last_read_seq: 0,
            muted: false,
            removed_at_ms: None,
            created_at_ms: 1000,
        }
    }

    #[test]
    fn commit_before_publish_is_atomic_and_idempotent() {
        let (_path, store) = temp_db();
        store.create_group(&sample_group("grp-1")).unwrap();
        store
            .add_member(&sample_member("gm-a", "grp-1", "claude", "acp-1"))
            .unwrap();
        store
            .add_member(&sample_member("gm-b", "grp-1", "codex", "acp-2"))
            .unwrap();

        let msg = NewGroupMessage {
            id: "gmsg-1".to_string(),
            group_id: "grp-1".to_string(),
            sender_type: "user".to_string(),
            sender_member_id: None,
            kind: "task".to_string(),
            content: "@codex 实现 token 刷新".to_string(),
            attachments_json: b"[]".to_vec(),
            reply_to_seq: None,
            thread_root_seq: None,
            blind_deadline_at_ms: None,
            blind_revealed_at_ms: None,
            task_status: Some("open".to_string()),
            chain_id: Some("chain-1".to_string()),
            chain_depth: 0,
            auto_bridged: false,
            pinned: false,
            idempotency_key: Some("req-1".to_string()),
            created_at_ms: 2000,
        };
        let mentions = vec![NewGroupMention {
            target_member_id: "gm-b".to_string(),
            mention_kind: "direct".to_string(),
            delivery_status: "queued".to_string(),
            delivery_id: Some("gd-1".to_string()),
            status_reason: None,
            updated_at_ms: 2000,
        }];

        let (stored, stored_mentions, replay) =
            store.append_message_with_mentions(&msg, &mentions).unwrap();
        assert!(!replay);
        assert_eq!(stored.seq, 1);
        assert_eq!(stored_mentions.len(), 1);
        assert_eq!(store.get_group("grp-1").unwrap().unwrap().next_seq, 2);

        // 幂等重放不会分配新 seq
        let (again, again_mentions, replay2) =
            store.append_message_with_mentions(&msg, &mentions).unwrap();
        assert!(replay2);
        assert_eq!(again.seq, 1);
        assert_eq!(again_mentions.len(), 1);
        assert_eq!(store.get_group("grp-1").unwrap().unwrap().next_seq, 2);

        // 启动自愈扫描可见 queued mention
        let pending = store.list_pending_mentions(Some("grp-1")).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].delivery_status, "queued");

        // 任务状态更新与查询
        assert_eq!(store.list_open_tasks("grp-1").unwrap().len(), 1);
        store.update_task_status("grp-1", 1, "done").unwrap();
        assert!(store.list_open_tasks("grp-1").unwrap().is_empty());
    }

    #[test]
    fn blind_submissions_are_isolated_until_atomic_reveal() {
        let (_path, store) = temp_db();
        store.create_group(&sample_group("grp-blind")).unwrap();
        store
            .add_member(&sample_member("gm-claude", "grp-blind", "claude", "acp-c"))
            .unwrap();
        store
            .add_member(&sample_member("gm-codex", "grp-blind", "codex", "acp-x"))
            .unwrap();

        let prompt = NewGroupMessage {
            id: "gmsg-p".to_string(),
            group_id: "grp-blind".to_string(),
            sender_type: "user".to_string(),
            sender_member_id: None,
            kind: "blind_prompt".to_string(),
            content: "评审鉴权设计".to_string(),
            attachments_json: b"[]".to_vec(),
            reply_to_seq: None,
            thread_root_seq: None,
            blind_deadline_at_ms: Some(9000),
            blind_revealed_at_ms: None,
            task_status: None,
            chain_id: None,
            chain_depth: 0,
            auto_bridged: false,
            pinned: false,
            idempotency_key: None,
            created_at_ms: 3000,
        };
        let (prompt_rec, _, _) = store.append_message_with_mentions(&prompt, &[]).unwrap();

        // claude 先交卷，尚未揭晓
        let sub = NewGroupMessage {
            id: "gmsg-s1".to_string(),
            group_id: "grp-blind".to_string(),
            sender_type: "member".to_string(),
            sender_member_id: Some("gm-claude".to_string()),
            kind: "blind_submission".to_string(),
            content: "Claude 独立盲评结论".to_string(),
            attachments_json: b"[]".to_vec(),
            reply_to_seq: Some(prompt_rec.seq),
            thread_root_seq: None,
            blind_deadline_at_ms: None,
            blind_revealed_at_ms: None,
            task_status: None,
            chain_id: None,
            chain_depth: 0,
            auto_bridged: false,
            pinned: false,
            idempotency_key: None,
            created_at_ms: 3100,
        };
        store.append_message_with_mentions(&sub, &[]).unwrap();

        // codex 视角绝对看不到 claude 的未揭晓盲评
        let (codex_view, _) = store
            .list_messages(
                "grp-blind",
                &GroupMessageQuery {
                    limit: 50,
                    viewer_member_id: Some("gm-codex".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(codex_view.len(), 1);
        assert_eq!(codex_view[0].kind, "blind_prompt");

        // claude 自己和人类视角都能看到
        let (claude_view, _) = store
            .list_messages(
                "grp-blind",
                &GroupMessageQuery {
                    limit: 50,
                    viewer_member_id: Some("gm-claude".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(claude_view.len(), 2);

        // 原子揭晓后 codex 立刻可见
        let revealed = store
            .reveal_blind_prompt("grp-blind", prompt_rec.seq, 4000)
            .unwrap();
        assert_eq!(revealed.len(), 1);
        assert_eq!(revealed[0].blind_revealed_at_ms, Some(4000));

        let (codex_after, _) = store
            .list_messages(
                "grp-blind",
                &GroupMessageQuery {
                    limit: 50,
                    viewer_member_id: Some("gm-codex".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(codex_after.len(), 2);
    }

    #[test]
    fn remote_grant_whitelist_supports_toggle_and_revoke() {
        let (_path, store) = temp_db();
        let grant = RemoteGroupMemberGrantRecord {
            host_runtime_id: "rt-server".to_string(),
            group_id: "grp-remote".to_string(),
            member_id: "gm-local".to_string(),
            local_session_id: "acp-local-1".to_string(),
            allow_remote_full_access: false,
            created_at_ms: 5000,
        };
        store.upsert_remote_grant(&grant).unwrap();
        let loaded = store
            .get_remote_grant("rt-server", "grp-remote", "gm-local")
            .unwrap()
            .unwrap();
        assert!(!loaded.allow_remote_full_access);

        // 用户中途切换为允许 FullAccess
        store
            .upsert_remote_grant(&RemoteGroupMemberGrantRecord {
                allow_remote_full_access: true,
                ..grant
            })
            .unwrap();
        let toggled = store
            .get_remote_grant("rt-server", "grp-remote", "gm-local")
            .unwrap()
            .unwrap();
        assert!(toggled.allow_remote_full_access);

        assert!(
            store
                .revoke_remote_grant("rt-server", "grp-remote", "gm-local")
                .unwrap()
        );
        assert!(
            store
                .get_remote_grant("rt-server", "grp-remote", "gm-local")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn orphan_sidecar_refuses_to_create_empty_groups_db() {
        let dir = std::env::temp_dir().join(format!(
            "smelt-group-orphan-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(GROUPS_DATABASE_FILE_NAME);
        std::fs::write(format!("{}-wal", path.to_string_lossy()), b"stale").unwrap();
        let err = GroupStore::open_or_create(&path).unwrap_err();
        assert!(matches!(err, StoreError::OrphanSidecars { .. }));
    }

    #[test]
    fn latest_message_pin_group_owned_sessions_and_attachment_origin_work() {
        let (_path, store) = temp_db();
        store.create_group(&sample_group("grp-q")).unwrap();
        let mut owned = sample_member("gm-1", "grp-q", "claude", "acp-owned-1");
        owned.ownership = "group_owned".into();
        let mut linked = sample_member("gm-2", "grp-q", "codex", "acp-linked-1");
        linked.ownership = "linked".into();
        store.add_member(&owned).unwrap();
        store.add_member(&linked).unwrap();

        assert_eq!(
            store.list_group_owned_session_ids().unwrap(),
            vec!["acp-owned-1".to_string()]
        );

        for (i, text) in [(1, "第一条"), (2, "第二条"), (3, "最新一条")] {
            let att = if i == 2 {
                serde_json::to_vec(&serde_json::json!([
                    { "id": "gart-remote-1", "runtime_id": "rt-gpu", "label": "diff.patch" }
                ]))
                .unwrap()
            } else {
                b"[]".to_vec()
            };
            store
                .append_message_with_mentions(
                    &NewGroupMessage {
                        id: format!("gmsg-{i}"),
                        group_id: "grp-q".into(),
                        sender_type: "user".into(),
                        sender_member_id: None,
                        kind: "chat".into(),
                        content: text.into(),
                        attachments_json: att,
                        reply_to_seq: None,
                        thread_root_seq: None,
                        blind_deadline_at_ms: None,
                        blind_revealed_at_ms: None,
                        task_status: None,
                        chain_id: None,
                        chain_depth: 0,
                        auto_bridged: false,
                        pinned: false,
                        idempotency_key: None,
                        created_at_ms: 1000 + i,
                    },
                    &[],
                )
                .unwrap();
        }

        let latest = store.latest_message("grp-q").unwrap().unwrap();
        assert_eq!(latest.seq, 3);
        assert_eq!(latest.content, "最新一条");

        let pinned = store.set_message_pinned("grp-q", 2, true).unwrap().unwrap();
        assert!(pinned.pinned);
        assert_eq!(
            store
                .find_attachment_runtime_id("grp-q", "gart-remote-1")
                .unwrap()
                .as_deref(),
            Some("rt-gpu")
        );
    }
}
