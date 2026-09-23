//! 事件订阅与插件 UI 的连接身份校验。
//!
//! 身份原语只问内核，不问磁盘：team + signing-id 经 `csops_audittoken` 从
//! 进程的代码目录读（audit token 绑定，无 TOCTOU），自更新把磁盘文件换掉后
//! 照样有效——旧链条（Security framework 动态查询）在此报 -67034，先后炸过
//! 两次（`SecCodeCheckValidity`，`SecCodeCopySigningInformation` 的静态解析）。
//! Santa/Chromium 同款做法；`csops` SPI 直调（Chromium 同款 notarized DMG
//! 分发先例，公证不扫 SPI）。
//!
//! 威胁模型注记（改这里之前先读完）：
//! - 身份三段论：**身份 = audit token；分类与校验 = 绑定 token 的内核 code
//!   identity（team + signing-id）；pid/路径 = 纯诊断，best-effort**。磁盘
//!   二进制被原子 rename 换掉（自更新落盘、开发重编译）后，老进程的 vnode
//!   已从目录树摘除，`proc_pidpath` 对活进程也会 ENOENT——路径拿不到只
//!   降级日志，绝不 gating 鉴权（2026-09-18 曾因此炸过：插件 invocation
//!   全部失败，GUI 重启才自愈）。
//! - Prod（Developer ID）：只比 team + signing-id，与 Apple XPC
//!   team-identity 要求同构。故意不 pin 具体证书： pinning 逼我们读文件
//!   （证书只存在文件里）从而重引入 skew 脆弱，还在证书轮换时误杀；
//!   攻击者拿到我们签名密钥即全局沦陷，pin 哪张证书没有意义。
//! - Local（自签名，无 team）：比较双方路径上磁盘文件的静态证书。注意两边
//!   都是“磁盘当下”：StageDiskOnly 把 GUI 和 managed 一起换成新文件后，老进程
//!   照样能过（两边都是新证书）——这是刻意保留的 skew 宽容，与 Prod leg
//!   “证书轮换不误杀”一致，不强制重启。攻击者视角：没我们目录写权限就伪造
//!   不了任何一边；有该权限时直接读 sqlite/换守护二进制收益更大——socket
//!   鉴权本来也不在那条威胁模型里。
//! - Ad-hoc：debug 才认同目录兄弟，release 照样拒绝（"禁 ad-hoc 上生产"不变）。
//! - 有效性（CS_VALID 之类）不查：内核在 exec/page-in 时已强制执行非法即杀；
//!   我们只认"谁签的"，这是身份问题不是有效性问题（同 Apple team 要求的口径）。

use super::super::*;
use smelt_plugin_api::{
    CORE_CAPABILITY_AUTOMATION_READ, CORE_CAPABILITY_REMOTE_SESSIONS_READ,
    CORE_CAPABILITY_SESSION_READ, CORE_CAPABILITY_WORKSPACE_READ, Capability, EventClientAuth,
    FIRST_PARTY_DESKTOP_PLUGIN_ID, FIRST_PARTY_REMOTE_GATEWAY_PLUGIN_ID, FirstPartyClientKind,
    PluginId,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthenticatedEventClient {
    pub(crate) plugin_id: PluginId,
    pub(crate) capabilities: BTreeSet<Capability>,
}

/// macOS 上标识对端进程的 audit token。
///
/// 只用 pid 做签名查询存在 TOCTOU：取到 pid 与查询签名之间进程可能已退出，
/// pid 被内核回收并复用给另一个进程，鉴权就会落到错误的目标上。audit token
/// 携带 pidversion，能唯一锁定一次进程实例。
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PeerAuditToken(pub(crate) [u32; 8]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerIdentity {
    pub(crate) pid: u32,
    pub(crate) executable: Option<PathBuf>,
    /// 为 None 时表示内核未能提供 audit token，此时拒绝签名鉴权而不是退回 pid。
    #[cfg(target_os = "macos")]
    pub(crate) audit_token: Option<PeerAuditToken>,
}

pub(crate) trait PeerIdentityProvider {
    fn identity(&self, conn: &UnixStream) -> Result<PeerIdentity, String>;
}

pub(super) struct SystemPeerIdentityProvider;

impl PeerIdentityProvider for SystemPeerIdentityProvider {
    fn identity(&self, conn: &UnixStream) -> Result<PeerIdentity, String> {
        system_peer_identity(conn)
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KernelCodeIdentity {
    /// Apple 颁发证书的 Team ID。ad-hoc/自签名/未签名进程无 team，为 None。
    pub team: Option<String>,
    /// 代码目录里的 signing identifier（与 codesign 读到的是同一份）。
    pub signing_id: Option<String>,
}

#[cfg(target_os = "macos")]
pub(super) trait PeerCodeIdentity {
    fn kernel_identity(&self, token: PeerAuditToken) -> Result<KernelCodeIdentity, String>;
    /// 磁盘文件的静态首证书 SHA256。Ok(None) = 可读但无证书（ad-hoc）。
    /// 纯静态读取，不比对任何活进程，永不产生 -67034。
    fn static_cert_hash(&self, path: &Path) -> Result<Option<String>, String>;
}

#[cfg(target_os = "macos")]
pub(super) struct SystemPeerCodeIdentity;

#[cfg(target_os = "macos")]
impl PeerCodeIdentity for SystemPeerCodeIdentity {
    fn kernel_identity(&self, token: PeerAuditToken) -> Result<KernelCodeIdentity, String> {
        kernel_code_identity(token)
    }

    fn static_cert_hash(&self, path: &Path) -> Result<Option<String>, String> {
        static_cert_hash(path)
    }
}

/// 读取当前进程的 audit token，用于把 daemon 自身纳入同一套签名比对。
#[cfg(target_os = "macos")]
pub(super) fn self_audit_token() -> Result<PeerAuditToken, String> {
    // task_info(mach_task_self(), TASK_AUDIT_TOKEN, ...)
    const TASK_AUDIT_TOKEN: libc::c_uint = 15;
    const TASK_AUDIT_TOKEN_COUNT: libc::c_uint = 8;

    unsafe extern "C" {
        fn mach_task_self() -> libc::c_uint;
        fn task_info(
            target_task: libc::c_uint,
            flavor: libc::c_uint,
            task_info_out: *mut u32,
            task_info_outCnt: *mut libc::c_uint,
        ) -> libc::c_int;
    }

    let mut token = [0_u32; 8];
    let mut count = TASK_AUDIT_TOKEN_COUNT;
    let result = unsafe {
        task_info(
            mach_task_self(),
            TASK_AUDIT_TOKEN,
            token.as_mut_ptr(),
            &mut count,
        )
    };
    if result != 0 {
        return Err(format!(
            "cannot read audit token for the running daemon: mach error {result}"
        ));
    }
    Ok(PeerAuditToken(token))
}

#[cfg(target_os = "macos")]
pub(crate) fn log_self_kernel_identity() {
    // 启动时打一行自身内核身份：team/signing-id 进程生命期内恒定，
    // 这行是以后一切鉴权排障的锚点（磁盘被换后框架查询会撒谎，内核不会）。
    match self_audit_token().and_then(kernel_code_identity) {
        Ok(identity) => crate::dlog(&format!(
            "auth: self identity team={:?} signing-id={:?}",
            identity.team, identity.signing_id
        )),
        Err(error) => crate::dlog(&format!("auth: self identity query failed: {error}")),
    }
}

#[cfg(target_os = "macos")]
pub(super) fn kernel_code_identity(token: PeerAuditToken) -> Result<KernelCodeIdentity, String> {
    let pid = token.0[5];
    Ok(KernelCodeIdentity {
        team: csops_string(token, pid, CS_OPS_TEAMID, 256, "team")?,
        signing_id: csops_string(token, pid, CS_OPS_IDENTITY, 1024, "signing-id")?,
    })
}

// csops(2) 不在 macOS SDK 头文件里（SPI），按 xnu bsd/sys/codesign.h 自行声明。
// 操作码是内核 ABI，从不变化。Chromium（同款 notarized DMG 分发）与 Santa 同样直调。
// 公证只扫恶意代码，不扫 SPI 使用；App Store 审核才扫——我们走 DMG，不受影响。
#[cfg(target_os = "macos")]
const CS_OPS_IDENTITY: libc::c_uint = 11; // get codesign identity
#[cfg(target_os = "macos")]
const CS_OPS_TEAMID: libc::c_uint = 14; // get team id

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn csops_audittoken(
        pid: libc::pid_t,
        ops: libc::c_uint,
        useraddr: *mut libc::c_void,
        usersize: libc::size_t,
        token: *mut u32,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn csops_string(
    token: PeerAuditToken,
    pid: u32,
    ops: libc::c_uint,
    buf_len: usize,
    what: &'static str,
) -> Result<Option<String>, String> {
    let mut words = token.0;
    let mut buf = vec![0u8; buf_len];
    let result = unsafe {
        csops_audittoken(
            pid as libc::pid_t,
            ops,
            buf.as_mut_ptr().cast(),
            buf.len(),
            words.as_mut_ptr(),
        )
    };
    if result != 0 {
        // Chromium 同款语义：ENOENT=ad-hoc（无该属性），EINVAL=未签名。
        // 其余一律 fail closed（含 ESRCH：进程已死/复用，token 对不上）。
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(code) if code == libc::ENOENT || code == libc::EINVAL => Ok(None),
            errno => Err(format!(
                "csops {what} for pid {pid} failed: errno {errno:?}"
            )),
        };
    }
    parse_csops_blob(&buf)
}

/// csops 字符串类返回：8 字节头（magic + len，均大端）+ 数据。
/// Santa 同款解析，另加：长度越界/非 UTF-8 一律拒绝（fail closed）。
/// len 语义取 Santa 的（含头 + 字符串 + NUL），但解析按"头后取到第一个 NUL
/// 为止"兜底——两种布局都正确（单测覆盖）。
#[cfg(target_os = "macos")]
fn parse_csops_blob(buf: &[u8]) -> Result<Option<String>, String> {
    if buf.len() < 8 {
        return Err("csops 返回不足 8 字节".to_string());
    }
    let len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    if len > buf.len() {
        return Err(format!("csops 返回长度 {len} 越过缓冲区"));
    }
    if len <= 9 {
        return Ok(None);
    }
    let data = &buf[8..len];
    let end = data
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(data.len());
    let text = std::str::from_utf8(&data[..end]).map_err(|_| "csops 返回非 UTF-8".to_string())?;
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text.to_string()))
    }
}

/// 磁盘文件的静态首证书 SHA256。只读指定文件，不比对任何活进程，
/// 因此没有动态→静态比对链的 -67034 类错误（静态读取自身的解析失败照常报错）。
/// Ok(None) = 文件可读但无内嵌证书（ad-hoc）。
#[cfg(target_os = "macos")]
pub(super) fn static_cert_hash(path: &Path) -> Result<Option<String>, String> {
    use core_foundation::{
        array::CFArray,
        base::{CFType, TCFType},
        data::CFData,
        dictionary::CFDictionary,
        string::CFString,
        url::CFURL,
    };
    use core_foundation_sys::{
        base::{CFTypeRef, OSStatus},
        dictionary::CFDictionaryRef,
        string::CFStringRef,
        url::CFURLRef,
    };
    use security_framework_sys::{
        base::{SecCertificateRef, SecCopyErrorMessageString, errSecSuccess},
        certificate::SecCertificateCopyData,
        code_signing::{SecCSFlags, SecStaticCodeCreateWithPath, SecStaticCodeRef},
    };
    use sha2::{Digest, Sha256};
    use std::{ffi::c_void, ptr};

    const SEC_CS_SIGNING_INFORMATION: SecCSFlags = 1 << 1;

    unsafe extern "C" {
        static kSecCodeInfoCertificates: CFStringRef;

        fn SecCodeCopySigningInformation(
            code: SecStaticCodeRef,
            flags: SecCSFlags,
            information: *mut CFDictionaryRef,
        ) -> OSStatus;
    }

    fn status_error(context: &str, path: &Path, status: OSStatus) -> String {
        let detail = unsafe {
            let message = SecCopyErrorMessageString(status, ptr::null_mut());
            if message.is_null() {
                format!("OSStatus {status}")
            } else {
                CFString::wrap_under_create_rule(message).to_string()
            }
        };
        format!("{context} {}: {detail} ({status})", path.display())
    }

    let url = CFURL::from_path(path, false)
        .ok_or_else(|| format!("cannot form file URL for {}", path.display()))?;
    let mut code: SecStaticCodeRef = ptr::null_mut();
    let url_ref: CFURLRef = url.as_concrete_TypeRef();
    let status = unsafe { SecStaticCodeCreateWithPath(url_ref, 0, &mut code) };
    if status != errSecSuccess || code.is_null() {
        return Err(status_error("cannot resolve static code", path, status));
    }
    let _code_owner = unsafe { CFType::wrap_under_create_rule(code.cast::<c_void>() as CFTypeRef) };

    let mut information: CFDictionaryRef = ptr::null();
    let status = unsafe {
        SecCodeCopySigningInformation(code, SEC_CS_SIGNING_INFORMATION, &mut information)
    };
    if status != errSecSuccess || information.is_null() {
        return Err(status_error(
            "cannot read static code signing information",
            path,
            status,
        ));
    }
    let information =
        unsafe { CFDictionary::<CFString, CFType>::wrap_under_create_rule(information) };
    let certificates_key = unsafe { CFString::wrap_under_get_rule(kSecCodeInfoCertificates) };
    let certificates = information
        .find(&certificates_key)
        .and_then(|value| value.downcast::<CFArray>());
    let Some(certificate) = certificates
        .as_ref()
        .and_then(|certificates| certificates.get(0))
        .map(|certificate| *certificate as SecCertificateRef)
    else {
        return Ok(None);
    };
    let certificate_data = unsafe { SecCertificateCopyData(certificate) };
    if certificate_data.is_null() {
        return Err(format!(
            "static code signing certificate is unavailable for {}",
            path.display()
        ));
    }
    let certificate_data = unsafe { CFData::wrap_under_create_rule(certificate_data) };
    Ok(Some(format!(
        "{:x}",
        Sha256::digest(certificate_data.bytes())
    )))
}

#[cfg(target_os = "macos")]
pub(super) fn system_peer_identity(conn: &UnixStream) -> Result<PeerIdentity, String> {
    const LOCAL_PEERPID: libc::c_int = 0x002;
    const LOCAL_PEERTOKEN: libc::c_int = 0x006;

    let mut pid: libc::pid_t = 0;
    let mut pid_len = std::mem::size_of_val(&pid) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            conn.as_raw_fd(),
            libc::SOL_LOCAL,
            LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut pid_len,
        )
    };
    if result != 0 || pid <= 0 {
        return Err(format!(
            "cannot determine Unix peer pid: {}",
            std::io::Error::last_os_error()
        ));
    }

    // audit token 是签名鉴权的唯一依据；pid 只用于诊断日志。
    let mut token = [0_u32; 8];
    let mut token_len = std::mem::size_of_val(&token) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            conn.as_raw_fd(),
            libc::SOL_LOCAL,
            LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &mut token_len,
        )
    };
    let audit_token = if result == 0 && token_len as usize == std::mem::size_of_val(&token) {
        Some(PeerAuditToken(token))
    } else {
        None
    };

    // audit token 是签名鉴权的唯一依据；pid 只用于诊断日志。
    // 可执行文件路径也是诊断：自更新 rename 掉磁盘二进制后老进程照样活着，
    // proc_pidpath 却拿不到路径——取不到不能连坐鉴权。
    let executable = match process_executable(pid as u32) {
        Ok(executable) => Some(executable),
        Err(error) => {
            crate::dlog(&format!(
                "auth: peer pid {pid} executable path unavailable ({error}); relying on kernel code identity"
            ));
            None
        }
    };
    Ok(PeerIdentity {
        pid: pid as u32,
        executable,
        audit_token,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn process_executable(pid: u32) -> Result<PathBuf, String> {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStringExt;

    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut c_void, buffersize: u32) -> libc::c_int;
    }
    let pid = i32::try_from(pid).map_err(|_| "process pid exceeds macOS pid range".to_string())?;
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let path_len = unsafe {
        proc_pidpath(
            pid,
            path.as_mut_ptr().cast(),
            path.len().try_into().unwrap_or(u32::MAX),
        )
    };
    if path_len <= 0 {
        return Err(format!(
            "cannot determine executable for Unix peer pid {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    path.truncate(path_len as usize);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(path)))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn system_peer_identity(conn: &UnixStream) -> Result<PeerIdentity, String> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut credentials_len = std::mem::size_of_val(&credentials) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            conn.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut credentials_len,
        )
    };
    if result != 0 || credentials.pid <= 0 {
        return Err(format!(
            "cannot determine Unix peer credentials: {}",
            std::io::Error::last_os_error()
        ));
    }
    let executable = process_executable(credentials.pid as u32)?;
    Ok(PeerIdentity {
        pid: credentials.pid as u32,
        executable: Some(executable),
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn process_executable(pid: u32) -> Result<PathBuf, String> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .map_err(|error| format!("cannot determine executable for Unix peer pid {pid}: {error}"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(super) fn process_executable(_pid: u32) -> Result<PathBuf, String> {
    Err("process executable lookup is unsupported on this platform".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(super) fn system_peer_identity(_conn: &UnixStream) -> Result<PeerIdentity, String> {
    Err("first-party Unix peer authentication is unsupported on this platform".to_string())
}

#[cfg(test)]
pub(crate) fn authenticate_event_client(
    value: &serde_json::Value,
    peer: &PeerIdentity,
    daemon_pid: u32,
    daemon_executable: &Path,
) -> Result<AuthenticatedEventClient, String> {
    authenticate_event_client_for_platform(value, peer, daemon_pid, daemon_executable)
}

pub(super) fn authenticate_event_client_for_platform(
    value: &serde_json::Value,
    peer: &PeerIdentity,
    daemon_pid: u32,
    daemon_executable: &Path,
) -> Result<AuthenticatedEventClient, String> {
    #[cfg(target_os = "macos")]
    {
        authenticate_event_client_with_system_verifier(
            value,
            peer,
            daemon_pid,
            daemon_executable,
            &SystemPeerCodeIdentity,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        authenticate_event_client_without_codesign(value, peer, daemon_pid, daemon_executable)
    }
}

#[cfg(all(target_os = "macos", test))]
pub(super) fn authenticate_event_client_with_verifier(
    value: &serde_json::Value,
    peer: &PeerIdentity,
    daemon_pid: u32,
    daemon_executable: &Path,
    verifier: &dyn PeerCodeIdentity,
) -> Result<AuthenticatedEventClient, String> {
    authenticate_event_client_with_system_verifier(
        value,
        peer,
        daemon_pid,
        daemon_executable,
        verifier,
    )
}

#[cfg(target_os = "macos")]
pub(super) fn authenticate_event_client_with_system_verifier(
    value: &serde_json::Value,
    peer: &PeerIdentity,
    daemon_pid: u32,
    daemon_executable: &Path,
    verifier: &dyn PeerCodeIdentity,
) -> Result<AuthenticatedEventClient, String> {
    let auth = value
        .get("auth")
        .cloned()
        .ok_or_else(|| "missing authenticated event client context".to_string())?;
    let auth = serde_json::from_value::<EventClientAuth>(auth)
        .map_err(|error| format!("invalid event client auth: {error}"))?;
    let EventClientAuth::FirstParty { kind } = auth;
    authenticate_first_party_macos(kind, peer, daemon_pid, daemon_executable, verifier)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn authenticate_event_client_without_codesign(
    value: &serde_json::Value,
    peer: &PeerIdentity,
    daemon_pid: u32,
    daemon_executable: &Path,
) -> Result<AuthenticatedEventClient, String> {
    let auth = value
        .get("auth")
        .cloned()
        .ok_or_else(|| "missing authenticated event client context".to_string())?;
    let auth = serde_json::from_value::<EventClientAuth>(auth)
        .map_err(|error| format!("invalid event client auth: {error}"))?;
    let EventClientAuth::FirstParty { kind } = auth;
    authenticate_first_party_sibling(kind, peer, daemon_pid, daemon_executable)
}

pub(super) fn authenticate_event_connection(
    value: &serde_json::Value,
    conn: &UnixStream,
    provider: &dyn PeerIdentityProvider,
    daemon_pid: u32,
    daemon_executable: &Path,
) -> Result<AuthenticatedEventClient, String> {
    let peer = provider.identity(conn)?;
    authenticate_event_client_for_platform(value, &peer, daemon_pid, daemon_executable)
}

/// 正在看画面的活连接数。headless 自升级只在这是 0 时才动手——GUI 自己有空闲
/// 门控（agent 忙不闪终端），有人连着就把升级时机让出去。
///
/// 必须是租约而不是「最近 N 分钟活跃过」：desktop 鉴权 / 终端 open 都是稀疏事件，
/// ACP 对话可以连着看很久却不再触发它们。用过期时间当「GUI 不在」，会把正在跑的
/// 回合当成没人看，自升级把 host 换掉，toolUse 就停在半截。
static LIVE_VIEWERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 一条会挡住 headless 自升级的观看连接。Drop 时归还。
pub(crate) struct ViewerLease;

impl ViewerLease {
    pub(crate) fn acquire() -> Self {
        LIVE_VIEWERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

impl Drop for ViewerLease {
    fn drop(&mut self) {
        LIVE_VIEWERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) fn viewer_is_present() -> bool {
    LIVE_VIEWERS.load(std::sync::atomic::Ordering::SeqCst) > 0
}

pub(super) fn authenticate_desktop_connection(
    value: &serde_json::Value,
    conn: &UnixStream,
) -> Result<AuthenticatedEventClient, String> {
    let executable = daemon_executable_path()
        .map_err(|error| format!("cannot determine smeltd executable: {error}"))?;
    let identity = authenticate_event_connection(
        value,
        conn,
        &SystemPeerIdentityProvider,
        std::process::id(),
        &executable,
    )?;
    require_desktop_identity(identity)
}

pub(super) fn require_desktop_identity(
    identity: AuthenticatedEventClient,
) -> Result<AuthenticatedEventClient, String> {
    if identity.plugin_id.as_str() != FIRST_PARTY_DESKTOP_PLUGIN_ID {
        return Err("plugin UI operation requires authenticated desktop".to_string());
    }
    Ok(identity)
}

#[cfg(all(target_os = "macos", test))]
pub(super) fn authenticate_event_connection_with_verifier(
    value: &serde_json::Value,
    conn: &UnixStream,
    provider: &dyn PeerIdentityProvider,
    daemon_pid: u32,
    daemon_executable: &Path,
    verifier: &dyn PeerCodeIdentity,
) -> Result<AuthenticatedEventClient, String> {
    let peer = provider.identity(conn)?;
    authenticate_event_client_with_verifier(value, &peer, daemon_pid, daemon_executable, verifier)
}

#[cfg(target_os = "macos")]
pub(super) fn authenticate_first_party_macos(
    requested: FirstPartyClientKind,
    peer: &PeerIdentity,
    daemon_pid: u32,
    daemon_executable: &Path,
    verifier: &dyn PeerCodeIdentity,
) -> Result<AuthenticatedEventClient, String> {
    let actual = if peer.pid == daemon_pid {
        FirstPartyClientKind::RemoteGateway
    } else {
        // 分类问内核：signing identifier 是身份，不受磁盘替换影响。
        // 内核签名身份在场即是唯一分类依据——命中表内签名 id → kind；
        // 有签名 id 但不在表内 → 明确非第一方，直接拒绝，basename 无权翻案
        //（签名说了它是谁，文件名说了不算）。无内核签名身份（无 token、
        // 或未签名/ad-hoc 进程）才退回路径 basename，两者都缺则拒绝。
        let kernel_signing_id = match peer.audit_token {
            Some(token) => verifier.kernel_identity(token)?.signing_id,
            None => None,
        };
        let kind = match kernel_signing_id {
            Some(signing_id) => kind_from_signing_id(&signing_id).ok_or_else(|| {
                format!("Unix peer kernel code identity {signing_id:?} is not first-party")
            })?,
            None => peer
                .executable
                .as_deref()
                .and_then(executable_kind)
                .ok_or_else(|| {
                    "Unix peer presents neither a first-party kernel identity nor an executable path"
                        .to_string()
                })?,
        };
        if !trusted_macos_executable(
            daemon_executable,
            peer,
            peer.executable.as_deref(),
            kind,
            verifier,
        )? {
            return Err("Unix peer is not an authenticated first-party event client".to_string());
        }
        kind
    };
    if requested != actual {
        return Err(format!(
            "requested first-party kind {requested:?} does not match Unix peer identity {actual:?}"
        ));
    }
    Ok(first_party_event_client(actual))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn authenticate_first_party_sibling(
    requested: FirstPartyClientKind,
    peer: &PeerIdentity,
    daemon_pid: u32,
    daemon_executable: &Path,
) -> Result<AuthenticatedEventClient, String> {
    let actual = if peer.pid == daemon_pid {
        FirstPartyClientKind::RemoteGateway
    } else {
        let executable = peer
            .executable
            .as_deref()
            .ok_or_else(|| "Unix peer executable is unavailable".to_string())?;
        let kind = executable_kind(executable)
            .ok_or_else(|| "Unix peer executable basename is not first-party".to_string())?;
        if !trusted_sibling(daemon_executable, executable, executable_basename(kind)) {
            return Err("Unix peer is not an authenticated first-party event client".to_string());
        }
        kind
    };
    if requested != actual {
        return Err(format!(
            "requested first-party kind {requested:?} does not match Unix peer identity {actual:?}"
        ));
    }
    Ok(first_party_event_client(actual))
}

/// 一方第一方组件的三个名字：客户端声称的 kind、磁盘文件 basename、内核
/// code directory 里的 signing identifier。分类（basename/signing-id → kind）
/// 与校验（kind → 期望 signing-id）都从这张表出，单一事实源——Desktop 的
/// signing id（com.zzfn.smelt）与 basename（smelt）本就不同，两列若分散在
/// 多份 match 表里，改一处漏一处就会静默错配。
struct FirstPartyExecutable {
    kind: FirstPartyClientKind,
    basename: &'static str,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    signing_id: &'static str,
}

const FIRST_PARTY_EXECUTABLES: &[FirstPartyExecutable] = &[
    FirstPartyExecutable {
        kind: FirstPartyClientKind::Desktop,
        basename: "smelt",
        signing_id: "com.zzfn.smelt",
    },
    FirstPartyExecutable {
        kind: FirstPartyClientKind::RemoteGateway,
        basename: "gateway",
        signing_id: "gateway",
    },
];

pub(super) fn executable_kind(executable: &Path) -> Option<FirstPartyClientKind> {
    let basename = executable.file_name()?.to_str()?;
    FIRST_PARTY_EXECUTABLES
        .iter()
        .find(|entry| entry.basename == basename)
        .map(|entry| entry.kind)
}

pub(super) fn executable_basename(kind: FirstPartyClientKind) -> &'static str {
    FIRST_PARTY_EXECUTABLES
        .iter()
        .find(|entry| entry.kind == kind)
        .map(|entry| entry.basename)
        .expect("first-party executable table covers every client kind")
}

#[cfg(target_os = "macos")]
pub(super) fn signed_code_identifier(kind: FirstPartyClientKind) -> &'static str {
    FIRST_PARTY_EXECUTABLES
        .iter()
        .find(|entry| entry.kind == kind)
        .map(|entry| entry.signing_id)
        .expect("first-party executable table covers every client kind")
}

/// 内核 signing identifier → 客户端 kind。这是 macOS 上对端分类的首选依据
///（路径 basename 只是签名身份缺失时的兜底）。
#[cfg(target_os = "macos")]
fn kind_from_signing_id(signing_id: &str) -> Option<FirstPartyClientKind> {
    FIRST_PARTY_EXECUTABLES
        .iter()
        .find(|entry| entry.signing_id == signing_id)
        .map(|entry| entry.kind)
}

pub(super) fn trusted_sibling(
    daemon_executable: &Path,
    peer_executable: &Path,
    expected_basename: &str,
) -> bool {
    let Some(parent) = daemon_executable.parent() else {
        return false;
    };
    let trusted = parent.join(expected_basename);
    match (trusted.canonicalize(), peer_executable.canonicalize()) {
        (Ok(trusted), Ok(peer)) => trusted == peer,
        _ => false,
    }
}

#[cfg(target_os = "macos")]
pub(super) fn trusted_macos_executable(
    daemon_executable: &Path,
    peer: &PeerIdentity,
    peer_executable: Option<&Path>,
    kind: FirstPartyClientKind,
    verifier: &dyn PeerCodeIdentity,
) -> Result<bool, String> {
    let Some(peer_token) = peer.audit_token else {
        return Err(format!(
            "Unix peer pid {} did not provide an audit token",
            peer.pid
        ));
    };
    // 两边身份都只问内核（audit token 绑定，无 TOCTOU），不读磁盘：
    // 自更新把磁盘文件换掉后，运行中进程照样能自证，不再 -67034。
    let daemon = verifier.kernel_identity(self_audit_token()?)?;
    let peer_id = verifier.kernel_identity(peer_token)?;
    let expected = signed_code_identifier(kind);

    // Leg 1（生产）：双边 team 一致 + identifier 相符。与 Apple XPC
    // team-identity 要求同构；证书轮换不影响（team 不变）。无 pinning：
    // 攻击者拿到我们签名密钥即全局沦陷，pin 具体哪张证书没有意义。
    if let (Some(daemon_team), Some(peer_team)) = (&daemon.team, &peer_id.team) {
        return Ok(peer_team == daemon_team
            && peer_id.signing_id.as_deref() == Some(expected)
            && daemon.signing_id.as_deref() == Some("smeltd"));
    }
    // 混合（一边有 team 一边没有）→ 直接失败，不进 local leg。
    if daemon.team.is_some() || peer_id.team.is_some() {
        return Ok(false);
    }

    // Leg 2（本地自签名）：两边都没 team。比较双方路径上磁盘文件的静态证书——
    // 同 keychain 重签的包证书恒定；安装把两边一起换成新文件后，老进程与新
    // GUI 比的是两个新证书，照样过（skew 宽容，与 Prod 一致，不强制重启）。
    // 威胁模型注记：能同时伪造两边磁盘证书的攻击者必须有目录写权限，
    // 而有该权限时直接读 sqlite / 换守护二进制收益更大——socket 鉴权本来
    // 也不在那条威胁模型里（见模块注释）。ad-hoc（无证书）落到 Leg 3。
    // Leg 1（生产）不读路径，故 prod 进程在自更新 rename 窗口照常过；
    // 本地自签腿必须读 peer 的磁盘文件，路径缺失（老进程 + 磁盘已换）时
    // 无法比对 → 拒绝（与自更新前行为一致，dev 重启即恢复）。
    let Some(peer_executable) = peer_executable else {
        return Err(format!(
            "Unix peer pid {} has no executable path for local code comparison",
            peer.pid
        ));
    };
    let peer_cert = verifier.static_cert_hash(peer_executable)?;
    let daemon_cert = verifier.static_cert_hash(daemon_executable)?;
    match (peer_cert, daemon_cert) {
        (Some(a), Some(b)) => Ok(a == b
            && peer_id.signing_id.as_deref() == Some(expected)
            && daemon.signing_id.as_deref() == Some("smeltd")),
        // 双边都无证书（ad-hoc/unsigned）→ Leg 3。单边有证书是混合体，直接
        // 拒绝（与旧 `(Signed, Unsigned) => false` 一致，不进 debug 通道）。
        (None, None) => Ok(cfg!(debug_assertions)
            && trusted_sibling(
                daemon_executable,
                peer_executable,
                executable_basename(kind),
            )),
        _ => Ok(false),
    }
}

pub(crate) fn verify_plugin_process(
    _package: &smelt_plugin_host::PluginPackage,
    program: &std::path::Path,
    plugin_pid: u32,
) -> Result<(), smelt_plugin_host::HostError> {
    let running_executable =
        process_executable(plugin_pid).map_err(smelt_plugin_host::HostError::new)?;
    let expected = program
        .canonicalize()
        .map_err(|error| smelt_plugin_host::HostError::new(error.to_string()))?;
    let running = running_executable
        .canonicalize()
        .map_err(|error| smelt_plugin_host::HostError::new(error.to_string()))?;
    if running != expected {
        return Err(smelt_plugin_host::HostError::new(format!(
            "plugin pid {plugin_pid} runs {}, expected {}",
            running.display(),
            expected.display()
        )));
    }
    let managed = smelt_core::managed_runtime::managed_bun_path_if_ready()
        .and_then(|path| path.canonicalize().ok())
        .ok_or_else(|| {
            smelt_plugin_host::HostError::new("managed script runtime is unavailable")
        })?;
    if expected != managed {
        return Err(smelt_plugin_host::HostError::new(format!(
            "plugin pid {plugin_pid} runs an unmanaged script runtime {}",
            expected.display()
        )));
    }
    Ok(())
}

pub(super) fn first_party_event_client(kind: FirstPartyClientKind) -> AuthenticatedEventClient {
    let (plugin_id, capabilities): (&str, &[&str]) = match kind {
        FirstPartyClientKind::Desktop => (
            FIRST_PARTY_DESKTOP_PLUGIN_ID,
            &[
                CORE_CAPABILITY_SESSION_READ,
                CORE_CAPABILITY_REMOTE_SESSIONS_READ,
                CORE_CAPABILITY_WORKSPACE_READ,
                CORE_CAPABILITY_AUTOMATION_READ,
            ],
        ),
        FirstPartyClientKind::RemoteGateway => (
            FIRST_PARTY_REMOTE_GATEWAY_PLUGIN_ID,
            &[
                CORE_CAPABILITY_SESSION_READ,
                CORE_CAPABILITY_REMOTE_SESSIONS_READ,
                CORE_CAPABILITY_WORKSPACE_READ,
                // 自动化 Run 停下来等审批时，手机往往是唯一在场的客户端：没有这条
                // 读能力，那一刻在移动端根本不存在。
                CORE_CAPABILITY_AUTOMATION_READ,
            ],
        ),
    };
    AuthenticatedEventClient {
        plugin_id: PluginId::new(plugin_id).expect("first-party plugin id is valid"),
        capabilities: capabilities
            .iter()
            .map(|capability| {
                Capability::new(*capability).expect("first-party capability is valid")
            })
            .collect(),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod blob_tests {
    use super::parse_csops_blob;

    /// Santa 布局：magic(4) + len(be, 含头+串+NUL) + bytes + NUL。
    fn santa_blob(payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; 8 + payload.len() + 1];
        let len = (8 + payload.len() + 1) as u32;
        buf[4..8].copy_from_slice(&len.to_be_bytes());
        buf[8..8 + payload.len()].copy_from_slice(payload);
        buf
    }

    #[test]
    fn blob_parses_team_and_identity() {
        assert_eq!(
            parse_csops_blob(&santa_blob(b"SMELTTEAM1")).unwrap(),
            Some("SMELTTEAM1".to_string())
        );
        assert_eq!(
            parse_csops_blob(&santa_blob(b"com.zzfn.smelt")).unwrap(),
            Some("com.zzfn.smelt".to_string())
        );
    }

    #[test]
    fn blob_edge_cases_fail_closed_or_empty() {
        // 空（只有 NUL）→ None，不报错。
        assert_eq!(parse_csops_blob(&santa_blob(b"")).unwrap(), None);
        // 长度越过缓冲区 → 拒绝。
        let mut bad = santa_blob(b"AB");
        bad[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse_csops_blob(&bad).is_err());
        // 非 UTF-8 → 拒绝。
        assert!(parse_csops_blob(&santa_blob(&[0xff, 0xfe])).is_err());
        // 太短 → 拒绝。
        assert!(parse_csops_blob(b"short").is_err());
        // 无 NUL 布局（len=8+n）→ 照样读对（兜底分支）。
        let mut bare = vec![0u8; 8 + 3];
        bare[4..8].copy_from_slice(&11u32.to_be_bytes());
        bare[8..11].copy_from_slice(b"XYZ");
        assert_eq!(parse_csops_blob(&bare).unwrap(), Some("XYZ".to_string()));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod cert_tests {
    use super::*;

    #[test]
    fn static_cert_reads_real_world_files() {
        // Apple 系统二进制：有证书，两次读一致（确定性）。
        let ls = PathBuf::from("/bin/ls");
        let first = static_cert_hash(&ls).expect("系统二进制应可读证书");
        assert!(first.is_some());
        assert_eq!(first, static_cert_hash(&ls).unwrap());
        // 本测试二进制是 ad-hoc 的：可读但无证书 → None（不是 Err）。
        let test_exe = std::env::current_exe().unwrap();
        assert_eq!(static_cert_hash(&test_exe).unwrap(), None);
        // 不存在的文件 → Err（fail closed 上游处理）。
        assert!(static_cert_hash(&PathBuf::from("/nonexistent-smelt-auth-test")).is_err());
    }
}
