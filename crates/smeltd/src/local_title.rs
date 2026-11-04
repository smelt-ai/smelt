//! 本机小模型给 ACP 会话起标题，用户每发一条消息就按对话最近内容重起。
//!
//! - 只用这台机器上用户在设置里明确装好的模型（`~/.smelt/models`），从不下载；
//!   没装就什么都不改，会话名照旧是 provider 标题或首条 prompt 兜底。装上之后，
//!   下一条用户消息发出就会起名。
//! - 优先级：用户手改名（展示层）> provider 标题 > 旧版标题 > 本机模型标题 > 首条 prompt。
//!   有 provider 标题或旧版标题时不起名，也不改它们；本机标题只写自己的来源字段。
//!   用户手改名存在目录里，展示层最先采用，本机标题盖不住它。
//! - 触发：用户消息真正写进消息流时（发出 prompt，含队列消息出队发送；steer 被
//!   provider 吃进当前回合），归约状态的内容版本加一并冻结输入，不等助手回复或回合
//!   结束。发送路径随即调 [`request_for_session`]，steer 由事件 drain 归约后调
//!   [`request_if_needed`]。重发、回声、重放不加版本；撤回、回退清空冻结输入，
//!   在跑的结果作废，它们本身不排新任务。不轮询。
//! - 并发：每段会话同一时刻至多一个任务在队列里或在跑
//!   （[`LocalTitleSchedule::queued`]），所以队列长度以会话数为上限。生成期间用户又发了
//!   消息：旧结果因版本不符被丢掉，这一次跑完后为最新版本再排一次；中间再发几条
//!   也只合并成这一次。工作线程开始时读的是最近那条用户消息发出时冻结的输入，之后的
//!   助手增量不混进来；新结果回来前，上一次起的名照常显示。
//! - 所有会话共用一条工作线程，阻塞在 channel 上等任务：空闲时不醒，推理一次一个，
//!   模型用完即卸。
//! - 结果写回归约状态后照常推快照、写库；render 不碰这里的任何东西。

use crate::acp_host::AcpSession;
use crate::acp_registry::AcpSlot;
use crate::event_hub::EventHubHandle;
use smelt_local_model::{TitleModel, TitleModelSpec};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak, mpsc};

/// 一段会话的本机起名排期。只在本模块里改。
#[derive(Debug, Default)]
pub(crate) struct LocalTitleSchedule {
    /// 已经要求过的最新内容版本。新用户消息的版本比它大才排。
    wanted: u64,
    /// 这段会话已有任务在 channel 里或正在生成。
    queued: bool,
    /// 这段会话自己的槽位。发送路径只拿得到 `&AcpSession`，靠它排任务；弱引用，不续命。
    slot: Option<Weak<AcpSlot<AcpSession>>>,
}

impl LocalTitleSchedule {
    #[cfg(test)]
    pub(crate) fn wanted(&self) -> u64 {
        self.wanted
    }

    #[cfg(test)]
    pub(crate) fn queued(&self) -> bool {
        self.queued
    }

    /// 测试用：当作已有任务在跑，只观察排期是否前进，不真的交给工作线程。
    #[cfg(test)]
    pub(crate) fn mark_queued(&mut self) {
        self.queued = true;
    }
}

struct Job {
    slot: Weak<AcpSlot<AcpSession>>,
    subscribers: EventHubHandle,
}

/// 登记会话自己的槽位，之后发送路径就能用 [`request_for_session`] 排任务。
pub(crate) fn attach(slot: &Arc<AcpSlot<AcpSession>>) {
    slot.value.local_title.lock().slot = Some(Arc::downgrade(slot));
}

/// 有新的用户消息、还没为它排过时，把这段会话交给工作线程。每批事件归约之后调用；
/// 只读计数，不扫正文。
pub(crate) fn request_if_needed(slot: &Arc<AcpSlot<AcpSession>>, subscribers: &EventHubHandle) {
    attach(slot);
    request(&slot.value, Arc::downgrade(slot), subscribers);
}

/// 发送路径用：用户消息刚写进对话。还没登记槽位（drain 尚未启动）时什么都不做，
/// drain 下一批事件归约后会补排，不会漏。
pub(crate) fn request_for_session(sess: &AcpSession, subscribers: &EventHubHandle) {
    let Some(slot) = sess.local_title.lock().slot.clone() else {
        return;
    };
    request(sess, slot, subscribers);
}

fn request(sess: &AcpSession, slot: Weak<AcpSlot<AcpSession>>, subscribers: &EventHubHandle) {
    let Some(revision) = sess.reduced.lock().local_title_revision() else {
        return;
    };
    let mut schedule = sess.local_title.lock();
    if revision <= schedule.wanted {
        return;
    }
    schedule.wanted = revision;
    if schedule.queued {
        // 正在跑的那一次结束时会看到更大的 `wanted`，再排一次。
        return;
    }
    let Some(worker) = worker() else {
        // 工作线程起不来：`wanted` 已记下，下一条消息版本更大，会再试。
        return;
    };
    schedule.queued = worker
        .send(Job {
            slot,
            subscribers: subscribers.clone(),
        })
        .is_ok();
}

fn worker() -> Option<&'static mpsc::Sender<Job>> {
    static WORKER: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            match std::thread::Builder::new()
                .name("smelt-local-title".into())
                .spawn(move || {
                    // 阻塞在 channel 上：没有任务时不醒。
                    for job in rx {
                        run(job);
                    }
                }) {
                Ok(_) => Some(tx),
                Err(error) => {
                    eprintln!("[local-title] 起不来工作线程：{error}");
                    None
                }
            }
        })
        .as_ref()
}

/// GUI 设置页装模型的同一个目录。
fn models_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("models"))
}

fn run(job: Job) {
    let Some(slot) = job.slot.upgrade() else {
        return;
    };
    let started = slot.value.local_title.lock().wanted;
    drop(slot);
    let attempted = generate_and_apply(&job);
    let Some(slot) = job.slot.upgrade() else {
        return;
    };
    let mut schedule = slot.value.local_title.lock();
    // 即使这次没跑成，新消息也不能被 queued 标记吞掉；没有新消息则不重试。
    let rerun = needs_rerun(started, attempted, schedule.wanted);
    if rerun && let Some(worker) = worker() {
        schedule.queued = worker.send(job).is_ok();
    } else {
        schedule.queued = false;
    }
}

fn needs_rerun(started: u64, attempted: Option<u64>, wanted: u64) -> bool {
    wanted > attempted.unwrap_or(started)
}

fn model_from_setting(stored: Option<&str>) -> Result<TitleModel, String> {
    match stored {
        None => Ok(TitleModel::default()),
        Some(id) => TitleModel::from_id(id).ok_or_else(|| format!("未知本机标题模型：{id}")),
    }
}

fn selected_model() -> Result<TitleModel, String> {
    let store = smelt_core::sqlite_state::default_sqlite_store()?;
    let stored = store.local_title_model().map_err(|e| e.to_string())?;
    model_from_setting(stored.as_deref())
}

/// 按最近一条用户消息发出时冻结的输入生成并写回。返回这次用的内容版本；没跑推理时返回 `None`。
fn generate_and_apply(job: &Job) -> Option<u64> {
    let dir = models_dir()?;
    let model = match selected_model() {
        Ok(model) => model,
        Err(error) => {
            crate::dlog(&format!("读取本机标题模型失败：{error}"));
            return None;
        }
    };
    // 没装（或装了一半）就什么都不改，也不下载或切换到别的模型。
    if !matches!(smelt_local_model::is_installed(&dir, model.spec()), Ok(true)) {
        return None;
    }
    // 排队期间会话可能已经关了、输入被撤回/回退，或 provider 已经报了标题：不必再花推理。
    let (revision, input) = {
        let slot = job.slot.upgrade()?;
        slot.value.reduced.lock().local_title_request()?
    };
    let title = match crate::local_model_process::generate_title(model, &input) {
        Ok(title) => title,
        Err(error) => {
            crate::dlog(&format!("本机模型起标题失败：{error}"));
            return Some(revision);
        }
    };
    let slot = job.slot.upgrade()?;
    let sess = &slot.value;
    // 和事件 drain 同一把回合锁，且覆盖推送与状态广播：drain 也是持锁推送，
    // 否则这里读到的旧投影可能在 drain 的新状态之后写回/广播。
    let _turn_completion = sess.turn_completion.lock();
    // 写回前再检查一次选择；生成期间已经切换模型时丢弃旧结果。
    if !matches!(selected_model(), Ok(current) if current == model) {
        return Some(revision);
    }
    let applied = sess
        .reduced
        .lock()
        .apply_local_title(revision, &input, title);
    if applied {
        crate::acp_host::push_acp_snapshot_since(sess, false, None);
        crate::acp_host::update_acp_daemon_state(sess, &job.subscribers);
    }
    Some(revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_core::acp_conn::ConversationEvent;

    #[test]
    fn newer_message_is_requeued_even_if_the_previous_attempt_did_not_run() {
        assert!(needs_rerun(1, None, 2));
        assert!(!needs_rerun(1, None, 1));
        assert!(needs_rerun(1, Some(1), 2));
        assert!(!needs_rerun(1, Some(2), 2));
    }

    #[test]
    fn model_selection_uses_saved_value_and_never_falls_back_from_unknown() {
        assert_eq!(model_from_setting(None), Ok(TitleModel::Qwen3_5_0_8B));
        assert_eq!(model_from_setting(Some("qwen3.5-0.8b")), Ok(TitleModel::Qwen3_5_0_8B));
        assert!(model_from_setting(Some("unknown")).is_err());
        // 已下线的 0.6B 不回退到默认：存储迁移已把它改成 0.8B，再见到就是坏数据。
        assert!(model_from_setting(Some("qwen3-0.6b")).is_err());
    }

    fn send(slot: &Arc<AcpSlot<AcpSession>>, prompt: &str) {
        smelt_core::acp_session::note_prompt_sent(
            &mut slot.value.reduced.lock(),
            prompt.into(),
            Vec::new(),
        );
    }

    fn finish_turn(slot: &Arc<AcpSlot<AcpSession>>, reply: &str) {
        let mut reduced = slot.value.reduced.lock();
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::AgentChunk {
                thought: false,
                text: reply.into(),
                parent_id: None,
            },
        );
        smelt_core::acp_session::apply_event(
            &mut reduced,
            ConversationEvent::TurnEnded(
                agent_client_protocol::schema::v1::StopReason::EndTurn,
            ),
        );
    }

    fn test_slot(id: &str) -> Arc<AcpSlot<AcpSession>> {
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        acp_sessions
            .reserve_with(id, || crate::acp_host::make_acp_session(id, None, None, None))
            .0
    }

    #[test]
    fn each_sent_message_is_requested_once_without_waiting_for_the_turn_to_end() {
        let slot = test_slot("acp-local-title");
        let hub = crate::new_event_hub();
        slot.value.local_title.lock().mark_queued();

        request_if_needed(&slot, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 0, "还没有用户消息，不排");

        send(&slot, "帮我排查登录接口为什么超时");
        request_if_needed(&slot, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 1, "发出就排，不等回复");

        // 助手回复、回合结束、同一版重复通知都不再排。
        finish_turn(&slot, "看下日志");
        request_if_needed(&slot, &hub);
        request_if_needed(&slot, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 1);

        send(&slot, "再看下重试逻辑");
        request_if_needed(&slot, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 2, "每条消息都排");

        // 排队标记在测试中预先占住，不启动模型；会话名仍是首条 prompt 兜底。
        assert_eq!(slot.value.reduced.lock().local_title, None);
        assert_eq!(
            slot.value.reduced.lock().resolved_session_title().as_deref(),
            Some("帮我排查登录接口为什么超时")
        );
    }

    /// 发送路径只有 `&AcpSession`：登记过槽位就当场排上；还没登记时不排，
    /// 由 drain 下一批的 `request_if_needed` 补上。
    #[test]
    fn send_path_requests_through_the_attached_slot() {
        let slot = test_slot("acp-local-title-send");
        let hub = crate::new_event_hub();
        send(&slot, "帮我排查登录接口为什么超时");
        request_for_session(&slot.value, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 0, "未登记槽位时不排");

        attach(&slot);
        slot.value.local_title.lock().mark_queued();
        request_for_session(&slot.value, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 1);
        request_for_session(&slot.value, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 1, "同一条不重复排");
    }

    /// 撤回、回退本身不排新任务；之后真的再发一条才排。
    #[test]
    fn undo_and_rewind_do_not_queue_new_work() {
        let slot = test_slot("acp-local-title-undo");
        let hub = crate::new_event_hub();
        {
            // 占住排队标记，模拟任务在跑，只看 `wanted` 是否前进。
            slot.value.local_title.lock().queued = true;
        }
        send(&slot, "第一个问题");
        request_if_needed(&slot, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 1);

        smelt_core::acp_session::undo_prompt_sent(&mut slot.value.reduced.lock())
            .expect("刚发的可撤回");
        request_if_needed(&slot, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 1, "撤回不排");
        assert_eq!(slot.value.reduced.lock().local_title_request(), None);

        send(&slot, "第二个问题");
        finish_turn(&slot, "第二个回答");
        smelt_core::acp_session::apply_event(
            &mut slot.value.reduced.lock(),
            ConversationEvent::Rewound {
                truncate_from: 0,
                then: smelt_core::acp_conn::AfterRewind::RestoreComposer,
            },
        );
        request_if_needed(&slot, &hub);
        assert_eq!(
            slot.value.local_title.lock().wanted(),
            1,
            "回退后没有可用输入，不排（第二条的版本也因输入已清空没排上）"
        );
        assert_eq!(slot.value.reduced.lock().local_title_request(), None);
    }

    /// 已经有任务在跑时又发了消息：只记下最新版本，不再往 channel 里塞第二个。
    /// 跑完看到更新的版本再排一次；没跑成（未装模型等）就清掉排队标记，下一条可再试。
    #[test]
    fn a_running_job_coalesces_new_messages_and_a_failed_attempt_can_retry_later() {
        let slot = test_slot("acp-local-title-busy");
        let hub = crate::new_event_hub();
        {
            let mut schedule = slot.value.local_title.lock();
            schedule.queued = true;
            schedule.wanted = 0;
        }
        send(&slot, "第一个问题");
        request_if_needed(&slot, &hub);
        send(&slot, "第二个问题");
        request_if_needed(&slot, &hub);
        {
            let schedule = slot.value.local_title.lock();
            assert_eq!(schedule.wanted(), 2);
            assert!(schedule.queued(), "仍是原来那一个任务");
        }
        // 工作线程开始时读的是最新那条消息冻结的输入。
        assert!(
            slot.value
                .reduced
                .lock()
                .local_title_request()
                .is_some_and(|(revision, input)| revision == 2 && input.contains("第二个问题"))
        );

        // 模拟任务收尾并占住下一次排队，不启动真实模型。
        assert!(!needs_rerun(2, None, 2));
        slot.value.local_title.lock().queued = true;

        // 下一条消息（比如期间装好了模型）照常更新请求版本。
        send(&slot, "第三个问题");
        request_if_needed(&slot, &hub);
        assert_eq!(slot.value.local_title.lock().wanted(), 3);
    }

    #[test]
    fn a_conversation_with_a_provider_title_is_not_queued() {
        let slot = test_slot("acp-provider-title");
        send(&slot, "帮我排查登录接口为什么超时");
        slot.value.reduced.lock().protocol_title = Some("定位登录接口超时".into());
        request_if_needed(&slot, &crate::new_event_hub());
        assert_eq!(slot.value.local_title.lock().wanted(), 0);
        assert_eq!(
            slot.value.reduced.lock().protocol_title.as_deref(),
            Some("定位登录接口超时")
        );
    }
}
