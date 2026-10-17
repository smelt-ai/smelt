//! 运行时目录选择器：在运行时那台机器上逐级浏览目录，选一个或几个路径。
//!
//! 项目、智能体绑定的目录、要导入的插件都在运行时那台机器上，本机的系统选择框选出来
//! 的是桌面上的路径，对远程运行时没有意义。本机运行时也走这里，保持同一条路径（设计
//! 文档 P1）。列目录走 `workspace.browse_dirs`，渲染只读 [`DirPickerState`] 里缓存的
//! 结果。

use std::collections::BTreeSet;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::ButtonVariants as _;
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
        match picker.purpose {
            DirPickerPurpose::OpenProject => {
                if let Some(dir) = chosen.into_iter().next() {
                    self.add_project(picker.runtime.key().clone(), dir, cx);
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
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);
        let multiple = picker.purpose.multiple();
        let listing = picker.listing.as_ref();
        let current = listing.and_then(|listing| listing.as_ref().ok());

        let toolbar = h_flex()
            .gap_2()
            .items_center()
            .child(
                gpui_component::button::Button::new("dir-picker-up")
                    .ghost()
                    .small()
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
                gpui_component::button::Button::new("dir-picker-home")
                    .ghost()
                    .small()
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
                    h_flex()
                        .id(("dir-picker-entry", ix))
                        .px_3()
                        .py_1p5()
                        .gap_2()
                        .items_center()
                        .cursor_pointer()
                        .when(selected, |row| {
                            row.bg(ui_theme::tint(ui_theme::action_fill(), 0x22))
                        })
                        .hover(|row| row.bg(ui_theme::overlay(0x14)))
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
                                gpui_component::button::Button::new(("dir-picker-pick", ix))
                                    .ghost()
                                    .xsmall()
                                    .icon(if selected {
                                        IconName::Check
                                    } else {
                                        IconName::Plus
                                    })
                                    .tooltip(if selected { "取消选择" } else { "选择" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
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
                    .child(Self::modal_button(
                        "cancel-dir-picker",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_dir_picker(cx),
                        cx,
                    ))
                    .child(
                        Self::modal_button(
                            "confirm-dir-picker",
                            confirm_label,
                            tint,
                            hover,
                            accent_text,
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
