//! 工作树的合并情况：关闭前、窗口回到前台时问运行时（`workspace.workset.inspect`，会先
//! fetch 各仓库的基线），结果缓存在 [`SessionList::workset_checks`] 里给侧栏画。
//!
//! 什么时候问，全由事件决定，不起定时器：
//! - 目录里出现了还没问过的工作树（`SessionsChanged`）：问一次，侧栏才有状态可画。
//! - 窗口回到前台（`WindowActivated`）：上次问过超过 [`RECHECK_AFTER`] 的再问。人常常是
//!   去浏览器合完 PR 回来，这时候该看到「已合并」。
//! - 用户点「检查合并状态」、点「关闭工作树」：马上问。
//!
//! fetch 走网络，所以同一棵树同时只问一次，间隔也有下限。

use rust_i18n::t;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Task};
use smelt_plugin_api::model::control_api::WorkspaceWorksetInspect;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceWorksetInspectParams, WorkspaceWorksetInspectResult, WorkspaceWorksetMerge,
};
use smelt_plugin_api::{self as api, RuntimeKey};

use crate::model::SessionList;

/// 窗口回到前台时，离上次检查超过这么久才再查。查一次要 fetch 每个仓库。
const RECHECK_AFTER: Duration = Duration::from_secs(5 * 60);

/// 检查、关闭工作树的调用时限：要 fetch 每个仓库的远端，慢网络下可能要几分钟。
pub(crate) const WORKSET_GIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WorksetKey {
    pub(crate) machine: RuntimeKey,
    pub(crate) id: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorksetCheck {
    /// 正在问。
    pub(crate) running: bool,
    /// 上一次的结果和时间；还没问完过是 `None`。
    pub(crate) last: Option<(Instant, Result<WorkspaceWorksetInspectResult, String>)>,
}

/// 侧栏上能说的合并情况。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeSummary {
    /// 还没查过，或者查失败了。
    Unknown,
    /// 每个仓库都干净、分支都合进了基线：可以关。
    Merged,
    /// 有仓库判断不了是否已合并，其余都没问题：确认后可以关。
    Unconfirmed,
    /// 有未提交改动或还没合并的分支。
    Pending,
}

impl MergeSummary {
    pub(crate) fn of(check: Option<&WorksetCheck>) -> Self {
        match check.and_then(|check| check.last.as_ref()) {
            Some((_, Ok(result))) => Self::of_result(result),
            _ => Self::Unknown,
        }
    }

    pub(crate) fn of_result(result: &WorkspaceWorksetInspectResult) -> Self {
        let blocked = result.repositories.iter().any(|repository| {
            repository.dirty || repository.merge == Some(WorkspaceWorksetMerge::Unmerged)
        });
        let unconfirmed = result
            .repositories
            .iter()
            .any(|repository| repository.merge == Some(WorkspaceWorksetMerge::Unconfirmed));
        if blocked {
            Self::Pending
        } else if unconfirmed {
            Self::Unconfirmed
        } else {
            Self::Merged
        }
    }
}

/// 给人看的每个仓库的情况，一行一个（悬停提示、确认框里用）。
pub(crate) fn describe(result: &WorkspaceWorksetInspectResult) -> Vec<String> {
    let mut lines: Vec<String> = result
        .repositories
        .iter()
        .map(|repository| {
            let state = if repository.dirty {
                t!("workset.dirty")
            } else {
                match repository.merge {
                    Some(WorkspaceWorksetMerge::Merged) | None => t!("workset.merged"),
                    Some(WorkspaceWorksetMerge::Unmerged) => t!("workset.unmerged"),
                    Some(WorkspaceWorksetMerge::Unconfirmed) => t!("workset.unconfirmed"),
                }
            };
            t!(
                "workset.repository_state",
                name = repository.name,
                state = state
            )
            .into_owned()
        })
        .collect();
    if !result.refresh_failures.is_empty() {
        lines.push(t!("workset.refresh_failed").into_owned());
    }
    lines
}

/// 问一次合并情况，结果记进缓存、重算侧栏，也交给调用方。同一棵树已经在问时也照问：
/// 调用方（关闭）要等这次的结果。
pub(crate) fn inspect(
    key: WorksetKey,
    cx: &mut App,
) -> Task<Result<WorkspaceWorksetInspectResult, String>> {
    let sessions = SessionList::global(cx);
    sessions.update(cx, |sessions, _| {
        sessions
            .workset_checks
            .entry(key.clone())
            .or_default()
            .running = true;
    });
    let params = WorkspaceWorksetInspectParams {
        workset_id: key.id.clone(),
    };
    let call = api::call::<WorkspaceWorksetInspect>(&key.machine, params, WORKSET_GIT_TIMEOUT, cx);
    cx.spawn(async move |cx| {
        let result = call.await;
        cx.update(|cx| {
            sessions.update(cx, |sessions, cx| {
                let check = sessions.workset_checks.entry(key).or_default();
                check.running = false;
                check.last = Some((Instant::now(), result.clone()));
                sessions.rebuild_sidebar(cx);
            })
        });
        result
    })
}

/// 目录里每棵工作树要不要问：`stale` 为真时连问过但超过 [`RECHECK_AFTER`] 的也问，否则
/// 只问还没问过的。顺手丢掉已经不在目录里的。
pub(crate) fn refresh(stale: bool, cx: &mut App) {
    let host = api::host(cx);
    let mut present: Vec<WorksetKey> = Vec::new();
    for machine in host.machines(cx) {
        let Some(directory) = host.workspace_directory(&machine.key, cx) else {
            continue;
        };
        present.extend(directory.worksets.iter().map(|workset| WorksetKey {
            machine: machine.key.clone(),
            id: workset.id.clone(),
        }));
    }
    let sessions = SessionList::global(cx);
    let due: Vec<WorksetKey> = sessions.update(cx, |sessions, _| {
        sessions
            .workset_checks
            .retain(|key, _| present.contains(key));
        due_checks(&present, &sessions.workset_checks, stale, Instant::now())
    });
    for key in due {
        inspect(key, cx).detach();
    }
}

fn due_checks(
    present: &[WorksetKey],
    checks: &HashMap<WorksetKey, WorksetCheck>,
    stale: bool,
    now: Instant,
) -> Vec<WorksetKey> {
    present
        .iter()
        .filter(|key| match checks.get(key) {
            None => true,
            Some(check) if check.running => false,
            Some(check) => match &check.last {
                None => true,
                Some((at, _)) => stale && now.duration_since(*at) >= RECHECK_AFTER,
            },
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use smelt_plugin_api::model::workspace_directory::WorkspaceWorksetRepositoryState;

    use super::*;

    fn key(id: &str) -> WorksetKey {
        WorksetKey {
            machine: RuntimeKey::Local,
            id: id.to_string(),
        }
    }

    fn repo(
        name: &str,
        dirty: bool,
        merge: Option<WorkspaceWorksetMerge>,
    ) -> WorkspaceWorksetRepositoryState {
        WorkspaceWorksetRepositoryState {
            name: name.to_string(),
            dirty,
            merge,
        }
    }

    fn checked(repositories: Vec<WorkspaceWorksetRepositoryState>) -> WorksetCheck {
        WorksetCheck {
            running: false,
            last: Some((
                Instant::now(),
                Ok(WorkspaceWorksetInspectResult {
                    repositories,
                    refresh_failures: Vec::new(),
                }),
            )),
        }
    }

    #[test]
    fn summary_puts_blockers_before_unconfirmed_merges() {
        use WorkspaceWorksetMerge::*;
        let summary = |repos| MergeSummary::of(Some(&checked(repos)));
        assert_eq!(
            summary(vec![
                repo("api", false, Some(Merged)),
                repo("web", false, None)
            ]),
            MergeSummary::Merged
        );
        assert_eq!(
            summary(vec![repo("api", false, Some(Unconfirmed))]),
            MergeSummary::Unconfirmed
        );
        assert_eq!(
            summary(vec![
                repo("api", false, Some(Unconfirmed)),
                repo("web", true, Some(Merged))
            ]),
            MergeSummary::Pending
        );
        assert_eq!(
            summary(vec![repo("api", false, Some(Unmerged))]),
            MergeSummary::Pending
        );
        assert_eq!(MergeSummary::of(None), MergeSummary::Unknown);
        let failed = WorksetCheck {
            running: false,
            last: Some((Instant::now(), Err("连不上".to_string()))),
        };
        assert_eq!(MergeSummary::of(Some(&failed)), MergeSummary::Unknown);
    }

    #[test]
    fn only_unchecked_worksets_are_due_unless_stale_checks_are_wanted() {
        let now = Instant::now();
        let old = WorksetCheck {
            running: false,
            last: Some((now - RECHECK_AFTER, Err("x".to_string()))),
        };
        let fresh = WorksetCheck {
            running: false,
            last: Some((now, Err("x".to_string()))),
        };
        let running = WorksetCheck {
            running: true,
            last: None,
        };
        let checks = HashMap::from([
            (key("old"), old),
            (key("fresh"), fresh),
            (key("running"), running),
        ]);
        let present = [key("new"), key("old"), key("fresh"), key("running")];
        assert_eq!(due_checks(&present, &checks, false, now), [key("new")]);
        assert_eq!(
            due_checks(&present, &checks, true, now),
            [key("new"), key("old")]
        );
    }
}
