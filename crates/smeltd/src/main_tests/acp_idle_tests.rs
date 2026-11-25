use super::*;
use smelt_core::acp_session::{
    AcpSessionState, AcpUserAction, BackgroundTaskStatus, BackgroundTaskView,
};
use smelt_core::daemon_state::DaemonPhase;

fn quiet_state() -> AcpSessionState {
    let mut state = AcpSessionState::default();
    state.phase = DaemonPhase::Idle;
    state
}

fn quiet_with(change: impl FnOnce(&mut AcpSessionState)) -> AcpSessionState {
    let mut state = quiet_state();
    change(&mut state);
    state
}

/// 过了闲置期的「现在」。
fn long_after_now() -> u64 {
    crate::event_hub::now_ms() + acp_idle::ACP_IDLE_SUSPEND_AFTER.as_millis() as u64 + 1_000
}

fn direct_session(
    reduced: AcpSessionState,
) -> (
    AcpSession,
    smol::channel::Receiver<smelt_core::acp_conn::ConversationCommand>,
) {
    let session = make_acp_session_value("acp-idle", reduced);
    let (cmd_tx, cmd_rx) = smol::channel::unbounded();
    let (_event_tx, event_rx) = smol::channel::unbounded();
    *session.handle.lock() = Some(smelt_core::acp_conn::ConversationHandle {
        cmd_tx,
        event_rx,
        stdio: Arc::new(Mutex::new(None)),
        in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    });
    (session, cmd_rx)
}

#[test]
fn a_quiet_conversation_can_be_suspended() {
    assert_eq!(acp_idle::reduced_suspend_blocker(&quiet_state()), None);
}

#[test]
fn work_in_progress_blocks_suspension() {
    let cases: Vec<(&str, AcpSessionState)> = vec![
        (
            "connecting",
            quiet_with(|state| state.phase = DaemonPhase::Connecting),
        ),
        (
            "thinking",
            quiet_with(|state| state.phase = DaemonPhase::Thinking),
        ),
        (
            "turn clock still running",
            quiet_with(|state| state.turn_started_at_ms = Some(1)),
        ),
        ("compacting", quiet_with(|state| state.compacting = true)),
        (
            "replaying history",
            quiet_with(|state| state.replaying_history = true),
        ),
        (
            "queued follow-up",
            quiet_with(|state| state.queued_follow_up = vec!["next".into()]),
        ),
    ];
    for (name, state) in cases {
        assert!(
            acp_idle::reduced_suspend_blocker(&state).is_some(),
            "{name} 不能挂起"
        );
    }
}

#[test]
fn only_running_background_tasks_block_suspension() {
    let task = |status| BackgroundTaskView {
        id: "t".into(),
        title: "t".into(),
        command: "sleep 1".into(),
        status,
        exit_code: None,
        output: String::new(),
        started_at_ms: 0,
        finished_at_ms: None,
    };
    let running =
        quiet_with(|state| state.background_tasks = vec![task(BackgroundTaskStatus::Running)]);
    let finished =
        quiet_with(|state| state.background_tasks = vec![task(BackgroundTaskStatus::Completed)]);
    assert!(acp_idle::reduced_suspend_blocker(&running).is_some());
    assert_eq!(acp_idle::reduced_suspend_blocker(&finished), None);
}

#[test]
fn only_undelivered_peer_messages_block_suspension() {
    use smelt_core::acp_session::{PeerMessageKind, PeerMessageStatus, PeerMessageView};
    let message = |status| PeerMessageView {
        id: "pm-1".into(),
        source_session_id: "acp-a".into(),
        source_title: "修登录".into(),
        kind: PeerMessageKind::Task,
        status,
        queued_at_ms: 0,
        delivered_at_ms: None,
    };
    let pending = quiet_with(|state| state.peer_inbox = vec![message(PeerMessageStatus::Pending)]);
    let delivered =
        quiet_with(|state| state.peer_inbox = vec![message(PeerMessageStatus::Delivered)]);
    assert_eq!(
        acp_idle::reduced_suspend_blocker(&pending),
        Some("peer message pending")
    );
    assert_eq!(acp_idle::reduced_suspend_blocker(&delivered), None);
}

#[test]
fn a_live_conversation_idle_long_enough_can_be_suspended() {
    let (session, _cmd_rx) = direct_session(quiet_state());
    assert_eq!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER),
        None
    );
}

#[test]
fn recent_activity_blocks_suspension() {
    let (session, _cmd_rx) = direct_session(quiet_state());
    assert!(
        acp_idle::acp_suspend_blocker(
            &session,
            crate::event_hub::now_ms(),
            acp_idle::ACP_IDLE_SUSPEND_AFTER
        )
        .is_some(),
        "刚建好的会话不能在第一轮扫描就挂起"
    );
}

#[test]
fn sessions_without_a_runtime_are_left_alone() {
    let session = make_acp_session_value("acp-direct", quiet_state());
    assert_eq!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER),
        Some("no runtime")
    );
}

#[test]
fn daemon_owned_conversations_are_never_suspended() {
    let (session, _cmd_rx) = direct_session(quiet_state());
    *session.conversation_binding.lock() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run".into(),
        });
    assert_eq!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER),
        Some("daemon-owned binding")
    );
}

#[test]
fn a_held_prompt_gate_blocks_suspension() {
    let (session, _cmd_rx) = direct_session(quiet_state());
    session.prompt_in_flight.store(true, Ordering::SeqCst);
    assert!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER)
            .is_some()
    );
}

#[test]
fn reading_the_mirror_does_not_wake_a_conversation() {
    assert!(!acp_idle::acp_action_needs_runtime(&AcpUserAction::Refresh));
    assert!(!acp_idle::acp_action_needs_runtime(&AcpUserAction::Cancel));
    assert!(!acp_idle::acp_action_needs_runtime(
        &AcpUserAction::RefreshModels
    ));
    assert!(
        !acp_idle::acp_action_needs_runtime(&AcpUserAction::StopBackgroundTask {
            id: "bg-1".into()
        }),
        "有后台任务在跑的会话不会挂起，挂起的会话没有任务可停"
    );
    assert!(acp_idle::acp_action_needs_runtime(&AcpUserAction::Prompt {
        text: "hi".into(),
        images: Vec::new(),
        delivery_id: None,
    }));
    assert!(acp_idle::acp_action_needs_runtime(&AcpUserAction::Compact));
}

#[test]
fn waking_a_running_conversation_only_records_activity() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-awake", || {
        make_acp_session_value("acp-awake", quiet_state())
    });
    let subscribers = new_event_hub();
    assert_eq!(
        acp_idle::wake_suspended_acp_session("acp-awake", &slot, &acp_sessions, &subscribers),
        Ok(())
    );
    assert!(!slot.value.idle.is_suspended());
    assert!(
        slot.value.launch_spec.lock().is_none(),
        "没挂起就不该重新拉起"
    );
}

fn pi_model(current: &str) -> smelt_core::acp_conn::ModelState {
    smelt_core::acp_conn::ModelState {
        config_id: "model".into(),
        current_value: current.into(),
        current_name: current.into(),
        options: vec![
            ("openai/gpt-5".into(), "openai/gpt-5".into()),
            ("deepseek/flash".into(), "deepseek/flash".into()),
        ],
        provider_groups: Vec::new(),
    }
}

fn select(
    id: &str,
    current: &str,
    options: &[(&str, &str)],
) -> smelt_core::acp_conn::SessionConfigState {
    smelt_core::acp_conn::SessionConfigState {
        config_id: id.into(),
        name: id.into(),
        description: None,
        current_name: current.into(),
        options: options
            .iter()
            .map(|(value, name)| (value.to_string(), name.to_string()))
            .collect(),
        boolean: None,
        smelt: None,
        is_thought_level: false,
    }
}

fn mode(current: &str) -> smelt_core::acp_conn::SessionConfigState {
    select(
        "mode",
        current,
        &[
            ("default", "Ask each time"),
            ("bypassPermissions", "Full access"),
        ],
    )
}

fn thinking(current: &str) -> smelt_core::acp_conn::SessionConfigState {
    select(
        "thought_level",
        current,
        &[("medium", "Medium"), ("high", "High")],
    )
}

fn switch(id: &str, on: bool) -> smelt_core::acp_conn::SessionConfigState {
    smelt_core::acp_conn::SessionConfigState {
        boolean: Some(on),
        ..select(id, "", &[])
    }
}

/// 挂起前用户调过的一组配置：换了模型、开了Full access、推理拉到高、开了开关。
fn tuned_state() -> AcpSessionState {
    quiet_with(|state| {
        state.model = Some(pi_model("deepseek/flash"));
        state.config_options = vec![
            mode("Full access"),
            thinking("High"),
            switch("codemode", true),
        ];
    })
}

/// 新 runtime 起来时的默认值。
fn fresh_defaults() -> AcpSessionState {
    quiet_with(|state| {
        state.model = Some(pi_model("openai/gpt-5"));
        state.config_options = vec![
            mode("Ask each time"),
            thinking("Medium"),
            switch("codemode", false),
        ];
    })
}

fn set(config_id: &str, value_id: &str, boolean: Option<bool>) -> String {
    format!(
        "{:?}",
        AcpUserAction::SetConfigOption {
            config_id: config_id.into(),
            value_id: value_id.into(),
            boolean,
        }
    )
}

fn debug_all(actions: impl IntoIterator<Item = AcpUserAction>) -> Vec<String> {
    actions
        .into_iter()
        .map(|action| format!("{action:?}"))
        .collect()
}

#[test]
fn waking_restores_every_setting_that_fell_back_to_default() {
    let saved = acp_idle::SuspendedConfig::capture(&tuned_state());
    let fresh = fresh_defaults();
    assert_eq!(
        debug_all(saved.model_action(&fresh)),
        [set("model", "deepseek/flash", None)]
    );
    assert_eq!(
        debug_all(saved.option_actions(&fresh)),
        [
            set("mode", "bypassPermissions", None),
            set("thought_level", "high", None),
            set("codemode", "true", Some(true)),
        ]
    );
}

#[test]
fn settings_the_new_runtime_already_has_are_not_resent() {
    let saved = acp_idle::SuspendedConfig::capture(&tuned_state());
    let same = tuned_state();
    assert!(saved.model_action(&same).is_none());
    assert!(saved.option_actions(&same).is_empty());
}

#[test]
fn settings_the_new_runtime_no_longer_offers_are_skipped() {
    let saved = acp_idle::SuspendedConfig::capture(&tuned_state());
    let narrowed = quiet_with(|state| {
        let mut model = pi_model("openai/gpt-5");
        model.options.retain(|(value, _)| value == "openai/gpt-5");
        state.model = Some(model);
        // 推理强度只剩 medium，codemode 整项没了。
        state.config_options = vec![
            mode("Ask each time"),
            select("thought_level", "Medium", &[("medium", "Medium")]),
        ];
    });
    assert!(
        saved.model_action(&narrowed).is_none(),
        "模型已下架就不硬塞"
    );
    assert_eq!(
        debug_all(saved.option_actions(&narrowed)),
        [set("mode", "bypassPermissions", None)]
    );
}

#[test]
fn a_model_also_listed_as_a_config_option_is_restored_once() {
    let mut tuned = tuned_state();
    tuned.config_options.push(select(
        "model",
        "deepseek/flash",
        &[("deepseek/flash", "deepseek/flash")],
    ));
    let saved = acp_idle::SuspendedConfig::capture(&tuned);
    let mut fresh = fresh_defaults();
    fresh.config_options.push(select(
        "model",
        "openai/gpt-5",
        &[
            ("openai/gpt-5", "openai/gpt-5"),
            ("deepseek/flash", "deepseek/flash"),
        ],
    ));
    assert!(
        !debug_all(saved.option_actions(&fresh))
            .iter()
            .any(|action| action.contains("\"model\"")),
        "模型只走 model 那一步"
    );
}

#[test]
fn initial_config_from_the_client_is_written_like_a_restore() {
    let saved = acp_idle::SuspendedConfig::from_values(&[
        ("model".into(), "deepseek/flash".into()),
        ("mode".into(), "bypassPermissions".into()),
        ("thought_level".into(), "  ".into()),
        ("".into(), "high".into()),
    ]);
    assert!(!saved.is_empty());
    let fresh = fresh_defaults();
    assert_eq!(
        debug_all(saved.model_action(&fresh)),
        [set("model", "deepseek/flash", None)]
    );
    assert_eq!(
        debug_all(saved.option_actions(&fresh)),
        [set("mode", "bypassPermissions", None)],
        "空的 id 和值不发"
    );
    assert!(acp_idle::SuspendedConfig::from_values(&[]).is_empty());
}

/// 客户端存的是 agent 自己的模型配置 id（不是 `model` 占位名）时，也要按模型那一步先换，
/// 不能当普通配置项跟其它项一起写。
#[test]
fn initial_config_naming_the_agents_own_model_id_switches_the_model_first() {
    let saved = acp_idle::SuspendedConfig::from_values(&[
        ("claude_model".into(), "deepseek/flash".into()),
        ("mode".into(), "bypassPermissions".into()),
    ]);
    let fresh = quiet_with(|state| {
        let mut model = pi_model("openai/gpt-5");
        model.config_id = "claude_model".into();
        state.model = Some(model);
        state.config_options = vec![
            mode("Ask each time"),
            select(
                "claude_model",
                "openai/gpt-5",
                &[
                    ("openai/gpt-5", "openai/gpt-5"),
                    ("deepseek/flash", "deepseek/flash"),
                ],
            ),
        ];
    });
    assert_eq!(
        debug_all(saved.model_action(&fresh)),
        [set("claude_model", "deepseek/flash", None)]
    );
    assert_eq!(
        debug_all(saved.option_actions(&fresh)),
        [set("mode", "bypassPermissions", None)],
        "模型只走 model 那一步"
    );
}

/// 换模型会把 Pi 的推理强度打回模型默认值，所以必须等模型确认后再写其它项。
#[test]
fn restore_waits_for_the_model_before_writing_other_settings() {
    use smelt_core::acp_conn::{ConfigValue, ConversationCommand};
    let saved = acp_idle::SuspendedConfig::capture(&tuned_state());
    let (session, cmd_rx) = direct_session(fresh_defaults());
    let session = Arc::new(session);
    let subscribers = new_event_hub();
    let restoring = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            acp_idle::restore_suspended_config("acp-idle", &session, &saved, &subscribers)
        })
    };
    let next = || cmd_rx.recv_blocking().unwrap();
    let expect = |command: ConversationCommand, id: &str, value: ConfigValue| match command {
        ConversationCommand::SetConfigOption {
            config_id,
            value: got,
        } => {
            assert_eq!(config_id, id);
            assert_eq!(got, value);
        }
        _ => panic!("不是配置动作"),
    };

    expect(
        next(),
        "model",
        ConfigValue::Select("deepseek/flash".into()),
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(!restoring.is_finished(), "模型没确认前不能往下走");
    assert!(cmd_rx.try_recv().is_err(), "模型没确认前不能写其它项");
    // agent 确认换好了模型，推理强度被打回中。
    // 只改镜像、不发快照：等确认的一方靠守卫放锁时的通知醒来。
    session.reduced.lock().model = Some(pi_model("deepseek/flash"));

    expect(
        next(),
        "mode",
        ConfigValue::Select("bypassPermissions".into()),
    );
    expect(next(), "thought_level", ConfigValue::Select("high".into()));
    expect(next(), "codemode", ConfigValue::Boolean(true));
    session.reduced.lock().config_options = vec![
        mode("Full access"),
        thinking("High"),
        switch("codemode", true),
    ];
    restoring.join().unwrap();
}

/// Pi 驱动发布的脚本工具开关是 off/on 两项的下拉，不是布尔开关。
fn pi_codemode(on: bool) -> smelt_core::acp_conn::SessionConfigState {
    select(
        "codemode",
        if on { "On" } else { "Off" },
        &[("off", "Off"), ("on", "On")],
    )
}

/// 新开 Pi 对话带着「完全放行 + 脚本工具开」：就绪时脚本工具还是关的，两项要 agent 先后两次
/// 确认。写回在补做线程里等确认，事件线程每批归并完都要更新守护状态；写回等待期间不能占着
/// 事件线程要的锁，否则第二次确认进不来，只能等满超时，首条消息跟着干等。
#[test]
fn restoring_pi_script_tools_finishes_once_confirmed() {
    let (session, cmd_rx) = direct_session(quiet_with(|state| {
        state.config_options = vec![mode("Ask each time"), pi_codemode(false)];
    }));
    crate::acp_supervise::remember_relaunch_config(
        &session,
        acp_idle::SuspendedConfig::from_values(&[
            ("mode".into(), "bypassPermissions".into()),
            ("codemode".into(), "on".into()),
        ]),
    );
    session.startup_hold.store(true, Ordering::SeqCst);
    let session = Arc::new(session);
    let subscribers = new_event_hub();
    let after_ready = {
        let session = Arc::clone(&session);
        let subscribers = subscribers.clone();
        std::thread::spawn(move || {
            crate::acp_supervise::run_after_ready("acp-idle", &session, &subscribers)
        })
    };
    // 两项一起写：完全放行、脚本工具开。
    for _ in 0..2 {
        cmd_rx.recv_blocking().unwrap();
    }
    let started = std::time::Instant::now();
    // 照事件线程的做法：归并一批，再更新守护状态。
    for options in [
        vec![mode("Full access"), pi_codemode(false)],
        vec![mode("Full access"), pi_codemode(true)],
    ] {
        smelt_core::acp_session::apply_event(
            &mut session.reduced.lock(),
            smelt_core::acp_conn::ConversationEvent::ConfigOptions(options),
        );
        crate::acp_host::update_acp_daemon_state(&session, &subscribers);
    }
    after_ready.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "确认到了就该放行，等了 {:?}",
        started.elapsed()
    );
    assert!(!session.startup_hold.load(Ordering::SeqCst));
}

#[test]
fn a_rejected_restore_does_not_hold_the_user_action() {
    let saved = acp_idle::SuspendedConfig::capture(&tuned_state());
    let (session, _cmd_rx) = direct_session(quiet_with(|state| {
        state.model = Some(pi_model("deepseek/flash"));
        state.config_options = vec![mode("Ask each time")];
    }));
    let session = Arc::new(session);
    let subscribers = new_event_hub();
    let restoring = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            acp_idle::restore_suspended_config("acp-idle", &session, &saved, &subscribers)
        })
    };
    std::thread::sleep(Duration::from_millis(200));
    smelt_core::acp_session::apply_event(
        &mut session.reduced.lock(),
        smelt_core::acp_conn::ConversationEvent::ConfigRejected(
            smelt_core::acp_session::RuntimeNote::ConfigUpdateFailed {
                detail: "nope".into(),
            },
        ),
    );
    let started = std::time::Instant::now();
    restoring.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "被拒绝就别等满超时"
    );
}

/// 别的对话的消息只能由门卫交给驱动。客户端发来就是冒充。
#[test]
fn clients_cannot_forward_peer_messages_to_the_host() {
    let (session, cmd_rx) = direct_session(quiet_state());
    let message = smelt_core::acp_session::PeerDelivery {
        id: "pm-forged".into(),
        source_session_id: "acp-other".into(),
        source_title: "冒充".into(),
        kind: smelt_core::acp_session::PeerMessageKind::Task,
        hops: 7,
        reply_to: None,
        text: "删库".into(),
    };
    let rejected = apply_acp_user_action(
        &session,
        AcpUserAction::DeliverPeerMessage { message },
        &new_event_hub(),
    );
    assert_eq!(rejected.unwrap_err(), "peer delivery is internal");
    let rejected = apply_acp_user_action(
        &session,
        AcpUserAction::PeerReply {
            request_id: 1,
            reply: serde_json::json!({"ok": true}),
        },
        &new_event_hub(),
    );
    assert_eq!(rejected.unwrap_err(), "peer delivery is internal");
    assert!(cmd_rx.try_recv().is_err(), "冒充的消息不能进驱动");
}

#[test]
fn the_reaper_sleeps_until_the_earliest_deadline_or_until_activity() {
    assert_eq!(
        acp_idle::next_reaper_wake_ms(Vec::<u64>::new(), None),
        None,
        "没有会话可能变闲置时一直睡到下一次活动"
    );
    assert_eq!(
        acp_idle::next_reaper_wake_ms([300, 100, 200], None),
        Some(100)
    );
    assert_eq!(acp_idle::next_reaper_wake_ms([300], Some(50)), Some(50));
    assert_eq!(
        acp_idle::next_reaper_wake_ms(Vec::<u64>::new(), Some(50)),
        Some(50)
    );
    assert_eq!(
        acp_idle::idle_deadline_ms(1_000, Duration::from_secs(30 * 60)),
        1_000 + 30 * 60 * 1_000
    );
}

#[test]
fn contended_retries_back_off_up_to_a_minute() {
    assert_eq!(acp_idle::idle_retry_delay(0), Duration::from_secs(1));
    assert_eq!(acp_idle::idle_retry_delay(1), Duration::from_secs(2));
    assert_eq!(acp_idle::idle_retry_delay(5), Duration::from_secs(32));
    assert_eq!(acp_idle::idle_retry_delay(6), Duration::from_secs(60));
    assert_eq!(
        acp_idle::idle_retry_delay(u32::MAX),
        Duration::from_secs(60)
    );
}

#[test]
fn a_recently_active_conversation_sets_the_next_wake() {
    let acp_sessions = new_test_acp_sessions();
    let (session, _cmd_rx) = direct_session(quiet_state());
    let (slot, _) = acp_sessions.reserve_with("acp-idle-recent", || session);
    let before = crate::event_hub::now_ms();
    let idle_after = acp_idle::ACP_IDLE_SUSPEND_AFTER.as_millis() as u64;
    let sweep =
        acp_idle::suspend_idle_acp_sessions(&acp_sessions, acp_idle::ACP_IDLE_SUSPEND_AFTER, 0);
    let deadline = sweep.next_deadline_ms.expect("还没闲置够的会话要排上截止");
    // 会话建好时记的活动早于 `before`，截止落在 `before` 加闲置期前后，差不过一秒。
    assert!(
        deadline + 1_000 > before + idle_after && deadline <= before + idle_after,
        "截止是最后活动加闲置期"
    );
    assert!(!sweep.contended);
    assert!(!slot.value.idle.is_suspended());
}

#[test]
fn a_blocked_conversation_past_its_deadline_waits_for_activity() {
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-idle-blocked", || {
        make_acp_session_value("acp-idle-blocked", quiet_state())
    });
    let sweep = acp_idle::suspend_idle_acp_sessions(&acp_sessions, Duration::ZERO, 0);
    assert_eq!(
        sweep.next_deadline_ms, None,
        "没有 runtime 的会话不排截止，下次拉起会记活动"
    );
    assert!(!sweep.contended);
}

#[test]
fn a_busy_lifecycle_lock_is_retried_later() {
    let acp_sessions = new_test_acp_sessions();
    let (session, _cmd_rx) = direct_session(quiet_state());
    let (slot, _) = acp_sessions.reserve_with("acp-idle-contended", || session);
    let _held = slot.lifecycle.lock();
    let sweep = acp_idle::suspend_idle_acp_sessions(&acp_sessions, Duration::ZERO, 0);
    assert!(sweep.contended, "抢不到生命周期锁要稍后重试");
    assert!(!slot.value.idle.is_suspended());
}

#[test]
fn suspending_trims_the_transcript_to_the_current_turn() {
    use smelt_core::acp_chat::AcpEntry;
    let acp_sessions = new_test_acp_sessions();
    let (session, _cmd_rx) = direct_session(quiet_state());
    session.reduced.lock().entries_mut().extend([
        AcpEntry::User("第一轮".into()),
        AcpEntry::Assistant {
            text: "旧回答".into(),
            thought: false,
        },
        AcpEntry::User("第二轮".into()),
        AcpEntry::Assistant {
            text: "刚结束".into(),
            thought: false,
        },
    ]);
    let (slot, _) = acp_sessions.reserve_with("acp-idle", || session);
    let sweep = acp_idle::suspend_idle_acp_sessions(&acp_sessions, Duration::ZERO, 0);
    assert!(!sweep.contended);
    let reduced = slot.value.reduced.lock();
    assert_eq!(reduced.entries_base(), 2);
    assert_eq!(reduced.entries_total(), 4);
    assert_eq!(reduced.entries().len(), 2);
    drop(reduced);
    assert!(slot.value.idle.is_suspended());
}

#[test]
fn waking_without_a_launch_spec_stays_suspended() {
    let acp_sessions = new_test_acp_sessions();
    let (session, _cmd_rx) = direct_session(quiet_state());
    let (slot, _) = acp_sessions.reserve_with("acp-idle", || session);
    acp_idle::suspend_idle_acp_sessions(&acp_sessions, Duration::ZERO, 0);
    assert!(slot.value.idle.is_suspended());
    let error =
        acp_idle::wake_suspended_acp_session("acp-idle", &slot, &acp_sessions, &new_event_hub())
            .unwrap_err();
    assert!(
        error.contains("no launch spec") || error.contains("启动规格"),
        "{error}"
    );
    assert!(slot.value.idle.is_suspended());
}

#[test]
fn a_poisoned_session_lock_does_not_panic_the_suspend_check() {
    let (session, _cmd_rx) = direct_session(quiet_state());
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = session.handle.lock();
        panic!("one session");
    }));
    let blocker =
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER);
    assert_eq!(blocker, None);
}

/// 用户发消息：runtime 在跑就只记活动，不重新拉起。
#[test]
fn user_input_to_a_running_conversation_does_not_relaunch() {
    let acp_sessions = new_test_acp_sessions();
    let (session, _cmd_rx) = direct_session(quiet_state());
    let (slot, _) = acp_sessions.reserve_with("acp-idle", || session);
    let generation = slot.value.connection_generation.load(Ordering::SeqCst);
    assert_eq!(
        acp_idle::launch_for_user_input("acp-idle", &slot, &acp_sessions, &new_event_hub()),
        Ok(())
    );
    assert_eq!(
        slot.value.connection_generation.load(Ordering::SeqCst),
        generation
    );
}

/// 上次因失败停下的对话（这里是没登录）不是终点：用户发消息就按记录的启动规格拉起。
/// 没记下启动规格时拉不起来，原话回给发消息的人。
#[test]
fn user_input_to_a_stopped_conversation_relaunches_it() {
    let acp_sessions = new_test_acp_sessions();
    let mut stopped = quiet_state();
    smelt_core::acp_session::force_end_because(
        &mut stopped,
        smelt_core::acp_session::AcpEndKind::RestoreFailed,
        smelt_core::acp_session::ConversationEndReason::SignInRequired {
            agent: smelt_core::acp_session::SignInAgent::Antigravity,
        },
        "Authentication required",
    );
    let (slot, _) = acp_sessions.reserve_with("acp-stopped", || {
        make_acp_session_value("acp-stopped", stopped)
    });
    let error =
        acp_idle::launch_for_user_input("acp-stopped", &slot, &acp_sessions, &new_event_hub())
            .unwrap_err();
    assert!(error.contains("no launch spec"), "{error}");
}

fn reserve_live_session(
    acp_sessions: &AcpSessions,
    id: &str,
    reduced: AcpSessionState,
    last_activity_ms: u64,
) -> (
    Arc<AcpSlot<AcpSession>>,
    smol::channel::Receiver<smelt_core::acp_conn::ConversationCommand>,
) {
    let (session, cmd_rx) = direct_session(reduced);
    session.idle.set_last_activity_ms(last_activity_ms);
    let (slot, _) = acp_sessions.reserve_with(id, || session);
    (slot, cmd_rx)
}

#[test]
fn sessions_within_the_warm_limit_are_never_suspended() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let old = now.saturating_sub(60 * 60 * 1_000);
    let mut slots = Vec::new();
    let mut rxs = Vec::new();
    for i in 0..acp_idle::ACP_IDLE_KEEP_WARM {
        let (slot, rx) = reserve_live_session(
            &acp_sessions,
            &format!("acp-warm-{i}"),
            quiet_state(),
            old + (i as u64) * 1_000,
        );
        slots.push(slot);
        rxs.push(rx);
    }

    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert_eq!(sweep.next_deadline_ms, None, "保底名额内不排定时唤醒");
    assert!(!sweep.contended);
    for (i, slot) in slots.iter().enumerate() {
        assert!(
            !slot.value.idle.is_suspended(),
            "第 {i} 个会话在保底名额内，不应挂起"
        );
    }
}

#[test]
fn excess_sessions_are_suspended_oldest_activity_first() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let min_ms = 60 * 1_000;
    // 故意乱序插入：最老的两个是 s-oldest(50m) 和 s-second(40m)，其余 5 个较新（即使部分也超过 20m）。
    let specs = [
        ("acp-s3", 30 * min_ms),
        ("acp-oldest", 50 * min_ms),
        ("acp-s6", 5 * min_ms),
        ("acp-s4", 25 * min_ms),
        ("acp-second", 40 * min_ms),
        ("acp-s5", 10 * min_ms),
        ("acp-s7", min_ms),
    ];
    let mut slots = std::collections::HashMap::new();
    let mut rxs = Vec::new();
    for (id, age_ms) in specs {
        let (slot, rx) =
            reserve_live_session(&acp_sessions, id, quiet_state(), now.saturating_sub(age_ms));
        slots.insert(id, slot);
        rxs.push(rx);
    }

    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert_eq!(sweep.next_deadline_ms, None, "回收完剩 5 个，不再排截止");
    assert!(!sweep.contended);
    assert!(slots["acp-oldest"].value.idle.is_suspended());
    assert!(slots["acp-second"].value.idle.is_suspended());
    for keep in ["acp-s3", "acp-s4", "acp-s5", "acp-s6", "acp-s7"] {
        assert!(
            !slots[keep].value.idle.is_suspended(),
            "{keep} 属于最近活动的 5 个，应保留热态"
        );
    }
}

#[test]
fn excess_sessions_still_wait_for_the_idle_timeout() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let min_ms = 60 * 1_000;
    let second_activity = now.saturating_sub(10 * min_ms);
    let specs = [
        ("acp-oldest", now.saturating_sub(30 * min_ms)),
        ("acp-second", second_activity),
        ("acp-s3", now.saturating_sub(5 * min_ms)),
        ("acp-s4", now.saturating_sub(4 * min_ms)),
        ("acp-s5", now.saturating_sub(3 * min_ms)),
        ("acp-s6", now.saturating_sub(2 * min_ms)),
        ("acp-s7", now.saturating_sub(min_ms)),
    ];
    let mut slots = std::collections::HashMap::new();
    let mut rxs = Vec::new();
    for (id, activity) in specs {
        let (slot, rx) = reserve_live_session(&acp_sessions, id, quiet_state(), activity);
        slots.insert(id, slot);
        rxs.push(rx);
    }

    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert!(slots["acp-oldest"].value.idle.is_suspended());
    assert!(
        !slots["acp-second"].value.idle.is_suspended(),
        "超出名额但未满 20 分钟闲置，不能提前挂起"
    );
    assert_eq!(
        sweep.next_deadline_ms,
        Some(acp_idle::idle_deadline_ms(
            second_activity,
            acp_idle::ACP_IDLE_SUSPEND_AFTER,
        )),
        "下一次唤醒应排在超出名额且最早到点的那条会话"
    );
    assert!(!sweep.contended);
}

#[test]
fn busy_sessions_count_toward_the_warm_pool_without_being_suspended() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let min_ms = 60 * 1_000;
    let mut rxs = Vec::new();

    // 1 个挂着后台任务且很老的会话 + 4 个思考中的会话 = 5 个不可挂起的活会话。
    let running_task = quiet_with(|state| {
        state.background_tasks = vec![BackgroundTaskView {
            id: "bg".into(),
            title: "bg".into(),
            command: "sleep 60".into(),
            status: BackgroundTaskStatus::Running,
            exit_code: None,
            output: String::new(),
            started_at_ms: 0,
            finished_at_ms: None,
        }];
    });
    let (bg_slot, rx) = reserve_live_session(
        &acp_sessions,
        "acp-bg-old",
        running_task,
        now.saturating_sub(60 * min_ms),
    );
    rxs.push(rx);

    let mut thinking_slots = Vec::new();
    for i in 0..4u64 {
        let (slot, rx) = reserve_live_session(
            &acp_sessions,
            &format!("acp-busy-{i}"),
            quiet_with(|state| state.phase = DaemonPhase::Thinking),
            now.saturating_sub((i + 1) * 1_000),
        );
        thinking_slots.push(slot);
        rxs.push(rx);
    }

    // 第 6 个是闲置 30 分钟的空闲会话：因为忙碌会话已占满 5 个，它会被回收。
    let (idle_slot, rx) = reserve_live_session(
        &acp_sessions,
        "acp-idle-extra",
        quiet_state(),
        now.saturating_sub(30 * min_ms),
    );
    rxs.push(rx);

    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert!(!bg_slot.value.idle.is_suspended());
    for slot in &thinking_slots {
        assert!(!slot.value.idle.is_suspended());
    }
    assert!(
        idle_slot.value.idle.is_suspended(),
        "活会话总数超 5，跳过不可挂起的忙碌会话后应回收空闲会话"
    );
    assert_eq!(sweep.next_deadline_ms, None);
}

#[test]
fn automation_dead_and_suspended_sessions_do_not_consume_warm_slots() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let old = now.saturating_sub(60 * 60 * 1_000);
    let mut rxs = Vec::new();

    // 5 个闲置很久的互动会话。
    let mut kept = Vec::new();
    for i in 0..acp_idle::ACP_IDLE_KEEP_WARM {
        let (slot, rx) = reserve_live_session(
            &acp_sessions,
            &format!("acp-keep-{i}"),
            quiet_state(),
            old + (i as u64) * 1_000,
        );
        kept.push(slot);
        rxs.push(rx);
    }

    // 先挂起 1 个更老的会话，让注册表里确实存在已挂起槽位。
    let (susp_slot, rx) = reserve_live_session(
        &acp_sessions,
        "acp-already-suspended",
        quiet_state(),
        old.saturating_sub(10_000),
    );
    rxs.push(rx);
    acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert!(susp_slot.value.idle.is_suspended());

    // 再塞 2 个活自动化会话和 2 个无 runtime 的已停会话：都不该挤掉 5 个保底名额。
    for i in 0..2 {
        let (slot, rx) = reserve_live_session(
            &acp_sessions,
            &format!("acp-automation-{i}"),
            quiet_state(),
            now,
        );
        *slot.value.conversation_binding.lock() =
            Some(smelt_core::conversation::ConversationBinding::Automation {
                run_id: format!("run-{i}"),
            });
        rxs.push(rx);
        acp_sessions.reserve_with(&format!("acp-dead-{i}"), || {
            make_acp_session_value(&format!("acp-dead-{i}"), quiet_state())
        });
    }

    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert_eq!(sweep.next_deadline_ms, None);
    for (i, slot) in kept.iter().enumerate() {
        assert!(
            !slot.value.idle.is_suspended(),
            "第 {i} 个互动会话不应被自动化/已停/已挂起会话挤出保底名额"
        );
    }
}

#[test]
fn contended_oldest_session_does_not_evict_a_newer_session() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let min_ms = 60 * 1_000;
    let mut slots = Vec::new();
    let mut rxs = Vec::new();
    for i in 0..7u64 {
        let (slot, rx) = reserve_live_session(
            &acp_sessions,
            &format!("acp-s{i}"),
            quiet_state(),
            now.saturating_sub((60 - i * 5) * min_ms),
        );
        slots.push(slot);
        rxs.push(rx);
    }
    // 7 个活会话（超出 2 个）：最老的 s0 锁冲突占住 1 个名额，次老的 s1 应照常挂起，
    // 第 3 老的 s2 不应被越级挂起。
    let _held = slots[0].lifecycle.lock();
    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert!(sweep.contended);
    assert!(!slots[0].value.idle.is_suspended());
    assert!(slots[1].value.idle.is_suspended(), "次老的 s1 应照常挂起");
    for (i, slot) in slots.iter().enumerate().skip(2) {
        assert!(
            !slot.value.idle.is_suspended(),
            "最老会话锁冲突时不应越过它去挂起更新的会话 s{i}"
        );
    }
}

#[test]
fn busy_oldest_session_still_schedules_next_idle_deadline() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let min_ms = 60 * 1_000;
    let mut rxs = Vec::new();

    let (busy_old, rx) = reserve_live_session(
        &acp_sessions,
        "acp-busy-old",
        quiet_with(|state| state.phase = DaemonPhase::Thinking),
        now.saturating_sub(40 * min_ms),
    );
    rxs.push(rx);

    let next_idle_activity = now.saturating_sub(10 * min_ms);
    let (next_idle, rx) = reserve_live_session(
        &acp_sessions,
        "acp-idle-next",
        quiet_state(),
        next_idle_activity,
    );
    rxs.push(rx);

    for i in 0..4u64 {
        let (_, rx) = reserve_live_session(
            &acp_sessions,
            &format!("acp-fresh-{i}"),
            quiet_state(),
            now.saturating_sub((i + 1) * min_ms),
        );
        rxs.push(rx);
    }

    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert!(!busy_old.value.idle.is_suspended());
    assert!(!next_idle.value.idle.is_suspended());
    assert_eq!(
        sweep.next_deadline_ms,
        Some(acp_idle::idle_deadline_ms(
            next_idle_activity,
            acp_idle::ACP_IDLE_SUSPEND_AFTER,
        )),
        "跳过最老的忙碌会话后，应排上最近要到期的空闲会话截止"
    );
}

#[test]
fn updating_activity_reorders_eviction_priority() {
    let acp_sessions = new_test_acp_sessions();
    let now = crate::event_hub::now_ms();
    let min_ms = 60 * 1_000;
    let mut slots = Vec::new();
    let mut rxs = Vec::new();
    for i in 0..6u64 {
        let (slot, rx) = reserve_live_session(
            &acp_sessions,
            &format!("acp-s{i}"),
            quiet_state(),
            now.saturating_sub((60 - i * 5) * min_ms),
        );
        slots.push(slot);
        rxs.push(rx);
    }
    // 原本最老的 s0 刚有了新活动，现在次老的 s1 应当成为被回收的那个。
    slots[0].value.idle.touch();
    let sweep = acp_idle::suspend_idle_acp_sessions(
        &acp_sessions,
        acp_idle::ACP_IDLE_SUSPEND_AFTER,
        acp_idle::ACP_IDLE_KEEP_WARM,
    );
    assert_eq!(sweep.next_deadline_ms, None);
    assert!(!slots[0].value.idle.is_suspended(), "刚活动的 s0 应保留");
    assert!(slots[1].value.idle.is_suspended(), "此时最老的 s1 应被回收");
    for slot in &slots[2..] {
        assert!(!slot.value.idle.is_suspended());
    }
}

/// 唤醒流程已经把 `idle` 标回运行中、但历史重放还没把裁掉的前缀装回来时，
/// 翻页读更早正文不能直接返回空切片，必须等到重放完成再读。
#[test]
fn read_history_page_waits_for_replay_while_waking_trimmed_session() {
    use smelt_core::acp_chat::AcpEntry;
    use smelt_core::acp_conn::{ConversationEvent, ReadyKind};

    let acp_sessions = new_test_acp_sessions();
    let subscribers = new_event_hub();
    let mut state = quiet_state();
    state.entries_mut().extend([
        AcpEntry::User("第一问".into()),
        AcpEntry::Assistant {
            text: "第一答".into(),
            thought: false,
        },
        AcpEntry::User("第二问".into()),
        AcpEntry::Assistant {
            text: "第二答".into(),
            thought: false,
        },
    ]);
    state.trim_to_current_turn();
    assert_eq!(state.entries_base(), 2);
    state.phase = DaemonPhase::Connecting;
    state.replaying_history = true;
    let (slot, _rx) = reserve_live_session(
        &acp_sessions,
        "acp-waking-page",
        state,
        crate::event_hub::now_ms(),
    );
    assert!(
        !slot.value.idle.is_suspended(),
        "拉起后 idle 已标为运行中，但重放尚未完成"
    );

    let sessions_for_thread = Arc::clone(&acp_sessions);
    let subs_for_thread = subscribers.clone();
    let reader = std::thread::spawn(move || {
        crate::acp_host::read_history_page(
            "acp-waking-page",
            &sessions_for_thread,
            &subs_for_thread,
            2,
            2,
            false,
        )
    });

    std::thread::sleep(Duration::from_millis(40));
    assert!(
        !reader.is_finished(),
        "重放未完成时翻页应等待，不能直接回空页"
    );
    {
        let mut reduced = slot.value.reduced.lock();
        smelt_core::acp_session::apply_event(&mut reduced, ConversationEvent::HistoryReplayStarted);
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::UserChunk("第一问".into()),
        );
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::AgentChunk {
                thought: false,
                text: "第一答".into(),
                parent_id: None,
            },
        );
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::UserChunk("第二问".into()),
        );
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::AgentChunk {
                thought: false,
                text: "第二答".into(),
                parent_id: None,
            },
        );
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::HistoryReplayFinished,
        );
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::Ready {
                session_id: agent_client_protocol::v1::schema::SessionId::new("restored"),
                kind: ReadyKind::ResumedWithReplay,
                supports_image: false,
            },
        );
    }

    let page = reader.join().unwrap().expect("重放完成后翻页应成功");
    assert_eq!(page.entries_offset, 0);
    assert_eq!(page.entries.len(), 2);
    assert!(matches!(&page.entries[0], AcpEntry::User(text) if text == "第一问"));
}
