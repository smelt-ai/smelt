//! 打开的文件：打开、保存、外部改动后重读、预览、未保存时切换的确认。
//!
//! 读写都交给文件所在那台机器的运行时，后台跑。每次打开有自己的编号：结果回来时按编号找
//! 那个文件，它可能已经随项目停放了，照样写回去。

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use gpui_component::WindowExt as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogFooter;
use gpui_component::input::{EditorState, InputEvent, Position};
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::client::protocol::control::{ControlClientError, ControlErrorCode};
use smelt_plugin_api::client::protocol::files::{
    FileContent, FileOpenCall, FileOpenResult, FilePathParams, FileSaveCall, FileSaveParams,
    is_previewable_image,
};

use crate::html_preview::HtmlPreview;
use crate::model::{FileRef, OpenFile, PreviewKind, editor_language_for_path};
use crate::rt::{RUNTIME_CALL_TIMEOUT, call_on, error_text, target};
use crate::{FilesPanel, ProjectRef};

/// 编辑器停手多久后再更新预览。
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(80);

enum SaveOutcome {
    Saved(String),
    Conflict,
    Error(String),
}

impl FilesPanel {
    /// 编号是 `id` 的那个打开的文件：当前的，或停放着的项目里的。
    fn open_file_with_id(&mut self, id: u64) -> Option<&mut OpenFile> {
        if self.open_file.as_ref().is_some_and(|of| of.id == id) {
            return self.open_file.as_mut();
        }
        self.parked
            .values_mut()
            .find_map(|ui| ui.open_file.as_mut().filter(|of| of.id == id))
    }

    pub(crate) fn close_open_file(&mut self) {
        self.open_file = None;
        self._editor_sub = None;
        self.preview_task = None;
        self.pending_switch_after_save = None;
    }

    /// 对话框开了或关了。HTML 预览的原生网页视图盖得住对话框，要重画一次让它让开或回来。
    pub(crate) fn dialog_toggled(&mut self, cx: &mut Context<Self>) {
        if let Some(html) = self
            .open_file
            .as_ref()
            .and_then(|of| of.html_preview.clone())
        {
            html.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    /// 右上角源码 / 预览切换（只对 [`PreviewKind`] 认得的文件有效）。
    pub(crate) fn set_file_preview(
        &mut self,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(of) = self.open_file.as_mut() {
            of.preview = preview;
            of.html_preview = (preview
                && PreviewKind::for_path(&of.path) == Some(PreviewKind::Html))
            .then(|| cx.new(|cx| HtmlPreview::new(window, cx)));
        }
        if preview {
            self.refresh_preview_source(cx);
        }
        cx.notify();
    }

    fn refresh_preview_source(&mut self, cx: &mut Context<Self>) {
        let Some(of) = self.open_file.as_mut() else {
            return;
        };
        if !of.preview {
            return;
        }
        let next = of.editor.read(cx).value().to_string();
        if of.preview_source.as_ref() != next {
            of.preview_source = next.into();
        }
        of.sync_html_preview(cx);
    }

    /// 编辑器停手 [`PREVIEW_DEBOUNCE`] 后再更新预览；又按了键就换掉上一次。
    fn schedule_preview_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.open_file.as_ref().is_some_and(|of| of.preview) {
            return;
        }
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                this.refresh_preview_source(cx);
                cx.notify();
            });
        }));
    }

    /// 打开树那台机器上的文件。
    pub(crate) fn view_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.view_file_at(path, None, window, cx);
    }

    /// 打开树那台机器上的文件，可跳到某一行（从 1 数，搜索命中用）。
    pub(crate) fn view_file_at(
        &mut self,
        path: String,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = FileRef {
            runtime: self.tree_runtime(),
            path,
        };
        self.view_file_on(file, line, window, cx);
    }

    /// 打开某台机器上的文件（对话里的文件链接可能指向别的机器）。当前文件有没保存的改动时
    /// 先问怎么办。
    pub(crate) fn view_file_on(
        &mut self,
        file: FileRef,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(current) = self.open_file.as_ref().filter(|of| of.dirty(cx)) else {
            self.open_file_now(file, line, window, cx);
            return;
        };
        if current.runtime == file.runtime && current.path == file.path {
            // 就是这个文件：不重读（会丢掉改动），只跳行。
            if let Some(line) = line {
                let editor = current.editor.clone();
                editor.update(cx, |state, cx| {
                    let line0 = line.saturating_sub(1) as u32;
                    state.set_cursor_position(Position::new(line0, 0), window, cx);
                });
            }
            return;
        }
        self.confirm_unsaved_switch(current.id, file, line, window, cx);
    }

    fn confirm_unsaved_switch(
        &mut self,
        current_id: u64,
        target: FileRef,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = |file: &FileRef, cx: &App| {
            self.path_style(&file.runtime, cx)
                .file_name(&file.path)
                .unwrap_or(&file.path)
                .to_string()
        };
        let current = self
            .open_file
            .as_ref()
            .map(|of| FileRef {
                runtime: of.runtime.clone(),
                path: of.path.clone(),
            })
            .map(|file| name(&file, cx))
            .unwrap_or_default();
        let target_name = name(&target, cx);
        let panel = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let discard_panel = panel.clone();
            let save_panel = panel.clone();
            let close_panel = panel.clone();
            let discard_target = target.clone();
            let save_target = target.clone();
            alert
                .title(format!("「{current}」有未保存的改动"))
                .description(format!(
                    "要切换到「{target_name}」了，这些改动还没保存，要怎么处理？"
                ))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("unsaved-cancel")
                                .label("取消")
                                .outline()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("unsaved-discard")
                                .label("不保存，直接切换")
                                .outline()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    discard_panel.update(cx, |this, cx| {
                                        this.open_file_now(
                                            discard_target.clone(),
                                            line,
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        )
                        .child(
                            Button::new("unsaved-save-switch")
                                .label("保存并切换")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    save_panel.update(cx, |this, cx| {
                                        this.pending_switch_after_save =
                                            Some((current_id, save_target.clone()));
                                        this.save_open_file(cx);
                                    });
                                }),
                        ),
                )
                .on_close(move |_, _, cx| {
                    close_panel.update(cx, |this, cx| this.dialog_toggled(cx))
                })
        });
        self.dialog_toggled(cx);
    }

    /// 真正打开：建一个可编辑的代码编辑器（tree-sitter 高亮、行号、搜索），后台读文件，
    /// 读完灌进去。`goto_line` 从 1 数，读完把光标放过去。
    pub(crate) fn open_file_now(
        &mut self,
        file: FileRef,
        goto_line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let FileRef { runtime, path } = file;
        // 树只显示当前项目那台机器的目录，别的机器上的文件定位不到。
        if runtime == self.tree_runtime() {
            self.reveal_in_tree(&path, cx);
        }
        self.next_file_id = self.next_file_id.wrapping_add(1);
        let id = self.next_file_id;
        let preview_kind = PreviewKind::for_path(&path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(editor_language_for_path(&path))
                .line_number(true)
                .searchable(true)
                // 代码换行会破坏缩进对齐，超长行横向滚动。
                .soft_wrap(false)
        });
        let style = self.path_style(&runtime, cx);
        let client = target(&runtime, cx);
        self.open_file = Some(OpenFile {
            id,
            runtime,
            path: path.clone(),
            editor: editor.clone(),
            saved_content: Rc::new(String::new()),
            saved_version: None,
            image: None,
            save_error: None,
            // 读完确认是文本才翻真，读完之前按保存不会把空内容写回去。
            readable: false,
            conflict_pending: false,
            reload_inflight: false,
            reload_again: false,
            preview: preview_kind.is_some(),
            preview_source: SharedString::default(),
            html_preview: (preview_kind == Some(PreviewKind::Html))
                .then(|| cx.new(|cx| HtmlPreview::new(window, cx))),
        });
        self.preview_task = None;
        self.pending_switch_after_save = None;
        // 编辑器自己会重画；预览停手后再更新。
        self._editor_sub = Some(cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.schedule_preview_refresh(cx);
            }
        }));
        self.persist_project();
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    let client = client?;
                    call_on::<FileOpenCall>(&client, FilePathParams { path }, RUNTIME_CALL_TIMEOUT)
                        .map_err(error_text)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(of) = this.open_file_with_id(id) else {
                    return;
                };
                let unreadable = match read {
                    Ok(FileOpenResult {
                        content: FileContent::Text { text, version },
                    }) => {
                        editor.update(cx, |state, cx| {
                            state.set_value(text.clone(), window, cx);
                            if let Some(line) = goto_line {
                                let line0 = line.saturating_sub(1) as u32;
                                state.set_cursor_position(Position::new(line0, 0), window, cx);
                            }
                        });
                        of.saved_content = Rc::new(text.clone());
                        of.saved_version = Some(version);
                        of.preview_source = text.into();
                        of.sync_html_preview(cx);
                        of.readable = true;
                        None
                    }
                    Ok(FileOpenResult {
                        content: FileContent::Image { data },
                    }) => match style
                        .extension(&of.path)
                        .and_then(smelt_plugin_api::ui::image::format_for_extension)
                    {
                        Some(format) => {
                            of.image = Some(Arc::new(gpui::Image::from_bytes(format, data.0)));
                            None
                        }
                        None => Some("（无法预览这种图片格式）".to_string()),
                    },
                    Ok(FileOpenResult {
                        content: FileContent::Binary,
                    }) => Some("（无法以文本方式读取：可能是二进制文件）".to_string()),
                    Ok(FileOpenResult {
                        content: FileContent::TooLarge { size, limit },
                    }) => Some(format!(
                        "（文件过大：{:.1} MB，查看上限 {} MB）",
                        size as f64 / (1024.0 * 1024.0),
                        limit / (1024 * 1024)
                    )),
                    Err(error) => Some(format!("（无法读取：{error}）")),
                };
                if let Some(placeholder) = unreadable {
                    editor.update(cx, |state, cx| state.set_value(placeholder, window, cx));
                    of.readable = false;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// ⌘S：把编辑器内容交给运行时写回。带上打开时的版本：磁盘上已经不是那一版（外部改过）
    /// 就不写、回冲突，提示一下；再按一次就当作确认覆盖，不带版本写。成功后如果是「保存并
    /// 切换」，接着打开那个目标；失败或冲突就留在当前文件上。
    pub(crate) fn save_open_file(&mut self, cx: &mut Context<Self>) {
        let Some(of) = self.open_file.as_mut() else {
            return;
        };
        if is_previewable_image(&of.path) {
            return;
        }
        if !of.readable {
            of.save_error = Some("此文件未能正常读取为文本，不支持保存".to_string());
            self.pending_switch_after_save = None;
            cx.notify();
            return;
        }
        let id = of.id;
        let path = of.path.clone();
        let client = target(&of.runtime, cx);
        let content = of.editor.read(cx).value().to_string();
        let base_version = if of.conflict_pending {
            None
        } else {
            of.saved_version.clone()
        };
        let Some(window) = self.host.main_window(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let text = content.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let client = match client {
                        Ok(client) => client,
                        Err(error) => return SaveOutcome::Error(error),
                    };
                    let params = FileSaveParams {
                        path,
                        text,
                        base_version,
                    };
                    match call_on::<FileSaveCall>(&client, params, RUNTIME_CALL_TIMEOUT) {
                        Ok(saved) => SaveOutcome::Saved(saved.version),
                        Err(ControlClientError::Protocol(error))
                            if error.code == ControlErrorCode::Conflict =>
                        {
                            SaveOutcome::Conflict
                        }
                        Err(error) => SaveOutcome::Error(error_text(error)),
                    }
                })
                .await;
            let _ = cx.update_window(window, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let switch_target = this
                        .pending_switch_after_save
                        .take_if(|(from, _)| *from == id)
                        .map(|(_, target)| target);
                    let Some(of) = this.open_file_with_id(id) else {
                        return;
                    };
                    match outcome {
                        SaveOutcome::Saved(version) => {
                            of.saved_content = Rc::new(content);
                            of.saved_version = Some(version);
                            of.save_error = None;
                            of.conflict_pending = false;
                            if let Some(target) = switch_target
                                && this.open_file.as_ref().is_some_and(|of| of.id == id)
                            {
                                this.open_file_now(target, None, window, cx);
                            }
                        }
                        SaveOutcome::Conflict => {
                            of.conflict_pending = true;
                            of.save_error = Some(
                                "文件已被外部修改；再按一次 ⌘S 会强制覆盖磁盘上的改动".to_string(),
                            );
                        }
                        SaveOutcome::Error(error) => {
                            of.save_error = Some(format!("保存失败：{error}"));
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 项目里有文件变了。打开着的文件在这个项目里就重读一次：没改过就换成新内容，有没保存
    /// 的改动就提示，保存时照常走冲突检测。事件分不出是不是这个文件，版本没变时什么也不动。
    /// 停放着的项目不管：换回来时重读一次。
    pub(crate) fn on_open_file_project_changed(
        &mut self,
        project: &ProjectRef,
        cx: &mut Context<Self>,
    ) {
        let style = self.path_style(&project.runtime, cx);
        let Some(of) = self.open_file.as_mut() else {
            return;
        };
        if of.runtime != project.runtime
            || !of.readable
            || !style.starts_with(&of.path, &project.root)
        {
            return;
        }
        if of.reload_inflight {
            of.reload_again = true;
            return;
        }
        self.reload_open_file(cx);
    }

    pub(crate) fn reload_open_file(&mut self, cx: &mut Context<Self>) {
        let Some(window) = self.host.main_window(cx) else {
            return;
        };
        let Some(of) = self.open_file.as_mut() else {
            return;
        };
        let Ok(client) = target(&of.runtime, cx) else {
            return;
        };
        of.reload_inflight = true;
        of.reload_again = false;
        let id = of.id;
        let path = of.path.clone();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    call_on::<FileOpenCall>(&client, FilePathParams { path }, RUNTIME_CALL_TIMEOUT)
                })
                .await;
            let _ = cx.update_window(window, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let Some(of) = this.open_file_with_id(id) else {
                        return;
                    };
                    of.reload_inflight = false;
                    let again = std::mem::take(&mut of.reload_again);
                    if let Ok(FileOpenResult {
                        content: FileContent::Text { text, version },
                    }) = read
                        && of.saved_version.as_deref() != Some(version.as_str())
                    {
                        if of.dirty(cx) {
                            of.save_error =
                                Some("这个文件已在别处被改过；保存时会提示冲突".to_string());
                        } else {
                            of.editor
                                .update(cx, |state, cx| state.set_value(text.clone(), window, cx));
                            of.saved_content = Rc::new(text.clone());
                            of.saved_version = Some(version);
                            of.preview_source = text.into();
                            of.sync_html_preview(cx);
                        }
                        cx.notify();
                    }
                    if again && this.open_file.as_ref().is_some_and(|of| of.id == id) {
                        this.reload_open_file(cx);
                    }
                });
            });
        })
        .detach();
    }

    /// HTML 文件在系统默认浏览器里打开。开的是磁盘上的文件，相对路径的资源能加载；没保存
    /// 的改动不会带过去。浏览器只能开这台电脑上的文件。
    pub(crate) fn open_file_in_browser(&mut self, cx: &mut Context<Self>) {
        let Some(of) = self.open_file.as_mut() else {
            return;
        };
        if of.runtime != RuntimeKey::Local {
            return;
        }
        match url::Url::from_file_path(&of.path) {
            Ok(url) => cx.open_url(url.as_str()),
            Err(()) => {
                of.save_error = Some("这个路径转不成文件地址，打不开浏览器".to_string());
                cx.notify();
            }
        }
    }

    /// 编辑器右键「发送选中内容到会话」：带上文件名和选中的文字，交给当前会话（不替用户
    /// 发出）。
    pub(crate) fn send_open_file_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(of) = &self.open_file else {
            return;
        };
        let selected = of.editor.read(cx).selected_value().to_string();
        if selected.trim().is_empty() {
            return;
        }
        let style = self.path_style(&of.runtime, cx);
        let rel = self
            .project
            .as_ref()
            .filter(|project| project.runtime == of.runtime)
            .and_then(|project| style.strip_prefix(&of.path, &project.root))
            .unwrap_or_else(|| of.path.clone());
        let message = format!("{rel} 里选中的这段：\n```\n{selected}\n```\n");
        self.send_to_session(&message, window, cx);
    }
}
