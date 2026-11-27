use std::path::PathBuf;

use serde_json::json;
use smelt_core::agent_group::{
    GroupDeliveryStatus, GroupMentionKind, GroupMessageKind, GroupMode, GroupTaskStatus,
    MemberOwnership, MemberRoleKind, deterministic_group_delivery_id,
};
use smelt_core::agent_kind::ApprovalPosture;
use smelt_core::control_api::{
    GroupCreateParams, GroupMentionActionParams, GroupMessageListParams, GroupMessagePostParams,
    GroupPauseParams, GroupResumeParams,
};
use smelt_store::{AgentGroupMemberRecord, Store};

use super::crud::{
    create_group_with_store, list_messages_with_store, pause_group_with_store,
    resume_group_with_store,
};
use super::routing::{
    approve_mention_with_store, evaluate_member_mention, on_member_turn_ended_with_store,
    post_member_message_with_store, post_user_message_with_store, reject_mention_with_store,
    session_has_mcp_or_pi,
};
use super::tools::dispatch_agent_group_tool_with_store;
use super::{ChainBudget, on_delivery_settled_with_store};
use crate::{WorkspaceDirectoryReader, event_hub::DaemonEventHub, new_test_acp_sessions};

fn temp_store(label: &str) -> (PathBuf, Store) {
    let dir = std::env::temp_dir().join(format!(
        "smelt-group-runtime-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join(smelt_store::DATABASE_FILE_NAME);
    let store = Store::open_or_create(&db).unwrap();
    (dir, store)
}

fn test_hub() -> crate::EventHubHandle {
    DaemonEventHub::new(WorkspaceDirectoryReader::unavailable()).unwrap()
}

fn insert_member(
    store: &Store,
    group_id: &str,
    id: &str,
    alias: &str,
    session_id: &str,
    ownership: MemberOwnership,
    role_kind: MemberRoleKind,
    profile: &str,
) -> AgentGroupMemberRecord {
    let record = AgentGroupMemberRecord {
        id: id.into(),
        group_id: group_id.into(),
        alias: alias.into(),
        display_name: alias.into(),
        ownership: ownership.as_str().into(),
        session_id: session_id.into(),
        role_kind: role_kind.as_str().into(),
        role_title: String::new(),
        role_prompt: String::new(),
        tool_restriction_profile: profile.into(),
        briefing_sent: false,
        last_delivered_seq: 0,
        last_read_seq: 0,
        muted: false,
        removed_at_ms: None,
        created_at_ms: 100,
    };
    store.insert_agent_group_member(&record, 100).unwrap();
    record
}

#[test]
fn coordinator_mode_routes_unmentioned_user_message_to_coordinator_and_settles() {
    let (_dir, store) = temp_store("coordinator-routing");
    let acp = new_test_acp_sessions();
    let hub = test_hub();
    let dir_reader = WorkspaceDirectoryReader::unavailable();

    let created = create_group_with_store(
        &store,
        GroupCreateParams {
            title: "Arch".into(),
            mode: GroupMode::Coordinator,
            ..Default::default()
        },
        &dir_reader,
        &acp,
        &hub,
    )
    .unwrap();
    let coord = insert_member(
        &store,
        &created.id,
        "mem-coord",
        "lead",
        "acp-lead",
        MemberOwnership::GroupOwned,
        MemberRoleKind::Coordinator,
        "default",
    );
    let mut group_rec = store.get_agent_group(&created.id).unwrap().unwrap();
    group_rec.coordinator_member_id = Some(coord.id.clone());
    store.update_agent_group(&group_rec).unwrap();

    let posted = post_user_message_with_store(
        &store,
        GroupMessagePostParams {
            group_id: created.id.clone(),
            content: "Please break down the migration plan".into(),
            kind: GroupMessageKind::Task,
            task_status: Some(GroupTaskStatus::Open),
            ..Default::default()
        },
        &acp,
        &hub,
    )
    .unwrap();
    assert_eq!(posted.seq, 1);
    assert_eq!(posted.mentions.len(), 1);
    assert_eq!(
        posted.mentions[0].mention_kind,
        GroupMentionKind::CoordinatorDefault
    );
    assert_eq!(
        posted.mentions[0].delivery_id,
        deterministic_group_delivery_id(&posted.id, &coord.id)
    );
    assert_eq!(
        posted.mentions[0].delivery_status,
        GroupDeliveryStatus::Queued
    );

    on_delivery_settled_with_store(&store, &posted.mentions[0].delivery_id, Ok(()), &hub);
    let updated_coord = store.get_agent_group_member(&coord.id).unwrap().unwrap();
    assert!(updated_coord.briefing_sent);
    assert_eq!(updated_coord.last_delivered_seq, 1);
}

#[test]
fn permission_waterline_holds_read_only_to_writer_mention_for_approval() {
    let (_dir, store) = temp_store("waterline");
    let acp = new_test_acp_sessions();
    let hub = test_hub();
    let dir_reader = WorkspaceDirectoryReader::unavailable();

    let created = create_group_with_store(
        &store,
        GroupCreateParams {
            title: "Review".into(),
            ..Default::default()
        },
        &dir_reader,
        &acp,
        &hub,
    )
    .unwrap();
    let reviewer = insert_member(
        &store,
        &created.id,
        "mem-rev",
        "reviewer",
        "acp-rev",
        MemberOwnership::Linked,
        MemberRoleKind::Reviewer,
        "read_only",
    );
    let coder = insert_member(
        &store,
        &created.id,
        "mem-coder",
        "coder",
        "acp-coder",
        MemberOwnership::GroupOwned,
        MemberRoleKind::Worker,
        "default",
    );

    let msg = post_member_message_with_store(
        &store,
        &created.id,
        &reviewer,
        "@coder fix the SQL injection".into(),
        GroupMessageKind::Chat,
        None,
        None,
        None,
        false,
        &acp,
        &hub,
    )
    .unwrap();
    assert_eq!(msg.mentions.len(), 1);
    assert_eq!(
        msg.mentions[0].delivery_status,
        GroupDeliveryStatus::PendingApproval
    );
    let original_delivery_id = msg.mentions[0].delivery_id.clone();

    let approved = approve_mention_with_store(
        &store,
        GroupMentionActionParams {
            group_id: created.id.clone(),
            message_id: msg.id.clone(),
            target_member_id: coder.id.clone(),
            reason: None,
        },
        &acp,
        &hub,
    )
    .unwrap();
    assert!(approved.updated);
    assert_eq!(approved.delivery_status, GroupDeliveryStatus::Queued);

    let reloaded = store
        .get_agent_group_mention(&msg.id, &coder.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        reloaded.mention.delivery_id, original_delivery_id,
        "approving must reuse deterministic delivery_id"
    );

    // Rejecting an already-approved mention is a no-op.
    let reject_after = reject_mention_with_store(
        &store,
        GroupMentionActionParams {
            group_id: created.id,
            message_id: msg.id,
            target_member_id: coder.id,
            reason: Some("too late".into()),
        },
        &hub,
    )
    .unwrap();
    assert!(!reject_after.updated);
}

#[test]
fn circuit_breakers_and_cross_talk_skip_over_budget_mentions() {
    let (_dir, store) = temp_store("breakers");
    let acp = new_test_acp_sessions();
    let hub = test_hub();
    let dir_reader = WorkspaceDirectoryReader::unavailable();

    let created = create_group_with_store(
        &store,
        GroupCreateParams {
            title: "Guarded".into(),
            allow_worker_cross_talk: true,
            max_chain_hops: 2,
            max_chain_depth: 3,
            max_pair_ping_pong: 1,
            ..Default::default()
        },
        &dir_reader,
        &acp,
        &hub,
    )
    .unwrap();
    let mut group = store.get_agent_group(&created.id).unwrap().unwrap();
    let a = insert_member(
        &store,
        &group.id,
        "mem-a",
        "alpha",
        "acp-a",
        MemberOwnership::GroupOwned,
        MemberRoleKind::Worker,
        "default",
    );
    let b = insert_member(
        &store,
        &group.id,
        "mem-b",
        "beta",
        "acp-b",
        MemberOwnership::GroupOwned,
        MemberRoleKind::Worker,
        "default",
    );
    let mut budget = ChainBudget::fresh("ch-1".into());

    let d1 = evaluate_member_mention(
        &group,
        &a,
        &b,
        ApprovalPosture::FullAccess,
        ApprovalPosture::FullAccess,
        1,
        &mut budget,
    );
    assert_eq!(d1.status, GroupDeliveryStatus::Queued);

    // Second ping-pong between the same pair hits max_pair_ping_pong=1.
    let d2 = evaluate_member_mention(
        &group,
        &b,
        &a,
        ApprovalPosture::FullAccess,
        ApprovalPosture::FullAccess,
        2,
        &mut budget,
    );
    assert_eq!(d2.status, GroupDeliveryStatus::Skipped);
    assert!(d2.reason.unwrap().contains("max_pair_ping_pong"));

    // Depth exceeding max_chain_depth=3 is skipped.
    let d3 = evaluate_member_mention(
        &group,
        &a,
        &b,
        ApprovalPosture::FullAccess,
        ApprovalPosture::FullAccess,
        4,
        &mut budget,
    );
    assert_eq!(d3.status, GroupDeliveryStatus::Skipped);
    assert!(d3.reason.unwrap().contains("max_chain_depth"));

    // Disabling worker cross-talk blocks worker -> worker.
    group.allow_worker_cross_talk = false;
    let d4 = evaluate_member_mention(
        &group,
        &a,
        &b,
        ApprovalPosture::FullAccess,
        ApprovalPosture::FullAccess,
        1,
        &mut budget,
    );
    assert_eq!(d4.status, GroupDeliveryStatus::Skipped);
    assert_eq!(d4.reason.as_deref(), Some("worker_cross_talk_disabled"));
}

#[test]
fn auto_bridge_respects_guardrail_and_skips_when_member_already_acted() {
    let (_dir, store) = temp_store("auto-bridge");
    let acp = new_test_acp_sessions();
    let hub = test_hub();
    let dir_reader = WorkspaceDirectoryReader::unavailable();

    let created = create_group_with_store(
        &store,
        GroupCreateParams {
            title: "Bridge".into(),
            auto_bridge_mentions: false,
            ..Default::default()
        },
        &dir_reader,
        &acp,
        &hub,
    )
    .unwrap();
    let _a = insert_member(
        &store,
        &created.id,
        "mem-a",
        "alpha",
        "acp-a",
        MemberOwnership::GroupOwned,
        MemberRoleKind::Worker,
        "default",
    );
    let _b = insert_member(
        &store,
        &created.id,
        "mem-b",
        "beta",
        "acp-b",
        MemberOwnership::GroupOwned,
        MemberRoleKind::Worker,
        "default",
    );

    // Default auto_bridge_mentions=false: nothing bridged.
    on_member_turn_ended_with_store(&store, "acp-a", "@beta check this", &acp, &hub);
    assert!(
        list_messages_with_store(
            &store,
            GroupMessageListParams {
                group_id: created.id.clone(),
                ..Default::default()
            }
        )
        .unwrap()
        .messages
        .is_empty()
    );

    // Enable auto_bridge_mentions: bridges message with valid @beta.
    let mut rec = store.get_agent_group(&created.id).unwrap().unwrap();
    rec.auto_bridge_mentions = true;
    store.update_agent_group(&rec).unwrap();

    on_member_turn_ended_with_store(&store, "acp-a", "@beta check this", &acp, &hub);
    let msgs = list_messages_with_store(
        &store,
        GroupMessageListParams {
            group_id: created.id.clone(),
            ..Default::default()
        },
    )
    .unwrap()
    .messages;
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].auto_bridged);

    // After explicit group_yield, turn end does NOT auto-bridge.
    let yielded = dispatch_agent_group_tool_with_store(
        &store,
        "group_yield",
        "acp-a",
        &json!({"group_id": created.id, "up_to_seq": 1}),
        &acp,
        &dir_reader,
        &hub,
    );
    assert_eq!(yielded["ok"], true);
    on_member_turn_ended_with_store(&store, "acp-a", "@beta second time", &acp, &hub);
    let msgs2 = list_messages_with_store(
        &store,
        GroupMessageListParams {
            group_id: created.id,
            ..Default::default()
        },
    )
    .unwrap()
    .messages;
    assert_eq!(msgs2.len(), 1);
}

#[test]
fn agent_tool_enforces_membership_and_session_has_mcp_is_fail_closed() {
    let (_dir, store) = temp_store("tools-auth");
    let acp = new_test_acp_sessions();
    let hub = test_hub();
    let dir_reader = WorkspaceDirectoryReader::unavailable();

    assert!(
        !session_has_mcp_or_pi("acp-missing", &acp),
        "missing session must fail closed to CLI fallback"
    );

    let created = create_group_with_store(
        &store,
        GroupCreateParams {
            title: "Team".into(),
            ..Default::default()
        },
        &dir_reader,
        &acp,
        &hub,
    )
    .unwrap();
    let _a = insert_member(
        &store,
        &created.id,
        "mem-a",
        "alpha",
        "acp-a",
        MemberOwnership::GroupOwned,
        MemberRoleKind::Worker,
        "default",
    );

    // Outsider cannot read or post.
    let denied = dispatch_agent_group_tool_with_store(
        &store,
        "group_read",
        "acp-outsider",
        &json!({"group_id": created.id}),
        &acp,
        &dir_reader,
        &hub,
    );
    assert_eq!(denied["ok"], false);
    assert_eq!(denied["reason"], "permission_denied");

    // Pause + resume works cleanly on a single group.
    let paused = pause_group_with_store(
        &store,
        GroupPauseParams {
            group_id: created.id.clone(),
            abort_running: true,
        },
        &dir_reader,
        &acp,
        &hub,
    )
    .unwrap();
    assert!(paused.paused);

    let resumed = resume_group_with_store(
        &store,
        GroupResumeParams {
            group_id: created.id,
        },
        &dir_reader,
        &acp,
        &hub,
    )
    .unwrap();
    assert!(!resumed.paused);
}
