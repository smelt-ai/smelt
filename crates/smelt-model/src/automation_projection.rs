//! 客户端手里一台机器的自动化投影：运行时经 `automations.changed` 推来的整份库（自动化、
//! 状态、运行记录）。客户端不据此调度，只拿来显示；改动一律发给运行时。
//!
//! 每台运行时有自己的自动化库，两台上同 id 的自动化是两回事，所以按机器各留一份。

use std::collections::HashSet;

use smelt_event_protocol::CoreProjectionEvent;

use crate::automation::{Automation, AutomationFile, AutomationRun, AutomationState};

/// `automations.changed` 的载荷（快照和事件同一种外壳）解出来的那份库。运行时还没有库是
/// `None`。
pub fn decode_automations_payload(
    payload: serde_json::Value,
) -> Result<Option<AutomationFile>, String> {
    let projection: CoreProjectionEvent =
        serde_json::from_value(payload).map_err(|error| error.to_string())?;
    if projection.data.is_null() {
        return Ok(None);
    }
    serde_json::from_value(projection.data).map_err(|error| error.to_string())
}

/// 一台机器推来的自动化投影。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MachineAutomations {
    pub automations: Vec<Automation>,
    pub store_id: String,
    pub retired_store_ids: HashSet<String>,
    pub revision: u64,
    pub states: Vec<AutomationState>,
    pub runs: Vec<AutomationRun>,
    pub store_error: Option<String>,
    /// 这台机器的 webhook 地址前缀。
    pub webhook_base_url: Option<String>,
}

impl MachineAutomations {
    /// 收到过运行时的库。
    pub fn is_ready(&self) -> bool {
        !self.store_id.is_empty()
    }

    pub fn automation(&self, automation_id: &str) -> Option<&Automation> {
        self.automations
            .iter()
            .find(|automation| automation.id == automation_id)
    }

    /// 挂着这个智能体的自动化。
    pub fn automations_for<'a>(&'a self, agent_id: &str) -> Vec<&'a Automation> {
        self.automations
            .iter()
            .filter(|automation| automation.agent_definition_id() == Some(agent_id))
            .collect()
    }

    pub fn state_for(&self, automation_id: &str) -> Option<&AutomationState> {
        self.states
            .iter()
            .find(|state| state.automation_id == automation_id)
    }

    pub fn active_run(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.runs
            .iter()
            .find(|run| run.automation_id == automation_id && run.blocks_overlap())
    }

    pub fn latest_run(&self, automation_id: &str) -> Option<&AutomationRun> {
        self.runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .max_by_key(|run| (run.created_at, run.id.as_str()))
    }

    /// 那条自动化的运行，新的在前。
    pub fn runs_for(&self, automation_id: &str) -> Vec<&AutomationRun> {
        let mut runs = self
            .runs
            .iter()
            .filter(|run| run.automation_id == automation_id)
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse((run.created_at, run.id.as_str())));
        runs
    }

    pub fn is_retired_store(&self, store_id: &str) -> bool {
        self.retired_store_ids.contains(store_id)
    }

    /// 收下一份快照，返回有没有变化。同一个库：版本旧了不收，内容没变也不算变化。换了库
    /// （运行时重建了库）：旧库从此退休，它迟到的快照不再收。
    pub fn replace_snapshot(&mut self, snapshot: AutomationFile) -> bool {
        if snapshot.store_id == self.store_id {
            if snapshot.revision < self.revision {
                return false;
            }
            if snapshot.revision == self.revision
                && snapshot.automations == self.automations
                && snapshot.states == self.states
                && snapshot.runs == self.runs
                && snapshot.store_error == self.store_error
                && snapshot.webhook_base_url == self.webhook_base_url
            {
                return false;
            }
        } else {
            if self.retired_store_ids.contains(&snapshot.store_id) {
                return false;
            }
            if !self.store_id.is_empty() {
                self.retired_store_ids.insert(self.store_id.clone());
            }
        }
        self.store_id = snapshot.store_id;
        self.revision = snapshot.revision;
        self.automations = snapshot.automations;
        self.states = snapshot.states;
        self.runs = snapshot.runs;
        self.store_error = snapshot.store_error;
        self.webhook_base_url = snapshot.webhook_base_url;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_rejects_stale_revisions_and_retired_stores() {
        let mut machine = MachineAutomations::default();
        let mut store_a = AutomationFile {
            store_id: "store-a".into(),
            revision: 10,
            ..Default::default()
        };
        assert!(machine.replace_snapshot(store_a.clone()));
        assert!(
            !machine.replace_snapshot(store_a.clone()),
            "同一 revision 的相同投影不应再刷新"
        );
        store_a.webhook_base_url = Some("http://127.0.0.1:17827".into());
        assert!(
            machine.replace_snapshot(store_a.clone()),
            "同 revision 补上 webhook 根地址仍要应用"
        );

        store_a.revision = 9;
        assert!(!machine.replace_snapshot(store_a.clone()));
        assert_eq!(machine.revision, 10);

        let store_b = AutomationFile {
            store_id: "store-b".into(),
            revision: 1,
            ..Default::default()
        };
        assert!(machine.replace_snapshot(store_b));
        assert_eq!(machine.store_id, "store-b");
        assert_eq!(machine.revision, 1);

        store_a.revision = 11;
        assert!(!machine.replace_snapshot(store_a));
        assert_eq!(machine.store_id, "store-b");
    }

    #[test]
    fn payload_decodes_the_projection_envelope() {
        let file = AutomationFile {
            store_id: "s".into(),
            revision: 3,
            ..Default::default()
        };
        let payload = serde_json::to_value(CoreProjectionEvent {
            revision: 3,
            data: serde_json::to_value(&file).unwrap(),
        })
        .unwrap();
        assert_eq!(decode_automations_payload(payload).unwrap(), Some(file));

        let empty = serde_json::to_value(CoreProjectionEvent {
            revision: 0,
            data: serde_json::Value::Null,
        })
        .unwrap();
        assert_eq!(decode_automations_payload(empty).unwrap(), None);
        assert!(decode_automations_payload(serde_json::json!({"x": 1})).is_err());
    }
}
