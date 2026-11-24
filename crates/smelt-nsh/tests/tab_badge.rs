//! 非活跃面板页签（如未点开的 `smelt.git/main`「变更」）角标与标题响应式更新回归测试：
//! 覆盖：
//! 1. 冷启动时守护离线，未点开「变更」页签；守护上线重连后，多仓库 `git.discover` + `git.status`
//!    必须立即跑通（不被失败缓存节流 15 秒）并算出合并角标 `"3"`；
//! 2. 不点开「变更」页签，`MainShell` 的 `renderTab` 必须把 `"3"` 直接渲染到未选中页签上；
//! 3. `project_changed` 后变为 0 条变更时角标消失，再变成 5 条时更新为 `"5"`；
//! 4. 切换语言（`en` → `zh-CN`）时，未选中页签标题与角标同步更新为「变更」+ `"5"`，全程不点击页签。

use std::{
    collections::HashMap,
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, IntoElement, Render, TestAppContext,
    VisualTestContext, Window, div, px, size,
};
use serde_json::{Value, json};
use smelt_nsh::{
    BackdropHooks, MainShell, OutletTarget, PluginSource, ShellConfig, ShellLabels, SourceDir,
    WindowCorners,
};
use smelt_plugin_api::{
    self as api, ActiveContext, Host, HostEvent, Machine, NotificationPermission, PluginView,
    Project, RuntimeKey, ViewHandle, ViewSpec,
    client::{Connector, RuntimeClient, RuntimeStream, protocol::control::ControlResponse},
    model::{
        daemon_state::{DaemonSessionState, RuntimePresence},
        runtime_path::PathStyle,
        workspace_directory::WorkspaceDirectory,
    },
};

type RepoFiles = HashMap<String, Vec<(String, String)>>;

struct TabBadgeTestHost {
    local_online: Arc<AtomicBool>,
    repo_files: Arc<Mutex<RepoFiles>>,
}

impl Host for TabBadgeTestHost {
    fn machines(&self, _cx: &App) -> Vec<Machine> {
        vec![Machine {
            key: RuntimeKey::Local,
            name: "This Mac".into(),
            unavailable: None,
        }]
    }

    fn client(&self, machine: &RuntimeKey, _cx: &App) -> Option<RuntimeClient> {
        if *machine != RuntimeKey::Local || !self.local_online.load(Ordering::SeqCst) {
            return None;
        }
        let repo_files = self.repo_files.clone();
        Some(RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let repo_files = repo_files.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .is_err()
                {
                    return;
                }
                let Ok(req) = serde_json::from_str::<Value>(&line) else {
                    return;
                };
                let request_id = req["request_id"].as_str().unwrap_or("req");
                let method = req["method"].as_str().unwrap_or("");
                let params = &req["params"];
                let payload = match method {
                    "project.watch" => json!({}),
                    "project.tree" => json!({ "entries": [] }),
                    "git.discover" => json!({
                        "repos": [
                            {
                                "root": "/work/smelt",
                                "rel_path": "",
                                "kind": "root"
                            },
                            {
                                "root": "/work/smelt/sub",
                                "rel_path": "sub",
                                "kind": "submodule"
                            }
                        ],
                        "truncated": false
                    }),
                    "git.status" => {
                        let root = params["root"].as_str().unwrap_or("");
                        let files = repo_files
                            .lock()
                            .unwrap()
                            .get(root)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(code, path)| json!([code, path]))
                            .collect::<Vec<_>>();
                        json!({
                            "ok": true,
                            "branch": "main",
                            "ahead": 0,
                            "behind": 0,
                            "upstream": "origin/main",
                            "stash_count": 0,
                            "files": files,
                            "worktree_hashes": {},
                            "insertions": 0,
                            "deletions": 0
                        })
                    }
                    "git.branches" => json!({
                        "local": ["main"],
                        "remote": ["origin/main"]
                    }),
                    "git.diff" => json!({
                        "patch": "",
                        "truncated": false
                    }),
                    "remote.config.get" => json!({ "enabled": false, "iroh_relay": "" }),
                    _ => json!({}),
                };
                let resp = serde_json::to_value(ControlResponse::ok(request_id, payload)).unwrap();
                let _ = writeln!(&server, "{resp}");
            });
            Ok(client)
        }))
    }

    fn connector(&self, _machine: &RuntimeKey, _cx: &App) -> Option<Arc<dyn Connector>> {
        None
    }

    fn machine_online(&self, machine: &RuntimeKey, _cx: &App) -> bool {
        *machine == RuntimeKey::Local && self.local_online.load(Ordering::SeqCst)
    }

    fn connections(&self, _cx: &App) -> api::Connections {
        api::Connections::default()
    }

    fn view_shown(&self, id: &str, _cx: &App) -> bool {
        // 模拟「文件」当前激活、「变更」从头到尾都没点开过的真实状态。
        id != "smelt.git/main"
    }

    fn view_laid_out(&self, _id: &str, _cx: &App) -> bool {
        true
    }

    fn main_window(&self, _cx: &App) -> Option<AnyWindowHandle> {
        None
    }

    fn window_active(&self, _cx: &App) -> bool {
        true
    }

    fn overlay_open(&self, _cx: &App) -> bool {
        false
    }

    fn path_style(&self, _machine: &RuntimeKey, _cx: &App) -> PathStyle {
        PathStyle::Posix
    }

    fn session_states(&self, _machine: &RuntimeKey, _cx: &App) -> Vec<DaemonSessionState> {
        Vec::new()
    }

    fn session_presence(
        &self,
        _machine: &RuntimeKey,
        _session_id: &str,
        _cx: &App,
    ) -> Option<RuntimePresence> {
        None
    }

    fn workspace_directory(&self, _machine: &RuntimeKey, _cx: &App) -> Option<WorkspaceDirectory> {
        Some(WorkspaceDirectory {
            epoch: "1".into(),
            revision: 1,
            projects: vec!["/work/smelt".into()],
            ..Default::default()
        })
    }

    fn notification_permission(&self, _cx: &App) -> NotificationPermission {
        NotificationPermission::Unsupported
    }
}

struct StubView;

impl Render for StubView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl PluginView for StubView {
    fn open(&mut self, _: &Value, _: &mut Window, _: &mut Context<Self>) {}
}

fn stub_spec(name: &str) -> ViewSpec {
    ViewSpec {
        name: name.to_owned().into(),
        title: name.to_owned().into(),
        icon: "icons/layout-dashboard.svg".into(),
        draws_top_bar: true,
        settings: None,
        instances: false,
        fills: Vec::new(),
        build: Rc::new(|_, cx| -> Rc<dyn ViewHandle> { Rc::new(cx.new(|_| StubView)) }),
    }
}

fn node_texts(node: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(aria) = node.get("aria") {
        if let Some(label) = aria.get("label").and_then(Value::as_str)
            && !label.is_empty()
        {
            out.push(label.to_owned());
        }
        if let Some(value) = aria.get("value").and_then(Value::as_str)
            && !value.is_empty()
        {
            out.push(value.to_owned());
        }
    }
    out
}

fn collect_subtree_texts(nodes: &serde_json::Map<String, Value>, id: &str, out: &mut Vec<String>) {
    let Some(node) = nodes.get(id) else {
        return;
    };
    out.extend(node_texts(node));
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            if let Some(child_id) = child.as_str() {
                collect_subtree_texts(nodes, child_id, out);
            } else if let Some(child_id) = child.as_u64() {
                collect_subtree_texts(nodes, &child_id.to_string(), out);
            }
        }
    }
}

fn tab_nodes_snapshot(cx: &mut VisualTestContext) -> Vec<(String, bool, Vec<String>)> {
    let Some(raw) = cx.update(|window, _| window.debug_a11y_tree_json()) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(nodes) = parsed.get("nodes").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut tabs = Vec::new();
    for (id, node) in nodes {
        let role = node
            .pointer("/aria/role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !role.eq_ignore_ascii_case("tab") {
            continue;
        }
        let selected = node
            .pointer("/aria/selected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut texts = Vec::new();
        collect_subtree_texts(nodes, id, &mut texts);
        tabs.push((id.clone(), selected, texts));
    }
    tabs
}

fn a11y_texts(cx: &mut VisualTestContext) -> Vec<String> {
    let Some(raw) = cx.update(|window, _| window.debug_a11y_tree_json()) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(nodes) = parsed.get("nodes").and_then(Value::as_object) else {
        return Vec::new();
    };
    nodes.values().flat_map(node_texts).collect()
}

fn inactive_tab_has(cx: &mut VisualTestContext, title: &str, badge: Option<&str>) -> bool {
    let tabs = tab_nodes_snapshot(cx);
    tabs.into_iter().any(|(_, selected, texts)| {
        if selected {
            return false;
        }
        let has_title = texts.iter().any(|t| t == title || t.contains(title));
        if !has_title {
            return false;
        }
        match badge {
            Some(b) => texts.iter().any(|t| t == b || t.ends_with(b)),
            None => !texts
                .iter()
                .any(|t| t != title && t.chars().all(|c| c.is_ascii_digit())),
        }
    })
}

fn wait_for(
    cx: &mut VisualTestContext,
    timeout: Duration,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        if start.elapsed() >= timeout {
            panic!(
                "未等到：{what}；tabs={:?}；all_a11y={:?}",
                tab_nodes_snapshot(cx),
                a11y_texts(cx)
            );
        }
        // 轮询例外：测试平台等待引擎线程提交帧并完成重绘；周期 15ms，上限由调用方指定。
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[gpui::test]
fn inactive_tab_badge_and_title_update_without_clicking(cx: &mut TestAppContext) {
    unsafe {
        std::env::set_var("GPUI_A11Y_ACTIVE", "1");
    }

    let local_online = Arc::new(AtomicBool::new(false));
    let repo_files: Arc<Mutex<RepoFiles>> = Arc::new(Mutex::new(HashMap::from([
        (
            "/work/smelt".to_owned(),
            vec![
                (" M".to_owned(), "Cargo.toml".to_owned()),
                ("??".to_owned(), "README.md".to_owned()),
            ],
        ),
        (
            "/work/smelt/sub".to_owned(),
            vec![(" M".to_owned(), "lib.rs".to_owned())],
        ),
    ])));

    cx.executor().allow_parking();
    cx.update(|cx| {
        smelt_ui::init(cx);
        api::set_locale("en", cx);
        api::install(
            TabBadgeTestHost {
                local_online: local_online.clone(),
                repo_files: repo_files.clone(),
            },
            cx,
        );
        api::register_views(
            "smelt",
            vec![stub_spec("outlet"), stub_spec("settings")],
            cx,
        )
        .unwrap();
        api::register_commands(
            "smelt.app",
            "App",
            vec![api::SET_VIEW_BADGE.spec("Set view badge", |args, _, cx| {
                api::set_view_badge(&args.view, args.label.map(Into::into), cx);
                Ok(())
            })],
            cx,
        )
        .unwrap();
    });

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let subset_dir =
        std::env::temp_dir().join(format!("smelt-nsh-tab-badge-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&subset_dir).unwrap();
    for name in ["files", "git", "layouts", "themes"] {
        #[cfg(unix)]
        std::os::unix::fs::symlink(repo_plugins.join(name), subset_dir.join(name)).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(repo_plugins.join(name), subset_dir.join(name)).unwrap();
    }
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _guard = TempDir(subset_dir.clone());

    let (shell, cx) = cx.add_window_view(|window, cx| {
        MainShell::new(
            ShellConfig {
                sources: vec![SourceDir {
                    path: subset_dir,
                    source: PluginSource::Builtin,
                    watch: false,
                }],
                backdrop: BackdropHooks::default(),
                corners: WindowCorners {
                    lights: px(78.),
                    rounded: px(10.),
                },
                padding: 0.,
                labels: ShellLabels::default(),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1200.), px(800.)));

    wait_for(
        cx,
        Duration::from_secs(10),
        "内置插件初始化完毕",
        |cx| {
            shell.read_with(cx, |s, _| s.initial_plugins_settled())
                && cx.update(|_, cx| {
                    api::layout("smelt.layouts/standard", cx).is_some()
                        && api::is_registered("smelt.files/main", cx)
                        && api::is_registered("smelt.git/main", cx)
                })
        },
    );

    let standard = cx
        .update(|_, cx| api::layout("smelt.layouts/standard", cx))
        .expect("standard layout");
    shell.update(cx, |shell, cx| {
        shell
            .load_page("smelt.layouts/workbench".into(), &standard, cx)
            .unwrap();
        shell.set_active_page(
            "smelt.layouts/workbench".into(),
            Some(OutletTarget {
                id: "smelt/outlet".into(),
                args: Value::Null,
                seq: 1,
                managed_rust_instance: false,
            }),
            cx,
        );
    });
    cx.update(|_, cx| {
        api::set_active_context(
            ActiveContext {
                project: Some(Project {
                    machine: RuntimeKey::Local,
                    root: "/work/smelt".into(),
                }),
                session: None,
            },
            cx,
        );
    });

    // 1. 守护未上线前，未点开的「Changes」页签已渲染且无角标 "3"。
    wait_for(
        cx,
        Duration::from_secs(5),
        "未选中 Changes 页签渲染出来且无角标",
        |cx| inactive_tab_has(cx, "Changes", None),
    );

    // 2. 守护上线重连后（不点开 Changes），5s 内必须立刻算完多仓变更数 "3" 并渲染到未选中页签上。
    local_online.store(true, Ordering::SeqCst);
    cx.update(|_, cx| {
        api::emit(HostEvent::MachinesChanged, cx);
        api::emit(HostEvent::Reconnected(RuntimeKey::Local), cx);
    });
    wait_for(
        cx,
        Duration::from_secs(5),
        "不点开 Changes 页签时宿主角标与未选中页签均更新为 3",
        |cx| {
            let badge_ok =
                cx.update(|_, cx| api::view_badge("smelt.git/main", cx)) == Some("3".into());
            badge_ok && inactive_tab_has(cx, "Changes", Some("3"))
        },
    );

    // 3. 变更全部清空后，角标必须从宿主与未选中页签同步消失。
    {
        let mut map = repo_files.lock().unwrap();
        map.insert("/work/smelt".to_owned(), Vec::new());
        map.insert("/work/smelt/sub".to_owned(), Vec::new());
    }
    cx.update(|_, cx| {
        api::emit(
            HostEvent::ProjectChanged {
                machine: RuntimeKey::Local,
                root: "/work/smelt".into(),
            },
            cx,
        );
    });
    wait_for(
        cx,
        Duration::from_secs(5),
        "变更清零后宿主角标清空且未选中页签角标消失",
        |cx| {
            let badge_ok = cx
                .update(|_, cx| api::view_badge("smelt.git/main", cx))
                .is_none();
            badge_ok && inactive_tab_has(cx, "Changes", None)
        },
    );

    // 4. 再产生 5 条变更后，未选中页签角标必须更新为 "5"。
    {
        let mut map = repo_files.lock().unwrap();
        map.insert(
            "/work/smelt".to_owned(),
            vec![
                (" M".to_owned(), "a.rs".to_owned()),
                (" M".to_owned(), "b.rs".to_owned()),
                (" M".to_owned(), "c.rs".to_owned()),
                (" M".to_owned(), "d.rs".to_owned()),
                (" M".to_owned(), "e.rs".to_owned()),
            ],
        );
    }
    cx.update(|_, cx| {
        api::emit(
            HostEvent::ProjectChanged {
                machine: RuntimeKey::Local,
                root: "/work/smelt".into(),
            },
            cx,
        );
    });
    wait_for(
        cx,
        Duration::from_secs(5),
        "再次产生 5 条变更后未选中页签角标更新为 5",
        |cx| {
            let badge_ok =
                cx.update(|_, cx| api::view_badge("smelt.git/main", cx)) == Some("5".into());
            badge_ok && inactive_tab_has(cx, "Changes", Some("5"))
        },
    );

    // 5. 切换语言到 zh-CN 后，未选中页签标题必须更新为「变更」且保留角标 "5"，全程未点击页签。
    cx.update(|_, cx| {
        api::set_locale("zh-CN", cx);
    });
    wait_for(
        cx,
        Duration::from_secs(5),
        "切换语言后未选中页签标题更新为「变更」且保留角标 5",
        |cx| inactive_tab_has(cx, "变更", Some("5")),
    );

    shell.update(cx, |shell, cx| shell.shutdown(cx));
}
