//! 设置「系统 → 清理残留」：列出 `~/.smelt` 下没登记的条目（[`super::residue`]），确认后删掉。

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, div, rgb,
};
use rust_i18n::t;
use smelt_plugin_api::{
    self as api, PluginView, SettingsEntry, SettingsGroup, ViewHandle, ViewSpec, text,
};
use smelt_ui::kit::{TYPE, TypeStyled as _};
use smelt_ui::{Button, IconName, SettingsPage, SettingsRow, SettingsSection, ui_theme};

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
        build: Rc::new(|_, cx| Rc::new(cx.new(|_| CleanupPage::default())) as Rc<dyn ViewHandle>),
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
struct CleanupPage {
    scan: Scan,
    cleaning: bool,
    failures: HashMap<PathBuf, String>,
}

impl CleanupPage {
    fn rescan(cx: &mut Context<Self>) {
        let scan = cx.background_executor().spawn(async { residue::scan() });
        cx.spawn(async move |this, cx| {
            let scan = scan.await;
            let _ = this.update(cx, |page, cx| {
                page.scan = match scan {
                    Ok(items) => Scan::Done(items),
                    Err(error) => Scan::Failed(error.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Scan::Done(items) = &self.scan else {
            return;
        };
        if items.is_empty() || self.cleaning {
            return;
        }
        let paths: Vec<PathBuf> = items.iter().map(|item| item.path.clone()).collect();
        let bytes = items.iter().map(|item| item.bytes).sum();
        let page = cx.entity();
        smelt_ui::dialog::Confirm::new(
            t!("maintenance.cleanup.confirm.title", count = paths.len()),
            t!("maintenance.cleanup.confirm.action"),
            move |_, cx| {
                let paths = paths.clone();
                page.update(cx, |page, cx| page.clean(paths, cx));
                true
            },
        )
        .danger()
        .description(t!(
            "maintenance.cleanup.confirm.description",
            size = format_bytes(bytes)
        ))
        .open(window, cx);
    }

    fn clean(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.cleaning {
            return;
        }
        self.cleaning = true;
        self.failures.clear();
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let failures = residue::remove(&paths);
            (failures, residue::scan())
        });
        cx.spawn(async move |this, cx| {
            let (failures, scan) = work.await;
            let _ = this.update(cx, |page, cx| {
                for (path, error) in &failures {
                    eprintln!("[maintenance] 删 {} 失败：{error}", path.display());
                }
                page.failures = failures.into_iter().collect();
                page.scan = match scan {
                    Ok(items) => Scan::Done(items),
                    Err(error) => Scan::Failed(error.to_string()),
                };
                page.cleaning = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl PluginView for CleanupPage {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        if !self.cleaning {
            Self::rescan(cx);
        }
    }
}

impl Render for CleanupPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items: &[Residue] = match &self.scan {
            Scan::Done(items) => items,
            _ => &[],
        };
        let summary: SharedString = match &self.scan {
            Scan::Running => t!("maintenance.cleanup.scanning").into(),
            Scan::Failed(error) => t!("maintenance.cleanup.scan_failed", error = error).into(),
            Scan::Done(items) if items.is_empty() => t!("maintenance.cleanup.empty").into(),
            Scan::Done(items) => t!(
                "maintenance.cleanup.summary",
                count = items.len(),
                size = format_bytes(items.iter().map(|item| item.bytes).sum())
            )
            .into(),
        };
        let page = cx.entity();
        let header = SettingsRow::new(t!("maintenance.cleanup.row"))
            .description(summary)
            .child(
                Button::new("clean-residue")
                    .small()
                    .label(t!("maintenance.cleanup.action"))
                    .loading(self.cleaning)
                    .disabled(self.cleaning || items.is_empty())
                    .on_click(move |_, window, cx: &mut App| {
                        page.update(cx, |page, cx| page.confirm(window, cx));
                    }),
            );
        let rows = items.iter().map(|item| {
            let row = SettingsRow::new(item.label.clone()).child(
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::muted()))
                    .child(format_bytes(item.bytes)),
            );
            match self.failures.get(&item.path) {
                Some(error) => {
                    row.description(t!("maintenance.cleanup.remove_failed", error = error))
                }
                None => row,
            }
        });
        SettingsPage::new("maintenance-cleanup")
            .child(SettingsSection::new().child(header).children(rows))
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
