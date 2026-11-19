//! 选择题工具 `smelt_ask_user`：扩展把整组题目原样交过来，这里摆成一张提问卡，等人答完把
//! 答案按题交回去。
//!
//! 题目、选项说明、单选还是多选都是结构化的，不经过 Pi 只认字符串的 `ui.select`。一组题
//! 一张卡；每道题都允许不选选项、自己写答案——题目工具的合同是答案来自人，选项只是便利。
//!
//! 连接一直开着等答案。卡片答了、跳过了，或者因为回合结束、会话关掉被收走（回执被丢掉时
//! 回取消），这次等待都会返回，不会留下等不到头的线程。

use std::collections::BTreeMap;
use std::io::Write;

use agent_client_protocol::v1::schema::ElicitationContentValue;

use super::{Inner, TaskStream};
use crate::acp_conn::{
    ConversationEvent, ElicitField, ElicitFieldKind, ElicitOption, ElicitationResponder,
};

#[derive(Debug, PartialEq)]
struct Question {
    text: String,
    multi_select: bool,
    options: Vec<QuestionOption>,
}

#[derive(Debug, PartialEq)]
struct QuestionOption {
    label: String,
    description: Option<String>,
}

pub(super) fn serve(inner: &Inner, mut stream: TaskStream, request: &serde_json::Value) {
    let reply = match parse_questions(request) {
        Some(questions) => ask(inner, &questions),
        None => serde_json::json!({"ok": false, "error": "No question with options to show"}),
    };
    let _ = stream.write_all(reply.to_string().as_bytes());
    let _ = stream.write_all(b"\n");
}

fn ask(inner: &Inner, questions: &[Question]) -> serde_json::Value {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let responder = ElicitationResponder::external(move |content| {
        let _ = tx.send(content);
    });
    // 发不出去时事件连同回执一起被丢掉，回执回取消，下面照样等得到。
    let _ = inner.events.try_send(ConversationEvent::Elicitation {
        message: card_message(questions),
        fields: card_fields(questions),
        responder,
        raw_request_line: None,
    });
    match rx.recv() {
        Ok(Some(content)) => answers(questions, content),
        _ => serde_json::json!({"ok": true, "cancelled": true}),
    }
}

/// 只有一道题时题面就是卡片正文；几道题时各题在自己的字段上写题面。
fn card_message(questions: &[Question]) -> String {
    match questions {
        [only] => only.text.clone(),
        _ => String::new(),
    }
}

fn field_key(ix: usize) -> String {
    format!("q{ix}")
}

fn card_fields(questions: &[Question]) -> Vec<ElicitField> {
    questions
        .iter()
        .enumerate()
        .map(|(ix, question)| {
            let options = question
                .options
                .iter()
                .map(|option| ElicitOption {
                    value: ElicitationContentValue::String(option.label.clone()),
                    label: option.label.clone(),
                    description: option.description.clone(),
                })
                .collect();
            ElicitField {
                key: field_key(ix),
                title: question.text.clone(),
                required: true,
                allow_custom_input: true,
                kind: if question.multi_select {
                    ElicitFieldKind::MultiSelect(options)
                } else {
                    ElicitFieldKind::Select(options)
                },
            }
        })
        .collect()
}

/// 每道题的答案都是一组字符串：单选一项，多选若干项，自己写的答案也在里面。
fn answers(
    questions: &[Question],
    mut content: BTreeMap<String, ElicitationContentValue>,
) -> serde_json::Value {
    let answers: Vec<serde_json::Value> = questions
        .iter()
        .enumerate()
        .map(|(ix, question)| {
            let answer: Vec<String> = match content.remove(&field_key(ix)) {
                Some(ElicitationContentValue::String(value)) => vec![value],
                Some(ElicitationContentValue::StringArray(values)) => values,
                _ => Vec::new(),
            };
            serde_json::json!({"question": question.text, "answer": answer})
        })
        .collect();
    serde_json::json!({"ok": true, "answers": answers})
}

fn trimmed(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// 扩展已经把模型给的参数归一成 `questions: [{question, multiSelect, options: [{label,
/// description}]}]`。没有题面、没有选项的题不算；一道都不剩就不出卡。
fn parse_questions(request: &serde_json::Value) -> Option<Vec<Question>> {
    let questions: Vec<Question> = request
        .get("questions")?
        .as_array()?
        .iter()
        .filter_map(|question| {
            let text = trimmed(question.get("question"))?;
            let options: Vec<QuestionOption> = question
                .get("options")?
                .as_array()?
                .iter()
                .filter_map(|option| {
                    Some(QuestionOption {
                        label: trimmed(option.get("label"))?,
                        description: trimmed(option.get("description")),
                    })
                })
                .collect();
            (!options.is_empty()).then(|| Question {
                text,
                multi_select: question
                    .get("multiSelect")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                options,
            })
        })
        .collect();
    (!questions.is_empty()).then_some(questions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> serde_json::Value {
        serde_json::json!({
            "op": "ask_user",
            "questions": [
                {
                    "question": "按哪个方案做？",
                    "options": [
                        {"label": "按推荐方案", "description": "修 GPUI Shell 和 Smelt"},
                        {"label": "先不改", "description": "  "},
                    ],
                },
                {
                    "question": "带上哪些改动？",
                    "multiSelect": true,
                    "options": [{"label": "焦点修复"}, {"label": "弹窗定高"}],
                },
                {"question": "没有选项的题", "options": []},
            ],
        })
    }

    #[test]
    fn questions_keep_their_option_descriptions_and_drop_empty_ones() {
        let questions = parse_questions(&request()).unwrap();
        assert_eq!(questions.len(), 2, "没有选项的题不出");
        assert_eq!(
            questions[0].options,
            vec![
                QuestionOption {
                    label: "按推荐方案".into(),
                    description: Some("修 GPUI Shell 和 Smelt".into()),
                },
                QuestionOption {
                    label: "先不改".into(),
                    description: None,
                },
            ]
        );
        assert!(!questions[0].multi_select);
        assert!(questions[1].multi_select);
        assert_eq!(parse_questions(&serde_json::json!({"questions": []})), None);
    }

    #[test]
    fn a_group_of_questions_is_one_card_that_accepts_custom_answers() {
        let questions = parse_questions(&request()).unwrap();
        assert_eq!(card_message(&questions), "", "几道题时题面写在各自字段上");
        let fields = card_fields(&questions);
        assert_eq!(fields.len(), 2);
        assert!(
            fields
                .iter()
                .all(|field| field.allow_custom_input && field.required)
        );
        assert_eq!(fields[0].title, "按哪个方案做？");
        let ElicitFieldKind::Select(options) = &fields[0].kind else {
            panic!("第一题是单选");
        };
        assert_eq!(
            options[0].description.as_deref(),
            Some("修 GPUI Shell 和 Smelt")
        );
        assert!(matches!(fields[1].kind, ElicitFieldKind::MultiSelect(_)));

        let single = parse_questions(&serde_json::json!({
            "questions": [{"question": "继续吗？", "options": [{"label": "继续"}]}],
        }))
        .unwrap();
        assert_eq!(card_message(&single), "继续吗？");
    }

    #[test]
    fn answers_come_back_per_question_as_string_lists() {
        let questions = parse_questions(&request()).unwrap();
        let content = BTreeMap::from([
            (
                "q0".to_string(),
                ElicitationContentValue::String("我自己写的".into()),
            ),
            (
                "q1".to_string(),
                ElicitationContentValue::StringArray(vec!["焦点修复".into(), "弹窗定高".into()]),
            ),
        ]);
        assert_eq!(
            answers(&questions, content),
            serde_json::json!({
                "ok": true,
                "answers": [
                    {"question": "按哪个方案做？", "answer": ["我自己写的"]},
                    {"question": "带上哪些改动？", "answer": ["焦点修复", "弹窗定高"]},
                ],
            })
        );
    }
}
