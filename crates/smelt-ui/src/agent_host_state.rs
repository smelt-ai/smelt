//! 智能体宿主状态：每台机器上运行时那部分配置（启动命令、env、profile、智能体定义、
//! hook 开关），加上客户端自己的通知偏好和自动化投影。
//!
//! 需要 `gpui::Global`，所以不能进不许引 GPUI 的 smelt-core。设置页怎么渲染
//! 这些字段仍在主 crate 的 settings 模块。

use gpui::{App, AsyncApp, Global};
use std::collections::{BTreeMap, HashSet};

use crate::automation::{Automation, AutomationRun, AutomationState};
pub use smelt_client::runtime::RuntimeKey;
pub use smelt_model::agent_definition::AgentDefinition;
use smelt_model::agent_host::{AgentHostEdit, AgentHostSettings};
use smelt_model::automation::AutomationFile;
use smelt_model::control_api::{AgentHostApplyEdit, AgentHostGet};
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::EmptyParams;

/// 读写运行时配置的上限。一次合并写库，正常是毫秒级。
const RUNTIME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 客户端自己的那部分：通知偏好（每台设备可以不同）和自动化投影。运行时那部分按机器
/// 放在 [`AgentHosts`]。
#[derive(Clone)]
pub struct AgentHostState {
    pub notify_approval: bool,
    pub notify_input: bool,
    pub notify_success: bool,
    pub notify_failure: bool,
    pub notify_terminal_bell: bool,
    /// daemon 自动化投影；GUI 不据此调度。
    pub automations: Vec<Automation>,
    pub automation_store_id: String,
    pub retired_automation_store_ids: HashSet<String>,
    pub automation_revision: u64,
    pub automation_states: Vec<AutomationState>,
    pub automation_runs: Vec<AutomationRun>,
    pub automation_store_error: Option<String>,
    pub webhook_base_url: Option<String>,
    /// 通知偏好写不进客户端库时的原因。
    pub persistence_error: Option<String>,
}

impl AgentHostState {
    pub fn automations_for<'a>(&'a self, agent_id: &str) -> Vec<&'a Automation> {
        self.automations
            .iter()
            .filter(|automation| automation.agent_definition_id() == Some(agent_id))
            .collect()
    }

    pub fn automation_state_for(&self, automation_id: &str) -> Option<&AutomationState> {
        self.automation_states
            .iter()
            .find(|state| state.automation_id == automation_id)
    }

    pub fn active_automation_run(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.automation_runs
            .iter()
            .find(|run| run.automation_id == automation_id && run.blocks_overlap())
    }

    pub fn latest_automation_run(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.automation_runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .max_by_key(|run| (run.created_at, run.id.as_str()))
    }

    pub fn automation_runs_for(&self, automation_id: &str) -> Vec<&AutomationRun> {
        let mut runs = self
            .automation_runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse((run.created_at, run.id.as_str())));
        runs
    }

    pub fn is_retired_automation_store(&self, store_id: &str) -> bool {
        self.retired_automation_store_ids.contains(store_id)
    }

    pub fn replace_automation_snapshot(&mut self, snapshot: AutomationFile) -> bool {
        if snapshot.store_id == self.automation_store_id {
            if snapshot.revision < self.automation_revision {
                return false;
            }
            if snapshot.revision == self.automation_revision
                && snapshot.automations == self.automations
                && snapshot.states == self.automation_states
                && snapshot.runs == self.automation_runs
                && snapshot.store_error == self.automation_store_error
                && snapshot.webhook_base_url == self.webhook_base_url
            {
                return false;
            }
        } else {
            if self
                .retired_automation_store_ids
                .contains(&snapshot.store_id)
            {
                return false;
            }
            if !self.automation_store_id.is_empty() {
                self.retired_automation_store_ids
                    .insert(self.automation_store_id.clone());
            }
        }
        self.automation_store_id = snapshot.store_id;
        self.automation_revision = snapshot.revision;
        self.automations = snapshot.automations;
        self.automation_states = snapshot.states;
        self.automation_runs = snapshot.runs;
        self.automation_store_error = snapshot.store_error;
        self.webhook_base_url = snapshot.webhook_base_url;
        true
    }

    fn notify_prefs(&self) -> NotifyPrefs {
        NotifyPrefs {
            approval: self.notify_approval,
            input: self.notify_input,
            success: self.notify_success,
            failure: self.notify_failure,
            terminal_bell: self.notify_terminal_bell,
        }
    }

    fn set_notify_prefs(&mut self, prefs: NotifyPrefs) {
        self.notify_approval = prefs.approval;
        self.notify_input = prefs.input;
        self.notify_success = prefs.success;
        self.notify_failure = prefs.failure;
        self.notify_terminal_bell = prefs.terminal_bell;
    }
}

impl Default for AgentHostState {
    fn default() -> Self {
        Self {
            notify_approval: true,
            notify_input: true,
            notify_success: true,
            notify_failure: true,
            notify_terminal_bell: true,
            automations: Vec::new(),
            automation_store_id: String::new(),
            retired_automation_store_ids: HashSet::new(),
            automation_revision: 0,
            automation_states: Vec::new(),
            automation_runs: Vec::new(),
            automation_store_error: None,
            webhook_base_url: None,
            persistence_error: None,
        }
    }
}

impl Global for AgentHostState {}

/// 通知怎么提醒是客户端自己的事（设计文档 D1），每台设备可以不同，不交给运行时。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NotifyPrefs {
    approval: bool,
    input: bool,
    success: bool,
    failure: bool,
    terminal_bell: bool,
}

/// 读客户端库里的通知偏好；没有时用出厂值。
fn load_notify_prefs() -> NotifyPrefs {
    match smelt_client_store::client_store().and_then(|store| store.notify_prefs()) {
        Ok(Some(record)) => NotifyPrefs {
            approval: record.approval,
            input: record.input,
            success: record.success,
            failure: record.failure,
            terminal_bell: record.terminal_bell,
        },
        Ok(None) => AgentHostState::default().notify_prefs(),
        Err(error) => {
            eprintln!("[settings] 读取通知偏好失败，使用默认值：{error}");
            AgentHostState::default().notify_prefs()
        }
    }
}

fn save_notify_prefs(prefs: NotifyPrefs) -> Result<(), String> {
    smelt_client_store::client_store()
        .and_then(|store| {
            store.put_notify_prefs(&smelt_client_store::NotifyPrefsRecord {
                approval: prefs.approval,
                input: prefs.input,
                success: prefs.success,
                failure: prefs.failure,
                terminal_bell: prefs.terminal_bell,
            })
        })
        .map_err(|error| error.to_string())
}

/// 启动时的初始状态：客户端的通知偏好立刻可用。
pub fn initial_agent_host_state() -> AgentHostState {
    let mut state = AgentHostState::default();
    state.set_notify_prefs(load_notify_prefs());
    state
}

/// 改通知偏好：先写客户端库，写成了才换上。
pub fn apply_notify_prefs(f: impl FnOnce(&mut AgentHostState), cx: &mut App) {
    let current = cx.global::<AgentHostState>().clone();
    let mut candidate = current.clone();
    f(&mut candidate);
    if candidate.notify_prefs() == current.notify_prefs() {
        return;
    }
    match save_notify_prefs(candidate.notify_prefs()) {
        Ok(()) => candidate.persistence_error = None,
        Err(error) => {
            eprintln!("[agent-ui] 保存通知偏好失败: {error}");
            candidate = current;
            candidate.persistence_error = Some(format!("保存通知偏好失败：{error}"));
        }
    }
    cx.set_global(candidate);
}

// ===================== 运行时那部分：按机器缓存 + 异步提交 =====================

/// 一台机器上的智能体配置。
#[derive(Clone)]
pub struct MachineAgentHost {
    client: RuntimeClient,
    /// 运行时给的权威配置。还没读到是 `None`：这时不能拿去启动会话，也不接受修改。
    settings: Option<AgentHostSettings>,
    /// 最近一次读取或保存失败的原因。
    pub error: Option<String>,
}

impl MachineAgentHost {
    pub fn settings(&self) -> Option<&AgentHostSettings> {
        self.settings.as_ref()
    }
}

/// 每台挂着的机器各一份智能体配置。本机和远程机器同一套：读、改都按 `RuntimeKey`。
#[derive(Clone, Default)]
pub struct AgentHosts {
    machines: BTreeMap<RuntimeKey, MachineAgentHost>,
}

impl Global for AgentHosts {}

impl AgentHosts {
    pub fn machine(&self, key: &RuntimeKey) -> Option<&MachineAgentHost> {
        self.machines.get(key)
    }

    /// 那台机器的配置；没挂或还没读到是 `None`。
    pub fn settings(&self, key: &RuntimeKey) -> Option<&AgentHostSettings> {
        self.machines.get(key)?.settings.as_ref()
    }

    /// 那台机器上的智能体定义；没挂或还没读到是空的。
    pub fn agents(&self, key: &RuntimeKey) -> &[AgentDefinition] {
        self.settings(key)
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default()
    }

    pub fn agent(&self, key: &RuntimeKey, id: &str) -> Option<&AgentDefinition> {
        self.settings(key)?.find_agent(id)
    }

    /// 挂着的机器，本机在前。
    pub fn keys(&self) -> impl Iterator<Item = &RuntimeKey> {
        self.machines.keys()
    }

    fn adopt(&mut self, key: &RuntimeKey, settings: AgentHostSettings) {
        if let Some(machine) = self.machines.get_mut(key) {
            machine.settings = Some(settings);
            machine.error = None;
        }
    }

    fn set_error(&mut self, key: &RuntimeKey, error: String) {
        if let Some(machine) = self.machines.get_mut(key) {
            machine.error = Some(error);
        }
    }
}

/// 读到的那台机器的配置。没挂这个全局（测试、启动早期）时当作什么都没有。
pub fn agent_hosts(cx: &App) -> Option<&AgentHosts> {
    cx.try_global::<AgentHosts>()
}

/// 那台机器的配置读到了没有。没读到之前，依赖它的启动和恢复要等。
pub fn agent_host_loaded(key: &RuntimeKey, cx: &App) -> bool {
    agent_hosts(cx).is_some_and(|hosts| hosts.settings(key).is_some())
}

/// 那台机器的配置不用再等了：已经读到，或者机器没挂（离线的远程机器连上才挂，等不来）。
/// 会话恢复用它决定要不要等配置再拼启动命令。
pub fn agent_host_settled(key: &RuntimeKey, cx: &App) -> bool {
    agent_hosts(cx)
        .and_then(|hosts| hosts.machine(key))
        .is_none_or(|machine| machine.settings.is_some())
}

fn fetch_agent_host(client: &RuntimeClient) -> Result<AgentHostSettings, String> {
    client
        .call::<AgentHostGet>(&EmptyParams::default(), RUNTIME_TIMEOUT)
        .map_err(|error| error.to_string())
}

fn update_hosts(cx: &mut App, change: impl FnOnce(&mut AgentHosts)) {
    if !cx.has_global::<AgentHosts>() {
        cx.set_global(AgentHosts::default());
    }
    change(cx.global_mut::<AgentHosts>());
}

/// 挂上一台机器。已经挂着的只换客户端（比如改了中继），配置不重读，返回 `false`；新挂上
/// 的返回 `true`，由调用方接着用 [`load_agent_host`] 读它的配置——什么时候读（等那台机器
/// 在线）是主 crate 的事，这里看不到机器的连通性。
pub fn register_agent_host(key: &RuntimeKey, client: RuntimeClient, cx: &mut App) -> bool {
    let mut fresh = false;
    update_hosts(cx, |hosts| {
        if let Some(machine) = hosts.machines.get_mut(key) {
            machine.client = client;
        } else {
            fresh = true;
            hosts.machines.insert(
                key.clone(),
                MachineAgentHost {
                    client,
                    settings: None,
                    error: None,
                },
            );
        }
    });
    fresh
}

/// 读一次配置的结果，见 [`load_agent_host`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentHostLoad {
    /// 读到了，已经存上。
    Loaded,
    /// 没读到。
    Failed,
    /// 机器已经移除（[`detach_agent_host`]），不用再读。
    Detached,
}

/// 在后台读一次那台机器的配置，读到就存上。没读到时 `report` 为真就把原因记在机器上给人
/// 看；运行时刚被拉起、正在升级时读不到是常态，调用方先安静重试几次再报。
pub async fn load_agent_host(key: &RuntimeKey, report: bool, cx: &AsyncApp) -> AgentHostLoad {
    let Some(client) = cx.update(|cx| {
        agent_hosts(cx)
            .and_then(|hosts| hosts.machine(key))
            .map(|machine| machine.client.clone())
    }) else {
        return AgentHostLoad::Detached;
    };
    let fetched = cx
        .background_executor()
        .spawn(async move { fetch_agent_host(&client) })
        .await;
    cx.update(|cx| {
        if !agent_hosts(cx).is_some_and(|hosts| hosts.machines.contains_key(key)) {
            return AgentHostLoad::Detached;
        }
        match fetched {
            Ok(settings) => {
                update_hosts(cx, |hosts| hosts.adopt(key, settings));
                AgentHostLoad::Loaded
            }
            Err(error) => {
                if report {
                    update_hosts(cx, |hosts| {
                        hosts.set_error(key, format!("读取智能体配置失败，正在重试：{error}"))
                    });
                }
                AgentHostLoad::Failed
            }
        }
    })
}

/// 机器被移除：丢掉它的配置，正在排队的修改也不再发。
pub fn detach_agent_host(key: &RuntimeKey, cx: &mut App) {
    update_hosts(cx, |hosts| {
        hosts.machines.remove(key);
    });
    if cx.has_global::<AgentHostEditQueues>() {
        cx.global_mut::<AgentHostEditQueues>().0.remove(key);
    }
}

/// 修改按提交顺序逐个发给那台机器。并发发出去，运行时处理的先后和回包的先后都可能
/// 乱，界面就会停在一个旧版本上。每台机器一条队列，互不等待。
#[derive(Default)]
struct AgentHostEditQueues(BTreeMap<RuntimeKey, smol::channel::Sender<AgentHostEdit>>);

impl Global for AgentHostEditQueues {}

fn submit_agent_host_edit(
    key: RuntimeKey,
    client: RuntimeClient,
    edit: AgentHostEdit,
    cx: &mut App,
) {
    if !cx.has_global::<AgentHostEditQueues>() {
        cx.set_global(AgentHostEditQueues::default());
    }
    if let Some(tx) = cx.global::<AgentHostEditQueues>().0.get(&key) {
        let _ = tx.try_send(edit);
        return;
    }
    let (tx, rx) = smol::channel::unbounded::<AgentHostEdit>();
    let _ = tx.try_send(edit);
    cx.global_mut::<AgentHostEditQueues>()
        .0
        .insert(key.clone(), tx);
    cx.spawn(async move |cx| {
        while let Ok(edit) = rx.recv().await {
            let apply_client = client.clone();
            let applied = cx
                .background_executor()
                .spawn(async move {
                    apply_client
                        .call::<AgentHostApplyEdit>(&edit, RUNTIME_TIMEOUT)
                        .map_err(|error| error.to_string())
                })
                .await;
            // 后面还有排着的修改时，这一包不是最终状态，先不覆盖本地。
            let drained = rx.is_empty();
            let settled = match applied {
                Ok(settings) => Ok(settings),
                Err(error) if drained => {
                    // 失败后以运行时为准，把本地乐观改动退回去。
                    let fetch_client = client.clone();
                    let fresh = cx
                        .background_executor()
                        .spawn(async move { fetch_agent_host(&fetch_client) })
                        .await;
                    Err((error, fresh.ok()))
                }
                Err(error) => Err((error, None)),
            };
            cx.update(|cx| {
                update_hosts(cx, |hosts| match settled {
                    Ok(settings) => {
                        if drained {
                            hosts.adopt(&key, settings);
                        }
                    }
                    Err((error, fresh)) => {
                        eprintln!("[agent-ui] 保存配置失败: {error}");
                        if let Some(settings) = fresh {
                            hosts.adopt(&key, settings);
                        }
                        hosts.set_error(&key, format!("保存智能体配置失败：{error}"));
                    }
                })
            });
        }
    })
    .detach();
}

/// 改一台机器的配置。本地立刻生效；按「这次改了什么」异步提交给那台机器，以它合并后的
/// 结果为准。返回 false 表示这次没有接受：机器没挂，或配置还没读到。
pub fn try_apply_agent_host(
    key: &RuntimeKey,
    f: impl FnOnce(&mut AgentHostSettings),
    cx: &mut App,
) -> bool {
    let Some(machine) = agent_hosts(cx).and_then(|hosts| hosts.machine(key)) else {
        return false;
    };
    let client = machine.client.clone();
    let Some(before) = machine.settings.clone() else {
        update_hosts(cx, |hosts| {
            hosts.set_error(key, "这台机器的智能体配置还没读到，稍后再改".to_string())
        });
        return false;
    };
    let mut after = before.clone();
    f(&mut after);
    if before == after {
        return true;
    }
    update_hosts(cx, |hosts| hosts.adopt(key, after.clone()));
    submit_agent_host_edit(key.clone(), client, AgentHostEdit { before, after }, cx);
    true
}

pub fn apply_agent_host(key: &RuntimeKey, f: impl FnOnce(&mut AgentHostSettings), cx: &mut App) {
    let _ = try_apply_agent_host(key, f, cx);
}

/// 从各台已经读到过的机器重新取一份。CLI / Control API 改过定义后，打开智能体面时对齐。
pub fn reload_agent_hosts(cx: &mut App) {
    let Some(hosts) = agent_hosts(cx) else {
        return;
    };
    let targets: Vec<(RuntimeKey, RuntimeClient)> = hosts
        .machines
        .iter()
        .filter(|(_, machine)| machine.settings.is_some())
        .map(|(key, machine)| (key.clone(), machine.client.clone()))
        .collect();
    for (key, client) in targets {
        cx.spawn(async move |cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move { fetch_agent_host(&client) })
                .await;
            cx.update(|cx| {
                update_hosts(cx, |hosts| match fetched {
                    Ok(settings) => hosts.adopt(&key, settings),
                    Err(error) => hosts.set_error(&key, format!("读取智能体配置失败：{error}")),
                })
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn automation_snapshot_rejects_stale_revisions_and_retired_stores() {
        let mut config = AgentHostState::default();
        let mut store_a = AutomationFile {
            store_id: "store-a".into(),
            revision: 10,
            ..Default::default()
        };
        assert!(config.replace_automation_snapshot(store_a.clone()));
        assert!(
            !config.replace_automation_snapshot(store_a.clone()),
            "同一 revision 的相同投影不应再刷新"
        );
        store_a.webhook_base_url = Some("http://127.0.0.1:17827".into());
        assert!(
            config.replace_automation_snapshot(store_a.clone()),
            "同 revision 补上 webhook 根地址仍要应用"
        );

        store_a.revision = 9;
        assert!(!config.replace_automation_snapshot(store_a.clone()));
        assert_eq!(config.automation_revision, 10);

        let store_b = AutomationFile {
            store_id: "store-b".into(),
            revision: 1,
            ..Default::default()
        };
        assert!(config.replace_automation_snapshot(store_b));
        assert_eq!(config.automation_store_id, "store-b");
        assert_eq!(config.automation_revision, 1);

        store_a.revision = 11;
        assert!(!config.replace_automation_snapshot(store_a));
        assert_eq!(config.automation_store_id, "store-b");
    }

    /// 假运行时：`agent_host.get` 回它手里那份，`agent_host.apply_edit` 按 `after` 换上；
    /// `fail_edits` 时修改一律报错。每台机器各一个，用来确认调用发到了对的机器。
    #[derive(Clone)]
    struct FakeRuntime {
        settings: std::sync::Arc<std::sync::Mutex<AgentHostSettings>>,
        fail_edits: bool,
    }

    impl FakeRuntime {
        fn new(agent_name: &str, fail_edits: bool) -> Self {
            let mut settings = AgentHostSettings::fresh();
            settings.agents.push(AgentDefinition {
                id: "a".into(),
                name: agent_name.into(),
                ..Default::default()
            });
            Self {
                settings: std::sync::Arc::new(std::sync::Mutex::new(settings)),
                fail_edits,
            }
        }

        fn stored(&self) -> AgentHostSettings {
            self.settings.lock().unwrap().clone()
        }

        /// 每次连接给出一对 socket，另一端在新线程里答一条控制请求。
        fn client(&self) -> RuntimeClient {
            use smelt_runtime_client::protocol::control::{
                ControlError, ControlErrorCode, ControlResponse,
            };
            use std::io::{BufRead as _, Write as _};

            let fake = self.clone();
            RuntimeClient::new(move || {
                let (client, server) = smelt_runtime_client::RuntimeStream::pair()?;
                let fake = fake.clone();
                std::thread::spawn(move || {
                    let mut line = String::new();
                    std::io::BufReader::new(server.try_clone().unwrap())
                        .read_line(&mut line)
                        .unwrap();
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let request_id = request["request_id"].as_str().unwrap().to_string();
                    let response = match request["method"].as_str().unwrap() {
                        "agent.host.get" => {
                            serde_json::to_value(ControlResponse::ok(request_id, fake.stored()))
                        }
                        "agent.host.apply_edit" if fake.fail_edits => {
                            serde_json::to_value(ControlResponse::<()>::error(
                                request_id,
                                ControlError::new(ControlErrorCode::OperationFailed, "写库失败"),
                            ))
                        }
                        "agent.host.apply_edit" => {
                            let edit: AgentHostEdit =
                                serde_json::from_value(request["params"].clone()).unwrap();
                            *fake.settings.lock().unwrap() = edit.after.clone();
                            serde_json::to_value(ControlResponse::ok(request_id, edit.after))
                        }
                        other => panic!("没预期的调用 {other}"),
                    }
                    .unwrap();
                    writeln!(&server, "{response}").unwrap();
                });
                Ok(client)
            })
        }
    }

    /// 挂上并读一次。主 crate 里要等那台机器在线才读，这里的假运行时总在。
    fn attach(key: RuntimeKey, client: RuntimeClient, cx: &mut App) {
        if register_agent_host(&key, client, cx) {
            cx.spawn(async move |cx| {
                load_agent_host(&key, true, cx).await;
            })
            .detach();
        }
    }

    fn rename(name: &'static str) -> impl FnOnce(&mut AgentHostSettings) {
        move |settings| settings.agents[0].name = name.into()
    }

    /// 两台机器各读各的、各改各的：改远程那台只落到远程，本机不受影响。
    #[gpui::test]
    fn each_machine_loads_and_saves_its_own_settings(cx: &mut TestAppContext) {
        let local = FakeRuntime::new("本机的", false);
        let far = FakeRuntime::new("远程的", false);
        let far_key = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            attach(RuntimeKey::Local, local.client(), cx);
            attach(far_key.clone(), far.client(), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let hosts = agent_hosts(cx).unwrap();
            assert_eq!(hosts.agents(&RuntimeKey::Local)[0].name, "本机的");
            assert_eq!(hosts.agents(&far_key)[0].name, "远程的");
            assert!(try_apply_agent_host(&far_key, rename("改过的远程"), cx));
            // 乐观更新：立刻能看到。
            assert_eq!(
                agent_hosts(cx).unwrap().agents(&far_key)[0].name,
                "改过的远程"
            );
        });
        cx.run_until_parked();
        assert_eq!(far.stored().agents[0].name, "改过的远程");
        assert_eq!(local.stored().agents[0].name, "本机的");
        cx.update(|cx| {
            assert_eq!(
                agent_hosts(cx).unwrap().agents(&RuntimeKey::Local)[0].name,
                "本机的"
            );
        });
    }

    /// 一台机器保存失败只退回那一台的乐观改动，别的机器照常。
    #[gpui::test]
    fn a_failed_save_rolls_back_only_that_machine(cx: &mut TestAppContext) {
        let local = FakeRuntime::new("本机的", false);
        let far = FakeRuntime::new("远程的", true);
        let far_key = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            attach(RuntimeKey::Local, local.client(), cx);
            attach(far_key.clone(), far.client(), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(try_apply_agent_host(&far_key, rename("改不进去"), cx));
            assert!(try_apply_agent_host(
                &RuntimeKey::Local,
                rename("改过的本机"),
                cx
            ));
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let hosts = agent_hosts(cx).unwrap();
            assert_eq!(
                hosts.agents(&far_key)[0].name,
                "远程的",
                "失败要退回运行时那份"
            );
            assert!(hosts.machine(&far_key).unwrap().error.is_some());
            assert_eq!(hosts.agents(&RuntimeKey::Local)[0].name, "改过的本机");
            assert!(hosts.machine(&RuntimeKey::Local).unwrap().error.is_none());
        });
    }

    /// 机器移除后取不到它的配置，也不再接受修改。
    #[gpui::test]
    fn a_detached_machine_is_gone(cx: &mut TestAppContext) {
        let far = FakeRuntime::new("远程的", false);
        let far_key = RuntimeKey::Remote("far".into());
        cx.update(|cx| attach(far_key.clone(), far.client(), cx));
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(agent_host_loaded(&far_key, cx));
            detach_agent_host(&far_key, cx);
            assert!(!agent_host_loaded(&far_key, cx));
            assert!(agent_hosts(cx).unwrap().agents(&far_key).is_empty());
            assert!(!try_apply_agent_host(&far_key, rename("没人收"), cx));
        });
    }
}
