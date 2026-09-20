//! Daemon-owned automation scheduler and AutomationRun lifecycle.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use chrono::Local;
use smelt_core::acp_session::{AcpTurnOutcome, AcpUserAction};
use smelt_core::agent_definition_store::{
    AgentExecutionDefinition, load_agent_execution_definition,
};
use smelt_core::automation::{AutomationFile, AutomationRun, AutomationRunStatus};
use smelt_core::automation_store::{AutomationStore, local_timezone_fingerprint};
use smelt_core::conversation::ConversationBinding;
use smelt_core::daemon_state::DaemonPhase;
use smelt_core::session_control::{RemoteAcpSession, RemoteSessionLifecycle};

use smelt_core::acp_chat::AcpEntry;
use smelt_core::pi_rpc::with_unpersisted_pi_session;

use super::{
    AcpOpenRequest, AcpSessions, EventHubHandle, RemoteSessions, Sessions, acp_runtime_alive,
    apply_acp_user_action, create_daemon_acp_session, find_agent_option_for, kill_acp_session,
    push_acp_snapshot_since,
};

const RECONCILE_INTERVAL: Duration = Duration::from_millis(200);
const SCHEDULE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_DELIVERY_ATTEMPTS: u32 = 5;
const MAX_DELIVERY_RETRY_SECONDS: i64 = 30;

pub(crate) fn spawn(
    automations: AutomationStore,
    sessions: Sessions,
    acp_sessions: AcpSessions,
    remote_sessions: RemoteSessions,
    event_hub: EventHubHandle,
    came_from_handoff: bool,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("[automation-runtime] 无法创建 Tokio runtime: {error}");
                return;
            }
        };
        runtime.block_on(async move {
            let mut driver = Driver::new(
                automations,
                sessions,
                acp_sessions,
                remote_sessions,
                event_hub,
            );
            driver.recover_active_runs(came_from_handoff);
            driver.run().await;
        });
    });
}

struct ShellOutcome {
    run_id: String,
    result: Result<std::process::Output, String>,
}

struct Driver {
    automations: AutomationStore,
    sessions: Sessions,
    acp_sessions: AcpSessions,
    remote_sessions: RemoteSessions,
    event_hub: EventHubHandle,
    last_reconcile: Instant,
    last_schedule: Instant,
    timezone_fingerprint: String,
    last_error: Option<String>,
    shell_inflight: HashSet<String>,
    shell_pgids: HashMap<String, i32>,
    shell_tx: Sender<ShellOutcome>,
    shell_rx: Receiver<ShellOutcome>,
}

impl Driver {
    fn new(
        automations: AutomationStore,
        sessions: Sessions,
        acp_sessions: AcpSessions,
        remote_sessions: RemoteSessions,
        event_hub: EventHubHandle,
    ) -> Self {
        let now = Instant::now();
        let (shell_tx, shell_rx) = mpsc::channel();
        Self {
            automations,
            sessions,
            acp_sessions,
            remote_sessions,
            event_hub,
            last_reconcile: now.checked_sub(RECONCILE_INTERVAL).unwrap_or(now),
            last_schedule: now.checked_sub(SCHEDULE_INTERVAL).unwrap_or(now),
            timezone_fingerprint: local_timezone_fingerprint(),
            last_error: None,
            shell_inflight: HashSet::new(),
            shell_pgids: HashMap::new(),
            shell_tx,
            shell_rx,
        }
    }

    async fn run(&mut self) {
        loop {
            self.collect_shell_results();
            self.cancel_shell_runs();
            let now = Instant::now();
            if now.duration_since(self.last_schedule) >= SCHEDULE_INTERVAL {
                self.recover_locked_store();
                self.refresh_timezone_if_changed();
                self.claim_due_runs();
                self.last_schedule = now;
            }
            if now.duration_since(self.last_reconcile) >= RECONCILE_INTERVAL {
                self.reconcile_runs();
                self.launch_unbound_runs();
                self.dispatch_queued_runs();
                self.release_terminal_sessions();
                self.last_reconcile = now;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn recover_active_runs(&mut self, came_from_handoff: bool) {
        let runs = self
            .automations
            .lock()
            .unwrap()
            .snapshot()
            .runs
            .into_iter()
            .filter(|run| run.status.is_active())
            .collect::<Vec<_>>();
        for run in runs {
            let Some(session_id) = run.session_id.as_deref() else {
                if run.status != AutomationRunStatus::Starting {
                    self.fail_run(&run, "Run 缺少 ACP 会话，结果未知");
                }
                continue;
            };
            let Some(slot) = self.acp_sessions.get(session_id) else {
                if !matches!(
                    run.status,
                    AutomationRunStatus::Starting | AutomationRunStatus::Queued
                ) {
                    self.fail_run(&run, "daemon 重启后 ACP 会话已丢失，结果未知，未自动重放");
                }
                continue;
            };
            match ensure_run_conversation_binding(&slot.value, &run.id) {
                Ok(true) => push_acp_snapshot_since(&slot.value, true, None),
                Ok(false) => {}
                Err(error) => {
                    self.fail_run_without_session_ownership(&run, &error);
                    continue;
                }
            }
            if run.status == AutomationRunStatus::Dispatching {
                let accepted = slot
                    .value
                    .reduced
                    .lock()
                    .unwrap()
                    .accepted_delivery_ids
                    .contains(&run.id);
                if accepted {
                    self.mark_run_status(
                        &run,
                        AutomationRunStatus::Running,
                        provider_id(&slot.value),
                    );
                } else if came_from_handoff {
                    self.mark_run_status(
                        &run,
                        AutomationRunStatus::Queued,
                        provider_id(&slot.value),
                    );
                } else {
                    self.fail_run(&run, "daemon 重启发生在投递边界，结果未知，未自动重放");
                }
            }
        }
    }

    fn recover_locked_store(&mut self) {
        let snapshot = {
            let mut owner = self.automations.lock().unwrap();
            if !owner.recover_if_locked(Local::now(), local_timezone_fingerprint()) {
                return;
            }
            owner.snapshot()
        };
        publish_automation_projection(&self.event_hub, &snapshot);
    }

    fn refresh_timezone_if_changed(&mut self) {
        let fingerprint = local_timezone_fingerprint();
        if fingerprint == self.timezone_fingerprint {
            return;
        }
        let result = self
            .automations
            .lock()
            .unwrap()
            .refresh_schedules(Local::now());
        match result {
            Ok(snapshot) => {
                publish_automation_projection(&self.event_hub, &snapshot);
                self.timezone_fingerprint = fingerprint;
            }
            Err(error) => self.record_error("刷新自动化时区", error),
        }
    }

    fn claim_due_runs(&mut self) {
        let result = self.automations.lock().unwrap().claim_due(Local::now());
        match result {
            Ok(applied) => {
                if applied.changed {
                    publish_automation_projection(&self.event_hub, &applied.snapshot);
                }
                self.last_error = None;
            }
            Err(error) => self.record_error("claim 到期自动化", error),
        }
    }

    fn launch_unbound_runs(&mut self) {
        let file = self.automations.lock().unwrap().snapshot();
        let pending = file
            .runs
            .iter()
            .filter(|run| {
                run.status == AutomationRunStatus::Starting
                    || (run.status == AutomationRunStatus::Queued
                        && run
                            .session_id
                            .as_deref()
                            .is_some_and(|id| self.acp_sessions.get(id).is_none()))
            })
            .cloned()
            .collect::<Vec<_>>();

        for mut run in pending {
            if !std::path::Path::new(&run.context.cwd).is_dir() {
                self.fail_run(&run, "自动化的 Smelt 工作区不存在或不是目录");
                continue;
            }
            let agent_definition_id = match &run.context.action {
                smelt_core::automation::AutomationAction::Agent {
                    agent_definition_id,
                    ..
                } => agent_definition_id.clone(),
                smelt_core::automation::AutomationAction::Shell { .. } => {
                    self.start_shell_run(run);
                    continue;
                }
            };
            let mut execution = match load_agent_execution_definition(&agent_definition_id) {
                Ok(execution) => execution,
                Err(error) => {
                    self.fail_run(&run, &error);
                    continue;
                }
            };
            if run.context.agent_instructions.is_none() {
                let result = self.automations.lock().unwrap().bind_run_execution(
                    &run.id,
                    execution.definition.name.clone(),
                    execution.kind.id().to_string(),
                    execution.definition.prompt.clone(),
                );
                match result {
                    Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
                    Err(error) => {
                        self.fail_run(&run, &format!("保存智能体执行上下文失败: {error}"));
                        continue;
                    }
                }
                run.context.agent_definition_name = Some(execution.definition.name.clone());
                run.context.engine_kind_id = Some(execution.kind.id().to_string());
                run.context.agent_instructions = Some(execution.definition.prompt.clone());
            } else if run.context.engine_kind_id.as_deref() != Some(execution.kind.id()) {
                self.fail_run(&run, "智能体执行引擎与 Run 快照不一致");
                continue;
            }
            if let Some(instructions) = run.context.agent_instructions.as_ref() {
                execution.launch.env.insert(
                    smelt_core::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV.to_string(),
                    instructions.clone(),
                );
            }
            let session_id = match run.session_id.clone() {
                Some(session_id) => session_id,
                None => {
                    let session_id = format!("acp-automation-{}", uuid::Uuid::new_v4());
                    let result = self.automations.lock().unwrap().bind_run_session(
                        &run.id,
                        session_id.clone(),
                        Local::now().timestamp(),
                    );
                    match result {
                        Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
                        Err(error) => {
                            self.record_error("绑定自动化 ACP 会话", error);
                            continue;
                        }
                    }
                    run.status = AutomationRunStatus::Queued;
                    run.session_id = Some(session_id.clone());
                    session_id
                }
            };
            if let Err(error) = self.create_run_session(&run, &session_id, &execution) {
                self.fail_run(&run, &format!("ACP runtime 启动失败: {error}"));
            }
        }
    }

    fn create_run_session(
        &self,
        run: &AutomationRun,
        session_id: &str,
        execution: &AgentExecutionDefinition,
    ) -> Result<(), String> {
        let option = find_agent_option_for(execution.kind, None)
            .ok_or_else(|| format!("没有可用的 {} ACP 配置", execution.kind.label()))?;
        let cwd = run.context.cwd.clone();
        let launch = launch_for_automation_run(execution);
        let request = AcpOpenRequest {
            id: session_id.to_string(),
            cwd: Some(cwd.clone()),
            launch: launch.clone(),
            ephemeral_env: Default::default(),
            agent_needs_transcript_check: false,
            resume_id: None,
            fork_id: None,
            fork_cut: None,
            tail_limit: None,
            conversation_binding: Some(ConversationBinding::Automation {
                run_id: run.id.clone(),
            }),
            agent_session: None,
            pending_agent_preset: None,
        };
        let remote = RemoteAcpSession {
            id: session_id.to_string(),
            cwd,
            title: format!(
                "{} · {}",
                run.context.automation_name,
                run.context
                    .agent_definition_name
                    .as_deref()
                    .unwrap_or(execution.definition.name.as_str())
            ),
            agent_option_id: option.id,
            agent: execution.kind.id().to_string(),
            launch,
            resume_id: None,
            created_at: run.created_at,
            lifecycle: RemoteSessionLifecycle::Active,
            hidden: true,
        };
        create_daemon_acp_session(
            &request,
            Some(remote),
            &self.sessions,
            &self.acp_sessions,
            &self.event_hub,
            &self.remote_sessions,
        )?;
        Ok(())
    }

    fn dispatch_queued_runs(&mut self) {
        let file = self.automations.lock().unwrap().snapshot();
        let now = Local::now().timestamp();
        let queued = file
            .runs
            .iter()
            .filter(|run| run.status == AutomationRunStatus::Queued)
            .cloned()
            .collect::<Vec<_>>();

        for run in queued {
            if delivery_attempts_exhausted(run.delivery_attempts) {
                self.finish_run(
                    &run,
                    AutomationRunStatus::Failed,
                    None,
                    Some(delivery_failure_message(
                        run.delivery_attempts,
                        "投递预算已耗尽",
                    )),
                    run.provider_session_id.clone(),
                );
                continue;
            }
            if !delivery_retry_is_due(&run, now) {
                continue;
            }
            let Some(session_id) = run.session_id.as_deref() else {
                self.fail_run(&run, "排队投递缺少 ACP 会话");
                continue;
            };
            let Some(slot) = self.acp_sessions.get(session_id) else {
                continue;
            };
            let (accepted, phase, provider_session_id) = {
                let reduced = slot.value.reduced.lock().unwrap();
                (
                    reduced.accepted_delivery_ids.contains(&run.id),
                    reduced.phase,
                    reduced
                        .history_session_id
                        .clone()
                        .or_else(|| reduced.acp_session_id.clone()),
                )
            };
            if accepted {
                self.mark_run_status(&run, AutomationRunStatus::Running, provider_session_id);
                continue;
            }
            if phase == DaemonPhase::Dead {
                self.fail_run(&run, "ACP runtime 在投递前退出");
                continue;
            }
            if phase != DaemonPhase::Idle
                || slot.value.prompt_in_flight.load(Ordering::SeqCst)
                || !acp_runtime_alive(&slot.value)
            {
                continue;
            }

            if let Err(error) = dispatch_if_still_queued(
                &self.automations,
                &run,
                &slot.value,
                provider_session_id,
                &self.event_hub,
            ) {
                self.record_error("投递自动化首包", error);
            }
        }
    }

    fn reconcile_runs(&mut self) {
        let runs = self
            .automations
            .lock()
            .unwrap()
            .snapshot()
            .runs
            .into_iter()
            .filter(|run| run.status.is_active())
            .collect::<Vec<_>>();
        for run in runs {
            let Some(session_id) = run.session_id.as_deref() else {
                continue;
            };
            let Some(slot) = self.acp_sessions.get(session_id) else {
                if !matches!(
                    run.status,
                    AutomationRunStatus::Starting | AutomationRunStatus::Queued
                ) {
                    self.fail_run(&run, "ACP 会话已丢失，结果未知，未自动重放");
                }
                continue;
            };
            let projection = {
                let reduced = slot.value.reduced.lock().unwrap();
                RunProjection {
                    phase: reduced.phase,
                    accepted: reduced.accepted_delivery_ids.contains(&run.id),
                    active: reduced.active_delivery_id.as_deref() == Some(run.id.as_str()),
                    completed: reduced.completed_delivery_id.as_deref() == Some(run.id.as_str()),
                    outcome: reduced.turn_outcome,
                    output: completed_turn_output(&reduced.entries),
                    error: (!reduced.end_reason.is_empty()).then(|| reduced.end_reason.clone()),
                    provider_session_id: reduced
                        .history_session_id
                        .clone()
                        .or_else(|| reduced.acp_session_id.clone()),
                    entries: reduced.entries.clone(),
                }
            };
            if projection.completed {
                self.finish_from_projection(&run, projection);
                continue;
            }
            if projection.phase == DaemonPhase::Dead {
                self.persist_run_transcript(&run.id, &projection.entries);
                self.finish_run(
                    &run,
                    AutomationRunStatus::Failed,
                    None,
                    Some(
                        projection
                            .error
                            .unwrap_or_else(|| "ACP runtime 已退出".to_string()),
                    ),
                    projection.provider_session_id,
                );
                continue;
            }
            if projection.active || projection.accepted {
                let status = match projection.phase {
                    DaemonPhase::AwaitingApproval => AutomationRunStatus::AwaitingApproval,
                    DaemonPhase::WaitingForUser => AutomationRunStatus::WaitingForUser,
                    _ => AutomationRunStatus::Running,
                };
                self.mark_run_status(&run, status, projection.provider_session_id);
            } else if run.status == AutomationRunStatus::Dispatching
                && run
                    .delivery_attempt_at
                    .is_some_and(|attempted| Local::now().timestamp() - attempted >= 5)
            {
                let result = defer_or_fail_delivery(
                    &mut self.automations.lock().unwrap(),
                    &run.id,
                    "投递后未收到 ACP 接收确认",
                    projection.provider_session_id,
                    Local::now().timestamp(),
                );
                match result {
                    Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
                    Err(error) => self.record_error("延后未确认的自动化投递", error),
                }
            }
        }
    }

    fn finish_from_projection(&mut self, run: &AutomationRun, projection: RunProjection) {
        let (status, error) = match projection.outcome {
            Some(AcpTurnOutcome::Succeeded) => (AutomationRunStatus::Completed, None),
            Some(AcpTurnOutcome::Cancelled) => (AutomationRunStatus::Cancelled, None),
            Some(outcome) => (
                AutomationRunStatus::Failed,
                Some(
                    outcome
                        .failure_message()
                        .unwrap_or("回合未正常完成")
                        .to_string(),
                ),
            ),
            None => (
                AutomationRunStatus::Failed,
                Some("ACP 回合结束但没有结果状态".to_string()),
            ),
        };
        self.persist_run_transcript(&run.id, &projection.entries);
        self.finish_run(
            run,
            status,
            (!projection.output.trim().is_empty()).then_some(projection.output),
            error,
            projection.provider_session_id,
        );
    }

    fn persist_run_transcript(&mut self, run_id: &str, entries: &[AcpEntry]) {
        if entries.is_empty() {
            return;
        }
        let result = self
            .automations
            .lock()
            .unwrap()
            .save_run_transcript(run_id, entries);
        if let Err(error) = result {
            self.record_error("保存自动化 Run 对话", error);
        }
    }

    fn mark_run_status(
        &mut self,
        run: &AutomationRun,
        status: AutomationRunStatus,
        provider_session_id: Option<String>,
    ) {
        if run.status == status
            && provider_session_id
                .as_deref()
                .is_none_or(|provider_id| run.provider_session_id.as_deref() == Some(provider_id))
        {
            return;
        }
        let result = self.automations.lock().unwrap().mark_run_status(
            &run.id,
            status,
            provider_session_id,
            Local::now().timestamp(),
        );
        match result {
            Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
            Err(error) => self.record_error("归约自动化 Run 状态", error),
        }
    }

    fn fail_run(&mut self, run: &AutomationRun, error: &str) {
        self.finish_run(
            run,
            AutomationRunStatus::Failed,
            None,
            Some(error.to_string()),
            None,
        );
    }

    fn fail_run_without_session_ownership(&mut self, run: &AutomationRun, error: &str) {
        let now = Local::now().timestamp();
        let finished = self.automations.lock().unwrap().finish_run(
            &run.id,
            AutomationRunStatus::Failed,
            None,
            Some(error.to_string()),
            None,
            now,
        );
        match finished {
            Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
            Err(error) => {
                self.record_error("结束失去 session 所有权的自动化 Run", error);
                return;
            }
        }
        let released = self
            .automations
            .lock()
            .unwrap()
            .mark_runtime_released(&run.id, now);
        match released {
            Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
            Err(error) => self.record_error("记录自动化 Run 不拥有 runtime", error),
        }
    }

    fn finish_run(
        &mut self,
        run: &AutomationRun,
        status: AutomationRunStatus,
        output: Option<String>,
        error: Option<String>,
        provider_session_id: Option<String>,
    ) {
        let result = self.automations.lock().unwrap().finish_run(
            &run.id,
            status,
            output,
            error,
            provider_session_id,
            Local::now().timestamp(),
        );
        match result {
            Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
            Err(error) => self.record_error("结束自动化 Run", error),
        }
    }

    fn start_shell_run(&mut self, run: AutomationRun) {
        let Some((command, args)) = run.context.action.shell_invocation() else {
            self.fail_run(&run, "不是可执行的 Shell 动作");
            return;
        };
        if !self.shell_inflight.insert(run.id.clone()) {
            return;
        }
        self.mark_run_status(&run, AutomationRunStatus::Running, None);
        let still_running =
            self.automations
                .lock()
                .unwrap()
                .snapshot()
                .runs
                .iter()
                .any(|candidate| {
                    candidate.id == run.id && candidate.status == AutomationRunStatus::Running
                });
        if !still_running {
            self.shell_inflight.remove(&run.id);
            return;
        }
        let cwd = run.context.cwd.clone();
        let run_id = run.id.clone();
        let tx = self.shell_tx.clone();
        let mut process = std::process::Command::new(&command);
        process
            .args(&args)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            process.process_group(0);
        }
        let child = {
            // 与 Terminal/ACP spawn 一样拿共享锁：upgrade 清 CLOEXEC 期间不能再 fork。
            let _gate = super::SPAWN_GATE.read().unwrap();
            match process.spawn() {
                Ok(child) => child,
                Err(error) => {
                    self.shell_inflight.remove(&run.id);
                    self.fail_run(&run, &format!("命令启动失败: {error}"));
                    return;
                }
            }
        };
        if let Ok(pid) = i32::try_from(child.id()) {
            self.shell_pgids.insert(run.id.clone(), pid);
        }
        if let Err(error) = std::thread::Builder::new()
            .name("smelt-automation-shell".into())
            .spawn(move || {
                let result = child
                    .wait_with_output()
                    .map_err(|error| format!("命令等待失败: {error}"));
                let _ = tx.send(ShellOutcome { run_id, result });
            })
        {
            self.shell_inflight.remove(&run.id);
            if let Some(pid) = self.shell_pgids.remove(&run.id)
                && pid > 1
            {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            self.fail_run(&run, &format!("无法启动 Shell 执行线程: {error}"));
        }
    }

    fn cancel_shell_runs(&mut self) {
        let cancelled = self
            .automations
            .lock()
            .unwrap()
            .snapshot()
            .runs
            .into_iter()
            .filter(|run| run.status == AutomationRunStatus::Cancelled)
            .map(|run| run.id)
            .collect::<Vec<_>>();
        for run_id in cancelled {
            let Some(pid) = self.shell_pgids.get(&run_id).copied() else {
                continue;
            };
            if pid > 1 {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
    }

    #[cfg(test)]
    fn shell_upgrade_blockers(&self) -> Vec<String> {
        let mut ids = self.shell_inflight.iter().cloned().collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn collect_shell_results(&mut self) {
        while let Ok(outcome) = self.shell_rx.try_recv() {
            self.complete_shell_run(outcome);
        }
    }

    fn complete_shell_run(&mut self, outcome: ShellOutcome) {
        self.shell_inflight.remove(&outcome.run_id);
        self.shell_pgids.remove(&outcome.run_id);
        let Some(run) = self
            .automations
            .lock()
            .unwrap()
            .snapshot()
            .runs
            .into_iter()
            .find(|run| run.id == outcome.run_id)
        else {
            return;
        };
        if run.status.is_terminal() {
            if run.runtime_released_at.is_none() {
                let released = self
                    .automations
                    .lock()
                    .unwrap()
                    .mark_runtime_released(&run.id, Local::now().timestamp());
                match released {
                    Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
                    Err(error) => self.record_error("记录自动化 runtime 已回收", error),
                }
            }
            return;
        }
        match outcome.result {
            Ok(output) => {
                let status = if output.status.success() {
                    AutomationRunStatus::Completed
                } else {
                    AutomationRunStatus::Failed
                };
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let combined_output = if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                };
                let error = if output.status.success() {
                    if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    }
                } else {
                    Some(format!("命令退出码: {:?}\n{stderr}", output.status.code()))
                };
                self.finish_run(&run, status, combined_output, error, None);
            }
            Err(error) => {
                self.finish_run(&run, AutomationRunStatus::Failed, None, Some(error), None);
            }
        }
    }

    #[cfg(test)]
    fn wait_for_shell_idle(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            self.collect_shell_results();
            if self.shell_inflight.is_empty() {
                return;
            }
            if Instant::now() >= deadline {
                panic!("shell run 未在 {timeout:?} 内结束");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn release_terminal_sessions(&mut self) {
        let runs = self
            .automations
            .lock()
            .unwrap()
            .snapshot()
            .runs
            .into_iter()
            .filter(|run| run.status.is_terminal() && run.runtime_released_at.is_none())
            .filter(|run| run.session_id.is_some())
            .collect::<Vec<_>>();
        for run in runs {
            let session_id = run.session_id.as_deref().expect("filtered session id");
            match kill_acp_session(
                session_id,
                &self.acp_sessions,
                &self.remote_sessions,
                &self.event_hub,
            ) {
                Ok(()) => {
                    let result = self
                        .automations
                        .lock()
                        .unwrap()
                        .mark_runtime_released(&run.id, Local::now().timestamp());
                    match result {
                        Ok(snapshot) => publish_automation_projection(&self.event_hub, &snapshot),
                        Err(error) => self.record_error("记录自动化 runtime 已回收", error),
                    }
                }
                Err(error) => self.record_error("回收自动化 ACP runtime", error),
            }
        }
    }

    fn record_error(&mut self, context: &str, error: String) {
        let message = format!("{context}: {error}");
        if self.last_error.as_deref() != Some(message.as_str()) {
            eprintln!("[automation-runtime] {message}");
            self.last_error = Some(message);
        }
    }
}

struct RunProjection {
    phase: DaemonPhase,
    accepted: bool,
    active: bool,
    completed: bool,
    outcome: Option<AcpTurnOutcome>,
    output: String,
    error: Option<String>,
    provider_session_id: Option<String>,
    entries: Vec<AcpEntry>,
}

fn launch_for_automation_run(
    execution: &AgentExecutionDefinition,
) -> smelt_core::agent_kind::ConversationLaunchSpec {
    with_unpersisted_pi_session(execution.launch.clone())
}

fn publish_automation_projection(event_hub: &EventHubHandle, snapshot: &AutomationFile) {
    let snapshot = crate::webhook::annotate_snapshot(snapshot.clone());
    if let Err(error) = event_hub.publish_automations(&snapshot) {
        eprintln!("[automation-runtime] 发布自动化投影失败: {error}");
    }
}

fn ensure_run_conversation_binding(
    session: &super::AcpSession,
    run_id: &str,
) -> Result<bool, String> {
    let mut binding = session.conversation_binding.lock().unwrap();
    if session.agent_session.lock().unwrap().is_some() {
        return Err("自动化 ACP 会话带有不兼容的插件智能体绑定".to_string());
    }
    match binding.as_ref() {
        Some(ConversationBinding::Automation { run_id: bound }) if bound == run_id => Ok(false),
        None | Some(ConversationBinding::Direct) => {
            *binding = Some(ConversationBinding::Automation {
                run_id: run_id.to_string(),
            });
            Ok(true)
        }
        Some(ConversationBinding::Automation { run_id: bound }) => {
            Err(format!("自动化 ACP 会话已属于其他 Run: {bound}"))
        }
        Some(ConversationBinding::Plugin { .. }) => {
            Err("自动化 ACP 会话带有不兼容的插件输入绑定".to_string())
        }
    }
}

fn dispatch_if_still_queued(
    automations: &AutomationStore,
    run: &AutomationRun,
    session: &super::AcpSession,
    provider_session_id: Option<String>,
    event_hub: &EventHubHandle,
) -> Result<bool, String> {
    let mut owner = automations.lock().unwrap();
    let still_queued = owner.snapshot().runs.iter().any(|candidate| {
        candidate.id == run.id
            && candidate.status == AutomationRunStatus::Queued
            && candidate.session_id == run.session_id
    });
    if !still_queued {
        return Ok(false);
    }
    let dispatching = owner
        .mark_run_status(
            &run.id,
            AutomationRunStatus::Dispatching,
            provider_session_id.clone(),
            Local::now().timestamp(),
        )
        .map_err(|error| format!("持久化投递边界失败: {error}"))?;
    publish_automation_projection(event_hub, &dispatching);
    let delivery =
        apply_acp_user_action(session, pi_full_access_action(), event_hub).and_then(|_| {
            let prompt_text = run.context.prompt.clone().unwrap_or_else(|| {
                run.context
                    .trigger_payload
                    .as_ref()
                    .map(smelt_core::automation::format_payload_prompt)
                    .unwrap_or_default()
            });
            apply_acp_user_action(
                session,
                AcpUserAction::Prompt {
                    text: prompt_text,
                    images: Vec::new(),
                    delivery_id: Some(run.id.clone()),
                },
                event_hub,
            )
        });
    if let Err(error) = delivery {
        let snapshot = defer_or_fail_delivery(
            &mut owner,
            &run.id,
            error,
            provider_session_id,
            Local::now().timestamp(),
        )
        .map_err(|rollback| format!("{error}; 记录投递失败状态失败: {rollback}"))?;
        publish_automation_projection(event_hub, &snapshot);
        return Err(error.to_string());
    }
    // The owner lock linearizes cancellation with dispatch. A cancellation committed before
    // this section suppresses delivery; one committed afterward retires the runtime through
    // terminal cleanup. Socket write success is not acceptance; reconciliation still waits for
    // the Run id in the ACP ledger.
    Ok(true)
}

fn delivery_retry_delay_seconds(attempts: u32) -> i64 {
    if attempts == 0 {
        return 0;
    }
    let exponent = attempts.saturating_sub(1).min(5);
    (1_i64 << exponent).min(MAX_DELIVERY_RETRY_SECONDS)
}

fn delivery_attempts_exhausted(attempts: u32) -> bool {
    attempts >= MAX_DELIVERY_ATTEMPTS
}

fn delivery_failure_message(attempts: u32, error: &str) -> String {
    format!("自动化投递在 {attempts} 次尝试后失败: {error}")
}

fn delivery_retry_is_due(run: &AutomationRun, now: i64) -> bool {
    run.delivery_attempt_at.is_none_or(|attempted| {
        now.saturating_sub(attempted) >= delivery_retry_delay_seconds(run.delivery_attempts)
    })
}

fn defer_or_fail_delivery(
    owner: &mut smelt_core::automation_store::AutomationOwner,
    run_id: &str,
    error: &str,
    provider_session_id: Option<String>,
    now: i64,
) -> Result<smelt_core::automation::AutomationFile, String> {
    let attempts = owner
        .snapshot()
        .runs
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| format!("Run 不存在: {run_id}"))?
        .delivery_attempts;
    if delivery_attempts_exhausted(attempts) {
        owner.finish_run(
            run_id,
            AutomationRunStatus::Failed,
            None,
            Some(delivery_failure_message(attempts, error)),
            provider_session_id,
            now,
        )
    } else {
        owner.defer_run_delivery(run_id, now)
    }
}

fn completed_turn_output(entries: &[AcpEntry]) -> String {
    let turn_start = entries
        .iter()
        .rposition(|entry| matches!(entry, AcpEntry::User(_) | AcpEntry::UserWithImages { .. }))
        .unwrap_or(0);
    entries[turn_start..]
        .iter()
        .rev()
        .find_map(|entry| match entry {
            AcpEntry::Assistant {
                text,
                thought: false,
            } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn provider_id(session: &super::AcpSession) -> Option<String> {
    let reduced = session.reduced.lock().unwrap();
    reduced
        .history_session_id
        .clone()
        .or_else(|| reduced.acp_session_id.clone())
}

#[cfg(test)]
mod lifecycle_tests {
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use smelt_core::acp_chat::AcpEntry;
    use smelt_core::acp_session::AcpSessionState;
    use smelt_core::agent_definition::AgentDefinition;
    use smelt_core::agent_kind::ConversationAgentKind;
    use smelt_core::automation::{
        Automation, AutomationCommand, AutomationSchedule, AutomationTrigger,
    };
    use smelt_core::automation_store::AutomationOwner;

    use super::*;
    use crate::{
        RemoteCatalogState, make_acp_session, new_event_hub, new_sessions, new_test_acp_sessions,
    };

    fn dispatching_fixture(accepted: bool) -> (Driver, AutomationStore, AutomationRun) {
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(Automation {
                        id: "automation-1".into(),
                        name: "Morning report".into(),
                        enabled: true,
                        workspace_dir: Some("/tmp".into()),
                        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes {
                            minutes: 30,
                        }),
                        action: smelt_core::automation::AutomationAction::Agent {
                            agent_definition_id: "agent-1".into(),
                            prompt: Some("Summarize the project".into()),
                        },
                        sinks: Vec::new(),
                    }),
                },
                Utc::now(),
            )
            .unwrap();
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Reporter".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Follow reporting rules".into(),
                plugins: Vec::new(),
                context_folders: Vec::new(),
                context_links: Vec::new(),
                ..Default::default()
            },
            kind: ConversationAgentKind::Pi,
            launch: ConversationAgentKind::Pi.default_launch(),
        };
        let run = owner
            .run_once_with_execution("automation-1".into(), Utc::now(), &execution)
            .unwrap()
            .result
            .run()
            .unwrap();
        owner
            .bind_run_session(&run.id, "session-1".into(), Utc::now().timestamp())
            .unwrap();
        owner
            .mark_run_status(
                &run.id,
                AutomationRunStatus::Dispatching,
                None,
                Utc::now().timestamp(),
            )
            .unwrap();
        let automations = Arc::new(Mutex::new(owner));
        let acp_sessions = new_test_acp_sessions();
        let (slot, created) = acp_sessions.reserve_with("session-1", || {
            make_acp_session(
                "session-1",
                Some("/tmp".into()),
                false,
                Some(ConversationBinding::Direct),
                None,
                None,
            )
        });
        assert!(created);
        let mut reduced = AcpSessionState::default();
        reduced.phase = DaemonPhase::Idle;
        if accepted {
            reduced.accepted_delivery_ids.insert(run.id.clone());
            reduced.active_delivery_id = Some(run.id.clone());
        }
        *slot.value.reduced.lock().unwrap() = reduced;
        let driver = Driver::new(
            Arc::clone(&automations),
            new_sessions(),
            acp_sessions,
            Arc::new(Mutex::new(RemoteCatalogState::in_memory())),
            new_event_hub(),
        );
        (driver, automations, run)
    }

    #[test]
    fn automation_pi_launch_uses_no_session() {
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Reporter".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Follow reporting rules".into(),
                plugins: Vec::new(),
                context_folders: Vec::new(),
                context_links: Vec::new(),
                ..Default::default()
            },
            kind: ConversationAgentKind::Pi,
            launch: ConversationAgentKind::Pi.default_launch(),
        };
        let launch = launch_for_automation_run(&execution);
        assert!(
            launch
                .command
                .split_whitespace()
                .any(|token| token == "--no-session"),
            "自动化 Pi 启动应关闭交互 session 落盘: {}",
            launch.command
        );
    }

    #[test]
    fn dispatch_recovery_distinguishes_cold_restart_handoff_and_acknowledgment() {
        let (mut cold, cold_store, _) = dispatching_fixture(false);
        cold.recover_active_runs(false);
        let cold_snapshot = cold_store.lock().unwrap().snapshot();
        let cold_run = &cold_snapshot.runs[0];
        assert_eq!(cold_run.status, AutomationRunStatus::Failed);
        assert!(cold_run.error.as_deref().unwrap().contains("结果未知"));

        let (mut handoff, handoff_store, _) = dispatching_fixture(false);
        handoff.recover_active_runs(true);
        assert_eq!(
            handoff_store.lock().unwrap().snapshot().runs[0].status,
            AutomationRunStatus::Queued
        );

        let (mut acknowledged, acknowledged_store, acknowledged_run) = dispatching_fixture(true);
        acknowledged.recover_active_runs(false);
        assert_eq!(
            acknowledged_store.lock().unwrap().snapshot().runs[0].status,
            AutomationRunStatus::Running
        );
        let acknowledged_session = acknowledged.acp_sessions.get("session-1").unwrap();
        assert!(matches!(
            &*acknowledged_session
                .value
                .conversation_binding
                .lock()
                .unwrap(),
            Some(ConversationBinding::Automation { run_id })
                if run_id == &acknowledged_run.id
        ));

        let (mut conflicting, conflicting_store, _) = dispatching_fixture(false);
        let conflicting_session = conflicting.acp_sessions.get("session-1").unwrap();
        *conflicting_session
            .value
            .conversation_binding
            .lock()
            .unwrap() = Some(ConversationBinding::Automation {
            run_id: "another-run".into(),
        });
        conflicting.recover_active_runs(true);
        let conflict = conflicting_store.lock().unwrap().snapshot();
        assert_eq!(conflict.runs[0].status, AutomationRunStatus::Failed);
        assert!(
            conflict.runs[0]
                .error
                .as_deref()
                .unwrap()
                .contains("其他 Run")
        );
        assert!(conflict.runs[0].runtime_released_at.is_some());
        conflicting.release_terminal_sessions();
        assert!(conflicting.acp_sessions.get("session-1").is_some());
    }

    #[test]
    fn handoff_cannot_requeue_a_run_past_its_delivery_budget() {
        let (mut driver, automations, run) = dispatching_fixture(false);
        {
            let mut owner = automations.lock().unwrap();
            for attempt in 1..MAX_DELIVERY_ATTEMPTS {
                owner
                    .mark_run_status(
                        &run.id,
                        AutomationRunStatus::Queued,
                        None,
                        i64::from(attempt),
                    )
                    .unwrap();
                owner
                    .mark_run_status(
                        &run.id,
                        AutomationRunStatus::Dispatching,
                        None,
                        i64::from(attempt),
                    )
                    .unwrap();
            }
            owner
                .mark_run_status(&run.id, AutomationRunStatus::Queued, None, 10)
                .unwrap();
        }

        driver.dispatch_queued_runs();

        let run = automations
            .lock()
            .unwrap()
            .snapshot()
            .runs
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(run.delivery_attempts, MAX_DELIVERY_ATTEMPTS);
        assert_eq!(run.status, AutomationRunStatus::Failed);
        assert!(run.error.as_deref().unwrap().contains("投递预算已耗尽"));
    }

    #[test]
    fn cancellation_committed_after_a_queue_snapshot_suppresses_delivery() {
        let (_driver, automations, run) = dispatching_fixture(false);
        let stale_queued = {
            let mut owner = automations.lock().unwrap();
            owner
                .mark_run_status(
                    &run.id,
                    AutomationRunStatus::Queued,
                    None,
                    Utc::now().timestamp(),
                )
                .unwrap();
            let queued = owner.snapshot().runs[0].clone();
            owner
                .apply(
                    AutomationCommand::CancelRun {
                        run_id: run.id.clone(),
                    },
                    Utc::now(),
                )
                .unwrap();
            queued
        };
        let session = make_acp_session(
            "session-1",
            Some("/tmp".into()),
            false,
            Some(ConversationBinding::Automation { run_id: run.id }),
            None,
            None,
        );

        let delivered = dispatch_if_still_queued(
            &automations,
            &stale_queued,
            &session,
            None,
            &new_event_hub(),
        )
        .unwrap();

        assert!(!delivered);
        assert_eq!(
            automations.lock().unwrap().snapshot().runs[0].status,
            AutomationRunStatus::Cancelled
        );
    }

    #[test]
    fn accepted_delivery_reconciles_approval_completion_and_runtime_release() {
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(Automation {
                        id: "automation-1".into(),
                        name: "Morning report".into(),
                        enabled: true,
                        workspace_dir: Some("/tmp".into()),
                        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes {
                            minutes: 30,
                        }),
                        action: smelt_core::automation::AutomationAction::Agent {
                            agent_definition_id: "agent-1".into(),
                            prompt: Some("Summarize the project".into()),
                        },
                        sinks: Vec::new(),
                    }),
                },
                Utc::now(),
            )
            .unwrap();
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Reporter".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Follow reporting rules".into(),
                plugins: Vec::new(),
                context_folders: Vec::new(),
                context_links: Vec::new(),
                ..Default::default()
            },
            kind: ConversationAgentKind::Pi,
            launch: ConversationAgentKind::Pi.default_launch(),
        };
        let run = owner
            .run_once_with_execution("automation-1".into(), Utc::now(), &execution)
            .unwrap()
            .result
            .run()
            .unwrap();
        owner
            .bind_run_session(&run.id, "session-1".into(), Utc::now().timestamp())
            .unwrap();
        owner
            .mark_run_status(
                &run.id,
                AutomationRunStatus::Dispatching,
                None,
                Utc::now().timestamp() - 10,
            )
            .unwrap();
        let automations = Arc::new(Mutex::new(owner));
        let acp_sessions = new_test_acp_sessions();
        let (slot, created) = acp_sessions.reserve_with("session-1", || {
            make_acp_session(
                "session-1",
                Some("/tmp".into()),
                false,
                Some(ConversationBinding::Direct),
                None,
                None,
            )
        });
        assert!(created);
        let mut reduced = AcpSessionState::default();
        reduced.phase = DaemonPhase::Idle;
        reduced.history_session_id = Some("provider-session".into());
        *slot.value.reduced.lock().unwrap() = reduced;

        let remote_sessions = Arc::new(Mutex::new(RemoteCatalogState::in_memory()));
        let event_hub = new_event_hub();
        let mut driver = Driver::new(
            Arc::clone(&automations),
            new_sessions(),
            Arc::clone(&acp_sessions),
            remote_sessions,
            Arc::clone(&event_hub),
        );
        driver.reconcile_runs();
        assert_eq!(
            automations.lock().unwrap().snapshot().runs[0].status,
            AutomationRunStatus::Queued,
            "a socket write without ACP acceptance must be retried"
        );
        automations
            .lock()
            .unwrap()
            .mark_run_status(
                &run.id,
                AutomationRunStatus::Dispatching,
                None,
                Utc::now().timestamp(),
            )
            .unwrap();
        {
            let mut reduced = slot.value.reduced.lock().unwrap();
            reduced.phase = DaemonPhase::AwaitingApproval;
            reduced.accepted_delivery_ids.insert(run.id.clone());
            reduced.active_delivery_id = Some(run.id.clone());
        }
        driver.reconcile_runs();
        let awaiting = automations.lock().unwrap().snapshot();
        assert_eq!(
            awaiting.runs[0].status,
            AutomationRunStatus::AwaitingApproval
        );
        assert_eq!(
            awaiting.runs[0].provider_session_id.as_deref(),
            Some("provider-session")
        );
        driver.reconcile_runs();
        assert_eq!(
            automations.lock().unwrap().snapshot().revision,
            awaiting.revision,
            "an unchanged provider projection must not emit another Run revision"
        );
        assert!(
            automations
                .lock()
                .unwrap()
                .run_once_with_execution("automation-1".into(), Utc::now(), &execution)
                .is_err()
        );

        {
            let mut reduced = slot.value.reduced.lock().unwrap();
            reduced.phase = DaemonPhase::Idle;
            reduced.active_delivery_id = None;
            reduced.completed_delivery_id = Some(run.id.clone());
            reduced.turn_outcome = Some(AcpTurnOutcome::Succeeded);
            reduced.entries = vec![
                AcpEntry::User("Summarize the project".into()),
                AcpEntry::Assistant {
                    text: "Report complete".into(),
                    thought: false,
                },
            ];
        }
        driver.reconcile_runs();
        let completed = automations.lock().unwrap().snapshot();
        assert_eq!(completed.runs[0].status, AutomationRunStatus::Completed);
        assert_eq!(completed.runs[0].output.as_deref(), Some("Report complete"));
        assert!(completed.runs[0].runtime_released_at.is_none());

        driver.release_terminal_sessions();
        let released = automations.lock().unwrap().snapshot();
        assert!(released.runs[0].runtime_released_at.is_some());
        assert!(acp_sessions.get("session-1").is_none());
        assert_eq!(
            event_hub.legacy_snapshot().unwrap()["automations"]["revision"],
            released.revision
        );
    }

    #[test]
    fn shell_run_completes_without_an_agent_session() {
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(Automation {
                        id: "automation-1".into(),
                        name: "Health check".into(),
                        enabled: true,
                        workspace_dir: Some("/tmp".into()),
                        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes {
                            minutes: 30,
                        }),
                        action: smelt_core::automation::AutomationAction::shell("true", Vec::new()),
                        sinks: Vec::new(),
                    }),
                },
                Utc::now(),
            )
            .unwrap();
        let automations = Arc::new(Mutex::new(owner));
        automations
            .lock()
            .unwrap()
            .run_once("automation-1".into(), Utc::now())
            .unwrap();
        let mut driver = Driver::new(
            Arc::clone(&automations),
            new_sessions(),
            new_test_acp_sessions(),
            Arc::new(Mutex::new(RemoteCatalogState::in_memory())),
            new_event_hub(),
        );
        driver.launch_unbound_runs();
        driver.wait_for_shell_idle(Duration::from_secs(2));

        let run = automations.lock().unwrap().snapshot().runs.remove(0);
        assert_eq!(run.status, AutomationRunStatus::Completed);
        assert!(run.session_id.is_none());
        assert!(run.runtime_released_at.is_some());
    }

    #[test]
    fn shell_run_does_not_block_the_scheduler_loop() {
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(Automation {
                        id: "automation-1".into(),
                        name: "Slow check".into(),
                        enabled: true,
                        workspace_dir: Some("/tmp".into()),
                        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes {
                            minutes: 30,
                        }),
                        action: smelt_core::automation::AutomationAction::shell(
                            "sleep 0.4",
                            Vec::new(),
                        ),
                        sinks: Vec::new(),
                    }),
                },
                Utc::now(),
            )
            .unwrap();
        let automations = Arc::new(Mutex::new(owner));
        automations
            .lock()
            .unwrap()
            .run_once("automation-1".into(), Utc::now())
            .unwrap();
        let mut driver = Driver::new(
            Arc::clone(&automations),
            new_sessions(),
            new_test_acp_sessions(),
            Arc::new(Mutex::new(RemoteCatalogState::in_memory())),
            new_event_hub(),
        );
        let started = Instant::now();
        driver.launch_unbound_runs();
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "launch_unbound_runs 不应等待 Shell 结束"
        );
        let run = &automations.lock().unwrap().snapshot().runs[0];
        assert_eq!(run.status, AutomationRunStatus::Running);
        assert!(!driver.shell_inflight.is_empty());

        driver.claim_due_runs();
        driver.reconcile_runs();
        driver.wait_for_shell_idle(Duration::from_secs(2));

        let run = automations.lock().unwrap().snapshot().runs.remove(0);
        assert_eq!(run.status, AutomationRunStatus::Completed);
        assert!(run.runtime_released_at.is_some());
    }

    #[test]
    fn cancelling_a_shell_run_kills_its_process_group_before_releasing_runtime() {
        let marker = std::env::temp_dir().join(format!(
            "smelt-cancelled-shell-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(Automation {
                        id: "automation-cancel-shell".into(),
                        name: "Cancelable command".into(),
                        enabled: true,
                        workspace_dir: Some("/tmp".into()),
                        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes {
                            minutes: 30,
                        }),
                        action: smelt_core::automation::AutomationAction::shell(
                            format!("sleep 0.5; printf leaked > {}", marker.display()),
                            Vec::new(),
                        ),
                        sinks: Vec::new(),
                    }),
                },
                Utc::now(),
            )
            .unwrap();
        let automations = Arc::new(Mutex::new(owner));
        let run = automations
            .lock()
            .unwrap()
            .run_once("automation-cancel-shell".into(), Utc::now())
            .unwrap()
            .result
            .run()
            .unwrap();
        let mut driver = Driver::new(
            Arc::clone(&automations),
            new_sessions(),
            new_test_acp_sessions(),
            Arc::new(Mutex::new(RemoteCatalogState::in_memory())),
            new_event_hub(),
        );
        driver.launch_unbound_runs();
        assert!(driver.shell_upgrade_blockers().contains(&run.id));

        automations
            .lock()
            .unwrap()
            .apply(
                AutomationCommand::CancelRun {
                    run_id: run.id.clone(),
                },
                Utc::now(),
            )
            .unwrap();
        assert!(
            automations
                .lock()
                .unwrap()
                .run_once("automation-cancel-shell".into(), Utc::now())
                .is_err(),
            "真实进程回收前不能启动下一次 Run"
        );

        driver.cancel_shell_runs();
        driver.wait_for_shell_idle(Duration::from_secs(2));
        let cancelled = automations.lock().unwrap().snapshot().runs[0].clone();
        assert_eq!(cancelled.status, AutomationRunStatus::Cancelled);
        assert!(cancelled.runtime_released_at.is_some());
        assert!(!driver.shell_upgrade_blockers().contains(&run.id));
        std::thread::sleep(Duration::from_millis(600));
        assert!(!marker.exists(), "被取消的 Shell 进程仍执行了后续副作用");
    }
}

fn pi_full_access_action() -> AcpUserAction {
    AcpUserAction::SetConfigOption {
        config_id: "mode".to_string(),
        value_id: "bypassPermissions".to_string(),
        boolean: None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use smelt_core::agent_definition::AgentDefinition;
    use smelt_core::agent_kind::ConversationAgentKind;
    use smelt_core::automation::{
        Automation, AutomationCommand, AutomationSchedule, AutomationTrigger,
    };
    use smelt_core::automation_store::AutomationOwner;

    use super::*;

    #[test]
    fn delivery_retry_uses_bounded_exponential_backoff() {
        assert_eq!(delivery_retry_delay_seconds(1), 1);
        assert_eq!(delivery_retry_delay_seconds(2), 2);
        assert_eq!(delivery_retry_delay_seconds(3), 4);
        assert_eq!(delivery_retry_delay_seconds(20), 30);
        assert!(!delivery_attempts_exhausted(MAX_DELIVERY_ATTEMPTS - 1));
        assert!(delivery_attempts_exhausted(MAX_DELIVERY_ATTEMPTS));
    }

    #[test]
    fn delivery_retry_exhaustion_finishes_the_run() {
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(Automation {
                        id: "automation-1".into(),
                        name: "Morning report".into(),
                        enabled: true,
                        workspace_dir: Some("/tmp".into()),
                        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes {
                            minutes: 30,
                        }),
                        action: smelt_core::automation::AutomationAction::Agent {
                            agent_definition_id: "agent-1".into(),
                            prompt: Some("Summarize the project".into()),
                        },
                        sinks: Vec::new(),
                    }),
                },
                Utc::now(),
            )
            .unwrap();
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Reporter".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Follow reporting rules".into(),
                plugins: Vec::new(),
                context_folders: Vec::new(),
                context_links: Vec::new(),
                ..Default::default()
            },
            kind: ConversationAgentKind::Pi,
            launch: ConversationAgentKind::Pi.default_launch(),
        };
        let run = owner
            .run_once_with_execution("automation-1".into(), Utc::now(), &execution)
            .unwrap()
            .result
            .run()
            .unwrap();
        owner
            .bind_run_session(&run.id, "session-1".into(), 1)
            .unwrap();

        for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
            owner
                .mark_run_status(
                    &run.id,
                    AutomationRunStatus::Dispatching,
                    None,
                    i64::from(attempt) * 10,
                )
                .unwrap();
            defer_or_fail_delivery(
                &mut owner,
                &run.id,
                "provider unavailable",
                None,
                i64::from(attempt) * 10 + 1,
            )
            .unwrap();
        }

        let run = owner.snapshot().runs.into_iter().next().unwrap();
        assert_eq!(run.delivery_attempts, MAX_DELIVERY_ATTEMPTS);
        assert_eq!(run.status, AutomationRunStatus::Failed);
        assert!(run.error.as_deref().unwrap().contains("5 次尝试后失败"));
    }

    #[test]
    fn automation_permission_is_fixed_full_access() {
        assert!(matches!(
            pi_full_access_action(),
            AcpUserAction::SetConfigOption { config_id, value_id, .. }
                if config_id == "mode" && value_id == "bypassPermissions"
        ));
    }
}
