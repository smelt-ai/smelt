//! `skills.*` 控制方法分发：在目标机器上执行技能扫描、导入校验、计划预览与写盘操作。

mod agents;
mod fsx;
mod ops;
mod scan;
#[cfg(all(test, unix))]
mod tests;

use std::path::Path;

use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, decode_params,
};
use smelt_runtime_client::protocol::skills::*;

pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::SkillsScan => call::<SkillsScanCall>(params, handle_scan),
        ControlMethod::SkillsInspectImport => {
            call::<SkillsInspectImportCall>(params, handle_inspect_import)
        }
        ControlMethod::SkillsPlan => call::<SkillsPlanCall>(params, handle_plan),
        ControlMethod::SkillsApply => call::<SkillsApplyCall>(params, handle_apply),
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not a skills method"),
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

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn wire_placement(p: scan::Placement) -> SkillPlacement {
    match p {
        scan::Placement::Universal => SkillPlacement::Universal,
        scan::Placement::Legacy => SkillPlacement::Legacy,
        scan::Placement::Agent(label) => SkillPlacement::Agent {
            label: label.to_owned(),
        },
    }
}

fn wire_access(a: scan::Access) -> SkillAccess {
    match a {
        scan::Access::Reads => SkillAccess::Reads,
        scan::Access::Linked => SkillAccess::Linked,
        scan::Access::Owner => SkillAccess::Owner,
        scan::Access::Missing => SkillAccess::Missing,
    }
}

fn wire_action(a: &ops::Action) -> SkillAction {
    match a {
        ops::Action::Install { from, to } => SkillAction::Install {
            from: path_str(from),
            to: path_str(to),
        },
        ops::Action::Move { from, to } => SkillAction::Move {
            from: path_str(from),
            to: path_str(to),
        },
        ops::Action::Link { at, to } => SkillAction::Link {
            at: path_str(at),
            to: path_str(to),
            agent: ops::agent_of_path(at).map(str::to_owned),
        },
        ops::Action::Unlink { at } => SkillAction::Unlink {
            at: path_str(at),
            agent: ops::agent_of_path(at).map(str::to_owned),
        },
        ops::Action::Trash { from } => SkillAction::Trash {
            from: path_str(from),
        },
    }
}

fn to_scope_request(req: SkillScopeRequest) -> ops::ScopeRequest {
    ops::ScopeRequest {
        universal: req.universal,
        labels: req.labels,
        project_scope: req.project_scope,
    }
}

fn handle_scan(params: SkillsScanParams) -> Result<SkillsScanResult, String> {
    let places = scan::Places::resolve(params.project.as_deref())?;
    let scanned = scan::scan(&places);
    let agents_list = agents::AGENTS
        .iter()
        .map(|agent| SkillAgentInfo {
            label: agent.label.to_owned(),
            has_user_dir: agent.user_dir.is_some(),
            has_project_dir: agent.project_dir.is_some(),
            reads_agents_dir: agent.reads_agents_dir,
        })
        .collect();
    let skills = scanned
        .skills
        .into_iter()
        .map(|s| SkillInfo {
            name: s.name,
            description: s.description,
            project_scope: s.project_scope,
            base: path_str(&s.base),
            dir: path_str(&s.dir),
            placement: wire_placement(s.placement),
            modified_ms: scan::modified_ms(s.modified),
            bridges: s.bridges.into_iter().map(str::to_owned).collect(),
            missing_bridges: s.missing_bridges.into_iter().map(str::to_owned).collect(),
            coverage: s
                .coverage
                .into_iter()
                .map(|c| SkillCoverage {
                    agent: c.agent.to_owned(),
                    access: wire_access(c.access),
                })
                .collect(),
            conflicts: s
                .conflicts
                .into_iter()
                .map(|c| SkillCopyInfo {
                    name: c.name,
                    description: c.description,
                    dir: path_str(&c.dir),
                    placement: wire_placement(c.placement),
                    modified_ms: scan::modified_ms(c.modified),
                })
                .collect(),
        })
        .collect();
    let broken = scanned
        .broken
        .into_iter()
        .map(|b| BrokenSkillLink {
            path: path_str(&b.path),
            project_scope: b.project_scope,
        })
        .collect();
    Ok(SkillsScanResult {
        home: path_str(&places.home),
        trash: path_str(&places.trash),
        project: places.project.as_deref().map(path_str),
        installed: scanned.installed.into_iter().map(str::to_owned).collect(),
        agents: agents_list,
        skills,
        broken,
    })
}

fn handle_inspect_import(params: SkillsInspectImportParams) -> Result<SkillsSourceInfo, String> {
    let places = scan::Places::resolve(params.project.as_deref())?;
    let info = ops::inspect_import(&places, Path::new(&params.source))?;
    Ok(SkillsSourceInfo {
        name: info.name,
        description: info.description,
        has_global: info.has_global,
    })
}

fn handle_plan(params: SkillsPlanParams) -> Result<SkillsPlanResult, String> {
    let places = scan::Places::resolve(params.project.as_deref())?;
    match params.kind {
        SkillPlanKind::Scope { dir, request } => {
            let actions = ops::plan_scope(&places, Path::new(&dir), &to_scope_request(request))?;
            Ok(SkillsPlanResult {
                actions: actions.iter().map(wire_action).collect(),
                replaces: false,
            })
        }
        SkillPlanKind::Import { source, request } => {
            let plan = ops::plan_import(&places, &to_scope_request(request), Path::new(&source))?;
            Ok(SkillsPlanResult {
                actions: plan.actions.iter().map(wire_action).collect(),
                replaces: plan.replaces,
            })
        }
    }
}

fn handle_apply(params: SkillsApplyParams) -> Result<SkillsApplyResult, String> {
    let places = scan::Places::resolve(params.project.as_deref())?;
    match params.op {
        SkillApplyOp::Scope { dir, request } => {
            let destination =
                ops::apply_scope(&places, Path::new(&dir), &to_scope_request(request))?;
            Ok(SkillsApplyResult {
                destination: Some(path_str(&destination)),
                migration: None,
            })
        }
        SkillApplyOp::Import { source, request } => {
            let destination = ops::import(&places, &to_scope_request(request), Path::new(&source))?;
            Ok(SkillsApplyResult {
                destination: Some(path_str(&destination)),
                migration: None,
            })
        }
        SkillApplyOp::Repair { dir } => {
            let skill = ops::locate(&places, Path::new(&dir))?;
            ops::repair_bridges(&places, &skill)?;
            Ok(SkillsApplyResult::default())
        }
        SkillApplyOp::RepairAll => {
            let scanned = scan::scan(&places);
            for skill in scanned
                .skills
                .iter()
                .filter(|s| !s.missing_bridges.is_empty() && s.conflicts.is_empty())
            {
                ops::repair_bridges(&places, skill)?;
            }
            Ok(SkillsApplyResult::default())
        }
        SkillApplyOp::Trash { dir } => {
            let skill = ops::locate(&places, Path::new(&dir))?;
            ops::trash_skill(&places, &skill)?;
            Ok(SkillsApplyResult::default())
        }
        SkillApplyOp::PruneBroken => {
            let scanned = scan::scan(&places);
            ops::prune_links(&scanned.broken)?;
            Ok(SkillsApplyResult::default())
        }
        SkillApplyOp::ResolveConflict { dir, keep } => {
            ops::resolve_conflict(&places, Path::new(&dir), Path::new(&keep))?;
            Ok(SkillsApplyResult::default())
        }
        SkillApplyOp::MigrateLegacy => {
            let outcome = ops::migrate_legacy(&places)?;
            Ok(SkillsApplyResult {
                destination: None,
                migration: Some(SkillMigrationResult {
                    migrated: outcome.migrated,
                    skipped: outcome.skipped,
                }),
            })
        }
        SkillApplyOp::EnsureTrash => {
            std::fs::create_dir_all(&places.trash).map_err(|e| e.to_string())?;
            Ok(SkillsApplyResult {
                destination: Some(path_str(&places.trash)),
                migration: None,
            })
        }
    }
}
