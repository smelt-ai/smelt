use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::acp_chat::AcpEntry;
use crate::agent_definition_store::{
    AgentExecutionDefinition, agent_space_in, build_automation_run_instructions,
    load_agent_execution_definition,
};
use crate::automation::{
    AUTOMATION_FILE_SCHEMA_VERSION, Automation, AutomationAction, AutomationCommand,
    AutomationCommandResult, AutomationEventPublished, AutomationFile, AutomationInboundEvent,
    AutomationRun, AutomationRunSource, AutomationRunStatus, AutomationState, MAX_RUN_ERROR_BYTES,
    MAX_RUN_OUTPUT_BYTES, apply_automation_command, claim_due_automations, prune_runs,
    publish_automation_event,
};
use crate::automation_transcript::save_run_transcript_on;
use chrono::{DateTime, Duration, Local, Offset, TimeZone};
use sha2::{Digest, Sha256};

const AUTOMATION_DOCUMENT: &str = "automations.json";

pub type AutomationStore = Arc<Mutex<AutomationOwner>>;

#[derive(Clone, Debug)]
pub struct AutomationCommandApplied {
    pub result: AutomationCommandResult,
    pub snapshot: AutomationFile,
    pub changed: bool,
}

#[derive(Clone, Debug)]
pub struct AutomationClaimApplied {
    pub runs: Vec<AutomationRun>,
    pub snapshot: AutomationFile,
    pub changed: bool,
}

#[derive(Clone, Debug)]
pub struct AutomationEventApplied {
    pub published: AutomationEventPublished,
    pub snapshot: AutomationFile,
    pub changed: bool,
}

pub struct AutomationOwner {
    file: AutomationFile,
    path: Option<PathBuf>,
    store: Option<smelt_store::Store>,
    workspaces: Option<WorkspaceRoots>,
    writable: bool,
    persist: bool,
}

impl AutomationOwner {
    pub fn load_default() -> Self {
        Self::load_from_paths(
            automation_file_path(),
            Local::now(),
            local_timezone_fingerprint(),
            true,
        )
    }

    pub fn in_memory() -> Self {
        let file = AutomationFile {
            timezone_fingerprint: "test".to_string(),
            ..Default::default()
        };
        Self {
            file,
            path: None,
            store: None,
            workspaces: None,
            writable: true,
            persist: false,
        }
    }

    fn load_from_paths<Tz>(
        path: Option<PathBuf>,
        now: DateTime<Tz>,
        timezone_fingerprint: String,
        persist: bool,
    ) -> Self
    where
        Tz: TimeZone,
    {
        let Some(storage_path) = path.clone() else {
            return Self {
                file: AutomationFile {
                    store_error: Some("无法确定自动化存储路径".to_string()),
                    ..Default::default()
                },
                path,
                store: None,
                workspaces: None,
                writable: false,
                persist,
            };
        };
        let workspaces = WorkspaceRoots::beside(&storage_path);
        let store = if persist {
            match open_automation_store(&storage_path) {
                Ok(store) => Some(store),
                Err(error) => {
                    return Self {
                        file: AutomationFile {
                            store_error: Some(format!("打开自动化 SQLite 失败: {error}")),
                            ..Default::default()
                        },
                        path,
                        store: None,
                        workspaces,
                        writable: false,
                        persist,
                    };
                }
            }
        } else {
            None
        };

        let loaded = match read_automation_snapshot(store.as_ref(), &storage_path) {
            Ok(result) => result,
            Err(error) => {
                return Self {
                    file: AutomationFile {
                        store_error: Some(error),
                        ..Default::default()
                    },
                    path,
                    store,
                    workspaces,
                    writable: false,
                    persist,
                };
            }
        };

        match loaded {
            Some(mut file) => {
                let before = file.clone();
                let automation_ids = file
                    .automations
                    .iter()
                    .map(|automation| automation.id.clone())
                    .collect::<Vec<_>>();
                let normalized =
                    assign_managed_workspaces(&mut file, workspaces.as_ref(), automation_ids)
                        .and_then(|()| {
                            normalize_loaded_file(&mut file, now, &timezone_fingerprint)
                        });
                match normalized {
                    Ok(()) => {
                        let should_persist = persist && file != before;
                        if should_persist {
                            let Some(next_revision) = file.revision.checked_add(1) else {
                                file.store_error = Some(
                                    "自动化存储 revision 已达上限，无法完成启动迁移".to_string(),
                                );
                                return Self {
                                    file,
                                    path,
                                    store,
                                    workspaces,
                                    writable: false,
                                    persist,
                                };
                            };
                            file.revision = next_revision;
                            if let Some(opened) = &store
                                && let Err(error) = save_automation_document(opened, &file)
                            {
                                file.store_error = Some(format!("更新自动化启动状态失败: {error}"));
                                return Self {
                                    file,
                                    path,
                                    store,
                                    workspaces,
                                    writable: false,
                                    persist,
                                };
                            }
                        }
                        Self {
                            file,
                            path,
                            store,
                            workspaces,
                            writable: true,
                            persist,
                        }
                    }
                    Err(error) => {
                        file.store_error = Some(error);
                        Self {
                            file,
                            path,
                            store,
                            workspaces,
                            writable: false,
                            persist,
                        }
                    }
                }
            }
            None => Self {
                file: AutomationFile {
                    timezone_fingerprint,
                    ..Default::default()
                },
                path,
                store,
                workspaces,
                writable: true,
                persist,
            },
        }
    }

    pub fn snapshot(&self) -> AutomationFile {
        self.file.clone()
    }

    /// 这个会话是不是某次 Run 的执行现场。Run 的会话归运行时管（claim / 投递 / 对账 /
    /// 回收），客户端不能从旁边删掉它。
    pub fn is_run_session(&self, session_id: &str) -> bool {
        self.file
            .runs
            .iter()
            .any(|run| run.session_id.as_deref() == Some(session_id))
    }

    /// 存储被锁死后重新读盘。热路径不以磁盘为准；只有 fail-closed 时才走这条恢复入口。
    pub fn recover_if_locked<Tz>(&mut self, now: DateTime<Tz>, timezone_fingerprint: String) -> bool
    where
        Tz: TimeZone,
    {
        if self.writable && self.file.store_error.is_none() {
            return false;
        }
        if self.path.is_none() {
            return false;
        }
        *self = Self::load_from_paths(self.path.clone(), now, timezone_fingerprint, self.persist);
        self.writable && self.file.store_error.is_none()
    }

    pub fn apply<Tz>(
        &mut self,
        mut command: AutomationCommand,
        now: DateTime<Tz>,
    ) -> Result<AutomationCommandApplied, String>
    where
        Tz: TimeZone,
    {
        if matches!(&command, AutomationCommand::RunOnce { .. }) {
            return Err("RunOnce 必须通过带智能体快照的提交入口".to_string());
        }
        let mut candidate = self.file.clone();
        candidate.store_error = None;
        match &mut command {
            AutomationCommand::Upsert { automation } => {
                assign_upsert_workspace(automation, self.workspaces.as_ref())?;
            }
            AutomationCommand::SetEnabled {
                automation_id,
                enabled: true,
            } => assign_managed_workspaces(
                &mut candidate,
                self.workspaces.as_ref(),
                [automation_id.clone()],
            )?,
            _ => {}
        }
        let result = apply_automation_command(&mut candidate, command, now)?;
        let changed = match &result {
            AutomationCommandResult::Applied(changed) => *changed,
            AutomationCommandResult::Run(_) => true,
        };
        if changed {
            self.commit(candidate)?;
        }
        Ok(AutomationCommandApplied {
            result,
            snapshot: self.file.clone(),
            changed,
        })
    }

    pub fn run_once<Tz>(
        &mut self,
        automation_id: String,
        now: DateTime<Tz>,
    ) -> Result<AutomationCommandApplied, String>
    where
        Tz: TimeZone,
    {
        let automation = self
            .file
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .cloned()
            .ok_or_else(|| "自动化不存在".to_string())?;
        if let Some(run) = self.file.active_run_for(&automation_id) {
            return Err(format!("自动化已有运行中的 Run: {}", run.id));
        }
        let execution = if let Some(agent_definition_id) = automation.agent_definition_id() {
            Some(load_agent_execution_definition(agent_definition_id)?)
        } else {
            None
        };
        self.run_once_with_optional_execution(automation_id, now, execution.as_ref())
    }

    pub fn run_once_with_execution<Tz>(
        &mut self,
        automation_id: String,
        now: DateTime<Tz>,
        execution: &AgentExecutionDefinition,
    ) -> Result<AutomationCommandApplied, String>
    where
        Tz: TimeZone,
    {
        self.run_once_with_optional_execution(automation_id, now, Some(execution))
    }

    pub fn run_once_with_optional_execution<Tz>(
        &mut self,
        automation_id: String,
        now: DateTime<Tz>,
        execution: Option<&AgentExecutionDefinition>,
    ) -> Result<AutomationCommandApplied, String>
    where
        Tz: TimeZone,
    {
        let mut candidate = self.file.clone();
        candidate.store_error = None;
        assign_managed_workspaces(
            &mut candidate,
            self.workspaces.as_ref(),
            [automation_id.clone()],
        )?;
        let result = apply_automation_command(
            &mut candidate,
            AutomationCommand::RunOnce { automation_id },
            now,
        )?;
        let run_id = match &result {
            AutomationCommandResult::Run(run) => run.id.clone(),
            AutomationCommandResult::Applied(_) => unreachable!("RunOnce returns a Run"),
        };
        if let Some(execution) = execution {
            bind_agent_execution(&mut candidate, &run_id, execution, self.workspaces.as_ref())?;
        }
        self.commit(candidate)?;
        let result = AutomationCommandResult::Run(Box::new(
            self.file
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .cloned()
                .expect("committed Run remains in bounded active history"),
        ));
        Ok(AutomationCommandApplied {
            result,
            snapshot: self.file.clone(),
            changed: true,
        })
    }

    pub fn claim_due<Tz>(&mut self, now: DateTime<Tz>) -> Result<AutomationClaimApplied, String>
    where
        Tz: TimeZone,
    {
        let mut candidate = self.file.clone();
        candidate.store_error = None;
        let before = candidate.clone();
        let enabled_ids = candidate
            .automations
            .iter()
            .filter(|automation| automation.enabled)
            .map(|automation| automation.id.clone())
            .collect::<Vec<_>>();
        assign_managed_workspaces(&mut candidate, self.workspaces.as_ref(), enabled_ids)?;
        let claimed = claim_due_automations(&mut candidate, now.clone());
        let mut runs = Vec::new();
        for claimed_run in claimed {
            if let Some(agent_definition_id) = claimed_run.context.action.agent_definition_id() {
                match load_agent_execution_definition(agent_definition_id) {
                    Ok(execution) => {
                        bind_agent_execution(
                            &mut candidate,
                            &claimed_run.id,
                            &execution,
                            self.workspaces.as_ref(),
                        )?;
                        runs.push(
                            candidate
                                .runs
                                .iter()
                                .find(|run| run.id == claimed_run.id)
                                .cloned()
                                .expect("claimed Run remains active"),
                        );
                    }
                    Err(error) => {
                        let run = candidate
                            .runs
                            .iter_mut()
                            .find(|run| run.id == claimed_run.id)
                            .expect("claimed Run remains in candidate");
                        run.status = AutomationRunStatus::Failed;
                        run.finished_at = Some(now.timestamp());
                        run.error = bounded_text(error, MAX_RUN_ERROR_BYTES);
                        run.runtime_released_at = Some(now.timestamp());
                    }
                }
            } else {
                runs.push(
                    candidate
                        .runs
                        .iter()
                        .find(|run| run.id == claimed_run.id)
                        .cloned()
                        .expect("claimed Run remains active"),
                );
            }
        }
        let changed = candidate != before;
        if changed {
            self.commit(candidate)?;
        }
        Ok(AutomationClaimApplied {
            runs,
            snapshot: self.file.clone(),
            changed,
        })
    }

    pub fn publish_event<Tz>(
        &mut self,
        event: AutomationInboundEvent,
        now: DateTime<Tz>,
    ) -> Result<AutomationEventApplied, String>
    where
        Tz: TimeZone,
    {
        let mut candidate = self.file.clone();
        candidate.store_error = None;
        let before = candidate.clone();
        event.validate()?;
        let topic = event.normalized_topic();
        let payload = event.payload.clone();
        let matched_ids = candidate
            .automations
            .iter()
            .filter(|automation| {
                automation.enabled
                    && automation
                        .trigger
                        .matches_inbound_event(&topic, payload.as_ref())
            })
            .map(|automation| automation.id.clone())
            .collect::<Vec<_>>();
        assign_managed_workspaces(&mut candidate, self.workspaces.as_ref(), matched_ids)?;
        let published = publish_automation_event(&mut candidate, &event, now.clone())?;
        for run in &published.runs {
            if run.status != AutomationRunStatus::Starting {
                continue;
            }
            let Some(agent_definition_id) = run.context.action.agent_definition_id() else {
                continue;
            };
            match load_agent_execution_definition(agent_definition_id) {
                Ok(execution) => bind_agent_execution(
                    &mut candidate,
                    &run.id,
                    &execution,
                    self.workspaces.as_ref(),
                )?,
                Err(error) => {
                    let stored = candidate
                        .runs
                        .iter_mut()
                        .find(|candidate_run| candidate_run.id == run.id)
                        .expect("published Run remains in candidate");
                    stored.status = AutomationRunStatus::Failed;
                    stored.finished_at = Some(now.timestamp());
                    stored.error = bounded_text(error, MAX_RUN_ERROR_BYTES);
                    stored.runtime_released_at = Some(now.timestamp());
                }
            }
        }
        let published = AutomationEventPublished {
            event_id: published.event_id,
            topic: published.topic,
            already_recorded: published.already_recorded,
            runs: published
                .runs
                .into_iter()
                .map(|run| {
                    candidate
                        .runs
                        .iter()
                        .find(|candidate_run| candidate_run.id == run.id)
                        .cloned()
                        .expect("published Run remains in candidate")
                })
                .collect(),
        };
        let changed = candidate != before;
        if changed {
            self.commit(candidate)?;
        }
        Ok(AutomationEventApplied {
            published,
            snapshot: self.file.clone(),
            changed,
        })
    }

    pub fn trigger_webhook<Tz>(
        &mut self,
        token: &str,
        payload: Option<serde_json::Value>,
        now: DateTime<Tz>,
    ) -> Result<AutomationCommandApplied, String>
    where
        Tz: TimeZone,
    {
        let automation = self
            .file
            .webhook_automation_for(token)
            .cloned()
            .ok_or_else(|| "外部触发不存在".to_string())?;
        if !automation.enabled {
            return Err("自动化已暂停".to_string());
        }
        let automation_id = automation.id;
        let mut candidate = self.file.clone();
        candidate.store_error = None;
        assign_managed_workspaces(
            &mut candidate,
            self.workspaces.as_ref(),
            [automation_id.clone()],
        )?;
        let result = apply_automation_command(
            &mut candidate,
            AutomationCommand::Trigger {
                automation_id,
                source: AutomationRunSource::Webhook,
                payload,
            },
            now.clone(),
        )?;
        let changed = match &result {
            AutomationCommandResult::Applied(changed) => *changed,
            AutomationCommandResult::Run(_) => true,
        };
        if changed {
            if let AutomationCommandResult::Run(run) = &result
                && let Some(agent_definition_id) = run.context.action.agent_definition_id()
            {
                match load_agent_execution_definition(agent_definition_id) {
                    Ok(execution) => bind_agent_execution(
                        &mut candidate,
                        &run.id,
                        &execution,
                        self.workspaces.as_ref(),
                    )?,
                    Err(error) => {
                        let stored = candidate
                            .runs
                            .iter_mut()
                            .find(|candidate_run| candidate_run.id == run.id)
                            .expect("webhook Run remains in candidate");
                        stored.status = AutomationRunStatus::Failed;
                        stored.finished_at = Some(now.timestamp());
                        stored.error = bounded_text(error, MAX_RUN_ERROR_BYTES);
                        stored.runtime_released_at = Some(now.timestamp());
                    }
                }
            }
            self.commit(candidate)?;
        }
        let result = match result {
            AutomationCommandResult::Run(run) => AutomationCommandResult::Run(Box::new(
                self.file
                    .runs
                    .iter()
                    .find(|candidate| candidate.id == run.id)
                    .cloned()
                    .expect("committed webhook Run remains in history"),
            )),
            other => other,
        };
        Ok(AutomationCommandApplied {
            result,
            snapshot: self.file.clone(),
            changed,
        })
    }

    pub fn bind_run_execution(
        &mut self,
        run_id: &str,
        execution: &AgentExecutionDefinition,
    ) -> Result<AutomationFile, String> {
        let workspaces = self.workspaces.clone();
        self.mutate_run(run_id, |run| {
            if run.status.is_terminal() {
                return Err(format!("Run {} 已结束", run.id));
            }
            if let Some(existing) = run.context.engine_kind_id.as_deref()
                && existing != execution.kind.id()
            {
                return Err("Run 已绑定到不同的智能体执行引擎".to_string());
            }
            bind_run_context(run, execution, workspaces.as_ref());
            Ok(())
        })
    }

    pub fn bind_run_session(
        &mut self,
        run_id: &str,
        session_id: String,
        _now: i64,
    ) -> Result<AutomationFile, String> {
        self.mutate_run(run_id, |run| {
            if run.status != AutomationRunStatus::Starting {
                return Err(format!("Run {} 已经进入 {:?}", run.id, run.status));
            }
            run.status = AutomationRunStatus::Queued;
            run.session_id = Some(session_id);
            Ok(())
        })
    }

    pub fn mark_run_status(
        &mut self,
        run_id: &str,
        status: AutomationRunStatus,
        provider_session_id: Option<String>,
        now: i64,
    ) -> Result<AutomationFile, String> {
        if !status.is_active() || status == AutomationRunStatus::Starting {
            return Err(format!("不能把活跃 Run 归约为 {status:?}"));
        }
        self.mutate_run(run_id, |run| {
            if run.status.is_terminal() {
                return Ok(());
            }
            run.status = status;
            if status == AutomationRunStatus::Dispatching {
                run.delivery_attempt_at = Some(now);
                run.delivery_attempts = run.delivery_attempts.saturating_add(1);
            } else if matches!(
                status,
                AutomationRunStatus::Running
                    | AutomationRunStatus::AwaitingApproval
                    | AutomationRunStatus::WaitingForUser
            ) {
                run.started_at.get_or_insert(now);
            }
            if provider_session_id.is_some() {
                run.provider_session_id = provider_session_id;
            }
            Ok(())
        })
    }

    pub fn defer_run_delivery(&mut self, run_id: &str, now: i64) -> Result<AutomationFile, String> {
        self.mutate_run(run_id, |run| {
            if run.status.is_terminal() {
                return Ok(());
            }
            if run.status != AutomationRunStatus::Dispatching {
                return Err(format!("Run {} 当前不在投递边界", run.id));
            }
            run.status = AutomationRunStatus::Queued;
            run.delivery_attempt_at = Some(now);
            Ok(())
        })
    }

    pub fn finish_run(
        &mut self,
        run_id: &str,
        status: AutomationRunStatus,
        output: Option<String>,
        error: Option<String>,
        provider_session_id: Option<String>,
        now: i64,
    ) -> Result<AutomationFile, String> {
        if !status.is_terminal() || status == AutomationRunStatus::Skipped {
            return Err(format!("不能用 {status:?} 结束 Run"));
        }
        self.mutate_run(run_id, |run| {
            if run.status.is_terminal() {
                return Ok(());
            }
            run.status = status;
            run.finished_at = Some(now);
            run.output = output.and_then(|value| bounded_text(value, MAX_RUN_OUTPUT_BYTES));
            run.error = error.and_then(|value| bounded_text(value, MAX_RUN_ERROR_BYTES));
            if run.session_id.is_none() {
                run.runtime_released_at = Some(now);
            }
            if provider_session_id.is_some() {
                run.provider_session_id = provider_session_id;
            }
            Ok(())
        })
    }

    pub fn mark_runtime_released(
        &mut self,
        run_id: &str,
        now: i64,
    ) -> Result<AutomationFile, String> {
        self.mutate_run(run_id, |run| {
            if !run.status.is_terminal() {
                return Err("仍在运行的 Run 不能释放 runtime".to_string());
            }
            run.runtime_released_at = Some(now);
            Ok(())
        })
    }

    pub fn refresh_schedules<Tz>(&mut self, now: DateTime<Tz>) -> Result<AutomationFile, String>
    where
        Tz: TimeZone,
    {
        let mut candidate = self.file.clone();
        candidate.store_error = None;
        refresh_schedule_states(&mut candidate, now, true);
        candidate.timezone_fingerprint = local_timezone_fingerprint();
        if candidate == self.file {
            return Ok(self.file.clone());
        }
        self.commit(candidate)?;
        Ok(self.file.clone())
    }

    fn mutate_run(
        &mut self,
        run_id: &str,
        mutate: impl FnOnce(&mut AutomationRun) -> Result<(), String>,
    ) -> Result<AutomationFile, String> {
        let mut candidate = self.file.clone();
        candidate.store_error = None;
        let run = candidate
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .ok_or_else(|| format!("Run 不存在: {run_id}"))?;
        let before = run.clone();
        mutate(run)?;
        if *run == before {
            return Ok(self.file.clone());
        }
        prune_runs(&mut candidate);
        self.commit(candidate)?;
        Ok(self.file.clone())
    }

    fn commit(&mut self, mut candidate: AutomationFile) -> Result<(), String> {
        if !self.writable {
            return Err(self
                .file
                .store_error
                .clone()
                .unwrap_or_else(|| "自动化存储当前不可安全写入".to_string()));
        }
        candidate.schema_version = AUTOMATION_FILE_SCHEMA_VERSION;
        candidate.revision = self
            .file
            .revision
            .checked_add(1)
            .ok_or_else(|| "自动化存储 revision 已达上限，拒绝覆盖状态".to_string())?;
        candidate.store_error = None;
        candidate.validate()?;
        candidate.webhook_base_url = None;
        if self.persist {
            let store = self
                .store
                .as_ref()
                .ok_or_else(|| "自动化 SQLite 未打开".to_string())?;
            save_automation_document(store, &candidate)?;
        }
        self.file = candidate;
        Ok(())
    }

    pub fn save_run_transcript(&self, run_id: &str, entries: &[AcpEntry]) -> Result<(), String> {
        let Some(store) = self.transcript_store() else {
            return Ok(());
        };
        save_run_transcript_on(&store, run_id, entries)
    }

    fn transcript_store(&self) -> Option<smelt_store::Store> {
        if !self.persist {
            return None;
        }
        self.store.clone()
    }
}

const AUTOMATION_WORKSPACE_HASH_DOMAIN: &[u8] = b"smelt.automation-workspace.v1\0";

fn automation_workspace_path(root: &Path, automation_id: &str) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(AUTOMATION_WORKSPACE_HASH_DOMAIN);
    digest.update((automation_id.len() as u64).to_be_bytes());
    digest.update(automation_id.as_bytes());
    let encoded = format!("{:x}", digest.finalize());
    root.join(encoded)
}

/// 自动化工作目录的两个根，都从存储所在的 smelt home 推出来。
///
/// 智能体自动化跑在它的 space 下的 `automations/` 里，每条一个子目录：能读到它在
/// 对话里攒下的东西，又不会和别的自动化互相覆盖。Shell 没有智能体，留在托管目录；
/// 那里也是智能体自动化搬进 space 之前的旧位置。
#[derive(Clone, Debug)]
struct WorkspaceRoots {
    managed: PathBuf,
    agent_spaces: PathBuf,
}

impl WorkspaceRoots {
    fn beside(storage_path: &Path) -> Option<Self> {
        let home = storage_path.parent()?;
        Some(Self {
            managed: home.join("workspaces").join("automations"),
            agent_spaces: home.join("agents"),
        })
    }

    fn agent_space(&self, agent_definition_id: &str) -> Option<PathBuf> {
        agent_space_in(&self.agent_spaces, agent_definition_id)
    }

    fn workspace_for(&self, automation: &Automation) -> Result<PathBuf, String> {
        match &automation.action {
            AutomationAction::Agent {
                agent_definition_id,
                ..
            } => {
                let space = self
                    .agent_space(agent_definition_id)
                    .ok_or_else(|| format!("智能体 id 不能用作目录名: {agent_definition_id}"))?;
                Ok(automation_workspace_path(
                    &space.join("automations"),
                    &automation.id,
                ))
            }
            AutomationAction::Shell { .. } => {
                Ok(automation_workspace_path(&self.managed, &automation.id))
            }
        }
    }
}

/// 旧产出跟着搬进 space。搬不动（比如 space 链到了另一块盘）就留在原处、
/// 从空目录开始，不为此拦下整个存储的载入。
fn move_legacy_workspace(legacy: &Path, workspace: &Path) {
    if legacy == workspace || fs::symlink_metadata(workspace).is_ok() {
        return;
    }
    if !fs::symlink_metadata(legacy).is_ok_and(|metadata| metadata.is_dir()) {
        return;
    }
    if let Some(parent) = workspace.parent()
        && fs::create_dir_all(parent).is_ok()
    {
        let _ = fs::rename(legacy, workspace);
    }
}

fn ensure_automation_workspace(
    roots: &WorkspaceRoots,
    automation: &Automation,
) -> Result<String, String> {
    let workspace = roots.workspace_for(automation)?;
    move_legacy_workspace(
        &automation_workspace_path(&roots.managed, &automation.id),
        &workspace,
    );
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "创建自动化 Smelt 工作区失败（{}）: {error}",
            workspace.display()
        )
    })?;
    let metadata = fs::symlink_metadata(&workspace).map_err(|error| {
        format!(
            "读取自动化 Smelt 工作区失败（{}）: {error}",
            workspace.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "自动化 Smelt 工作区不是目录: {}",
            workspace.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "设置自动化 Smelt 工作区权限失败（{}）: {error}",
                workspace.display()
            )
        })?;
    }
    workspace.to_str().map(str::to_owned).ok_or_else(|| {
        format!(
            "自动化 Smelt 工作区路径不是有效 UTF-8: {}",
            workspace.display()
        )
    })
}

fn assign_managed_workspaces(
    file: &mut AutomationFile,
    roots: Option<&WorkspaceRoots>,
    automation_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    let Some(roots) = roots else {
        return Ok(());
    };
    let automation_ids = automation_ids
        .into_iter()
        .filter(|automation_id| {
            file.automations
                .iter()
                .any(|automation| automation.id == automation_id.as_str())
        })
        .collect::<Vec<_>>();
    for automation_id in &automation_ids {
        file.automations
            .iter()
            .find(|automation| automation.id == automation_id.as_str())
            .expect("managed workspace ids were filtered against this file")
            .validate()?;
    }
    let workspaces = automation_ids
        .iter()
        .map(|automation_id| {
            let automation = file
                .automations
                .iter()
                .find(|automation| automation.id == automation_id.as_str())
                .expect("managed workspace ids were filtered against this file");
            ensure_automation_workspace(roots, automation)
                .map(|workspace| (automation_id, workspace))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (automation_id, workspace) in workspaces {
        if let Some(automation) = file
            .automations
            .iter_mut()
            .find(|automation| automation.id == automation_id.as_str())
        {
            automation.workspace_dir = Some(workspace);
        }
    }
    Ok(())
}

fn assign_upsert_workspace(
    automation: &mut Automation,
    roots: Option<&WorkspaceRoots>,
) -> Result<(), String> {
    let Some(roots) = roots else {
        return Ok(());
    };
    automation.validate()?;
    automation.workspace_dir = Some(ensure_automation_workspace(roots, automation)?);
    Ok(())
}

pub fn new_automation_store() -> AutomationStore {
    Arc::new(Mutex::new(AutomationOwner::load_default()))
}

pub fn automation_file_path() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join(AUTOMATION_DOCUMENT))
}

fn open_automation_store(legacy_path: &Path) -> Result<smelt_store::Store, String> {
    let database = legacy_path
        .parent()
        .ok_or_else(|| format!("{} 没有父目录", legacy_path.display()))?
        .join(smelt_store::DATABASE_FILE_NAME);
    crate::sqlite_state::open_sqlite_store(&database)
}

fn read_automation_snapshot(
    store: Option<&smelt_store::Store>,
    leftover_json: &Path,
) -> Result<Option<AutomationFile>, String> {
    if leftover_json.is_file() {
        let _ = fs::remove_file(leftover_json);
    }
    if let Some(store) = store
        && let Some(file) = load_automation_document(store)?
    {
        return Ok(Some(file));
    }
    Ok(None)
}

fn load_automation_document(store: &smelt_store::Store) -> Result<Option<AutomationFile>, String> {
    store
        .get_automation_snapshot()?
        .map(file_from_snapshot)
        .transpose()
}

fn save_automation_document(
    store: &smelt_store::Store,
    file: &AutomationFile,
) -> Result<(), String> {
    store
        .put_automation_snapshot(&snapshot_from_file(file)?)
        .map_err(Into::into)
}

fn snapshot_from_file(file: &AutomationFile) -> Result<smelt_store::AutomationSnapshot, String> {
    Ok(smelt_store::AutomationSnapshot {
        schema_version: file.schema_version,
        store_id: file.store_id.clone(),
        revision: file.revision,
        timezone_fingerprint: file.timezone_fingerprint.clone(),
        automations: file
            .automations
            .iter()
            .map(automation_record)
            .collect::<Result<Vec<_>, _>>()?,
        states: file.states.iter().map(state_record).collect(),
        runs: file
            .runs
            .iter()
            .map(run_record)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn file_from_snapshot(snapshot: smelt_store::AutomationSnapshot) -> Result<AutomationFile, String> {
    let has_retired_sinks = snapshot.schema_version < 2;
    Ok(AutomationFile {
        schema_version: snapshot.schema_version,
        store_id: snapshot.store_id,
        revision: snapshot.revision,
        timezone_fingerprint: snapshot.timezone_fingerprint,
        automations: snapshot
            .automations
            .into_iter()
            .map(|mut record| {
                if has_retired_sinks {
                    record.sinks_json = drop_retired_sinks(&record.sinks_json)?;
                }
                automation_from_record(record)
            })
            .collect::<Result<Vec<_>, _>>()?,
        states: snapshot.states.into_iter().map(state_from_record).collect(),
        runs: snapshot
            .runs
            .into_iter()
            .map(run_from_record)
            .collect::<Result<Vec<_>, _>>()?,
        store_error: None,
        webhook_base_url: None,
    })
}

fn automation_record(
    automation: &crate::automation::Automation,
) -> Result<smelt_store::AutomationRecord, String> {
    Ok(smelt_store::AutomationRecord {
        id: automation.id.clone(),
        name: automation.name.clone(),
        enabled: automation.enabled,
        workspace_dir: automation.workspace_dir.clone(),
        trigger_json: encode_json(&automation.trigger)?,
        action_json: encode_json(&automation.action)?,
        sinks_json: encode_json(&automation.sinks)?,
    })
}

fn automation_from_record(
    record: smelt_store::AutomationRecord,
) -> Result<crate::automation::Automation, String> {
    Ok(crate::automation::Automation {
        id: record.id,
        name: record.name,
        enabled: record.enabled,
        workspace_dir: record.workspace_dir,
        trigger: decode_json(&record.trigger_json, "trigger")?,
        action: decode_json(&record.action_json, "action")?,
        sinks: decode_json(&record.sinks_json, "sinks")?,
    })
}

/// v1 → v2：飞书 / Webhook sink 已从模型删除，剩下为空时回到默认的应用通知。
fn drop_retired_sinks(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut sinks: Vec<serde_json::Value> = decode_json(raw, "sinks")?;
    sinks.retain(|sink| {
        !matches!(
            sink.get("type").and_then(serde_json::Value::as_str),
            Some("feishu" | "webhook")
        )
    });
    encode_json(&sinks)
}

fn state_record(state: &AutomationState) -> smelt_store::AutomationStateRecord {
    smelt_store::AutomationStateRecord {
        automation_id: state.automation_id.clone(),
        next_run_at: state.next_run_at,
        last_run_id: state.last_run_id.clone(),
    }
}

fn state_from_record(record: smelt_store::AutomationStateRecord) -> AutomationState {
    AutomationState {
        automation_id: record.automation_id,
        next_run_at: record.next_run_at,
        last_run_id: record.last_run_id,
    }
}

fn run_record(run: &AutomationRun) -> Result<smelt_store::AutomationRunRecord, String> {
    Ok(smelt_store::AutomationRunRecord {
        id: run.id.clone(),
        automation_id: run.automation_id.clone(),
        source: run_source_name(run.source).to_string(),
        status: run_status_name(run.status).to_string(),
        created_at: run.created_at,
        scheduled_for: run.scheduled_for,
        started_at: run.started_at,
        delivery_attempt_at: run.delivery_attempt_at,
        delivery_attempts: i64::from(run.delivery_attempts),
        finished_at: run.finished_at,
        session_id: run.session_id.clone(),
        provider_session_id: run.provider_session_id.clone(),
        output: run.output.clone(),
        error: run.error.clone(),
        runtime_released_at: run.runtime_released_at,
        context_json: encode_json(&run.context)?,
    })
}

fn run_from_record(record: smelt_store::AutomationRunRecord) -> Result<AutomationRun, String> {
    Ok(AutomationRun {
        id: record.id,
        automation_id: record.automation_id,
        context: decode_json(&record.context_json, "context")?,
        source: run_source_from_name(&record.source)?,
        scheduled_for: record.scheduled_for,
        status: run_status_from_name(&record.status)?,
        created_at: record.created_at,
        started_at: record.started_at,
        delivery_attempt_at: record.delivery_attempt_at,
        delivery_attempts: u32::try_from(record.delivery_attempts.max(0)).unwrap_or(u32::MAX),
        finished_at: record.finished_at,
        session_id: record.session_id,
        provider_session_id: record.provider_session_id,
        output: record.output,
        error: record.error,
        runtime_released_at: record.runtime_released_at,
    })
}

/// 点开 Run 详情时读 SQLite 里的正文；直播投影不带这些字段。
pub fn load_run(run_id: &str) -> Option<AutomationRun> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return None;
    }
    let store = crate::sqlite_state::default_sqlite_store().ok()?;
    let snapshot = store.get_automation_snapshot().ok()??;
    snapshot
        .runs
        .into_iter()
        .find(|run| run.id == run_id)
        .and_then(|record| run_from_record(record).ok())
}

fn run_source_name(source: AutomationRunSource) -> &'static str {
    match source {
        AutomationRunSource::Manual => "manual",
        AutomationRunSource::Scheduled => "scheduled",
        AutomationRunSource::Webhook => "webhook",
        AutomationRunSource::Event => "event",
    }
}

fn run_source_from_name(name: &str) -> Result<AutomationRunSource, String> {
    match name {
        "manual" => Ok(AutomationRunSource::Manual),
        "scheduled" => Ok(AutomationRunSource::Scheduled),
        "webhook" => Ok(AutomationRunSource::Webhook),
        "event" => Ok(AutomationRunSource::Event),
        other => Err(format!("未知 Run source: {other}")),
    }
}

fn run_status_name(status: AutomationRunStatus) -> &'static str {
    match status {
        AutomationRunStatus::Starting => "starting",
        AutomationRunStatus::Queued => "queued",
        AutomationRunStatus::Dispatching => "dispatching",
        AutomationRunStatus::Running => "running",
        AutomationRunStatus::AwaitingApproval => "awaiting_approval",
        AutomationRunStatus::WaitingForUser => "waiting_for_user",
        AutomationRunStatus::Completed => "completed",
        AutomationRunStatus::Failed => "failed",
        AutomationRunStatus::Cancelled => "cancelled",
        AutomationRunStatus::Skipped => "skipped",
    }
}

fn run_status_from_name(name: &str) -> Result<AutomationRunStatus, String> {
    match name {
        "starting" => Ok(AutomationRunStatus::Starting),
        "queued" => Ok(AutomationRunStatus::Queued),
        "dispatching" => Ok(AutomationRunStatus::Dispatching),
        "running" => Ok(AutomationRunStatus::Running),
        "awaiting_approval" => Ok(AutomationRunStatus::AwaitingApproval),
        "waiting_for_user" => Ok(AutomationRunStatus::WaitingForUser),
        "completed" => Ok(AutomationRunStatus::Completed),
        "failed" => Ok(AutomationRunStatus::Failed),
        "cancelled" => Ok(AutomationRunStatus::Cancelled),
        "skipped" => Ok(AutomationRunStatus::Skipped),
        other => Err(format!("未知 Run status: {other}")),
    }
}

fn encode_json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| error.to_string())
}

fn decode_json<T: serde::de::DeserializeOwned>(raw: &[u8], field: &str) -> Result<T, String> {
    serde_json::from_slice(raw).map_err(|error| format!("解析自动化 {field} 失败: {error}"))
}

fn bind_agent_execution(
    file: &mut AutomationFile,
    run_id: &str,
    execution: &AgentExecutionDefinition,
    workspaces: Option<&WorkspaceRoots>,
) -> Result<(), String> {
    let run = file
        .runs
        .iter_mut()
        .find(|run| run.id == run_id)
        .ok_or_else(|| format!("Run 不存在: {run_id}"))?;
    bind_run_context(run, execution, workspaces);
    Ok(())
}

/// 固化这次运行的智能体快照；定义之后再改，不影响已经排上的运行。
fn bind_run_context(
    run: &mut AutomationRun,
    execution: &AgentExecutionDefinition,
    workspaces: Option<&WorkspaceRoots>,
) {
    let space = workspaces.and_then(|roots| roots.agent_space(&execution.definition.id));
    run.context.agent_definition_name = Some(execution.definition.name.clone());
    run.context.engine_kind_id = Some(execution.kind.id().to_string());
    run.context.agent_instructions = Some(build_automation_run_instructions(
        &execution.definition,
        space.as_deref(),
    ));
}

fn refresh_schedule_states<Tz>(file: &mut AutomationFile, now: DateTime<Tz>, timezone_changed: bool)
where
    Tz: TimeZone,
{
    for automation in file.automations.clone() {
        let existing = file
            .state_for(&automation.id)
            .and_then(|state| state.next_run_at);
        let calendar_schedule = automation.trigger.has_calendar_schedule();
        let next_run_at = if !automation.enabled {
            None
        } else if let Some(when) = automation.trigger.next_after(now.clone()) {
            if existing.is_none() || timezone_changed && calendar_schedule {
                Some(when.timestamp())
            } else {
                existing
            }
        } else {
            None
        };
        if let Some(state) = file.state_for_mut(&automation.id) {
            state.next_run_at = next_run_at;
        } else {
            file.states.push(AutomationState {
                automation_id: automation.id,
                next_run_at,
                last_run_id: None,
            });
        }
    }
}

pub fn local_timezone_fingerprint() -> String {
    if let Ok(zone) = iana_time_zone::get_timezone() {
        return format!("iana:{zone}");
    }
    let now = Local::now();
    [0_i64, 90, 180, 270]
        .into_iter()
        .map(|days| {
            let sample = now + Duration::days(days);
            format!(
                "{}:{}",
                sample.offset().fix().local_minus_utc(),
                sample.format("%Z")
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn normalize_loaded_file<Tz>(
    file: &mut AutomationFile,
    now: DateTime<Tz>,
    timezone_fingerprint: &str,
) -> Result<(), String>
where
    Tz: TimeZone,
{
    if file.store_id.trim().is_empty() {
        file.store_id = uuid::Uuid::new_v4().to_string();
    }
    file.validate()?;
    let automation_ids = file
        .automations
        .iter()
        .map(|automation| automation.id.clone())
        .collect::<HashSet<_>>();
    file.states
        .retain(|state| automation_ids.contains(&state.automation_id));
    file.runs
        .retain(|run| automation_ids.contains(&run.automation_id));
    let timezone_changed = file.timezone_fingerprint != timezone_fingerprint;
    refresh_schedule_states(file, now, timezone_changed);
    file.timezone_fingerprint = timezone_fingerprint.to_string();
    file.schema_version = AUTOMATION_FILE_SCHEMA_VERSION;
    file.store_error = None;
    prune_runs(file);
    Ok(())
}

fn bounded_text(mut value: String, max_bytes: usize) -> Option<String> {
    if value.trim().is_empty() {
        return None;
    }
    if value.len() > max_bytes {
        let mut end = max_bytes;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push_str("\n...[truncated]");
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_definition::AgentDefinition;
    use crate::agent_kind::ConversationAgentKind;
    use crate::automation::{
        Automation, AutomationRunContext, AutomationRunSource, AutomationSchedule, AutomationSink,
        AutomationTrigger,
    };
    use chrono::{TimeZone, Utc};
    use chrono_tz::America::New_York;
    use std::path::Path;

    fn automation() -> Automation {
        Automation {
            id: "automation-1".into(),
            name: "Report".into(),
            enabled: true,
            workspace_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes { minutes: 10 }),
            action: crate::automation::AutomationAction::Agent {
                agent_definition_id: "agent-1".into(),
                prompt: Some("Run".into()),
            },
            sinks: Vec::new(),
        }
    }

    fn seed_snapshot(root: &Path, file: &AutomationFile) {
        let store =
            smelt_store::Store::open_or_create(root.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        store
            .put_automation_snapshot(&snapshot_from_file(file).unwrap())
            .unwrap();
    }

    fn persisted_file(root: &Path) -> AutomationFile {
        let store =
            smelt_store::Store::open_or_create(root.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        file_from_snapshot(
            store
                .get_automation_snapshot()
                .unwrap()
                .expect("sqlite 中应有自动化快照"),
        )
        .unwrap()
    }

    #[test]
    fn owner_rolls_back_a_failed_persist() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        let mut owner = AutomationOwner::load_from_paths(
            Some(temp.path().join(AUTOMATION_DOCUMENT)),
            Utc::now(),
            "UTC".to_string(),
            true,
        );
        assert!(owner.snapshot().store_error.is_some());
        let result = owner.apply(
            AutomationCommand::Upsert {
                automation: Box::new(automation()),
            },
            Utc::now(),
        );
        assert!(result.is_err());
        assert!(owner.snapshot().automations.is_empty());
        assert_eq!(owner.snapshot().revision, 0);
    }

    #[test]
    fn unreadable_agent_ui_does_not_block_a_fresh_automation_store() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("agent_ui.json");
        let store_path = temp.path().join("automations.json");
        std::fs::write(&legacy_path, b"{not-json").unwrap();

        let mut owner = AutomationOwner::load_from_paths(
            Some(store_path.clone()),
            Utc::now(),
            "UTC".to_string(),
            true,
        );

        assert!(owner.snapshot().store_error.is_none());
        assert!(owner.snapshot().automations.is_empty());
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation()),
                },
                Utc::now(),
            )
            .unwrap();
        assert!(!store_path.exists());
        assert_eq!(persisted_file(temp.path()).automations.len(), 1);
    }

    #[test]
    fn legacy_agent_ui_automations_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let legacy_path = temp.path().join("agent_ui.json");
        let store_path = temp.path().join("automations.json");
        std::fs::write(
            &legacy_path,
            serde_json::to_vec(&serde_json::json!({
                "notify_success": true,
                "automations": [{
                    "id": "legacy-auto",
                    "agent_id": "agent-1",
                    "name": "Legacy",
                    "enabled": true,
                    "prompt": "Run legacy work",
                    "cwd": "/tmp",
                    "kind": "schedule",
                    "schedule": {"type": "daily", "hour": 9, "minute": 35},
                    "last_run_at": 1_700_000_000,
                    "next_run_at": 1_800_000_000,
                    "last_error": "old failure"
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let owner = AutomationOwner::load_from_paths(
            Some(store_path.clone()),
            Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
            "UTC:0".to_string(),
            true,
        );
        let snapshot = owner.snapshot();

        assert!(snapshot.store_error.is_none());
        assert!(snapshot.automations.is_empty());
        assert!(snapshot.states.is_empty());
        assert!(snapshot.runs.is_empty());
        assert!(!store_path.exists());
    }

    #[test]
    fn missing_store_starts_empty_with_a_generated_identity() {
        let temp = tempfile::tempdir().unwrap();
        let leftover = temp.path().join("automations.json");
        std::fs::write(&leftover, b"{\"schema_version\":1}").unwrap();

        let owner = AutomationOwner::load_from_paths(
            Some(leftover.clone()),
            Utc::now(),
            "UTC".to_string(),
            true,
        );
        let snapshot = owner.snapshot();

        assert!(!leftover.exists());
        assert!(snapshot.store_error.is_none());
        assert!(!snapshot.store_id.is_empty());
        assert_eq!(snapshot.revision, 0);
        assert!(snapshot.automations.is_empty());
        assert!(
            smelt_store::Store::open_or_create(temp.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap()
                .get_automation_snapshot()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn agent_automation_works_inside_its_agent_space() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("automations.json");
        let space = temp.path().join("agents").join("agent-1");
        let workspace_root = space.join("automations");
        let mut owner =
            AutomationOwner::load_from_paths(Some(store_path), Utc::now(), "UTC".to_string(), true);
        let missing_workspace = automation_workspace_path(&workspace_root, "missing");
        assert!(
            owner
                .apply(
                    AutomationCommand::SetEnabled {
                        automation_id: "missing".into(),
                        enabled: true,
                    },
                    Utc::now(),
                )
                .is_err()
        );
        assert!(!missing_workspace.exists());

        let mut definition = automation();
        definition.workspace_dir = Some("/outside/client-controlled".to_string());

        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(definition),
                },
                Utc::now(),
            )
            .unwrap();

        let workspace = owner.snapshot().automations[0]
            .workspace_dir
            .clone()
            .unwrap();
        let workspace = PathBuf::from(workspace);
        assert!(workspace.starts_with(&workspace_root));
        assert!(workspace.is_dir());
        assert_eq!(
            workspace,
            automation_workspace_path(&workspace_root, "automation-1")
        );
        assert!(!workspace.to_string_lossy().contains("client-controlled"));

        std::fs::remove_dir_all(&workspace).unwrap();
        let applied = owner
            .apply(
                AutomationCommand::SetEnabled {
                    automation_id: "automation-1".into(),
                    enabled: true,
                },
                Utc::now(),
            )
            .unwrap();
        assert!(!applied.changed);
        assert!(workspace.is_dir());
        assert_eq!(
            owner.snapshot().automations[0].workspace_dir.as_deref(),
            workspace.to_str()
        );

        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Agent".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Instructions".into(),
                plugins: Vec::new(),
                context_folders: vec!["/work/reports".into()],
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
        assert_eq!(run.context.cwd, workspace.to_str().unwrap());
        let instructions = run.context.agent_instructions.unwrap();
        assert!(instructions.starts_with("Instructions"), "{instructions}");
        assert!(instructions.contains("/work/reports"), "{instructions}");
        assert!(
            instructions.contains(space.to_str().unwrap()),
            "{instructions}"
        );
    }

    #[test]
    fn shell_automation_keeps_a_managed_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = AutomationOwner::load_from_paths(
            Some(temp.path().join("automations.json")),
            Utc::now(),
            "UTC".to_string(),
            true,
        );
        let mut definition = automation();
        definition.action = crate::automation::AutomationAction::shell("true", Vec::new());

        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(definition),
                },
                Utc::now(),
            )
            .unwrap();

        let expected = automation_workspace_path(
            &temp.path().join("workspaces").join("automations"),
            "automation-1",
        );
        assert_eq!(
            owner.snapshot().automations[0].workspace_dir.as_deref(),
            expected.to_str()
        );
        assert!(expected.is_dir());
    }

    #[test]
    fn startup_moves_an_agent_workspace_into_its_agent_space() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = automation_workspace_path(
            &temp.path().join("workspaces").join("automations"),
            "automation-1",
        );
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("report.md"), "yesterday").unwrap();
        let mut file = AutomationFile {
            timezone_fingerprint: "UTC".to_string(),
            ..Default::default()
        };
        let mut definition = automation();
        definition.workspace_dir = legacy.to_str().map(str::to_owned);
        file.automations.push(definition);
        seed_snapshot(temp.path(), &file);

        let owner = AutomationOwner::load_from_paths(
            Some(temp.path().join("automations.json")),
            Utc::now(),
            "UTC".to_string(),
            true,
        );

        let moved = automation_workspace_path(
            &temp
                .path()
                .join("agents")
                .join("agent-1")
                .join("automations"),
            "automation-1",
        );
        assert_eq!(
            owner.snapshot().automations[0].workspace_dir.as_deref(),
            moved.to_str()
        );
        assert_eq!(
            std::fs::read_to_string(moved.join("report.md")).unwrap(),
            "yesterday"
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn startup_replaces_a_legacy_workspace_with_the_managed_path() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("automations.json");
        let workspace_root = temp
            .path()
            .join("agents")
            .join("agent-1")
            .join("automations");
        let mut file = AutomationFile {
            timezone_fingerprint: "UTC".to_string(),
            ..Default::default()
        };
        let mut definition = automation();
        definition.workspace_dir = Some("/legacy/project".to_string());
        file.automations.push(definition);
        seed_snapshot(temp.path(), &file);

        let owner = AutomationOwner::load_from_paths(
            Some(store_path.clone()),
            Utc::now(),
            "UTC".to_string(),
            true,
        );

        let expected = automation_workspace_path(&workspace_root, "automation-1");
        assert_eq!(
            owner.snapshot().automations[0].workspace_dir.as_deref(),
            expected.to_str()
        );
        assert!(expected.is_dir());
        assert!(!store_path.exists());
        let persisted = persisted_file(temp.path());
        assert_eq!(
            persisted.automations[0].workspace_dir.as_deref(),
            expected.to_str()
        );
    }

    #[test]
    fn v1_store_drops_notification_targets_that_never_delivered() {
        let temp = tempfile::tempdir().unwrap();
        let mut file = AutomationFile {
            timezone_fingerprint: "UTC".to_string(),
            ..Default::default()
        };
        for id in ["app-and-feishu", "feishu-only"] {
            let mut definition = automation();
            definition.id = id.to_string();
            file.automations.push(definition);
        }
        let mut snapshot = snapshot_from_file(&file).unwrap();
        snapshot.schema_version = 1;
        snapshot.automations[0].sinks_json =
            br#"[{"type":"local"},{"type":"feishu","chat_id":"oc_1"}]"#.to_vec();
        snapshot.automations[1].sinks_json =
            br#"[{"type":"webhook","url":"https://example.com/hook"},{"type":"feishu","chat_id":"oc_2"}]"#
                .to_vec();
        smelt_store::Store::open_or_create(temp.path().join(smelt_store::DATABASE_FILE_NAME))
            .unwrap()
            .put_automation_snapshot(&snapshot)
            .unwrap();

        let owner = AutomationOwner::load_from_paths(
            Some(temp.path().join("automations.json")),
            Utc::now(),
            "UTC".to_string(),
            true,
        );

        let loaded = owner.snapshot();
        assert_eq!(loaded.store_error, None);
        let sinks = |file: &AutomationFile, id: &str| {
            file.automations
                .iter()
                .find(|automation| automation.id == id)
                .map(|automation| automation.sinks.clone())
                .unwrap()
        };
        assert_eq!(
            sinks(&loaded, "app-and-feishu"),
            vec![AutomationSink::Local]
        );
        assert!(sinks(&loaded, "feishu-only").is_empty());
        let persisted = persisted_file(temp.path());
        assert_eq!(persisted.schema_version, AUTOMATION_FILE_SCHEMA_VERSION);
        assert_eq!(
            sinks(&persisted, "app-and-feishu"),
            vec![AutomationSink::Local]
        );
        assert!(
            persisted
                .automations
                .iter()
                .all(crate::automation::Automation::notifies_app)
        );
    }

    #[cfg(unix)]
    #[test]
    fn owner_rejects_a_symlinked_managed_workspace() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("automations.json");
        let workspace_root = temp
            .path()
            .join("agents")
            .join("agent-1")
            .join("automations");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(
            &outside,
            automation_workspace_path(&workspace_root, "automation-1"),
        )
        .unwrap();
        let mut owner =
            AutomationOwner::load_from_paths(Some(store_path), Utc::now(), "UTC".to_string(), true);

        let error = owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation()),
                },
                Utc::now(),
            )
            .unwrap_err();

        assert!(error.contains("不是目录"));
        assert!(owner.snapshot().automations.is_empty());
    }

    #[test]
    fn startup_recomputes_calendar_cursor_when_timezone_changes() {
        let now = New_York.with_ymd_and_hms(2026, 3, 7, 10, 0, 0).unwrap();
        let mut file = AutomationFile::default();
        let mut daily = automation();
        daily.trigger = AutomationTrigger::schedule(AutomationSchedule::Daily {
            hour: 9,
            minute: 35,
        });
        file.automations.push(daily);
        file.states.push(AutomationState {
            automation_id: "automation-1".into(),
            next_run_at: Some(1),
            last_run_id: None,
        });
        file.timezone_fingerprint = "old-zone".into();

        normalize_loaded_file(&mut file, now, "new-zone").unwrap();

        let next = New_York
            .timestamp_opt(file.states[0].next_run_at.unwrap(), 0)
            .single()
            .unwrap();
        assert_eq!(next.date_naive().to_string(), "2026-03-08");
        assert_eq!(next.time().format("%H:%M").to_string(), "09:35");
        assert_eq!(next.offset().to_string(), "EDT");
    }

    #[test]
    fn timezone_change_does_not_rebase_interval_cursor() {
        let now = New_York.with_ymd_and_hms(2026, 3, 7, 10, 0, 0).unwrap();
        let mut file = AutomationFile::default();
        file.automations.push(automation());
        file.states.push(AutomationState {
            automation_id: "automation-1".into(),
            next_run_at: Some(1_800_000_000),
            last_run_id: None,
        });
        file.timezone_fingerprint = "old-zone".into();

        normalize_loaded_file(&mut file, now, "new-zone").unwrap();

        assert_eq!(file.states[0].next_run_at, Some(1_800_000_000));
    }

    #[test]
    fn concurrent_manual_claims_have_exactly_one_winner() {
        let temp = tempfile::tempdir().unwrap();
        let mut definition = automation();
        definition.workspace_dir = Some(temp.path().to_string_lossy().into_owned());
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(definition),
                },
                Utc::now(),
            )
            .unwrap();
        let owner = std::sync::Arc::new(std::sync::Mutex::new(owner));
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Agent".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Follow the rules".into(),
                plugins: Vec::new(),
                context_folders: Vec::new(),
                context_links: Vec::new(),
                ..Default::default()
            },
            kind: ConversationAgentKind::Pi,
            launch: ConversationAgentKind::Pi.default_launch(),
        };
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let owner = std::sync::Arc::clone(&owner);
            let execution = execution.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                owner.lock().unwrap().run_once_with_execution(
                    "automation-1".into(),
                    Utc::now(),
                    &execution,
                )
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let snapshot = owner.lock().unwrap().snapshot();
        assert_eq!(snapshot.runs.len(), 1);
        assert_eq!(snapshot.runs[0].status, AutomationRunStatus::Starting);
        assert_eq!(
            snapshot.runs[0].context.agent_definition_name.as_deref(),
            Some("Agent")
        );
    }

    #[test]
    fn delivery_attempts_are_persisted_when_a_dispatch_is_deferred() {
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation()),
                },
                Utc::now(),
            )
            .unwrap();
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Agent".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Follow the rules".into(),
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
            .bind_run_session(&run.id, "session-1".into(), 100)
            .unwrap();
        owner
            .mark_run_status(&run.id, AutomationRunStatus::Dispatching, None, 100)
            .unwrap();
        owner.defer_run_delivery(&run.id, 101).unwrap();

        let snapshot = owner.snapshot();
        let run = &snapshot.runs[0];
        assert_eq!(run.status, AutomationRunStatus::Queued);
        assert_eq!(run.delivery_attempts, 1);
        assert_eq!(run.delivery_attempt_at, Some(101));
    }

    #[test]
    fn terminal_history_is_bounded_per_automation() {
        let mut file = AutomationFile::default();
        file.automations.push(automation());
        for index in 0..(crate::automation::MAX_RUNS_PER_AUTOMATION + 10) {
            file.runs.push(AutomationRun {
                id: format!("run-{index}"),
                automation_id: "automation-1".into(),
                context: AutomationRunContext {
                    automation_name: "Report".into(),
                    action: crate::automation::AutomationAction::Agent {
                        agent_definition_id: "agent-1".into(),
                        prompt: Some("Run".into()),
                    },
                    cwd: "/tmp".into(),
                    trigger_payload: None,
                    prompt: Some("Run".into()),
                    agent_definition_name: Some("Agent".into()),
                    engine_kind_id: Some("pi".into()),
                    agent_instructions: Some("Be useful".into()),
                },
                source: AutomationRunSource::Manual,
                scheduled_for: None,
                status: AutomationRunStatus::Completed,
                created_at: index as i64,
                started_at: Some(index as i64),
                delivery_attempt_at: Some(index as i64),
                delivery_attempts: 1,
                finished_at: Some(index as i64),
                session_id: None,
                provider_session_id: None,
                output: None,
                error: None,
                runtime_released_at: Some(index as i64),
            });
        }
        prune_runs(&mut file);
        assert_eq!(file.runs.len(), crate::automation::MAX_RUNS_PER_AUTOMATION);
        assert!(file.runs.iter().all(|run| run.created_at >= 10));
    }

    #[test]
    fn leftover_automations_json_does_not_seed_the_sqlite_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("automations.json");
        std::fs::write(
            &store_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "store_id": "store-1",
                "revision": 3,
                "timezone_fingerprint": "UTC",
                "automations": [{
                    "id": "automation-1",
                    "agent_id": "agent-1",
                    "name": "sss",
                    "enabled": true,
                    "prompt": "dddd",
                    "workspace_dir": "/tmp",
                    "permission_mode": "full_access",
                    "kind": "schedule",
                    "schedule": {"type": "every_minutes", "minutes": 15}
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let owner = AutomationOwner::load_from_paths(
            Some(store_path.clone()),
            Utc::now(),
            "UTC".to_string(),
            true,
        );
        let snapshot = owner.snapshot();

        assert!(snapshot.store_error.is_none());
        assert!(snapshot.automations.is_empty());
        assert!(!store_path.exists());
        assert!(
            smelt_store::Store::open_or_create(temp.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap()
                .get_automation_snapshot()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn leftover_automations_json_is_dropped_without_import() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("automations.json");
        std::fs::write(&store_path, b"{not-json").unwrap();

        let mut owner = AutomationOwner::load_from_paths(
            Some(store_path.clone()),
            Utc::now(),
            "UTC".to_string(),
            true,
        );

        assert!(owner.snapshot().store_error.is_none());
        assert!(owner.snapshot().automations.is_empty());
        assert!(!store_path.exists());

        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation()),
                },
                Utc::now(),
            )
            .unwrap();
        assert_eq!(owner.snapshot().automations[0].id, "automation-1");
    }

    #[test]
    fn recover_if_locked_reloads_after_the_store_becomes_openable() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("automations.json");
        let database = temp.path().join(smelt_store::DATABASE_FILE_NAME);
        std::fs::create_dir(&database).unwrap();

        let mut owner =
            AutomationOwner::load_from_paths(Some(store_path), Utc::now(), "UTC".to_string(), true);
        assert!(owner.snapshot().store_error.is_some());
        assert!(
            owner
                .apply(
                    AutomationCommand::Upsert {
                        automation: Box::new(automation()),
                    },
                    Utc::now(),
                )
                .is_err()
        );

        std::fs::remove_dir(&database).unwrap();
        assert!(owner.recover_if_locked(Utc::now(), "UTC".to_string()));
        assert!(owner.snapshot().store_error.is_none());
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation()),
                },
                Utc::now(),
            )
            .unwrap();
        assert_eq!(owner.snapshot().automations.len(), 1);
        assert_eq!(persisted_file(temp.path()).automations.len(), 1);
    }

    #[test]
    fn persisted_runs_keep_transcripts_in_sqlite_and_drop_them_with_the_automation() {
        let temp = tempfile::tempdir().unwrap();
        let store_path = temp.path().join("automations.json");
        let mut owner = AutomationOwner::load_from_paths(
            Some(store_path.clone()),
            Utc::now(),
            "UTC".to_string(),
            true,
        );
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation()),
                },
                Utc::now(),
            )
            .unwrap();
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Agent".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Instructions".into(),
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
            .save_run_transcript(&run.id, &[AcpEntry::User("完整对话".into())])
            .unwrap();
        assert!(!store_path.exists());
        let store =
            smelt_store::Store::open_or_create(temp.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        assert!(crate::automation_transcript::load_run_transcript_on(&store, &run.id).is_some());

        owner
            .finish_run(
                &run.id,
                AutomationRunStatus::Completed,
                Some("ok".into()),
                None,
                None,
                Utc::now().timestamp(),
            )
            .unwrap();
        owner
            .apply(
                AutomationCommand::Delete {
                    automation_id: "automation-1".into(),
                },
                Utc::now(),
            )
            .unwrap();
        let store =
            smelt_store::Store::open_or_create(temp.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        assert!(crate::automation_transcript::load_run_transcript_on(&store, &run.id).is_none());
        assert!(!store_path.exists());
    }

    #[test]
    fn in_memory_owner_does_not_write_transcripts() {
        let mut owner = AutomationOwner::in_memory();
        owner
            .apply(
                AutomationCommand::Upsert {
                    automation: Box::new(automation()),
                },
                Utc::now(),
            )
            .unwrap();
        let execution = AgentExecutionDefinition {
            definition: AgentDefinition {
                id: "agent-1".into(),
                name: "Agent".into(),
                description: String::new(),
                engine_kind_id: ConversationAgentKind::Pi.id().into(),
                prompt: "Instructions".into(),
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
            .save_run_transcript(&run.id, &[AcpEntry::User("should not persist".into())])
            .unwrap();
        assert!(owner.transcript_store().is_none());
    }
}
