use gpui::{
    AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window, WindowHandle,
    div,
};

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
