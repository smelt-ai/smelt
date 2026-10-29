//! 选目录：项目、智能体绑定的目录、要导入的插件都在运行时那台机器上。
//!
//! 本机运行时用系统的选择框（macOS 是访达那个，Windows、Linux 是各自系统的），选出来的
//! 就是运行时能用的路径，常用位置、最近使用、搜索都有。远程运行时在那台机器上逐级浏览：
//! 系统选择框只看得到这台桌面，路径对远程没有意义。列目录走 `workspace.browse_dirs`，
//! 渲染只读 [`DirPickerState`] 里缓存的结果。

use std::collections::BTreeSet;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::scroll::ScrollableElement;
use gpui_component::*;
use smelt_runtime_client::protocol::workspace::{
    BrowseDirsParams, BrowseDirsResult, WorkspaceBrowseDirsCall,
};

use crate::{Workspace, ui_theme};

/// 选好之后拿路径去做什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DirPickerPurpose {
    /// 「打开项目」：选一个目录。
    OpenProject,
    /// 给智能体绑定参考目录：可以多选。
    AgentContextFolders { agent_id: String },
    /// 导入插件：技能是目录，扩展可以是单个文件。
    PluginImport {
        kind: smelt_model::pi_plugin_catalog::PiPluginKind,
    },
}

impl DirPickerPurpose {
    fn title(&self) -> &'static str {
        match self {
            Self::OpenProject => "选择项目目录",
            Self::AgentContextFolders { .. } => "选择要绑定的目录",
            Self::PluginImport { .. } => "选择要导入的插件",
        }
    }

    fn multiple(&self) -> bool {
        matches!(self, Self::AgentContextFolders { .. })
    }

    /// 系统选择框的确认按钮。
    fn confirm_label(&self) -> &'static str {
        match self {
            Self::OpenProject => "打开",
            Self::AgentContextFolders { .. } => "绑定",
            Self::PluginImport { .. } => "导入",
        }
    }

    fn include_files(&self) -> bool {
        matches!(
            self,
            Self::PluginImport {
                kind: smelt_model::pi_plugin_catalog::PiPluginKind::Extension
            }
        )
    }
}

pub(crate) struct DirPickerState {
    /// 在哪个运行时那台机器上选。
    runtime: crate::runtimes::Runtime,
    purpose: DirPickerPurpose,
    /// 当前列出的目录。`None` 表示还没列过（从主目录开始）。
    listing: Option<Result<BrowseDirsResult, String>>,
    loading: bool,
    /// 每次导航加一；晚回来的旧列表不能盖掉新的。
    generation: u64,
    /// 多选时勾上的目录；单选导入扩展时选中的那个文件。
    selected: BTreeSet<String>,
}

impl Workspace {
    pub(crate) fn open_dir_picker(
        &mut self,
        runtime: crate::runtimes::Runtime,
        purpose: DirPickerPurpose,
        cx: &mut Context<Self>,
    ) {
        if runtime.is_local() {
            self.prompt_local_paths(runtime, purpose, cx);
            return;
        }
        self.dir_picker = Some(DirPickerState {
            runtime,
            purpose,
            listing: None,
            loading: false,
            generation: 0,
            selected: BTreeSet::new(),
        });
        self.dir_picker_navigate(None, cx);
    }

    /// 本机运行时：弹系统的选择框。走 GPUI 的异步 `prompt_for_paths`，不能在回调里同步跑
    /// 模态框（见对话输入栏「+」的注释）。
    fn prompt_local_paths(
        &mut self,
        runtime: crate::runtimes::Runtime,
        purpose: DirPickerPurpose,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(system_prompt_options(&purpose));
        cx.spawn(async move |this, cx| {
            let paths = match rx.await {
                Ok(Ok(Some(paths))) => paths,
                // 用户取消。
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    crate::status_item::notify_error(format!("打不开系统选择框：{error}"));
                    return;
                }
            };
            let chosen: Vec<String> = paths
                .iter()
                .filter_map(|path| path.to_str().map(str::to_string))
                .collect();
            if chosen.len() < paths.len() {
                crate::status_item::notify_error("有路径不是 UTF-8，没法用，已跳过".to_string());
            }
            if chosen.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.use_chosen_paths(&runtime, purpose, chosen, cx);
            });
        })
        .detach();
    }

    pub(crate) fn cancel_dir_picker(&mut self, cx: &mut Context<Self>) {
        self.dir_picker = None;
        cx.notify();
    }

    fn dir_picker_navigate(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let Some(picker) = self.dir_picker.as_mut() else {
            return;
        };
        picker.generation += 1;
        picker.loading = true;
        // 换目录就清掉单选的文件；多选的目录跨目录保留。
        if !picker.purpose.multiple() {
            picker.selected.clear();
        }
        let generation = picker.generation;
        let client = picker.runtime.client().clone();
        let params = BrowseDirsParams {
            path,
            include_files: picker.purpose.include_files(),
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move {
                    client
                        .call::<WorkspaceBrowseDirsCall>(
                            &params,
                            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                        )
                        .map_err(crate::runtime_call::error_text)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(picker) = this
                    .dir_picker
                    .as_mut()
                    .filter(|picker| picker.generation == generation)
                else {
                    return;
                };
                picker.loading = false;
                match listed {
                    Ok(listing) => picker.listing = Some(Ok(listing)),
                    // 进不去的目录：留在原地，把原因显示出来。
                    Err(error) => {
                        if !matches!(picker.listing, Some(Ok(_))) {
                            picker.listing = Some(Err(error));
                        } else {
                            crate::status_item::notify_error(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dir_picker_toggle(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(picker) = self.dir_picker.as_mut() else {
            return;
        };
        if !picker.selected.remove(&path) {
            if !picker.purpose.multiple() {
                picker.selected.clear();
            }
            picker.selected.insert(path);
        }
        cx.notify();
    }

    fn dir_picker_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.dir_picker.take() else {
            return;
        };
        let current = match &picker.listing {
            Some(Ok(listing)) => listing.path.clone(),
            _ => {
                self.dir_picker = Some(picker);
                return;
            }
        };
        // 没勾选时就是「选当前目录」。
        let chosen: Vec<String> = if picker.selected.is_empty() {
            vec![current]
        } else {
            picker.selected.into_iter().collect()
        };
        self.use_chosen_paths(&picker.runtime, picker.purpose, chosen, cx);
    }

    /// 选好了：两种选择框都从这里收尾。
    fn use_chosen_paths(
        &mut self,
        runtime: &crate::runtimes::Runtime,
        purpose: DirPickerPurpose,
        chosen: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        match purpose {
            DirPickerPurpose::OpenProject => {
                if let Some(dir) = chosen.into_iter().next() {
                    self.add_project(runtime.key().clone(), dir, cx);
                }
            }
            DirPickerPurpose::AgentContextFolders { agent_id } => {
                self.add_agent_context_folders(agent_id, chosen, cx);
            }
            DirPickerPurpose::PluginImport { kind } => {
                if let Some(source) = chosen.into_iter().next() {
                    self.import_pi_plugin_from(kind, source, cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn render_dir_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(picker) = self.dir_picker.as_ref() else {
            return div().into_any_element();
        };
        let (fg, muted, border) = {
            let theme = cx.theme();
            (theme.foreground, theme.muted_foreground, theme.border)
        };
        let multiple = picker.purpose.multiple();
        let listing = picker.listing.as_ref();
        let current = listing.and_then(|listing| listing.as_ref().ok());

        let toolbar = h_flex()
            .gap_2()
            .items_center()
            .child(
                smelt_ui::Button::new("dir-picker-up")
                    .quiet()
                    .compact()
                    .icon(IconName::ArrowUp)
                    .tooltip("上一级")
                    .disabled(current.is_none_or(|listing| listing.parent.is_none()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let parent = this
                            .dir_picker
                            .as_ref()
                            .and_then(|picker| picker.listing.as_ref())
                            .and_then(|listing| listing.as_ref().ok())
                            .and_then(|listing| listing.parent.clone());
                        if parent.is_some() {
                            this.dir_picker_navigate(parent, cx);
                        }
                    })),
            )
            .child(
                smelt_ui::Button::new("dir-picker-home")
                    .quiet()
                    .compact()
                    .label("主目录")
                    .on_click(cx.listener(|this, _, _, cx| this.dir_picker_navigate(None, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(
                        current
                            .map(|listing| listing.path.clone())
                            .unwrap_or_else(|| "…".to_string()),
                    ),
            );

        let body: AnyElement = match listing {
            None => div()
                .py_6()
                .text_sm()
                .text_color(muted)
                .child("正在读取…")
                .into_any_element(),
            Some(Err(error)) => div()
                .py_6()
                .text_sm()
                .text_color(rgb(ui_theme::red()))
                .child(error.clone())
                .into_any_element(),
            Some(Ok(listing)) if listing.entries.is_empty() => div()
                .py_6()
                .text_sm()
                .text_color(muted)
                .child("这里没有子目录")
                .into_any_element(),
            Some(Ok(listing)) => v_flex()
                .id("dir-picker-list")
                .h(px(320.))
                .overflow_y_scrollbar()
                .border_1()
                .border_color(border)
                .rounded(ui_theme::row_radius())
                .children(listing.entries.iter().enumerate().map(|(ix, entry)| {
                    let selected = picker.selected.contains(&entry.path);
                    let path = entry.path.clone();
                    let is_dir = entry.is_dir;
                    let toggle_path = entry.path.clone();
                    smelt_ui::ListRow::new(("dir-picker-entry", ix))
                        .card()
                        .selected(selected)
                        .flex()
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .child(
                            Icon::new(if is_dir {
                                IconName::Folder
                            } else {
                                IconName::File
                            })
                            .size(px(14.))
                            .text_color(muted),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(fg)
                                .child(entry.name.clone()),
                        )
                        // 多选时目录行右侧单独一个勾选；点名字进入目录。
                        .when(multiple && is_dir, |row| {
                            row.child(
                                smelt_ui::Button::new(("dir-picker-pick", ix))
                                    .quiet()
                                    .icon_size()
                                    .icon(if selected {
                                        IconName::Check
                                    } else {
                                        IconName::Plus
                                    })
                                    .tooltip(if selected { "取消选择" } else { "选择" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        // 整行点击是进入目录；勾选不能冒泡上去，否则选中的同时就进去了。
                                        cx.stop_propagation();
                                        this.dir_picker_toggle(toggle_path.clone(), cx);
                                    })),
                            )
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if is_dir {
                                this.dir_picker_navigate(Some(path.clone()), cx);
                            } else {
                                this.dir_picker_toggle(path.clone(), cx);
                            }
                        }))
                }))
                .into_any_element(),
        };

        let confirm_label: &'static str = match (&picker.purpose, picker.selected.len()) {
            (DirPickerPurpose::AgentContextFolders { .. }, 0) => "绑定当前目录",
            (DirPickerPurpose::AgentContextFolders { .. }, _) => "绑定所选目录",
            (DirPickerPurpose::PluginImport { .. }, 0) => "导入当前目录",
            (DirPickerPurpose::PluginImport { .. }, _) => "导入所选文件",
            (DirPickerPurpose::OpenProject, _) => "打开当前目录",
        };
        let content = v_flex()
            .child(Self::modal_title(
                fg,
                if picker.runtime.is_local() {
                    picker.purpose.title().to_string()
                } else {
                    format!(
                        "{}（{}）",
                        picker.purpose.title(),
                        crate::runtimes::runtime_name(picker.runtime.key(), cx)
                    )
                },
            ))
            .child(toolbar)
            .child(body)
            .when(multiple && !picker.selected.is_empty(), |content| {
                content.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("已选 {} 个目录", picker.selected.len())),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_normal_button(
                        "cancel-dir-picker",
                        "取消",
                        |this, _, _, cx| this.cancel_dir_picker(cx),
                        cx,
                    ))
                    .child(
                        Self::modal_commit_button(
                            "confirm-dir-picker",
                            confirm_label,
                            |this, _, _, cx| this.dir_picker_confirm(cx),
                            cx,
                        )
                        .disabled(current.is_none() || picker.loading),
                    ),
            );
        self.modal_shell(
            520.,
            false,
            content,
            crate::overlay::ModalLayer::DirPicker,
            cx,
        )
    }
}

/// 系统选择框的选项：都能选目录；导入扩展还能选单个文件；绑定目录可以多选。
fn system_prompt_options(purpose: &DirPickerPurpose) -> PathPromptOptions {
    PathPromptOptions {
        files: purpose.include_files(),
        directories: true,
        multiple: purpose.multiple(),
        prompt: Some(purpose.confirm_label().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_model::pi_plugin_catalog::PiPluginKind;

    #[test]
    fn system_prompt_options_follow_the_purpose() {
        let project = system_prompt_options(&DirPickerPurpose::OpenProject);
        assert!(project.directories && !project.files && !project.multiple);

        let folders = system_prompt_options(&DirPickerPurpose::AgentContextFolders {
            agent_id: "writer".to_string(),
        });
        assert!(folders.directories && !folders.files && folders.multiple);

        let skill = system_prompt_options(&DirPickerPurpose::PluginImport {
            kind: PiPluginKind::Skill,
        });
        assert!(skill.directories && !skill.files && !skill.multiple);

        let extension = system_prompt_options(&DirPickerPurpose::PluginImport {
            kind: PiPluginKind::Extension,
        });
        assert!(extension.directories && extension.files && !extension.multiple);
    }
}
