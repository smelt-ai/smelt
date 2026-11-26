//! 插件要的路径（[`smelt_plugin_api::PICK_PATHS`]）：路径在运行时那台机器上。
//!
//! 本机运行时用系统的选择框（macOS 是访达那个，Windows、Linux 是各自系统的），选出来的
//! 就是运行时能用的路径，常用位置、最近使用、搜索都有。远程运行时在那台机器上逐级浏览：
//! 系统选择框只看得到这台桌面，路径对远程没有意义。列目录走 `workspace.browse_dirs`，
//! 渲染只读 [`DirPickerState`] 里缓存的结果。

use rust_i18n::t;
use std::collections::BTreeSet;

use gpui::{App, Context, PathPromptOptions, Task};
use smelt_runtime_client::protocol::workspace::{
    BrowseDirsParams, BrowseDirsResult, WorkspaceBrowseDirsCall,
};

use crate::Workspace;

/// 一次选路径：怎么选由插件说，选好从 `done` 交回；取消时它随选择框扔掉。
pub(crate) struct PathPick {
    pub(crate) request: smelt_plugin_api::PathRequest,
    pub(crate) done: smol::channel::Sender<Vec<String>>,
}

pub(crate) struct DirPickerState {
    /// 在哪个运行时那台机器上选。
    runtime: crate::runtimes::Runtime,
    pick: PathPick,
    /// 当前列出的目录。`None` 表示还没列过（从主目录开始）。
    listing: Option<Result<BrowseDirsResult, String>>,
    loading: bool,
    /// 每次导航加一；晚回来的旧列表不能盖掉新的。
    generation: u64,
    /// 每次成功切到新目录加一；通知前端把列表滚回顶部。
    list_rev: u64,
    /// 多选时勾上的目录；单选导入扩展时选中的那个文件。
    selected: BTreeSet<String>,
}

impl Workspace {
    /// 在远程运行时那台机器上逐级浏览。本机运行时用系统选择框（[`pick_local_paths`]），
    /// 不走这里。
    pub(crate) fn open_dir_picker(
        &mut self,
        runtime: crate::runtimes::Runtime,
        pick: PathPick,
        cx: &mut Context<Self>,
    ) {
        self.dir_picker = Some(DirPickerState {
            runtime,
            pick,
            listing: None,
            loading: false,
            generation: 0,
            list_rev: 0,
            selected: BTreeSet::new(),
        });
        self.dir_picker_navigate(None, cx);
    }

    pub(crate) fn cancel_dir_picker(&mut self, cx: &mut Context<Self>) {
        self.dir_picker = None;
        cx.notify();
    }

    pub(crate) fn dir_picker_go_up(&mut self, cx: &mut Context<Self>) {
        let parent = self
            .dir_picker
            .as_ref()
            .and_then(|picker| picker.listing.as_ref())
            .and_then(|listing| listing.as_ref().ok())
            .and_then(|listing| listing.parent.clone());
        if parent.is_some() {
            self.dir_picker_navigate(parent, cx);
        }
    }

    pub(crate) fn dir_picker_navigate(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let Some(picker) = self.dir_picker.as_mut() else {
            return;
        };
        picker.generation += 1;
        picker.loading = true;
        // 换目录就清掉单选的文件；多选的目录跨目录保留。
        if !picker.pick.request.multiple {
            picker.selected.clear();
        }
        let generation = picker.generation;
        let client = picker.runtime.client().clone();
        let params = BrowseDirsParams {
            path,
            include_files: picker.pick.request.files,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let listed = smelt_sys::blocking::run(move || {
                client
                    .call::<WorkspaceBrowseDirsCall>(&params, smelt_plugin_api::CALL_TIMEOUT)
                    .map_err(smelt_plugin_api::call_error_text)
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
                    Ok(listing) => {
                        picker.listing = Some(Ok(listing));
                        // 新目录从顶上看起，不沿用上一层滚到的位置。
                        picker.list_rev += 1;
                    }
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

    pub(crate) fn dir_picker_toggle(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(picker) = self.dir_picker.as_mut() else {
            return;
        };
        if !picker.selected.remove(&path) {
            if !picker.pick.request.multiple {
                picker.selected.clear();
            }
            picker.selected.insert(path);
        }
        cx.notify();
    }

    /// 确认按钮和对话框里的 Enter 都走这里。还在读下一层时列表是上一层的，不能确认。
    pub(crate) fn dir_picker_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.dir_picker.take() else {
            return;
        };
        let current = match &picker.listing {
            Some(Ok(listing)) if !picker.loading => listing.path.clone(),
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
        let _ = picker.pick.done.try_send(chosen);
        cx.notify();
    }

    /// 构造推给 NSH 主壳的远程目录选择器对话框快照。
    pub(crate) fn dir_picker_dialog_state(
        &self,
        cx: &App,
    ) -> Option<smelt_nsh::DirPickerDialogState> {
        let picker = self.dir_picker.as_ref()?;
        let listing = picker.listing.as_ref();
        let current = listing.and_then(|listing| listing.as_ref().ok());
        let error = listing.and_then(|listing| listing.as_ref().err().cloned());
        let entries = current
            .map(|listing| {
                listing
                    .entries
                    .iter()
                    .map(|entry| smelt_nsh::DirPickerEntryState {
                        name: entry.name.clone(),
                        path: entry.path.clone(),
                        is_dir: entry.is_dir,
                        selected: picker.selected.contains(&entry.path),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let request = &picker.pick.request;
        let multiple = request.multiple;
        let selected_summary = (multiple && !picker.selected.is_empty()).then(|| {
            if picker.selected.len() == 1 {
                t!("picker.selected.one").into_owned()
            } else {
                t!("picker.selected.other", count = picker.selected.len()).into_owned()
            }
        });
        let confirm_label = if picker.selected.is_empty() {
            t!("picker.confirm_current", action = request.confirm).into_owned()
        } else {
            t!("picker.confirm_selected", action = request.confirm).into_owned()
        };
        let title = t!(
            "picker.title",
            title = request.title,
            machine = crate::runtimes::runtime_name(picker.runtime.key(), cx)
        )
        .into_owned();
        Some(smelt_nsh::DirPickerDialogState {
            title,
            current_path: current.map(|listing| listing.path.clone()),
            can_go_up: current.is_some_and(|listing| listing.parent.is_some()),
            loading: picker.loading,
            error,
            entries,
            multiple,
            selected_summary,
            confirm_label,
            can_confirm: current.is_some() && !picker.loading,
            cancel_label: t!("action.cancel").into_owned(),
            up_tooltip: t!("picker.up").into_owned(),
            home_label: t!("picker.home").into_owned(),
            loading_label: t!("picker.loading").into_owned(),
            empty_label: t!("picker.empty").into_owned(),
            select_tooltip: t!("picker.select").into_owned(),
            deselect_tooltip: t!("picker.deselect").into_owned(),
            list_rev: picker.list_rev,
        })
    }
}

/// 本机的系统选择框：都能选目录，能不能选文件、多选照插件说的。它不挂在主窗口上，主窗口
/// 关着也能弹。走 GPUI 的异步 `prompt_for_paths`，不能在回调里同步跑模态框。
/// 取消、出错、一条能用的路径都没有时交回 `None`。
pub(crate) fn pick_local_paths(
    request: &smelt_plugin_api::PathRequest,
    cx: &mut App,
) -> Task<Option<Vec<String>>> {
    let rx = cx.prompt_for_paths(system_prompt_options(request));
    cx.spawn(async move |_| {
        let paths = match rx.await {
            Ok(Ok(Some(paths))) => paths,
            // 用户取消。
            Ok(Ok(None)) | Err(_) => return None,
            Ok(Err(error)) => {
                crate::status_item::notify_error(
                    t!("picker.system_failed", error = error).into_owned(),
                );
                return None;
            }
        };
        let chosen: Vec<String> = paths
            .iter()
            .filter_map(|path| path.to_str().map(str::to_string))
            .collect();
        if chosen.len() < paths.len() {
            crate::status_item::notify_error(t!("picker.non_utf8").into_owned());
        }
        (!chosen.is_empty()).then_some(chosen)
    })
}

fn system_prompt_options(request: &smelt_plugin_api::PathRequest) -> PathPromptOptions {
    PathPromptOptions {
        files: request.files,
        directories: true,
        multiple: request.multiple,
        prompt: Some(request.confirm.clone().into()),
    }
}

#[cfg(test)]
mod tests {
    // 不能 `use super::*`：上面 glob 了 `gpui::*`，`gpui::test` 会顶掉 `#[test]`，展开时无限递归。
    use super::system_prompt_options;

    #[test]
    fn system_prompt_options_follow_the_request() {
        let plugin = |files, multiple| smelt_plugin_api::PathRequest {
            machine: smelt_plugin_api::RuntimeKey::Local,
            title: "选择".to_string(),
            confirm: "导入".to_string(),
            files,
            multiple,
        };
        let dirs = system_prompt_options(&plugin(false, false));
        assert!(dirs.directories && !dirs.files && !dirs.multiple);
        assert_eq!(dirs.prompt.as_deref(), Some("导入"));

        let files = system_prompt_options(&plugin(true, true));
        assert!(files.directories && files.files && files.multiple);
    }
}
