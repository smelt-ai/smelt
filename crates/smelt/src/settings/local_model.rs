//! 设置「维护 → 本地模型」：由用户点击安装 / 卸载本地小模型，并选哪一个用来生成标题。
//! 目前只提供一个模型（Qwen3.5-0.8B）；清单只有一个时不显示选择按钮。
//!
//! 每个模型（[`TitleModel::ALL`]）各有独立的安装状态：多个模型可以同时下载，取消、卸载互不
//! 影响。所有文件与网络 IO 都在后台线程：打开设置窗口时查一次各模型是否已安装、向本机
//! 运行时读一次已保存的标题模型；点按钮时下载、删除或保存选择。结果写进全局
//! [`LocalModelState`]，设置窗口观察它重绘；渲染只读这份内存状态。
//!
//! 异步结果可能晚于用户的下一次操作回来，所以每个模型和「选择」各带一个代号：发起操作时
//! 加一，结果回来时代号对不上就丢弃，不让旧结果覆盖新状态。
//!
//! 选择不会触发下载：选中未安装的模型也会保存，只是在装好之前不会生成标题。

use std::path::PathBuf;

use gpui::*;
use gpui_component::progress::Progress;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use smelt_local_model::{CancelToken, DownloadProgress, TitleModel, TitleModelSpec};
use smelt_model::appearance_settings::{SettingsPatch, SettingsResult};
use smelt_model::control_api::{SettingsGet, SettingsUpdate};
use smelt_runtime_client::protocol::control::EmptyParams;

use super::Button;
use crate::runtime_call::{RUNTIME_CALL_TIMEOUT, call_on, error_text};

use smelt_model::local_title_settings::MODEL as TITLE_MODEL_KEY;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum LocalModelStatus {
    /// 还没查过（设置窗口没打开过）。
    #[default]
    Unknown,
    Checking,
    NotInstalled,
    Installed,
    Downloading {
        downloaded: u64,
        total: u64,
    },
    Removing,
}

/// 一个模型的安装状态。
#[derive(Clone, Default)]
struct ModelEntry {
    status: LocalModelStatus,
    /// 每次发起检查 / 下载 / 卸载加一；异步结果代号对不上就丢弃。
    generation: u64,
    /// 下载中才有；取消会立即打断正在等待的网络读。
    cancel: Option<CancelToken>,
    /// 点了「卸载…」，等「确认卸载」。
    confirming_remove: bool,
    /// 上一次操作的失败或结果提示。
    message: Option<SharedString>,
    /// 模型文件路径的展示文本，打开设置窗口时算好；渲染不去解析 `smelt_home`。
    model_path: Option<SharedString>,
}

/// 标题模型的选择。
#[derive(Clone, Default)]
struct Selection {
    /// 已保存（或正在保存）的选择；还没读到时是 `None`。
    model: Option<TitleModel>,
    /// 正在向运行时读或写。
    pending: bool,
    /// 每次读 / 写加一；结果代号对不上就丢弃。
    generation: u64,
    /// 读写失败的原因。
    error: Option<SharedString>,
}

#[derive(Clone, Default)]
pub(crate) struct LocalModelState {
    /// 按 [`TitleModel::ALL`] 的顺序。
    models: [ModelEntry; TitleModel::ALL.len()],
    selection: Selection,
}

impl Global for LocalModelState {}

fn index_of(model: TitleModel) -> usize {
    TitleModel::ALL
        .iter()
        .position(|m| *m == model)
        .expect("TitleModel::ALL 包含所有模型")
}

fn models_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("models"))
}

fn state(cx: &App) -> LocalModelState {
    cx.try_global::<LocalModelState>()
        .cloned()
        .unwrap_or_default()
}

fn update(cx: &mut App, f: impl FnOnce(&mut LocalModelState)) {
    let mut next = state(cx);
    f(&mut next);
    cx.set_global(next);
}

fn update_entry(cx: &mut App, model: TitleModel, f: impl FnOnce(&mut ModelEntry)) {
    update(cx, |s| f(&mut s.models[index_of(model)]));
}

/// 只在代号仍是发起时那一个时才改。
fn update_entry_if(
    cx: &mut App,
    model: TitleModel,
    generation: u64,
    f: impl FnOnce(&mut ModelEntry),
) {
    update_entry(cx, model, |e| {
        if e.generation == generation {
            f(e)
        }
    });
}

/// 开始一次新操作：代号加一并返回新代号。
fn begin(cx: &mut App, model: TitleModel, f: impl FnOnce(&mut ModelEntry)) -> u64 {
    let mut generation = 0;
    update_entry(cx, model, |e| {
        e.generation += 1;
        generation = e.generation;
        f(e);
    });
    generation
}

fn entry(cx: &App, model: TitleModel) -> ModelEntry {
    state(cx).models[index_of(model)].clone()
}

fn busy(status: &LocalModelStatus) -> bool {
    matches!(
        status,
        LocalModelStatus::Checking
            | LocalModelStatus::Downloading { .. }
            | LocalModelStatus::Removing
    )
}

/// 打开设置窗口时调：后台查各模型是否已安装，并读一次已保存的标题模型。
/// 下载、卸载进行中的模型不打扰。
pub(crate) fn refresh_local_model(cx: &mut App) {
    let dir = models_dir();
    for model in TitleModel::ALL {
        refresh_model(model, dir.clone(), cx);
    }
    load_selection(cx);
}

fn refresh_model(model: TitleModel, dir: Option<PathBuf>, cx: &mut App) {
    let model_path = dir.as_ref().map(|dir| {
        SharedString::from(
            smelt_local_model::model_path(dir, model.spec())
                .display()
                .to_string(),
        )
    });
    update_entry(cx, model, |e| e.model_path = model_path);
    if busy(&entry(cx, model).status) {
        return;
    }
    let Some(dir) = dir else {
        begin(cx, model, |e| {
            e.status = LocalModelStatus::NotInstalled;
            e.message = Some("找不到 ~/.smelt 目录".into());
        });
        return;
    };
    let generation = begin(cx, model, |e| e.status = LocalModelStatus::Checking);
    let task = cx
        .background_executor()
        .spawn(async move { smelt_local_model::is_installed(&dir, model.spec()) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            update_entry_if(cx, model, generation, |e| match result {
                Ok(true) => e.status = LocalModelStatus::Installed,
                Ok(false) => e.status = LocalModelStatus::NotInstalled,
                Err(err) => {
                    e.status = LocalModelStatus::NotInstalled;
                    e.message = Some(format!("检查安装状态失败：{err}").into());
                }
            })
        });
    })
    .detach();
}

enum DownloadEvent {
    Progress(DownloadProgress),
    Done(smelt_local_model::Result<PathBuf>),
}

fn install(model: TitleModel, cx: &mut App) {
    if busy(&entry(cx, model).status) {
        return;
    }
    let Some(dir) = models_dir() else {
        update_entry(cx, model, |e| e.message = Some("找不到 ~/.smelt 目录".into()));
        return;
    };
    let spec = model.spec();
    let cancel = CancelToken::new();
    let generation = begin(cx, model, |e| {
        e.status = LocalModelStatus::Downloading {
            downloaded: 0,
            total: spec.size,
        };
        e.cancel = Some(cancel.clone());
        e.confirming_remove = false;
        e.message = None;
    });
    let (tx, rx) = smol::channel::unbounded::<DownloadEvent>();
    // 下载是几百 MB 的网络与磁盘 IO，单开线程跑自己的 tokio 运行时，不占 GPUI 后台线程池。
    let spawned = std::thread::Builder::new()
        .name(format!("local-model-download-{}", model.id()))
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(smelt_local_model::Error::from)
                .and_then(|rt| {
                    // 只在百分比变化时报进度，避免每个分块都让设置窗口重绘。
                    let mut last_percent = u64::MAX;
                    let progress_tx = tx.clone();
                    rt.block_on(smelt_local_model::download_model(
                        &dir,
                        spec,
                        &mut |p: DownloadProgress| {
                            let percent = p.downloaded.saturating_mul(100) / p.total.max(1);
                            if percent != last_percent {
                                last_percent = percent;
                                let _ = progress_tx.try_send(DownloadEvent::Progress(p));
                            }
                        },
                        &cancel,
                    ))
                });
            let _ = tx.try_send(DownloadEvent::Done(result));
        });
    if let Err(err) = spawned {
        update_entry_if(cx, model, generation, |e| {
            e.status = LocalModelStatus::NotInstalled;
            e.cancel = None;
            e.message = Some(format!("无法启动下载：{err}").into());
        });
        return;
    }
    cx.spawn(async move |cx| {
        let mut finished = false;
        while let Ok(event) = rx.recv().await {
            match event {
                DownloadEvent::Progress(p) => cx.update(|cx| {
                    update_entry_if(cx, model, generation, |e| {
                        e.status = LocalModelStatus::Downloading {
                            downloaded: p.downloaded,
                            total: p.total,
                        }
                    })
                }),
                DownloadEvent::Done(result) => {
                    finished = true;
                    cx.update(|cx| {
                        update_entry_if(cx, model, generation, |e| {
                            e.cancel = None;
                            match result {
                                Ok(_) => {
                                    e.status = LocalModelStatus::Installed;
                                    e.message = Some("安装完成".into());
                                }
                                Err(smelt_local_model::Error::Cancelled) => {
                                    e.status = LocalModelStatus::NotInstalled;
                                    e.message = Some("已取消下载".into());
                                }
                                Err(err) => {
                                    smelt_sys::app_log::error(
                                        "local-model",
                                        &format!("下载本地模型 {} 失败：{err}", model.id()),
                                    );
                                    e.status = LocalModelStatus::NotInstalled;
                                    e.message = Some(format!("安装失败：{err}").into());
                                }
                            }
                        })
                    });
                    break;
                }
            }
        }
        if !finished {
            // 下载线程没发结果就退出了（panic）。
            cx.update(|cx| {
                update_entry_if(cx, model, generation, |e| {
                    e.cancel = None;
                    e.status = LocalModelStatus::NotInstalled;
                    e.message = Some("下载意外中断".into());
                })
            });
        }
    })
    .detach();
}

fn cancel_download(model: TitleModel, cx: &mut App) {
    if let Some(cancel) = &entry(cx, model).cancel {
        cancel.cancel();
    }
}

fn set_confirming_remove(model: TitleModel, confirming: bool, cx: &mut App) {
    update_entry(cx, model, |e| {
        // 只有已安装时确认才有意义；状态已变（如被刷新）就不挂确认。
        e.confirming_remove = confirming && e.status == LocalModelStatus::Installed;
    });
}

fn uninstall(model: TitleModel, cx: &mut App) {
    let current = entry(cx, model);
    // 必须先点过「卸载…」且仍是已安装，防止按钮残留时误删。
    if current.status != LocalModelStatus::Installed || !current.confirming_remove {
        return;
    }
    let Some(dir) = models_dir() else {
        return;
    };
    let generation = begin(cx, model, |e| {
        e.status = LocalModelStatus::Removing;
        e.confirming_remove = false;
        e.message = None;
    });
    let task = cx
        .background_executor()
        .spawn(async move { smelt_local_model::remove_model(&dir, model.spec()) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            update_entry_if(cx, model, generation, |e| match result {
                Ok(_) => {
                    e.status = LocalModelStatus::NotInstalled;
                    e.message = Some("已卸载".into());
                }
                Err(err) => {
                    e.status = LocalModelStatus::Installed;
                    e.message = Some(format!("卸载失败：{err}").into());
                }
            })
        });
    })
    .detach();
}

/// 未知的模型不能冒充默认值：守护也不会用它生成标题。
fn selection_from(result: &SettingsResult) -> (Option<TitleModel>, Option<String>) {
    match result.settings.get(TITLE_MODEL_KEY) {
        Some(Value::String(id)) => match TitleModel::from_id(id) {
            Some(model) => (Some(model), None),
            None => (None, Some(format!("不认识已保存的标题模型「{id}」，请重新选择"))),
        },
        None => (None, Some("后台服务未返回标题模型设置，请升级后台服务".into())),
        Some(other) => (None, Some(format!("已保存的标题模型格式不对：{other}"))),
    }
}

fn begin_selection(cx: &mut App, f: impl FnOnce(&mut Selection)) -> u64 {
    let mut generation = 0;
    update(cx, |s| {
        s.selection.generation += 1;
        generation = s.selection.generation;
        s.selection.pending = true;
        s.selection.error = None;
        f(&mut s.selection);
    });
    generation
}

/// 后台向本机运行时读已保存的标题模型。
fn load_selection(cx: &mut App) {
    if state(cx).selection.pending {
        return;
    }
    let client = crate::runtime_call::target(&crate::RuntimeKey::Local, cx);
    let generation = begin_selection(cx, |_| {});
    let task = cx.background_executor().spawn(async move {
        call_on::<SettingsGet>(&client?, EmptyParams::default(), RUNTIME_CALL_TIMEOUT)
            .map_err(error_text)
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| apply_selection_result(cx, generation, false, result));
    })
    .detach();
}

/// 选中某个模型：立即显示并保存到本机运行时。不触发下载。
fn select(model: TitleModel, cx: &mut App) {
    let current = state(cx).selection;
    if current.pending || current.model == Some(model) {
        return;
    }
    let client = crate::runtime_call::target(&crate::RuntimeKey::Local, cx);
    let generation = begin_selection(cx, |s| s.model = Some(model));
    let patch = SettingsPatch {
        settings: [(TITLE_MODEL_KEY.to_string(), Value::from(model.id()))]
            .into_iter()
            .collect(),
    };
    let task = cx.background_executor().spawn(async move {
        call_on::<SettingsUpdate>(&client?, patch, RUNTIME_CALL_TIMEOUT).map_err(error_text)
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| apply_selection_result(cx, generation, true, result));
    })
    .detach();
}

/// 读写结果回来：代号对不上就丢弃。写失败不猜测是否已落库。
fn apply_selection_result(
    cx: &mut App,
    generation: u64,
    saving: bool,
    result: Result<SettingsResult, String>,
) {
    update(cx, |s| {
        let sel = &mut s.selection;
        if sel.generation != generation {
            return;
        }
        sel.pending = false;
        match result {
            Ok(result) => {
                let (model, note) = selection_from(&result);
                sel.model = model;
                sel.error = note.map(Into::into);
            }
            Err(err) => {
                sel.model = None;
                sel.error = Some(if saving {
                    format!("保存结果未确认：{err}。请重新打开设置查看实际选择。")
                } else {
                    format!("读取标题模型失败：{err}")
                }.into());
            }
        }
    });
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

/// 一个模型那一行的说明文字。
fn model_summary(model: TitleModel, e: &ModelEntry) -> String {
    let spec = model.spec();
    let summary = match &e.status {
        LocalModelStatus::Unknown | LocalModelStatus::Checking => "正在检查…".to_string(),
        LocalModelStatus::NotInstalled => format!(
            "未安装。约 {:.0} MB，下载自 Hugging Face（{}）。",
            mb(spec.size),
            spec.repo
        ),
        LocalModelStatus::Installed => format!(
            "已安装：{}",
            e.model_path
                .clone()
                .unwrap_or_else(|| "~/.smelt/models".into())
        ),
        LocalModelStatus::Downloading { downloaded, total } => format!(
            "正在下载… {:.1} / {:.1} MB",
            mb(*downloaded),
            mb(*total)
        ),
        LocalModelStatus::Removing => "正在卸载…".to_string(),
    };
    match &e.message {
        Some(message) => format!("{summary}\n{message}"),
        None => summary,
    }
}

/// 「标题模型」一行的说明：选中模型没装好时明确告诉用户不会生成标题。
fn selection_summary(selected: Option<TitleModel>, status: Option<&LocalModelStatus>) -> String {
    let Some(model) = selected else {
        return "尚未确认标题模型。".to_string();
    };
    let label = model.label();
    match status {
        Some(LocalModelStatus::Installed) => format!("用 {label} 在本机生成对话标题。"),
        Some(LocalModelStatus::Downloading { .. }) => {
            format!("已选 {label}，正在下载；下载完成前不会生成标题。")
        }
        Some(LocalModelStatus::Unknown | LocalModelStatus::Checking) | None => {
            format!("已选 {label}，正在检查是否已安装。")
        }
        Some(LocalModelStatus::NotInstalled | LocalModelStatus::Removing) => {
            format!("已选 {label}，但它未安装，不会生成标题。请在下方安装。")
        }
    }
}

/// 只有一个可选模型且已确认选中它时，不显示多余的选择按钮；已保存的值认不出时仍要
/// 给出按钮，让用户能改回可用的模型。
fn shows_model_choice(selected: Option<TitleModel>) -> bool {
    TitleModel::ALL.len() > 1 || selected.is_none()
}

fn model_actions(model: TitleModel, e: &ModelEntry) -> Div {
    let id = model.id();
    let mut row = h_flex().gap_1().items_center();
    match &e.status {
        LocalModelStatus::Unknown | LocalModelStatus::Checking | LocalModelStatus::Removing => {}
        LocalModelStatus::NotInstalled => {
            row = row.child(
                Button::new(SharedString::from(format!("local-model-install-{id}")))
                    .commit()
                    .compact()
                    .label("安装")
                    .on_click(move |_, _window, cx: &mut App| install(model, cx)),
            );
        }
        LocalModelStatus::Downloading { downloaded, total } => {
            let value = *downloaded as f32 / (*total).max(1) as f32 * 100.0;
            row = row
                .child(
                    v_flex().w(px(160.)).child(
                        Progress::new(SharedString::from(format!("local-model-progress-{id}")))
                            .value(value),
                    ),
                )
                .child(
                    Button::new(SharedString::from(format!("local-model-cancel-{id}")))
                        .quiet()
                        .compact()
                        .label("取消")
                        .on_click(move |_, _window, cx: &mut App| cancel_download(model, cx)),
                );
        }
        LocalModelStatus::Installed if e.confirming_remove => {
            row = row
                .child(
                    Button::new(SharedString::from(format!("local-model-remove-cancel-{id}")))
                        .quiet()
                        .compact()
                        .label("取消")
                        .on_click(move |_, _window, cx: &mut App| {
                            set_confirming_remove(model, false, cx)
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("local-model-remove-confirm-{id}")))
                        .danger()
                        .compact()
                        .label("确认卸载")
                        .on_click(move |_, _window, cx: &mut App| uninstall(model, cx)),
                );
        }
        LocalModelStatus::Installed => {
            row = row.child(
                Button::new(SharedString::from(format!("local-model-remove-{id}")))
                    .danger_outline()
                    .compact()
                    .label("卸载…")
                    .on_click(move |_, _window, cx: &mut App| {
                        set_confirming_remove(model, true, cx)
                    }),
            );
        }
    }
    row
}

/// 「本地模型」设置组。只读全局状态，不做 IO。`workspace` 用来跳到资源页看它的占用。
pub(crate) fn local_model_group(workspace: Entity<crate::Workspace>, cx: &App) -> SettingGroup {
    let current = state(cx);
    let selected = current.selection.model;
    let mut selection_text = selection_summary(
        selected,
        selected.map(|m| &current.models[index_of(m)].status),
    );
    if let Some(error) = &current.selection.error {
        selection_text = format!("{selection_text}\n{error}");
    }

    let mut group = SettingGroup::new().item(
        SettingItem::new(
            "标题模型",
            SettingField::render(move |_, _, cx: &mut App| {
                let sel = state(cx).selection;
                let mut row = h_flex().gap_1().items_center();
                if !shows_model_choice(sel.model) {
                    return row;
                }
                for model in TitleModel::ALL {
                    row = row.child(
                        Button::new(SharedString::from(format!(
                            "local-title-model-{}",
                            model.id()
                        )))
                        .outline()
                        .compact()
                        .label(model.label())
                        .selected(sel.model == Some(model))
                        // 同时只允许一笔读写，避免较早的保存晚到并覆盖新选择。
                        .disabled(sel.pending)
                        .on_click(move |_, _window, cx: &mut App| select(model, cx)),
                    );
                }
                row
            }),
        )
        .description(selection_text),
    );

    for model in TitleModel::ALL {
        let summary = model_summary(model, &current.models[index_of(model)]);
        group = group.item(
            SettingItem::new(
                model.label(),
                SettingField::render(move |_, _, cx: &mut App| {
                    model_actions(model, &entry(cx, model))
                }),
            )
            .description(summary),
        );
    }

    group.item(
        SettingItem::new(
            "资源占用",
            SettingField::render(move |_, _, _cx: &mut App| {
                let workspace = workspace.clone();
                Button::new("local-model-open-resources")
                    .quiet()
                    .compact()
                    .label("查看资源占用")
                    .on_click(move |_, _window, cx: &mut App| {
                        workspace.update(cx, |ws, cx| {
                            ws.open_settings_section(super::SettingsSection::Resources, cx)
                        });
                    })
            }),
        )
        .description("模型运行时的 CPU 和内存，在「资源占用」页单独一行。"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_with(value: Option<Value>) -> SettingsResult {
        SettingsResult {
            settings: value
                .map(|v| (TITLE_MODEL_KEY.to_string(), v))
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn selection_without_setting_is_reported_as_unavailable() {
        let (model, note) = selection_from(&result_with(None));
        assert_eq!(model, None);
        assert!(note.is_some());
    }

    #[test]
    fn selection_reads_each_saved_id() {
        for model in TitleModel::ALL {
            assert_eq!(
                selection_from(&result_with(Some(Value::from(model.id())))),
                (Some(model), None)
            );
        }
    }

    #[test]
    fn unknown_selection_has_no_selected_model_and_shows_note() {
        let (model, note) = selection_from(&result_with(Some(Value::from("nope"))));
        assert_eq!(model, None);
        assert!(note.is_some());
    }

    #[test]
    fn uninstalled_selection_says_no_titles() {
        let model = TitleModel::default();
        let text = selection_summary(Some(model), Some(&LocalModelStatus::NotInstalled));
        assert!(text.contains("未安装"));
        assert!(text.contains("不会生成标题"));
        let text = selection_summary(Some(model), Some(&LocalModelStatus::Installed));
        assert!(!text.contains("不会生成标题"));
    }

    #[test]
    fn single_model_hides_choice_unless_selection_is_unknown() {
        assert_eq!(TitleModel::ALL, [TitleModel::Qwen3_5_0_8B]);
        assert!(!shows_model_choice(Some(TitleModel::Qwen3_5_0_8B)));
        assert!(shows_model_choice(None));
    }

    #[test]
    fn retired_qwen3_0_6b_selection_is_not_recognised() {
        let (model, note) = selection_from(&result_with(Some(Value::from("qwen3-0.6b"))));
        assert_eq!(model, None);
        assert!(note.is_some());
    }

    #[test]
    fn index_matches_all_order() {
        for (i, model) in TitleModel::ALL.into_iter().enumerate() {
            assert_eq!(index_of(model), i);
        }
    }
}
