//! 选中运行、又没有现场时的详情页。
//!
//! 先给结论（错误、运行结果），执行过程和元数据默认折起。落库内容在后台读：换
//! 运行或运行状态推进时排一次，render 只读内存。工作台随动画逐帧重绘，读库、
//! 反序列化放进 render 会把整个窗口拖卡。
use std::collections::HashSet;

use smelt_core::acp_chat::{AcpEntry, DiffLineTag, ToolCallStatus, ToolOutputPart};
use smelt_core::automation::{AutomationRun, AutomationRunStatus};

use super::*;

/// 单块工具输出最多展示的行数。这里供排查，不是完整日志。
const TOOL_OUTPUT_MAX_LINES: usize = 400;

const BLOCK_PROCESS: &str = "process";
const BLOCK_INPUT: &str = "input";
const BLOCK_INSTRUCTIONS: &str = "instructions";
const BLOCK_META: &str = "meta";

/// 落库内容对应的运行版本：运行没换、状态没推进，读过的就还算数。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RunDetailKey {
    run_id: String,
    status: AutomationRunStatus,
    finished_at: Option<i64>,
}

impl RunDetailKey {
    fn of(run: &AutomationRun) -> Self {
        Self {
            run_id: run.id.clone(),
            status: run.status,
            finished_at: run.finished_at,
        }
    }
}

/// 选中运行的落库内容，外加这页上展开着的折叠块。
pub(crate) struct AutomationRunDetail {
    key: RunDetailKey,
    output: Option<String>,
    instructions: Option<String>,
    transcript: Vec<AcpEntry>,
    /// 区块名，或工具调用在对话里的路径（`t/3/1`）。
    expanded: HashSet<String>,
}

impl AutomationRunDetail {
    /// `archived` 是库里整份运行，只在内存里的运行缺结果或指令快照时才读。
    fn new(
        key: RunDetailKey,
        run: &AutomationRun,
        archived: Option<AutomationRun>,
        transcript: Vec<AcpEntry>,
        expanded: HashSet<String>,
    ) -> Self {
        let output = run
            .output
            .clone()
            .or_else(|| archived.as_ref().and_then(|run| run.output.clone()));
        let instructions = run.context.agent_instructions.clone().or_else(|| {
            archived
                .as_ref()
                .and_then(|run| run.context.agent_instructions.clone())
        });
        Self {
            key,
            output: output.filter(|output| !output.trim().is_empty()),
            instructions: instructions.filter(|value| !value.trim().is_empty()),
            transcript,
            expanded,
        }
    }

    fn run_id(&self) -> &str {
        &self.key.run_id
    }

    fn is_expanded(&self, key: &str) -> bool {
        self.expanded.contains(key)
    }

    /// 对话最后一条助手正文就是运行结果，详情顶上已经画过，对话里不再整段重复。
    fn final_reply_index(&self) -> Option<usize> {
        let output = self.output.as_deref()?.trim();
        self.transcript
            .iter()
            .rposition(|entry| matches!(entry, AcpEntry::Assistant { thought: false, .. }))
            .filter(|&ix| {
                matches!(&self.transcript[ix], AcpEntry::Assistant { text, .. } if text.trim() == output)
            })
    }
}

impl Workspace {
    /// 选中运行的落库内容过期了就排一次后台读取；已是最新或正在读就什么都不做。
    /// 渲染里调用，自身不碰 SQLite。
    pub(super) fn sync_automation_run_detail(
        &mut self,
        run: &AutomationRun,
        cx: &mut Context<Self>,
    ) {
        let key = RunDetailKey::of(run);
        let surface = &mut self.automation_surface;
        if surface
            .run_detail
            .as_ref()
            .is_some_and(|detail| detail.key == key)
            || surface.run_detail_loading.as_ref() == Some(&key)
        {
            return;
        }
        surface.run_detail_loading = Some(key.clone());
        let need_archive = run.output.is_none() || run.context.agent_instructions.is_none();
        let run = run.clone();
        cx.spawn(async move |this, cx| {
            let run_id = run.id.clone();
            let (archived, transcript) = cx
                .background_executor()
                .spawn(async move {
                    // 运行记录在运行时库里；读不到时按「没有归档、没有对话记录」显示。
                    let detail =
                        crate::runtime_call::call::<smelt_core::control_api::AutomationRunDetail>(
                            smelt_core::control_api::AutomationRunDetailParams {
                                run_id,
                                include_archived_run: need_archive,
                            },
                            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                        )
                        .unwrap_or_default();
                    (detail.archived_run, detail.transcript)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let surface = &mut this.automation_surface;
                // 读的过程中又换了运行或状态又推进了：这份结果作废，由新的那次读取接手。
                if surface.run_detail_loading.as_ref() != Some(&key) {
                    return;
                }
                surface.run_detail_loading = None;
                // 同一次运行只是状态推进时，展开着的块保持展开。
                let expanded = surface
                    .run_detail
                    .take()
                    .filter(|detail| detail.key.run_id == key.run_id)
                    .map(|detail| detail.expanded)
                    .unwrap_or_default();
                surface.run_detail = Some(AutomationRunDetail::new(
                    key, &run, archived, transcript, expanded,
                ));
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_automation_run_block(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(detail) = self.automation_surface.run_detail.as_mut() else {
            return;
        };
        if !detail.expanded.remove(&key) {
            detail.expanded.insert(key);
        }
        cx.notify();
    }

    pub(super) fn render_automation_run_detail(
        &self,
        run: &AutomationRun,
        entity: Entity<Workspace>,
    ) -> AnyElement {
        let Some(detail) = self
            .automation_surface
            .run_detail
            .as_ref()
            .filter(|detail| detail.run_id() == run.id)
        else {
            return div()
                .text_sm()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("正在读取运行记录…")
                .into_any_element();
        };
        let process_meta = transcript_summary(&detail.transcript);
        let final_reply = detail.final_reply_index();

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(run_summary_line(run))
            .children(run.error.clone().map(run_error_card))
            .child(run_result_section(run, detail))
            .when(!detail.transcript.is_empty(), |column| {
                column.child(run_detail_disclosure(
                    BLOCK_PROCESS,
                    "执行过程",
                    Some(process_meta),
                    detail,
                    &entity,
                    || render_transcript(&detail.transcript, "t/", final_reply, detail, &entity),
                ))
            })
            .child(run_detail_disclosure(
                BLOCK_INPUT,
                "本次输入",
                None,
                detail,
                &entity,
                || {
                    let prompt = run
                        .context
                        .prompt
                        .clone()
                        .filter(|prompt| !prompt.trim().is_empty())
                        .unwrap_or_else(|| "（无）".to_string());
                    detail_card(crate::markdown_mermaid::markdown_view(
                        format!("automation-run-prompt-{}", run.id),
                        prompt,
                    ))
                },
            ))
            .children(detail.instructions.clone().map(|instructions| {
                run_detail_disclosure(
                    BLOCK_INSTRUCTIONS,
                    "智能体指令快照",
                    None,
                    detail,
                    &entity,
                    || {
                        detail_card(crate::markdown_mermaid::markdown_view(
                            format!("automation-run-instructions-{}", run.id),
                            instructions,
                        ))
                    },
                )
            }))
            .child(run_detail_disclosure(
                BLOCK_META,
                "运行信息",
                None,
                detail,
                &entity,
                || run_metadata(run),
            ))
            .into_any_element()
    }
}

fn run_status_color(status: AutomationRunStatus) -> u32 {
    match status {
        AutomationRunStatus::Completed => crate::ui_theme::green(),
        AutomationRunStatus::Failed => crate::ui_theme::red(),
        AutomationRunStatus::Cancelled | AutomationRunStatus::Skipped => {
            crate::ui_theme::text_faint()
        }
        AutomationRunStatus::AwaitingApproval | AutomationRunStatus::WaitingForUser => {
            crate::ui_theme::yellow()
        }
        AutomationRunStatus::Starting
        | AutomationRunStatus::Queued
        | AutomationRunStatus::Dispatching
        | AutomationRunStatus::Running => crate::ui_theme::blue(),
    }
}

/// 运行列表和详情头共用的状态点。
pub(super) fn run_status_dot(status: AutomationRunStatus) -> Div {
    div()
        .size(px(7.))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(run_status_color(status)))
}

/// 一行交代谁、什么时候、跑了多久。
fn run_summary_line(run: &AutomationRun) -> AnyElement {
    let agent = run
        .context
        .agent_definition_name
        .as_deref()
        .map(agent_display_name)
        .unwrap_or_else(|| "未知智能体".to_string());
    let mut parts = vec![format_unix_local(run.started_at.unwrap_or(run.created_at))];
    if let (Some(started_at), Some(finished_at)) = (run.started_at, run.finished_at) {
        parts.push(format!("用时 {}", format_elapsed(finished_at - started_at)));
    }
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(rgb(crate::ui_theme::text_muted()))
        .child(
            Icon::new(IconName::Bot)
                .size(px(13.))
                .text_color(rgb(crate::ui_theme::text_muted())),
        )
        .child(
            div()
                .text_color(rgb(crate::ui_theme::text_mid()))
                .child(agent),
        )
        .child(
            div()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("·"),
        )
        .child(div().child(parts.join(" · ")))
        .into_any_element()
}

fn run_error_card(error: String) -> AnyElement {
    let red = crate::ui_theme::red();
    div()
        .w_full()
        .flex()
        .gap_2()
        .px_3()
        .py_2p5()
        .rounded(crate::ui_theme::row_radius())
        .border_1()
        .border_color(crate::ui_theme::tint(red, 0x40))
        .bg(crate::ui_theme::tint(red, 0x14))
        .child(
            div().pt(px(2.)).child(
                Icon::new(IconName::CircleX)
                    .size(px(14.))
                    .text_color(rgb(red)),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .text_color(rgb(red))
                        .child("运行失败"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(crate::ui_theme::text()))
                        .child(error),
                ),
        )
        .into_any_element()
}

fn run_result_section(run: &AutomationRun, detail: &AutomationRunDetail) -> AnyElement {
    let body = match detail.output.clone() {
        Some(output) => detail_card(crate::markdown_mermaid::markdown_view(
            format!("automation-run-output-{}", run.id),
            output,
        )),
        None => {
            let hint = if run.status.is_active() {
                "还在运行，结束后结果显示在这里。"
            } else if run.error.is_some() {
                "没有产出结果。"
            } else {
                "这次运行没有给出最终回复。"
            };
            div()
                .text_sm()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child(hint)
                .into_any_element()
        }
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_color(rgb(crate::ui_theme::text_bright()))
                .child("运行结果"),
        )
        .child(body)
        .into_any_element()
}

fn detail_card(body: impl IntoElement) -> AnyElement {
    content_card()
        .w_full()
        .min_w_0()
        .p(crate::ui_theme::card_padding())
        .text_sm()
        .child(body)
        .into_any_element()
}

/// 可折叠区块：标题行常驻，正文只在展开时才构建，折着的区块不花渲染成本。
fn run_detail_disclosure(
    key: &'static str,
    title: &'static str,
    meta: Option<String>,
    detail: &AutomationRunDetail,
    entity: &Entity<Workspace>,
    body: impl FnOnce() -> AnyElement,
) -> AnyElement {
    let expanded = detail.is_expanded(key);
    let entity = entity.clone();
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .id(SharedString::from(format!("automation-run-block-{key}")))
                .flex()
                .items_center()
                .gap_1p5()
                .mx(px(-6.))
                .px(px(6.))
                .h(crate::ui_theme::height::xs())
                .rounded(crate::ui_theme::row_radius())
                .cursor_pointer()
                .hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
                .child(disclosure_chevron(expanded))
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .text_color(rgb(crate::ui_theme::text_bright()))
                        .child(title),
                )
                .children(meta.map(|meta| {
                    div()
                        .text_xs()
                        .text_color(rgb(crate::ui_theme::text_faint()))
                        .child(meta)
                }))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |workspace, cx| {
                        workspace.toggle_automation_run_block(key.to_string(), cx)
                    });
                }),
        )
        .when(expanded, |section| section.child(body()))
        .into_any_element()
}

fn disclosure_chevron(expanded: bool) -> Icon {
    Icon::new(if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .size(px(12.))
    .text_color(rgb(crate::ui_theme::text_faint()))
}

fn transcript_summary(entries: &[AcpEntry]) -> String {
    fn count(entries: &[AcpEntry], tools: &mut usize, failed: &mut usize) {
        for entry in entries {
            if let AcpEntry::ToolCall {
                status, children, ..
            } = entry
            {
                *tools += 1;
                if *status == ToolCallStatus::Failed {
                    *failed += 1;
                }
                count(children, tools, failed);
            }
        }
    }
    let (mut tools, mut failed) = (0, 0);
    count(entries, &mut tools, &mut failed);
    match (tools, failed) {
        (0, _) => format!("{} 条消息", entries.len()),
        (tools, 0) => format!("{tools} 次工具调用"),
        (tools, failed) => format!("{tools} 次工具调用 · {failed} 次失败"),
    }
}

/// 对话按条画：说的话是正文，工具和思考是一行可展开的步骤。`path` 让嵌套的
/// 子代理步骤有各自的展开键。
fn render_transcript(
    entries: &[AcpEntry],
    path: &str,
    final_reply: Option<usize>,
    detail: &AutomationRunDetail,
    entity: &Entity<Workspace>,
) -> AnyElement {
    let mut column = div().w_full().min_w_0().flex().flex_col().gap_1();
    for (ix, entry) in entries.iter().enumerate() {
        let key = format!("{path}{ix}");
        let element = match entry {
            AcpEntry::User(text) => transcript_user(&key, detail.run_id(), text, 0),
            AcpEntry::UserWithImages { text, images } => {
                transcript_user(&key, detail.run_id(), text, images.len())
            }
            AcpEntry::Assistant { thought: false, .. } if final_reply == Some(ix) => div()
                .py_1()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("最终回复，见上方运行结果")
                .into_any_element(),
            AcpEntry::Assistant {
                text,
                thought: false,
            } => {
                if text.trim().is_empty() {
                    continue;
                }
                div()
                    .py_1()
                    .text_sm()
                    .child(crate::markdown_mermaid::markdown_view(
                        format!("automation-run-reply-{}-{key}", detail.run_id()),
                        text.clone(),
                    ))
                    .into_any_element()
            }
            AcpEntry::Assistant {
                text,
                thought: true,
            } => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let preview = text.lines().next().unwrap_or_default().to_string();
                transcript_step(
                    &key,
                    process_icon(&smelt_core::acp_chat::ToolKind::Think),
                    format!("思考  {preview}"),
                    None,
                    detail,
                    entity,
                    Some(|| {
                        div()
                            .text_xs()
                            .text_color(rgb(crate::ui_theme::text_muted()))
                            .child(text.to_string())
                            .into_any_element()
                    }),
                )
            }
            AcpEntry::ToolCall {
                title,
                kind,
                status,
                output,
                children,
                ..
            } => {
                let flag = match status {
                    ToolCallStatus::Failed => Some(StepFlag::Failed),
                    ToolCallStatus::Pending | ToolCallStatus::InProgress => {
                        Some(StepFlag::Unfinished)
                    }
                    ToolCallStatus::Completed => None,
                };
                let expandable = !output.is_empty() || !children.is_empty();
                let output_id = format!("automation-run-tool-{}-{key}", detail.run_id());
                let child_path = format!("{key}/");
                transcript_step(
                    &key,
                    process_icon(kind),
                    acp_view::compact_tool_headline(*kind, title),
                    flag,
                    detail,
                    entity,
                    expandable.then_some(move || {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .when(!output.is_empty(), |body| {
                                body.child(tool_output_view(output_id, output))
                            })
                            .when(!children.is_empty(), |body| {
                                body.child(render_transcript(
                                    children,
                                    &child_path,
                                    None,
                                    detail,
                                    entity,
                                ))
                            })
                            .into_any_element()
                    }),
                )
            }
            AcpEntry::Divider(label) => div()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().h(px(1.)).bg(crate::ui_theme::hairline()))
                .when(!label.trim().is_empty(), |row| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(rgb(crate::ui_theme::text_faint()))
                            .child(label.trim().to_string()),
                    )
                    .child(div().flex_1().h(px(1.)).bg(crate::ui_theme::hairline()))
                })
                .into_any_element(),
            AcpEntry::TaskNote(note) => {
                let tail = note.output_tail.trim_end();
                let output_id = format!("automation-run-note-{}-{key}", detail.run_id());
                transcript_step(
                    &key,
                    IconName::Info,
                    note.summary(),
                    None,
                    detail,
                    entity,
                    (!tail.is_empty()).then_some(move || {
                        tool_output_view(output_id, &[ToolOutputPart::Text(tail.to_string())])
                    }),
                )
            }
        };
        column = column.child(element);
    }
    column.into_any_element()
}

fn process_icon(kind: &smelt_core::acp_chat::ToolKind) -> IconName {
    acp_view::process_timeline_icon(kind)
}

fn transcript_user(key: &str, run_id: &str, text: &str, images: usize) -> AnyElement {
    let mut body = text.trim().to_string();
    if images > 0 {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!("*（{images} 张图片，未写入存档）*"));
    }
    div()
        .py_1()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(rgb(crate::ui_theme::text_faint()))
                .child("输入"),
        )
        .child(detail_card(crate::markdown_mermaid::markdown_view(
            format!("automation-run-user-{run_id}-{key}"),
            body,
        )))
        .into_any_element()
}

/// 步骤行尾的状态。正常完成的不标。
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepFlag {
    Failed,
    Unfinished,
}

impl StepFlag {
    fn label(self) -> &'static str {
        match self {
            Self::Failed => "失败",
            Self::Unfinished => "未完成",
        }
    }

    fn color(self) -> u32 {
        match self {
            Self::Failed => crate::ui_theme::red(),
            Self::Unfinished => crate::ui_theme::text_faint(),
        }
    }
}

/// 一行步骤：图标、动词句、可选状态。能展开的整行可点，正文缩进挂在下面。
fn transcript_step(
    key: &str,
    icon: IconName,
    headline: String,
    flag: Option<StepFlag>,
    detail: &AutomationRunDetail,
    entity: &Entity<Workspace>,
    body: Option<impl FnOnce() -> AnyElement>,
) -> AnyElement {
    let expandable = body.is_some();
    let expanded = expandable && detail.is_expanded(key);
    let ink = if flag == Some(StepFlag::Failed) {
        crate::ui_theme::red()
    } else {
        crate::ui_theme::text_muted()
    };
    let toggle_key = key.to_string();
    let toggle_entity = entity.clone();
    let row = div()
        .id(SharedString::from(format!("automation-run-step-{key}")))
        .min_h(px(24.))
        .mx(px(-6.))
        .px(px(6.))
        .flex()
        .items_center()
        .gap_2()
        .rounded(crate::ui_theme::row_radius())
        .child(Icon::new(icon).size(px(12.)).text_color(rgb(ink)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(ink))
                .child(headline),
        )
        .children(flag.map(|flag| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(flag.color()))
                .child(flag.label())
        }))
        .when(expandable, |row| {
            row.cursor_pointer()
                .hover(|row| row.bg(rgb(crate::ui_theme::bg_row_hover())))
                .child(disclosure_chevron(expanded))
                .on_click(move |_, _, cx| {
                    let key = toggle_key.clone();
                    toggle_entity.update(cx, |workspace, cx| {
                        workspace.toggle_automation_run_block(key, cx)
                    });
                })
        });
    let body = body.filter(|_| expanded);
    div()
        .w_full()
        .flex()
        .flex_col()
        .child(row)
        .children(body.map(|body| {
            div()
                .w_full()
                .min_w_0()
                .pl(px(20.))
                .pt_1()
                .pb_2()
                .child(body())
        }))
        .into_any_element()
}

/// 工具输出原样进围栏代码块：等宽、可选中、右上角带复制，`---` 和 `#` 不会被
/// 当成 Markdown 结构。超长输出截掉尾部，只留排查要用的部分。
fn tool_output_view(id: String, parts: &[ToolOutputPart]) -> AnyElement {
    let mut blocks = Vec::new();
    for part in parts {
        match part {
            ToolOutputPart::Text(text) => {
                let text = text.trim_end();
                if !text.is_empty() {
                    blocks.push(fenced_block("", text));
                }
            }
            ToolOutputPart::Terminal {
                output,
                exit_code,
                truncated,
                ..
            } => {
                let output = output.trim_end();
                if !output.is_empty() {
                    blocks.push(fenced_block("", output));
                }
                if *truncated {
                    blocks.push("*输出在运行时已截断*".to_string());
                }
                if let Some(code) = exit_code.filter(|code| *code != 0) {
                    blocks.push(format!("*退出码 {code}*"));
                }
            }
            ToolOutputPart::Diff {
                path,
                old_text,
                new_text,
            } => {
                let lines = smelt_core::acp_chat::diff_lines(
                    old_text.as_deref().unwrap_or_default(),
                    new_text,
                );
                let mut body = format!("--- {path}\n");
                for line in lines {
                    body.push(match line.tag {
                        DiffLineTag::Added => '+',
                        DiffLineTag::Removed => '-',
                        DiffLineTag::Context => ' ',
                    });
                    body.push_str(line.text.trim_end_matches('\n'));
                    body.push('\n');
                }
                blocks.push(fenced_block("diff", body.trim_end()));
            }
            ToolOutputPart::Image(image) => {
                blocks.push(format!("*图片 {}，未写入存档*", image.mime));
            }
        }
    }
    if blocks.is_empty() {
        return div()
            .text_xs()
            .text_color(rgb(crate::ui_theme::text_faint()))
            .child("（无输出）")
            .into_any_element();
    }
    div()
        .w_full()
        .min_w_0()
        .text_xs()
        .child(crate::markdown_mermaid::markdown_view(
            id,
            blocks.join("\n\n"),
        ))
        .into_any_element()
}

/// 围栏比正文里最长的反引号串多一个，输出里带 ``` 也不会提前闭合。
fn fenced_block(lang: &str, body: &str) -> String {
    let total = body.lines().count();
    let mut body = if total > TOOL_OUTPUT_MAX_LINES {
        let mut clipped = body
            .lines()
            .take(TOOL_OUTPUT_MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        clipped.push_str(&format!(
            "\n…（还有 {} 行未显示）",
            total - TOOL_OUTPUT_MAX_LINES
        ));
        clipped
    } else {
        body.to_string()
    };
    let longest = body
        .split(|ch| ch != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest.max(2) + 1);
    body.insert_str(0, &format!("{fence}{lang}\n"));
    body.push('\n');
    body.push_str(&fence);
    body
}

fn run_metadata(run: &AutomationRun) -> AnyElement {
    let mut rows = vec![
        ("来源", run_source_label(run.source).to_string()),
        (
            "智能体",
            format!(
                "{} · {}",
                run.context
                    .agent_definition_name
                    .as_deref()
                    .map(agent_display_name)
                    .unwrap_or_else(|| "未知智能体".to_string()),
                run.context.engine_kind_id.as_deref().unwrap_or("未知引擎")
            ),
        ),
        ("工作区", run.context.cwd.clone()),
        ("创建", format_unix_local(run.created_at)),
    ];
    if let Some(scheduled_for) = run.scheduled_for {
        rows.push(("计划", format_unix_local(scheduled_for)));
    }
    if let Some(delivery_attempt_at) = run.delivery_attempt_at {
        rows.push((
            "投递",
            format!(
                "{} · 共 {} 次",
                format_unix_local(delivery_attempt_at),
                run.delivery_attempts
            ),
        ));
    }
    if let Some(started_at) = run.started_at {
        rows.push(("开始", format_unix_local(started_at)));
    }
    if let Some(finished_at) = run.finished_at {
        rows.push(("结束", format_unix_local(finished_at)));
    }
    rows.push(("Run ID", run.id.clone()));
    rows.push(("Automation ID", run.automation_id.clone()));
    if let Some(definition_id) = run.context.action.agent_definition_id() {
        rows.push(("智能体定义 ID", definition_id.to_string()));
    }
    if let Some(session_id) = run.session_id.clone() {
        rows.push(("ACP 会话", session_id));
    }
    if let Some(provider_session_id) = run.provider_session_id.clone() {
        rows.push(("Provider 会话", provider_session_id));
    }
    content_card()
        .w_full()
        .px(crate::ui_theme::card_padding())
        .py_2()
        .flex()
        .flex_col()
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .py_1()
                .flex()
                .gap_4()
                .text_xs()
                .child(
                    div()
                        .w(px(96.))
                        .flex_shrink_0()
                        .text_color(rgb(crate::ui_theme::text_faint()))
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(crate::ui_theme::text_mid()))
                        .child(value),
                )
        }))
        .into_any_element()
}

fn format_elapsed(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => format!("{seconds} 秒"),
        60..3600 => format!("{} 分 {} 秒", seconds / 60, seconds % 60),
        _ => format!("{} 小时 {} 分", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    // 不用 `super::*`：那条 glob 链会带进 `gpui::*` 里的 `test` 属性宏，把这里的
    // `#[test]` 换成 gpui 的版本，展开时撞上递归上限。
    use super::{
        AcpEntry, AutomationRunDetail, AutomationRunStatus, HashSet, RunDetailKey,
        TOOL_OUTPUT_MAX_LINES, fenced_block, format_elapsed,
    };

    #[test]
    fn fence_outgrows_backticks_inside_output() {
        let block = fenced_block("", "a ```` b");
        assert!(block.starts_with("`````\n"));
        assert!(block.ends_with("\n`````"));
    }

    #[test]
    fn markdown_lookalike_output_stays_inside_the_fence() {
        let block = fenced_block("", "---\nname: persona\n---");
        assert_eq!(block, "```\n---\nname: persona\n---\n```");
    }

    #[test]
    fn long_output_is_clipped_with_a_count() {
        let body = (0..TOOL_OUTPUT_MAX_LINES + 5)
            .map(|ix| ix.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let block = fenced_block("", &body);
        assert!(block.contains("还有 5 行未显示"));
        assert!(!block.contains(&format!("\n{}\n", TOOL_OUTPUT_MAX_LINES + 1)));
    }

    fn detail(output: Option<&str>, transcript: Vec<AcpEntry>) -> AutomationRunDetail {
        AutomationRunDetail {
            key: RunDetailKey {
                run_id: "run".into(),
                status: AutomationRunStatus::Completed,
                finished_at: None,
            },
            output: output.map(str::to_string),
            instructions: None,
            transcript,
            expanded: HashSet::new(),
        }
    }

    fn reply(text: &str) -> AcpEntry {
        AcpEntry::Assistant {
            text: text.into(),
            thought: false,
        }
    }

    #[test]
    fn final_reply_is_folded_only_when_it_matches_the_result() {
        let entries = vec![reply("中间说明"), reply("结论\n")];
        assert_eq!(
            detail(Some("结论"), entries.clone()).final_reply_index(),
            Some(1)
        );
        assert_eq!(
            detail(Some("别的"), entries.clone()).final_reply_index(),
            None
        );
        assert_eq!(detail(None, entries).final_reply_index(), None);
    }

    #[test]
    fn elapsed_reads_in_natural_units() {
        assert_eq!(format_elapsed(42), "42 秒");
        assert_eq!(format_elapsed(125), "2 分 5 秒");
        assert_eq!(format_elapsed(3720), "1 小时 2 分");
    }
}
