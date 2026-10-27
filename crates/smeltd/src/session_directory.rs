//! 已发布会话身份目录。
//!
//! list / subscribe 只认这里（再叠一层活 runtime 覆盖）。终端 / ACP registry
//! 只表示「现在能不能 attach」，不负责展示用身份。
//!
//! 崩溃后 PTY / ACP 子进程都接管不了，但标题、目录、最后相位还值得留给 GUI。
//! 正常 shutdown 只把还能续接的终端留在持久化文档里（哪家 CLI、哪段还在进行的对话，见
//! `terminal_resume`）。`cli_exited` 的终端 `resume_of` 直接返回 None，跟普通 shell 一样不留。
//! 对话 id 仍留在内存里给桌面用，只是不再进下次启动的续接名单。桌面会用原来的 id
//! 重新打开留下的终端，接回原来的对话。
//! 启动时文档里的条目灌回目录并标 `runtime=false`。
//!
//! 身份只走两个口：`upsert`（`broadcast_state`）和 `remove`（`forget_session`）。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::terminal_resume::{ResumeLaunch, resume_launch};
use super::{Phase, SessionState};

/// 持久化文档格式版本。结构演进时递增，旧版本文档直接丢弃。
pub const SESSION_DIRECTORY_VERSION: u32 = 1;

/// 状态变化落盘的节流间隔：hook 事件很密，每次都写盘没必要，1 秒一次足够。
const PERSIST_INTERVAL: Duration = Duration::from_secs(1);

/// 测试通过 `SMELT_GHOST_SESSIONS_DIR` 重定向到临时目录。
#[cfg(test)]
pub fn session_directory_persisted() -> bool {
    let path = session_directory_path();
    smelt_core::sqlite_state::open_sqlite_store(&path)
        .ok()
        .and_then(|store| store.get_published_session_snapshot().ok())
        .flatten()
        .is_some_and(|snapshot| !snapshot.sessions.is_empty())
}

pub fn session_directory_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("SMELT_GHOST_SESSIONS_DIR") {
        return PathBuf::from(dir).join(smelt_store::DATABASE_FILE_NAME);
    }
    let dir = smelt_paths::smelt_home().unwrap_or_else(|| std::env::temp_dir().join(".smelt"));
    let _ = std::fs::create_dir_all(&dir);
    dir.join(smelt_store::DATABASE_FILE_NAME)
}

/// 落盘条目：SessionState 的显式投影，只含恢复 GUI 展示需要的低频字段。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct SessionRecord {
    pub id: String,
    pub cwd: Option<String>,
    pub launch: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    /// 崩溃前这条会话最后对上的 provider 对话 id。CLI 退出后也留着，桌面继续用它
    /// 对历史标题；能不能续接看 `cli_exited`。
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// CLI 进程已经退出。崩溃恢复时必须带着，否则退出后留下的对话 id 会被当成还能续接。
    #[serde(default)]
    pub cli_exited: bool,
    pub title: Option<String>,
    pub phase: Phase,
    pub phase_since: u64,
    pub updated_at: u64,
    pub structured_events: bool,
    #[serde(default)]
    pub turn_events: bool,
    pub agent_event_version: Option<u32>,
    pub tokens_used: Option<u64>,
    pub branch: Option<String>,
    pub dirty_files: Vec<String>,
}

impl SessionRecord {
    fn from_state(state: &SessionState) -> Self {
        Self {
            id: state.id.clone(),
            cwd: state.cwd.clone(),
            launch: state.launch.clone(),
            provider: state.provider.clone(),
            conversation_id: state.conversation_id.clone(),
            cli_exited: state.cli_exited,
            title: state.title.clone(),
            phase: state.phase,
            phase_since: state.phase_since,
            updated_at: state.updated_at,
            structured_events: state.structured_events,
            turn_events: state.turn_events,
            agent_event_version: state.agent_event_version,
            tokens_used: state.tokens_used,
            branch: state.branch.clone(),
            dirty_files: state.dirty_files.clone(),
        }
    }

    /// 保留最后已知 phase（那是历史，不是当前活动），`runtime=false` 才表示
    /// 没有可 attach 的运行时。pending_question 清空，revision/instance 归零。
    fn into_state(self, now_unix: u64) -> SessionState {
        SessionState {
            id: self.id,
            instance: 0,
            cwd: self.cwd,
            launch: self.launch,
            provider: self.provider,
            conversation_id: self.conversation_id,
            cli_exited: self.cli_exited,
            agent_token: String::new(),
            title: self.title,
            phase: self.phase,
            phase_since: now_unix,
            pending_question: None,
            tokens_used: self.tokens_used,
            branch: self.branch,
            dirty_files: self.dirty_files,
            revision: 0,
            updated_at: now_unix,
            structured_events: self.structured_events,
            turn_events: self.turn_events,
            agent_event_version: self.agent_event_version,
            active_blocker: None,
            runtime: false,
        }
    }
}

fn load_records() -> Vec<SessionRecord> {
    let path = session_directory_path();
    let Ok(store) = smelt_core::sqlite_state::open_sqlite_store(&path) else {
        return Vec::new();
    };
    match store.get_published_session_snapshot() {
        Ok(Some(snapshot)) => snapshot
            .sessions
            .into_iter()
            .map(record_from_published)
            .collect(),
        Ok(None) | Err(_) => Vec::new(),
    }
}

fn persist_records(sessions: &[SessionRecord]) -> bool {
    let path = session_directory_path();
    let Ok(store) = smelt_core::sqlite_state::open_sqlite_store(&path) else {
        return false;
    };
    let snapshot = smelt_store::PublishedSessionSnapshot {
        version: SESSION_DIRECTORY_VERSION,
        sessions: sessions.iter().map(published_from_record).collect(),
    };
    store.put_published_session_snapshot(&snapshot).is_ok()
}

fn published_from_record(record: &SessionRecord) -> smelt_store::PublishedSessionRecord {
    smelt_store::PublishedSessionRecord {
        id: record.id.clone(),
        cwd: record.cwd.clone(),
        launch: record.launch.clone(),
        provider: record.provider.clone(),
        conversation_id: record.conversation_id.clone(),
        cli_exited: record.cli_exited,
        title: record.title.clone(),
        phase: serde_json::to_value(record.phase)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "idle".into()),
        phase_since: i64::try_from(record.phase_since).unwrap_or(0),
        updated_at: i64::try_from(record.updated_at).unwrap_or(0),
        structured_events: record.structured_events,
        turn_events: record.turn_events,
        agent_event_version: record.agent_event_version.map(i64::from),
        tokens_used: record
            .tokens_used
            .and_then(|value| i64::try_from(value).ok()),
        branch: record.branch.clone(),
        dirty_files_json: serde_json::to_vec(&record.dirty_files)
            .unwrap_or_else(|_| b"[]".to_vec()),
    }
}

fn record_from_published(record: smelt_store::PublishedSessionRecord) -> SessionRecord {
    SessionRecord {
        id: record.id,
        cwd: record.cwd,
        launch: record.launch,
        provider: record.provider,
        conversation_id: record.conversation_id,
        cli_exited: record.cli_exited,
        title: record.title,
        phase: serde_json::from_value(serde_json::Value::String(record.phase)).unwrap_or_default(),
        phase_since: record.phase_since.max(0) as u64,
        updated_at: record.updated_at.max(0) as u64,
        structured_events: record.structured_events,
        turn_events: record.turn_events,
        agent_event_version: record
            .agent_event_version
            .and_then(|value| u32::try_from(value).ok()),
        tokens_used: record
            .tokens_used
            .and_then(|value| u64::try_from(value).ok()),
        branch: record.branch,
        dirty_files: serde_json::from_slice(&record.dirty_files_json).unwrap_or_default(),
    }
}

/// 已发布身份：按 id 唯一。活会话和崩溃恢复的断连条目共用这一张表。
pub struct SessionDirectory {
    state: Mutex<DirectoryState>,
    interval: Duration,
}

/// 目录内存状态和它的持久化提交点必须共用同一把锁。否则 A 先复制旧快照、B 删除并
/// 提交后，A 的晚到提交仍会把已删除会话写回持久层。
struct DirectoryState {
    entries: HashMap<String, SessionState>,
    /// 已退休 runtime 实例的最高水位。保留到 daemon 退出，阻止其迟到回调复活条目。
    retired_instances: HashMap<String, u64>,
    last_persist: Option<Instant>,
}

impl Default for SessionDirectory {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionDirectory {
    pub fn new() -> Self {
        Self::with_interval(PERSIST_INTERVAL)
    }

    fn with_interval(interval: Duration) -> Self {
        Self {
            state: Mutex::new(DirectoryState {
                entries: HashMap::new(),
                retired_instances: HashMap::new(),
                last_persist: None,
            }),
            interval,
        }
    }

    /// 从持久化文档恢复。保留最后相位，统一标 `runtime=false`。缺失 / 损坏 → 空表。
    pub fn restore_from_disk() -> Self {
        let now = now_unix();
        let entries = load_records()
            .into_iter()
            .map(|record| (record.id.clone(), record.into_state(now)))
            .collect();
        Self {
            state: Mutex::new(DirectoryState {
                entries,
                retired_instances: HashMap::new(),
                last_persist: None,
            }),
            interval: PERSIST_INTERVAL,
        }
    }

    /// 状态变化：只接受比当前目录新且未退休的实例/修订，超过节流间隔才落盘。
    /// 返回值表示该状态是否进入目录，供上层与订阅总线保持同一提交结果。
    pub fn upsert(&self, incoming: &SessionState) -> bool {
        let mut state = self.state.lock().unwrap();
        if incoming.instance != 0
            && state
                .retired_instances
                .get(&incoming.id)
                .is_some_and(|retired| incoming.instance <= *retired)
        {
            return false;
        }

        let changed = match state.entries.get_mut(&incoming.id) {
            Some(current)
                if incoming.instance != 0
                    && current.instance != 0
                    && incoming.instance < current.instance =>
            {
                false
            }
            Some(current)
                if incoming.instance != 0
                    && current.instance != 0
                    && incoming.instance > current.instance =>
            {
                *current = incoming.clone();
                true
            }
            Some(current) if incoming.revision != 0 && incoming.revision <= current.revision => {
                false
            }
            Some(current) => {
                *current = incoming.clone();
                true
            }
            None => {
                state.entries.insert(incoming.id.clone(), incoming.clone());
                true
            }
        };
        if changed {
            self.maybe_persist_locked(&mut state);
        }
        changed
    }

    /// 退休指定 runtime 实例。较新的实例已经占位时，旧实例只能留下水位，不能删除它。
    /// `instance == 0` 只允许清理同样没有代际信息的历史条目。
    pub fn remove_instance(&self, id: &str, instance: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if instance != 0 {
            let watermark = state.retired_instances.entry(id.to_string()).or_default();
            *watermark = (*watermark).max(instance);
        }
        let blocked_by_live_instance = state.entries.get(id).is_some_and(|current| {
            if instance == 0 {
                current.instance != 0
            } else {
                current.instance != 0 && current.instance > instance
            }
        });
        let removed = if blocked_by_live_instance {
            false
        } else {
            state.entries.remove(id).is_some()
        };
        if removed {
            self.persist_locked(&mut state);
        }
        removed
    }

    #[cfg(test)]
    pub fn contains(&self, id: &str) -> bool {
        self.state.lock().unwrap().entries.contains_key(id)
    }

    pub fn snapshot(&self) -> Vec<SessionState> {
        self.state
            .lock()
            .unwrap()
            .entries
            .values()
            .cloned()
            .collect()
    }

    fn maybe_persist_locked(&self, state: &mut DirectoryState) {
        let now = Instant::now();
        if state
            .last_persist
            .is_some_and(|last| now.duration_since(last) < self.interval)
        {
            return;
        }
        self.persist_locked(state);
    }

    #[cfg(test)]
    pub fn persist_now(&self) {
        let mut state = self.state.lock().unwrap();
        self.persist_locked(&mut state);
    }

    /// 正常退出时，持久化文档里只留还能续接的终端：`terminal_ids` 是停机前还开着的终端，
    /// 再加上上次留下、这次还没被重新打开的。其余（对话、普通 shell、认不出的 CLI）清空。
    /// 写入和所有提交持有同一把状态锁，避免写完后被一个已在飞行中的旧快照覆盖。
    ///
    /// 只改快照，不碰文件本身：`session_directory_path()` 返回的是承载全部用户数据的主库，
    /// 删掉它等于抹掉所有项目与会话组。
    pub(crate) fn keep_resumable_terminals(&self, terminal_ids: &HashSet<String>) {
        let state = self.state.lock().unwrap();
        let sessions = state
            .entries
            .values()
            .filter(|entry| terminal_ids.contains(&entry.id) || !entry.runtime)
            .filter(|entry| resume_of(entry).is_some())
            .map(|entry| published_from_record(&SessionRecord::from_state(entry)))
            .collect();
        let path = session_directory_path();
        if let Ok(store) = smelt_core::sqlite_state::store_beside_json(&path) {
            let _ = store.put_published_session_snapshot(&smelt_store::PublishedSessionSnapshot {
                version: SESSION_DIRECTORY_VERSION,
                sessions,
            });
        }
    }

    /// 重新打开这个终端时怎么接回原来的对话。只看守护停过之后留下的条目
    /// （`runtime=false`）：活着的终端直接 attach，用不到。
    pub(crate) fn resume_launch(&self, id: &str) -> Option<ResumeLaunch> {
        let state = self.state.lock().unwrap();
        state
            .entries
            .get(id)
            .filter(|entry| !entry.runtime)
            .and_then(resume_of)
    }

    fn persist_locked(&self, state: &mut DirectoryState) {
        let sessions = state
            .entries
            .values()
            .map(SessionRecord::from_state)
            .collect::<Vec<_>>();
        if persist_records(&sessions) {
            state.last_persist = Some(Instant::now());
        }
    }
}

fn resume_of(entry: &SessionState) -> Option<ResumeLaunch> {
    if entry.cli_exited {
        return None;
    }
    resume_launch(
        entry.launch.as_deref(),
        entry.provider.as_deref(),
        entry.conversation_id.as_deref(),
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// 测试串行锁：单元测试与 main_tests 集成测试共享落盘路径，并行会互相覆盖。
#[cfg(test)]
pub(crate) static DIRECTORY_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    impl SessionDirectory {
        /// 无条件删除一个身份并立即落盘，返回目录里是否曾有该 id。
        ///
        /// 只服务于本模块的用例，所以定义在这里而不是 prod 区：prod 的正规入口是
        /// `remove_instance`，它带代际水位检查（旧实例不能删掉已占位的新实例）。
        /// 把这个绕过检查的版本摆在 prod 区，容易被后来者当成正规删除入口。
        fn remove(&self, id: &str) -> bool {
            let mut state = self.state.lock().unwrap();
            let removed = state.entries.remove(id).is_some();
            if removed {
                self.persist_locked(&mut state);
            }
            removed
        }
    }

    fn with_test_dir<T>(f: impl FnOnce() -> T) -> T {
        let _guard = DIRECTORY_TEST_LOCK.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("smelt-session-dir-tests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("SMELT_GHOST_SESSIONS_DIR", &dir) };
        let result = f();
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn sample_state(id: &str) -> SessionState {
        SessionState {
            id: id.to_string(),
            instance: 7,
            cwd: Some("/work/project".to_string()),
            launch: Some("claude".to_string()),
            provider: Some("claude".to_string()),
            conversation_id: Some("conv-1".to_string()),
            cli_exited: false,
            agent_token: "secret-token".to_string(),
            title: Some("重构存储层".to_string()),
            phase: Phase::AwaitingApproval,
            phase_since: 100,
            pending_question: Some("要不要继续".to_string()),
            tokens_used: Some(1234),
            branch: Some("feat/ghost".to_string()),
            dirty_files: vec!["a.rs".to_string()],
            revision: 42,
            updated_at: 200,
            structured_events: true,
            turn_events: true,
            agent_event_version: Some(1),
            active_blocker: Some(crate::session_state::AgentBlocker {
                tool_use_id: Some("t1".to_string()),
                agent_id: None,
                tool_name: None,
            }),
            runtime: true,
        }
    }

    #[test]
    fn persist_then_load_roundtrips_low_frequency_fields() {
        with_test_dir(|| {
            let record = SessionRecord::from_state(&sample_state("s1"));
            persist_records(std::slice::from_ref(&record));
            assert_eq!(
                session_directory_path()
                    .file_name()
                    .and_then(|name| name.to_str()),
                Some(smelt_store::DATABASE_FILE_NAME)
            );

            let loaded = load_records();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0], record);
            assert_eq!(loaded[0].phase, Phase::AwaitingApproval);
            assert_eq!(loaded[0].title.as_deref(), Some("重构存储层"));
            assert_eq!(loaded[0].cwd.as_deref(), Some("/work/project"));
            assert_eq!(loaded[0].launch.as_deref(), Some("claude"));
            assert_eq!(loaded[0].provider.as_deref(), Some("claude"));
        });
    }

    #[test]
    fn leftover_json_is_ignored() {
        with_test_dir(|| {
            let json_path = session_directory_path()
                .parent()
                .unwrap()
                .join("sessions.json");
            std::fs::write(&json_path, r#"{"version":1,"sessions":[{"id":"ghost"}]}"#).unwrap();
            assert!(load_records().is_empty());
            assert!(json_path.is_file());
        });
    }

    #[test]
    fn restore_keeps_last_phase_and_marks_runtime_gone() {
        let record = SessionRecord::from_state(&sample_state("s1"));
        let state = record.into_state(999);

        assert_eq!(state.id, "s1");
        assert_eq!(state.phase, Phase::AwaitingApproval);
        assert!(!state.runtime);
        assert_eq!(state.pending_question, None);
        assert_eq!(state.instance, 0);
        assert_eq!(state.revision, 0);
        assert_eq!(state.agent_token, "");
        assert!(state.active_blocker.is_none());
        assert_eq!(state.phase_since, 999);
        assert_eq!(state.updated_at, 999);
        assert_eq!(state.title.as_deref(), Some("重构存储层"));
        assert_eq!(state.cwd.as_deref(), Some("/work/project"));
        assert_eq!(state.launch.as_deref(), Some("claude"));
        assert_eq!(state.provider.as_deref(), Some("claude"));
        assert_eq!(state.tokens_used, Some(1234));
        assert_eq!(state.branch.as_deref(), Some("feat/ghost"));
        assert_eq!(state.dirty_files, vec!["a.rs".to_string()]);
        assert!(state.structured_events);
        assert!(state.turn_events);
        assert_eq!(state.agent_event_version, Some(1));
    }

    #[test]
    fn missing_snapshot_loads_empty() {
        with_test_dir(|| {
            assert!(load_records().is_empty());
        });
    }

    #[test]
    fn restore_from_disk_recovers_disconnected_sessions() {
        with_test_dir(|| {
            persist_records(&[SessionRecord::from_state(&sample_state("s1"))]);

            let directory = SessionDirectory::restore_from_disk();
            let snap = directory.snapshot();
            assert_eq!(snap.len(), 1);
            assert_eq!(snap[0].id, "s1");
            assert!(!snap[0].runtime);
            assert_eq!(snap[0].phase, Phase::AwaitingApproval);
            assert_eq!(snap[0].title.as_deref(), Some("重构存储层"));
            assert!(directory.contains("s1"));
            assert!(!directory.contains("s2"));
        });
    }

    #[test]
    fn upsert_is_visible_immediately_and_remove_persists() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            directory.upsert(&sample_state("s1"));
            directory.upsert(&sample_state("s2"));
            assert_eq!(directory.snapshot().len(), 2);

            assert!(directory.remove("s1"));
            assert!(!directory.contains("s1"));
            assert!(directory.contains("s2"));

            let reloaded = SessionDirectory::restore_from_disk();
            assert_eq!(reloaded.snapshot().len(), 1);
            assert_eq!(reloaded.snapshot()[0].id, "s2");
        });
    }

    fn persisted_titles() -> Vec<Option<String>> {
        let store = smelt_core::sqlite_state::store_beside_json(&session_directory_path()).unwrap();
        store
            .get_published_session_snapshot()
            .unwrap()
            .map(|snapshot| {
                snapshot
                    .sessions
                    .into_iter()
                    .map(|session| session.title)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn upsert_throttles_persist_within_interval() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::from_secs(3600));
            directory.upsert(&sample_state("s1"));
            let first = persisted_titles();
            assert_eq!(first, vec![Some("重构存储层".into())]);

            let mut changed = sample_state("s1");
            changed.revision = 43;
            changed.title = Some("新标题".to_string());
            directory.upsert(&changed);
            assert_eq!(persisted_titles(), first, "节流间隔内不应重复写盘");

            directory.persist_now();
            assert_eq!(persisted_titles(), vec![Some("新标题".into())]);

            let recovered = SessionDirectory::restore_from_disk();
            assert_eq!(recovered.snapshot()[0].title.as_deref(), Some("新标题"));
        });
    }

    #[test]
    fn stale_revision_cannot_replace_newer_directory_entry() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            let mut current = sample_state("s1");
            current.revision = 12;
            current.title = Some("new".to_string());
            assert!(directory.upsert(&current));

            let mut stale = current;
            stale.revision = 11;
            stale.title = Some("old".to_string());
            assert!(!directory.upsert(&stale));

            let snapshot = directory.snapshot();
            assert_eq!(snapshot.len(), 1);
            assert_eq!(snapshot[0].revision, 12);
            assert_eq!(snapshot[0].title.as_deref(), Some("new"));
        });
    }

    #[test]
    fn retired_instance_cannot_resurrect_or_remove_newer_instance() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            let mut old = sample_state("s1");
            old.instance = 7;
            old.revision = 4;
            assert!(directory.upsert(&old));
            assert!(directory.remove_instance("s1", 7));
            assert!(!directory.upsert(&old), "退休实例的迟到状态必须被拒绝");

            let mut newer = old;
            newer.instance = 8;
            newer.revision = 1;
            newer.title = Some("new runtime".to_string());
            assert!(directory.upsert(&newer));
            assert!(
                !directory.remove_instance("s1", 7),
                "旧实例的退出不得删掉新实例"
            );
            let snapshot = directory.snapshot();
            assert_eq!(snapshot.len(), 1);
            assert_eq!(snapshot[0].instance, 8);
            assert_eq!(snapshot[0].title.as_deref(), Some("new runtime"));
        });
    }

    #[test]
    fn instance_less_remove_cannot_remove_a_live_runtime() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            let mut current = sample_state("s1");
            current.instance = 8;
            assert!(directory.upsert(&current));

            assert!(
                !directory.remove_instance("s1", 0),
                "无实例清理不能删除有代际信息的活动条目"
            );
            assert_eq!(directory.snapshot()[0].instance, 8);
        });
    }

    #[test]
    fn clear_empties_the_directory_document() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            directory.upsert(&sample_state("s1"));
            assert!(session_directory_persisted());

            directory.keep_resumable_terminals(&HashSet::new());
            assert!(!session_directory_persisted());
            assert!(SessionDirectory::restore_from_disk().snapshot().is_empty());
        });
    }

    /// 停机只留能续接的终端；重启后它们回到目录里，重新打开时接回原来的对话。
    #[test]
    fn shutdown_keeps_only_resumable_terminals_for_the_next_run() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            // 终端里的 claude，hook 报过对话 id：要留。
            directory.upsert(&sample_state("claude-term"));
            // 普通 shell：没有可续接的。
            let mut shell = sample_state("shell-term");
            shell.launch = None;
            shell.provider = None;
            shell.conversation_id = None;
            directory.upsert(&shell);
            // 同样能拼出续接命令、但不是终端（对话会话由自己的存档续接）：不留。
            directory.upsert(&sample_state("acp-conversation"));

            let terminals: HashSet<String> = ["claude-term", "shell-term"]
                .into_iter()
                .map(String::from)
                .collect();
            directory.keep_resumable_terminals(&terminals);

            let next = SessionDirectory::restore_from_disk();
            let ids: Vec<String> = next.snapshot().into_iter().map(|entry| entry.id).collect();
            assert_eq!(ids, vec!["claude-term".to_string()]);
            assert_eq!(
                next.resume_launch("claude-term").map(|resume| resume.run),
                Some("claude --resume conv-1".to_string())
            );
            assert_eq!(next.resume_launch("shell-term"), None);
        });
    }

    /// CLI 跑完一轮再退出回到 shell：对话 id 还在，但停机不留这个终端，重新打开也不
    /// `--resume`。`launch` 为 None 是在 shell 里手动起的；`Some("claude")` 是用启动命令
    /// 起的，`/exit` 之后终端仍回到交互 shell。启动命令本身不能让它继续可续接。
    #[test]
    fn an_exited_cli_is_not_kept_or_resumed_across_restart() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            let cases = [("manual-claude", None), ("launched-claude", Some("claude"))];
            let mut terminal_ids = HashSet::new();
            for (id, launch) in cases {
                let mut state = exited_cli(id, launch);
                directory.upsert(&state);
                terminal_ids.insert(id.to_string());

                // 活着的 PTY 在停机前 runtime=true，resume_launch 本来就不看它。
                // 这里再放一条 runtime=false 的同样状态，确认不是「还活着」才返回 None。
                state.runtime = false;
                state.id = format!("{id}-ghost");
                directory.upsert(&state);
                assert_eq!(directory.resume_launch(&state.id), None, "{id}");
            }

            // 崩溃不走 keep_resumable，整份快照还在。退出标记必须跟着落盘，
            // 否则恢复出来的对话 id 又会被当成还能续接。
            let crashed = SessionDirectory::restore_from_disk();
            let ghost = crashed
                .snapshot()
                .into_iter()
                .find(|entry| entry.id == "manual-claude-ghost")
                .expect("崩溃恢复应留下退出前的终端");
            assert_eq!(ghost.conversation_id.as_deref(), Some("conv-old"));
            assert!(ghost.cli_exited);
            assert_eq!(crashed.resume_launch("manual-claude-ghost"), None);
            assert_eq!(crashed.resume_launch("launched-claude-ghost"), None);

            directory.keep_resumable_terminals(&terminal_ids);

            let next = SessionDirectory::restore_from_disk();
            let ids: Vec<String> = next.snapshot().into_iter().map(|entry| entry.id).collect();
            assert!(
                ids.is_empty(),
                "已退出的 CLI 不能留到下次启动，实际留下了 {ids:?}"
            );
            for (id, _) in cases {
                assert_eq!(next.resume_launch(id), None, "{id}");
            }
        });
    }

    /// SessionStarted（带 provider 和对话 id）→ TurnSucceeded → SessionEnded。
    /// 对话 id 和标题留着，退出只记在 `cli_exited`。
    fn exited_cli(id: &str, launch: Option<&str>) -> SessionState {
        use smelt_core::agent_event::{AgentEvent, AgentEventKind};

        let mut state = SessionState {
            id: id.to_string(),
            instance: 1,
            launch: launch.map(str::to_string),
            title: Some("这段对话".to_string()),
            runtime: true,
            ..SessionState::default()
        };
        let mut started = AgentEvent::new("claude", AgentEventKind::SessionStarted);
        started.conversation_id = Some("conv-old".into());
        crate::session_state::apply_agent_event(&mut state, &started);

        let mut done = AgentEvent::new("claude", AgentEventKind::TurnSucceeded);
        done.conversation_id = Some("conv-old".into());
        crate::session_state::apply_agent_event(&mut state, &done);
        assert_eq!(state.phase, Phase::Succeeded, "{id}");
        assert_eq!(state.conversation_id.as_deref(), Some("conv-old"), "{id}");

        let mut ended = AgentEvent::new("claude", AgentEventKind::SessionEnded);
        ended.conversation_id = Some("conv-old".into());
        crate::session_state::apply_agent_event(&mut state, &ended);

        assert_eq!(
            state.phase,
            Phase::Succeeded,
            "{id}：完成过的回合要留在 Finished"
        );
        assert_eq!(state.conversation_id.as_deref(), Some("conv-old"), "{id}");
        assert!(state.cli_exited, "{id}");
        assert_eq!(state.provider.as_deref(), Some("claude"), "{id}");
        assert_eq!(state.title.as_deref(), Some("这段对话"), "{id}");
        assert_eq!(state.launch.as_deref(), launch, "{id}");
        state
    }

    /// 上次留下、这次桌面还没来得及重新打开的终端，再停一次机也不能丢。
    #[test]
    fn a_terminal_not_yet_reopened_survives_another_shutdown() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            directory.upsert(&sample_state("claude-term"));
            directory.keep_resumable_terminals(&HashSet::from(["claude-term".to_string()]));

            let next = SessionDirectory::restore_from_disk();
            next.keep_resumable_terminals(&HashSet::new());

            assert!(
                SessionDirectory::restore_from_disk()
                    .resume_launch("claude-term")
                    .is_some()
            );
        });
    }

    /// 活着的终端直接 attach，不该拿到续接命令。
    #[test]
    fn a_live_terminal_has_nothing_to_resume() {
        with_test_dir(|| {
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            directory.upsert(&sample_state("claude-term"));
            assert_eq!(directory.resume_launch("claude-term"), None);
        });
    }

    /// 干净退出只该清空会话目录这一份文档，绝不能把承载全部用户数据的主库删掉。
    /// 历史上 `session_directory_path()` 指向 `sessions.json`，迁移到 SQLite 后改为
    /// 返回主库路径，而删除调用没跟着改，于是每次重启守护都会抹掉整个数据库。
    #[test]
    fn clean_shutdown_keeps_the_database_file() {
        with_test_dir(|| {
            let database = session_directory_path();
            let directory = SessionDirectory::with_interval(Duration::ZERO);
            directory.upsert(&sample_state("s1"));
            assert!(database.exists(), "写入后主库应当存在");
            #[cfg(unix)]
            let inode = file_identity(&database);

            directory.keep_resumable_terminals(&HashSet::new());

            assert!(
                database.exists(),
                "干净退出把主库删了，全部用户数据会随之消失"
            );
            #[cfg(unix)]
            assert_eq!(
                file_identity(&database),
                inode,
                "主库被删后重建同样是数据丢失"
            );
        });
    }

    #[cfg(unix)]
    fn file_identity(path: &std::path::Path) -> u64 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).expect("读取主库元数据").ino()
    }

    #[test]
    fn clean_shutdown_clear_follows_an_inflight_persist() {
        with_test_dir(|| {
            let directory = std::sync::Arc::new(SessionDirectory::with_interval(Duration::ZERO));
            let mut state = directory.state.lock().unwrap();
            state.entries.insert("s1".to_string(), sample_state("s1"));

            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let clear_directory = std::sync::Arc::clone(&directory);
            let clear = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                clear_directory.keep_resumable_terminals(&HashSet::new());
                done_tx.send(()).unwrap();
            });
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("清理线程未启动");

            // 清理必须等同一把目录锁，不能在旧快照提交前先删除文档。
            assert!(
                done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
                "清理不能绕过进行中的目录持久化"
            );
            directory.persist_locked(&mut state);
            assert!(session_directory_persisted());
            drop(state);

            done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("持久化结束后清理应完成");
            clear.join().unwrap();
            assert!(
                !session_directory_persisted(),
                "干净退出必须在最后一个持久化提交后删除身份目录"
            );
        });
    }
}
