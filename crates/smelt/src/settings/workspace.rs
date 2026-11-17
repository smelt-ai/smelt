//! `Workspace` 上设置页的交互：外观页的有状态控件和背景图。
//!
//! 外观的数据模型在父模块；本文件只放设置页对 `Workspace` 的状态操作。

use super::*;

impl Workspace {
    /// 懒创建外观设置的有状态控件（需要 window）。
    pub fn ensure_appearance_controls(&mut self, cx: &mut Context<Self>) {
        if self.ui_font_size_slider.is_some() && self.bg_image_opacity_slider.is_some() {
            return;
        }

        // 界面字号滑块 + 背景图透明度滑块。终端字号在终端插件自己的设置页。
        let ap = cx.global::<Appearance>().clone();
        let ui_font_size_slider = cx.new(|_| {
            SliderState::new()
                .min(MIN_UI_FONT_PX as f32)
                .max(MAX_UI_FONT_PX as f32)
                .step(1.0)
                .default_value(ap.ui_font_px as f32)
        });
        let bg_image_opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(ap.bg_image_opacity.clamp(0.0, 1.0) * 100.0)
        });

        self.settings_subs.clear();

        self.settings_subs.push(cx.subscribe(
            &ui_font_size_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let size = x
                        .round()
                        .clamp(MIN_UI_FONT_PX as f32, MAX_UI_FONT_PX as f32)
                        as u32;
                    this.set_appearance(move |a| a.ui_font_px = size, cx);
                }
            },
        ));
        self.settings_subs.push(cx.subscribe(
            &bg_image_opacity_slider,
            |this, _s, ev: &SliderEvent, cx| {
                let (SliderEvent::Change(v) | SliderEvent::Release(v)) = ev;
                if let SliderValue::Single(x) = v {
                    let op = (*x / 100.0).clamp(0.0, 1.0);
                    this.set_appearance(move |a| a.bg_image_opacity = op, cx);
                }
            },
        ));
        self.ui_font_size_slider = Some(ui_font_size_slider);
        self.bg_image_opacity_slider = Some(bg_image_opacity_slider);
    }

    /// 无 window 版：改全局 + 存盘 + 重绘。字号和材质的明暗由外观观察器排的
    /// `sync_appearance` 同步——供 slider/color_picker 的订阅回调用（它们拿不到 window）。
    pub fn set_appearance(&mut self, f: impl FnOnce(&mut Appearance), cx: &mut Context<Self>) {
        apply_appearance(f, cx);
        cx.notify();
        cx.refresh_windows();
    }

    /// 设置 / 清除背景图（挂在窗口根上，跟着重绘换，无需 window）。
    pub fn set_bg_image(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        apply_appearance(|a| a.bg_image = path, cx);
        cx.notify();
    }

    /// 弹原生选择框选一张背景图。
    pub fn pick_bg_image(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择背景图片".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths
                    .into_iter()
                    .next()
                    .and_then(|p| p.to_str().map(String::from))
            {
                this.update(cx, |this, cx| this.set_bg_image(Some(p), cx))
                    .ok();
            }
        })
        .detach();
    }
}
