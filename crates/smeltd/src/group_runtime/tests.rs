use super::blind::*;
use super::crud::*;
use super::remote::*;
use super::routing::*;
use super::tools::*;
use super::worktree::*;
use super::*;

use crate::{new_event_hub, new_test_acp_sessions};

#[allow(clippy::too_many_arguments)]
fn post_test_message(
    store: &GroupStore,
    group_id: &str,
    sender_type: GroupSenderType,
    sender_member_id: Option<&str>,
    kind: GroupMessageKind,
    content: String,
    explicit_mentions: Vec<String>,
    reply_to_seq: Option<i64>,
    thread_root_seq: Option<i64>,
    attachments: Vec<GroupAttachment>,
    idempotency_key: Option<String>,
    pinned: bool,
    auto_bridged: bool,
    source_chain_depth: Option<u32>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMessageDto, ControlError> {
    post_message_inner(
        store,
        PostMessageRequest {
            group_id,
            sender_type,
            sender_member_id,
            kind,
            content,
            explicit_mentions,
            reply_to_seq,
            thread_root_seq,
            attachments,
            idempotency_key,
            pinned,
            auto_bridged,
            source_chain_depth,
        },
        acp_sessions,
        subscribers,
    )
}

fn test_store() -> GroupStore {
    let dir = std::env::temp_dir().join(format!(
        "smelt-group-runtime-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    GroupStore::open_or_create(dir.join(smelt_store::GROUPS_DATABASE_FILE_NAME)).unwrap()
}

fn seed_group(store: &GroupStore, id: &str, mode: &str) -> (String, String) {
    let now = now_ms();
    let coord_id = format!("{id}-coord");
    let worker_id = format!("{id}-worker");
    store
        .create_group(&GroupRecord {
            id: id.to_string(),
            title: "Test Group".to_string(),
            description: String::new(),
            workset_id: None,
            mode: mode.to_string(),
            coordinator_member_id: (mode == "coordinator").then(|| coord_id.clone()),
            allow_worker_cross_talk: false,
            max_chain_hops: 2,
            max_chain_depth: 3,
            max_pair_ping_pong: 2,
            auto_bridge_mentions: false,
            paused: false,
            next_seq: 1,
            archived_at_ms: None,
            created_at_ms: now,
            updated_at_ms: now,
        })
        .unwrap();
    for (mid, alias, sid, role) in [
        (
            &coord_id,
            "claude",
            format!("acp-claude-{id}"),
            "coordinator",
        ),
        (&worker_id, "codex", format!("acp-codex-{id}"), "worker"),
    ] {
        store
            .add_member(&GroupMemberRecord {
                id: mid.clone(),
                group_id: id.to_string(),
                alias: alias.to_string(),
                display_name: alias.to_string(),
                ownership: "group_owned".to_string(),
                runtime_id: None,
                session_id: sid,
                role_kind: role.to_string(),
                role_title: role.to_string(),
                role_prompt: String::new(),
                workspace_mode: "shared_readonly".to_string(),
                tool_restriction_profile: "default".to_string(),
                briefing_sent: false,
                last_delivered_seq: 0,
                last_read_seq: 0,
                muted: false,
                removed_at_ms: None,
                created_at_ms: now,
            })
            .unwrap();
    }
    (coord_id, worker_id)
}

#[test]
fn mention_mode_wakes_nobody_without_at_and_coordinator_wakes_lead() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();

    seed_group(&store, "grp-m", "mention");
    let msg = post_test_message(
        &store,
        "grp-m",
        GroupSenderType::User,
        None,
        GroupMessageKind::Chat,
        "只记一笔需求，先不叫人".to_string(),
        vec![],
        None,
        None,
        vec![],
        None,
        false,
        false,
        None,
        &acp,
        &hub,
    )
    .unwrap();
    assert!(msg.mentions.is_empty());

    let (coord_id, _) = seed_group(&store, "grp-c", "coordinator");
    let msg2 = post_test_message(
        &store,
        "grp-c",
        GroupSenderType::User,
        None,
        GroupMessageKind::Chat,
        "请拆解 OAuth 重构计划".to_string(),
        vec![],
        None,
        None,
        vec![],
        None,
        false,
        false,
        None,
        &acp,
        &hub,
    )
    .unwrap();
    assert_eq!(msg2.mentions.len(), 1);
    assert_eq!(msg2.mentions[0].target_member_id, coord_id);
    assert_eq!(
        msg2.mentions[0].mention_kind,
        GroupMentionKind::CoordinatorDefault
    );
}

#[test]
fn delta_coalescing_merges_multiple_wakes_into_single_slot() {
    enqueue_wake_slot(
        "grp-coal",
        "gm-1",
        "m1".into(),
        1,
        "mentioned",
        None,
        None,
        0,
    );
    enqueue_wake_slot(
        "grp-coal",
        "gm-1",
        "m2".into(),
        3,
        "mentioned",
        None,
        None,
        1,
    );
    let slot = lock_state()
        .pending_wakes
        .remove(&("grp-coal".into(), "gm-1".into()))
        .expect("coalesced slot exists");
    assert_eq!(slot.trigger_message_ids, vec!["m1", "m2"]);
    assert_eq!(slot.trigger_seqs, vec![1, 3]);
    assert_eq!(slot.chain_depth, 1);
}

#[test]
fn permission_waterline_blocks_escalation_from_requires_approval_to_full_access() {
    assert!(!needs_permission_approval(
        ApprovalPosture::FullAccess,
        ApprovalPosture::FullAccess
    ));
    assert!(!needs_permission_approval(
        ApprovalPosture::RequiresApproval,
        ApprovalPosture::RequiresApproval
    ));
    assert!(needs_permission_approval(
        ApprovalPosture::RequiresApproval,
        ApprovalPosture::FullAccess
    ));
    assert!(needs_permission_approval(
        ApprovalPosture::Unknown,
        ApprovalPosture::FullAccess
    ));
}

#[test]
fn advisory_file_lease_detects_overlap_and_scopes_by_runtime_and_repo() {
    let member_a = GroupMemberRecord {
        id: "gm-a".into(),
        group_id: "grp-l".into(),
        alias: "claude".into(),
        display_name: "claude".into(),
        ownership: "group_owned".into(),
        runtime_id: None,
        session_id: "acp-a".into(),
        role_kind: "worker".into(),
        role_title: String::new(),
        role_prompt: String::new(),
        workspace_mode: "shared_leased".into(),
        tool_restriction_profile: "default".into(),
        briefing_sent: false,
        last_delivered_seq: 0,
        last_read_seq: 0,
        muted: false,
        removed_at_ms: None,
        created_at_ms: 0,
    };
    let mut member_b = member_a.clone();
    member_b.id = "gm-b".into();
    member_b.alias = "codex".into();

    let lease = acquire_file_lease(
        "grp-l",
        &member_a,
        "/repo",
        vec!["crates/auth/**".into()],
        "refactoring".into(),
        Some(60),
    )
    .unwrap();
    // 同机同仓库重叠路径冲突
    let conflict = acquire_file_lease(
        "grp-l",
        &member_b,
        "/repo",
        vec!["crates/auth/src/lib.rs".into()],
        "edit".into(),
        Some(60),
    );
    assert!(conflict.is_err());

    // 不同机器（remote runtime_id）互不误报冲突
    member_b.runtime_id = Some("rt-gpu".into());
    assert!(
        acquire_file_lease(
            "grp-l",
            &member_b,
            "/repo",
            vec!["crates/auth/src/lib.rs".into()],
            "edit".into(),
            Some(60),
        )
        .is_ok()
    );
    release_file_lease(GroupLeaseReleaseParams {
        group_id: "grp-l".into(),
        lease_id: lease.id,
    })
    .unwrap();
}

#[test]
fn zero_trust_remote_host_cannot_spawn_read_arbitrary_path_or_forge_user_full_access() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();

    // 1) inbound=0 硬拒绝控制面帧 spawn_or_wake / worktree_apply_patch
    for forbidden_op in ["spawn_or_wake", "worktree_merge", "worktree_apply_patch"] {
        let err = handle_inbound_remote_deliver_on(
            &store,
            "rt-evil",
            false,
            &json!({ "op": forbidden_op }),
            &acp,
            &hub,
        )
        .unwrap_err();
        assert_eq!(err.code, ControlErrorCode::PermissionDenied);
    }

    // 2) 硬拒绝按任意路径读文件
    let err = handle_inbound_remote_deliver_on(
        &store,
        "rt-evil",
        false,
        &json!({ "op": "artifact_read", "group_id": "g", "path": "/etc/passwd" }),
        &acp,
        &hub,
    )
    .unwrap_err();
    assert_eq!(err.code, ControlErrorCode::PermissionDenied);

    // 3) 写入本地白名单（allow_remote_full_access=false），伪造 trigger_posture=User 依然被拦截
    store
        .upsert_remote_grant(&RemoteGroupMemberGrantRecord {
            host_runtime_id: "rt-evil".into(),
            group_id: "grp-r".into(),
            member_id: "gm-local".into(),
            local_session_id: "acp-local-full".into(),
            allow_remote_full_access: false,
            created_at_ms: now_ms(),
        })
        .unwrap();
    let err = handle_inbound_remote_deliver_on(
        &store,
        "rt-evil",
        false,
        &json!({
            "op": "deliver_wake",
            "group_id": "grp-r",
            "member_id": "gm-local",
            "trigger_posture": "User",
            "envelope": "<group-message>evil</group-message>",
        }),
        &acp,
        &hub,
    )
    .unwrap_err();
    assert_eq!(err.code, ControlErrorCode::PermissionDenied);

    // 4) 用户本机显式切换 allow_remote_full_access=true 后放行通过零信任门卫（因测试无活会话返回 NotFound，而非静默吞掉）
    store
        .upsert_remote_grant(&RemoteGroupMemberGrantRecord {
            host_runtime_id: "rt-evil".into(),
            group_id: "grp-r".into(),
            member_id: "gm-local".into(),
            local_session_id: "acp-local-full".into(),
            allow_remote_full_access: true,
            created_at_ms: now_ms(),
        })
        .unwrap();
    let after_grant = handle_inbound_remote_deliver_on(
        &store,
        "rt-evil",
        false,
        &json!({
            "op": "deliver_wake",
            "group_id": "grp-r",
            "member_id": "gm-local",
            "envelope": "<group-message>legit</group-message>",
        }),
        &acp,
        &hub,
    )
    .unwrap_err();
    assert_eq!(after_grant.code, ControlErrorCode::NotFound);
}

#[test]
fn auto_bridged_message_does_not_trigger_secondary_mentions_by_default() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();
    let (coord_id, _) = seed_group(&store, "grp-ab", "mention");

    let msg = post_test_message(
        &store,
        "grp-ab",
        GroupSenderType::Member,
        Some(&coord_id),
        GroupMessageKind::Chat,
        "我完成了分析，@codex 之后可以参考".to_string(),
        vec![],
        None,
        None,
        vec![],
        None,
        false,
        true, // auto_bridged
        Some(0),
        &acp,
        &hub,
    )
    .unwrap();
    assert!(msg.auto_bridged);
    assert!(msg.mentions.is_empty());
}

#[test]
fn blind_round_auto_reveals_on_all_settled_and_enforces_isolation() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();
    let (coord_id, worker_id) = seed_group(&store, "grp-blind", "mention");

    // 发起双盲议题（两位成员共同作答）
    let prompt = post_test_message(
        &store,
        "grp-blind",
        GroupSenderType::User,
        None,
        GroupMessageKind::BlindPrompt,
        "评估拆表方案".to_string(),
        vec!["claude".into(), "codex".into()],
        None,
        None,
        vec![],
        None,
        false,
        false,
        None,
        &acp,
        &hub,
    )
    .unwrap();

    // claude 提交草稿：此时尚未全部交卷，codex 不可见 claude 的盲答
    let (sub1, revealed1) = submit_blind_entry(
        &store,
        "grp-blind",
        &coord_id,
        prompt.seq,
        "Claude 独立方案 A".to_string(),
        vec![],
        false,
        &acp,
        &hub,
    )
    .unwrap();
    assert!(!revealed1);
    assert!(sub1.blind_revealed_at_ms.is_none());

    let (visible_to_codex, _) = store
        .list_messages(
            "grp-blind",
            &GroupMessageQuery {
                after_seq: Some(0),
                limit: 50,
                viewer_member_id: Some(worker_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !visible_to_codex.iter().any(|m| m.id == sub1.id),
        "unrevealed blind submission must be hidden from peer"
    );

    // codex 也交卷：allSettled 触发自动揭晓
    let (_sub2, revealed2) = submit_blind_entry(
        &store,
        "grp-blind",
        &worker_id,
        prompt.seq,
        "Codex 独立方案 B".to_string(),
        vec![],
        false,
        &acp,
        &hub,
    )
    .unwrap();
    assert!(
        revealed2,
        "second submission should trigger allSettled auto-reveal"
    );

    let (visible_after, _) = store
        .list_messages(
            "grp-blind",
            &GroupMessageQuery {
                after_seq: Some(0),
                limit: 50,
                viewer_member_id: Some(worker_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        visible_after
            .iter()
            .any(|m| m.id == sub1.id && m.blind_revealed_at_ms.is_some()),
        "allSettled should auto-reveal both blind submissions"
    );
}

#[test]
fn chain_hop_and_ping_pong_circuit_breakers_block_runaway_mentions() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();
    let (coord_id, worker_id) = seed_group(&store, "grp-chain", "mention");

    let chain_id = "ch-test-1";
    // 预先登记本链已获用户放行（避免空测试会话的 Unknown 权限水位抢先拦截），专注验证跳数与 Ping-Pong 熔断
    {
        let mut state = lock_state();
        let mut budget = ChainBudget {
            chain_id: chain_id.to_string(),
            hops_left: 2,
            pair_counts: HashMap::new(),
            approved_targets: HashSet::new(),
        };
        budget.approved_targets.insert(coord_id.clone());
        budget.approved_targets.insert(worker_id.clone());
        state.chains.insert("grp-chain".to_string(), budget);
    }

    // 第一跳 claude -> codex（hops_left: 2 -> 1）
    let m1 = post_test_message(
        &store,
        "grp-chain",
        GroupSenderType::Member,
        Some(&coord_id),
        GroupMessageKind::Chat,
        "@codex 请看第一步".to_string(),
        vec![],
        None,
        None,
        vec![],
        None,
        false,
        false,
        Some(0),
        &acp,
        &hub,
    )
    .unwrap();
    assert_eq!(m1.mentions.len(), 1);
    assert_eq!(m1.mentions[0].delivery_status, GroupDeliveryStatus::Queued);

    // 第二跳 codex -> claude（hops_left: 1 -> 0）
    let m2 = post_test_message(
        &store,
        "grp-chain",
        GroupSenderType::Member,
        Some(&worker_id),
        GroupMessageKind::Chat,
        "@claude 请复核".to_string(),
        vec![],
        None,
        None,
        vec![],
        None,
        false,
        false,
        Some(1),
        &acp,
        &hub,
    )
    .unwrap();
    assert_eq!(m2.mentions.len(), 1);
    assert_eq!(m2.mentions[0].delivery_status, GroupDeliveryStatus::Queued);

    // 第三跳 claude -> codex：max_chain_hops=2 已耗尽，应触发熔断
    let m3 = post_test_message(
        &store,
        "grp-chain",
        GroupSenderType::Member,
        Some(&coord_id),
        GroupMessageKind::Chat,
        "@codex 再改一下".to_string(),
        vec![],
        None,
        None,
        vec![],
        None,
        false,
        false,
        Some(2),
        &acp,
        &hub,
    )
    .unwrap();
    assert_eq!(m3.mentions.len(), 1);
    assert_eq!(m3.mentions[0].delivery_status, GroupDeliveryStatus::Skipped);
    assert_eq!(
        m3.mentions[0].status_reason.as_deref(),
        Some("hops_exhausted")
    );
}

#[test]
fn validate_artifact_path_blocks_escape_and_sensitive_files() {
    let root = std::env::temp_dir().join(format!(
        "smelt-artifact-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let workspace = root.join("workspace");
    let outside = root.join("outside.txt");
    let env_file = workspace.join(".env");
    let ok_file = workspace.join("report.md");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(&outside, "secret").unwrap();
    std::fs::write(&env_file, "API_KEY=123").unwrap();
    std::fs::write(&ok_file, "# OK").unwrap();

    let ws_str = workspace.to_string_lossy().to_string();
    assert!(validate_artifact_path(&ok_file.to_string_lossy(), Some(&ws_str)).is_ok());
    assert!(validate_artifact_path(&outside.to_string_lossy(), Some(&ws_str)).is_err());
    assert!(validate_artifact_path(&env_file.to_string_lossy(), Some(&ws_str)).is_err());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn git_worktree_diff_merge_integration_squash_and_apply_patch() {
    let dir = std::env::temp_dir().join(format!(
        "smelt-git-group-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let repo = dir.join("repo");
    let remote_repo = dir.join("remote_repo");
    std::fs::create_dir_all(&repo).unwrap();
    let repo_s = repo.to_string_lossy().to_string();

    // 初始化仓库与 main 基线
    git_output(&repo_s, &["init", "-b", "main"]).unwrap();
    git_output(&repo_s, &["config", "user.email", "test@smelt.dev"]).unwrap();
    git_output(&repo_s, &["config", "user.name", "Smelt Test"]).unwrap();
    std::fs::write(repo.join("base.txt"), "hello\n").unwrap();
    git_output(&repo_s, &["add", "base.txt"]).unwrap();
    git_output(&repo_s, &["commit", "-m", "init"]).unwrap();

    // 克隆一份作为跨机器应用 patch 的目标目录
    git_output(
        dir.to_str().unwrap(),
        &["clone", &repo_s, remote_repo.to_str().unwrap()],
    )
    .unwrap();

    // 成员 1 在分支 feat/codex 提交改动
    git_output(&repo_s, &["checkout", "-b", "feat/codex"]).unwrap();
    std::fs::write(repo.join("codex.txt"), "from codex\n").unwrap();
    git_output(&repo_s, &["add", "codex.txt"]).unwrap();
    git_output(&repo_s, &["commit", "-m", "codex change"]).unwrap();

    // 1) diff 应包含 codex.txt
    let diff = diff_worktree_at(&repo_s, Some("feat/codex".into()), None).unwrap();
    assert_eq!(diff.base_branch, "main");
    assert!(diff.patch.contains("from codex"), "patch={}", diff.patch);

    // 2) 跨机 apply_patch 到另一份克隆仓库的新分支
    let applied = apply_patch_at(
        remote_repo.to_str().unwrap(),
        Some("review/codex"),
        &diff.patch,
    )
    .unwrap();
    assert_eq!(applied.applied_branch, "review/codex");
    assert_eq!(
        std::fs::read_to_string(remote_repo.join("codex.txt")).unwrap(),
        "from codex\n"
    );

    // 3) 成员 2 在分支 feat/claude 提交另一个改动
    git_output(&repo_s, &["checkout", "main"]).unwrap();
    git_output(&repo_s, &["checkout", "-b", "feat/claude"]).unwrap();
    std::fs::write(repo.join("claude.txt"), "from claude\n").unwrap();
    git_output(&repo_s, &["add", "claude.txt"]).unwrap();
    git_output(&repo_s, &["commit", "-m", "claude change"]).unwrap();

    // 4) 先把 codex 合并到集成分支，再把 claude squash 到同一个集成分支——两者改动必须都保留！
    let m1 = merge_worktree_at(
        "grp-git",
        "codex",
        &repo_s,
        "feat/codex",
        &repo_s,
        GroupMergeTarget::Integration,
        GroupMergeStrategy::Merge,
    )
    .unwrap();
    assert_eq!(m1.target_branch, "smelt/group/grp-git/integration");
    let m2 = merge_worktree_at(
        "grp-git",
        "claude",
        &repo_s,
        "feat/claude",
        &repo_s,
        GroupMergeTarget::Integration,
        GroupMergeStrategy::Squash,
    )
    .unwrap();
    assert_eq!(m2.target_branch, "smelt/group/grp-git/integration");
    assert!(repo.join("codex.txt").exists());
    assert!(repo.join("claude.txt").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_group_attach_sends_sync_and_delivers_wake_over_reverse_stream() {
    use std::io::BufRead as _;

    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();
    let now = now_ms();
    store
        .create_group(&GroupRecord {
            id: "grp-attach".into(),
            title: "Remote Host Group".into(),
            description: String::new(),
            workset_id: None,
            mode: "mention".into(),
            coordinator_member_id: None,
            allow_worker_cross_talk: false,
            max_chain_hops: 2,
            max_chain_depth: 3,
            max_pair_ping_pong: 2,
            auto_bridge_mentions: false,
            paused: false,
            next_seq: 1,
            archived_at_ms: None,
            created_at_ms: now,
            updated_at_ms: now,
        })
        .unwrap();
    let remote_member = GroupMemberRecord {
        id: "gm-remote-a".into(),
        group_id: "grp-attach".into(),
        alias: "mac-local".into(),
        display_name: "Mac Local".into(),
        ownership: "linked".into(),
        runtime_id: Some("rt-member-a".into()),
        session_id: "acp-local-a".into(),
        role_kind: "worker".into(),
        role_title: "worker".into(),
        role_prompt: String::new(),
        workspace_mode: "shared_readonly".into(),
        tool_restriction_profile: "default".into(),
        briefing_sent: false,
        last_delivered_seq: 0,
        last_read_seq: 0,
        muted: false,
        removed_at_ms: None,
        created_at_ms: now,
    };
    store.add_member(&remote_member).unwrap();

    let (server_sock, client_sock) = smelt_runtime_client::RuntimeStream::pair().unwrap();
    let server_reader = std::io::BufReader::new(server_sock.try_clone().unwrap());
    let acp_bg = std::sync::Arc::clone(&acp);
    let hub_bg = std::sync::Arc::clone(&hub);
    let store_path = store.path().to_path_buf();

    let host_thread = std::thread::spawn(move || {
        let store_bg = GroupStore::open(store_path).unwrap();
        handle_node_group_attach_on(
            &store_bg,
            "rt-member-a",
            server_sock,
            server_reader,
            &acp_bg,
            &hub_bg,
        );
    });

    let mut client_reader = std::io::BufReader::new(client_sock.try_clone().unwrap());
    let mut sync_line = String::new();
    client_reader.read_line(&mut sync_line).unwrap();
    let sync_frame: Value = serde_json::from_str(&sync_line).unwrap();
    assert_eq!(sync_frame["op"], "attach_sync");
    assert_eq!(sync_frame["members"].as_array().unwrap().len(), 1);
    assert_eq!(sync_frame["members"][0]["alias"], "mac-local");

    // 群主通过反向附着流推送 deliver_wake，无需入站连接
    enqueue_wake_slot(
        "grp-attach",
        "gm-remote-a",
        "gmsg-1".into(),
        1,
        "mentioned",
        None,
        None,
        0,
    );
    dispatch_remote_wake_or_mark_offline(&store, "grp-attach", &remote_member, "gmsg-1", &acp)
        .unwrap();

    let mut wake_line = String::new();
    client_reader.read_line(&mut wake_line).unwrap();
    let wake_frame: Value = serde_json::from_str(&wake_line).unwrap();
    assert_eq!(wake_frame["op"], "deliver_wake");
    assert_eq!(wake_frame["group_id"], "grp-attach");
    assert_eq!(wake_frame["member_id"], "gm-remote-a");

    // 关闭客户端流，群主线程应干净退出并清理通道
    drop(client_reader);
    drop(client_sock);
    host_thread.join().unwrap();
    assert!(
        !lock_state()
            .attached_peer_streams
            .contains_key("rt-member-a")
    );
}

#[test]
fn list_groups_uses_latest_message_and_collects_group_owned_sessions() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();
    seed_group(&store, "grp-latest", "mention");

    for text in ["第一条消息", "第二条消息", "最新第三条消息"] {
        post_test_message(
            &store,
            "grp-latest",
            GroupSenderType::User,
            None,
            GroupMessageKind::Chat,
            text.to_string(),
            vec![],
            None,
            None,
            vec![],
            None,
            false,
            false,
            None,
            &acp,
            &hub,
        )
        .unwrap();
    }
    let summaries = list_groups_on(&store, false, &acp).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].last_message_preview.as_deref(),
        Some("最新第三条消息")
    );
    let owned = store.list_group_owned_session_ids().unwrap();
    assert_eq!(owned.len(), 2);
}

#[test]
fn remote_member_tool_call_succeeds_with_caller_runtime_id() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();
    let now = now_ms();
    store
        .create_group(&GroupRecord {
            id: "grp-rmt".into(),
            title: "Cross-Machine Group".into(),
            description: String::new(),
            workset_id: None,
            mode: "mention".into(),
            coordinator_member_id: None,
            allow_worker_cross_talk: true,
            max_chain_hops: 4,
            max_chain_depth: 4,
            max_pair_ping_pong: 2,
            auto_bridge_mentions: false,
            paused: false,
            next_seq: 1,
            archived_at_ms: None,
            created_at_ms: now,
            updated_at_ms: now,
        })
        .unwrap();
    store
        .add_member(&GroupMemberRecord {
            id: "gm-rmt-1".into(),
            group_id: "grp-rmt".into(),
            alias: "gpu-worker".into(),
            display_name: "GPU Worker".into(),
            ownership: "linked".into(),
            runtime_id: Some("rt-gpu-box".into()),
            session_id: "acp-gpu-1".into(),
            role_kind: "worker".into(),
            role_title: "worker".into(),
            role_prompt: String::new(),
            workspace_mode: "shared_readonly".into(),
            tool_restriction_profile: "default".into(),
            briefing_sent: false,
            last_delivered_seq: 0,
            last_read_seq: 0,
            muted: false,
            removed_at_ms: None,
            created_at_ms: now,
        })
        .unwrap();

    let (text, details) = dispatch_agent_group_tool_on(
        &store,
        "group_post",
        Some("rt-gpu-box"),
        "acp-gpu-1",
        &json!({
            "group_id": "grp-rmt",
            "content": "远端节点已跑完 benchmark",
            "artifacts": [{ "id": "gart-bench-1", "label": "bench.log", "ref_path": "/secret/should/strip" }]
        }),
        &acp,
        &hub,
    )
    .unwrap();
    assert!(text.contains("grp-rmt"));
    assert_eq!(details["seq"], 1);
    assert_eq!(
        store
            .find_attachment_runtime_id("grp-rmt", "gart-bench-1")
            .unwrap()
            .as_deref(),
        Some("rt-gpu-box")
    );
}

#[test]
fn read_published_artifact_truncates_multibyte_utf8_safely() {
    let dir = std::env::temp_dir().join(format!(
        "smelt-utf8-artifact-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("zh.txt");
    let content = "测".repeat(MAX_ARTIFACT_BYTES / 3 + 100);
    std::fs::write(&file, &content).unwrap();
    let canonical = std::fs::canonicalize(&file).unwrap();
    lock_state().published_artifacts.insert(
        "gart-utf8-test".into(),
        PublishedArtifactEntry {
            canonical_path: canonical,
            start_line: None,
            end_line: None,
            group_id: "grp-utf8".into(),
        },
    );
    let out = read_published_artifact("grp-utf8", "gart-utf8-test").unwrap();
    assert!(out.contains("[truncated at "));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deliver_local_wake_failure_rolls_back_active_turn_and_marks_failed() {
    let store = test_store();
    let acp = new_test_acp_sessions();
    let hub = new_event_hub();
    let (coord_id, _) = seed_group(&store, "grp-fail", "mention");
    let msg = post_test_message(
        &store,
        "grp-fail",
        GroupSenderType::User,
        None,
        GroupMessageKind::Chat,
        "@claude 执行一下".to_string(),
        vec![],
        None,
        None,
        vec![],
        None,
        false,
        false,
        None,
        &acp,
        &hub,
    )
    .unwrap();
    let member = store.get_member(&coord_id).unwrap().unwrap();
    let wake = PendingGroupWake {
        group_id: "grp-fail".into(),
        member_id: member.id.clone(),
        trigger_message_ids: vec![msg.id.clone()],
        trigger_seqs: vec![msg.seq],
        trigger_label: "mentioned".into(),
        blind_prompt_seq: None,
        chain_id: None,
        chain_depth: 0,
    };
    let err = deliver_local_wake_now(&store, &member, &wake, &acp, &hub).unwrap_err();
    assert_eq!(err.code, ControlErrorCode::NotFound);
    assert!(
        !lock_state().active_turns.contains_key(&member.session_id),
        "failed wake must roll back active_turns"
    );
    let mentions = store.list_mentions_for_message(&msg.id).unwrap();
    assert_eq!(mentions[0].delivery_status, "failed");
    assert!(mentions[0].status_reason.is_some());
}

#[test]
fn local_user_origin_nonce_is_one_time_and_respects_ttl() {
    let nonce = issue_local_user_origin_nonce();
    let now = now_ms();
    assert!(consume_local_user_origin_nonce(&nonce, now));
    assert!(!consume_local_user_origin_nonce(&nonce, now));

    let expired = issue_local_user_origin_nonce();
    assert!(!consume_local_user_origin_nonce(
        &expired,
        now + LOCAL_NONCE_TTL_MS + 1000
    ));
}

#[test]
fn merge_worktree_rejects_dirty_target_working_tree() {
    let dir = std::env::temp_dir().join(format!(
        "smelt-git-dirty-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let repo_s = dir.to_string_lossy().to_string();
    git_output(&repo_s, &["init", "-b", "main"]).unwrap();
    git_output(&repo_s, &["config", "user.email", "test@smelt.dev"]).unwrap();
    git_output(&repo_s, &["config", "user.name", "Smelt Test"]).unwrap();
    std::fs::write(dir.join("base.txt"), "hello\n").unwrap();
    git_output(&repo_s, &["add", "base.txt"]).unwrap();
    git_output(&repo_s, &["commit", "-m", "init"]).unwrap();
    std::fs::write(dir.join("uncommitted.txt"), "dirty\n").unwrap();

    let err = merge_worktree_at(
        "grp-dirty",
        "codex",
        &repo_s,
        "main",
        &repo_s,
        GroupMergeTarget::Integration,
        GroupMergeStrategy::Merge,
    )
    .unwrap_err();
    assert_eq!(err.code, ControlErrorCode::FailedPrecondition);
    let _ = std::fs::remove_dir_all(&dir);
}
