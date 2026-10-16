use gpui::{
    AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window, WindowHandle,
    div,
};
use gpui_component::Root;

struct Kept;

impl Render for Kept {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct Host {
    kept: Entity<Kept>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui::test]
fn a_closed_window_is_gone_while_an_outside_owner_keeps_its_view(cx: &mut TestAppContext) {
    let window: WindowHandle<Host> = cx.add_window(|_, cx| {
        let kept = cx.new(|_| Kept);
        Host { kept }
    });
    let kept = window.update(cx, |host, _, _| host.kept.clone()).unwrap();
    let weak = kept.downgrade();

    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();

    assert!(
        weak.upgrade().is_some(),
        "窗口外还握着强引用时，实体升级仍然成功"
    );
    cx.update(|cx| {
        assert!(
            !crate::window_handle_is_open(&window, cx),
            "窗口已经关掉。主窗口还在不在必须看窗口句柄，不能看实体还能不能升级"
        );
    });
}

#[gpui::test]
fn closing_the_main_window_id_closes_settings(cx: &mut TestAppContext) {
    cx.update(gpui_base::init);
    let settings: WindowHandle<Root> = cx.add_window(|window, cx| {
        let content = cx.new(|_| Kept);
        Root::new(content, window, cx)
    });
    let main: WindowHandle<Host> = cx.add_window(|_, cx| {
        let kept = cx.new(|_| Kept);
        Host { kept }
    });
    cx.update(|cx| {
        cx.set_global(crate::settings::SettingsWindowHandle(settings));
        crate::close_settings_if_main_window_closed(
            Some(main.window_id()),
            settings.window_id(),
            cx,
        );
    });
    assert!(
        settings.update(cx, |_, _, _| ()).is_ok(),
        "关掉的不是主窗口时，设置窗口留着"
    );

    cx.update(|cx| {
        crate::close_settings_if_main_window_closed(Some(main.window_id()), main.window_id(), cx);
    });
    assert!(
        settings.update(cx, |_, _, _| ()).is_err(),
        "主窗口的 id 关掉时，设置窗口一起关掉，不再强持有旧 Workspace"
    );
}
