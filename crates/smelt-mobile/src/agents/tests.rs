//! 智能体编辑与记忆：调用的线格式、编辑页投影、只发改了的字段、记忆排序。用
//! `RuntimeStream::pair()` 上只认 control 的假运行时。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use smelt_model::agent_memory::{AgentMemories as ModelMemories, MemoryEntry, MemoryListing};
use smelt_model::pi_mcp::{PiMcpServer, PiMcpTransport};
use smelt_model::pi_model_settings::{PiModelChoice, PiModelSettings, group_pi_model_choices};
use smelt_model::pi_plugin_catalog::PiPlugin;
use smelt_model::pi_service::PiConfigPaths;
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode, ControlResponse};
use smelt_runtime_client::protocol::write_json_line;
use smelt_runtime_client::{RuntimeClient, RuntimeStream};

use super::*;

const FAST: Timing = Timing {
    call_timeout: Duration::from_secs(3),
    slow_call_timeout: Duration::from_secs(3),
};

/// 按方法名回固定结果（或错误）的运行时，记下收到的每个 control 请求。
#[derive(Default)]
struct FakeRuntime {
    results: Mutex<HashMap<String, Result<Value, String>>>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl FakeRuntime {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn result(&self, method: &str, result: Value) {
        self.results
            .lock()
            .unwrap()
            .insert(method.to_string(), Ok(result));
    }

    fn fail(&self, method: &str, message: &str) {
        self.results
            .lock()
            .unwrap()
            .insert(method.to_string(), Err(message.to_string()));
    }

    fn client(self: &Arc<Self>) -> RuntimeClient {
        let runtime = Arc::clone(self);
        RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let runtime = Arc::clone(&runtime);
            std::thread::spawn(move || runtime.serve(server));
            Ok(client)
        })
    }

    fn serve(&self, server: RuntimeStream) {
        let mut line = String::new();
        if BufReader::new(&server).read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["op"], "control", "只该发 control 调用");
        let id = request["request_id"].as_str().unwrap();
        let method = request["method"].as_str().unwrap().to_string();
        self.calls
            .lock()
            .unwrap()
            .push((method.clone(), request["params"].clone()));
        let response = match self.results.lock().unwrap().get(&method) {
            Some(Ok(result)) => serde_json::to_value(ControlResponse::ok(id, result.clone())),
            Some(Err(message)) => serde_json::to_value(ControlResponse::<()>::error(
                id,
                ControlError::new(ControlErrorCode::FailedPrecondition, message.clone()),
            )),
            None => serde_json::to_value(ControlResponse::<()>::error(
                id,
                ControlError::new(ControlErrorCode::MethodNotFound, method.clone()),
            )),
        }
        .unwrap();
        let _ = write_json_line(&mut &server, &response);
    }

    fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    fn params(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, params)| params.clone())
            .collect()
    }
}

struct FakeSource(Option<CurrentRuntime>);

impl RuntimeSource for FakeSource {
    fn current(&self) -> Option<CurrentRuntime> {
        self.0.clone()
    }
}

fn connected(runtime: &Arc<FakeRuntime>) -> Agents {
    Agents::new(
        Arc::new(FakeSource(Some(CurrentRuntime {
            generation: 1,
            endpoint_id: "ep-a".into(),
            client: runtime.client(),
        }))),
        FAST,
    )
}

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: "quant".into(),
        name: "量化".into(),
        engine_kind_id: "pi".into(),
        prompt: "  先看数据\n".into(),
        plugins: vec!["skill:charts".into()],
        mcp_servers: vec!["notion".into()],
        context_folders: vec!["/data".into()],
        context_links: vec!["https://example.com".into()],
        model_provider: "anthropic".into(),
        model_id: "opus".into(),
        memory_enabled: true,
        ..AgentDefinition::default()
    }
}

fn agent_result(agent: &AgentDefinition) -> Value {
    json!({ "agent": agent })
}

fn pi_config(mcp: Result<Vec<PiMcpServer>, String>) -> Value {
    pi_config_with(Ok(PiModelSettings::default()), mcp)
}

fn pi_config_with(
    model_settings: Result<PiModelSettings, String>,
    mcp: Result<Vec<PiMcpServer>, String>,
) -> Value {
    let choice = |provider: &str, provider_name: &str, id: &str, name: &str| PiModelChoice {
        provider: provider.into(),
        provider_name: provider_name.into(),
        id: id.into(),
        name: name.into(),
    };
    serde_json::to_value(PiConfig {
        model_settings,
        settings_summary: Err("没读".into()),
        mcp_servers: mcp,
        plugins: vec![
            PiPlugin {
                id: "skill:charts".into(),
                kind: PiPluginKind::Skill,
                name: "charts".into(),
                description: "画图".into(),
                path: PathBuf::from("/p/charts"),
                origin: "user".into(),
                broken: None,
                builtin: false,
            },
            PiPlugin {
                id: "extension:gone".into(),
                kind: PiPluginKind::Extension,
                name: "gone".into(),
                description: String::new(),
                path: PathBuf::from("/p/gone.ts"),
                origin: "user".into(),
                broken: Some("软链断了".into()),
                builtin: false,
            },
        ],
        model_choice_groups: group_pi_model_choices(vec![
            choice("anthropic", "Anthropic", "opus", "Claude Opus"),
            choice("local", "Local", "qwen", "qwen"),
        ]),
        auto_provider_ids: Vec::new(),
        paths: PiConfigPaths::default(),
    })
    .unwrap()
}

fn notion() -> PiMcpServer {
    PiMcpServer {
        name: "notion".into(),
        transport: PiMcpTransport::Http {
            url: "https://mcp.notion.com".into(),
        },
        enabled: false,
        description: Some("笔记".into()),
        signed_in: true,
    }
}

// ---------------------------------------------------------------------------
// 编辑页
// ---------------------------------------------------------------------------

#[test]
fn calls_without_a_runtime_are_offline() {
    let agents = Agents::new(Arc::new(FakeSource(None)), FAST);
    assert_eq!(
        agents.editor(None).unwrap_err().kind,
        SmeltErrorKind::Offline
    );
    assert_eq!(
        agents.memories("quant").unwrap_err().kind,
        SmeltErrorKind::Offline
    );
}

#[test]
fn editor_shows_the_definition_and_what_this_machine_offers() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.get", agent_result(&definition()));
    runtime.result("pi.config.get", pi_config(Ok(vec![notion()])));

    let editor = connected(&runtime).editor(Some("quant")).unwrap();

    assert_eq!(
        runtime.params("agent.definition.get"),
        [json!({"id": "quant"})]
    );
    assert_eq!(editor.draft.name, "量化");
    assert_eq!(editor.draft.prompt, "  先看数据\n", "长期指令原样给出");
    assert_eq!(
        (
            editor.draft.model_provider.as_str(),
            editor.draft.model_id.as_str()
        ),
        ("anthropic", "opus")
    );
    let choices = &editor.choices;
    let labels: Vec<&str> = choices
        .models
        .as_deref()
        .unwrap()
        .iter()
        .flat_map(|group| group.models.iter().map(|m| m.label.as_str()))
        .collect();
    assert_eq!(labels, ["Anthropic / Claude Opus", "Local / qwen"]);
    assert_eq!(
        choices.plugins.as_deref().unwrap()[1],
        PluginOption {
            id: "extension:gone".into(),
            kind: PluginKind::Extension,
            name: "gone".into(),
            description: String::new(),
            broken: Some("软链断了".into()),
        }
    );
    assert_eq!(
        choices.mcp_servers.as_deref().unwrap(),
        [McpOption {
            name: "notion".into(),
            description: Some("笔记".into()),
            enabled: false,
        }]
    );
    assert_eq!(choices.error, None);
}

#[test]
fn a_new_agent_starts_blank_with_memory_on() {
    let runtime = FakeRuntime::new();
    runtime.result("pi.config.get", pi_config(Ok(Vec::new())));

    let editor = connected(&runtime).editor(None).unwrap();

    assert_eq!(runtime.methods(), ["pi.config.get"], "新建不读定义");
    assert_eq!(
        editor.draft,
        AgentDraft {
            memory_enabled: true,
            ..AgentDraft::default()
        }
    );
}

#[test]
fn editor_still_opens_when_the_capability_library_cannot_be_read() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.get", agent_result(&definition()));
    runtime.fail("pi.config.get", "Pi 没装");

    let editor = connected(&runtime).editor(Some("quant")).unwrap();

    assert_eq!(editor.draft.name, "量化");
    // 没读到 ≠ 这台机器上没有：都是 None，界面不能把选了的标成「已经没有了」。
    let choices = editor.choices;
    assert_eq!(
        (choices.models, choices.plugins, choices.mcp_servers),
        (None, None, None)
    );
    assert_eq!(choices.error.as_deref(), Some("Pi 没装"));
}

#[test]
fn a_broken_config_file_marks_only_its_part_as_unread() {
    let runtime = FakeRuntime::new();
    runtime.result(
        "pi.config.get",
        pi_config_with(
            Err("models.json 不是合法 JSON".into()),
            Err("mcp.json 不是合法 JSON".into()),
        ),
    );

    let choices = connected(&runtime).choices().unwrap();

    assert_eq!(runtime.methods(), ["pi.config.get"], "只取选项不读定义");
    assert!(
        choices.models.is_none(),
        "models.json 坏了，可选模型只剩一部分，当没读到"
    );
    assert!(choices.mcp_servers.is_none());
    assert_eq!(choices.plugins.map(|p| p.len()), Some(2), "插件照常");
    assert_eq!(
        choices.error.as_deref(),
        Some("mcp.json 不是合法 JSON；models.json 不是合法 JSON")
    );

    // 真的一个 MCP 都没有：是空列表，不是 None。
    runtime.result("pi.config.get", pi_config(Ok(Vec::new())));
    let choices = connected(&runtime).choices().unwrap();
    assert_eq!(choices.mcp_servers, Some(Vec::new()));
    assert_eq!(choices.error, None);
}

// ---------------------------------------------------------------------------
// 保存
// ---------------------------------------------------------------------------

#[test]
fn create_trims_the_draft_and_leaves_the_model_to_the_default() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.create", agent_result(&definition()));
    let agents = connected(&runtime);

    let summary = agents
        .create(AgentDraft {
            name: "  量化 ".into(),
            prompt: "  先看数据\n".into(),
            plugins: vec![" skill:charts ".into(), "".into()],
            model_provider: "anthropic".into(),
            model_id: " ".into(),
            memory_enabled: false,
            ..AgentDraft::default()
        })
        .unwrap();

    assert_eq!(summary.id, "quant");
    assert_eq!(
        runtime.params("agent.definition.create"),
        [json!({
            "name": "量化",
            "prompt": "  先看数据\n",
            "plugins": ["skill:charts"],
            "memory_enabled": false,
        })],
        "模型只填了一半就当没填；空项去掉"
    );

    let unnamed = agents.create(AgentDraft {
        name: "  ".into(),
        ..AgentDraft::default()
    });
    assert_eq!(unnamed.unwrap_err().message, "智能体要有名字");
    assert_eq!(
        runtime.params("agent.definition.create").len(),
        1,
        "名字空不发"
    );
}

#[test]
fn update_sends_only_the_fields_that_changed() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.update", agent_result(&definition()));
    let agents = connected(&runtime);
    let original = draft_of(&definition());

    let mut edited = original.clone();
    edited.prompt = "先看数据，再下结论".into();
    edited.model_id = "sonnet".into();
    edited.context_folders.push("/reports".into());
    agents.update("quant", &original, &edited).unwrap();

    assert_eq!(
        runtime.params("agent.definition.update"),
        [json!({
            "id": "quant",
            "prompt": "先看数据，再下结论",
            "context_folders": ["/data", "/reports"],
            "model_provider": "anthropic",
            "model_id": "sonnet",
        })],
        "模型成对发；没改的名字、插件、MCP、链接、记忆开关都不发"
    );
}

#[test]
fn whitespace_in_stored_lists_does_not_count_as_a_change() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.update", agent_result(&definition()));
    // 命令行写进去的条目带空白：手机上只改名字，不能把整张插件列表也发出去。
    let original = AgentDraft {
        plugins: vec![" skill:charts ".into(), "".into()],
        ..draft_of(&definition())
    };
    let edited = AgentDraft {
        name: "量化研究".into(),
        ..original.clone()
    };

    connected(&runtime)
        .update("quant", &original, &edited)
        .unwrap();

    assert_eq!(
        runtime.params("agent.definition.update"),
        [json!({"id": "quant", "name": "量化研究"})]
    );
}

#[test]
fn reordered_plugins_are_not_a_change() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.get", agent_result(&definition()));
    let original = AgentDraft {
        plugins: vec!["skill:a".into(), "skill:b".into()],
        ..draft_of(&definition())
    };
    let edited = AgentDraft {
        plugins: vec!["skill:b".into(), "skill:a".into()],
        ..original.clone()
    };

    connected(&runtime)
        .update("quant", &original, &edited)
        .unwrap();

    assert_eq!(runtime.methods(), ["agent.definition.get"], "没改动只重读");
}

#[test]
fn clearing_the_model_sends_both_halves_empty() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.update", agent_result(&definition()));
    let original = draft_of(&definition());
    let edited = AgentDraft {
        model_provider: String::new(),
        model_id: String::new(),
        ..original.clone()
    };

    connected(&runtime)
        .update("quant", &original, &edited)
        .unwrap();

    assert_eq!(
        runtime.params("agent.definition.update"),
        [json!({"id": "quant", "model_provider": "", "model_id": ""})]
    );
}

#[test]
fn saving_without_changes_only_rereads_the_definition() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.definition.get", agent_result(&definition()));
    let original = draft_of(&definition());

    let summary = connected(&runtime)
        .update("quant", &original, &original)
        .unwrap();

    assert_eq!(runtime.methods(), ["agent.definition.get"]);
    assert_eq!(summary.name, "量化");
}

#[test]
fn delete_reports_why_the_runtime_refused() {
    let runtime = FakeRuntime::new();
    runtime.fail(
        "agent.definition.delete",
        "请先删除该智能体关联的 2 条自动化",
    );

    let refused = connected(&runtime).delete("quant").unwrap_err();

    assert_eq!(refused.message, "请先删除该智能体关联的 2 条自动化");
    assert_eq!(
        runtime.params("agent.definition.delete"),
        [json!({"id": "quant"})]
    );
}

// ---------------------------------------------------------------------------
// 记忆
// ---------------------------------------------------------------------------

fn entry(name: &str, kind: &str, used_secs: Option<u64>, pending: bool) -> MemoryEntry {
    MemoryEntry {
        file_name: format!("{name}.md"),
        name: name.into(),
        description: format!("{name} 的描述"),
        kind: Some(kind.into()),
        path: PathBuf::from(format!("/m/{name}.md")),
        read_count: 2,
        last_used: used_secs.map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
        stale: false,
        pending_reason: pending.then(|| "读过外部网页".into()),
        body: "正文".into(),
    }
}

fn memories_result() -> Value {
    serde_json::to_value(ModelMemories {
        agent: MemoryListing {
            entries: vec![
                entry("old", "project", Some(100), false),
                entry("never", "reference", None, false),
                entry("recent", "feedback", Some(200), false),
                entry("unsure", "project", Some(50), true),
            ],
            core_over_budget: false,
        },
        user: MemoryListing {
            entries: vec![entry("me", "user", Some(10), true)],
            core_over_budget: true,
        },
    })
    .unwrap()
}

#[test]
fn memories_put_pending_first_then_the_recently_used() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.memory.list", memories_result());

    let memories = connected(&runtime).memories("quant").unwrap();

    assert_eq!(
        runtime.params("agent.memory.list"),
        [json!({"id": "quant"})]
    );
    let names: Vec<&str> = memories
        .agent
        .items
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(names, ["unsure", "recent", "old", "never"]);
    assert_eq!(memories.pending, 2, "两层加起来");
    let recent = &memories.agent.items[1];
    assert!(recent.rule, "feedback 是规则");
    assert_eq!(recent.last_used_ms, Some(200_000));
    assert!(!memories.agent.items[2].rule, "project 是事实");
    assert!(memories.user.core_over_budget);
}

#[test]
fn approving_a_memory_returns_the_refreshed_layers() {
    let runtime = FakeRuntime::new();
    runtime.result("agent.memory.approve", json!({}));
    runtime.result("agent.memory.delete", json!({}));
    runtime.result("agent.memory.list", memories_result());
    let agents = connected(&runtime);

    let memories = agents
        .approve_memory("quant", MemoryLayer::User, "me.md")
        .unwrap();
    agents
        .delete_memory("quant", MemoryLayer::Agent, "old.md")
        .unwrap();

    assert_eq!(
        runtime.methods(),
        [
            "agent.memory.approve",
            "agent.memory.list",
            "agent.memory.delete",
            "agent.memory.list"
        ]
    );
    assert_eq!(
        runtime.params("agent.memory.approve"),
        [json!({"agent_id": "quant", "layer": "user", "file_name": "me.md"})]
    );
    assert_eq!(
        runtime.params("agent.memory.delete"),
        [json!({"agent_id": "quant", "layer": "agent", "file_name": "old.md"})]
    );
    assert_eq!(memories.user.items[0].name, "me");
}
