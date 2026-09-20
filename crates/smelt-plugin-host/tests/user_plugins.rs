use smelt_plugin_api::Contribution;
use smelt_plugin_host::{
    PluginProvenance, discover_all_plugins, discover_user_plugins, install_user_plugin,
    list_user_plugins, plan_user_plugin_install, uninstall_user_plugin,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn root(label: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("target/plugin-host-test-fixtures")
        .join(format!(
            "smelt-user-plugin-{}-{label}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
}

fn package(root: &Path, version: &str, body: &str) -> PathBuf {
    let package = root.join(format!("source-{version}"));
    let entrypoint = "bin/main.ts";
    fs::create_dir_all(package.join(Path::new(entrypoint).parent().unwrap())).unwrap();
    fs::write(
        package.join("plugin.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": "com.example.third-party",
            "name": "Third Party",
            "version": version,
            "api_version": 1,
            "entrypoint": entrypoint,
            "subscriptions": [],
            "publishes": [],
            "capabilities": [],
            "contributions": [],
            "bundled": false
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(package.join(entrypoint), body).unwrap();
    package
}

fn set_capabilities(package: &Path, capabilities: &[&str]) {
    let manifest_path = package.join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["capabilities"] = serde_json::json!(capabilities);
    fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

fn install(smelt_root: &Path, source: &Path) -> smelt_plugin_host::InstalledUserPlugin {
    let plan = plan_user_plugin_install(smelt_root, source).unwrap();
    install_user_plugin(smelt_root, &plan).unwrap()
}

#[test]
fn an_installed_package_keeps_its_ui_sidecar_and_panel_assets() {
    // tab 是用户能看见的那一半。安装路径整包复制，一旦漏掉 plugin-ui.json 或
    // web/，插件进程照样起得来，但右侧 tab 栏什么都不会多——这种"装成功了却没反应"
    // 最难查，所以这条要盯着 contribution 而不只是盯着安装返回值。
    let root = root("ui-sidecar");
    let smelt_root = root.join("home");
    let source = package(
        &root,
        "1.0.0",
        "export default { invoke() { return null; } }\n",
    );
    // UI 贡献必须自带 ui.contribute，否则宿主会丢弃整条声明。
    set_capabilities(&source, &["ui.contribute"]);
    fs::write(
        source.join("plugin-ui.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "contributions": [
                { "type": "tool_panel", "id": "hello", "title": "Hello", "entry": "web/index.html" }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::create_dir_all(source.join("web")).unwrap();
    fs::write(source.join("web/index.html"), "<!doctype html>hi\n").unwrap();

    install(&smelt_root, &source);
    let installed = discover_user_plugins(&smelt_root)
        .into_iter()
        .next()
        .unwrap()
        .unwrap();

    let panels = installed
        .manifest()
        .contributions
        .iter()
        .filter_map(|contribution| match contribution {
            Contribution::ToolPanel { id, title, entry } => Some((id.as_str(), title, entry)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(panels.len(), 1, "UI sidecar 必须在安装后仍然生效");
    assert_eq!(panels[0].1, "Hello");
    // GUI 用 resolve_asset 定位面板入口，解析不出来就会静默跳过这个 contribution。
    assert!(
        installed.resolve_asset(panels[0].2).is_some(),
        "面板入口资源必须随包一起被安装"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn first_install_records_the_exact_approved_capabilities() {
    let root = root("approval");
    let smelt_root = root.join("home");
    let source = package(
        &root,
        "1.0.0",
        "export default { invoke() { return null; } }\n",
    );
    set_capabilities(&source, &["session.read", "workspace.read"]);

    let plan = plan_user_plugin_install(&smelt_root, &source).unwrap();
    assert!(plan.requires_confirmation);
    assert_eq!(
        plan.requested_capabilities,
        ["session.read", "workspace.read"]
    );
    let installed = install_user_plugin(&smelt_root, &plan).unwrap();
    assert_eq!(
        installed.approved_capabilities,
        ["session.read", "workspace.read"]
    );

    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(smelt_root.join("plugins/.installed.json")).unwrap())
            .unwrap();
    assert_eq!(
        registry["plugins"]["com.example.third-party"]["capabilities"],
        serde_json::json!(["session.read", "workspace.read"])
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn install_rejects_package_or_capability_changes_after_review() {
    let root = root("review-race");
    let smelt_root = root.join("home");
    let source = package(&root, "1.0.0", "v1\n");
    let plan = plan_user_plugin_install(&smelt_root, &source).unwrap();
    fs::write(source.join("bin/main.ts"), "changed\n").unwrap();
    let error = install_user_plugin(&smelt_root, &plan).unwrap_err();
    assert!(error.to_string().contains("changed after"));

    let source = package(&root, "2.0.0", "v2\n");
    set_capabilities(&source, &["session.read"]);
    let mut plan = plan_user_plugin_install(&smelt_root, &source).unwrap();
    plan.requested_capabilities.clear();
    let error = install_user_plugin(&smelt_root, &plan).unwrap_err();
    assert!(error.to_string().contains("capabilities changed"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_diffs_against_the_last_approved_capabilities() {
    let root = root("approval-diff");
    let smelt_root = root.join("home");
    let first = package(&root, "1.0.0", "v1\n");
    set_capabilities(&first, &["session.read"]);
    install(&smelt_root, &first);

    let expanded = package(&root, "2.0.0", "v2\n");
    set_capabilities(&expanded, &["session.read", "workspace.read"]);
    let plan = plan_user_plugin_install(&smelt_root, &expanded).unwrap();
    assert!(plan.requires_confirmation);
    assert_eq!(plan.added_capabilities, ["workspace.read"]);
    install_user_plugin(&smelt_root, &plan).unwrap();

    let reduced = package(&root, "3.0.0", "v3\n");
    set_capabilities(&reduced, &["session.read"]);
    let plan = plan_user_plugin_install(&smelt_root, &reduced).unwrap();
    assert!(!plan.requires_confirmation);
    assert_eq!(plan.removed_capabilities, ["workspace.read"]);
    install_user_plugin(&smelt_root, &plan).unwrap();
    assert_eq!(
        list_user_plugins(&smelt_root)[0]
            .as_ref()
            .unwrap()
            .approved_capabilities,
        ["session.read"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_or_mismatched_approval_records_never_start() {
    let root = root("legacy-approval");
    let smelt_root = root.join("home");
    let source = package(&root, "1.0.0", "v1\n");
    set_capabilities(&source, &["session.read"]);
    let installed = install(&smelt_root, &source);
    let registry_path = smelt_root.join("plugins/.installed.json");

    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "plugins": { "com.example.third-party": installed.digest }
        }))
        .unwrap(),
    )
    .unwrap();
    let error = discover_user_plugins(&smelt_root).remove(0).unwrap_err();
    assert!(error.to_string().contains("requires capability approval"));

    fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "plugins": {
                "com.example.third-party": {
                    "digest": installed.digest,
                    "capabilities": []
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let error = discover_user_plugins(&smelt_root).remove(0).unwrap_err();
    assert!(error.to_string().contains("differ from the approved set"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn install_records_provenance_and_rejects_in_place_changes() {
    let root = root("digest");
    let smelt_root = root.join("home");
    let source = package(&root, "1.0.0", "v1\n");

    let installed = install(&smelt_root, &source);
    assert_eq!(installed.id, "com.example.third-party");
    assert_eq!(installed.version, "1.0.0");
    assert_eq!(installed.digest.len(), 64);

    let discovered = discover_user_plugins(&smelt_root)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(
        discovered[0].provenance(),
        &PluginProvenance::UserInstalled {
            digest: installed.digest
        }
    );

    fs::write(
        smelt_root.join("plugins/com.example.third-party/bin/main.ts"),
        "tampered\n",
    )
    .unwrap();
    let error = discover_user_plugins(&smelt_root)
        .into_iter()
        .next()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("changed after installation"));
    let listed = list_user_plugins(&smelt_root)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(
        listed[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("changed after installation"))
    );
    // 拒绝运行不等于从管理界面消失：损坏包仍然能卸载。
    uninstall_user_plugin(&smelt_root, "com.example.third-party").unwrap();

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn update_replaces_the_whole_package_and_uninstall_removes_the_record() {
    let root = root("update");
    let smelt_root = root.join("home");
    let first = package(&root, "1.0.0", "v1\n");
    let second = package(&root, "2.0.0", "v2\n");

    install(&smelt_root, &first);
    let updated = install(&smelt_root, &second);
    assert_eq!(updated.version, "2.0.0");
    let listed = list_user_plugins(&smelt_root)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].version, "2.0.0");
    assert_eq!(
        fs::read_to_string(smelt_root.join("plugins/com.example.third-party/bin/main.ts")).unwrap(),
        "v2\n"
    );

    uninstall_user_plugin(&smelt_root, "com.example.third-party").unwrap();
    assert!(list_user_plugins(&smelt_root).is_empty());
    assert!(!smelt_root.join("plugins/com.example.third-party").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn leftover_runtime_field_does_not_block_install() {
    let root = root("ignored-runtime");
    let smelt_root = root.join("home");
    let source = package(&root, "1.0.0", "v1\n");
    let manifest_path = source.join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["runtime"] = serde_json::json!("wasm");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    install(&smelt_root, &source);
    assert_eq!(
        list_user_plugins(&smelt_root)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .len(),
        1
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn uninstall_clears_the_registry_even_if_the_package_was_deleted_by_hand() {
    let root = root("missing-package");
    let smelt_root = root.join("home");
    let source = package(&root, "1.0.0", "v1\n");
    install(&smelt_root, &source);
    fs::remove_dir_all(smelt_root.join("plugins/com.example.third-party")).unwrap();

    uninstall_user_plugin(&smelt_root, "com.example.third-party").unwrap();
    assert!(list_user_plugins(&smelt_root).is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_user_plugin_cannot_shadow_a_bundled_plugin_id() {
    let root = root("duplicate");
    let smelt_root = root.join("home");
    let bundled_root = root.join("bundled");
    fs::create_dir_all(&bundled_root).unwrap();
    let bundled_source = package(&root, "1.0.0", "bundled\n");
    fs::rename(
        &bundled_source,
        bundled_root.join("com.example.third-party"),
    )
    .unwrap();
    let user_source = package(&root, "2.0.0", "user\n");
    install(&smelt_root, &user_source);

    let discovered = discover_all_plugins(Some(&bundled_root), &smelt_root);
    assert_eq!(discovered.len(), 2);
    let bundled = discovered[0].as_ref().unwrap();
    assert_eq!(bundled.manifest().version, "1.0.0");
    assert_eq!(bundled.provenance(), &PluginProvenance::FirstParty);
    let conflict = discovered[1].as_ref().unwrap_err().to_string();
    assert!(conflict.contains("conflicts with an already discovered plugin"));

    fs::remove_dir_all(root).unwrap();
}
