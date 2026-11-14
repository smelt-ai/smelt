//! 客户端（桌面、手机）和运行时的版本协商（设计文档 D7）。连上运行时先读
//! `system.describe`，对照控制协议和事件协议的版本，对不上就是不兼容，并说清该升级哪一边。
//! 不对的话，订阅被拒或解不出投影，重订没有尽头，会话目录永远到不了，界面只能一直
//! 「同步中」。说明里带着运行时的版本，看得出它是哪个版本。
//!
//! 不看方法清单：方法只增不改，运行时缺某个方法只是那项功能还没有，用到时调到
//! MethodNotFound 就报「运行时较旧」，不为此把整台机器拦在外面。客户端离了就没法用的方法
//! 靠版本下限保证：事件协议 v6 起的运行时都有工作区目录那几个方法；以后再有这种方法，抬
//! 控制协议的最低版本，不另列必需方法。

use std::fmt;

use smelt_event_protocol::{EVENT_BUS_PROTOCOL_VERSION, MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION};

use crate::protocol::control::{CONTROL_API_VERSION, SystemDescribeResult};

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

/// 桌面能不能用这个运行时：控制协议加事件（[`check_events`]）。运行时支持的控制协议
/// 版本区间要包含客户端的：版本只在已有方法换了形状时升，对不上就是同名方法读不懂。
pub fn check(described: &SystemDescribeResult) -> Result<(), Incompatibility> {
    with_runtime_version(
        described,
        check_control(described).and_then(|()| event_versions(described)),
    )
}

/// 订阅解不解得了运行时推来的状态：只看事件总线协议版本落不落在客户端支持的区间里。载荷
/// 宽容读取，推送加字段不升版本，所以这类改动下运行时比客户端新、旧都照常连（见
/// `smelt_event_protocol` 的模块文档）。
///
/// 手机只对这一项：它只用少数几个控制方法，调不通由用到的地方报错。
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

#[cfg(test)]
mod tests {
    use smelt_event_protocol::{
        CORE_PROJECTION_VERSION, CORE_SESSION_PROJECTION_VERSION, CORE_SESSION_STATE_SCHEMA_VERSION,
    };

    use crate::protocol::control::{
        CONTROL_API_MIN_SUPPORTED_VERSION, ControlMethod, ControlProtocolDescription,
        DaemonRuntimeInfo, EventProtocolDescription,
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

    /// 缺方法不拦：2026.1031.0 缺后来加的 `pi.mcp.login_start`，整台机器被判成不兼容，终端和
    /// 对话都用不了，其实只是 MCP 登录还没有。缺的方法由用到的地方报错。
    #[test]
    fn missing_methods_never_make_the_runtime_incompatible() {
        let mut described = current();
        described
            .control
            .methods
            .retain(|method| method != "pi.mcp.login_start");
        assert_eq!(check(&described), Ok(()));

        described.control.methods.clear();
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

    /// 没升级的运行时读得了就照常连：这里写死已发布的 2026.1031.0（控制 v3、事件 v6），谁抬
    /// 最低版本，这条就失败，得想清楚是不是真读不了。
    #[test]
    fn the_released_runtime_baseline_stays_compatible() {
        let mut baseline = current();
        baseline
            .control
            .methods
            .retain(|method| method != ControlMethod::PiMcpLoginStart.as_str());
        baseline.control.current_version = 3;
        baseline.control.min_supported_version = 3;
        baseline.control.max_supported_version = 3;
        let events = baseline.events.as_mut().unwrap();
        events.protocol_version = 6;
        assert_eq!(check(&baseline), Ok(()));
    }

    /// 手机只对事件：控制协议对不上不拦，事件协议对不上照样拦。
    #[test]
    fn check_events_ignores_the_control_version_but_not_event_versions() {
        let mut described = current();
        described.control.min_supported_version = CONTROL_API_VERSION + 1;
        described.control.max_supported_version = CONTROL_API_VERSION + 1;
        assert!(check(&described).is_err());
        assert_eq!(check_events(&described), Ok(()));

        let mut older = current();
        older.events.as_mut().unwrap().protocol_version =
            MIN_SUPPORTED_EVENT_BUS_PROTOCOL_VERSION - 1;
        let error = check_events(&older).unwrap_err();
        assert_eq!(error.outdated, Outdated::Runtime);
        assert!(error.reason.contains("事件协议"), "{}", error.reason);

        let mut newer = current();
        newer.events.as_mut().unwrap().protocol_version = EVENT_BUS_PROTOCOL_VERSION + 1;
        assert_eq!(check_events(&newer).unwrap_err().outdated, Outdated::Client);
    }
}
