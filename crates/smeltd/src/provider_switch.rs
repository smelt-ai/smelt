//! provider 切换的运行时边界。应用 sid 不变，归约器仍只持有当前原生会话。
//! 已结束的历史段按原生存档引用恢复，出线时才组合成应用时间线。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use smelt_core::acp_chat::AcpEntry;
use smelt_core::acp_session::{AcpUserAction, ConversationSnapshot};
use smelt_core::agent_kind::ConversationLaunchSpec;
use smelt_core::provider_switch::{
    ProviderSwitchParams, SWITCH_CONTEXT_MAX_CHARS, provider_key, switch_context, without_handoff,
};
use smelt_core::workspace_directory::WorkspaceAcp;
use smelt_store::{
    ContextHandoffRecord, ProviderHistory, ProviderSegmentRecord, ProviderThreadRecord, Store,
    WorkspaceAcpRecord,
};

use crate::acp_host::{AcpSession, AcpSessions, acp_relaunch, retire_acp_runtime};
use crate::{EventHubHandle, Sessions};

#[derive(Clone)]
pub(crate) struct Timeline {
    pub active: WorkspaceAcp,
    history: ProviderHistory,
    prefix: Vec<AcpEntry>,
    context: Vec<(String, String)>,
    skip_users: usize,
    base: Option<usize>,
    delivery: Option<ContextHandoffRecord>,
    store: Store,
}

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn active_record(store: &Store, sid: &str) -> Result<WorkspaceAcp, String> {
    let (_, directory) = store.workspace_directory().map_err(message)?;
    let a = directory
        .sessions
        .into_iter()
        .filter_map(|s| s.acp)
        .find(|a| a.sid.as_deref() == Some(sid))
        .ok_or_else(|| "The conversation isn't in the workspace directory".to_string())?;
    Ok(WorkspaceAcp {
        sid: sid.into(),
        cwd: a.cwd,
        agent: a.agent,
        profile_id: a.profile_id,
        agent_definition_id: a.agent_definition_id,
        history_session_id: a.history_session_id,
        launch: ConversationLaunchSpec {
            command: a.launch_command,
            env: a.launch_env.into_iter().collect(),
        },
        config_values: a.config_values,
        pending_prompt: a.pending_prompt,
        pending_agent_preset: a.pending_agent_preset,
        session_title: a.session_title,
        automation_id: a.automation_id,
    })
}

pub(crate) fn authoritative_open(store: &Store, sid: &str) -> Result<Option<WorkspaceAcp>, String> {
    if store
        .provider_history(sid)
        .map_err(message)?
        .threads
        .is_empty()
    {
        Ok(None)
    } else {
        active_record(store, sid).map(Some)
    }
}

pub(crate) fn prepare_open(
    mut request: crate::acp_host::AcpOpenRequest,
    store: &Store,
) -> Result<crate::acp_host::AcpOpenRequest, String> {
    if let Some(active) = authoritative_open(store, &request.id)? {
        let same_provider = request.provider_key.as_deref() == Some(provider_key(&active).as_str());
        if !same_provider {
            request.launch = active.launch;
            request.ephemeral_env.clear();
        }
        request.cwd = active.cwd;
        request.resume_id = active.history_session_id;
        request.initial_config = active.config_values;
        request.pending_agent_preset = active.pending_agent_preset;
        request.initial_prompt = None;
        request.fork_id = None;
        request.fork_cut = None;
    }
    Ok(request)
}

fn thread(active: &WorkspaceAcp) -> ProviderThreadRecord {
    ProviderThreadRecord {
        provider_key: provider_key(active),
        agent: active.agent.clone(),
        profile_id: active.profile_id.clone(),
        agent_definition_id: active.agent_definition_id.clone(),
        history_session_id: active.history_session_id.clone(),
        covered_segment: 0,
    }
}

fn native_turns(
    active: &WorkspaceAcp,
    native_id: &str,
) -> Result<Vec<smelt_runtime_client::protocol::history::HistoryTurn>, String> {
    let cwd = active
        .cwd
        .as_deref()
        .ok_or_else(|| "The conversation has no working directory".to_string())?;
    let sessions = smelt_core::history_service::list(
        &smelt_runtime_client::protocol::history::HistoryListParams {
            agent: active.agent.clone(),
            profile_id: active.profile_id.clone(),
            cwd: cwd.into(),
        },
    )
    .map_err(message)?;
    let source = sessions
        .sessions
        .into_iter()
        .find(|s| s.resume_id == native_id)
        .ok_or_else(|| {
            format!(
                "The {} history {native_id} is unavailable; switching would lose context",
                active.agent
            )
        })?;
    smelt_core::history_service::detail(&active.agent, &source.path)
        .map_err(message)?
        .map(|detail| detail.turns)
        .ok_or_else(|| "The provider history couldn't be read".into())
}

fn assemble(
    active: WorkspaceAcp,
    history: ProviderHistory,
    store: Store,
    mut read: impl FnMut(
        &WorkspaceAcp,
        &str,
    )
        -> Result<Vec<smelt_runtime_client::protocol::history::HistoryTurn>, String>,
) -> Result<Timeline, String> {
    let key = provider_key(&active);
    let covered = history
        .threads
        .iter()
        .find(|t| t.provider_key == key)
        .map_or(0, |t| t.covered_segment);
    let mut prefix = Vec::new();
    let mut context = Vec::new();
    let mut skip_users = 0;
    // 每份原生存档在本次装配中只读一次。
    let mut cache = std::collections::HashMap::new();
    for segment in &history.segments {
        let source = history
            .threads
            .iter()
            .find(|t| t.provider_key == segment.provider_key)
            .ok_or_else(|| "A provider segment has no source identity".to_string())?;
        let source_acp = WorkspaceAcp {
            agent: source.agent.clone(),
            profile_id: source.profile_id.clone(),
            cwd: active.cwd.clone(),
            ..Default::default()
        };
        let turns = match cache.entry((
            segment.provider_key.clone(),
            segment.history_session_id.clone(),
        )) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(read(&source_acp, &segment.history_session_id)?)
            }
        };
        let first = usize::try_from(segment.first_turn).map_err(message)?;
        let end = usize::try_from(segment.end_turn).map_err(message)?;
        let range = turns.get(first..end).ok_or_else(|| {
            "The provider history no longer contains the recorded range".to_string()
        })?;
        prefix.push(AcpEntry::Divider(source.agent.clone()));
        for turn in range {
            let text = without_handoff(&turn.text).to_string();
            if segment.position >= covered && segment.provider_key != key {
                context.push((
                    format!(
                        "{} / {} / segment {} / {}",
                        source.agent,
                        segment.history_session_id,
                        segment.position,
                        if turn.is_user { "user" } else { "assistant" }
                    ),
                    text.clone(),
                ));
            }
            prefix.push(if turn.is_user {
                AcpEntry::User(text)
            } else {
                AcpEntry::Assistant {
                    text,
                    thought: false,
                }
            });
        }
        if segment.provider_key == key
            && active.history_session_id.as_deref() == Some(&segment.history_session_id)
        {
            skip_users = turns[..end].iter().filter(|turn| turn.is_user).count();
        }
    }
    Ok(Timeline {
        active,
        history,
        prefix,
        context,
        skip_users,
        base: None,
        delivery: None,
        store,
    })
}

pub(crate) fn restore(sess: &AcpSession, store: &Store, sid: &str) -> Result<(), String> {
    let history = store.provider_history(sid).map_err(message)?;
    if history.threads.is_empty() {
        return Ok(());
    }
    let active = active_record(store, sid)?;
    let timeline = assemble(active, history, store.clone(), native_turns)?;
    *sess.provider_timeline.lock() = Some(timeline);
    Ok(())
}

pub(crate) fn reset(sess: &AcpSession) {
    if let Some(timeline) = &mut *sess.provider_timeline.lock() {
        timeline.base = None;
    }
}

/// 只在原生重放就绪后确定切片下标。原生一条回复可能有很多 UI entry，按用户轮次定位。
pub(crate) fn ready(sess: &AcpSession) {
    let mut reduced = sess.reduced.lock();
    if !matches!(crate::acp_idle::acp_ready_outcome(&reduced), Some(Ok(()))) {
        return;
    }
    if let Some(timeline) = &mut *sess.provider_timeline.lock()
        && timeline.base.is_none()
    {
        if reduced
            .entries()
            .iter()
            .filter(|entry| matches!(entry, AcpEntry::User(_) | AcpEntry::UserWithImages { .. }))
            .count()
            < timeline.skip_users
        {
            smelt_core::acp_session::force_end(
                &mut reduced,
                smelt_core::acp_session::AcpEndKind::RestoreFailed,
                "The native replay is shorter than the recorded provider history",
            );
            return;
        }
        timeline.base = Some(
            reduced
                .entries()
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e, AcpEntry::User(_) | AcpEntry::UserWithImages { .. }))
                .nth(timeline.skip_users)
                .map_or(reduced.entries_total(), |(i, _)| i + reduced.entries_base()),
        );
    }
}

pub(crate) fn project(sess: &AcpSession, snapshot: &mut ConversationSnapshot) {
    let timeline = sess.provider_timeline.lock();
    let Some(t) = &*timeline else {
        return;
    };
    snapshot.supports_rewind = false;
    snapshot.supports_compaction = false;
    if let Some(base) = t.base {
        let offset = snapshot.entries_offset;
        let trim = base.saturating_sub(offset).min(snapshot.entries.len());
        snapshot.entries.drain(..trim);
        if offset <= base {
            let mut prefix = t.prefix.clone();
            prefix.append(&mut snapshot.entries);
            snapshot.entries = prefix;
            snapshot.entries_offset = 0;
        } else {
            snapshot.entries_offset = t.prefix.len() + offset - base;
        }
        snapshot.entries_total = t.prefix.len() + snapshot.entries_total.saturating_sub(base);
        snapshot.turn_timings.retain(|t| t.user_index >= base);
        for timing in &mut snapshot.turn_timings {
            timing.user_index = t.prefix.len() + timing.user_index - base;
        }
    } else {
        snapshot.entries = t.prefix.clone();
        snapshot.entries_offset = 0;
        snapshot.entries_total = t.prefix.len();
    }
    for entry in &mut snapshot.entries {
        match entry {
            AcpEntry::User(text) | AcpEntry::UserWithImages { text, .. } => {
                *text = without_handoff(text).to_string()
            }
            _ => {}
        }
    }
}

pub(crate) fn project_live(sess: &AcpSession, live: &mut smelt_core::acp_session::SessionLive) {
    if let Some(t) = &*sess.provider_timeline.lock() {
        live.history_len = live
            .history_len
            .map(|len| t.prefix.len() + t.base.map_or(0, |base| len.saturating_sub(base)));
    }
}

/// watch 的游标属于应用时间线；归约器的游标只属于当前 provider。
pub(crate) fn watch_offset(
    sess: &AcpSession,
    native_total: usize,
    known: Option<usize>,
    tail: Option<usize>,
) -> (usize, usize) {
    let timeline = sess.provider_timeline.lock();
    let (prefix, base) = timeline
        .as_ref()
        .map_or((0, 0), |t| (t.prefix.len(), t.base.unwrap_or(native_total)));
    let total = prefix + native_total.saturating_sub(base);
    let wanted = known
        .map(|n| n.min(total))
        .or_else(|| tail.map(|n| total.saturating_sub(n)))
        .unwrap_or(0);
    (
        wanted,
        (base + wanted.saturating_sub(prefix)).min(native_total),
    )
}

pub(crate) fn reject_action(sess: &AcpSession, action: &AcpUserAction) -> Option<&'static str> {
    if sess.provider_switching.load(Ordering::SeqCst)
        && !matches!(
            action,
            AcpUserAction::Refresh | AcpUserAction::SetConfigOption { .. }
        )
    {
        return Some("A provider switch is in progress");
    }
    if sess.provider_timeline.lock().is_some()
        && matches!(
            action,
            AcpUserAction::RewindToMessage { .. }
                | AcpUserAction::RetryLastTurn
                | AcpUserAction::Compact
        )
    {
        return Some(
            "Rewind and manual compaction aren't supported for mixed-provider conversations",
        );
    }
    None
}

/// 在越过命令通道前持久化不确定性。只装饰本次用户请求，不自动重发旧请求。
pub(crate) fn prompt(sess: &AcpSession, text: &str) -> Result<String, &'static str> {
    let native = {
        let reduced = sess.reduced.lock();
        reduced
            .history_session_id
            .clone()
            .or_else(|| reduced.acp_session_id.clone())
    };
    let mut timeline = sess.provider_timeline.lock();
    let Some(t) = &mut *timeline else {
        return Ok(text.into());
    };
    if t.context.is_empty() {
        return Ok(text.into());
    }
    if text.starts_with('/') {
        return Err(
            "Send a regular message to deliver the provider handoff before using slash commands",
        );
    }
    if t.delivery.is_none() {
        t.delivery = Some(ContextHandoffRecord {
            delivery_id: uuid::Uuid::new_v4().to_string(),
            provider_key: provider_key(&t.active),
            history_session_id: native,
            through_segment: t.history.segments.len() as i64,
            status: "unknown".into(),
        });
    }
    let delivery = t.delivery.as_ref().ok_or("Missing handoff identity")?;
    if delivery.history_session_id.is_none() {
        return Err("The target provider hasn't confirmed a native session identity");
    }
    t.store
        .remember_context_handoff(&t.active.sid, delivery)
        .map_err(|_| "Couldn't persist the context handoff; request wasn't sent")?;
    let context = switch_context(&t.context, SWITCH_CONTEXT_MAX_CHARS);
    if context.is_empty() {
        return Err("The provider handoff doesn't fit the context budget");
    }
    Ok(format!("{context}{text}"))
}

pub(crate) fn not_sent(sess: &AcpSession) {
    if let Some(t) = &mut *sess.provider_timeline.lock()
        && let Some(mut delivery) = t.delivery.take()
    {
        delivery.status = "prepared".into();
        let _ = t.store.remember_context_handoff(&t.active.sid, &delivery);
    }
}

pub(crate) fn acknowledge(sess: &AcpSession) {
    let mut timeline = sess.provider_timeline.lock();
    let Some(t) = &mut *timeline else {
        return;
    };
    let Some(mut delivery) = t.delivery.clone() else {
        return;
    };
    delivery.status = "delivered".into();
    if let Some(id) = &delivery.history_session_id {
        if t.store
            .remember_history_session_id(&t.active.sid, id)
            .is_err()
        {
            return;
        }
        t.active.history_session_id = Some(id.clone());
    }
    if t.store
        .remember_context_handoff(&t.active.sid, &delivery)
        .is_ok()
    {
        if let Some(thread) = t
            .history
            .threads
            .iter_mut()
            .find(|p| p.provider_key == delivery.provider_key)
        {
            thread.covered_segment = delivery.through_segment;
        }
        t.context.clear();
        t.delivery = None;
    }
}

fn busy(sess: &AcpSession) -> Option<&'static str> {
    crate::acp_idle::reduced_suspend_blocker(&sess.reduced.lock()).or_else(|| {
        (sess.prompt_in_flight.load(Ordering::SeqCst)
            || sess.startup_hold.load(Ordering::SeqCst)
            || !sess.pending_prompts.lock().is_empty()
            || sess.unaccepted_prompt.lock().is_some())
        .then_some("pending input")
    })
}

pub(crate) fn switch(
    params: ProviderSwitchParams,
    _sessions: &Sessions,
    sessions: &AcpSessions,
    events: &EventHubHandle,
) -> Result<WorkspaceAcp, String> {
    let store = crate::workspace_directory::default_store().map_err(|error| error.message)?;
    let option = smelt_core::session_control::find_agent_option(&params.agent_option_id)
        .ok_or_else(|| "The target provider is unavailable".to_string())?;
    switch_with(params, &store, option, sessions, events, native_turns)
}

fn switch_with(
    params: ProviderSwitchParams,
    store: &Store,
    option: smelt_core::session_control::AcpAgentOption,
    sessions: &AcpSessions,
    events: &EventHubHandle,
    mut read: impl FnMut(
        &WorkspaceAcp,
        &str,
    )
        -> Result<Vec<smelt_runtime_client::protocol::history::HistoryTurn>, String>,
) -> Result<WorkspaceAcp, String> {
    let slot = sessions
        .get(&params.session_id)
        .ok_or_else(|| "The conversation is no longer running".to_string())?;
    let _lifecycle = slot.lifecycle.lock();
    let sess = &slot.value;
    let _input = sess.conversation_submit.lock();
    let mut source = active_record(store, &params.session_id)?;
    if provider_key(&source) != params.expected_provider {
        return Err("The active provider changed; refresh before switching".into());
    }
    if params.expected_provider == option.id {
        return Ok(source);
    }
    if smelt_core::agent_kind::HistorySourceKind::from_id(&option.kind).is_none() {
        return Err("The target provider doesn't have a portable history reader".into());
    }
    if source.automation_id.is_some()
        || !matches!(
            &*sess.conversation_binding.lock(),
            None | Some(smelt_core::conversation::ConversationBinding::Direct)
        )
        || !sess.disallowed_tools.lock().is_empty()
    {
        return Err("Managed or tool-restricted conversations can't switch providers".into());
    }
    {
        let _turn = sess.turn_completion.lock();
        if let Some(reason) = busy(sess) {
            return Err(format!(
                "Wait for the conversation to finish before switching: {reason}"
            ));
        }
        sess.provider_switching.store(true, Ordering::SeqCst);
    }
    let original_resumes = crate::acp_host::live_acp_owner_ids(&sess.reduced.lock());
    let source_ephemeral = sess.ephemeral_env.lock().clone();
    let accepted_deliveries = sess.reduced.lock().accepted_delivery_ids.clone();
    let result = (|| {
        source.history_session_id = crate::acp_host::known_acp_resume_id(&sess.reduced.lock())
            .or(source.history_session_id.clone());
        let mut history = store.provider_history(&source.sid).map_err(message)?;
        let source_key = provider_key(&source);
        let previous = history
            .threads
            .iter()
            .find(|t| t.provider_key == source_key)
            .cloned();
        let previous_segments = history.segments.len();
        let start = source.history_session_id.as_deref().map_or(0, |id| {
            history
                .segments
                .iter()
                .filter(|s| s.provider_key == source_key && s.history_session_id == id)
                .map(|s| s.end_turn)
                .max()
                .unwrap_or(0)
        });
        if let Some(id) = &source.history_session_id {
            let turns = read(&source, id)?;
            if turns.is_empty() && !sess.reduced.lock().entries().is_empty() {
                return Err("The source provider hasn't saved its conversation history yet".into());
            }
            if (turns.len() as i64) < start {
                return Err(
                    "The source history was truncated; switching would lose context".into(),
                );
            }
            if turns.len() as i64 > start {
                history.segments.push(ProviderSegmentRecord {
                    position: history.segments.len() as i64,
                    provider_key: source_key.clone(),
                    history_session_id: id.clone(),
                    first_turn: start,
                    end_turn: turns.len() as i64,
                });
            }
        } else if !sess.reduced.lock().entries().is_empty() {
            return Err("The source provider hasn't saved a resumable history".into());
        }
        history.threads.retain(|t| t.provider_key != source_key);
        let mut source_thread = thread(&source);
        source_thread.covered_segment = previous.map_or(0, |t| t.covered_segment);
        if history.segments.len() > previous_segments {
            source_thread.covered_segment = history.segments.len() as i64;
        }
        history.threads.push(source_thread);
        if history
            .threads
            .iter()
            .any(|t| t.provider_key == option.id && t.agent != option.kind)
        {
            return Err("The target's engine changed; its old native session cannot be resumed with the new engine".into());
        }
        let mut target = WorkspaceAcp {
            agent: option.kind.clone(),
            profile_id: option.profile_id().map(str::to_string),
            agent_definition_id: option.agent_definition_id().map(str::to_string),
            launch: option.launch.clone(),
            history_session_id: history
                .threads
                .iter()
                .find(|t| t.provider_key == option.id)
                .and_then(|t| t.history_session_id.clone()),
            config_values: smelt_core::session_control::new_conversation_config(&option),
            ..source.clone()
        };
        target.pending_prompt = None;
        target.pending_agent_preset = None;
        if !history.threads.iter().any(|t| t.provider_key == option.id) {
            history.threads.push(thread(&target));
        }
        let source_timeline = assemble(source.clone(), history.clone(), store.clone(), &mut read)?;
        let target_timeline = assemble(target.clone(), history.clone(), store.clone(), &mut read)?;
        let resume_ids: Vec<_> = target.history_session_id.iter().cloned().collect();
        sessions
            .try_acquire_resumes(&resume_ids, &source.sid)
            .map_err(message)?;
        if !retire_acp_runtime(sess) {
            return Err("The old provider hasn't exited; the target wasn't started".into());
        }
        let old_timeline = sess
            .provider_timeline
            .lock()
            .take()
            .or(Some(source_timeline));
        *sess.provider_timeline.lock() = Some(target_timeline);
        let launch = |active: &WorkspaceAcp,
                      ephemeral: &std::collections::BTreeMap<String, String>| {
            *sess.ephemeral_env.lock() = ephemeral.clone();
            {
                let mut reduced = sess.reduced.lock();
                *reduced = Default::default();
                reduced.accepted_delivery_ids = accepted_deliveries.clone();
            }
            *sess.pending_agent_preset.lock() = active.pending_agent_preset.clone();
            *sess.after_ready.lock() = Default::default();
            if let Some(req) = crate::acp_host::parse_acp_open_request(&serde_json::json!({
                "id": active.sid, "cwd": active.cwd, "launch": active.launch,
                "resume_id": active.history_session_id, "initial_config": active.config_values,
            })) {
                crate::acp_supervise::remember_requested_after_ready(sess, &req);
            }
            *sess.durable_history_session_id.lock() = active.history_session_id.clone();
            acp_relaunch(
                &slot,
                &source.sid,
                active.launch.clone(),
                active.history_session_id.clone(),
                None,
                None,
                Arc::clone(sessions),
                events,
                false,
            );
        };
        launch(&target, &Default::default());
        let started = crate::acp_idle::wait_for_acp_ready(sess, Duration::from_secs(45));
        let committed = started.and_then(|()| {
            crate::acp_supervise::run_after_ready(&source.sid, sess, events);
            target.history_session_id = crate::acp_host::known_acp_resume_id(&sess.reduced.lock())
                .or(target.history_session_id.clone());
            store
                .switch_provider(
                    &source.sid,
                    &source_key,
                    &WorkspaceAcpRecord {
                        agent: target.agent.clone(),
                        profile_id: target.profile_id.clone(),
                        agent_definition_id: target.agent_definition_id.clone(),
                        history_session_id: target.history_session_id.clone(),
                        launch_command: target.launch.command.clone(),
                        launch_env: target
                            .launch
                            .env
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        config_values: target.config_values.clone(),
                        ..Default::default()
                    },
                    &history,
                )
                .map_err(message)?;
            Ok(())
        });
        if let Err(error) = committed {
            if retire_acp_runtime(sess) {
                *sess.provider_timeline.lock() = old_timeline;
                launch(&source, &source_ephemeral);
            }
            return Err(format!(
                "Couldn't switch provider: {error}; the original provider binding was retained"
            ));
        }
        sessions.retain_resumes_for(&source.sid, &resume_ids);
        if let Some(timeline) = &mut *sess.provider_timeline.lock() {
            timeline.active = target.clone();
        }
        ready(sess);
        crate::workspace_directory::publish_current(events);
        crate::acp_host::push_acp_snapshot_since(sess, true, Some(0));
        Ok(target)
    })();
    if result.is_err() && sess.unreaped_pid.lock().is_none() {
        sessions.retain_resumes_for(&params.session_id, &original_resumes);
    }
    sess.provider_switching.store(false, Ordering::SeqCst);
    result
}

#[cfg(test)]
mod tests;
