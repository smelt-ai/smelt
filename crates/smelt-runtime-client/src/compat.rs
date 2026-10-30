//! 客户端（桌面、手机）和运行时的版本协商（设计文档 D7）。连上运行时先读
//! `system.describe`，对照客户端要用的控制方法和事件协议版本，对不上就是不兼容，并说清
//! 该升级哪一边。不对的话，订阅被拒或解不出投影，重订没有尽头，会话目录永远到不了，界面
//! 只能一直「同步中」；控制方法缺了则各处零散地报 MethodNotFound。说明里带着运行时的
//! 版本，看得出它是哪个版本。

use std::fmt;

use smelt_event_protocol::{EVENT_BUS_PROTOCOL_VERSION, MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION};

use crate::protocol::control::{CONTROL_API_VERSION, ControlMethod, SystemDescribeResult};

/// 该升级的一边。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outdated {
    Runtime,
    /// 这一端：桌面或手机。
    Client,
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
            Outdated::Client => write!(
                f,
                "运行时 {} 较新，需要升级客户端（{}）",
                self.runtime_version, self.reason
            ),
        }
    }
}

/// 桌面能不能用这个运行时：控制方法加事件（[`check_events`]）。
///
/// 控制方法：运行时支持的协议版本区间要包含客户端的，还要有全部必需方法（方法只增不改，
/// 缺了就是运行时旧）。可选方法（[`ControlMethod::OPTIONAL`]）缺了不算：只有那一项功能
/// 用不了，由用它的地方降级，不能为此把整台机器的终端和对话都拦在外面。
pub fn check(described: &SystemDescribeResult) -> Result<(), Incompatibility> {
    with_runtime_version(
        described,
        check_control(described).and_then(|()| event_versions(described)),
    )
}

/// 订阅解不解得了运行时推来的状态：只看事件总线协议版本落不落在客户端支持的区间里。载荷
/// 宽容读取，加字段、给标了「会增长」的枚举加值不升版本，所以这些改动下运行时比客户端新、旧都照常连（见
/// `smelt_event_protocol` 的模块文档）。
///
/// 手机只对这一项：[`ControlMethod`] 的必需方法是按桌面的用法列的，手机只用其中一部分，
/// 缺的方法由用到的地方报 MethodNotFound。
pub fn check_events(described: &SystemDescribeResult) -> Result<(), Incompatibility> {
    with_runtime_version(described, event_versions(described))
}

fn with_runtime_version(
    described: &SystemDescribeResult,
    result: Result<(), (Outdated, String)>,
) -> Result<(), Incompatibility> {
    result.map_err(|(outdated, reason)| Incompatibility {
        outdated,
        runtime_version: described.daemon.version.clone(),
        reason,
    })
}

fn check_control(described: &SystemDescribeResult) -> Result<(), (Outdated, String)> {
    let control = &described.control;
    if CONTROL_API_VERSION < control.min_supported_version {
        return Err(client(format!(
            "控制协议 v{CONTROL_API_VERSION}，运行时至少要 v{}",
            control.min_supported_version
        )));
    }
    if CONTROL_API_VERSION > control.max_supported_version {
        return Err(runtime(format!(
            "控制协议 v{}，客户端要 v{CONTROL_API_VERSION}",
            control.max_supported_version
        )));
    }
    let missing: Vec<&str> = ControlMethod::ALL
        .iter()
        .filter(|method| method.required())
        .map(|method| method.as_str())
        .filter(|name| !control.methods.iter().any(|known| known == name))
        .collect();
    if !missing.is_empty() {
        return Err(runtime(missing_methods(&missing)));
    }
    Ok(())
}

fn event_versions(described: &SystemDescribeResult) -> Result<(), (Outdated, String)> {
    let Some(events) = &described.events else {
        return Err(runtime("没有声明事件协议版本".to_string()));
    };
    let version = events.protocol_version;
    if version < MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION {
        return Err(runtime(format!(
            "事件协议 v{version}，客户端至少要 v{MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION}"
        )));
    }
    if version > EVENT_BUS_PROTOCOL_VERSION {
        return Err(client(format!(
            "事件协议 v{version}，客户端只认到 v{EVENT_BUS_PROTOCOL_VERSION}"
        )));
    }
    Ok(())
}

fn runtime(reason: String) -> (Outdated, String) {
    (Outdated::Runtime, reason)
}

fn client(reason: String) -> (Outdated, String) {
    (Outdated::Client, reason)
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
    use smelt_event_protocol::{
        CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION, CORE_SESSION_STATE_SCHEMA_VERSION,
    };

    use crate::protocol::control::{
        CONTROL_API_MIN_SUPPORTED_VERSION, ControlProtocolDescription, DaemonRuntimeInfo,
        EventProtocolDescription,
    };

    use super::*;

    /// 和这个客户端同一份构建的运行时会回的 describe。
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

    /// 最低协议版本之后才加的方法，旧运行时没有也照常连：只是那一项功能用不了。
    #[test]
    fn missing_optional_methods_are_still_compatible() {
        let mut described = current();
        described.control.methods.retain(|method| {
            ControlMethod::from_wire_name(method).is_none_or(ControlMethod::required)
        });
        assert!(
            !ControlMethod::OPTIONAL.is_empty(),
            "这条测试要真的去掉了可选方法才有意义"
        );
        assert_eq!(check(&described), Ok(()));
    }

    #[test]
    fn control_version_range_decides_the_direction() {
        let mut newer = current();
        newer.control.min_supported_version = CONTROL_API_VERSION + 1;
        newer.control.max_supported_version = CONTROL_API_VERSION + 1;
        assert_eq!(outdated(&newer), Outdated::Client);

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

    /// 事件总线协议落在客户端支持的区间外才拦，按高低说清该升级哪一边。
    #[test]
    fn event_protocol_outside_the_supported_range_decides_the_direction() {
        let mut newer = current();
        newer.events.as_mut().unwrap().protocol_version = EVENT_BUS_PROTOCOL_VERSION + 1;
        assert_eq!(outdated(&newer), Outdated::Client);

        let mut older = current();
        older.events.as_mut().unwrap().protocol_version =
            MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION - 1;
        assert_eq!(outdated(&older), Outdated::Runtime);
    }

    /// 投影版本号冻结了，不再参与判断：载荷宽容读取，运行时比客户端新也读得了。
    #[test]
    fn projection_versions_no_longer_decide_compatibility() {
        let mut described = current();
        let events = described.events.as_mut().unwrap();
        events.projection_version = CORE_PROJECTION_VERSION + 1;
        events.session_projection_version = CORE_SESSION_PROJECTION_VERSION + 1;
        events.session_state_schema_version = CORE_SESSION_STATE_SCHEMA_VERSION + 1;
        assert_eq!(check(&described), Ok(()));
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

    /// 手机只对事件：桌面的必需方法缺了不拦，事件协议对不上照样拦。
    #[test]
    fn check_events_ignores_control_methods_but_not_event_versions() {
        let mut described = current();
        described.control.methods.clear();
        assert_eq!(check_events(&described), Ok(()));

        let mut older = current();
        older.events.as_mut().unwrap().protocol_version = EVENT_BUS_PROTOCOL_VERSION - 1;
        let error = check_events(&older).unwrap_err();
        assert_eq!(error.outdated, Outdated::Runtime);
        assert!(error.reason.contains("事件协议"), "{}", error.reason);

        let mut newer = current();
        newer.events.as_mut().unwrap().protocol_version = EVENT_BUS_PROTOCOL_VERSION + 1;
        assert_eq!(check_events(&newer).unwrap_err().outdated, Outdated::Client);
    }
}
