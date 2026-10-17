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
}
