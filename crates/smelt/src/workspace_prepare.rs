//! 外观落到窗口上的时机：窗口材质、界面字号。
//!
//! `Workspace::render` 每帧都会整段重跑，这些不挂在那里：外观变了在事件里排一次
//! `sync_appearance`，绘制前跑完，render 只读已经放好的结果。

use gpui::*;

use crate::Workspace;
use crate::settings::Appearance;

impl Workspace {
    /// 绘制前唯一还留在帧上的准备：窗口是否在前台只有每帧问一次系统才准，全屏切 Space 时
    /// 窗口自己的激活位会撒谎。不碰 SQLite、文件或进程。
    pub(crate) fn prepare_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.note_window_activation(window, cx);
    }

    /// 让窗口材质的种类跟上外观、深浅跟上 smelt 的主题。都没变就不再发给系统。
    pub(crate) fn sync_window_backdrop(&mut self, window: &Window, cx: &App) {
        let material = cx.global::<Appearance>().material;
        let dark = !crate::ui_theme::is_light();
        if self.applied_backdrop != Some((material, dark)) {
            crate::window_backdrop::sync(window, material, dark);
            self.applied_backdrop = Some((material, dark));
        }
    }

    /// 把一次 [`Self::sync_appearance`] 排到当前更新结束之后、下一帧绘制之前。
    ///
    /// 没有 `&mut Window` 的变更（滑块、取色器的回调）靠构造时留下的窗口句柄重新进入窗口。
    /// 同一轮里多次调用合并成一次。
    pub(crate) fn schedule_appearance_sync(&mut self, cx: &mut Context<Self>) {
        if self.appearance_sync_scheduled || self.appearance_sync_running {
            return;
        }
        self.appearance_sync_scheduled = true;
        let handle = self.window_handle;
        let view = cx.weak_entity();
        cx.defer(move |cx| {
            // 弱引用的 update 在工作区还在时返回 Ok。窗口已经关了就把排队标记清掉，
            // 否则后面的变更会一直以为刷新已经排上。
            let refreshed = cx
                .update_window(handle, |_root, window, cx| {
                    view.update(cx, |this, cx| {
                        this.sync_appearance(window, cx);
                    })
                    .is_ok()
                })
                .unwrap_or(false);
            if !refreshed {
                let _ = view.update(cx, |this, _cx| {
                    this.appearance_sync_scheduled = false;
                });
            }
        });
    }

    /// 把窗口材质和界面字号对上外观。
    ///
    /// 调用方是构造、排队后的延迟，以及外观变化。不要从 `render` 里进这里。
    pub(crate) fn sync_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.appearance_sync_scheduled = false;
        self.appearance_sync_running = true;

        self.sync_window_backdrop(window, cx);
        let ui_font_px = cx.global::<Appearance>().ui_font_px;
        if self.applied_ui_font_px != Some(ui_font_px) {
            window.set_rem_size(px(ui_font_px as f32));
            self.applied_ui_font_px = Some(ui_font_px);
        }

        self.appearance_sync_running = false;
    }
}
