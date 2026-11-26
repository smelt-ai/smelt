use super::*;
// ============================================================================
// Worktree Diff / 自由合并 / 跨机 Patch 应用
// ============================================================================

pub(crate) fn diff_member_worktree(
    params: GroupMemberIdParams,
) -> Result<GroupWorktreeDiffResult, ControlError> {
    let store = group_store()?;
    let member = store
        .get_member(&params.member_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| err(ControlErrorCode::NotFound, "Group member not found"))?;
    let (cwd, branch) = resolve_session_cwd_and_branch(&member.session_id);
    let cwd = cwd.ok_or_else(|| {
        err(
            ControlErrorCode::FailedPrecondition,
            "Member session has no working directory",
        )
    })?;
    diff_worktree_at(&cwd, branch, member.runtime_id)
}

pub(super) fn diff_worktree_at(
    cwd: &str,
    branch: Option<String>,
    runtime_id: Option<String>,
) -> Result<GroupWorktreeDiffResult, ControlError> {
    let branch = branch.unwrap_or_else(|| {
        git_output(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_default()
            .trim()
            .to_string()
    });
    let base_branch = detect_base_branch(cwd);
    let range = format!("{base_branch}...HEAD");
    let diff_stat = git_output(cwd, &["diff", "--stat", &range]).unwrap_or_default();
    let patch = git_output(cwd, &["diff", &range])
        .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
    Ok(GroupWorktreeDiffResult {
        runtime_id,
        base_branch,
        branch,
        diff_stat,
        patch,
    })
}

pub(crate) fn merge_member_worktree(
    params: GroupWorktreeMergeParams,
) -> Result<GroupWorktreeMergeResult, ControlError> {
    let store = group_store()?;
    let group = store
        .get_group(&params.group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| err(ControlErrorCode::NotFound, "Group not found"))?;
    let member = store
        .get_member(&params.member_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| err(ControlErrorCode::NotFound, "Group member not found"))?;
    let (source_cwd, source_branch) = resolve_session_cwd_and_branch(&member.session_id);
    let source_cwd = source_cwd.ok_or_else(|| {
        err(
            ControlErrorCode::FailedPrecondition,
            "Member session has no working directory",
        )
    })?;
    let source_branch = source_branch
        .or_else(|| {
            git_output(&source_cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
                .ok()
                .map(|s| s.trim().to_string())
        })
        .ok_or_else(|| {
            err(
                ControlErrorCode::OperationFailed,
                "Cannot resolve member branch",
            )
        })?;

    let target_cwd = resolve_group_target_cwd(&group, &source_cwd);
    merge_worktree_at(
        &group.id,
        &member.alias,
        &source_cwd,
        &source_branch,
        &target_cwd,
        params.target,
        params.strategy,
    )
}

pub(super) fn merge_worktree_at(
    group_id: &str,
    member_alias: &str,
    source_cwd: &str,
    source_branch: &str,
    target_cwd: &str,
    target: GroupMergeTarget,
    strategy: GroupMergeStrategy,
) -> Result<GroupWorktreeMergeResult, ControlError> {
    // 按仓库路径串行化 git checkout / merge，防止并发合入互相踩 `.git/index.lock` 或切乱分支。
    let repo_lock = {
        let mut state = lock_state();
        std::sync::Arc::clone(
            state
                .repo_merge_locks
                .entry(target_cwd.to_string())
                .or_insert_with(|| std::sync::Arc::new(Mutex::new(()))),
        )
    };
    let _repo_guard = repo_lock.lock().unwrap_or_else(|p| p.into_inner());

    // 合并前检查目标工作区是否干净，避免 `git checkout` 覆盖或污染用户未提交改动。
    let dirty = git_output(target_cwd, &["status", "--porcelain"]).unwrap_or_default();
    if !dirty.trim().is_empty() {
        return Err(err(
            ControlErrorCode::FailedPrecondition,
            format!(
                "Target working tree `{target_cwd}` has uncommitted changes; commit or stash before merging"
            ),
        ));
    }

    let target_branch = match target {
        GroupMergeTarget::Integration => {
            let slug: String = group_id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            let integ = format!("smelt/group/{slug}/integration");
            // 若集成分支已存在则切换过去（保留已合并内容）；不存在则基于基线分支创建。
            if git_output(target_cwd, &["rev-parse", "--verify", &integ]).is_ok() {
                git_output(target_cwd, &["checkout", &integ])
                    .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
            } else {
                let base = detect_base_branch(target_cwd);
                let _ = git_output(target_cwd, &["checkout", &base]);
                git_output(target_cwd, &["checkout", "-b", &integ])
                    .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
            }
            integ
        }
        GroupMergeTarget::Base => {
            let base = detect_base_branch(target_cwd);
            git_output(target_cwd, &["checkout", &base])
                .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
            base
        }
    };

    match strategy {
        GroupMergeStrategy::Merge => {
            git_output(
                target_cwd,
                &[
                    "merge",
                    "--no-ff",
                    "-m",
                    &format!("group({group_id}): merge @{member_alias}"),
                    source_branch,
                ],
            )
            .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
        }
        GroupMergeStrategy::Squash => {
            git_output(target_cwd, &["merge", "--squash", source_branch])
                .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
            git_output(
                target_cwd,
                &[
                    "commit",
                    "-m",
                    &format!("group({group_id}): squash @{member_alias}"),
                ],
            )
            .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
        }
        GroupMergeStrategy::CherryPick => {
            let sha = git_output(source_cwd, &["rev-parse", "HEAD"])
                .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
            git_output(target_cwd, &["cherry-pick", sha.trim()])
                .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
        }
    }
    let commit_sha = git_output(target_cwd, &["rev-parse", "HEAD"])
        .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
    Ok(GroupWorktreeMergeResult {
        commit_sha: commit_sha.trim().to_string(),
        target_branch,
    })
}

pub(crate) fn apply_member_patch(
    params: GroupWorktreeApplyPatchParams,
) -> Result<GroupWorktreeApplyPatchResult, ControlError> {
    let diff = diff_member_worktree(GroupMemberIdParams {
        group_id: params.group_id,
        member_id: params.source_member_id,
    })?;
    apply_patch_at(
        &params.target_cwd,
        params.create_branch.as_deref(),
        &diff.patch,
    )
}

pub(super) fn apply_patch_at(
    target_cwd: &str,
    create_branch: Option<&str>,
    patch: &str,
) -> Result<GroupWorktreeApplyPatchResult, ControlError> {
    if patch.trim().is_empty() {
        return Err(err(
            ControlErrorCode::FailedPrecondition,
            "Source member has no diff to apply",
        ));
    }
    if let Some(branch) = create_branch.filter(|s| !s.is_empty()) {
        git_output(target_cwd, &["checkout", "-B", branch])
            .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
    }
    let mut child = Command::new("git")
        .args(["apply", "--3way", "-"])
        .current_dir(target_cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(patch.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if !output.status.success() {
        return Err(err(
            ControlErrorCode::OperationFailed,
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let applied_branch = git_output(target_cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(GroupWorktreeApplyPatchResult {
        applied_branch,
        commit_sha: None,
    })
}

pub(super) fn git_output(cwd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git {:?} failed: {e}", args))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn detect_base_branch(cwd: &str) -> String {
    for candidate in ["main", "master"] {
        if git_output(cwd, &["rev-parse", "--verify", candidate]).is_ok() {
            return candidate.to_string();
        }
    }
    "HEAD~1".to_string()
}

fn resolve_group_target_cwd(group: &GroupRecord, fallback: &str) -> String {
    if let Some(ws_id) = group.workset_id.as_deref()
        && let Ok(main_store) = smelt_core::sqlite_state::default_sqlite_store()
        && let Ok((_, snapshot)) = main_store.workspace_directory()
        && let Some(ws) = snapshot.worksets.iter().find(|w| w.id == ws_id)
    {
        return ws.project_root.clone();
    }
    fallback.to_string()
}
