//! 智能体宿主状态：每台机器上运行时那部分配置（启动命令、env、profile、智能体定义、
//! hook 开关）在桌面上的只读缓存。
//!
//! 配置只有运行时写，改完整份推过来（话题 `agent_host.changed`，订上时先给一份快照）；
//! 这里只收推送，不自己读、不先改本地。要改配置就把修改发给那台机器的运行时，等它推回来
//! （见 `smelt_plugin_api::edit_agent_settings`）。
//!
//! 需要 `gpui::Global`，所以不能进不许引 GPUI 的 smelt-core。设置页怎么渲染
//! 这些字段仍在主 crate 的 settings 模块。

use gpui::{App, Global};
use std::collections::BTreeMap;

pub use smelt_client::runtime::RuntimeKey;
pub use smelt_model::agent_definition::AgentDefinition;
use smelt_model::agent_host::AgentHostSettings;

/// 一台机器的智能体配置收到了什么。还没收到推送的机器不在表里。
#[derive(Clone, Debug, PartialEq)]
pub enum MachineAgentHost {
    /// 运行时推来的整份配置。
    Ready(AgentHostSettings),
    /// 那台机器给不了：运行时太旧、不认识这个话题等。原因是给人看的。
    Unavailable(String),
}

/// 每台机器各一份智能体配置。本机和远程机器同一套：读都按 `RuntimeKey`。
#[derive(Clone, Default)]
pub struct AgentHosts {
    machines: BTreeMap<RuntimeKey, MachineAgentHost>,
}

impl Global for AgentHosts {}

impl AgentHosts {
    pub fn machine(&self, key: &RuntimeKey) -> Option<&MachineAgentHost> {
        self.machines.get(key)
    }

    /// 那台机器的配置；还没收到、给不了都是 `None`。
    pub fn settings(&self, key: &RuntimeKey) -> Option<&AgentHostSettings> {
        match self.machines.get(key)? {
            MachineAgentHost::Ready(settings) => Some(settings),
            MachineAgentHost::Unavailable(_) => None,
        }
    }

    /// 那台机器给不了配置的原因；收到了或还没收到是 `None`。
    pub fn unavailable(&self, key: &RuntimeKey) -> Option<&str> {
        match self.machines.get(key)? {
            MachineAgentHost::Ready(_) => None,
            MachineAgentHost::Unavailable(reason) => Some(reason),
        }
    }

    /// 那台机器上的智能体定义；没收到是空的。
    pub fn agents(&self, key: &RuntimeKey) -> &[AgentDefinition] {
        self.settings(key)
            .map(|settings| settings.agents.as_slice())
            .unwrap_or_default()
    }

    pub fn agent(&self, key: &RuntimeKey, id: &str) -> Option<&AgentDefinition> {
        self.settings(key)?.find_agent(id)
    }

    /// 收到过推送的机器，本机在前。
    pub fn keys(&self) -> impl Iterator<Item = &RuntimeKey> {
        self.machines.keys()
    }
}

/// 读到的各台机器的配置。没挂这个全局（测试、启动早期）时当作什么都没有。
pub fn agent_hosts(cx: &App) -> Option<&AgentHosts> {
    cx.try_global::<AgentHosts>()
}

/// 那台机器的配置收到了没有。没收到之前，依赖它的启动要等。
pub fn agent_host_loaded(key: &RuntimeKey, cx: &App) -> bool {
    agent_hosts(cx).is_some_and(|hosts| hosts.settings(key).is_some())
}

/// 那台机器的配置有了结果：收到了，或者它给不了。还没结果时要不要接着等（机器在不在线）
/// 由调用方结合连通性判断，这里看不到。
pub fn agent_host_settled(key: &RuntimeKey, cx: &App) -> bool {
    agent_hosts(cx).is_some_and(|hosts| hosts.machine(key).is_some())
}

fn update_hosts(cx: &mut App, change: impl FnOnce(&mut AgentHosts)) {
    if !cx.has_global::<AgentHosts>() {
        cx.set_global(AgentHosts::default());
    }
    change(cx.global_mut::<AgentHosts>());
}

/// 运行时推来了那台机器的整份配置（快照或改完的那份），整份换上。
pub fn receive_agent_host(key: &RuntimeKey, settings: AgentHostSettings, cx: &mut App) {
    let next = MachineAgentHost::Ready(settings);
    if agent_hosts(cx).and_then(|hosts| hosts.machine(key)) == Some(&next) {
        return;
    }
    update_hosts(cx, |hosts| {
        hosts.machines.insert(key.clone(), next);
    });
}

/// 那台机器给不了配置（订阅被拒等）。原先收到的那份不再可信，一起丢掉。
pub fn agent_host_unavailable(key: &RuntimeKey, reason: String, cx: &mut App) {
    let next = MachineAgentHost::Unavailable(reason);
    if agent_hosts(cx).and_then(|hosts| hosts.machine(key)) == Some(&next) {
        return;
    }
    update_hosts(cx, |hosts| {
        hosts.machines.insert(key.clone(), next);
    });
}

/// 机器移除了，或它的运行时和桌面对不上：丢掉它的配置，下次订上时由快照重新给。
pub fn forget_agent_host(key: &RuntimeKey, cx: &mut App) {
    if agent_hosts(cx).is_none_or(|hosts| hosts.machine(key).is_none()) {
        return;
    }
    update_hosts(cx, |hosts| {
        hosts.machines.remove(key);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn named(name: &str) -> AgentHostSettings {
        let mut settings = AgentHostSettings::fresh();
        settings.agents.push(AgentDefinition {
            id: "a".into(),
            name: name.into(),
            ..Default::default()
        });
        settings
    }

    /// 各台机器各收各的：推给远程那台只换远程的，本机不受影响。
    #[gpui::test]
    fn each_machine_keeps_what_it_was_pushed(cx: &mut TestAppContext) {
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            assert!(!agent_host_loaded(&RuntimeKey::Local, cx));
            assert!(!agent_host_settled(&RuntimeKey::Local, cx));
            receive_agent_host(&RuntimeKey::Local, named("本机的"), cx);
            receive_agent_host(&far, named("远程的"), cx);
            receive_agent_host(&far, named("改过的远程"), cx);
            let hosts = agent_hosts(cx).unwrap();
            assert_eq!(hosts.agents(&RuntimeKey::Local)[0].name, "本机的");
            assert_eq!(hosts.agents(&far)[0].name, "改过的远程");
            assert!(agent_host_loaded(&far, cx));
            assert!(agent_host_settled(&far, cx));
        });
    }

    /// 给不了时丢掉原先那份，原因留着给人看；之后再推来就恢复。
    #[gpui::test]
    fn unavailable_drops_the_old_copy_until_pushed_again(cx: &mut TestAppContext) {
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            receive_agent_host(&far, named("远程的"), cx);
            agent_host_unavailable(&far, "升级运行时后可用".into(), cx);
            let hosts = agent_hosts(cx).unwrap();
            assert!(hosts.settings(&far).is_none());
            assert_eq!(hosts.unavailable(&far), Some("升级运行时后可用"));
            assert!(!agent_host_loaded(&far, cx));
            assert!(agent_host_settled(&far, cx), "给不了也是有了结果，不用再等");
            receive_agent_host(&far, named("升级后的"), cx);
            let hosts = agent_hosts(cx).unwrap();
            assert_eq!(hosts.agents(&far)[0].name, "升级后的");
            assert!(hosts.unavailable(&far).is_none());
        });
    }

    /// 同样的一份再推来不算变化：不惊动观察这个全局的界面和插件。
    #[gpui::test]
    fn the_same_push_twice_is_not_a_change(cx: &mut TestAppContext) {
        let changes = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| {
            cx.set_global(AgentHosts::default());
            let counted = changes.clone();
            cx.observe_global::<AgentHosts>(move |_| counted.set(counted.get() + 1))
                .detach();
            receive_agent_host(&RuntimeKey::Local, named("本机的"), cx);
            receive_agent_host(&RuntimeKey::Local, named("本机的"), cx);
            forget_agent_host(&RuntimeKey::Remote("没有".into()), cx);
        });
        cx.run_until_parked();
        assert_eq!(changes.get(), 1);
    }

    /// 机器移除后取不到它的配置，也不算有结果。
    #[gpui::test]
    fn a_forgotten_machine_is_gone(cx: &mut TestAppContext) {
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            receive_agent_host(&far, named("远程的"), cx);
            forget_agent_host(&far, cx);
            assert!(!agent_host_loaded(&far, cx));
            assert!(!agent_host_settled(&far, cx));
            assert!(agent_hosts(cx).unwrap().agents(&far).is_empty());
        });
    }
}
