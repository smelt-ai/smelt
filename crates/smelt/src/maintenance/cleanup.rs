//! 设置「系统 → 清理残留」：列出 `~/.smelt` 下没登记的条目（[`super::residue`]），确认后删掉；
//! 渲染与确认框由 NSH 主壳承担。

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{Context, SharedString};
use rust_i18n::t;
use serde_json::{Value, json};
use smelt_plugin_api::{SettingsEntry, SettingsGroup, ViewSpec, text};
use smelt_ui::IconName;

use crate::Workspace;

use super::residue::{self, Residue};

pub(crate) fn spec() -> ViewSpec {
    let keywords = [
        "清理", "缓存", "残留", "磁盘", "空间", "~/.smelt", "clean", "cache", "disk", "storage",
    ];
    ViewSpec {
        name: "cleanup".into(),
        title: text!("maintenance.cleanup.title"),
        icon: IconName::Trash.path(),
        draws_top_bar: false,
        instances: false,
        settings: Some(SettingsEntry {
            group: SettingsGroup::System,
            keywords: keywords.into_iter().map(SharedString::from).collect(),
        }),
        fills: Vec::new(),
        build: crate::host_views::nsh_managed_view_builder(),
    }
}

#[derive(Default)]
enum Scan {
    #[default]
    Running,
    Done(Vec<Residue>),
    Failed(String),
}

#[derive(Default)]
pub(crate) struct CleanupState {
    scan: Scan,
    cleaning: bool,
    failures: HashMap<PathBuf, String>,
}

impl CleanupState {
    pub(crate) fn snapshot(&self) -> Value {
        let items: &[Residue] = match &self.scan {
            Scan::Done(items) => items,
            _ => &[],
        };
        let total_bytes: u64 = items.iter().map(|item| item.bytes).sum();
        let summary = match &self.scan {
            Scan::Running => t!("maintenance.cleanup.scanning").into_owned(),
            Scan::Failed(error) => {
                t!("maintenance.cleanup.scan_failed", error = error).into_owned()
            }
            Scan::Done(items) if items.is_empty() => t!("maintenance.cleanup.empty").into_owned(),
            Scan::Done(items) => t!(
                "maintenance.cleanup.summary",
                count = items.len(),
                size = format_bytes(total_bytes)
            )
            .into_owned(),
        };
        let rows: Vec<Value> = items
            .iter()
            .map(|item| {
                let error = self.failures.get(&item.path).map(|error| {
                    t!("maintenance.cleanup.remove_failed", error = error).into_owned()
                });
                json!({
                    "path": item.path.display().to_string(),
                    "label": item.label,
                    "size": format_bytes(item.bytes),
                    "error": error,
                })
            })
            .collect();
        json!({
            "kind": "cleanup",
            "row_title": t!("maintenance.cleanup.row").into_owned(),
            "summary": summary,
            "action_label": t!("maintenance.cleanup.action").into_owned(),
            "cleaning": self.cleaning,
            "can_clean": !self.cleaning && !items.is_empty(),
            "confirm": {
                "title": t!("maintenance.cleanup.confirm.title", count = items.len()).into_owned(),
                "description": t!(
                    "maintenance.cleanup.confirm.description",
                    size = format_bytes(total_bytes)
                )
                .into_owned(),
                "action": t!("maintenance.cleanup.confirm.action").into_owned(),
                "cancel": t!("action.cancel").into_owned(),
            },
            "items": rows,
        })
    }

    pub(crate) fn rescan(
        &self,
        mut accessor: impl FnMut(&mut Workspace) -> &mut Self + 'static,
        cx: &mut Context<Workspace>,
    ) {
        if self.cleaning {
            return;
        }
        let scan = cx.background_executor().spawn(async { residue::scan() });
        cx.spawn(async move |this, cx| {
            let scan = scan.await;
            let _ = this.update(cx, |workspace, cx| {
                accessor(workspace).scan = match scan {
                    Ok(items) => Scan::Done(items),
                    Err(error) => Scan::Failed(error.to_string()),
                };
                workspace.sync_host_settings(cx);
            });
        })
        .detach();
    }

    pub(crate) fn clean(
        &mut self,
        mut accessor: impl FnMut(&mut Workspace) -> &mut Self + 'static,
        cx: &mut Context<Workspace>,
    ) {
        let Scan::Done(items) = &self.scan else {
            return;
        };
        if items.is_empty() || self.cleaning {
            return;
        }
        let paths: Vec<PathBuf> = items.iter().map(|item| item.path.clone()).collect();
        self.cleaning = true;
        self.failures.clear();
        let work = cx.background_executor().spawn(async move {
            let failures = residue::remove(&paths);
            (failures, residue::scan())
        });
        cx.spawn(async move |this, cx| {
            let (failures, scan) = work.await;
            let _ = this.update(cx, |workspace, cx| {
                let state = accessor(workspace);
                for (path, error) in &failures {
                    eprintln!("[maintenance] 删 {} 失败：{error}", path.display());
                }
                state.failures = failures.into_iter().collect();
                state.scan = match scan {
                    Ok(items) => Scan::Done(items),
                    Err(error) => Scan::Failed(error.to_string()),
                };
                state.cleaning = false;
                workspace.sync_host_settings(cx);
            });
        })
        .detach();
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.;
    const MB: f64 = KB * 1024.;
    const GB: f64 = MB * 1024.;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn sizes_read_in_the_nearest_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
