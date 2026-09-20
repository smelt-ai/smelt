//! 交接 v2 manifest：版本化、分级、校验。纯数据 + 编解码，无 IO。
//!
//! 分级：MUST（本 manifest，整体 sha256 校验，坏则整个交接 ABORT、老进程回滚
//! 继续服务）vs BEST-EFFORT（grid 字节随后单独传输、各自校验；损坏只丢画面
//! 不丢会话——"无 grid 空 Term + jolt"路径恢复侧本来就有）。
//!
//! fd 传递：manifest 只声明角色与顺序（[`FdRole`]），字节随后经 `SCM_RIGHTS`
//! 到达；transport 按声明顺序认领，多一个少一个都是 ABORT。

use std::collections::{HashMap, HashSet};

/// 帧魔数 + 协议版本 + manifest 长度 + manifest sha256 的总头长。
pub const HANDOFF_MAGIC: &[u8; 4] = b"SMHD";
pub const HANDOFF_PROTO_VERSION: u32 = 1;
pub const HANDOFF_FRAME_HEADER_LEN: usize = 4 + 4 + 8 + 32;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProducerInfo {
    pub version: String,
    pub pid: u32,
}

/// `SCM_RIGHTS` 到达顺序 = [`HandoffManifest::fd_roles`] 的声明顺序。
/// 两端同一份代码，顺序错一位就 ABORT，不猜。
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FdRole {
    Listen,
    TerminalMaster { session_id: String },
    AcpStdin { session_id: String },
    AcpStdout { session_id: String },
    AcpHost { session_id: String },
}

impl std::fmt::Display for FdRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FdRole::Listen => write!(f, "listen"),
            FdRole::TerminalMaster { session_id } => write!(f, "term:{session_id}"),
            FdRole::AcpStdin { session_id } => write!(f, "acp-stdin:{session_id}"),
            FdRole::AcpStdout { session_id } => write!(f, "acp-stdout:{session_id}"),
            FdRole::AcpHost { session_id } => write!(f, "acp-host:{session_id}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalChildHandoff {
    /// 仍活着：successor 接管 fd 后订阅退出监控。pid 恒 >0（validate 强制；
    /// predecessor 跳过 pid<=0 的会话并打日志——与旧版"关 fd 跳过"同结局）。
    Live { pid: i32 },
    /// 交接窗口内已退出：predecessor（仍是父进程）已 reap，无需监控——
    /// 但 master fd 照传：会话恢复成"已结束"（scrollback 完整，pump 读 EOF
    /// 优雅收尾），与旧版行为一致。直接丢会话才是回归。
    /// `pid` 是已回收的 stale pid，仅记录展示：Finished 状态永不 wait/kill，
    /// 即使号码已被复用也安全。
    ExitedDuringHandoff { pid: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalHandoff {
    pub id: String,
    pub child: TerminalChildHandoff,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<String>,
    pub launch: Option<String>,
    pub agent_mcp: bool,
    pub agent_token: String,
    pub alt_screen: bool,
}

/// ACP 两种运行时形态。`ConversationSnapshot` 等 smelt-core 类型只带
/// Clone/Debug（无 PartialEq），本 enum 同样不派生 PartialEq，单测走 JSON
/// 往返比对。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "runtime", rename_all = "snake_case")]
pub enum AcpHandoff {
    Hosted {
        id: String,
        host_pid: i32,
        provider_pid: Option<i32>,
        host_snapshot_revision: u64,
        cwd: Option<String>,
        launch: smelt_core::agent_kind::ConversationLaunchSpec,
        agent_mcp: bool,
        agent_token: String,
        agent_needs_transcript_check: bool,
        runtime_spec_fingerprint: Option<String>,
        conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
        snapshot: smelt_core::acp_session::ConversationSnapshot,
    },
    Direct {
        id: String,
        pid: i32,
        cwd: Option<String>,
        launch: smelt_core::agent_kind::ConversationLaunchSpec,
        agent_mcp: bool,
        agent_token: String,
        agent_needs_transcript_check: bool,
        runtime_spec_fingerprint: Option<String>,
        conversation_binding: Option<smelt_core::conversation::ConversationBinding>,
        snapshot: smelt_core::acp_session::ConversationSnapshot,
        pending_raw_line: Option<String>,
    },
}

impl AcpHandoff {
    pub fn id(&self) -> &str {
        match self {
            AcpHandoff::Hosted { id, .. } | AcpHandoff::Direct { id, .. } => id,
        }
    }
}

/// Grid 字节的引用：manifest 只声明，字节随后单独传输。
/// 损坏/缺失只丢画面不丢会话（恢复侧走"无 grid 空 Term + jolt"）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GridRef {
    pub session_id: String,
    pub len: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HandoffManifest {
    pub producer: ProducerInfo,
    /// 快照时刻（wall clock unix 毫秒，前任收集时盖章）。successor 清理失败项
    /// 动刀前验 pid 身份用：启动早于此时刻的才是原进程（复用只能发生在原进程
    /// 死后，即此时刻之后——因果律）。0=未知（老帧/legacy），恢复侧回退到
    /// 恢复入口时刻。
    #[serde(default)]
    pub snapshot_wall_ms: u64,
    pub fd_roles: Vec<FdRole>,
    pub sessions: Vec<TerminalHandoff>,
    pub acp: Vec<AcpHandoff>,
    pub menu_gui_pids: Vec<i32>,
    pub grids: Vec<GridRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    EmptyProducerVersion,
    EmptyId,
    DuplicateSessionId(String),
    DuplicateAcpId(String),
    MissingListenRole,
    SessionWithoutFd(String),
    InvalidChildPid(String),
    AcpWithoutFd(String),
    DanglingFdRole(String),
    DuplicateFdRole(String),
    DanglingGrid(String),
    DuplicateGrid(String),
    EmptyGrid(String),
    InvalidMenuPid(i32),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::EmptyProducerVersion => write!(f, "producer.version 为空"),
            ManifestError::EmptyId => write!(f, "会话 id 为空"),
            ManifestError::DuplicateSessionId(id) => write!(f, "终端会话 id 重复：{id}"),
            ManifestError::DuplicateAcpId(id) => write!(f, "ACP 会话 id 重复：{id}"),
            ManifestError::MissingListenRole => {
                write!(f, "fd_roles 缺少 listen（须恰好一个且首位）")
            }
            ManifestError::SessionWithoutFd(id) => {
                write!(f, "终端会话缺 master fd 角色：{id}")
            }
            ManifestError::InvalidChildPid(id) => {
                write!(f, "Live 终端会话 pid 非法：{id}")
            }
            ManifestError::AcpWithoutFd(id) => write!(f, "ACP 会话缺 fd 角色：{id}"),
            ManifestError::DanglingFdRole(role) => write!(f, "fd 角色无对应条目：{role}"),
            ManifestError::DuplicateFdRole(role) => write!(f, "fd 角色重复：{role}"),
            ManifestError::DanglingGrid(id) => write!(f, "grid 引用了不存在的终端会话：{id}"),
            ManifestError::DuplicateGrid(id) => write!(f, "终端会话 grid 重复：{id}"),
            ManifestError::EmptyGrid(id) => write!(f, "grid 长度为 0（应省略）：{id}"),
            ManifestError::InvalidMenuPid(pid) => write!(f, "menu_gui_pid 非法：{pid}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// 两端同一份代码：严格校验，不猜不兜底。任何一项失败都是 ABORT + 回滚。
pub fn validate(manifest: &HandoffManifest) -> Result<(), ManifestError> {
    if manifest.producer.version.trim().is_empty() {
        return Err(ManifestError::EmptyProducerVersion);
    }

    let mut session_ids = HashSet::new();
    for session in &manifest.sessions {
        if session.id.is_empty() {
            return Err(ManifestError::EmptyId);
        }
        if !session_ids.insert(session.id.as_str()) {
            return Err(ManifestError::DuplicateSessionId(session.id.clone()));
        }
    }
    let mut acp_ids = HashSet::new();
    for item in &manifest.acp {
        if item.id().is_empty() {
            return Err(ManifestError::EmptyId);
        }
        if !acp_ids.insert(item.id()) {
            return Err(ManifestError::DuplicateAcpId(item.id().to_string()));
        }
    }

    // 期望的角色集合：listen 恰好一个且首位；每个终端会话（Live/已退出都
    // 一样，见 TerminalChildHandoff）各一个 master；hosted ACP 一个 host；
    // direct ACP 一对 stdin/stdout。
    let mut expected: HashSet<FdRole> = HashSet::new();
    expected.insert(FdRole::Listen);
    for session in &manifest.sessions {
        match &session.child {
            TerminalChildHandoff::Live { pid }
            | TerminalChildHandoff::ExitedDuringHandoff { pid } => {
                if *pid <= 1 {
                    return Err(ManifestError::InvalidChildPid(session.id.clone()));
                }
            }
        }
        expected.insert(FdRole::TerminalMaster {
            session_id: session.id.clone(),
        });
    }
    for item in &manifest.acp {
        match item {
            AcpHandoff::Hosted { id, .. } => {
                expected.insert(FdRole::AcpHost {
                    session_id: id.clone(),
                });
            }
            AcpHandoff::Direct { id, .. } => {
                expected.insert(FdRole::AcpStdin {
                    session_id: id.clone(),
                });
                expected.insert(FdRole::AcpStdout {
                    session_id: id.clone(),
                });
            }
        }
    }

    if manifest.fd_roles.first() != Some(&FdRole::Listen)
        || manifest
            .fd_roles
            .iter()
            .filter(|role| **role == FdRole::Listen)
            .count()
            != 1
    {
        return Err(ManifestError::MissingListenRole);
    }
    let mut seen_roles = HashSet::new();
    for role in &manifest.fd_roles {
        if !seen_roles.insert(role.clone()) {
            return Err(ManifestError::DuplicateFdRole(role.to_string()));
        }
        if !expected.contains(role) {
            return Err(ManifestError::DanglingFdRole(role.to_string()));
        }
    }
    // 反向：每个终端条目必须有角色（悬空检查只覆盖了"角色→条目"方向）。
    for session in &manifest.sessions {
        if !seen_roles.contains(&FdRole::TerminalMaster {
            session_id: session.id.clone(),
        }) {
            return Err(ManifestError::SessionWithoutFd(session.id.clone()));
        }
    }
    for item in &manifest.acp {
        let has_roles = match item {
            AcpHandoff::Hosted { id, .. } => seen_roles.contains(&FdRole::AcpHost {
                session_id: id.clone(),
            }),
            AcpHandoff::Direct { id, .. } => {
                seen_roles.contains(&FdRole::AcpStdin {
                    session_id: id.clone(),
                }) && seen_roles.contains(&FdRole::AcpStdout {
                    session_id: id.clone(),
                })
            }
        };
        if !has_roles {
            return Err(ManifestError::AcpWithoutFd(item.id().to_string()));
        }
    }

    let mut grid_sessions = HashSet::new();
    for grid in &manifest.grids {
        if grid.len == 0 {
            return Err(ManifestError::EmptyGrid(grid.session_id.clone()));
        }
        if !session_ids.contains(grid.session_id.as_str()) {
            return Err(ManifestError::DanglingGrid(grid.session_id.clone()));
        }
        if !grid_sessions.insert(grid.session_id.as_str()) {
            return Err(ManifestError::DuplicateGrid(grid.session_id.clone()));
        }
    }

    for pid in &manifest.menu_gui_pids {
        if *pid <= 1 {
            return Err(ManifestError::InvalidMenuPid(*pid));
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum FrameError {
    TooShort { need: usize, got: usize },
    BadMagic { got: [u8; 4] },
    UnsupportedVersion { got: u32 },
    LengthMismatch { declared: u64, available: usize },
    ChecksumMismatch,
    Json(serde_json::Error),
    Invalid(ManifestError),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooShort { need, got } => {
                write!(f, "帧太短：要 {need} 字节，实得 {got}")
            }
            FrameError::BadMagic { got } => write!(f, "魔数错误：{got:02x?}"),
            FrameError::UnsupportedVersion { got } => {
                write!(
                    f,
                    "交接协议版本不支持：对端 {got}，本端 {HANDOFF_PROTO_VERSION}"
                )
            }
            FrameError::LengthMismatch {
                declared,
                available,
            } => {
                write!(f, "manifest 长度声明 {declared}，实到 {available}")
            }
            FrameError::ChecksumMismatch => write!(f, "manifest sha256 校验失败"),
            FrameError::Json(error) => write!(f, "manifest JSON 解析失败：{error}"),
            FrameError::Invalid(error) => write!(f, "manifest 非法：{error}"),
        }
    }
}

impl std::error::Error for FrameError {}

pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    to_hex(&Sha256::digest(bytes))
}

/// MAGIC + ver(u32le) + len(u64le) + sha256(32B) + json。
pub fn encode_frame(manifest: &HandoffManifest) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(manifest)?;
    let mut out = Vec::with_capacity(HANDOFF_FRAME_HEADER_LEN + json.len());
    out.extend_from_slice(HANDOFF_MAGIC);
    out.extend_from_slice(&HANDOFF_PROTO_VERSION.to_le_bytes());
    out.extend_from_slice(&(json.len() as u64).to_le_bytes());
    {
        use sha2::{Digest, Sha256};
        out.extend_from_slice(&Sha256::digest(&json));
    }
    out.extend_from_slice(&json);
    Ok(out)
}

/// 解一帧，返回 (manifest, 消费字节数)。成功必经 [`validate`]。
pub fn decode_frame(bytes: &[u8]) -> Result<(HandoffManifest, usize), FrameError> {
    if bytes.len() < HANDOFF_FRAME_HEADER_LEN {
        return Err(FrameError::TooShort {
            need: HANDOFF_FRAME_HEADER_LEN,
            got: bytes.len(),
        });
    }
    if &bytes[..4] != HANDOFF_MAGIC {
        let mut got = [0u8; 4];
        got.copy_from_slice(&bytes[..4]);
        return Err(FrameError::BadMagic { got });
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 字节版本号"));
    if version != HANDOFF_PROTO_VERSION {
        return Err(FrameError::UnsupportedVersion { got: version });
    }
    let len = u64::from_le_bytes(bytes[8..16].try_into().expect("8 字节长度"));
    let len_usize: usize = len.try_into().unwrap_or(usize::MAX);
    let total = HANDOFF_FRAME_HEADER_LEN + len_usize;
    if bytes.len() < total {
        return Err(FrameError::LengthMismatch {
            declared: len,
            available: bytes.len() - HANDOFF_FRAME_HEADER_LEN,
        });
    }
    let body = &bytes[HANDOFF_FRAME_HEADER_LEN..total];
    {
        use sha2::{Digest, Sha256};
        if Sha256::digest(body).as_slice() != &bytes[16..HANDOFF_FRAME_HEADER_LEN] {
            return Err(FrameError::ChecksumMismatch);
        }
    }
    let manifest: HandoffManifest = serde_json::from_slice(body).map_err(FrameError::Json)?;
    validate(&manifest).map_err(FrameError::Invalid)?;
    Ok((manifest, total))
}

/// 按声明顺序把收到的 fd 对号入座。transport 保证数量一致，这里只做映射。
pub fn assign_fds(
    roles: &[FdRole],
    fds: Vec<std::os::fd::OwnedFd>,
) -> HashMap<FdRole, std::os::fd::OwnedFd> {
    roles.iter().cloned().zip(fds).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn producer() -> ProducerInfo {
        ProducerInfo {
            version: "0.9.0".to_string(),
            pid: 4242,
        }
    }

    fn live_session(id: &str) -> TerminalHandoff {
        TerminalHandoff {
            id: id.to_string(),
            child: TerminalChildHandoff::Live { pid: 100 },
            cols: 80,
            rows: 24,
            cwd: Some("/repo".to_string()),
            launch: None,
            agent_mcp: false,
            agent_token: "tok".to_string(),
            alt_screen: false,
        }
    }

    fn snapshot_fixture() -> smelt_core::acp_session::ConversationSnapshot {
        // 最小合法快照：必填字段给空值，其余走 default。
        serde_json::from_value(serde_json::json!({
            "entries": [],
            "phase": "Idle",
            "pending_elicitation": null,
            "status_line": null,
            "acp_session_id": null,
            "supports_image": false,
            "available_commands": [],
            "usage": null,
            "plan": null,
            "model": null,
            "config_options": [],
            "completed_unread": false,
            "should_persist": false,
        }))
        .expect("最小快照应合法")
    }

    fn launch_fixture() -> smelt_core::agent_kind::ConversationLaunchSpec {
        smelt_core::agent_kind::ConversationLaunchSpec::from_command("test-agent --flag")
    }

    fn hosted_acp(id: &str) -> AcpHandoff {
        AcpHandoff::Hosted {
            id: id.to_string(),
            host_pid: 200,
            provider_pid: Some(201),
            host_snapshot_revision: 7,
            cwd: None,
            launch: launch_fixture(),
            agent_mcp: true,
            agent_token: "tok".to_string(),
            agent_needs_transcript_check: false,
            runtime_spec_fingerprint: None,
            conversation_binding: None,
            snapshot: snapshot_fixture(),
        }
    }

    fn direct_acp(id: &str) -> AcpHandoff {
        AcpHandoff::Direct {
            id: id.to_string(),
            pid: 300,
            cwd: None,
            launch: launch_fixture(),
            agent_mcp: false,
            agent_token: String::new(),
            agent_needs_transcript_check: true,
            runtime_spec_fingerprint: Some("fp".to_string()),
            conversation_binding: None,
            snapshot: snapshot_fixture(),
            pending_raw_line: None,
        }
    }

    fn full_manifest() -> HandoffManifest {
        HandoffManifest {
            producer: producer(),
            snapshot_wall_ms: 0,
            fd_roles: vec![
                FdRole::Listen,
                FdRole::TerminalMaster {
                    session_id: "t1".to_string(),
                },
                // 已退出会话照样占一个 master 角色（恢复成"已结束"，不断会话）。
                FdRole::TerminalMaster {
                    session_id: "t-exited".to_string(),
                },
                FdRole::AcpHost {
                    session_id: "a1".to_string(),
                },
                FdRole::AcpStdin {
                    session_id: "a2".to_string(),
                },
                FdRole::AcpStdout {
                    session_id: "a2".to_string(),
                },
            ],
            sessions: vec![
                live_session("t1"),
                TerminalHandoff {
                    id: "t-exited".to_string(),
                    child: TerminalChildHandoff::ExitedDuringHandoff { pid: 101 },
                    ..live_session("ignored")
                },
            ],
            acp: vec![hosted_acp("a1"), direct_acp("a2")],
            menu_gui_pids: vec![400],
            grids: vec![GridRef {
                session_id: "t1".to_string(),
                len: 11,
                sha256: sha256_hex(b"hello world"),
            }],
        }
    }

    #[test]
    fn empty_manifest_round_trips() {
        let manifest = HandoffManifest {
            producer: producer(),
            snapshot_wall_ms: 0,
            fd_roles: vec![FdRole::Listen],
            sessions: Vec::new(),
            acp: Vec::new(),
            menu_gui_pids: Vec::new(),
            grids: Vec::new(),
        };
        let bytes = encode_frame(&manifest).unwrap();
        let (decoded, consumed) = decode_frame(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            serde_json::to_value(&manifest).unwrap()
        );
    }

    #[test]
    fn full_manifest_round_trips() {
        let manifest = full_manifest();
        let bytes = encode_frame(&manifest).unwrap();
        let (decoded, consumed) = decode_frame(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            serde_json::to_value(&manifest).unwrap()
        );
        // 结构化字段穿过去没丢。
        assert_eq!(decoded.sessions.len(), 2);
        assert_eq!(decoded.acp.len(), 2);
        assert!(matches!(
            decoded.sessions[1].child,
            TerminalChildHandoff::ExitedDuringHandoff { pid: 101 }
        ));
    }

    #[test]
    fn decode_rejects_bad_magic_version_checksum_truncation() {
        let bytes = encode_frame(&full_manifest()).unwrap();

        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            decode_frame(&bad_magic),
            Err(FrameError::BadMagic { .. })
        ));

        let mut bad_version = bytes.clone();
        bad_version[4] = 99;
        assert!(matches!(
            decode_frame(&bad_version),
            Err(FrameError::UnsupportedVersion { got: 99 })
        ));

        let mut bad_body = bytes.clone();
        let last = bad_body.len() - 1;
        bad_body[last] ^= 0xff;
        assert!(matches!(
            decode_frame(&bad_body),
            Err(FrameError::ChecksumMismatch)
        ));

        assert!(matches!(
            decode_frame(&bytes[..HANDOFF_FRAME_HEADER_LEN - 1]),
            Err(FrameError::TooShort { .. })
        ));
        assert!(matches!(
            decode_frame(&bytes[..bytes.len() - 1]),
            Err(FrameError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn decode_surfaces_validation_failures() {
        let mut manifest = full_manifest();
        manifest.fd_roles.remove(1); // 抽掉 t1 的 master 角色
        let bytes = encode_frame(&manifest).unwrap();
        assert!(matches!(
            decode_frame(&bytes),
            Err(FrameError::Invalid(ManifestError::SessionWithoutFd(_)))
        ));
    }

    #[test]
    fn validate_catches_shape_errors() {
        // listen 缺失 / 不在首位。
        let mut m = full_manifest();
        m.fd_roles.remove(0);
        assert_eq!(validate(&m), Err(ManifestError::MissingListenRole));

        // 重复 id。
        let mut m = full_manifest();
        m.sessions.push(live_session("t1"));
        assert_eq!(
            validate(&m),
            Err(ManifestError::DuplicateSessionId("t1".to_string()))
        );

        // 已退出会话缺 master 角色同样非法（必须恢复成"已结束"）。
        let mut m = full_manifest();
        m.fd_roles.retain(|role| {
            *role
                != FdRole::TerminalMaster {
                    session_id: "t-exited".to_string(),
                }
        });
        assert_eq!(
            validate(&m),
            Err(ManifestError::SessionWithoutFd("t-exited".to_string()))
        );

        // grid 悬空 / 空 grid。
        let mut m = full_manifest();
        m.grids.push(GridRef {
            session_id: "ghost".to_string(),
            len: 1,
            sha256: String::new(),
        });
        assert_eq!(
            validate(&m),
            Err(ManifestError::DanglingGrid("ghost".to_string()))
        );
        let mut m = full_manifest();
        m.grids[0].len = 0;
        assert_eq!(
            validate(&m),
            Err(ManifestError::EmptyGrid("t1".to_string()))
        );

        // 非法 menu pid。
        let mut m = full_manifest();
        m.menu_gui_pids.push(1);
        assert_eq!(validate(&m), Err(ManifestError::InvalidMenuPid(1)));

        // Live 会话 pid 非法。
        let mut m = full_manifest();
        m.sessions[0].child = TerminalChildHandoff::Live { pid: 0 };
        assert_eq!(
            validate(&m),
            Err(ManifestError::InvalidChildPid("t1".to_string()))
        );

        // 空 producer 版本。
        let mut m = full_manifest();
        m.producer.version.clear();
        assert_eq!(validate(&m), Err(ManifestError::EmptyProducerVersion));
    }
}
