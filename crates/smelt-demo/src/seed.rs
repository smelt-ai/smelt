//! 展台种子。全部是虚构名字，只写进调用方给的家目录。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use smelt_model::acp_chat::{AcpEntry, ToolCallStatus, ToolKind, ToolOutputPart};
use smelt_model::acp_session::{
    ApprovalDetailsView, PendingPermission, PermissionOptionKindView, PermissionOptionView,
};
use smelt_model::automation::{
    AUTOMATION_FILE_SCHEMA_VERSION, AutomationAction, AutomationRunContext, AutomationSchedule,
    AutomationTrigger, SCHEDULE_DAY_MON,
};
use smelt_store::{
    AgentDefinitionRecord, AgentUiSnapshot, AutomationRecord, AutomationRunRecord,
    AutomationSnapshot, AutomationStateRecord, DATABASE_FILE_NAME, Store, WorkspaceSessionRecord,
    WorkspaceSnapshot, WorkspaceTerminalRecord,
};

use crate::SEEDED_MARKER;

const DEMO_UNIX: i64 = 1_720_000_000;
const EDITOR_ID: &str = "demo-lumen-editor";
const REVIEWER_ID: &str = "demo-harbor-review";
const MORNING_ID: &str = "demo-morning-lumen";
const PATROL_ID: &str = "demo-harbor-patrol";
const MORNING_RUN_ID: &str = "demo-morning-lumen-run";
const TERMINAL_ID: &str = "demo-lumen-terminal";

pub fn project_name() -> &'static str {
    "lumen-notes"
}

pub fn notes_committed() -> &'static str {
    "pub const WELCOME: &str = \"hello harbor\";\n"
}

pub fn notes_working() -> &'static str {
    "pub const WELCOME: &str = \"hello booth\";\n"
}

pub fn seed_into(home: &Path) -> Result<(), String> {
    let smelt_home = home.join(".smelt");
    std::fs::create_dir_all(&smelt_home).map_err(|error| error.to_string())?;
    let marker = smelt_home.join(SEEDED_MARKER);
    if marker.is_file() {
        return Ok(());
    }
    let project = seed_project(home)?;
    let store = Store::open_or_create(smelt_home.join(DATABASE_FILE_NAME))
        .map_err(|error| error.to_string())?;
    store
        .put_agent_ui_snapshot(&agent_snapshot())
        .map_err(|error| error.to_string())?;
    store
        .put_automation_snapshot(&automation_snapshot(&project))
        .map_err(|error| error.to_string())?;
    store
        .put_automation_run_transcript(MORNING_RUN_ID, &transcript_json()?)
        .map_err(|error| error.to_string())?;
    store
        .put_workspace_snapshot(&workspace_snapshot(&project))
        .map_err(|error| error.to_string())?;
    std::fs::write(marker, "seeded\n").map_err(|error| error.to_string())?;
    Ok(())
}

pub fn showcase_entries() -> Vec<AcpEntry> {
    vec![
        AcpEntry::User("把欢迎语从港口改成展台。".into()),
        AcpEntry::Assistant {
            text: "我先读一下 `src/notes.rs`。".into(),
            thought: false,
        },
        AcpEntry::tool_call(
            "demo-read",
            "Read src/notes.rs",
            ToolKind::Read,
            ToolCallStatus::Completed,
            vec![ToolOutputPart::Text(notes_committed().into())],
        ),
        AcpEntry::tool_call(
            "demo-edit",
            "Edit src/notes.rs",
            ToolKind::Edit,
            ToolCallStatus::Pending,
            vec![ToolOutputPart::Diff {
                path: "src/notes.rs".into(),
                old_text: Some(notes_committed().into()),
                new_text: notes_working().into(),
            }],
        ),
        AcpEntry::Assistant {
            text: "改动还停在批准前。点允许或拒绝都只留在这个窗口里。".into(),
            thought: false,
        },
    ]
}

pub fn showcase_permission(project: &Path) -> PendingPermission {
    PendingPermission {
        question: "允许把 src/notes.rs 的欢迎语改成展台用语吗？".into(),
        tool_call_id: "demo-edit".into(),
        options: vec![
            PermissionOptionView {
                option_id: "allow-once".into(),
                name: "允许一次".into(),
                kind: PermissionOptionKindView::AllowOnce,
            },
            PermissionOptionView {
                option_id: "reject-once".into(),
                name: "拒绝".into(),
                kind: PermissionOptionKindView::RejectOnce,
            },
        ],
        details: ApprovalDetailsView::FileChange {
            reason: Some("把欢迎语改成展台用语".into()),
            grant_root: Some(project.display().to_string()),
        },
    }
}

fn seed_project(home: &Path) -> Result<PathBuf, String> {
    let project = home.join("projects").join(project_name());
    std::fs::create_dir_all(project.join("src")).map_err(|error| error.to_string())?;
    std::fs::write(
        project.join("README.md"),
        "# 灯塔笔记\n\n展台演示用的本地仓库。欢迎语在 `src/notes.rs`。\n",
    )
    .map_err(|error| error.to_string())?;
    let notes = project.join("src/notes.rs");
    let committed = project.join(".git").is_dir() && git_ok(&project, &["rev-parse", "HEAD"]);
    if !committed {
        std::fs::write(&notes, notes_committed()).map_err(|error| error.to_string())?;
        try_git_commit(&project);
    }
    std::fs::write(&notes, notes_working()).map_err(|error| error.to_string())?;
    Ok(project)
}

fn try_git_commit(project: &Path) {
    if !git_ok(project, &["init"]) || !git_ok(project, &["add", "README.md", "src/notes.rs"]) {
        eprintln!("演示仓库没有做成 git 历史（继续用普通文件）。");
        return;
    }
    let committed = git_ok(
        project,
        &[
            "-c",
            "user.name=Lumen Demo",
            "-c",
            "user.email=demo@lumen-notes.invalid",
            "commit",
            "-m",
            "hello harbor",
        ],
    );
    if !committed {
        eprintln!("演示仓库没有做成 git 历史（继续用普通文件）。");
    }
}

fn git_ok(project: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(project)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn agent_snapshot() -> AgentUiSnapshot {
    AgentUiSnapshot {
        // 单独插定义时钩子默认会打开。演示数据必须明确关掉，避免写进主人的配置。
        agent_hooks_enabled: false,
        commands: Vec::new(),
        env: Vec::new(),
        config_memory: Vec::new(),
        agents: vec![agent(EDITOR_ID, "灯塔编辑"), agent(REVIEWER_ID, "港湾审阅")],
        profiles: Vec::new(),
    }
}

fn agent(id: &str, name: &str) -> AgentDefinitionRecord {
    AgentDefinitionRecord {
        id: id.into(),
        name: name.into(),
        description: "展台演示用的虚构智能体，不会连接真实模型。".into(),
        engine_kind_id: "pi".into(),
        prompt: "只用这个临时仓库里的文件。不要访问网络，也不要改仓库以外的路径。".into(),
        plugins_json: b"[]".to_vec(),
        mcp_servers_json: b"[]".to_vec(),
        context_folders_json: b"[]".to_vec(),
        context_links_json: b"[]".to_vec(),
        model_provider: "demo".into(),
        model_id: "demo-local".into(),
        memory_enabled: false,
    }
}

fn automation_snapshot(project: &Path) -> AutomationSnapshot {
    let project = project.display().to_string();
    let morning_action =
        AutomationAction::agent(EDITOR_ID, Some("汇总灯塔笔记昨晚的改动。".into()));
    let patrol_action = AutomationAction::agent(REVIEWER_ID, Some("巡一遍灯塔笔记的待办。".into()));
    let morning_trigger = AutomationTrigger {
        schedules: vec![AutomationSchedule::Daily {
            hour: 8,
            minute: 15,
        }],
        ..AutomationTrigger::default()
    };
    let patrol_trigger = AutomationTrigger {
        schedules: vec![AutomationSchedule::Weekly {
            days: SCHEDULE_DAY_MON,
            hour: 9,
            minute: 0,
        }],
        ..AutomationTrigger::default()
    };
    AutomationSnapshot {
        schema_version: AUTOMATION_FILE_SCHEMA_VERSION,
        store_id: "demo-lumen".into(),
        revision: 1,
        timezone_fingerprint: "UTC".into(),
        automations: vec![
            automation(
                MORNING_ID,
                "灯塔晨报",
                &project,
                &morning_trigger,
                &morning_action,
            ),
            automation(
                PATROL_ID,
                "港湾巡检",
                &project,
                &patrol_trigger,
                &patrol_action,
            ),
        ],
        states: vec![
            AutomationStateRecord {
                automation_id: MORNING_ID.into(),
                next_run_at: None,
                last_run_id: Some(MORNING_RUN_ID.into()),
            },
            AutomationStateRecord {
                automation_id: PATROL_ID.into(),
                next_run_at: None,
                last_run_id: None,
            },
        ],
        runs: vec![AutomationRunRecord {
            id: MORNING_RUN_ID.into(),
            automation_id: MORNING_ID.into(),
            source: "scheduled".into(),
            status: "completed".into(),
            created_at: DEMO_UNIX,
            scheduled_for: Some(DEMO_UNIX),
            started_at: Some(DEMO_UNIX + 5),
            delivery_attempt_at: None,
            delivery_attempts: 0,
            finished_at: Some(DEMO_UNIX + 120),
            session_id: None,
            provider_session_id: None,
            output: Some("灯塔欢迎语已从 hello harbor 改成 hello booth。".into()),
            error: None,
            runtime_released_at: Some(DEMO_UNIX + 120),
            context_json: serde_json::to_vec(&AutomationRunContext {
                automation_name: "灯塔晨报".into(),
                action: morning_action,
                cwd: project,
                trigger_payload: None,
                prompt: Some("汇总灯塔笔记昨晚的改动。".into()),
                agent_definition_name: Some("灯塔编辑".into()),
                engine_kind_id: Some("pi".into()),
                agent_instructions: None,
            })
            .expect("automation run context"),
        }],
    }
}

fn automation(
    id: &str,
    name: &str,
    project: &str,
    trigger: &AutomationTrigger,
    action: &AutomationAction,
) -> AutomationRecord {
    AutomationRecord {
        id: id.into(),
        name: name.into(),
        // 暂停着。打开演示包不应该到点就去拉起真实智能体。
        enabled: false,
        workspace_dir: Some(project.into()),
        trigger_json: serde_json::to_vec(trigger).expect("automation trigger"),
        action_json: serde_json::to_vec(action).expect("automation action"),
        sinks_json: b"[]".to_vec(),
    }
}

fn transcript_json() -> Result<Vec<u8>, String> {
    let entries = vec![
        AcpEntry::User("汇总灯塔笔记昨晚的改动。".into()),
        AcpEntry::Assistant {
            text: "欢迎语已经从 hello harbor 改成 hello booth。".into(),
            thought: false,
        },
        AcpEntry::tool_call(
            "demo-morning-read",
            "Read src/notes.rs",
            ToolKind::Read,
            ToolCallStatus::Completed,
            vec![ToolOutputPart::Text(notes_working().into())],
        ),
    ];
    serde_json::to_vec(&entries).map_err(|error| error.to_string())
}

fn workspace_snapshot(project: &Path) -> WorkspaceSnapshot {
    let project = project.display().to_string();
    WorkspaceSnapshot {
        projects: vec![project.clone()],
        sessions: vec![WorkspaceSessionRecord {
            id: TERMINAL_ID.into(),
            custom_title: Some("灯塔笔记".into()),
            last_updated_at: DEMO_UNIX,
            terminals: vec![WorkspaceTerminalRecord {
                id: None,
                cwd: Some(project),
                custom_title: None,
                launch_label: None,
                launch_cmd: None,
            }],
            // 不写 ACP 行。恢复 ACP 会自动拉起智能体；工具卡片对话只活在界面内存里。
            acp: None,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_stays_inside_the_given_home_and_is_idempotent() {
        let home = std::env::temp_dir().join(format!(
            "smelt-demo-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&home);
        seed_into(&home).expect("seed");
        seed_into(&home).expect("second seed");

        let project = home.join("projects").join(project_name());
        let notes = std::fs::read_to_string(project.join("src/notes.rs")).expect("notes");
        assert!(notes.contains("hello booth"));
        assert!(project.join("README.md").is_file());
        assert!(home.join(".smelt").join(SEEDED_MARKER).is_file());
        assert!(home.join(".claude").is_dir() || home.join(".smelt").is_dir());

        let store = Store::open(home.join(".smelt").join(DATABASE_FILE_NAME)).expect("store");
        let agents = store
            .get_agent_ui_snapshot()
            .expect("agents")
            .expect("rows");
        assert!(!agents.agent_hooks_enabled);
        assert_eq!(agents.agents.len(), 2);
        assert!(
            agents
                .agents
                .iter()
                .all(|agent| agent.model_id == "demo-local")
        );

        let automations = store
            .get_automation_snapshot()
            .expect("automations")
            .expect("rows");
        assert_eq!(automations.automations.len(), 2);
        assert!(automations.automations.iter().all(|row| !row.enabled));
        assert!(
            store
                .get_automation_run_transcript(MORNING_RUN_ID)
                .expect("transcript")
                .is_some()
        );

        let workspace = store
            .get_workspace_snapshot()
            .expect("workspace")
            .expect("rows");
        assert!(
            workspace
                .sessions
                .iter()
                .all(|session| session.acp.is_none())
        );
        assert_eq!(workspace.sessions.len(), 1);

        let entries = showcase_entries();
        let AcpEntry::ToolCall { output, .. } = &entries[3] else {
            panic!("edit tool");
        };
        let ToolOutputPart::Diff {
            old_text, new_text, ..
        } = &output[0]
        else {
            panic!("diff");
        };
        assert!(old_text.as_deref().unwrap_or("").contains("hello harbor"));
        assert!(new_text.contains("hello booth"));
        let permission = showcase_permission(&project);
        assert_eq!(permission.options[0].name, "允许一次");
        assert_eq!(permission.options[1].name, "拒绝");

        let _ = std::fs::remove_dir_all(&home);
    }
}
