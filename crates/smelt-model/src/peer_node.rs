//! 对端节点：和本节点双向配对过的其它运行时（见 `docs/cross-machine.md`）。
//!
//! 一台电脑是一个节点，身份是运行时的 iroh 密钥。两台节点配对后各自记下对方：怎么连、
//! 这边怎么称呼它、本机智能体能不能经它出去。

use serde::{Deserialize, Serialize};

/// 节点自报的身份，配对时双方互换。`name` 是对方的机器名，只作首次显示名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub runtime_id: String,
    pub name: String,
    /// 对方自建 relay 的地址；`None` 走公共 relay 和地址发现。
    #[serde(default)]
    pub relay_url: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
}

/// `node.pair`：节点之间配对。对方的 endpoint id 取自连接，不在参数里。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePairParams {
    pub code: String,
    pub node: NodeInfo,
}

/// 本节点记下的一个对端。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerNode {
    pub runtime_id: String,
    pub endpoint_id: String,
    /// 这边给它起的显示名。
    pub name: String,
    #[serde(default)]
    pub relay_url: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    /// 本机智能体会话能不能经本节点转发到它。
    #[serde(default)]
    pub agent_access: bool,
    pub added_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerList {
    pub peers: Vec<PeerNode>,
}

/// `peer.pair`：用对方的配对串双向配对。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerPairParams {
    pub pairing_uri: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerIdParams {
    pub runtime_id: String,
}

/// `peer.update`：只改给了的字段。`relay_url` 给空串表示改回公共 relay。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerUpdateParams {
    pub runtime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_access: Option<bool>,
}

/// 到一个对端的连接走的哪条路。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerRoute {
    Direct,
    Relay { address: String },
    Other,
}

/// 到一个对端的当前路径和往返延迟。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerLink {
    pub route: PeerRoute,
    pub rtt_ms: u32,
}

/// `peer.link` 的结果：还没连上、连接已断时没有路径。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerLinkResult {
    #[serde(default)]
    pub link: Option<PeerLink>,
}
