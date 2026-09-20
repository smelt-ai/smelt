#[test]
fn workspace_overlays_stay_in_the_main_gpui_window() {
    let overlay = include_str!("../overlay.rs");
    for forbidden in ["open_window(", "WindowKind::PopUp", "WindowHandle<Root>"] {
        assert!(
            !overlay.contains(forbidden),
            "overlay.rs must not create a native GPUI overlay window: {forbidden}"
        );
    }

    let main = include_str!("../main.rs");
    for forbidden in ["overlay_plane", "overlay_parent"] {
        assert!(
            !main.contains(forbidden),
            "Workspace must not retain native overlay state: {forbidden}"
        );
    }
    assert!(
        !main.contains("Root::render_notification_layer"),
        "Workspace must not render the in-app notification layer"
    );
    assert!(main.contains("self.render_overlay_layers(window, cx)"));

    let webview = concat!(
        include_str!("../../../smelt-webview/src/lib.rs"),
        include_str!("../../../smelt-webview/src/imp.rs")
    );
    for forbidden in [
        "OverlayInputMode",
        "OverlayWindowLevel",
        "attach_overlay_window",
        "sync_overlay_window",
        "set_overlay_visible",
        "set_overlay_input_mode",
        "detach_overlay_window",
    ] {
        assert!(
            !webview.contains(forbidden),
            "smelt-webview must not expose native overlay bridge code: {forbidden}"
        );
    }
}
