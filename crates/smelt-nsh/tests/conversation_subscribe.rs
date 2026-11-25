//! `<SmeltConversation>` → `acp.subscribe` 线格式端到端回归测试（独立进程，避开 `nsh_platform_gpui::rt::JOBS` `OnceLock`）：
//! 真实在 QuickJS 沙箱里挂载 `smelt/sdk` 导出的 `<SmeltConversation>`，把 JS 发出的 `acp.subscribe`
//! 原样交给 `serde_json::from_value::<smelt_acp::ConversationClientLaunch>` 反序列化，验证：
//! 1. 工作区会话（`directoryAcp`）默认 `conversation_binding` 为 `{ "type": "direct" }`（`ConversationBinding::Direct`），
//!    字符串 `pending_prompt` 正确组装为 `AcpInitialPrompt`，`start.fork.cut` 正确解码为 `AcpForkCut`；
//! 2. 自动化现场会话（`spec.binding`）正确解码为 `ConversationBinding::Automation { run_id }`；
//! 3. 宿主 `smelt.state` 推进导致 `directoryAcp` 对象引用刷新但会话 id 未变时，不触发无意义的退订重订。

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::TestAppContext;
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{AppOptions, AppView, Limits};
use serde_json::{Value, json};
use smelt_acp::ConversationClientLaunch;
use smelt_nsh::plugins::{PluginHost, Plugins};
use smelt_plugin_api::model::{acp_conn::AcpForkCut, conversation::ConversationBinding};

const SHELL: &str = r#"
import { View } from "nsh";
import { Slot } from "./slots.tsx";

export function Shell() {
  return (
    <View style={{ flex: 1 }}>
      <Slot id="probe" />
    </View>
  );
}
"#;

const PLUGIN: &str = r#"
import { host, signal, View } from "nsh";
import { SmeltConversation } from "smelt/conversation";

const mode = signal<"workspace" | "automation">("workspace");
const acpRev = signal(0);

host.on("test.bump", () => acpRev.set(acpRev() + 1));
host.on("test.mode", (next: any) => mode.set(next === "automation" ? "automation" : "workspace"));

export default function Probe() {
  const rev = () => acpRev();
  const dirAcp = () => ({
    sid: "acp-ws-1",
    agent: "pi",
    cwd: "/repo",
    launch: { command: "pi", env: { FOO: "bar" } },
    history_session_id: null,
    config_values: [["model", "anthropic/sonnet"]],
    pending_prompt: "先检查这里",
    pending_agent_preset: "strict",
    agent_definition_id: null,
    session_title: `rev-${rev()}`,
  });

  return (
    <View style={{ flex: 1, width: 800, height: 600 }}>
      <If
        when={mode() === "workspace"}
        else={
          <SmeltConversation
            machine={null}
            sessionId="acp-auto-1"
            spec={{
              agent: "pi",
              cwd: "/repo",
              binding: { type: "automation", run_id: "run-42" },
            }}
            embedded={true}
          />
        }
      >
        <SmeltConversation
          machine={null}
          sessionId="acp-ws-1"
          directoryAcp={dirAcp()}
          start={{
            fork: {
              provider_session_id: "prov-1",
              cut: { text: "retry me", occurrence: 1 },
            },
            prefill: null,
          }}
          embedded={false}
          title="新对话"
        />
      </If>
    </View>
  );
}
"#;

#[derive(Debug, Clone)]
struct SubscribeRecord {
    sub_id: String,
    raw_launch: Value,
    parsed: Result<ConversationClientLaunch, String>,
}

#[gpui::test]
fn conversation_subscribe_wire_contract(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    nsh_platform_gpui::set_ui_sets(vec![
        nsh_base::registry(),
        smelt_nsh::testing::host_registry(),
    ]);

    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conversation_subscribe");
    let _ = fs::remove_dir_all(&root);

    let host = PluginHost::new();
    host.set_plugin_limits(Limits {
        memory_bytes: 32 << 20,
        load_budget: Duration::from_millis(250),
        call_budget: Duration::from_millis(120),
        ..Limits::default()
    });

    let subscribes: Arc<Mutex<Vec<SubscribeRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let unsubscribes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let app_errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    host.handle("state", |_plugin, call| {
        call.resolve(json!({
            "revision": 1,
            "locale": "zh-CN",
            "theme": smelt_nsh::testing::theme_state(16.),
            "machines_meta": [],
            "sessions_data": [],
        }));
    });
    host.handle("call", |_plugin, call| {
        let data = call.data();
        if data["method"].as_str() == Some("session.launch.prepare") {
            let launch = data["params"]["launch"].clone();
            call.resolve(json!({
                "launch": launch,
                "skills": [],
                "mcp_servers": [],
            }));
        } else {
            call.resolve(Value::Null);
        }
    });
    {
        let subscribes = Arc::clone(&subscribes);
        host.handle("acp.subscribe", move |_plugin, mut call| {
            let mut data = call.take_data();
            let sub_id = data["sub_id"].as_str().unwrap_or_default().to_owned();
            let raw_launch = data["launch"].take();
            let parsed = serde_json::from_value::<ConversationClientLaunch>(raw_launch.clone())
                .map_err(|e| format!("acp.subscribe：launch 参数不对：{e}"));
            subscribes.lock().unwrap().push(SubscribeRecord {
                sub_id,
                raw_launch,
                parsed: parsed.clone(),
            });
            match parsed {
                Ok(_) => call.resolve(Value::Null),
                Err(msg) => call.reject(msg),
            }
        });
    }
    {
        let unsubscribes = Arc::clone(&unsubscribes);
        host.handle("acp.unsubscribe", move |_plugin, call| {
            let sub_id = call.data()["sub_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            unsubscribes.lock().unwrap().push(sub_id);
            call.resolve(Value::Null);
        });
    }

    let (view, cx) = cx.add_window_view(|window, cx| {
        AppView::new(
            AppOptions {
                inline_root: true,
                ..AppOptions::default()
            },
            window,
            cx,
        )
    });
    {
        let app_errors = Arc::clone(&app_errors);
        let err_host = host.clone();
        view.update(cx, |view, _| {
            view.on_app_error(move |error| {
                app_errors.lock().unwrap().push(error.to_string());
                err_host.on_app_error(error);
            });
        });
    }

    let ui_sets = vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(smelt_nsh::testing::host_registry()),
    ];
    let mut plugins = Plugins::new(&root.join("stage"), SHELL, ui_sets).unwrap();
    let repo_plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    plugins.set_modules(smelt_nsh::discover_module_packages(&[
        smelt_nsh::SourceDir {
            path: repo_plugins,
            source: smelt_nsh::PluginSource::Builtin,
            watch: false,
        },
    ]));
    let plugin_dir = root.join("sources/probe");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(plugin_dir.join("main.tsx"), PLUGIN).unwrap();
    plugins
        .install_entry(
            "probe",
            &plugin_dir,
            "main.tsx",
            &["smelt/conversation".to_owned()],
        )
        .unwrap();

    let start = plugins.start();
    view.update(cx, |view, _| host.apply(view, start));

    let wait_until = |cx: &mut gpui::VisualTestContext, what: &str, done: &dyn Fn() -> bool| {
        let begin = Instant::now();
        loop {
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            if done() {
                return;
            }
            assert!(
                begin.elapsed() < Duration::from_secs(10),
                "等待 {what} 超时；errors={:?}",
                app_errors.lock().unwrap(),
            );
            // 轮询例外：测试平台无引擎帧完成事件；周期 10ms，上限 10s。
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    // 1. 工作区会话订阅：必须反序列化为 ConversationClientLaunch（Direct + AcpInitialPrompt + AcpForkCut）。
    wait_until(cx, "workspace acp.subscribe", &|| {
        !subscribes.lock().unwrap().is_empty()
    });
    let first = subscribes.lock().unwrap()[0].clone();
    let ws_launch = first.parsed.unwrap_or_else(|err| {
        panic!(
            "workspace launch 反序列化失败：{err}\nraw={}",
            first.raw_launch
        )
    });
    assert_eq!(ws_launch.id, "acp-ws-1");
    assert_eq!(ws_launch.conversation_binding, ConversationBinding::Direct);
    assert_eq!(
        ws_launch.initial_prompt.as_ref().map(|p| p.text.as_str()),
        Some("先检查这里"),
        "WorkspaceAcp.pending_prompt 字符串应映射为 AcpInitialPrompt",
    );
    assert_eq!(
        ws_launch.fork_cut,
        Some(AcpForkCut {
            text: "retry me".into(),
            occurrence: 1,
        })
    );
    assert_eq!(ws_launch.pending_agent_preset.as_deref(), Some("strict"));
    assert_eq!(
        ws_launch.initial_config,
        vec![("model".to_owned(), "anthropic/sonnet".to_owned())]
    );

    // 1b. 收到空对话就绪快照（新建对话首帧）应渲染欢迎态且不抛出 JS 运行时异常。
    host.send_to(
        "probe",
        "smelt.acp.event",
        json!({
            "sub_id": first.sub_id,
            "kind": "snapshot",
            "snapshot": {
                "phase": "ready",
                "replaying_history": false,
                "entries": [],
                "entries_offset": 0,
                "entries_total": 0,
                "snapshot_revision": 1,
                "config_options": [],
                "model": null,
            },
        }),
    );
    for _ in 0..8 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        // 轮询例外：渲染错误异步通过 on_app_error 送达，无独立完成事件；共 80ms。
        std::thread::sleep(Duration::from_millis(10));
    }
    {
        let errs = app_errors.lock().unwrap();
        assert!(errs.is_empty(), "空对话渲染失败：{errs:?}");
    }
    assert!(
        unsubscribes.lock().unwrap().is_empty(),
        "空对话欢迎态渲染不应触发 ErrorBoundary 卸载并退订",
    );

    // 1c. 推进运行态与等待确认态再回落就绪态：验证 Composer 原生 Animated 扫光光带（sweep -> hold -> off）
    //     在真实 QuickJS + GPUI 下挂载、动画驱动、onLayout 与卸载全程不抛错。
    for (rev, phase, perms) in [
        (2, "thinking", json!([])),
        (
            3,
            "awaiting_approval",
            json!([{
                "id": "perm-1",
                "tool_call_id": "tc-1",
                "question": "Run?",
                "options": [{ "id": "allow", "label": "Allow", "kind": "allow_once" }],
            }]),
        ),
        (4, "ready", json!([])),
    ] {
        host.send_to(
            "probe",
            "smelt.acp.event",
            json!({
                "sub_id": first.sub_id,
                "kind": "snapshot",
                "snapshot": {
                    "phase": phase,
                    "replaying_history": false,
                    "entries": [],
                    "entries_offset": 0,
                    "entries_total": 0,
                    "snapshot_revision": rev,
                    "pending_permissions": perms,
                    "config_options": [],
                    "model": null,
                },
            }),
        );
        for _ in 0..6 {
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            // 轮询例外：渲染错误与动画队列异步在 GPUI 帧中推进，无独立完成事件；每阶段共 60ms。
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    {
        let errs = app_errors.lock().unwrap();
        assert!(errs.is_empty(), "Composer 运行态动效渲染失败：{errs:?}");
    }
    assert!(
        unsubscribes.lock().unwrap().is_empty(),
        "Composer 运行态动效切换不应触发 ErrorBoundary 卸载并退订",
    );

    // 2. directoryAcp 对象引用刷新（如 session_title 更新）不应触发重复退订/重订。
    host.broadcast("test.bump", Value::Null);
    for _ in 0..8 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        // 轮询例外：证明「不再触发重订」无事件可等；共 80ms。
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        subscribes.lock().unwrap().len(),
        1,
        "directoryAcp 引用变化不应导致重复 acp.subscribe"
    );
    assert!(
        unsubscribes.lock().unwrap().is_empty(),
        "directoryAcp 引用变化不应触发 acp.unsubscribe"
    );

    // 3. 切到自动化现场会话：spec.binding 应正确反序列化为 Automation { run_id }。
    host.broadcast("test.mode", json!("automation"));
    wait_until(cx, "automation acp.subscribe", &|| {
        subscribes.lock().unwrap().len() >= 2
    });
    let second = subscribes.lock().unwrap()[1].clone();
    let auto_launch = second.parsed.unwrap_or_else(|err| {
        panic!(
            "automation launch 反序列化失败：{err}\nraw={}",
            second.raw_launch
        )
    });
    assert_eq!(auto_launch.id, "acp-auto-1");
    assert_eq!(
        auto_launch.conversation_binding,
        ConversationBinding::Automation {
            run_id: "run-42".into(),
        }
    );
    assert!(auto_launch.initial_prompt.is_none());
    let errs = app_errors.lock().unwrap();
    assert!(errs.is_empty(), "unexpected app_errors: {errs:?}");
}
