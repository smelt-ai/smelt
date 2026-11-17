use gpui::AssetSource as _;

/// 应用的资源表把 smelt 自带的图标和 gpui-kit 的全套图标拼在一起，两边都要取得到。
#[test]
fn bundled_and_kit_icons_both_load() {
    for path in [
        "smelt-icons/agent-omp.svg",
        "smelt-icons/machine-linux.svg",
        "smelt-icons/trash.svg",
        gpui_kit_assets::IconName::Archive.path().as_ref(),
    ] {
        let icon = crate::SmeltAssets
            .load(path)
            .unwrap()
            .unwrap_or_else(|| panic!("{path} 没打包进应用资源"));
        assert!(!icon.is_empty());
    }
}
