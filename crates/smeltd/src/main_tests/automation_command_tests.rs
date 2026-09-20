use super::*;
use smelt_core::automation::{
    Automation, AutomationCommand, AutomationFile, AutomationSchedule, AutomationTrigger,
};

fn call_automation(
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
    request: serde_json::Value,
) -> serde_json::Value {
    let (server, mut client) = UnixStream::pair().unwrap();
    writeln!(client, "{request}").unwrap();
    handle_conn(
        server,
        ServerContext {
            sessions: new_sessions(),
            acp_sessions: new_test_acp_sessions(),
            remote_sessions: new_test_remote_sessions(),
            workspace_menu: new_test_workspace_menu(),
            automations: Arc::clone(automations),
            exe_mtime: 0,
            daemon_fingerprint: None,
            listen_fd: -1,
            remote_state: new_remote_state(Some(uuid::Uuid::new_v4().simple().to_string())),
            iroh_state: Arc::new(Mutex::new(None)),
            iroh_connections: new_iroh_connections(),
            event_hub: Arc::clone(event_hub),
        },
    );
    let mut line = String::new();
    BufReader::new(client).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn automation_commands_and_snapshots_share_one_daemon_owner() {
    let automations = new_test_automation_store();
    let event_hub = new_event_hub();
    let automation = Automation {
        id: "automation-1".into(),
        name: "Morning report".into(),
        enabled: true,
        workspace_dir: Some("/tmp".into()),
        trigger: AutomationTrigger::schedule(AutomationSchedule::EveryMinutes { minutes: 30 }),
        action: smelt_core::automation::AutomationAction::Agent {
            agent_definition_id: "missing-agent".into(),
            prompt: Some("Summarize the project".into()),
        },
        sinks: Vec::new(),
    };

    let response = call_automation(
        &automations,
        &event_hub,
        serde_json::json!({
            "op": "automation_command",
            "command": AutomationCommand::Upsert {
                automation: Box::new(automation),
            }
        }),
    );
    assert_eq!(response["ok"], true);
    let committed: AutomationFile =
        serde_json::from_value(response["automations"].clone()).unwrap();
    assert_eq!(committed.revision, 1);
    assert!(!committed.store_id.is_empty());
    assert_eq!(
        event_hub.legacy_snapshot().unwrap()["automations"]["revision"],
        1
    );

    let snapshot = call_automation(
        &automations,
        &event_hub,
        serde_json::json!({"op": "automations_snapshot"}),
    );
    let snapshot: AutomationFile = serde_json::from_value(snapshot["automations"].clone()).unwrap();
    assert_eq!(snapshot.store_id, committed.store_id);
    assert_eq!(snapshot.revision, committed.revision);
    assert_eq!(snapshot.automations.len(), 1);

    let failed_run = call_automation(
        &automations,
        &event_hub,
        serde_json::json!({
            "op": "automation_command",
            "command": AutomationCommand::RunOnce {
                automation_id: "automation-1".into(),
            }
        }),
    );
    assert_eq!(failed_run["ok"], false);
    assert!(failed_run["error"].as_str().unwrap().contains("智能体"));
    let after_failure = automations.lock().unwrap().snapshot();
    assert_eq!(after_failure.revision, 1);
    assert!(after_failure.runs.is_empty());
}

#[test]
fn event_publish_matches_subscribers_without_automation_id() {
    let automations = new_test_automation_store();
    let event_hub = new_event_hub();
    let upsert = call_automation(
        &automations,
        &event_hub,
        serde_json::json!({
            "op": "automation_command",
            "command": AutomationCommand::Upsert {
                automation: Box::new(Automation {
                    id: "lark-mentions".into(),
                    name: "飞书 @我".into(),
                    enabled: true,
                    workspace_dir: Some("/tmp".into()),
                    trigger: AutomationTrigger::event(
                        "lark.message.received",
                        Some(r#"{"mention":true}"#.into()),
                    ),
                    action: smelt_core::automation::AutomationAction::shell(
                        "true",
                        Vec::new(),
                    ),
                    sinks: Vec::new(),
                }),
            }
        }),
    );
    assert_eq!(upsert["ok"], true);

    let published = call_automation(
        &automations,
        &event_hub,
        serde_json::json!({
            "op": "event_publish",
            "event": {
                "topic": "lark.message.received",
                "event_id": "om_1",
                "payload": { "text": "@bot", "mention": true }
            }
        }),
    );
    assert_eq!(published["ok"], true);
    assert_eq!(published["topic"], "lark.message.received");
    assert_eq!(published["matched"], 1);
    assert_eq!(published["accepted"], 1);
    assert_eq!(published["duplicate"], false);
    assert_eq!(published["runs"][0]["automation_id"], "lark-mentions");
    assert_eq!(published["runs"][0]["status"], "starting");

    let missed = call_automation(
        &automations,
        &event_hub,
        serde_json::json!({
            "op": "event_publish",
            "event": {
                "topic": "git.push.received",
                "event_id": "sha_1",
                "payload": { "ref": "main" }
            }
        }),
    );
    assert_eq!(missed["ok"], true);
    assert_eq!(missed["matched"], 0);
    assert_eq!(missed["accepted"], 0);

    let again = call_automation(
        &automations,
        &event_hub,
        serde_json::json!({
            "op": "event_publish",
            "event": {
                "topic": "lark.message.received",
                "event_id": "om_1",
                "payload": { "text": "@bot", "mention": true }
            }
        }),
    );
    assert_eq!(again["ok"], true);
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["accepted"], 0);
    assert_eq!(again["matched"], 1);
}
