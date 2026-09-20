use std::collections::HashMap;

use smelt_core::automation::{AutomationRun, AutomationRunStatus};
use smelt_core::daemon_state::DaemonPhase;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationProjectionOrigin {
    InitialSnapshot,
    Incremental,
}

pub(crate) fn effective_projection_origin(
    requested: AutomationProjectionOrigin,
    previous_store_id: &str,
    current_store_id: &str,
) -> AutomationProjectionOrigin {
    if requested == AutomationProjectionOrigin::InitialSnapshot
        && !previous_store_id.is_empty()
        && previous_store_id == current_store_id
    {
        AutomationProjectionOrigin::Incremental
    } else {
        requested
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationRunNotificationKind {
    Success,
    Failure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutomationRunNotification {
    pub automation_id: String,
    pub run_id: String,
    pub kind: AutomationRunNotificationKind,
    pub subtitle: String,
    pub body: String,
}

pub(crate) fn automation_run_notifications(
    previous: &[AutomationRun],
    current: &[AutomationRun],
    origin: AutomationProjectionOrigin,
) -> Vec<AutomationRunNotification> {
    if origin == AutomationProjectionOrigin::InitialSnapshot {
        return Vec::new();
    }

    let previous = previous
        .iter()
        .map(|run| (run.id.as_str(), run))
        .collect::<HashMap<_, _>>();
    current
        .iter()
        .filter_map(|run| {
            let (kind, state_label, fallback) = match run.status {
                AutomationRunStatus::Completed => (
                    AutomationRunNotificationKind::Success,
                    "已完成",
                    "运行已完成",
                ),
                AutomationRunStatus::Failed => {
                    (AutomationRunNotificationKind::Failure, "失败", "运行失败")
                }
                AutomationRunStatus::Skipped => (
                    AutomationRunNotificationKind::Failure,
                    "已跳过",
                    "本次运行已跳过",
                ),
                AutomationRunStatus::Starting
                | AutomationRunStatus::Queued
                | AutomationRunStatus::Dispatching
                | AutomationRunStatus::Running
                | AutomationRunStatus::AwaitingApproval
                | AutomationRunStatus::WaitingForUser
                | AutomationRunStatus::Cancelled => return None,
            };
            if previous
                .get(run.id.as_str())
                .is_some_and(|previous| previous.status.is_terminal())
            {
                return None;
            }
            let detail = match run.status {
                AutomationRunStatus::Completed => run.output.as_deref(),
                AutomationRunStatus::Failed | AutomationRunStatus::Skipped => run.error.as_deref(),
                _ => None,
            }
            .and_then(notification_excerpt)
            .unwrap_or_else(|| fallback.to_string());
            Some(AutomationRunNotification {
                automation_id: run.automation_id.clone(),
                run_id: run.id.clone(),
                kind,
                subtitle: format!(
                    "自动化{state_label} · {}",
                    run.context.automation_name.trim()
                ),
                body: format!("{} · {detail}", run.source.label()),
            })
        })
        .collect()
}

fn notification_excerpt(value: &str) -> Option<String> {
    const MAX_CHARS: usize = 240;

    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.is_empty() {
        return None;
    }
    let mut chars = value.chars();
    let mut excerpt = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        excerpt.push('…');
    }
    Some(excerpt)
}

pub(crate) fn owns_session(runs: &[AutomationRun], session_id: &str) -> bool {
    runs.iter()
        .any(|run| run.session_id.as_deref() == Some(session_id))
}

pub(crate) fn suppress_runtime_terminal_attention(
    runs: &[AutomationRun],
    session_id: &str,
    phase: DaemonPhase,
) -> bool {
    owns_session(runs, session_id)
        && matches!(
            phase,
            DaemonPhase::Succeeded | DaemonPhase::Failed | DaemonPhase::Dead
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_core::automation::{AutomationAction, AutomationRunContext, AutomationRunSource};

    fn run(status: AutomationRunStatus) -> AutomationRun {
        AutomationRun {
            id: "run-1".into(),
            automation_id: "automation-1".into(),
            context: AutomationRunContext {
                automation_name: "日报汇总".into(),
                action: AutomationAction::Shell {
                    command: "echo done".into(),
                    args: Vec::new(),
                },
                cwd: "/tmp".into(),
                trigger_payload: None,
                prompt: None,
                agent_definition_name: None,
                engine_kind_id: None,
                agent_instructions: None,
            },
            source: AutomationRunSource::Scheduled,
            scheduled_for: Some(10),
            status,
            created_at: 10,
            started_at: Some(11),
            delivery_attempt_at: None,
            delivery_attempts: 0,
            finished_at: None,
            session_id: None,
            provider_session_id: None,
            output: None,
            error: None,
            runtime_released_at: None,
        }
    }

    #[test]
    fn shell_completion_emits_one_system_notification() {
        let running = run(AutomationRunStatus::Running);
        let mut completed = running.clone();
        completed.status = AutomationRunStatus::Completed;
        completed.finished_at = Some(12);
        completed.output = Some("42 files updated".into());

        let notifications = automation_run_notifications(
            &[running],
            &[completed.clone()],
            AutomationProjectionOrigin::Incremental,
        );
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].kind,
            AutomationRunNotificationKind::Success
        );
        assert_eq!(notifications[0].run_id, "run-1");
        assert!(notifications[0].subtitle.contains("日报汇总"));
        assert!(notifications[0].body.contains("42 files updated"));

        completed.runtime_released_at = Some(13);
        assert!(
            automation_run_notifications(
                &[run(AutomationRunStatus::Completed)],
                &[completed],
                AutomationProjectionOrigin::Incremental,
            )
            .is_empty(),
            "runtime 回收投影不能重复通知"
        );
    }

    #[test]
    fn agent_completion_uses_the_run_not_the_hidden_acp_session() {
        let mut running = run(AutomationRunStatus::Running);
        running.context.action = AutomationAction::Agent {
            agent_definition_id: "agent-1".into(),
            prompt: Some("生成日报".into()),
        };
        running.session_id = Some("acp-automation-hidden".into());
        let mut completed = running.clone();
        completed.status = AutomationRunStatus::Completed;
        completed.finished_at = Some(12);

        let notifications = automation_run_notifications(
            &[running],
            &[completed],
            AutomationProjectionOrigin::Incremental,
        );
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].automation_id, "automation-1");
        assert_eq!(notifications[0].run_id, "run-1");
    }

    #[test]
    fn initial_snapshot_does_not_replay_historical_completions() {
        let mut completed = run(AutomationRunStatus::Completed);
        completed.finished_at = Some(12);
        assert!(
            automation_run_notifications(
                &[],
                &[completed],
                AutomationProjectionOrigin::InitialSnapshot,
            )
            .is_empty()
        );
    }

    #[test]
    fn reconnect_snapshot_preserves_completion_edges_for_the_same_store() {
        assert_eq!(
            effective_projection_origin(
                AutomationProjectionOrigin::InitialSnapshot,
                "store-1",
                "store-1",
            ),
            AutomationProjectionOrigin::Incremental
        );
        assert_eq!(
            effective_projection_origin(AutomationProjectionOrigin::InitialSnapshot, "", "store-1",),
            AutomationProjectionOrigin::InitialSnapshot
        );
        assert_eq!(
            effective_projection_origin(
                AutomationProjectionOrigin::InitialSnapshot,
                "retired-store",
                "new-store",
            ),
            AutomationProjectionOrigin::InitialSnapshot
        );

        let running = run(AutomationRunStatus::Running);
        let mut completed = running.clone();
        completed.status = AutomationRunStatus::Completed;
        completed.finished_at = Some(12);
        let notifications = automation_run_notifications(
            &[running],
            &[completed],
            effective_projection_origin(
                AutomationProjectionOrigin::InitialSnapshot,
                "store-1",
                "store-1",
            ),
        );
        assert_eq!(notifications.len(), 1);
    }

    #[test]
    fn failures_and_immediate_terminal_runs_are_not_lost() {
        let mut failed = run(AutomationRunStatus::Failed);
        failed.finished_at = Some(12);
        failed.error = Some("command exited with status 2".into());

        let notifications =
            automation_run_notifications(&[], &[failed], AutomationProjectionOrigin::Incremental);
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].kind,
            AutomationRunNotificationKind::Failure
        );
        assert!(notifications[0].body.contains("status 2"));
    }

    #[test]
    fn skipped_runs_notify_as_failures_but_user_cancellations_stay_quiet() {
        let mut skipped = run(AutomationRunStatus::Skipped);
        skipped.finished_at = Some(12);
        skipped.error = Some("上一条 Run 仍在执行，本次计划已跳过".into());
        let notifications =
            automation_run_notifications(&[], &[skipped], AutomationProjectionOrigin::Incremental);
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].kind,
            AutomationRunNotificationKind::Failure
        );

        assert!(
            automation_run_notifications(
                &[run(AutomationRunStatus::Running)],
                &[run(AutomationRunStatus::Cancelled)],
                AutomationProjectionOrigin::Incremental,
            )
            .is_empty()
        );
    }

    #[test]
    fn automation_runtime_terminal_attention_is_suppressed_for_every_failure_path() {
        let mut agent_run = run(AutomationRunStatus::Running);
        agent_run.session_id = Some("acp-automation-1".into());
        let runs = [agent_run];

        for phase in [
            DaemonPhase::Succeeded,
            DaemonPhase::Failed,
            DaemonPhase::Dead,
        ] {
            assert!(suppress_runtime_terminal_attention(
                &runs,
                "acp-automation-1",
                phase
            ));
        }
        assert!(!suppress_runtime_terminal_attention(
            &runs,
            "acp-automation-1",
            DaemonPhase::Thinking
        ));
        assert!(!suppress_runtime_terminal_attention(
            &[],
            "acp-automation-1",
            DaemonPhase::Failed
        ));
    }
}
