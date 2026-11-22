//! 运行时连接：这台桌面配对了哪些远程机器、各自连得怎样，以及加、移除、改名、改中继、
//! 重连、升级。
//!
//! 连接表归宿主：它据此拨号、订阅各台机器的会话，存在客户端库里。插件经
//! [`crate::Host::connections`] 读，经这里的命令改；变了发 [`crate::HostEvent::MachinesChanged`]。
//! 本机不在这张表里：本机运行时总在，不用配对、不能移除。

use serde::{Deserialize, Serialize};

use crate::{Command, RuntimeKey, machine_json};

/// 远程机器的连接表，见 [`crate::Host::connections`]。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connections {
    /// 配对过的远程机器，按添加的先后。
    pub remotes: Vec<Connection>,
    /// 启动时没读出登记过的机器（比如客户端库比这个程序新）的原因，给人看。这时表是空的，
    /// 不说一声就像机器全丢了。
    pub load_error: Option<String>,
}

/// 一台远程机器的连接。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 在这台电脑上的显示名。
    pub name: String,
    /// 这台电脑经哪个中继连它（配对时记下、或之后改过的）；`None` 是 n0 的公共 relay。
    pub relay: Option<String>,
    /// 它跑的系统（`linux`、`macos`、`windows`），最近一次连上时报的、宿主记着的那份：离线、
    /// 连接中也在。从没连上过是 `None`。
    pub os: Option<String>,
    pub state: ConnectionState,
    /// 连着时走哪条路、多慢；没连着、还没读过是 `None`。只在有人看时采样
    /// （[`REFRESH_CONNECTIONS`]），平时只在 iroh 换路径时更新。
    pub link: Option<ConnectionLink>,
    /// 最近一次手动升级没升成的原因。
    pub upgrade_note: Option<String>,
    /// 能请它升级（[`UPGRADE_MACHINE`]）：连得上、它的运行时能远程升级、这个桌面是发版构建。
    pub can_upgrade: bool,
    /// 能请宿主马上重连（[`RECONNECT_MACHINE`]）：离线或版本对不上。
    pub can_reconnect: bool,
    /// 这台电脑上的智能体能不能经本机运行时使用它（[`SET_MACHINE_AGENT_ACCESS`]）。`None` 是
    /// 还没迁到本机运行时的旧直连机器，没有这个设置。
    #[serde(default)]
    pub agent_access: Option<bool>,
}

/// 最近一次探测的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    Connecting,
    Online {
        version: String,
        /// 如 `linux/x86_64`；旧运行时不报。
        platform: Option<String>,
    },
    Offline {
        error: String,
    },
    /// 连得上，版本和这个桌面对不上；`detail` 说该升级哪一边。
    Incompatible {
        version: String,
        detail: String,
    },
    Upgrading {
        progress: String,
    },
}

/// 连接走哪条路、多慢。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionLink {
    pub route: LinkRoute,
    pub rtt_ms: u32,
    /// 慢到值得提醒：终端敲一个字要等这么久才回显。门槛归宿主，侧栏和设置页一个标准。
    pub slow: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkRoute {
    /// 点对点直连。
    Direct,
    /// 经中继转发，带着给人看的中继名。
    Relay { relay: String },
    /// 别的传输。
    Other,
}

/// 用配对串添加一台远程机器：以这台电脑的主机名和它配对，问出它的身份记下来。同一台
/// 机器重新配对就更新原来那条。要走公网拨号，最长等半分钟。
pub const ADD_MACHINE: Command<AddMachine, AddedMachine> = Command::new("smelt.app/add-machine");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddMachine {
    /// `smelt+iroh://…`，在那台机器上 `smeltd pair` 或它的远程设置里生成，只能用一次。
    pub pairing_uri: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedMachine {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub name: String,
}

/// 移除一台远程机器：这台电脑不再连它，它的项目和会话从这边一起移走；它上面的会话照常
/// 运行。在线时顺手请它撤销这台电脑的配对，离线的不等。
pub const REMOVE_MACHINE: Command<MachineTarget, ()> = Command::new("smelt.app/remove-machine");

/// 请宿主马上重连一台离线或版本对不上的机器，不等下一次退避。
pub const RECONNECT_MACHINE: Command<MachineTarget, ()> =
    Command::new("smelt.app/reconnect-machine");

/// 请一台机器把运行时升到这个桌面的版本。进度和结果看 [`Connection::state`]、
/// [`Connection::upgrade_note`]。
pub const UPGRADE_MACHINE: Command<MachineTarget, ()> = Command::new("smelt.app/upgrade-machine");

/// 哪台远程机器。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTarget {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
}

/// 改一台远程机器在这台电脑上的显示名，只在这台电脑上生效。
pub const RENAME_MACHINE: Command<RenameMachine, ()> = Command::new("smelt.app/rename-machine");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameMachine {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub name: String,
}

/// 改连一台远程机器用的中继。在线时先请它自己改用新中继，成了再改这边记下的；离线时只改
/// 这边（那台机器要已经用 `smeltd relay` 改成同一个）。改完马上重连。
pub const SET_MACHINE_RELAY: Command<SetMachineRelay, ()> =
    Command::new("smelt.app/set-machine-relay");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMachineRelay {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 自建 iroh relay 的域名、IP 或 URL；空的是 n0 的公共 relay。
    pub relay: String,
}

/// 允不允许这台电脑上的智能体（经 `smelt --machine`）使用一台远程机器。默认不允许。
pub const SET_MACHINE_AGENT_ACCESS: Command<SetMachineAgentAccess, ()> =
    Command::new("smelt.app/set-machine-agent-access");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMachineAgentAccess {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub allowed: bool,
}

/// 有人在看连接状态了：现在重读各台连着的机器走哪条路、多慢（不拨号），离线的叫宿主现在
/// 探一次。看着连接状态的界面露出来时调一次，不要定时调。
pub const REFRESH_CONNECTIONS: Command<(), ()> = Command::new("smelt.app/refresh-connections");
