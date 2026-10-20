//! 一台机器的会话目录：会话摘要与 attention 的事件流、工作区、自动化、历史、新建与删除。
//! 实现，Dart 入口见 `crate::api::catalog`。机队里每个节点一份（见 `crate::fleet`）。
//!
//! ```text
//! 本节点的连接状态 ──> Catalog ──> 每代一个订阅线程：event_subscribe 长连接
//!        │ generation 变：丢状态、停订阅、读缓存          │ 逐行 decode_line → CatalogState
//!        │ Connected：开订阅；其它：停订阅                │ 快照 / 菜单 / 远程目录变了 → session.agent_bindings
//!        v                                                v
//!   调度线程：合并 100ms 推会话列表、最多每 500ms 存一次离线缓存 ──> Dart 订阅者（可多个）
//! ```
//!
//! 只在连接状态是 Connected 时订阅：探活失败（Reconnecting）时旧链路多半已经僵死，阻塞读
//! 不会自己醒，所以直接 shutdown；探活恢复后用新链路重开。订阅自己断开（运行时关掉了流、
//! 协议错误）时按 `daemon_reconnect_backoff` 重连，连接活过 5 秒退避清零。
//!
//! 锁的顺序：连接状态机的锁 → 本模块的锁。持有本模块的锁时绝不调
//! [`RuntimeSource::current`] / [`RuntimeSource::cache`]，也不做网络和 SQLite IO。
//! 状态里不存 `RuntimeClient`（它的 Drop 会 block_on），客户端只活在订阅线程和 FRB 工作线程上。

mod projection;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use smelt_model::automation::AutomationRun;
use smelt_model::control_api::{
    AgentDefinitionIdParams, AgentDefinitionList, AutomationList, AutomationRunOnce,
    AutomationSetEnabled, AutomationSetEnabledParams, NewSessionCatalog, NewSessionCatalogResult,
    SessionAgentBindings, SessionCreate, SessionCreateKind, SessionCreateParams, SessionDelete,
    SessionIdParams,
};
use smelt_model::daemon_state::{
    DaemonStateEvent, DaemonStateEventClient, SubscriptionShape, daemon_reconnect_backoff,
};
use smelt_model::session_control::AcpAgentOption;
use smelt_runtime_client::protocol::control::{ControlClientError, ControlErrorCode, EmptyParams};
use smelt_runtime_client::protocol::daemon::DaemonOperation;
use smelt_runtime_client::protocol::history::{HistoryListCall, HistoryListParams, HistorySession};
use smelt_runtime_client::{RequestError, RuntimeClient};

use crate::api::catalog::{
    AutomationCatalog, CatalogEvent, CatalogEventKind, HistoryRenameResult, HistorySessionSummary,
    SessionKind, SessionList, SessionSummary, WorkspaceCatalog,
};
use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::ConnectionState;
use crate::cache::{Cache, Cached};
use crate::connection::{ConnectionEvent, CurrentRuntime, Listener};
use projection::CatalogState;

/// 这台机器的连接、连接状态与离线缓存从哪来。生产用节点的 `NodeSource`（见
/// `crate::fleet`），测试注入假的。
pub(crate) trait RuntimeSource: Send + Sync {
    fn current(&self) -> Option<CurrentRuntime>;
    fn subscribe(&self, listener: Listener);
    fn cache(&self) -> Option<Arc<Cache>>;
}

/// 各种时限。生产值见 [`Timing::PRODUCTION`]，测试用毫秒级。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// 会话列表的连续变化合并多久再推。
    pub push_delay: Duration,
    /// 实时列表变化后多久存一次离线缓存（这段时间里的后续变化一起存，最多每段存一次）。
    pub save_delay: Duration,
    /// 订阅连接活过这么久，重连退避清零。
    pub stable_after: Duration,
    /// 订阅断开后第 n 次（从 0 起）重连前等多久。
    pub backoff: fn(u32) -> Duration,
    /// 刚连上时，要用会话目录的调用最多等首个快照多久。
    pub sync_wait: Duration,
    /// 一般 Control 调用的时限。
    pub call_timeout: Duration,
    /// 可能要扫目录、探测 CLI、起进程的调用的时限。
    pub slow_call_timeout: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        push_delay: Duration::from_millis(100),
        save_delay: Duration::from_millis(500),
        stable_after: Duration::from_secs(5),
        backoff: daemon_reconnect_backoff,
        sync_wait: Duration::from_secs(5),
        call_timeout: Duration::from_secs(15),
        slow_call_timeout: Duration::from_secs(30),
    };
}

/// Dart 订阅者：返回 false 表示已取消。在状态锁里调用，不能阻塞。
pub(crate) type EventListener = Box<dyn Fn(&CatalogEvent) -> bool + Send>;

/// `create_session` 的参数。
pub(crate) struct CreateSession {
    pub project_root: String,
    pub agent_option_id: Option<String>,
    pub launch_key: Option<String>,
    pub resume_id: Option<String>,
    pub kind: Option<SessionKind>,
    pub request_id: Option<String>,
}

pub(crate) struct Catalog {
    shared: Arc<Shared>,
}

struct Shared {
    source: Arc<dyn RuntimeSource>,
    timing: Timing,
    inner: Mutex<Inner>,
    /// 调度线程和等首个快照的调用共用；条件各自重查。
    wake: Condvar,
}

struct Inner {
    /// 推出去的事件都带上它：Dart 一条流收所有机器的事件。
    runtime_id: String,
    closed: bool,
    /// 最近一次看到的连接代号。还没看到过时为 None。
    generation: Option<u64>,
    /// 这一代的运行时。
    endpoint_id: Option<String>,
    /// 连接状态是 Connected。
    connected: bool,
    /// 这一代当前的订阅线程。
    worker: Option<Arc<Worker>>,
    state: CatalogState,
    /// 还没收到实时快照时展示的离线缓存。
    cached: Option<Cached<Vec<SessionSummary>>>,
    /// 这一代的缓存还没读。
    cache_pending: bool,
    /// 这一代最近一次 `new_session.catalog` 的 agent 选项，历史列表靠它把选项还原成引擎和 profile。
    agent_options: Vec<AcpAgentOption>,
    listeners: Vec<EventListener>,
    /// 上次推出去的列表，内容没变就不再推。
    last_list: Option<SessionList>,
    push_at: Option<Instant>,
    save_at: Option<Instant>,
    /// 这一代上次推过的订阅错误，同样的错误不重复推。
    last_error: Option<String>,
    /// 调度线程有活：开订阅线程或读缓存。
    signal: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Inner {
    fn is_current(&self, worker: &Arc<Worker>) -> bool {
        !self.closed
            && self
                .worker
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, worker))
    }

    fn stop_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.stop();
        }
    }

    fn emit(&mut self, event: CatalogEvent) {
        self.listeners.retain(|listener| listener(&event));
    }

    fn event(&self, kind: CatalogEventKind) -> CatalogEvent {
        CatalogEvent {
            runtime_id: self.runtime_id.clone(),
            kind,
            sessions: None,
            attention: None,
            session_id: None,
            message: None,
        }
    }

    fn current_list(&self) -> SessionList {
        if self.state.primed {
            return SessionList {
                sessions: self.state.summaries(),
                cached: false,
                saved_at_ms: None,
            };
        }
        match &self.cached {
            Some(cached) => SessionList {
                sessions: cached.value.clone(),
                cached: true,
                saved_at_ms: Some(cached.saved_at_ms),
            },
            None => SessionList {
                sessions: Vec::new(),
                cached: true,
                saved_at_ms: None,
            },
        }
    }

    fn schedule_push(&mut self, timing: &Timing) -> bool {
        if self.push_at.is_some() {
            return false;
        }
        self.push_at = Some(Instant::now() + timing.push_delay);
        true
    }

    /// 立刻推当前列表（和上次一样就不推）。实时列表变了就排一次存缓存。
    fn flush_sessions(&mut self, timing: &Timing) {
        self.push_at = None;
        let list = self.current_list();
        if self.last_list.as_ref() == Some(&list) {
            return;
        }
        if !list.cached && self.save_at.is_none() {
            self.save_at = Some(Instant::now() + timing.save_delay);
        }
        self.last_list = Some(list.clone());
        let event = self.sessions_event(list);
        self.emit(event);
    }

    /// 提醒类事件之前先把攒着的列表推出去，界面收到提醒时列表已经是新的。
    fn flush_pending(&mut self, timing: &Timing) {
        if self.push_at.is_some() {
            self.flush_sessions(timing);
        }
    }
}

impl Inner {
    fn sessions_event(&self, list: SessionList) -> CatalogEvent {
        CatalogEvent {
            sessions: Some(list),
            ..self.event(CatalogEventKind::Sessions)
        }
    }

    fn attention_event(&self, item: crate::api::catalog::AttentionItem) -> CatalogEvent {
        CatalogEvent {
            attention: Some(item),
            ..self.event(CatalogEventKind::Attention)
        }
    }

    fn resolved_event(&self, session_id: String) -> CatalogEvent {
        CatalogEvent {
            session_id: Some(session_id),
            ..self.event(CatalogEventKind::AttentionResolved)
        }
    }

    fn error_event(&self, message: String) -> CatalogEvent {
        CatalogEvent {
            message: Some(message),
            ..self.event(CatalogEventKind::Error)
        }
    }
}

impl Catalog {
    pub(crate) fn new(runtime_id: String, source: Arc<dyn RuntimeSource>, timing: Timing) -> Self {
        let shared = Arc::new(Shared {
            source: Arc::clone(&source),
            timing,
            inner: Mutex::new(Inner {
                runtime_id,
                closed: false,
                generation: None,
                endpoint_id: None,
                connected: false,
                worker: None,
                state: CatalogState::default(),
                cached: None,
                cache_pending: false,
                agent_options: Vec::new(),
                listeners: Vec::new(),
                last_list: None,
                push_at: None,
                save_at: None,
                last_error: None,
                signal: false,
            }),
            wake: Condvar::new(),
        });
        let supervisor = Arc::clone(&shared);
        if let Err(spawn_error) = std::thread::Builder::new()
            .name("smelt-catalog".to_string())
            .spawn(move || supervise(supervisor))
        {
            log::error!("起不了会话目录的调度线程：{spawn_error}");
        }
        let weak = Arc::downgrade(&shared);
        source.subscribe(Box::new(move |generation, event| {
            on_connection(&weak, generation, event)
        }));
        Self { shared }
    }

    /// 订阅目录事件：先推一次当前已知的列表。
    pub(crate) fn subscribe(&self, listener: EventListener) {
        let mut inner = self.shared.lock();
        if inner.closed {
            return;
        }
        let event = inner.sessions_event(inner.current_list());
        if listener(&event) {
            inner.listeners.push(listener);
        }
    }

    pub(crate) fn mark_read(&self, session_id: &str) {
        let mut inner = self.shared.lock();
        if inner.state.mark_read(session_id) && inner.schedule_push(&self.shared.timing) {
            self.shared.wake.notify_all();
        }
    }

    fn runtime(&self) -> Result<CurrentRuntime, SmeltError> {
        self.shared
            .source
            .current()
            .ok_or_else(|| error(SmeltErrorKind::Offline, "还没有连上这台机器"))
    }

    /// 等这一代的首个快照。代号已变或超时返回 false。
    fn wait_primed(&self, generation: u64) -> bool {
        let inner = self.shared.lock();
        let (inner, _) = self
            .shared
            .wake
            .wait_timeout_while(inner, self.shared.timing.sync_wait, |inner| {
                !inner.closed && inner.generation == Some(generation) && !inner.state.primed
            })
            .unwrap_or_else(|e| e.into_inner());
        inner.generation == Some(generation) && inner.state.primed
    }

    fn new_session_catalog(
        &self,
        runtime: &CurrentRuntime,
    ) -> Result<NewSessionCatalogResult, SmeltError> {
        let catalog = runtime
            .client
            .call::<NewSessionCatalog>(
                &EmptyParams::default(),
                self.shared.timing.slow_call_timeout,
            )
            .map_err(control_error)?;
        let mut inner = self.shared.lock();
        if inner.generation == Some(runtime.generation) {
            inner.agent_options = catalog.agents.clone();
        }
        Ok(catalog)
    }

    pub(crate) fn workspace_catalog(&self) -> Result<WorkspaceCatalog, SmeltError> {
        let runtime = self.runtime()?;
        let catalog = self.new_session_catalog(&runtime)?;
        if !self.wait_primed(runtime.generation) {
            return Err(not_synced());
        }
        let projects = self.shared.lock().state.projects();
        Ok(WorkspaceCatalog {
            projects,
            agents: catalog
                .agents
                .iter()
                .map(projection::agent_option)
                .collect(),
            sections: projection::launch_sections(catalog.sections),
        })
    }

    pub(crate) fn automation_catalog(&self) -> Result<AutomationCatalog, SmeltError> {
        let runtime = self.runtime()?;
        self.automation_catalog_with(&runtime, None)
    }

    pub(crate) fn set_automation_enabled(
        &self,
        automation_id: &str,
        enabled: bool,
    ) -> Result<AutomationCatalog, SmeltError> {
        let runtime = self.runtime()?;
        runtime
            .client
            .call::<AutomationSetEnabled>(
                &AutomationSetEnabledParams {
                    id: automation_id.to_string(),
                    enabled,
                },
                self.shared.timing.call_timeout,
            )
            .map_err(control_error)?;
        self.automation_catalog_with(&runtime, None)
    }

    pub(crate) fn run_automation_once(
        &self,
        automation_id: &str,
    ) -> Result<AutomationCatalog, SmeltError> {
        let runtime = self.runtime()?;
        let result = runtime
            .client
            .call::<AutomationRunOnce>(
                &AgentDefinitionIdParams {
                    id: automation_id.to_string(),
                },
                self.shared.timing.call_timeout,
            )
            .map_err(control_error)?;
        self.automation_catalog_with(&runtime, result.result.run().as_ref())
    }

    /// 定义取 `automation.list`（命令刚改过也是新的），状态和运行记录取订阅到的投影。
    fn automation_catalog_with(
        &self,
        runtime: &CurrentRuntime,
        fresh_run: Option<&AutomationRun>,
    ) -> Result<AutomationCatalog, SmeltError> {
        let timeout = self.shared.timing.call_timeout;
        let automations = runtime
            .client
            .call::<AutomationList>(&EmptyParams::default(), timeout)
            .map_err(control_error)?
            .automations;
        let definitions = runtime
            .client
            .call::<AgentDefinitionList>(&EmptyParams::default(), timeout)
            .map_err(control_error)?
            .agents;
        // 没等到快照也照样给目录，只是没有「下次 / 上次」。
        self.wait_primed(runtime.generation);
        let inner = self.shared.lock();
        let file = (inner.generation == Some(runtime.generation))
            .then_some(inner.state.automations.as_ref())
            .flatten();
        Ok(projection::automation_catalog(
            &automations,
            file,
            &definitions,
            fresh_run,
        ))
    }

    /// 按 id 找 agent 选项：先查这一代缓存的目录，没有再问运行时。
    fn agent_option(
        &self,
        runtime: &CurrentRuntime,
        agent_option_id: &str,
    ) -> Result<AcpAgentOption, SmeltError> {
        {
            let inner = self.shared.lock();
            if inner.generation == Some(runtime.generation)
                && let Some(option) = inner
                    .agent_options
                    .iter()
                    .find(|option| option.id == agent_option_id)
            {
                return Ok(option.clone());
            }
        }
        self.new_session_catalog(runtime)?
            .agents
            .into_iter()
            .find(|option| option.id == agent_option_id)
            .ok_or_else(|| {
                error(
                    SmeltErrorKind::Other,
                    format!("运行时上没有这个智能体选项：{agent_option_id}"),
                )
            })
    }

    fn history_sessions(
        &self,
        runtime: &CurrentRuntime,
        option: &AcpAgentOption,
        project_root: &str,
    ) -> Result<Vec<HistorySession>, SmeltError> {
        runtime
            .client
            .call::<HistoryListCall>(
                &HistoryListParams {
                    agent: option.kind.clone(),
                    profile_id: option.profile_id().map(str::to_string),
                    cwd: project_root.to_string(),
                },
                self.shared.timing.slow_call_timeout,
            )
            .map(|result| result.sessions)
            .map_err(control_error)
    }

    pub(crate) fn list_history(
        &self,
        project_root: &str,
        agent_option_id: &str,
    ) -> Result<Vec<HistorySessionSummary>, SmeltError> {
        let runtime = self.runtime()?;
        let option = self.agent_option(&runtime, agent_option_id)?;
        Ok(self
            .history_sessions(&runtime, &option, project_root)?
            .into_iter()
            .map(|session| HistorySessionSummary {
                resume_id: session.resume_id,
                title: session.title,
                custom_title: session.custom_title,
                last_active_at_ms: session.last_active_at_ms,
                message_count: u32::try_from(session.message_count).unwrap_or(u32::MAX),
            })
            .collect())
    }

    /// 改名走首帧 op `history_rename`（运行时写标题覆盖层并同步远程目录）。恢复默认名时
    /// 展示标题得回到 agent 原始标题，那个值要重新列一遍历史才知道。
    pub(crate) fn rename_history(
        &self,
        project_root: &str,
        agent_option_id: &str,
        resume_id: &str,
        title: Option<&str>,
    ) -> Result<HistoryRenameResult, SmeltError> {
        let resume_id = resume_id.trim();
        if resume_id.is_empty() {
            return Err(error(SmeltErrorKind::Other, "缺少要改名的历史会话"));
        }
        let custom_title = title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string);
        let runtime = self.runtime()?;
        let option = self.agent_option(&runtime, agent_option_id)?;
        let request = json!({
            "op": DaemonOperation::HistoryRename,
            "agent": option.kind,
            "profile_id": option.profile_id(),
            "resume_id": resume_id,
            "title": custom_title,
            "cwd": project_root,
        });
        let response = runtime
            .client
            .request(&request, self.shared.timing.call_timeout)
            .map_err(request_error)?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            let message = response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("改名失败");
            return Err(error(SmeltErrorKind::Other, message));
        }
        let display = self
            .history_sessions(&runtime, &option, project_root)
            .map_err(|e| log::warn!("改名后重列历史失败：{}", e.message))
            .ok()
            .and_then(|sessions| {
                sessions
                    .into_iter()
                    .find(|session| session.resume_id == resume_id)
                    .map(|session| session.custom_title.unwrap_or(session.title))
            });
        Ok(HistoryRenameResult {
            title: display.or_else(|| custom_title.clone()).unwrap_or_default(),
            custom_title,
        })
    }

    pub(crate) fn create_session(&self, params: CreateSession) -> Result<String, SmeltError> {
        let nonempty = |value: Option<String>| {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let runtime = self.runtime()?;
        let params = SessionCreateParams {
            project_root: params.project_root.trim().to_string(),
            agent_option_id: nonempty(params.agent_option_id),
            launch_key: nonempty(params.launch_key),
            resume_id: nonempty(params.resume_id),
            kind: params.kind.map(|kind| match kind {
                SessionKind::Acp => SessionCreateKind::Acp,
                SessionKind::Terminal => SessionCreateKind::Terminal,
            }),
            request_id: nonempty(params.request_id),
        };
        runtime
            .client
            .call::<SessionCreate>(&params, self.shared.timing.slow_call_timeout)
            .map(|result| result.session_id)
            .map_err(control_error)
    }

    pub(crate) fn delete_session(&self, session_id: &str) -> Result<(), SmeltError> {
        let runtime = self.runtime()?;
        runtime
            .client
            .call::<SessionDelete>(
                &SessionIdParams {
                    session_id: session_id.to_string(),
                },
                self.shared.timing.call_timeout,
            )
            .map_err(control_error)?;
        {
            let mut inner = self.shared.lock();
            if inner.generation == Some(runtime.generation) {
                inner.state.remove_session(session_id);
                inner.flush_sessions(&self.shared.timing);
            }
        }
        if let Some(cache) = self.shared.source.cache() {
            cache.delete_snapshot(&runtime.endpoint_id, session_id);
        }
        Ok(())
    }
}

impl Catalog {
    /// 停下：停订阅、放掉所有订阅者，之后不再推任何事件。忘记机器、节点被替换时调，不等
    /// 最后一个持有者放手。
    pub(crate) fn close(&self) {
        let mut inner = self.shared.lock();
        inner.closed = true;
        inner.stop_worker();
        inner.listeners.clear();
        drop(inner);
        self.shared.wake.notify_all();
    }
}

impl Drop for Catalog {
    fn drop(&mut self) {
        self.close();
    }
}

/// 连接状态变化。在连接状态机的锁里调用，只记状态、叫醒调度线程。
fn on_connection(weak: &Weak<Shared>, generation: u64, event: &ConnectionEvent) -> bool {
    let Some(shared) = weak.upgrade() else {
        return false;
    };
    let mut inner = shared.lock();
    if inner.closed {
        return false;
    }
    if inner.generation != Some(generation) {
        // 重新连过或断开过：上一代的一切作废（提醒基线、订阅、智能体选项）。
        inner.stop_worker();
        inner.generation = Some(generation);
        inner.endpoint_id = event.endpoint_id.clone();
        inner.state = CatalogState::default();
        inner.cached = None;
        inner.agent_options.clear();
        inner.last_error = None;
        inner.save_at = None;
        inner.cache_pending = inner.endpoint_id.is_some();
        if !inner.cache_pending {
            // 没有缓存可读，立刻清空列表；有缓存的等读完一起推，免得先闪一下空列表。
            inner.push_at = Some(Instant::now());
        }
        inner.signal = true;
    }
    inner.connected = event.state == ConnectionState::Connected;
    if !inner.connected {
        inner.stop_worker();
    } else if inner.worker.is_none() {
        inner.signal = true;
    }
    drop(inner);
    shared.wake.notify_all();
    true
}

/// 调度线程：开订阅线程、读缓存、合并推送列表、存缓存。
fn supervise(shared: Arc<Shared>) {
    let timing = shared.timing;
    loop {
        let mut inner = shared.lock();
        loop {
            if inner.closed {
                return;
            }
            let now = Instant::now();
            let due = |at: Option<Instant>| at.is_some_and(|at| at <= now);
            if inner.signal || due(inner.push_at) || due(inner.save_at) {
                break;
            }
            let next = [inner.push_at, inner.save_at].into_iter().flatten().min();
            let wait = next.map_or(Duration::from_secs(3600), |at| at - now);
            inner = shared
                .wake
                .wait_timeout(inner, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        inner.signal = false;

        if inner.connected
            && inner.worker.is_none()
            && let Some(generation) = inner.generation
        {
            let worker = Arc::new(Worker::new(generation));
            inner.worker = Some(Arc::clone(&worker));
            let thread_shared = Arc::clone(&shared);
            let spawned = std::thread::Builder::new()
                .name("smelt-catalog-events".to_string())
                .spawn(move || run_worker(thread_shared, worker));
            if let Err(spawn_error) = spawned {
                log::error!("起不了会话目录的订阅线程：{spawn_error}");
                inner.worker = None;
            }
        }

        let cache_target = inner
            .cache_pending
            .then(|| inner.generation.zip(inner.endpoint_id.clone()))
            .flatten();
        inner.cache_pending = false;

        let now = Instant::now();
        if inner.push_at.is_some_and(|at| at <= now) {
            inner.flush_sessions(&timing);
        }
        let save = if inner.save_at.is_some_and(|at| at <= now) {
            inner.save_at = None;
            inner
                .state
                .primed
                .then(|| inner.endpoint_id.clone())
                .flatten()
                .map(|endpoint| (endpoint, cache_record(inner.state.summaries())))
        } else {
            None
        };
        drop(inner);

        // 下面是 SQLite IO，不持锁。
        if let Some((generation, endpoint)) = cache_target {
            let loaded = shared
                .source
                .cache()
                .and_then(|cache| cache.load_session_list::<Vec<SessionSummary>>(&endpoint));
            let mut inner = shared.lock();
            if inner.generation == Some(generation) && !inner.state.primed {
                inner.cached = loaded;
                inner.flush_sessions(&timing);
            }
        }
        if let Some((endpoint, sessions)) = save
            && let Some(cache) = shared.source.cache()
        {
            cache.save_session_list(&endpoint, &sessions);
        }
    }
}

/// 存进缓存的列表：提醒和已读是这个进程的本地状态，不进缓存。
fn cache_record(mut sessions: Vec<SessionSummary>) -> Vec<SessionSummary> {
    for session in &mut sessions {
        session.attention = None;
    }
    sessions
}

/// 一代的一个订阅线程。被叫停时把正在读的连接 shutdown，阻塞读随之返回。
struct Worker {
    generation: u64,
    signal: Mutex<WorkerSignal>,
    wake: Condvar,
}

#[derive(Default)]
struct WorkerSignal {
    stopped: bool,
    stream: Option<UnixStream>,
}

impl Worker {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            signal: Mutex::new(WorkerSignal::default()),
            wake: Condvar::new(),
        }
    }

    fn signal(&self) -> MutexGuard<'_, WorkerSignal> {
        self.signal.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn stop(&self) {
        let mut signal = self.signal();
        signal.stopped = true;
        if let Some(stream) = signal.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        self.wake.notify_all();
    }

    fn stopped(&self) -> bool {
        self.signal().stopped
    }

    /// 登记正在读的连接。已被叫停就直接关掉它并返回 false。
    fn attach(&self, stream: &UnixStream) -> bool {
        let mut signal = self.signal();
        if signal.stopped {
            let _ = stream.shutdown(Shutdown::Both);
            return false;
        }
        signal.stream = stream.try_clone().ok();
        true
    }

    /// 等 `delay`，被叫停时提前返回 true。
    fn sleep(&self, delay: Duration) -> bool {
        let signal = self.signal();
        let (signal, _) = self
            .wake
            .wait_timeout_while(signal, delay, |signal| !signal.stopped)
            .unwrap_or_else(|e| e.into_inner());
        signal.stopped
    }
}

fn run_worker(shared: Arc<Shared>, worker: Arc<Worker>) {
    let timing = shared.timing;
    let mut attempt = 0u32;
    while !worker.stopped() {
        // 每次重连都重新取：探活重连成功后客户端会换成新链路的。
        let Some(runtime) = shared
            .source
            .current()
            .filter(|runtime| runtime.generation == worker.generation)
        else {
            break;
        };
        let started = Instant::now();
        if let Err(message) = subscribe_once(&shared, &worker, &runtime.client) {
            log::warn!("会话目录订阅出错：{message}");
            shared.report_error(&worker, message);
        }
        // RuntimeClient 在这个普通线程上释放。
        drop(runtime);
        if started.elapsed() >= timing.stable_after {
            attempt = 0;
        }
        let delay = (timing.backoff)(attempt);
        attempt = attempt.saturating_add(1);
        if worker.sleep(delay) {
            break;
        }
    }
    let mut inner = shared.lock();
    if inner.is_current(&worker) {
        inner.worker = None;
    }
}

/// 开一条 event_subscribe 并读到断开。连接断开返回 Ok，协议错误、运行时拒绝返回 Err。
fn subscribe_once(
    shared: &Arc<Shared>,
    worker: &Arc<Worker>,
    client: &RuntimeClient,
) -> Result<(), String> {
    let stream = match client.connect() {
        Ok(stream) => stream,
        Err(connect_error) => {
            log::info!("会话目录订阅连不上：{connect_error}");
            return Ok(());
        }
    };
    if !worker.attach(&stream) {
        return Ok(());
    }
    let mut decoder = DaemonStateEventClient::new(SubscriptionShape::CATALOG);
    let request = decoder.request_line().map_err(|e| e.to_string())?;
    if (&stream).write_all(request.as_bytes()).is_err() {
        return Ok(());
    }
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some(event) = decoder.decode_line(&line).map_err(|e| e.to_string())? else {
            continue;
        };
        match shared.apply(worker, event) {
            None => return Ok(()),
            Some(true) => refresh_agent_bindings(shared, worker, client),
            Some(false) => {}
        }
    }
}

/// 重取会话的智能体归属。在订阅线程上阻塞做：期间的事件在连接里排队，不会丢。
fn refresh_agent_bindings(shared: &Arc<Shared>, worker: &Arc<Worker>, client: &RuntimeClient) {
    match client.call::<SessionAgentBindings>(&EmptyParams::default(), shared.timing.call_timeout) {
        Ok(result) => shared.set_agent_bindings(worker, result.bindings),
        Err(call_error) => log::warn!("取会话的智能体归属失败：{call_error}"),
    }
}

impl Shared {
    /// 应用一条订阅事件。这个订阅线程已经过时返回 None；否则返回是否要重取智能体归属。
    fn apply(&self, worker: &Arc<Worker>, event: DaemonStateEvent) -> Option<bool> {
        let mut inner = self.lock();
        if !inner.is_current(worker) {
            return None;
        }
        let was_primed = inner.state.primed;
        let applied = inner.state.apply(event, Instant::now());
        if inner.state.primed && !was_primed {
            inner.cached = None;
            inner.last_error = None;
            // 实时列表第一次到，不等合并窗口。
            inner.flush_sessions(&self.timing);
            self.wake.notify_all();
        } else if applied.sessions_changed && inner.schedule_push(&self.timing) {
            self.wake.notify_all();
        }
        if !applied.attention.is_empty() || !applied.resolved.is_empty() {
            inner.flush_pending(&self.timing);
        }
        for item in &applied.attention {
            let event = inner.attention_event(projection::attention_item(item));
            inner.emit(event);
        }
        for session_id in applied.resolved {
            let event = inner.resolved_event(session_id);
            inner.emit(event);
        }
        Some(applied.refresh_bindings)
    }

    fn set_agent_bindings(&self, worker: &Arc<Worker>, bindings: BTreeMap<String, String>) {
        let mut inner = self.lock();
        if inner.is_current(worker)
            && inner.state.set_agent_bindings(bindings)
            && inner.schedule_push(&self.timing)
        {
            self.wake.notify_all();
        }
    }

    fn report_error(&self, worker: &Arc<Worker>, message: String) {
        let mut inner = self.lock();
        if !inner.is_current(worker) || inner.last_error.as_deref() == Some(message.as_str()) {
            return;
        }
        inner.last_error = Some(message.clone());
        let event = inner.error_event(message);
        inner.emit(event);
    }
}

fn not_synced() -> SmeltError {
    error(
        SmeltErrorKind::Offline,
        "还没拿到运行时的会话目录，稍后再试",
    )
}

/// Control 调用失败归类。
fn control_error(call_error: ControlClientError) -> SmeltError {
    match call_error {
        ControlClientError::Transport(message) => {
            error(SmeltErrorKind::Offline, format!("连不上运行时：{message}"))
        }
        ControlClientError::Protocol(protocol_error) => {
            let kind = match protocol_error.code {
                ControlErrorCode::PermissionDenied => SmeltErrorKind::Unpaired,
                ControlErrorCode::UnsupportedVersion | ControlErrorCode::MethodNotFound => {
                    SmeltErrorKind::Incompatible
                }
                _ => SmeltErrorKind::Other,
            };
            error(kind, protocol_error.message)
        }
        ControlClientError::InvalidRequest(message)
        | ControlClientError::InvalidResponse(message) => error(SmeltErrorKind::Other, message),
    }
}

/// 首帧 op 失败归类。运行时不认识的 op 会读完请求直接断开。
fn request_error(request_error: RequestError) -> SmeltError {
    match request_error {
        RequestError::Transport(io_error) => {
            error(SmeltErrorKind::Offline, format!("连不上运行时：{io_error}"))
        }
        RequestError::Closed => error(
            SmeltErrorKind::Incompatible,
            "运行时不支持这个操作，先升级它",
        ),
        RequestError::InvalidResponse(message) => error(SmeltErrorKind::Other, message),
    }
}

#[cfg(test)]
mod tests;
