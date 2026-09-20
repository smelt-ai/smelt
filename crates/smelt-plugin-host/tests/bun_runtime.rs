//! Shared Bun Host integration tests.

use smelt_plugin_api::{
    ContributionId, InvocationId, InvocationOperation, InvocationRequest, InvocationResponse,
};
use smelt_plugin_host::{
    HostError, PluginPackage, PluginProcessVerifier, SharedBunHost, SharedBunHostOptions,
    SpawnOptions,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct RecordingVerifier;

impl PluginProcessVerifier for RecordingVerifier {
    fn verify(&self, package: &PluginPackage, program: &Path, _pid: u32) -> Result<(), HostError> {
        assert_ne!(program, package.entrypoint());
        assert_eq!(program, managed_bun().expect("managed bun"));
        Ok(())
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn managed_bun() -> Option<PathBuf> {
    let runtime_dir = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join(".smelt/runtime");
    fs::read_dir(&runtime_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bun"))
        .find(|path| path.is_file())?
        .canonicalize()
        .ok()
}

fn fixture_root(label: &str) -> PathBuf {
    workspace_root()
        .join("target/plugin-host-test-fixtures")
        .join(format!(
            "sph-bun-{}-{label}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn stage_echo_bun(root: &Path, id: &str) -> PluginPackage {
    let package_root = root.join(id);
    fs::create_dir_all(package_root.join("bin")).unwrap();
    let manifest = serde_json::json!({
        "id": id,
        "name": "Echo test fixture",
        "version": "0.1.0",
        "api_version": 1,
        "entrypoint": "bin/main.ts",
        "subscriptions": [],
        "publishes": [],
        "capabilities": ["ui.contribute"],
        "contributions": [
            {
                "type": "command",
                "id": "echo",
                "title": "Echo back a payload",
                "operation": "echo"
            }
        ],
        "bundled": false
    });
    fs::write(
        package_root.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        package_root.join("bin/main.ts"),
        r#"import type {
  InvocationRequest,
  SharedPlugin,
} from "@smelt-ai/plugin-sdk";

const plugin: SharedPlugin = {
  invoke(request: InvocationRequest) {
    return {
      bun_version: Bun.version,
      operation: request.operation,
      shared_host_pid: process.pid,
      echo: request.payload,
    };
  },
};

export default plugin;
"#,
    )
    .unwrap();
    let sdk = package_root.join("node_modules/@smelt-ai/plugin-sdk");
    let sdk_source = workspace_root().join("packages/plugin-sdk");
    fs::create_dir_all(&sdk).unwrap();
    fs::copy(sdk_source.join("package.json"), sdk.join("package.json")).unwrap();
    copy_tree(&sdk_source.join("src"), &sdk.join("src"));
    PluginPackage::load(package_root).unwrap()
}

fn stage_browser_bun(root: &Path) -> PluginPackage {
    let source = workspace_root().join("plugins/browser");
    let package_root = root.join("com.smelt.browser");
    fs::create_dir_all(package_root.join("bin")).unwrap();
    fs::copy(source.join("plugin.json"), package_root.join("plugin.json")).unwrap();
    fs::copy(
        source.join("plugin-ui.json"),
        package_root.join("plugin-ui.json"),
    )
    .unwrap();
    fs::copy(source.join("bin/main.ts"), package_root.join("bin/main.ts")).unwrap();
    copy_tree(&source.join("web"), &package_root.join("web"));
    PluginPackage::load(package_root).unwrap()
}

fn stage_skills_bun(root: &Path) -> PluginPackage {
    let source = workspace_root().join("plugins/skills");
    let package_root = root.join("com.smelt.skills");
    fs::create_dir_all(package_root.join("bin")).unwrap();
    fs::copy(source.join("plugin.json"), package_root.join("plugin.json")).unwrap();
    fs::copy(
        source.join("plugin-ui.json"),
        package_root.join("plugin-ui.json"),
    )
    .unwrap();
    fs::copy(source.join("bin/main.ts"), package_root.join("bin/main.ts")).unwrap();
    copy_tree(&source.join("assets"), &package_root.join("assets"));
    copy_tree(&source.join("web"), &package_root.join("web"));
    PluginPackage::load(package_root).unwrap()
}

fn bun_spawn(bun: &Path) -> SpawnOptions {
    SpawnOptions {
        bun: Some(bun.to_path_buf()),
        startup_timeout: Duration::from_secs(20),
        shutdown_grace: Duration::from_secs(2),
    }
}
/// 迁移完成后剩下的旧副本数，作为后续增量断言的基线。
fn shadowed_migrate_baseline(migrated: &serde_json::Value) -> u64 {
    migrated["data"]["legacy_count"].as_u64().unwrap()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn expect_success(response: InvocationResponse) -> serde_json::Value {
    match response {
        InvocationResponse::Success { result, .. } => result,
        InvocationResponse::Error { message, code, .. } => {
            panic!("plugin returned an error ({code:?}): {message}")
        }
    }
}

fn browser_request(host: &SharedBunHost, payload: serde_json::Value) -> InvocationResponse {
    panel_request(host, "com.smelt.browser", "browser", payload)
}

fn skills_request(host: &SharedBunHost, payload: serde_json::Value) -> InvocationResponse {
    panel_request(host, "com.smelt.skills", "skills", payload)
}

fn panel_request(
    host: &SharedBunHost,
    plugin_id: &str,
    contribution_id: &str,
    payload: serde_json::Value,
) -> InvocationResponse {
    host.invoke(
        smelt_plugin_api::PluginId::new(plugin_id).unwrap(),
        InvocationRequest {
            invocation_id: InvocationId::new(format!(
                "panel-message-{}",
                uuid::Uuid::new_v4().simple()
            ))
            .unwrap(),
            contribution_id: ContributionId::new(contribution_id).unwrap(),
            operation: InvocationOperation::new("panel.message").unwrap(),
            payload,
            deadline_ms: now_ms() + 15_000,
        },
        Duration::from_secs(15),
    )
    .unwrap()
}

#[test]
fn shared_bun_plugins_use_one_managed_bun_process() {
    let Some(bun) = managed_bun() else {
        eprintln!("受管 bun 未安装，跳过（真实下载见 smelt-core 的 manual_ensure_bun）");
        return;
    };
    let root = fixture_root("shared");
    fs::create_dir_all(&root).unwrap();
    let first = stage_echo_bun(&root, "com.example.echo-one");
    let second = stage_echo_bun(&root, "com.example.echo-two");
    let host = SharedBunHost::start_packages(
        vec![first, second],
        root.join("data"),
        Arc::new(RecordingVerifier),
        SharedBunHostOptions {
            spawn: bun_spawn(&bun),
            ..Default::default()
        },
    )
    .expect("shared bun host should start");

    let statuses = host.statuses();
    assert_eq!(statuses.len(), 2);
    let pids = statuses
        .iter()
        .filter_map(|status| match status.state {
            smelt_plugin_host::PluginLifecycleState::Ready { pid } => Some(pid),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(pids.len(), 1, "both modules must share one Bun Host pid");

    let first_response = host
        .invoke(
            smelt_plugin_api::PluginId::new("com.example.echo-one").unwrap(),
            InvocationRequest {
                invocation_id: InvocationId::new("shared-one").unwrap(),
                contribution_id: ContributionId::new("echo").unwrap(),
                operation: InvocationOperation::new("echo").unwrap(),
                payload: serde_json::json!({ "plugin": "one" }),
                deadline_ms: now_ms() + 15_000,
            },
            Duration::from_secs(15),
        )
        .unwrap();
    let second_response = host
        .invoke(
            smelt_plugin_api::PluginId::new("com.example.echo-two").unwrap(),
            InvocationRequest {
                invocation_id: InvocationId::new("shared-two").unwrap(),
                contribution_id: ContributionId::new("echo").unwrap(),
                operation: InvocationOperation::new("echo").unwrap(),
                payload: serde_json::json!({ "plugin": "two" }),
                deadline_ms: now_ms() + 15_000,
            },
            Duration::from_secs(15),
        )
        .unwrap();
    let first_result = expect_success(first_response);
    let second_result = expect_success(second_response);
    assert_eq!(first_result["echo"]["plugin"], "one");
    assert_eq!(second_result["echo"]["plugin"], "two");
    assert_eq!(
        first_result["shared_host_pid"], second_result["shared_host_pid"],
        "both modules must execute inside one Bun process"
    );

    host.set_enabled(
        smelt_plugin_api::PluginId::new("com.example.echo-one").unwrap(),
        false,
    )
    .unwrap();
    assert!(
        host.invoke(
            smelt_plugin_api::PluginId::new("com.example.echo-one").unwrap(),
            InvocationRequest {
                invocation_id: InvocationId::new("shared-disabled").unwrap(),
                contribution_id: ContributionId::new("echo").unwrap(),
                operation: InvocationOperation::new("echo").unwrap(),
                payload: serde_json::Value::Null,
                deadline_ms: now_ms() + 15_000,
            },
            Duration::from_secs(15),
        )
        .is_err()
    );
    assert!(matches!(
        host.statuses()
            .into_iter()
            .find(|status| status.plugin_id.as_str() == "com.example.echo-one")
            .unwrap()
            .state,
        smelt_plugin_host::PluginLifecycleState::Disabled
    ));
    let remaining = expect_success(
        host.invoke(
            smelt_plugin_api::PluginId::new("com.example.echo-two").unwrap(),
            InvocationRequest {
                invocation_id: InvocationId::new("shared-after-disable").unwrap(),
                contribution_id: ContributionId::new("echo").unwrap(),
                operation: InvocationOperation::new("echo").unwrap(),
                payload: serde_json::json!({ "still_loaded": true }),
                deadline_ms: now_ms() + 15_000,
            },
            Duration::from_secs(15),
        )
        .unwrap(),
    );
    assert_eq!(remaining["echo"]["still_loaded"], true);

    drop(host);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn browser_runs_in_the_shared_bun_host_without_a_plugin_binary() {
    let Some(bun) = managed_bun() else {
        eprintln!("受管 bun 未安装，跳过（真实下载见 smelt-core 的 manual_ensure_bun）");
        return;
    };

    let root = fixture_root("browser-shared");
    fs::create_dir_all(&root).unwrap();
    let package = stage_browser_bun(&root);
    let package_version = package.manifest().version.clone();
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(package.entrypoint())
            .unwrap()
            .permissions()
            .mode()
    };
    assert_eq!(
        mode & 0o111,
        0,
        "the browser entrypoint must be data, not an executable"
    );

    let data_root = root.join("data");
    let options = SharedBunHostOptions {
        spawn: bun_spawn(&bun),
        ..Default::default()
    };
    let host = SharedBunHost::start_packages(
        vec![package.clone()],
        data_root.clone(),
        Arc::new(RecordingVerifier),
        options.clone(),
    )
    .unwrap();
    let data_dir_mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(data_root.join("com.smelt.browser"))
            .unwrap()
            .permissions()
            .mode()
    };
    assert_eq!(
        data_dir_mode & 0o077,
        0,
        "the shared host must create a private plugin data directory"
    );

    let initial = expect_success(browser_request(&host, serde_json::json!({ "op": "state" })));
    assert_eq!(initial["state"]["bookmarks"], serde_json::json!([]));
    assert_eq!(
        initial["version"],
        serde_json::Value::String(package_version)
    );
    let added = expect_success(browser_request(
        &host,
        serde_json::json!({ "op": "bookmark.add", "url": "example.com", "title": "Example" }),
    ));
    assert_eq!(added["state"]["bookmarks"][0]["url"], "https://example.com");
    let updated = expect_success(browser_request(
        &host,
        serde_json::json!({
            "op": "bookmark.add",
            "url": "https://example.com",
            "title": "Updated"
        }),
    ));
    assert_eq!(updated["state"]["bookmarks"].as_array().unwrap().len(), 1);
    assert_eq!(updated["state"]["bookmarks"][0]["title"], "Updated");
    let rejected = browser_request(
        &host,
        serde_json::json!({ "op": "bookmark.add", "url": "javascript:alert(1)" }),
    );
    assert!(matches!(
        rejected,
        InvocationResponse::Error {
            code: smelt_plugin_api::InvocationErrorCode::Rejected,
            ..
        }
    ));
    expect_success(browser_request(
        &host,
        serde_json::json!({
            "op": "history.push",
            "url": "https://example.com",
            "title": "Example",
            "at": 42
        }),
    ));
    for index in 0..220 {
        expect_success(browser_request(
            &host,
            serde_json::json!({
                "op": "history.push",
                "url": format!("site{index}.example"),
                "title": "History",
                "at": index
            }),
        ));
    }
    let revisited = expect_success(browser_request(
        &host,
        serde_json::json!({
            "op": "history.push",
            "url": "site0.example",
            "title": "Revisited",
            "at": 999
        }),
    ));
    assert_eq!(revisited["state"]["history"].as_array().unwrap().len(), 200);
    assert_eq!(
        revisited["state"]["history"][0]["url"],
        "https://site0.example"
    );
    assert_eq!(revisited["state"]["history"][0]["title"], "Revisited");
    drop(host);

    assert!(data_root.join("com.smelt.browser/browser.json").is_file());
    let recovery_package = package.clone();
    let recovery_options = options.clone();
    let restarted = SharedBunHost::start_packages(
        vec![package],
        data_root.clone(),
        Arc::new(RecordingVerifier),
        options,
    )
    .unwrap();
    let restored = expect_success(browser_request(
        &restarted,
        serde_json::json!({ "op": "state" }),
    ));
    assert_eq!(
        restored["state"]["bookmarks"][0]["url"],
        "https://example.com"
    );
    assert_eq!(restored["state"]["history"][0]["at"], 999);

    drop(restarted);
    fs::write(
        data_root.join("com.smelt.browser/browser.json"),
        b"{ broken",
    )
    .unwrap();
    let recovered = SharedBunHost::start_packages(
        vec![recovery_package],
        data_root,
        Arc::new(RecordingVerifier),
        recovery_options,
    )
    .unwrap();
    let empty = expect_success(browser_request(
        &recovered,
        serde_json::json!({ "op": "state" }),
    ));
    assert_eq!(empty["state"]["bookmarks"], serde_json::json!([]));
    assert_eq!(empty["state"]["history"], serde_json::json!([]));

    drop(recovered);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn skills_runs_in_the_shared_bun_host_with_atomic_skill_updates() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Some(bun) = managed_bun() else {
        eprintln!("受管 bun 未安装，跳过（真实下载见 smelt-core 的 manual_ensure_bun）");
        return;
    };

    let root = fixture_root("skills-shared");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    let package = stage_skills_bun(&root);
    let source_manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(workspace_root().join("plugins/skills/plugin.json")).unwrap(),
    )
    .unwrap();
    let package_version = package.manifest().version.clone();
    assert_eq!(
        source_manifest["contributions"],
        serde_json::json!([]),
        "the base manifest must remain readable by older installers"
    );
    let capabilities = source_manifest["capabilities"].as_array().unwrap();
    for capability in ["ui.contribute", "ui.context.read", "ui.dialog", "fs.reveal"] {
        assert!(
            capabilities.contains(&serde_json::json!(capability)),
            "skills panel requires {capability}"
        );
    }
    let panel = package.resolve_asset("web/index.html").unwrap();
    let markup = fs::read_to_string(panel).unwrap();
    assert!(markup.contains("skills.js"));
    // skill 是磁盘上的目录，谁都能在 smelt 之外增删；面板没有文件监听，
    // 没有这个入口就只能重启才看得到手动装的 skill。
    assert!(
        markup.contains("id=\"refresh\""),
        "the panel needs a manual refresh entry"
    );
    let script = fs::read_to_string(package.resolve_asset("web/skills.js").unwrap()).unwrap();
    assert!(
        script.contains("visibilitychange"),
        "the panel must also rescan when it becomes visible again"
    );
    for asset in ["web/skills.js", "web/skills.css"] {
        assert!(
            package.resolve_asset(asset).is_some(),
            "{asset} must be packaged"
        );
    }
    assert_eq!(
        fs::metadata(package.entrypoint())
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0,
        "the skills entrypoint must be data, not an executable"
    );

    let host = SharedBunHost::start_packages(
        vec![package],
        root.join("data"),
        Arc::new(RecordingVerifier),
        SharedBunHostOptions {
            spawn: bun_spawn(&bun),
            ..Default::default()
        },
    )
    .unwrap();
    let project_root = project.to_string_lossy().into_owned();

    let initial = expect_success(skills_request(
        &host,
        serde_json::json!({ "op": "state", "project_root": project_root }),
    ));
    // 能力表来自 assets/agents.json，不是代码里的常量：断言表的形状与不变量，
    // 而不是某一版的名单，否则每加一个 agent 都要改测试。
    let table: Vec<serde_json::Value> = initial["data"]["agents"].as_array().unwrap().clone();
    assert!(table.len() >= 4, "the capability table must not be empty");
    for agent in &table {
        assert!(
            agent["label"]
                .as_str()
                .is_some_and(|label| !label.is_empty())
        );
        assert!(agent["installed"].is_boolean());
        assert!(agent["reads_agents_dir"]["user"].is_boolean());
        assert!(agent["reads_agents_dir"]["project"].is_boolean());
        // 目录要么是老实的相对路径，要么是 null（这个 agent 只认 .agents/skills）。
        for key in ["user_dir", "project_dir"] {
            let dir = &agent[key];
            assert!(
                dir.is_null() || dir.as_str().is_some_and(|dir| !dir.starts_with('/')),
                "{key} must be a relative path or null"
            );
        }
    }
    // 桥接名单 = 这台机器上装了、且自己不读 .agents/skills 的 agent。
    // 从状态里推导而不是写死，测试才不依赖跑它的那台机器装了什么。
    // 这里只跑项目级，所以看 project 那一列。
    let bridged: Vec<String> = table
        .iter()
        .filter(|agent| {
            agent["installed"] == serde_json::json!(true)
                && agent["reads_agents_dir"]["project"] == serde_json::json!(false)
                && !agent["project_dir"].is_null()
        })
        .map(|agent| agent["label"].as_str().unwrap().to_string())
        .collect();
    let unbridged: Vec<String> = table
        .iter()
        .filter(|agent| !bridged.contains(&agent["label"].as_str().unwrap().to_string()))
        .map(|agent| agent["label"].as_str().unwrap().to_string())
        .collect();
    // 需要「一条真实存在的兼容链接」的场景统一用这个 agent，
    // 机器上一个非原生 agent 都没装时才跳过，其余断言照跑。
    let bridge_probe: Option<String> = bridged.first().cloned();
    // 目录直接取自状态：`.github/skills`、`~/.config/opencode/skills` 这类
    // 一律对不上 `.<小写名>/skills`，猜不得。
    let agent_dir = |label: &str| -> Option<String> {
        table
            .iter()
            .find(|agent| agent["label"] == serde_json::json!(label))
            .and_then(|agent| agent["project_dir"].as_str())
            .map(str::to_string)
    };
    assert_eq!(initial["data"]["universal_dir"], ".agents/skills");
    assert_eq!(
        initial["version"],
        serde_json::Value::String(package_version)
    );
    assert!(
        host.invoke(
            smelt_plugin_api::PluginId::new("com.smelt.skills").unwrap(),
            InvocationRequest {
                invocation_id: InvocationId::new("skills-forged-operation").unwrap(),
                contribution_id: ContributionId::new("skills").unwrap(),
                operation: InvocationOperation::new("something.else").unwrap(),
                payload: serde_json::json!({}),
                deadline_ms: now_ms() + 15_000,
            },
            Duration::from_secs(15),
        )
        .is_err(),
        "undeclared operations must be rejected before reaching the module"
    );

    let find = |value: &serde_json::Value, name: &str| -> serde_json::Value {
        value["data"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("{name} must be listed"))
            .clone()
    };

    let created = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "create",
            "project_root": project_root,
            "project_scope": true,
            "universal": true,
            "name": "shared-round-trip",
            "description": "Shared Bun skill",
        }),
    ));
    let created_entry = find(&created, "shared-round-trip");
    let created_dir = created_entry["dir"].as_str().unwrap().to_string();
    assert_eq!(created_entry["placement"], "universal");
    assert_eq!(
        created_dir,
        project
            .join(".agents/skills/shared-round-trip")
            .to_string_lossy()
    );
    assert!(Path::new(&created_dir).join("SKILL.md").is_file());
    // 只有「在用 + 自己不读 .agents/skills」的 agent 才该拿到兼容链接。
    for label in &bridged {
        let link = project
            .join(agent_dir(label).unwrap())
            .join("shared-round-trip");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{label} needs a compatibility link at {}",
            link.display()
        );
    }
    for label in &unbridged {
        // 没有自有目录的 agent（Codex、Grok）本来就无处可查，跳过。
        let Some(dir) = agent_dir(label) else {
            continue;
        };
        assert!(
            !project.join(dir).join("shared-round-trip").exists(),
            "{label} reads .agents/skills natively or is not installed; it must not be bridged"
        );
    }
    assert_eq!(
        created_entry["missing_bridges"],
        serde_json::json!([]),
        "a freshly created universal skill has every bridge it needs"
    );

    let renamed = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "update",
            "project_root": project_root,
            "dir": created_dir,
            "name": "shared-renamed",
            "description": "Updated Shared Bun skill",
        }),
    ));
    let renamed_entry = find(&renamed, "shared-renamed");
    let renamed_dir = renamed_entry["dir"].as_str().unwrap().to_string();
    assert!(!Path::new(&created_dir).exists());
    assert!(Path::new(&renamed_dir).join("SKILL.md").is_file());
    for label in &bridged {
        assert!(
            !project
                .join(agent_dir(label).unwrap())
                .join("shared-round-trip")
                .exists(),
            "renaming must not leave a dangling link behind in {label}"
        );
    }
    assert_eq!(renamed_entry["missing_bridges"], serde_json::json!([]));

    // 作用范围：通用 → 只给某两个 agent。挑「表里前两个有自有项目目录的」，
    // 显式勾选不受「装没装」限制，所以这一段在任何机器上都跑得动。
    let hostable: Vec<String> = table
        .iter()
        .filter(|agent| !agent["project_dir"].is_null())
        .map(|agent| agent["label"].as_str().unwrap().to_string())
        .collect();
    let primary_label = hostable[0].clone();
    let secondary_label = hostable[1].clone();
    let primary_dir = project.join(agent_dir(&primary_label).unwrap());
    let secondary_dir = project.join(agent_dir(&secondary_label).unwrap());
    // 计划先讲清楚要动哪些文件。
    let plan = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "scope.plan",
            "project_root": project_root,
            "dir": renamed_dir,
            "universal": false,
            "labels": [primary_label, secondary_label],
        }),
    ));
    assert_eq!(plan["data"]["moves"], true);
    assert!(
        plan["data"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap().contains(&format!(
                "{}/shared-renamed",
                agent_dir(&primary_label).unwrap()
            ))),
        "the preview must name the destination the apply will use"
    );
    let scoped = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "scope.apply",
            "project_root": project_root,
            "dir": renamed_dir,
            "universal": false,
            "labels": [primary_label, secondary_label],
        }),
    ));
    let scoped_entry = find(&scoped, "shared-renamed");
    assert_eq!(scoped_entry["placement"], "agent");
    assert_eq!(scoped_entry["agent"], serde_json::json!(primary_label));
    let mut visible = vec![primary_label.clone(), secondary_label];
    visible.sort();
    assert_eq!(
        scoped_entry["agents"],
        serde_json::json!(visible),
        "an agent-scoped skill is visible exactly where it lives or is linked"
    );
    assert!(primary_dir.join("shared-renamed/SKILL.md").is_file());
    assert!(
        fs::symlink_metadata(secondary_dir.join("shared-renamed"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        !project.join(".agents/skills/shared-renamed").exists(),
        "the universal copy must move, not be duplicated"
    );
    for label in &hostable[2..] {
        assert!(
            !project
                .join(agent_dir(label).unwrap())
                .join("shared-renamed")
                .exists(),
            "{label} was not selected; it must not keep a copy or a link"
        );
    }

    let universal_again = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "scope.apply",
            "project_root": project_root,
            "dir": primary_dir.join("shared-renamed"),
            "universal": true,
        }),
    ));
    let universal_entry = find(&universal_again, "shared-renamed");
    assert_eq!(universal_entry["placement"], "universal");
    assert!(
        project
            .join(".agents/skills/shared-renamed/SKILL.md")
            .is_file()
    );
    for label in &bridged {
        assert!(
            fs::symlink_metadata(
                project
                    .join(agent_dir(label).unwrap())
                    .join("shared-renamed")
            )
            .unwrap()
            .file_type()
            .is_symlink(),
            "moving back to universal must leave {label} a compatibility link, not a copy"
        );
    }

    let source = root.join("import-source");
    fs::create_dir_all(source.join("references")).unwrap();
    fs::write(
        source.join("SKILL.md"),
        "---\nname: imported-shared\ndescription: First copy\n---\n\n# First\n",
    )
    .unwrap();
    fs::write(source.join("references/old.txt"), "old").unwrap();
    let imported = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "import",
            "project_root": project_root,
            "project_scope": true,
            "universal": true,
            "source": source,
        }),
    ));
    let imported_entry = find(&imported, "imported-shared");
    let imported_dir = PathBuf::from(imported_entry["dir"].as_str().unwrap());
    assert_eq!(imported_dir, project.join(".agents/skills/imported-shared"));
    assert!(imported_dir.join("references/old.txt").is_file());
    let probe_link = bridge_probe.as_ref().map(|label| {
        project
            .join(agent_dir(label).unwrap())
            .join("imported-shared")
    });
    let probe_link_inode = probe_link
        .as_ref()
        .map(|link| fs::symlink_metadata(link).unwrap().ino());

    fs::write(
        source.join("SKILL.md"),
        "---\nname: imported-shared\ndescription: Replacement copy\n---\n\n# Second\n",
    )
    .unwrap();
    fs::remove_file(source.join("references/old.txt")).unwrap();
    fs::write(source.join("references/new.txt"), "new").unwrap();
    expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "import",
            "project_root": project_root,
            "project_scope": true,
            "universal": true,
            "source": source,
        }),
    ));
    assert!(!imported_dir.join("references/old.txt").exists());
    assert_eq!(
        fs::read_to_string(imported_dir.join("references/new.txt")).unwrap(),
        "new"
    );
    if let (Some(link), Some(inode)) = (probe_link.as_ref(), probe_link_inode) {
        assert_eq!(
            fs::symlink_metadata(link).unwrap().ino(),
            inode,
            "correct links must survive an atomic content replacement"
        );
    }

    // 同名副本：不自动合并内容，让用户选真身，落败的那份进回收目录。
    // 优先挑一个真有兼容链接的 agent，这样顺带覆盖「真身目录顶掉软链」；
    // 机器上一个都没有时退回任意 agent 目录，冲突本身照样成立。
    let conflict_label = bridge_probe
        .clone()
        .unwrap_or_else(|| primary_label.clone());
    let duplicate = project
        .join(agent_dir(&conflict_label).unwrap())
        .join("imported-shared");
    if duplicate.exists() {
        fs::remove_file(&duplicate).unwrap();
    }
    fs::create_dir_all(&duplicate).unwrap();
    fs::write(
        duplicate.join("SKILL.md"),
        "---\nname: imported-shared\ndescription: stale duplicate\n---\n",
    )
    .unwrap();
    let with_duplicate = expect_success(skills_request(
        &host,
        serde_json::json!({ "op": "state", "project_root": project_root }),
    ));
    let duplicate_entry = find(&with_duplicate, "imported-shared");
    assert_eq!(duplicate_entry["conflicts"].as_array().unwrap().len(), 1);
    assert!(
        matches!(
            skills_request(
                &host,
                serde_json::json!({
                    "op": "scope.apply",
                    "project_root": project_root,
                    "dir": imported_dir,
                    "universal": true,
                }),
            ),
            InvocationResponse::Error {
                code: smelt_plugin_api::InvocationErrorCode::Rejected,
                ..
            }
        ),
        "an unresolved conflict must block any scope change"
    );
    let resolved = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "conflicts.resolve",
            "project_root": project_root,
            "dir": imported_dir,
            "keep_index": 0,
        }),
    ));
    assert!(
        !duplicate.exists(),
        "the losing copy must leave the agent dir"
    );
    let resolved_entry = find(&resolved, "imported-shared");
    assert_eq!(resolved_entry["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(
        resolved_entry["missing_bridges"],
        match bridge_probe.as_ref() {
            Some(label) => serde_json::json!([label]),
            None => serde_json::json!([]),
        },
        "a bridge that was overwritten by a real directory must be reported as missing"
    );
    expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "bridges.repair",
            "project_root": project_root,
            "dir": imported_dir,
        }),
    ));
    match bridge_probe.as_ref() {
        Some(_) => assert!(
            fs::symlink_metadata(&duplicate)
                .unwrap()
                .file_type()
                .is_symlink(),
            "repairing must rebuild the compatibility link"
        ),
        None => assert!(
            !duplicate.exists(),
            "an agent nobody uses must not get a directory conjured for it"
        ),
    }

    // 旧的 .smelt/skills 只读出来引导迁移，迁完落到通用目录。
    let legacy = project.join(".smelt/skills/legacy-shared");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(
        legacy.join("SKILL.md"),
        "---\nname: legacy-shared\ndescription: Legacy copy\n---\n",
    )
    .unwrap();
    let legacy_state = expect_success(skills_request(
        &host,
        serde_json::json!({ "op": "state", "project_root": project_root }),
    ));
    let legacy_entry = find(&legacy_state, "legacy-shared");
    assert_eq!(legacy_entry["placement"], "legacy");
    let migrated = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "legacy.migrate",
            "project_root": project_root,
            "project_scope": true,
        }),
    ));
    assert_eq!(
        migrated["data"]["migrated"],
        serde_json::json!(["legacy-shared"])
    );
    assert!(!legacy.exists());
    let migrated_entry = find(&migrated, "legacy-shared");
    assert_eq!(migrated_entry["placement"], "universal");
    assert!(
        project
            .join(".agents/skills/legacy-shared/SKILL.md")
            .is_file()
    );
    for label in &bridged {
        assert!(
            fs::symlink_metadata(
                project
                    .join(agent_dir(label).unwrap())
                    .join("legacy-shared")
            )
            .unwrap()
            .file_type()
            .is_symlink(),
            "a migrated skill must reach {label} too"
        );
    }

    // 通用目录里的软链是「引用」，不是本体。曾经只在 agent 目录里认软链，
    // 于是这种链接被 isDirectory 跟随后当成实体副本参与同名处理：用户选它保留、
    // 真身进回收目录，链接当场悬空，skill 直接消失。
    let real_home = project.join(".smelt/skills/linked-only");
    fs::create_dir_all(&real_home).unwrap();
    fs::write(
        real_home.join("SKILL.md"),
        "---\nname: linked-only\ndescription: The only real copy\n---\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&real_home, project.join(".agents/skills/linked-only")).unwrap();
    let linked_state = expect_success(skills_request(
        &host,
        serde_json::json!({ "op": "state", "project_root": project_root }),
    ));
    let linked_entry = find(&linked_state, "linked-only");
    assert_eq!(
        linked_entry["conflicts"].as_array().unwrap().len(),
        0,
        "a symlink pointing at the real copy is a reference, never a rival copy"
    );
    assert_eq!(
        linked_entry["dir"].as_str().unwrap(),
        real_home.to_str().unwrap(),
        "the entry must resolve to the real directory, not to the link"
    );
    // 删本体时，通用目录里那条链接也要被带走，不能留成悬空链接。
    expect_success(skills_request(
        &host,
        serde_json::json!({ "op": "delete", "project_root": project_root, "dir": real_home }),
    ));
    assert!(
        fs::symlink_metadata(project.join(".agents/skills/linked-only")).is_err(),
        "deleting the real copy must take the link with it"
    );

    // 悬空链接不属于任何 skill，面板够不着，只能单独报出来清理。
    std::os::unix::fs::symlink(
        project.join(".agents/skills/never-existed"),
        project.join(".agents/skills/dangling"),
    )
    .unwrap();
    let dangling_state = expect_success(skills_request(
        &host,
        serde_json::json!({ "op": "state", "project_root": project_root }),
    ));
    assert!(
        dangling_state["data"]["broken_links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["path"] == serde_json::json!(project.join(".agents/skills/dangling"))),
        "a dangling link must be reported so the user can clear it"
    );
    // 只清这个项目：跑测试的机器上，用户级往往本来就有失效链接，
    // 测试不该顺手改动开发者的家目录。
    let pruned = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "links.prune",
            "project_root": project_root,
            "project_scope": true,
        }),
    ));
    assert_eq!(pruned["data"]["pruned"].as_array().unwrap().len(), 1);
    assert!(fs::symlink_metadata(project.join(".agents/skills/dangling")).is_err());
    assert!(
        !pruned["data"]["broken_links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["project_scope"] == serde_json::json!(true)),
        "pruning must leave nothing behind in the scope it was asked to clean"
    );
    // 清理只碰失效链接，健在的兼容链接必须原封不动。
    for label in &bridged {
        assert!(
            fs::symlink_metadata(
                project
                    .join(agent_dir(label).unwrap())
                    .join("legacy-shared")
            )
            .unwrap()
            .file_type()
            .is_symlink(),
            "pruning must not touch working links in {label}"
        );
    }

    // 与通用目录同名的旧副本会被归进 conflicts，placement 不是 "legacy"。
    // 横幅按「含冲突」计数，迁移却只找 placement=="legacy" —— 口径不一致的结果
    // 就是点了迁移毫无反应。必须照实报成 skipped，用户才知道要去处理冲突。
    // legacy_count 是「用户级 + 项目级」的总数，跑测试的机器上可能本来就有旧副本，
    // 所以只能断增量。
    let legacy_baseline = shadowed_migrate_baseline(&migrated);
    let shadowed = project.join(".smelt/skills/shared-renamed");
    fs::create_dir_all(&shadowed).unwrap();
    fs::write(
        shadowed.join("SKILL.md"),
        "---\nname: shared-renamed\ndescription: Shadowed legacy copy\n---\n",
    )
    .unwrap();
    let shadowed_state = expect_success(skills_request(
        &host,
        serde_json::json!({ "op": "state", "project_root": project_root }),
    ));
    assert_eq!(
        shadowed_state["data"]["legacy_count"].as_u64().unwrap(),
        legacy_baseline + 1,
        "a legacy copy hiding inside conflicts still counts for the banner"
    );
    let shadowed_migrate = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "legacy.migrate",
            "project_root": project_root,
            "project_scope": true,
        }),
    ));
    assert_eq!(shadowed_migrate["data"]["migrated"], serde_json::json!([]));
    assert_eq!(
        shadowed_migrate["data"]["skipped"],
        serde_json::json!(["shared-renamed"]),
        "migration must report what it could not move instead of silently doing nothing"
    );
    assert!(
        shadowed.exists(),
        "a blocked migration must not quietly destroy the legacy copy"
    );

    // 删除默认只删这一份，同名副本会顶上来——那一行不会消失。界面必须讲清楚，
    // 后端也得能一次删干净，否则用户点了删除只会觉得「什么都没发生」。
    let one_gone = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "delete",
            "project_root": project_root,
            "dir": project.join(".agents/skills/shared-renamed"),
        }),
    ));
    let survivor = find(&one_gone, "shared-renamed");
    assert_eq!(
        survivor["placement"], "legacy",
        "deleting one copy must leave the other standing, not silently take both"
    );
    assert!(shadowed.exists());
    let all_gone = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "delete",
            "project_root": project_root,
            "dir": survivor["dir"].as_str().unwrap(),
            "all": true,
        }),
    ));
    assert!(
        !all_gone["data"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "shared-renamed"),
        "deleting every copy must make the name disappear from the panel"
    );
    assert!(!shadowed.exists());

    // 后面的用例还要用这个 skill，重新造一份回来。
    let universal_dir = project.join(".agents/skills/shared-renamed");
    fs::create_dir_all(&universal_dir).unwrap();
    fs::write(
        universal_dir.join("SKILL.md"),
        "---\r\nname: shared-renamed\r\ndescription: before\r\ncustom: retain\r\n---\r\n\r\n# Body\r\n",
    )
    .unwrap();
    expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "update",
            "project_root": project_root,
            "dir": universal_dir,
            "name": "shared-renamed",
            "description": "after",
        }),
    ));
    let rewritten = fs::read_to_string(universal_dir.join("SKILL.md")).unwrap();
    assert!(rewritten.contains("custom: retain\n"));
    assert!(rewritten.contains("# Body\n"));
    assert!(
        !rewritten.contains('\r'),
        "rewriting frontmatter must retain Rust's normalized LF output"
    );
    assert!(matches!(
        skills_request(
            &host,
            serde_json::json!({
                "op": "create",
                "project_root": project_root,
                "project_scope": true,
                "name": "../invalid",
                "description": "",
            }),
        ),
        InvocationResponse::Error {
            code: smelt_plugin_api::InvocationErrorCode::Rejected,
            ..
        }
    ));
    assert!(
        matches!(
            skills_request(
                &host,
                serde_json::json!({
                    "op": "scope.apply",
                    "project_root": project_root,
                    "dir": universal_dir,
                    "universal": false,
                    "labels": [],
                }),
            ),
            InvocationResponse::Error {
                code: smelt_plugin_api::InvocationErrorCode::Rejected,
                ..
            }
        ),
        "an agent-scoped skill with no agent selected has nowhere to live"
    );

    let deleted = expect_success(skills_request(
        &host,
        serde_json::json!({
            "op": "delete",
            "project_root": project_root,
            "dir": universal_dir,
        }),
    ));
    assert!(
        !deleted["data"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "shared-renamed")
    );
    assert!(
        !primary_dir.join("shared-renamed").exists(),
        "deleting must take the compatibility links with it"
    );
    assert!(matches!(
        skills_request(
            &host,
            serde_json::json!({
                "op": "delete",
                "project_root": project_root,
                "dir": universal_dir,
            }),
        ),
        InvocationResponse::Error {
            code: smelt_plugin_api::InvocationErrorCode::Rejected,
            ..
        }
    ));

    drop(host);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn shared_bun_rejects_a_symlinked_plugin_data_directory() {
    use std::os::unix::fs::symlink;

    let root = fixture_root("shared-data-dir");
    fs::create_dir_all(&root).unwrap();
    let package = stage_echo_bun(&root, "com.example.shared-data-dir");
    let data_root = root.join("data");
    fs::create_dir_all(&data_root).unwrap();
    symlink(&root, data_root.join("com.example.shared-data-dir")).unwrap();

    let host = SharedBunHost::start_packages(
        vec![package],
        data_root,
        Arc::new(RecordingVerifier),
        Default::default(),
    )
    .unwrap();
    let status = host.statuses().pop().unwrap();
    assert!(matches!(
        status.state,
        smelt_plugin_host::PluginLifecycleState::Failed
    ));
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("data directory must be a real directory"))
    );

    drop(host);
    fs::remove_dir_all(&root).ok();
}
