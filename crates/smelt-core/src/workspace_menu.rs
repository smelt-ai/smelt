//! PC 侧栏对外暴露的纯数据快照。
//!
//! GPUI 和 Flutter 不共享控件，但必须共享项目分组、会话类型、显示标题和顺序。
//! 桌面经 smeltd `workspace_menu` op 发布；网关只消费 event_subscribe，不读工作区快照。

pub use smelt_model::workspace_menu::*;

use std::path::PathBuf;

fn workspace_menu_store() -> Result<smelt_store::Store, String> {
    if let Some(dir) = std::env::var_os("SMELT_WORKSPACE_MENU_DIR") {
        return crate::sqlite_state::open_sqlite_store(
            &PathBuf::from(dir).join(smelt_store::DATABASE_FILE_NAME),
        );
    }
    crate::sqlite_state::default_sqlite_store()
}

/// smeltd 启动时加载守护自己发布的侧栏菜单。
pub fn load_published_workspace_menu() -> WorkspaceMenuSnapshot {
    let Ok(store) = workspace_menu_store() else {
        return WorkspaceMenuSnapshot::default();
    };
    match store.get_workspace_menu_snapshot() {
        Ok(Some(snapshot)) => menu_from_snapshot(snapshot)
            .ok()
            .filter(|menu| menu.version > 0)
            .unwrap_or_default(),
        Ok(None) | Err(_) => WorkspaceMenuSnapshot::default(),
    }
}

pub fn persist_published_workspace_menu(menu: &WorkspaceMenuSnapshot) -> Result<(), String> {
    let store = workspace_menu_store()?;
    store
        .put_workspace_menu_snapshot(&snapshot_from_menu(menu)?)
        .map_err(Into::into)
}

fn snapshot_from_menu(
    menu: &WorkspaceMenuSnapshot,
) -> Result<smelt_store::PublishedWorkspaceMenuSnapshot, String> {
    Ok(smelt_store::PublishedWorkspaceMenuSnapshot {
        version: menu.version,
        revision: menu.revision,
        source_id: menu.source_id.clone(),
        source_revision: menu.source_revision,
        projects: menu
            .projects
            .iter()
            .map(|project| smelt_store::WorkspaceMenuProjectRecord {
                root: project.root.clone(),
                title: project.title.clone(),
                order: project.order,
            })
            .collect(),
        sessions: menu
            .sessions
            .iter()
            .map(|session| smelt_store::WorkspaceMenuSessionRecord {
                id: session.id.clone(),
                kind: match session.kind {
                    WorkspaceMenuSessionKind::Terminal => "terminal",
                    WorkspaceMenuSessionKind::Acp => "acp",
                }
                .to_string(),
                title: session.title.clone(),
                custom_title: session.custom_title,
                cwd: session.cwd.clone(),
                project_root: session.project_root.clone(),
                project_title: session.project_title.clone(),
                project_order: session.project_order,
                session_order: session.session_order,
                leaf_order: session.leaf_order,
                agent: session.agent.clone(),
            })
            .collect(),
    })
}

fn menu_from_snapshot(
    snapshot: smelt_store::PublishedWorkspaceMenuSnapshot,
) -> Result<WorkspaceMenuSnapshot, String> {
    let sessions = snapshot
        .sessions
        .into_iter()
        .map(|session| {
            let kind = match session.kind.as_str() {
                "terminal" => WorkspaceMenuSessionKind::Terminal,
                "acp" => WorkspaceMenuSessionKind::Acp,
                other => return Err(format!("未知的会话类型：{other}")),
            };
            Ok(WorkspaceMenuSession {
                id: session.id,
                kind,
                title: session.title,
                custom_title: session.custom_title,
                cwd: session.cwd,
                project_root: session.project_root,
                project_title: session.project_title,
                project_order: session.project_order,
                session_order: session.session_order,
                leaf_order: session.leaf_order,
                agent: session.agent,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(WorkspaceMenuSnapshot {
        version: snapshot.version,
        revision: snapshot.revision,
        source_id: snapshot.source_id,
        source_revision: snapshot.source_revision,
        projects: snapshot
            .projects
            .into_iter()
            .map(|project| WorkspaceMenuProject {
                root: project.root,
                title: project.title,
                order: project.order,
            })
            .collect(),
        sessions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_filter_uses_explicit_kind_instead_of_id_or_agent_name() {
        let snapshot = WorkspaceMenuSnapshot::current(
            vec![],
            vec![
                WorkspaceMenuSession {
                    id: "terminal-running-codex".into(),
                    kind: WorkspaceMenuSessionKind::Terminal,
                    title: "Codex CLI".into(),
                    custom_title: false,
                    cwd: None,
                    project_root: None,
                    project_title: None,
                    project_order: 0,
                    session_order: 0,
                    leaf_order: 0,
                    agent: Some("codex".into()),
                },
                WorkspaceMenuSession {
                    id: "any-stable-id".into(),
                    kind: WorkspaceMenuSessionKind::Acp,
                    title: "ACP conversation".into(),
                    custom_title: false,
                    cwd: None,
                    project_root: None,
                    project_title: None,
                    project_order: 0,
                    session_order: 1,
                    leaf_order: 0,
                    agent: Some("codex".into()),
                },
            ],
        );

        assert!(snapshot.acp_session("terminal-running-codex").is_none());
        assert!(snapshot.acp_session("any-stable-id").is_some());
        assert_eq!(
            snapshot.session("terminal-running-codex").map(|s| s.kind),
            Some(WorkspaceMenuSessionKind::Terminal)
        );
    }

    #[test]
    fn published_menu_roundtrips_through_dedicated_file() {
        let dir = std::env::temp_dir().join(format!(
            "smelt-workspace-menu-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        unsafe { std::env::set_var("SMELT_WORKSPACE_MENU_DIR", &dir) };
        let mut menu = WorkspaceMenuSnapshot::current(
            vec![WorkspaceMenuProject {
                root: "/repo".into(),
                title: "repo".into(),
                order: 0,
            }],
            vec![],
        );
        menu.revision = 3;
        persist_published_workspace_menu(&menu).unwrap();
        let loaded = load_published_workspace_menu();
        unsafe { std::env::remove_var("SMELT_WORKSPACE_MENU_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(loaded, menu);
    }
}
