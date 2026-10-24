//! 桌面和运行时的版本协商（设计文档 D7）。连上运行时先读 `system.describe`，对照这个
//! 桌面要用的控制方法和事件投影版本，对不上就是不兼容，并说清该升级哪一边。不对的话，
//! 运行时较新时订阅解不出投影，断开重连没有尽头，会话状态永远到不了；较旧时各处零散地
//! 报 MethodNotFound。说明里带着运行时的版本，看得出它是哪个版本。

use std::fmt;

use smelt_event_protocol::{
    CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION, CORE_SESSION_STATE_SCHEMA_VERSION,
    EVENT_BUS_PROTOCOL_VERSION,
};
use smelt_runtime_client::protocol::control::{
    CONTROL_API_VERSION, ControlMethod, SystemDescribeResult,
};

/// 该升级的一边。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outdated {
    Runtime,
    Desktop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incompatibility {
    pub outdated: Outdated,
    /// 运行时自己报的版本（`system.describe` 的 `daemon.version`）。
    pub runtime_version: String,
    pub reason: String,
}

impl fmt::Display for Incompatibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.outdated {
            Outdated::Runtime => write!(
                f,
                "运行时 {} 较旧，需要升级运行时（{}）",
                self.runtime_version, self.reason
            ),
            Outdated::Desktop => write!(
                f,
                "运行时 {} 较新，需要升级桌面（{}）",
                self.runtime_version, self.reason
            ),
        }
    }
}

/// 这个桌面能不能用这个运行时。
///
/// 控制方法：运行时支持的协议版本区间要包含桌面的，还要有桌面认得的全部方法（方法只增
/// 不改，缺了就是运行时旧）。事件：总线协议和核心投影要相同，会话投影和会话状态事件
/// 不能比桌面新（新字段都可缺省，桌面解得了更旧的）。
pub fn check(described: &SystemDescribeResult) -> Result<(), Incompatibility> {
    check_versions(described).map_err(|(outdated, reason)| Incompatibility {
        outdated,
        runtime_version: described.daemon.version.clone(),
        reason,
    })
}

fn check_versions(described: &SystemDescribeResult) -> Result<(), (Outdated, String)> {
    let control = &described.control;
    if CONTROL_API_VERSION < control.min_supported_version {
        return Err(desktop(format!(
            "控制协议 v{CONTROL_API_VERSION}，运行时至少要 v{}",
            control.min_supported_version
        )));
    }
    if CONTROL_API_VERSION > control.max_supported_version {
        return Err(runtime(format!(
            "控制协议 v{}，桌面要 v{CONTROL_API_VERSION}",
            control.max_supported_version
        )));
    }
    let missing: Vec<&str> = ControlMethod::ALL
        .iter()
        .map(|method| method.as_str())
        .filter(|name| !control.methods.iter().any(|known| known == name))
        .collect();
    if !missing.is_empty() {
        return Err(runtime(missing_methods(&missing)));
    }
    let Some(events) = &described.events else {
        return Err(runtime("没有声明事件协议版本".to_string()));
    };
    same(
        "事件协议",
        events.protocol_version,
        EVENT_BUS_PROTOCOL_VERSION,
    )?;
    same(
        "核心投影",
        events.projection_version,
        CORE_PROJECTION_VERSION,
    )?;
    not_newer(
        "会话投影",
        events.session_projection_version,
        CORE_SESSION_PROJECTION_VERSION,
    )?;
    not_newer(
        "会话状态事件",
        events.session_state_schema_version,
        CORE_SESSION_STATE_SCHEMA_VERSION,
    )?;
    Ok(())
}

fn runtime(reason: String) -> (Outdated, String) {
    (Outdated::Runtime, reason)
}

fn desktop(reason: String) -> (Outdated, String) {
    (Outdated::Desktop, reason)
}

fn same(label: &str, runtime_version: u16, desktop_version: u16) -> Result<(), (Outdated, String)> {
    if runtime_version < desktop_version {
        return Err(runtime(format!(
            "{label} v{runtime_version}，桌面要 v{desktop_version}"
        )));
    }
    not_newer(label, runtime_version, desktop_version)
}

fn not_newer(
    label: &str,
    runtime_version: u16,
    desktop_version: u16,
) -> Result<(), (Outdated, String)> {
    if runtime_version > desktop_version {
        return Err(desktop(format!(
            "{label} v{runtime_version}，桌面只认到 v{desktop_version}"
        )));
    }
    Ok(())
}

fn missing_methods(missing: &[&str]) -> String {
    const SHOWN: usize = 3;
    let shown = missing[..missing.len().min(SHOWN)].join("、");
    if missing.len() > SHOWN {
        format!("缺少 {} 个方法：{shown} 等", missing.len())
    } else {
        format!("缺少方法：{shown}")
    }
}

#[cfg(test)]
mod tests {
    use smelt_runtime_client::protocol::control::{
        CONTROL_API_MIN_SUPPORTED_VERSION, ControlProtocolDescription, DaemonRuntimeInfo,
        EventProtocolDescription,
    };

    use super::*;

    /// 和这个桌面同一份构建的运行时会回的 describe。
    fn current() -> SystemDescribeResult {
        SystemDescribeResult {
            control: ControlProtocolDescription {
                current_version: CONTROL_API_VERSION,
                min_supported_version: CONTROL_API_MIN_SUPPORTED_VERSION,
                max_supported_version: CONTROL_API_VERSION,
                methods: ControlMethod::ALL.iter().map(ToString::to_string).collect(),
            },
            daemon: DaemonRuntimeInfo {
                version: "test".to_string(),
                exe_mtime: 0,
                exe: None,
                pid: 1,
                started_at: 0,
                session_count: 0,
                agent_event_version: 1,
                runtime_id: Some("runtime-a".to_string()),
                os: Some("linux".to_string()),
                arch: Some("x86_64".to_string()),
                hostname: None,
                desktop: None,
                smelt_home: None,
            },
            events: Some(EventProtocolDescription {
                protocol_version: EVENT_BUS_PROTOCOL_VERSION,
                projection_version: CORE_PROJECTION_VERSION,
                session_projection_version: CORE_SESSION_PROJECTION_VERSION,
                session_state_schema_version: CORE_SESSION_STATE_SCHEMA_VERSION,
            }),
        }
    }

    fn outdated(described: &SystemDescribeResult) -> Outdated {
        check(described).expect_err("应判为不兼容").outdated
    }

    #[test]
    fn same_build_is_compatible() {
        assert_eq!(check(&current()), Ok(()));
    }

    #[test]
    fn extra_methods_from_a_newer_runtime_are_fine() {
        let mut described = current();
        described
            .control
            .methods
            .push("runtime.upgrade".to_string());
        assert_eq!(check(&described), Ok(()));
    }

    #[test]
    fn missing_method_means_the_runtime_is_older() {
        let mut described = current();
        described
            .control
            .methods
            .retain(|method| method != "conversation.create_for_agent");
        let error = check(&described).unwrap_err();
        assert_eq!(error.outdated, Outdated::Runtime);
        assert!(error.reason.contains("conversation.create_for_agent"));
    }

    #[test]
    fn control_version_range_decides_the_direction() {
        let mut newer = current();
        newer.control.min_supported_version = CONTROL_API_VERSION + 1;
        newer.control.max_supported_version = CONTROL_API_VERSION + 1;
        assert_eq!(outdated(&newer), Outdated::Desktop);

        let mut older = current();
        older.control.min_supported_version = 0;
        older.control.max_supported_version = CONTROL_API_VERSION - 1;
        assert_eq!(outdated(&older), Outdated::Runtime);
    }

    /// 早于版本协商的运行时不报事件版本，订阅解不解得了无从知道，按旧处理。
    #[test]
    fn runtime_without_event_versions_is_older() {
        let mut described = current();
        described.events = None;
        assert_eq!(outdated(&described), Outdated::Runtime);
    }

    #[test]
    fn core_projection_must_match_exactly() {
        let mut newer = current();
        newer.events.as_mut().unwrap().projection_version = CORE_PROJECTION_VERSION + 1;
        assert_eq!(outdated(&newer), Outdated::Desktop);

        let mut older = current();
        older.events.as_mut().unwrap().projection_version = CORE_PROJECTION_VERSION - 1;
        assert_eq!(outdated(&older), Outdated::Runtime);

        let mut bus = current();
        bus.events.as_mut().unwrap().protocol_version = EVENT_BUS_PROTOCOL_VERSION + 1;
        assert_eq!(outdated(&bus), Outdated::Desktop);
    }

    /// 会话投影的新字段都可缺省：旧的解得了，新的解不了。
    #[test]
    fn session_projection_may_be_older_but_not_newer() {
        let mut older = current();
        older.events.as_mut().unwrap().session_projection_version = 1;
        older.events.as_mut().unwrap().session_state_schema_version = 1;
        assert_eq!(check(&older), Ok(()));

        let mut newer = current();
        newer.events.as_mut().unwrap().session_projection_version =
            CORE_SESSION_PROJECTION_VERSION + 1;
        assert_eq!(outdated(&newer), Outdated::Desktop);

        let mut newer_schema = current();
        newer_schema
            .events
            .as_mut()
            .unwrap()
            .session_state_schema_version = CORE_SESSION_STATE_SCHEMA_VERSION + 1;
        assert_eq!(outdated(&newer_schema), Outdated::Desktop);
    }

    #[test]
    fn message_names_the_side_to_upgrade_in_chinese() {
        let mut described = current();
        described.events = None;
        assert_eq!(
            check(&described).unwrap_err().to_string(),
            "运行时 test 较旧，需要升级运行时（没有声明事件协议版本）"
        );
    }
}
