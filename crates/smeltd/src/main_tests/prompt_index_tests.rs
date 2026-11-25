use super::*;
use smelt_core::acp_chat::{AcpEntry, PromptIndexItem};
use smelt_core::acp_conn::AcpForkCut;
use smelt_core::acp_session::AcpSessionState;
use smelt_core::control_api::{ConversationForkCutParams, ConversationPromptIndexParams, ForkSide};
use smelt_runtime_client::protocol::control::ControlErrorCode;

fn answer(text: &str) -> AcpEntry {
    AcpEntry::Assistant {
        text: text.into(),
        thought: false,
    }
}

#[test]
fn prompt_index_lists_every_prompt_before_the_cutoff() {
    let mut reduced = AcpSessionState::default();
    reduced.entries_mut().extend([
        AcpEntry::User("第一问".into()),
        answer("第一答"),
        AcpEntry::User("第二问".into()),
        answer("第二答"),
        AcpEntry::User("第三问".into()),
    ]);
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-index", || make_acp_session_value("acp-index", reduced));
    let fetch = |session_id: &str, before: Option<u64>| {
        crate::conversation_prompt_index::prompt_index(
            ConversationPromptIndexParams {
                session_id: session_id.into(),
                before,
            },
            &acp_sessions,
            &new_event_hub(),
        )
    };

    let all = fetch("acp-index", None).unwrap().prompts;
    assert_eq!(
        all.iter().map(|item| item.entry_ix).collect::<Vec<_>>(),
        [0, 2, 4]
    );
    assert_eq!(
        all[1],
        PromptIndexItem {
            entry_ix: 2,
            prompt: "第二问".into(),
            images: 0,
            answer: "第二答".into(),
        }
    );

    // 客户端已经加载了第 2 条往后，只要前面的。
    let head = fetch("acp-index", Some(2)).unwrap().prompts;
    assert_eq!(head.len(), 1);
    assert_eq!(head[0].answer, "第一答");
    // 超出末尾就是整段。
    assert_eq!(fetch("acp-index", Some(99)).unwrap().prompts, all);

    let error = fetch("acp-nope", None).unwrap_err();
    assert_eq!(error.code, ControlErrorCode::NotFound);
}

/// 同文本的提问按整段对话数序号：客户端只加载了后半段时，前面那条「继续」也要算上。
#[test]
fn fork_cut_counts_same_text_prompts_across_the_whole_conversation() {
    let mut reduced = AcpSessionState::default();
    reduced.entries_mut().extend([
        AcpEntry::User("继续".into()),
        answer("第一答"),
        AcpEntry::User("继续".into()),
        answer("第二答"),
        AcpEntry::User("继续".into()),
        answer("第三答"),
    ]);
    let acp_sessions = new_test_acp_sessions();
    acp_sessions.reserve_with("acp-fork", || make_acp_session_value("acp-fork", reduced));
    let cut = |session_id: &str, entry_index: u64, side: ForkSide| {
        crate::conversation_prompt_index::fork_cut(
            ConversationForkCutParams {
                session_id: session_id.into(),
                entry_index,
                side,
            },
            &acp_sessions,
            &new_event_hub(),
        )
    };
    let continue_at = |occurrence| {
        Some(AcpForkCut {
            text: "继续".into(),
            occurrence,
        })
    };

    // 从第二答分叉：切在第三问之前，它是第 2 条「继续」。
    assert_eq!(
        cut("acp-fork", 3, ForkSide::After).unwrap().cut,
        continue_at(2)
    );
    // 改第二问重发：切在它之前，它是第 1 条。
    assert_eq!(
        cut("acp-fork", 2, ForkSide::Before).unwrap().cut,
        continue_at(1)
    );
    // 从最后一条回答分叉是整段照抄；回答不是能改的提问。
    assert_eq!(cut("acp-fork", 5, ForkSide::After).unwrap().cut, None);
    assert_eq!(cut("acp-fork", 1, ForkSide::Before).unwrap().cut, None);

    let error = cut("acp-nope", 0, ForkSide::After).unwrap_err();
    assert_eq!(error.code, ControlErrorCode::NotFound);
}
