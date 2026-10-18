//! `git.*` 控制方法：客户端不再自己起 git，统一由运行时在它那台机器上执行。
//!
//! 这里只做参数解码和类型绑定，git 的具体做法都在 `smelt-git`。

use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};
use smelt_runtime_client::protocol::git::*;

/// `method` 必须是 `git.*` 之一；控制分发器按方法名把整组转过来。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::GitDiscover => call::<GitDiscoverCall>(params, |p| {
            Ok(smelt_git::discovery::discover(
                &smelt_core::fs::LocalFs,
                &smelt_git::LocalSubprocess,
                std::path::Path::new(&p.root),
                &smelt_git::discovery::DiscoveryOptions::default(),
            ))
        }),
        ControlMethod::GitRepoInfo => call::<GitRepoInfoCall>(params, |p| {
            Ok(GitRepoInfoResult {
                info: smelt_git::load_repo_info(&p.root),
            })
        }),
        ControlMethod::GitStatus => {
            call::<GitStatusCall>(params, |p| Ok(smelt_git::load_git_status(&p.root)))
        }
        ControlMethod::GitBranches => {
            call::<GitBranchesCall>(params, |p| Ok(smelt_git::load_branches(&p.root)))
        }
        ControlMethod::GitCheckout => call::<GitCheckoutCall>(params, |p| {
            smelt_git::checkout_branch(&p.root, &p.branch).map(done)
        }),
        ControlMethod::GitBranchDelete => call::<GitBranchDeleteCall>(params, |p| {
            smelt_git::delete_branch(&p.root, &p.branch, p.remote).map(done)
        }),
        ControlMethod::GitMerge => call::<GitMergeCall>(params, |p| {
            smelt_git::merge_branch(&p.root, &p.branch).map(done)
        }),
        ControlMethod::GitStage => call::<GitStageCall>(params, |p| {
            smelt_git::stage_paths(&p.root, &p.paths).map(done)
        }),
        ControlMethod::GitUnstage => call::<GitUnstageCall>(params, |p| {
            smelt_git::unstage_paths(&p.root, &p.paths).map(done)
        }),
        ControlMethod::GitDiscardFile => call::<GitDiscardFileCall>(params, |p| {
            smelt_git::discard_file(&p.root, &p.seen).map(done)
        }),
        ControlMethod::GitApplyPatch => call::<GitApplyPatchCall>(params, |p| {
            smelt_git::apply_patch_action(&p.root, p.action, &p.patch).map(done)
        }),
        ControlMethod::GitDiscardAll => call::<GitDiscardAllCall>(params, |p| {
            smelt_git::discard_all(&p.root, &p.seen).map(done)
        }),
        ControlMethod::GitCommit => call::<GitCommitCall>(params, |p| {
            smelt_git::commit_and_maybe_push(&p.root, &p.message, p.push, &p.branch).map(done)
        }),
        ControlMethod::GitPush => call::<GitPushCall>(params, |p| {
            smelt_git::push_current(&p.root, &p.branch).map(done)
        }),
        ControlMethod::GitFetch => {
            call::<GitFetchCall>(params, |p| smelt_git::fetch_remote(&p.root).map(done))
        }
        ControlMethod::GitPull => {
            call::<GitPullCall>(params, |p| smelt_git::pull_rebase(&p.root).map(done))
        }
        ControlMethod::GitStashPush => {
            call::<GitStashPushCall>(params, |p| smelt_git::stash_push(&p.root).map(done))
        }
        ControlMethod::GitStashPop => {
            call::<GitStashPopCall>(params, |p| smelt_git::stash_pop(&p.root).map(done))
        }
        ControlMethod::GitFileDiff => call::<GitFileDiffCall>(params, |p| {
            Ok(smelt_git::file_diff(&p.root, &p.path, p.untracked, p.scope))
        }),
        ControlMethod::GitAllDiff => call::<GitAllDiffCall>(params, |p| {
            Ok(GitTextResult {
                text: smelt_git::all_diff(&p.root, p.scope, &p.untracked_paths),
            })
        }),
        ControlMethod::GitLog => call::<GitLogCall>(params, |p| {
            Ok(GitLogResult {
                commits: smelt_git::log(&p.root, &p.scope, p.limit)?,
            })
        }),
        ControlMethod::GitCommitFiles => call::<GitCommitFilesCall>(params, |p| {
            Ok(GitCommitFilesResult {
                files: smelt_git::commit_files(&p.root, &p.hash)?,
            })
        }),
        ControlMethod::GitCommitFileDiff => call::<GitCommitFileDiffCall>(params, |p| {
            Ok(GitTextResult {
                text: smelt_git::commit_file_diff(&p.root, &p.hash, &p.path),
            })
        }),
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not a git method"),
        )),
    }
}

fn call<C: ControlCall>(
    params: serde_json::Value,
    run: impl FnOnce(C::Params) -> Result<C::Result, String>,
) -> Result<serde_json::Value, ControlError> {
    let params = decode_params::<C::Params>(params)?;
    let result = run(params)
        .map_err(|message| ControlError::new(ControlErrorCode::OperationFailed, message))?;
    serde_json::to_value(result)
        .map_err(|error| ControlError::new(ControlErrorCode::Internal, error.to_string()))
}

fn done(_: ()) -> EmptyParams {
    EmptyParams::default()
}
