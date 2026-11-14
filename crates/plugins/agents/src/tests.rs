//! 页面上的纯判断：记忆列表、名字和摘要的写法。

use std::time::{Duration, SystemTime};

use smelt_plugin_api::model::agent_memory::MemoryEntry;
use smelt_plugin_api::model::daemon_state::DaemonPhase;

// 不用 `use super::*;`：父模块有 gpui 的 glob 导入，`gpui::test` 会顶掉 `#[test]`，
// 也让 trait 解析变慢。只导入用到的名字。
use super::model::{
    MemoryFilter, agent_catalog_monogram, memory_summary_text, session_phase_settled,
    visible_memories,
};

fn memory(name: &str, kind: Option<&str>) -> MemoryEntry {
    MemoryEntry {
        file_name: format!("{name}.md"),
        name: name.to_string(),
        description: String::new(),
        kind: kind.map(str::to_string),
        path: std::path::PathBuf::from(format!("/m/{name}.md")),
        read_count: 0,
        last_used: None,
        stale: false,
        pending_reason: None,
        body: String::new(),
    }
}

/// 待确认要用户处理，排最前；其余按最近用过排，没用过的垫底，同档按名字。
#[test]
fn memory_list_puts_pending_first_then_recently_used() {
    let at = |secs| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
    let mut old = memory("old", Some("project"));
    old.last_used = at(100);
    let mut recent = memory("recent", Some("project"));
    recent.last_used = at(200);
    let never_b = memory("b-never", Some("project"));
    let never_a = memory("a-never", Some("project"));
    let mut pending = memory("pending", Some("project"));
    pending.pending_reason = Some("读过外部内容".to_string());
    let entries = vec![old, never_b, recent, pending, never_a];

    let names: Vec<&str> = visible_memories(&entries, "", MemoryFilter::All)
        .into_iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["pending", "recent", "old", "a-never", "b-never"]);
}

/// 搜索不分大小写，名字、描述、文件名、正文任一命中都算；筛选和搜索同时生效。
#[test]
fn memory_search_and_filter_narrow_the_list_together() {
    let mut rule = memory("Reply-Language", Some("feedback"));
    rule.body = "回复用中文".to_string();
    let mut fact = memory("deploy", Some("reference"));
    fact.description = "流水线地址".to_string();
    let untyped = memory("scratch", None);
    let mut pending = memory("proxy", Some("user"));
    pending.pending_reason = Some("自动化写的".to_string());
    let entries = vec![rule, fact, untyped, pending];
    let names = |query: &str, filter| -> Vec<String> {
        visible_memories(&entries, query, filter)
            .into_iter()
            .map(|entry| entry.name.clone())
            .collect()
    };

    assert_eq!(names("reply", MemoryFilter::All), ["Reply-Language"]);
    assert_eq!(names("中文", MemoryFilter::All), ["Reply-Language"]);
    assert_eq!(names("流水线", MemoryFilter::All), ["deploy"]);
    assert_eq!(names("scratch.md", MemoryFilter::All), ["scratch"]);
    assert_eq!(names("  ", MemoryFilter::All).len(), 4, "空白搜索等于不搜");

    assert_eq!(names("", MemoryFilter::Pending), ["proxy"]);
    assert_eq!(names("", MemoryFilter::Rules), ["proxy", "Reply-Language"]);
    assert_eq!(
        names("", MemoryFilter::Facts),
        ["deploy", "scratch"],
        "没写类型的按事实算"
    );
    assert!(names("中文", MemoryFilter::Facts).is_empty());
}

/// 配置页那一行摘要：条数固定一行，待确认单独提醒，没有就不提。
#[test]
fn memory_summary_is_one_line_of_counts() {
    assert_eq!(memory_summary_text(0, 0), "还没有记忆");
    assert_eq!(memory_summary_text(8, 4), "这个智能体 8 条 · 全局 4 条");
    assert_eq!(memory_summary_text(0, 3), "这个智能体 0 条 · 全局 3 条");
}

/// 记忆卡片在会话停下时重扫：模型还在想、工具还在跑时，元数据可能还没落库。
#[test]
fn memory_card_rescans_only_when_the_session_is_between_steps() {
    for phase in [
        DaemonPhase::Connecting,
        DaemonPhase::Thinking,
        DaemonPhase::ExecutingTool,
    ] {
        assert!(!session_phase_settled(phase), "{phase:?} 还在一步当中");
    }
    for phase in [
        DaemonPhase::AwaitingApproval,
        DaemonPhase::WaitingForUser,
        DaemonPhase::Succeeded,
        DaemonPhase::Failed,
        DaemonPhase::Idle,
        DaemonPhase::Dead,
    ] {
        assert!(session_phase_settled(phase), "{phase:?} 已停在两步之间");
    }
}

#[test]
fn catalog_monogram_keeps_two_characters_when_they_identify_the_agent() {
    assert_eq!(agent_catalog_monogram("编码助手"), "编码");
    assert_eq!(agent_catalog_monogram("日志与磁盘清理智能体"), "日志");
    assert_eq!(agent_catalog_monogram("Pi 智能体 3"), "Pi");
    assert_eq!(agent_catalog_monogram("A"), "A");
    assert_eq!(agent_catalog_monogram("   "), "智");
}

mod agent_definition_ui_tests {
    use api::model::agent_definition::AgentDefinition;
    use api::model::agent_kind::ConversationAgentKind;
    use smelt_plugin_api as api;

    use super::super::model::{
        agent_card_prompt, agent_conversation_start_error, agent_display_name,
        agent_instructions_footer, agent_usage_summary,
    };

    fn definition(name: &str, prompt: &str, engine_kind_id: &str) -> AgentDefinition {
        AgentDefinition {
            id: "quant".into(),
            name: name.into(),
            description: String::new(),
            engine_kind_id: engine_kind_id.into(),
            prompt: prompt.into(),
            plugins: Vec::new(),
            context_folders: Vec::new(),
            context_links: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn unnamed_agent_falls_back_to_placeholder() {
        assert_eq!(agent_display_name(""), "未命名");
        assert_eq!(agent_display_name("   "), "未命名");
        assert_eq!(agent_display_name(" 量化交易 "), "量化交易");
    }

    #[test]
    fn conversation_start_does_not_require_name_or_instructions() {
        assert!(definition("Pi 智能体", "", ConversationAgentKind::Pi.id()).is_ready());
        assert_eq!(
            agent_conversation_start_error(&definition("", "", ConversationAgentKind::Pi.id())),
            None
        );
        assert_eq!(
            agent_conversation_start_error(&definition("量化交易", "规则", "unknown")),
            Some("请选择一个可用的执行引擎")
        );
        assert_eq!(
            agent_conversation_start_error(&definition(
                "量化交易",
                "规则",
                ConversationAgentKind::Pi.id()
            )),
            None
        );
    }

    #[test]
    fn card_prompt_stays_within_two_lines_and_skips_blank() {
        assert_eq!(agent_card_prompt("   \n  "), None);
        assert_eq!(
            agent_card_prompt("先验证行情时间，再给出结论。"),
            Some("先验证行情时间，再给出结论。".into())
        );
        let long = "你是一个专业的本地系统与磁盘清理智能体。职责：检查并安全清理本地超过 7 天的历史日志、AI 助手会话轨迹与临时缓存，然后再做别的。";
        let card = agent_card_prompt(long).unwrap();
        assert!(card.ends_with('…'));
        assert!(card.chars().count() <= 65);
    }

    #[test]
    fn instructions_footer_includes_count_only_when_written() {
        assert_eq!(
            agent_instructions_footer(0),
            "会作为系统提示词带进每次对话和自动化。"
        );
        assert_eq!(
            agent_instructions_footer(48),
            "48 字 · 会作为系统提示词带进每次对话和自动化"
        );
    }

    #[test]
    fn usage_summary_omits_zero_counts_and_joins_the_rest() {
        assert_eq!(agent_usage_summary(0, 0), "还没有对话");
        assert_eq!(agent_usage_summary(2, 0), "2 段对话");
        assert_eq!(agent_usage_summary(0, 1), "1 条自动化");
        assert_eq!(agent_usage_summary(2, 1), "2 段对话 · 1 条自动化");
    }
}
