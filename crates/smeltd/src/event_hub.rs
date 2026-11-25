//! smeltd domain projections backed by the single process-wide `smelt_event_bus::EventHub`.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use smelt_event_bus::{
    Delivery, EventBusError, EventHub, Snapshot, SnapshotProvider, StoredEvent, TopicRegistry,
};
use smelt_event_protocol::{
    AggregateRef, AttentionKind, CORE_SNAPSHOT_AGENT_HOST, CORE_SNAPSHOT_ATTENTION,
    CORE_SNAPSHOT_AUTOMATIONS, CORE_SNAPSHOT_NEW_SESSION_CATALOG, CORE_SNAPSHOT_SESSIONS,
    CORE_SNAPSHOT_WORKSPACE, CORE_TOPIC_AGENT_HOST_CHANGED, CORE_TOPIC_AUTOMATIONS_CHANGED,
    CORE_TOPIC_CONVERSATION_WAKE_REQUESTED, CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED,
    CORE_TOPIC_SESSION_REMOVED, CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_WORKSPACE_CHANGED,
    Capability, ConversationWakeRequested, CoreProjectionEvent, CoreSessionPhase, CorrelationId,
    EventEnvelope, EventId, EventSource, PluginId, PublisherIdentity, SessionStateChanged,
    SessionStateRemoved, SessionStatesSnapshot, SnapshotKind, SubscriptionId, Topic,
};

use super::{
    AutomationFile, DaemonProjectionSeed, Phase, SessionState, WorkspaceDirectoryReader,
};
use crate::attention::{AttentionLedger, Outcome};

const MAILBOX_CAPACITY: usize = 256;

pub(crate) type EventHubHandle = Arc<DaemonEventHub>;

#[derive(Default)]
struct ProjectionState {
    sessions: BTreeMap<String, SessionState>,
    sessions_revision: u64,
    retired_instances: BTreeMap<String, u64>,
    workspace: Option<CoreProjectionEvent>,
    automations: Option<CoreProjectionEvent>,
}

struct ProjectionSnapshotProvider {
    projection: Arc<Mutex<ProjectionState>>,
    workspace: WorkspaceDirectoryReader,
}

impl ProjectionSnapshotProvider {
    /// 守护起来时库读不出来（例：库的形状认不出），缓存里就没有工作区目录，订阅首帧也就
    /// 不带。库修好以后没有改动可等，所以新订阅进来时再读一次：重连或重启的桌面马上拿到
    /// 目录、恢复对话，不用重启守护。读库不占投影锁；这期间推出去的更新版本更大，合并时
    /// 不会被这份盖掉。
    fn load_missing_workspace(&self) -> Result<(), EventBusError> {
        let missing = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?
            .workspace
            .is_none();
        if !missing {
            return Ok(());
        }
        let Ok(directory) = self.workspace.read() else {
            return Ok(());
        };
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        merge_projection(&mut projection.workspace, Some(projection_event(directory)));
        Ok(())
    }
}

impl SnapshotProvider for ProjectionSnapshotProvider {
    fn snapshot(&self, kind: &SnapshotKind, watermark: u64) -> Result<Snapshot, EventBusError> {
        if kind.0 == CORE_SNAPSHOT_WORKSPACE {
            self.load_missing_workspace()?;
        }
        let projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        let (revision, payload) = match kind.0.as_str() {
            CORE_SNAPSHOT_SESSIONS => {
                let sessions = projection
                    .sessions
                    .values()
                    .map(session_dto)
                    .collect::<Vec<_>>();
                (
                    projection.sessions_revision,
                    serde_json::to_value(SessionStatesSnapshot {
                        revision: projection.sessions_revision,
                        sessions,
                    })
                    .map_err(|error| EventBusError::new(error.to_string()))?,
                )
            }
            CORE_SNAPSHOT_WORKSPACE => projection_value(&projection.workspace),
            CORE_SNAPSHOT_AUTOMATIONS => projection_value(&projection.automations),
            _ => {
                return Err(EventBusError::new(format!(
                    "unknown daemon projection snapshot kind: {}",
                    kind.0
                )));
            }
        };
        Ok(Snapshot {
            kind: kind.clone(),
            watermark,
            revision,
            payload,
        })
    }
}

struct AttentionSnapshotProvider {
    attention: Arc<Mutex<AttentionLedger>>,
}

impl SnapshotProvider for AttentionSnapshotProvider {
    fn snapshot(&self, kind: &SnapshotKind, watermark: u64) -> Result<Snapshot, EventBusError> {
        let snapshot = self
            .attention
            .lock()
            .map_err(|_| EventBusError::new("attention lock poisoned"))?
            .snapshot();
        Ok(Snapshot {
            kind: kind.clone(),
            watermark,
            revision: snapshot.revision,
            payload: serde_json::to_value(snapshot)
                .map_err(|error| EventBusError::new(error.to_string()))?,
        })
    }
}

/// 智能体配置的快照现读库：配置只在库里，运行时不另留一份。
struct AgentHostSnapshotProvider;

impl SnapshotProvider for AgentHostSnapshotProvider {
    fn snapshot(&self, kind: &SnapshotKind, watermark: u64) -> Result<Snapshot, EventBusError> {
        let settings = smelt_core::agent_host::load().map_err(EventBusError::new)?;
        Ok(Snapshot {
            kind: kind.clone(),
            watermark,
            revision: 0,
            payload: serde_json::to_value(settings)
                .map_err(|error| EventBusError::new(error.to_string()))?,
        })
    }
}

#[derive(Default)]
struct NewSessionCatalogState {
    diagnostics: Option<smelt_core::acp_conn::AcpRuntimeDiagnostics>,
    diagnostics_gen: u64,
    catalog: Option<smelt_core::control_api::NewSessionCatalogResult>,
}

/// 进程级新建会话目录权威缓存：慢速 `locate_acp_runtime` CLI 探测在无锁下执行并带代际戳，
/// `rebuild_lock` 只覆盖快速建目录与原子发布，防止冷启动探测卡住并发读/改配置。
#[derive(Default)]
struct NewSessionCatalogCache {
    rebuild_lock: Mutex<()>,
    probe_gen: AtomicU64,
    state: Mutex<NewSessionCatalogState>,
}

impl NewSessionCatalogCache {
    fn next_probe_gen(&self) -> u64 {
        self.probe_gen.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn get_cached(&self) -> Option<smelt_core::control_api::NewSessionCatalogResult> {
        self.state.lock().ok().and_then(|s| s.catalog.clone())
    }

    fn diagnostics(&self) -> Option<smelt_core::acp_conn::AcpRuntimeDiagnostics> {
        self.state.lock().ok().and_then(|s| s.diagnostics.clone())
    }

    fn store_diagnostics_if_newer(
        &self,
        diag: smelt_core::acp_conn::AcpRuntimeDiagnostics,
        probe_gen: u64,
    ) -> (smelt_core::acp_conn::AcpRuntimeDiagnostics, bool) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if probe_gen >= guard.diagnostics_gen {
            guard.diagnostics = Some(diag.clone());
            guard.diagnostics_gen = probe_gen;
            (diag, true)
        } else {
            (guard.diagnostics.clone().unwrap_or(diag), false)
        }
    }

    fn store_if_changed(
        &self,
        next: smelt_core::control_api::NewSessionCatalogResult,
    ) -> (smelt_core::control_api::NewSessionCatalogResult, bool) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = guard.catalog.as_ref() != Some(&next);
        guard.catalog = Some(next.clone());
        (next, changed)
    }

    fn rebuild(
        &self,
        reprobe_cli: bool,
        override_diagnostics: Option<(smelt_core::acp_conn::AcpRuntimeDiagnostics, u64)>,
        allow_fast_path: bool,
    ) -> (smelt_core::control_api::NewSessionCatalogResult, bool) {
        if allow_fast_path
            && !reprobe_cli
            && override_diagnostics.is_none()
            && let Some(cached) = self.get_cached()
        {
            return (cached, false);
        }

        // 先在 rebuild_lock 之外完成可能阻塞数秒的 CLI/login shell 探测。
        let probed = match override_diagnostics {
            Some(pair) => Some(pair),
            None if reprobe_cli => {
                let probe_gen = self.next_probe_gen();
                Some((smelt_core::acp_conn::locate_acp_runtime(), probe_gen))
            }
            None => match self.diagnostics() {
                Some(_) => None,
                None => {
                    let probe_gen = self.next_probe_gen();
                    Some((smelt_core::acp_conn::locate_acp_runtime(), probe_gen))
                }
            },
        };

        let Ok(_guard) = self.rebuild_lock.lock() else {
            eprintln!("[event-hub] new_session_catalog rebuild lock poisoned");
            let diag = probed
                .map(|(d, _)| d)
                .or_else(|| self.diagnostics())
                .unwrap_or_else(smelt_core::acp_conn::locate_acp_runtime);
            return (super::session_control::catalog_with(&diag), false);
        };

        if allow_fast_path
            && !reprobe_cli
            && probed.is_none()
            && let Some(cached) = self.get_cached()
        {
            return (cached, false);
        }

        let effective_diag = match probed {
            Some((diag, probe_gen)) => self.store_diagnostics_if_newer(diag, probe_gen).0,
            None => match self.diagnostics() {
                Some(existing) => existing,
                None => {
                    let probe_gen = self.next_probe_gen();
                    let fresh = smelt_core::acp_conn::locate_acp_runtime();
                    self.store_diagnostics_if_newer(fresh, probe_gen).0
                }
            },
        };
        let next = super::session_control::catalog_with(&effective_diag);
        self.store_if_changed(next)
    }
}

struct NewSessionCatalogSnapshotProvider {
    cache: Arc<NewSessionCatalogCache>,
}

impl SnapshotProvider for NewSessionCatalogSnapshotProvider {
    fn snapshot(&self, kind: &SnapshotKind, watermark: u64) -> Result<Snapshot, EventBusError> {
        let (catalog, _) = self.cache.rebuild(false, None, true);
        Ok(Snapshot {
            kind: kind.clone(),
            watermark,
            revision: 0,
            payload: serde_json::to_value(catalog)
                .map_err(|error| EventBusError::new(error.to_string()))?,
        })
    }
}

fn projection_value(projection: &Option<CoreProjectionEvent>) -> (u64, serde_json::Value) {
    match projection {
        Some(projection) => (
            projection.revision,
            serde_json::to_value(projection).unwrap_or(serde_json::Value::Null),
        ),
        None => (
            0,
            serde_json::to_value(CoreProjectionEvent {
                revision: 0,
                data: serde_json::Value::Null,
            })
            .unwrap_or(serde_json::Value::Null),
        ),
    }
}

pub(crate) struct DaemonEventHub {
    pub(crate) appearance_requests: Arc<crate::app_requests::AppearanceRequests>,
    pub(crate) command_requests: Arc<crate::app_requests::CommandRequests>,
    hub: Arc<EventHub>,
    projection: Arc<Mutex<ProjectionState>>,
    /// 提醒（见 `crate::attention`）。锁序：投影锁在前，总线的锁在后；快照只拿这一把。
    attention: Arc<Mutex<AttentionLedger>>,
    /// 每个终端正在显示的进度条（会话 id → 实例号、进度）。只为新订阅者补发：
    /// `terminal.progress` 没有快照，后连上的客户端靠它看到已经在跑的进度条。
    terminal_progress: Mutex<BTreeMap<String, (u64, smelt_event_protocol::TerminalProgress)>>,
    /// 推智能体配置时拿着：读库和发布在同一把锁里，见 [`Self::agent_host_changed`]。
    agent_host: Mutex<()>,
    /// 新建会话目录权威内存投影：启动后后台预热、写偏好/智能体/CLI 探测后刷新并推送。
    new_session_catalog: Arc<NewSessionCatalogCache>,
}

impl DaemonEventHub {
    /// `workspace`：缓存里还没有工作区目录时，订阅首帧从这里现读。
    pub(crate) fn new(workspace: WorkspaceDirectoryReader) -> Result<EventHubHandle, String> {
        let mut registry = TopicRegistry::default();
        for descriptor in smelt_event_protocol::core_topic_descriptors()
            .map_err(|error| format!("invalid core topic descriptor: {error}"))?
        {
            registry
                .register(descriptor)
                .map_err(|error| format!("register core topic: {error}"))?;
        }
        let hub = Arc::new(EventHub::new(registry));
        let projection = Arc::new(Mutex::new(ProjectionState::default()));
        let attention = Arc::new(Mutex::new(AttentionLedger::default()));
        let new_session_catalog = Arc::new(NewSessionCatalogCache::default());
        register_snapshot_providers(
            &hub,
            &projection,
            &attention,
            Arc::clone(&new_session_catalog),
            workspace,
        )
        .map_err(|error| format!("register core snapshot provider: {error}"))?;
        Ok(Arc::new(Self {
            appearance_requests: Arc::default(),
            command_requests: Arc::default(),
            hub,
            projection,
            attention,
            terminal_progress: Mutex::new(BTreeMap::new()),
            agent_host: Mutex::new(()),
            new_session_catalog,
        }))
    }

    pub(crate) fn runtime(&self) -> &Arc<EventHub> {
        &self.hub
    }

    pub(crate) fn synchronize_projection(&self, seed: DaemonProjectionSeed) {
        let mut projection = self.projection.lock().unwrap();
        let mut sessions = seed
            .sessions
            .into_iter()
            .filter(|state| {
                state.instance == 0
                    || !projection
                        .retired_instances
                        .get(&state.id)
                        .is_some_and(|retired| state.instance <= *retired)
            })
            .map(|state| (state.id.clone(), state))
            .collect::<BTreeMap<_, _>>();
        for (id, next) in &mut sessions {
            if let Some(current) = projection.sessions.get(id)
                && session_state_is_newer(current, next)
            {
                *next = current.clone();
            }
        }
        if session_projection_changed(&projection.sessions, &sessions) {
            projection.sessions = sessions;
            projection.sessions_revision = projection.sessions_revision.saturating_add(1);
        }
        merge_projection(
            &mut projection.workspace,
            seed.workspace.map(projection_event),
        );
        merge_projection(
            &mut projection.automations,
            seed.automations
                .map(|file| projection_event(file.live_projection())),
        );
    }

    pub(crate) fn publish_session(&self, state: &SessionState) -> Result<bool, EventBusError> {
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        if state.instance != 0
            && projection
                .retired_instances
                .get(&state.id)
                .is_some_and(|retired| state.instance <= *retired)
        {
            return Ok(false);
        }
        let changed = match projection.sessions.get(&state.id) {
            // 实例 0 是没有运行时的留下的会话（崩溃恢复、归档）。活实例一来就顶掉它，不比
            // 修订：留下的那份也会改展示字段重发（见 `crate::session_names`），修订可能比
            // 刚起来、还没发出去的活实例新。反过来留下的那份不能盖掉活实例。
            Some(current) if state.instance != 0 && current.instance == 0 => true,
            Some(current) if state.instance == 0 && current.instance != 0 => false,
            Some(current)
                if state.instance != 0
                    && current.instance != 0
                    && state.instance < current.instance =>
            {
                false
            }
            Some(current)
                if state.instance != 0
                    && current.instance != 0
                    && state.instance > current.instance =>
            {
                true
            }
            Some(current) if state.revision != 0 && state.revision <= current.revision => false,
            Some(_) | None => true,
        };
        if !changed {
            return Ok(false);
        }
        let dto = session_dto(state);
        let aggregate = session_aggregate(state);
        self.publish_core(
            CORE_TOPIC_SESSION_STATE_CHANGED,
            serde_json::to_value(&dto)
                .map_err(|error| EventBusError::new(format!("serialize session event: {error}")))?,
            aggregate,
        )?;
        projection.sessions.insert(state.id.clone(), state.clone());
        projection.sessions_revision = projection.sessions_revision.saturating_add(1);
        // 还在投影锁里：同一段会话先后两次状态按发布的顺序进提醒。
        self.update_attention(|ledger| ledger.observe(dto.into(), Instant::now()));
        Ok(true)
    }

    /// 总线上现在发布着的这段会话状态。
    pub(crate) fn published_session(&self, id: &str) -> Option<SessionState> {
        let projection = self.projection.lock().ok()?;
        projection.sessions.get(id).cloned()
    }

    pub(crate) fn remove_session(&self, id: &str, instance: u64) -> Result<bool, EventBusError> {
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        if instance != 0 {
            let retired = projection
                .retired_instances
                .entry(id.to_string())
                .or_default();
            *retired = (*retired).max(instance);
        }
        let Some(current) = projection.sessions.get(id).cloned() else {
            return Ok(true);
        };
        let blocked = if instance == 0 {
            current.instance != 0
        } else {
            current.instance != 0 && current.instance > instance
        };
        if blocked {
            return Ok(false);
        }
        let removed_at_ms = now_ms();
        let payload = SessionStateRemoved {
            session_id: id.to_string(),
            generation: current.instance,
            last_revision: current.revision,
            removed_at_ms,
        };
        let aggregate_revision = current.revision.checked_add(1);
        let aggregate = aggregate_revision.map(|revision| {
            (
                AggregateRef::new("session", format!("{}:{}", current.id, current.instance))
                    .expect("daemon session aggregate is valid"),
                revision,
            )
        });
        self.publish_core(
            CORE_TOPIC_SESSION_REMOVED,
            serde_json::to_value(payload)
                .map_err(|error| EventBusError::new(format!("serialize removal event: {error}")))?,
            aggregate,
        )?;
        projection.sessions.remove(id);
        projection.sessions_revision = projection.sessions_revision.saturating_add(1);
        self.update_attention(|ledger| ledger.remove(id));
        Ok(true)
    }

    /// 工作区目录改完之后推整份。版本不比已推出去的新就不推：并发的两次改动各自读回
    /// 整份目录，后读的那份一定带着更大的版本。
    pub(crate) fn publish_workspace(
        &self,
        directory: &smelt_core::workspace_directory::WorkspaceDirectory,
    ) -> Result<bool, EventBusError> {
        let published = self.publish_projection(
            CORE_TOPIC_WORKSPACE_CHANGED,
            directory.revision,
            serde_json::to_value(directory).map_err(|error| {
                EventBusError::new(format!("serialize workspace directory: {error}"))
            })?,
            ProjectionTarget::Workspace,
        )?;
        if published {
            crate::session_names::directory(directory);
        }
        Ok(published)
    }

    pub(crate) fn publish_automations(
        &self,
        snapshot: &AutomationFile,
    ) -> Result<bool, EventBusError> {
        let snapshot = snapshot.live_projection();
        self.publish_projection(
            CORE_TOPIC_AUTOMATIONS_CHANGED,
            snapshot.revision,
            serde_json::to_value(&snapshot)
                .map_err(|error| EventBusError::new(format!("serialize automations: {error}")))?,
            ProjectionTarget::Automations,
        )
    }

    fn publish_projection(
        &self,
        topic: &str,
        revision: u64,
        data: serde_json::Value,
        target: ProjectionTarget,
    ) -> Result<bool, EventBusError> {
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| EventBusError::new("daemon projection lock poisoned"))?;
        let current = match target {
            ProjectionTarget::Workspace => &projection.workspace,
            ProjectionTarget::Automations => &projection.automations,
        };
        if current
            .as_ref()
            .is_some_and(|current| revision != 0 && revision <= current.revision)
        {
            return Ok(false);
        }
        let event = CoreProjectionEvent { revision, data };
        let aggregate = (revision != 0).then(|| {
            (
                AggregateRef::new("projection", topic.replace('.', "-"))
                    .expect("core projection aggregate is valid"),
                revision,
            )
        });
        self.publish_core(
            topic,
            serde_json::to_value(&event).map_err(|error| {
                EventBusError::new(format!("serialize core projection event: {error}"))
            })?,
            aggregate,
        )?;
        match target {
            ProjectionTarget::Workspace => projection.workspace = Some(event),
            ProjectionTarget::Automations => projection.automations = Some(event),
        }
        Ok(true)
    }

    /// 项目目录里的文件变了。没人订阅就不发。
    pub(crate) fn publish_project_changed(&self, root: &str) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_PROJECT_CHANGED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::ProjectChanged {
            root: root.to_string(),
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_PROJECT_CHANGED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize project change: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 一次目录占用统计的进度。没人订阅就不发。
    pub(crate) fn publish_fs_usage_progress(
        &self,
        scan: &str,
        files: u64,
        bytes: u64,
        done: bool,
    ) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_FS_USAGE_PROGRESS)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::FsUsageProgressChanged {
            scan: scan.to_string(),
            files,
            bytes,
            done,
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_FS_USAGE_PROGRESS,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize fs usage progress: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 智能体的记忆目录里有文件变了（`agent_id` 为 `None` 是用户全局记忆）。没人订阅就不发。
    pub(crate) fn publish_agent_memory_changed(
        &self,
        agent_id: Option<&str>,
    ) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::AgentMemoryChanged {
            agent_id: agent_id.map(str::to_string),
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_AGENT_MEMORY_CHANGED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize agent memory change: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 经运行时改了 Pi 的配置。没人订阅就不发。
    pub(crate) fn publish_pi_config_changed(&self) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_PI_CONFIG_CHANGED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::PiConfigChanged {};
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_PI_CONFIG_CHANGED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize pi config change: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 智能体配置写过了：谁写的、改成功还是改了一半都调。读出整份推给客户端（没人订阅就不推，
    /// 后订阅的从快照拿），会话名也跟着重算。
    ///
    /// 读库和发布在同一把锁里：两次写挨着发生时，先读到旧配置的那次一定先发出去，客户端最后
    /// 拿到的总是新的那份。
    pub(crate) fn agent_host_changed(&self) {
        crate::session_names::agents_changed();
        {
            let Ok(_publishing) = self.agent_host.lock() else {
                eprintln!("[event-hub] agent host lock poisoned");
                self.refresh_new_session_catalog(false);
                return;
            };
            let topic =
                Topic::new(CORE_TOPIC_AGENT_HOST_CHANGED).expect("registered core topic is valid");
            if self.hub.has_subscriber(&topic).unwrap_or(true) {
                let published = smelt_core::agent_host::load()
                    .map_err(EventBusError::new)
                    .and_then(|settings| {
                        serde_json::to_value(settings).map_err(|error| {
                            EventBusError::new(format!("serialize agent host settings: {error}"))
                        })
                    })
                    .and_then(|payload| {
                        self.publish_core(CORE_TOPIC_AGENT_HOST_CHANGED, payload, None)
                    });
                if let Err(error) = published {
                    eprintln!("[event-hub] agent host publish failed: {error}");
                }
            }
        }
        self.refresh_new_session_catalog(false);
    }

    /// 读取新建会话目录；`refresh = false` 时直接走内存缓存，`true` 时强扫一遍 PATH。
    pub(crate) fn new_session_catalog(
        &self,
        refresh: bool,
    ) -> smelt_core::control_api::NewSessionCatalogResult {
        if !refresh && let Some(cached) = self.new_session_catalog.get_cached() {
            return cached;
        }
        self.rebuild_new_session_catalog(refresh, None, refresh)
    }

    /// 偏好、智能体或启动项改过后刷新目录投影，并在结果确实变了且有人订阅时推给客户端。
    pub(crate) fn refresh_new_session_catalog(&self, reprobe_cli: bool) {
        let _ = self.rebuild_new_session_catalog(reprobe_cli, None, true);
    }

    /// 设置页刚完成 `agent.cli.inspect` / `install` 时顺便同步探测结果，不再重复扫 PATH。
    pub(crate) fn update_runtime_diagnostics(
        &self,
        diagnostics: smelt_core::acp_conn::AcpRuntimeDiagnostics,
    ) {
        let probe_gen = self.new_session_catalog.next_probe_gen();
        let _ = self.rebuild_new_session_catalog(false, Some((diagnostics, probe_gen)), true);
    }

    fn rebuild_new_session_catalog(
        &self,
        reprobe_cli: bool,
        override_diagnostics: Option<(smelt_core::acp_conn::AcpRuntimeDiagnostics, u64)>,
        publish_if_changed: bool,
    ) -> smelt_core::control_api::NewSessionCatalogResult {
        let (catalog, changed) = self.new_session_catalog.rebuild(
            reprobe_cli,
            override_diagnostics,
            !publish_if_changed,
        );
        if publish_if_changed && changed {
            let topic = Topic::new(CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED)
                .expect("registered core topic is valid");
            if self.hub.has_subscriber(&topic).unwrap_or(true) {
                match serde_json::to_value(&catalog) {
                    Ok(payload) => {
                        if let Err(error) =
                            self.publish_core(CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED, payload, None)
                        {
                            eprintln!("[event-hub] new_session_catalog publish failed: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!("[event-hub] serialize new_session_catalog failed: {error}");
                    }
                }
            }
        }
        catalog
    }

    /// 经运行时改了历史存档。没人订阅就不发。
    pub(crate) fn publish_history_changed(&self) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_HISTORY_CHANGED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::HistoryChanged {};
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_HISTORY_CHANGED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize history change: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 终端程序的通知、响铃、智能体发来的消息进提醒。`session_id` 为空是命令行发的。
    pub(crate) fn notify_attention(
        &self,
        session_id: &str,
        kind: AttentionKind,
        title: &str,
        message: &str,
    ) {
        self.update_attention(|ledger| {
            ledger.notice(session_id, kind, title, message, Instant::now())
        });
    }

    /// 用户看过这段会话了。
    pub(crate) fn mark_attention_read(&self, session_id: &str) {
        self.update_attention(|ledger| ledger.mark_read(session_id));
    }

    /// 改提醒，变化在同一把锁里广播：订阅时取的快照不会和广播交错，把旧的发到新的后面。
    /// 没人订阅就不广播，后来的订阅者从快照里拿。推送不看有没有人订阅。
    fn update_attention(&self, change: impl FnOnce(&mut AttentionLedger) -> Outcome) {
        let Ok(mut ledger) = self.attention.lock() else {
            eprintln!("[event-hub] attention lock poisoned");
            return;
        };
        let Outcome { changed, push } = change(&mut ledger);
        if let Some(command) = push {
            crate::push::apply(command);
        }
        let Some(changed) = changed else {
            return;
        };
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_ATTENTION_CHANGED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic).unwrap_or(true) {
            return;
        }
        let published = serde_json::to_value(changed)
            .map_err(|error| EventBusError::new(format!("serialize attention: {error}")))
            .and_then(|payload| {
                self.publish_core(
                    smelt_event_protocol::CORE_TOPIC_ATTENTION_CHANGED,
                    payload,
                    None,
                )
            });
        if let Err(error) = published {
            eprintln!("[event-hub] attention publish failed: {error}");
        }
    }

    /// 终端里的程序发了一条 OSC 通知，或智能体主动发了一条（带标题）。没人订阅就不发：
    /// 通知是一次性的，不补发。有人订阅、发出去了返回 true。
    pub(crate) fn publish_terminal_notification(
        &self,
        session_id: &str,
        title: Option<&str>,
        message: &str,
    ) -> Result<bool, EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_TERMINAL_NOTIFICATION)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(false);
        }
        let payload = smelt_event_protocol::TerminalNotification {
            session_id: session_id.to_string(),
            title: title.map(str::to_string),
            message: message.to_string(),
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_TERMINAL_NOTIFICATION,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize terminal notification: {error}"))
            })?,
            None,
        )?;
        Ok(true)
    }

    /// 记下并广播一个终端的进度变化（`None` = 清掉）。记账和广播在同一把锁里：订阅时的补发不会
    /// 插在中间，把旧值发到新值后面。
    pub(crate) fn publish_terminal_progress(
        &self,
        session_id: &str,
        instance: u64,
        progress: Option<smelt_event_protocol::TerminalProgress>,
    ) -> Result<(), EventBusError> {
        let mut current = self
            .terminal_progress
            .lock()
            .map_err(|_| EventBusError::new("terminal progress lock poisoned"))?;
        match progress {
            Some(progress) => {
                current.insert(session_id.to_string(), (instance, progress));
            }
            None => {
                current.remove(session_id);
            }
        }
        self.broadcast_terminal_progress(session_id, progress)
    }

    /// 新订阅了 `terminal.progress` 的客户端连上后调：把正在显示的进度条再发一遍。
    /// 广播会让老订阅者也收到一份，进度是状态，重复无害。
    pub(crate) fn replay_terminal_progress(&self) -> Result<(), EventBusError> {
        let current = self
            .terminal_progress
            .lock()
            .map_err(|_| EventBusError::new("terminal progress lock poisoned"))?;
        for (session_id, (_, progress)) in current.iter() {
            self.broadcast_terminal_progress(session_id, Some(*progress))?;
        }
        Ok(())
    }

    /// 这个终端实例结束了，不再补发它的进度条。同 id 的新实例不受影响。
    pub(crate) fn forget_terminal_progress(&self, session_id: &str, instance: u64) {
        let Ok(mut current) = self.terminal_progress.lock() else {
            return;
        };
        if current
            .get(session_id)
            .is_some_and(|(owner, _)| *owner == instance)
        {
            current.remove(session_id);
        }
    }

    /// 没人订阅就不发。
    fn broadcast_terminal_progress(
        &self,
        session_id: &str,
        progress: Option<smelt_event_protocol::TerminalProgress>,
    ) -> Result<(), EventBusError> {
        let topic = Topic::new(smelt_event_protocol::CORE_TOPIC_TERMINAL_PROGRESS)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(());
        }
        let payload = smelt_event_protocol::TerminalProgressChanged {
            session_id: session_id.to_string(),
            progress,
        };
        self.publish_core(
            smelt_event_protocol::CORE_TOPIC_TERMINAL_PROGRESS,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize terminal progress: {error}"))
            })?,
            None,
        )?;
        Ok(())
    }

    /// 请桌面端把一段冷恢复的对话拉起来。返回 `false` 表示此刻没有任何一方
    /// 能兑现这个请求（桌面端没开），调用方应立刻放弃而不是空等。
    pub(crate) fn request_conversation_wake(
        &self,
        session_id: &str,
    ) -> Result<bool, EventBusError> {
        let topic = Topic::new(CORE_TOPIC_CONVERSATION_WAKE_REQUESTED)
            .expect("registered core topic is valid");
        if !self.hub.has_subscriber(&topic)? {
            return Ok(false);
        }
        let payload = ConversationWakeRequested {
            session_id: session_id.to_string(),
        };
        self.publish_core(
            CORE_TOPIC_CONVERSATION_WAKE_REQUESTED,
            serde_json::to_value(payload).map_err(|error| {
                EventBusError::new(format!("serialize conversation wake request: {error}"))
            })?,
            None,
        )?;
        Ok(true)
    }

    fn publish_core(
        &self,
        topic: &str,
        payload: serde_json::Value,
        aggregate: Option<(AggregateRef, u64)>,
    ) -> Result<Arc<StoredEvent>, EventBusError> {
        let event_id = EventId::new(format!("event-{}", uuid::Uuid::new_v4().simple()))
            .expect("generated event id is valid");
        let envelope = core_envelope(
            event_id,
            topic,
            payload,
            aggregate,
            now_ms(),
            CorrelationId::new(format!("correlation-{}", uuid::Uuid::new_v4().simple()))
                .expect("generated correlation id is valid"),
        );
        self.hub
            .publish(&PublisherIdentity::Core, envelope, &BTreeSet::new())
    }

    pub(crate) fn subscribe(
        &self,
        plugin_id: PluginId,
        subscription_id: SubscriptionId,
        topics: BTreeSet<Topic>,
        capabilities: &BTreeSet<Capability>,
    ) -> Result<smelt_event_bus::Subscription, EventBusError> {
        self.hub.subscribe(
            plugin_id,
            subscription_id,
            topics,
            capabilities,
            MAILBOX_CAPACITY,
        )
    }

    #[cfg(test)]
    pub(crate) fn legacy_snapshot(&self) -> Result<serde_json::Value, EventBusError> {
        let sessions = self
            .hub
            .snapshot(&SnapshotKind(CORE_SNAPSHOT_SESSIONS.to_string()))?;
        let sessions: SessionStatesSnapshot = serde_json::from_value(sessions.payload)
            .map_err(|error| EventBusError::new(format!("decode session snapshot: {error}")))?;
        let mut root = serde_json::Map::from_iter([(
            "sessions".to_string(),
            serde_json::to_value(sessions.sessions)
                .map_err(|error| EventBusError::new(error.to_string()))?,
        )]);
        for (field, kind) in [
            ("workspace", CORE_SNAPSHOT_WORKSPACE),
            ("automations", CORE_SNAPSHOT_AUTOMATIONS),
        ] {
            let snapshot = self.hub.snapshot(&SnapshotKind(kind.to_string()))?;
            let projection: CoreProjectionEvent = serde_json::from_value(snapshot.payload)
                .map_err(|error| EventBusError::new(format!("decode {field} snapshot: {error}")))?;
            if !projection.data.is_null() {
                root.insert(field.to_string(), projection.data);
            }
        }
        Ok(serde_json::Value::Object(root))
    }
}

pub(crate) fn core_envelope(
    event_id: EventId,
    topic: &str,
    payload: serde_json::Value,
    aggregate: Option<(AggregateRef, u64)>,
    occurred_at_ms: u64,
    correlation_id: CorrelationId,
) -> EventEnvelope<serde_json::Value> {
    let (aggregate, aggregate_revision) = match aggregate {
        Some((aggregate, revision)) => (Some(aggregate), Some(revision)),
        None => (None, None),
    };
    EventEnvelope {
        event_id,
        topic: Topic::new(topic).expect("registered core topic is valid"),
        schema_version: smelt_event_protocol::core_topic_schema_version(topic),
        occurred_at_ms,
        aggregate,
        aggregate_revision,
        source: EventSource::Core,
        correlation_id,
        causation_id: None,
        hop_count: 0,
        payload,
    }
}

enum ProjectionTarget {
    Workspace,
    Automations,
}

fn register_snapshot_providers(
    hub: &EventHub,
    projection: &Arc<Mutex<ProjectionState>>,
    attention: &Arc<Mutex<AttentionLedger>>,
    new_session_catalog: Arc<NewSessionCatalogCache>,
    workspace: WorkspaceDirectoryReader,
) -> Result<(), EventBusError> {
    hub.register_snapshot_provider(
        SnapshotKind(CORE_SNAPSHOT_ATTENTION.to_string()),
        Arc::new(AttentionSnapshotProvider {
            attention: Arc::clone(attention),
        }),
    )?;
    hub.register_snapshot_provider(
        SnapshotKind(CORE_SNAPSHOT_AGENT_HOST.to_string()),
        Arc::new(AgentHostSnapshotProvider),
    )?;
    hub.register_snapshot_provider(
        SnapshotKind(CORE_SNAPSHOT_NEW_SESSION_CATALOG.to_string()),
        Arc::new(NewSessionCatalogSnapshotProvider {
            cache: new_session_catalog,
        }),
    )?;
    let projection_provider: Arc<dyn SnapshotProvider> = Arc::new(ProjectionSnapshotProvider {
        projection: Arc::clone(projection),
        workspace,
    });
    for kind in [
        CORE_SNAPSHOT_SESSIONS,
        CORE_SNAPSHOT_WORKSPACE,
        CORE_SNAPSHOT_AUTOMATIONS,
    ] {
        hub.register_snapshot_provider(
            SnapshotKind(kind.to_string()),
            Arc::clone(&projection_provider),
        )?;
    }
    Ok(())
}

fn projection_event<T: serde::Serialize>(value: T) -> CoreProjectionEvent {
    let data = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let revision = data
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    CoreProjectionEvent { revision, data }
}

fn session_projection_changed(
    current: &BTreeMap<String, SessionState>,
    next: &BTreeMap<String, SessionState>,
) -> bool {
    current.len() != next.len()
        || current.iter().any(|(id, current)| {
            next.get(id).is_none_or(|next| {
                current.instance != next.instance || current.revision != next.revision
            })
        })
}

fn session_state_is_newer(current: &SessionState, next: &SessionState) -> bool {
    current.instance > next.instance
        || (current.instance == next.instance && current.revision > next.revision)
}

fn merge_projection(current: &mut Option<CoreProjectionEvent>, next: Option<CoreProjectionEvent>) {
    if let Some(next) = next
        && current
            .as_ref()
            .is_none_or(|current| next.revision >= current.revision)
    {
        *current = Some(next);
    }
}

fn session_aggregate(state: &SessionState) -> Option<(AggregateRef, u64)> {
    (state.revision != 0).then(|| {
        (
            AggregateRef::new("session", format!("{}:{}", state.id, state.instance))
                .expect("daemon session aggregate is valid"),
            state.revision,
        )
    })
}

fn session_dto(state: &SessionState) -> SessionStateChanged {
    SessionStateChanged {
        session_id: state.id.clone(),
        generation: state.instance,
        revision: state.revision,
        cwd: state.cwd.clone(),
        launch: state.launch.clone(),
        provider: state.provider.clone(),
        conversation_id: state.conversation_id.clone(),
        custom_title: state.custom_title.clone(),
        title: state.title.clone(),
        conversation_name: state.conversation_name.clone(),
        name: state.name.clone(),
        phase: match state.phase {
            Phase::Connecting => CoreSessionPhase::Connecting,
            Phase::Thinking => CoreSessionPhase::Thinking,
            Phase::ExecutingTool => CoreSessionPhase::ExecutingTool,
            Phase::AwaitingApproval => CoreSessionPhase::AwaitingApproval,
            Phase::WaitingForUser => CoreSessionPhase::WaitingForUser,
            Phase::Succeeded => CoreSessionPhase::Succeeded,
            Phase::Failed => CoreSessionPhase::Failed,
            Phase::Idle => CoreSessionPhase::Idle,
            Phase::Dead => CoreSessionPhase::Dead,
        },
        phase_since_unix_seconds: state.phase_since,
        turn_started_at_unix_seconds: state.turn_started_at,
        pending_question: state
            .pending
            .as_ref()
            .and_then(crate::session_state::PendingLine::question),
        pending_request: state.pending_request.clone(),
        pending_note: state
            .pending
            .as_ref()
            .and_then(crate::session_state::PendingLine::note)
            .cloned(),
        completed_delivery_id: state.completed_delivery_id.clone(),
        tokens_used: state.tokens_used,
        branch: state.branch.clone(),
        dirty_files: state.dirty_files.clone(),
        updated_at_unix_seconds: state.updated_at,
        structured_events: state.structured_events,
        turn_events: state.turn_events,
        agent_event_version: state.agent_event_version,
        runtime: state.runtime,
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

pub(crate) fn delivery_to_subscribe_message(
    delivery: Delivery,
) -> smelt_event_protocol::SubscribeMessage {
    match delivery {
        Delivery::Snapshot(snapshot) => smelt_event_protocol::SubscribeMessage::Snapshot {
            kind: snapshot.kind,
            watermark: snapshot.watermark,
            revision: snapshot.revision,
            payload: snapshot.payload,
        },
        Delivery::Event(event) => smelt_event_protocol::SubscribeMessage::Event {
            sequence: (event.sequence != 0).then_some(event.sequence),
            envelope: event.envelope.clone(),
        },
        Delivery::Lag {
            dropped,
            resume_after,
        } => smelt_event_protocol::SubscribeMessage::Lag {
            dropped,
            resume_after,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_projection_is_available_to_snapshot_recovery() {
        let event_hub = DaemonEventHub::new(WorkspaceDirectoryReader::unavailable()).unwrap();
        let snapshot = AutomationFile {
            revision: 7,
            ..Default::default()
        };

        assert!(event_hub.publish_automations(&snapshot).unwrap());
        assert_eq!(
            event_hub.legacy_snapshot().unwrap()["automations"]["revision"],
            7
        );
        assert!(!event_hub.publish_automations(&snapshot).unwrap());
    }

    #[test]
    fn automation_live_projection_omits_run_bodies() {
        let event_hub = DaemonEventHub::new(WorkspaceDirectoryReader::unavailable()).unwrap();
        let mut snapshot = AutomationFile {
            revision: 4,
            ..Default::default()
        };
        snapshot.runs.push(smelt_core::automation::AutomationRun {
            id: "run-1".into(),
            automation_id: "auto-1".into(),
            context: smelt_core::automation::AutomationRunContext {
                automation_name: "Report".into(),
                action: smelt_core::automation::AutomationAction::Agent {
                    agent_definition_id: "agent-1".into(),
                    prompt: Some("Go".into()),
                },
                cwd: "/tmp".into(),
                trigger_payload: Some(serde_json::json!({"n": 1})),
                prompt: Some("Go".into()),
                agent_definition_name: Some("Agent".into()),
                engine_kind_id: Some("pi".into()),
                agent_instructions: Some("Be useful".into()),
            },
            source: smelt_core::automation::AutomationRunSource::Manual,
            scheduled_for: None,
            status: smelt_core::automation::AutomationRunStatus::Completed,
            created_at: 1,
            started_at: Some(1),
            delivery_attempt_at: Some(1),
            delivery_attempts: 1,
            finished_at: Some(2),
            session_id: None,
            provider_session_id: None,
            output: Some("blob".into()),
            error_code: None,
            error: None,
            runtime_released_at: Some(2),
        });
        assert!(event_hub.publish_automations(&snapshot).unwrap());
        let run = &event_hub.legacy_snapshot().unwrap()["automations"]["runs"][0];
        assert!(run.get("output").is_none() || run["output"].is_null());
        assert!(run["context"].get("trigger_payload").is_none());
        assert!(run["context"].get("agent_instructions").is_none());
        assert_eq!(run["context"]["prompt"], "Go");
    }

    #[test]
    fn new_session_catalog_is_cached_and_publishes_updates_to_subscribers() {
        use smelt_core::acp_conn::{AcpRuntimeDiagnostics, RuntimeExecutable};
        use smelt_core::agent_kind::TerminalAgentKind;
        use smelt_core::new_session::NewSessionTarget;

        let event_hub = DaemonEventHub::new(WorkspaceDirectoryReader::unavailable()).unwrap();

        let mut initial_diag = AcpRuntimeDiagnostics::default();
        initial_diag.record_terminal(
            TerminalAgentKind::Claude,
            RuntimeExecutable {
                program: "claude".into(),
                path: Some("/usr/bin/claude".into()),
                ..Default::default()
            },
        );
        event_hub.update_runtime_diagnostics(initial_diag);

        let cached = event_hub.new_session_catalog(false);
        assert!(cached.sections.terminal.iter().any(|action| matches!(
            &action.target,
            NewSessionTarget::Terminal {
                provider: Some(provider),
                ..
            } if provider == "claude"
        )));

        let sub = event_hub
            .subscribe(
                PluginId::new("test.desktop").unwrap(),
                SubscriptionId::new("catalog").unwrap(),
                BTreeSet::from([Topic::new(CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED).unwrap()]),
                &BTreeSet::from([Capability::new(
                    smelt_event_protocol::CORE_CAPABILITY_WORKSPACE_READ,
                )
                .unwrap()]),
            )
            .unwrap();

        let wire_expected: smelt_core::control_api::NewSessionCatalogResult =
            serde_json::from_value(serde_json::to_value(&cached).unwrap()).unwrap();

        match sub.recv().unwrap() {
            Delivery::Snapshot(snapshot) => {
                assert_eq!(snapshot.kind.0, CORE_SNAPSHOT_NEW_SESSION_CATALOG);
                let decoded: smelt_core::control_api::NewSessionCatalogResult =
                    serde_json::from_value(snapshot.payload).unwrap();
                assert_eq!(decoded, wire_expected);
            }
            other => panic!("expected initial catalog snapshot, got {other:?}"),
        }

        let mut updated_diag = AcpRuntimeDiagnostics::default();
        updated_diag.record_terminal(
            TerminalAgentKind::Grok,
            RuntimeExecutable {
                program: "grok".into(),
                path: Some("/usr/bin/grok".into()),
                ..Default::default()
            },
        );
        event_hub.update_runtime_diagnostics(updated_diag);

        match sub.recv().unwrap() {
            Delivery::Event(event) => {
                assert_eq!(
                    event.envelope.topic.as_str(),
                    CORE_TOPIC_NEW_SESSION_CATALOG_CHANGED
                );
                let decoded: smelt_core::control_api::NewSessionCatalogResult =
                    serde_json::from_value(event.envelope.payload.clone()).unwrap();
                assert!(decoded.sections.terminal.iter().any(|action| matches!(
                    &action.target,
                    NewSessionTarget::Terminal {
                        provider: Some(provider),
                        ..
                    } if provider == "grok"
                )));
                let current_wire: smelt_core::control_api::NewSessionCatalogResult =
                    serde_json::from_value(
                        serde_json::to_value(event_hub.new_session_catalog(false)).unwrap(),
                    )
                    .unwrap();
                assert_eq!(current_wire, decoded);
            }
            other => panic!("expected live catalog update event, got {other:?}"),
        }
    }

    #[test]
    fn agent_host_changed_refreshes_catalog_and_ignores_stale_probes() {
        use smelt_core::acp_conn::{AcpRuntimeDiagnostics, RuntimeExecutable};
        use smelt_core::agent_kind::TerminalAgentKind;

        let event_hub = DaemonEventHub::new(WorkspaceDirectoryReader::unavailable()).unwrap();
        let mut stale_diag = AcpRuntimeDiagnostics::default();
        stale_diag.record_terminal(
            TerminalAgentKind::Claude,
            RuntimeExecutable {
                program: "claude".into(),
                path: Some("/usr/bin/claude".into()),
                ..Default::default()
            },
        );
        let stale_gen = event_hub.new_session_catalog.next_probe_gen();

        let mut newer_diag = AcpRuntimeDiagnostics::default();
        newer_diag.record_terminal(
            TerminalAgentKind::Grok,
            RuntimeExecutable {
                program: "grok".into(),
                path: Some("/usr/bin/grok".into()),
                ..Default::default()
            },
        );
        // 模拟较新的探测/安装先落地，随后较老的 probe_gen 才返回：老探测不得覆盖新结果。
        event_hub.update_runtime_diagnostics(newer_diag);
        let _ = event_hub.rebuild_new_session_catalog(false, Some((stale_diag, stale_gen)), true);
        let catalog = event_hub.new_session_catalog(false);
        assert!(catalog.sections.terminal.iter().any(|action| {
            matches!(
                &action.target,
                smelt_core::new_session::NewSessionTarget::Terminal {
                    provider: Some(provider),
                    ..
                } if provider == "grok"
            )
        }));
        assert!(!catalog.sections.terminal.iter().any(|action| {
            matches!(
                &action.target,
                smelt_core::new_session::NewSessionTarget::Terminal {
                    provider: Some(provider),
                    ..
                } if provider == "claude"
            )
        }));

        // 没有任何 agent_host_changed 订阅者时，调用 agent_host_changed() 仍应保证缓存已建立。
        let fresh_hub = DaemonEventHub::new(WorkspaceDirectoryReader::unavailable()).unwrap();
        let mut diag = AcpRuntimeDiagnostics::default();
        diag.record_terminal(
            TerminalAgentKind::Grok,
            RuntimeExecutable {
                program: "grok".into(),
                path: Some("/usr/bin/grok".into()),
                ..Default::default()
            },
        );
        let probe_gen = fresh_hub.new_session_catalog.next_probe_gen();
        fresh_hub
            .new_session_catalog
            .store_diagnostics_if_newer(diag, probe_gen);
        fresh_hub.agent_host_changed();
        assert!(fresh_hub.new_session_catalog.get_cached().is_some());
    }
}
