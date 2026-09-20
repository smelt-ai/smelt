use super::auth::first_party_event_client;
#[cfg(target_os = "macos")]
use super::auth::{
    KernelCodeIdentity, PeerAuditToken, PeerCodeIdentity, SystemPeerCodeIdentity,
    authenticate_event_connection_with_verifier, self_audit_token,
};
use super::*;
use smelt_plugin_api::{
    Ack, CORE_CAPABILITY_AGENT_MESSAGE_READ, CORE_CAPABILITY_AUTOMATION_READ,
    CORE_CAPABILITY_REMOTE_SESSIONS_READ, CORE_CAPABILITY_SESSION_READ,
    CORE_CAPABILITY_WORKSPACE_READ, CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
    CORE_TOPIC_SESSION_STATE_CHANGED, Capability, DeliveryClass, EVENT_BUS_PROTOCOL_VERSION,
    FIRST_PARTY_DESKTOP_PLUGIN_ID, FIRST_PARTY_REMOTE_GATEWAY_PLUGIN_ID, FirstPartyClientKind,
    Nack, NackDisposition, PluginId, ProtocolRange, SubscribeControlMessage, SubscribeErrorCode,
    SubscribeMessage, SubscribeRequest, SubscriptionDeclaration, SubscriptionId,
};
#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::{
    io::{BufRead, BufReader, Write},
    net::Shutdown,
    path::PathBuf,
    time::Duration,
};

struct FakePeerIdentityProvider(PeerIdentity);

impl PeerIdentityProvider for FakePeerIdentityProvider {
    fn identity(&self, _conn: &UnixStream) -> Result<PeerIdentity, String> {
        Ok(self.0.clone())
    }
}

/// 测试里用确定性 token 代表一个进程实例：前 5 个字段填 0，第 6 个是 pid，
/// 末位放 pidversion，形状与内核给出的 audit token 一致。
#[cfg(target_os = "macos")]
fn token_for(pid: u32) -> PeerAuditToken {
    PeerAuditToken([0, 0, 0, 0, 0, pid, 0, pid])
}

#[cfg(target_os = "macos")]
fn peer(pid: u32, executable: Option<PathBuf>) -> PeerIdentity {
    PeerIdentity {
        pid,
        executable,
        audit_token: Some(token_for(pid)),
    }
}

#[cfg(not(target_os = "macos"))]
fn peer(pid: u32, executable: Option<PathBuf>) -> PeerIdentity {
    PeerIdentity { pid, executable }
}

#[cfg(target_os = "macos")]
struct FakePeerCodeIdentity {
    kernel: BTreeMap<u32, Result<KernelCodeIdentity, String>>,
    certs: BTreeMap<String, Result<Option<String>, String>>,
}

#[cfg(target_os = "macos")]
impl PeerCodeIdentity for FakePeerCodeIdentity {
    fn kernel_identity(&self, token: PeerAuditToken) -> Result<KernelCodeIdentity, String> {
        // daemon 侧查询的是本进程的真实 audit token，测试固定把它映射到 42 这条记录。
        let pid = if token == self_audit_token().expect("self audit token is available") {
            42
        } else {
            token.0[5]
        };
        self.kernel
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| Err(format!("unexpected pid {pid}")))
    }

    fn static_cert_hash(&self, path: &std::path::Path) -> Result<Option<String>, String> {
        self.certs
            .get(&path.display().to_string())
            .cloned()
            .unwrap_or_else(|| Err(format!("unexpected static read {}", path.display())))
    }
}

#[cfg(target_os = "macos")]
fn kernel(team: Option<&str>, signing_id: Option<&str>) -> KernelCodeIdentity {
    KernelCodeIdentity {
        team: team.map(str::to_string),
        signing_id: signing_id.map(str::to_string),
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_kernel_verifier_inspects_the_running_test_process() {
    // 测试二进制是 ad-hoc 签的：无 team（ENOENT→None），查询本身必须成功。
    let identity = SystemPeerCodeIdentity
        .kernel_identity(self_audit_token().unwrap())
        .expect("kernel identity query must succeed");
    assert_eq!(identity.team, None);
}

fn identity(capabilities: &[&str]) -> AuthenticatedEventClient {
    AuthenticatedEventClient {
        plugin_id: PluginId::new("test.protocol").unwrap(),
        capabilities: capabilities
            .iter()
            .map(|capability| Capability::new(*capability).unwrap())
            .collect(),
    }
}

fn request(
    id: &str,
    topic: &str,
    delivery: DeliveryClass,
    cursor: Option<u64>,
) -> serde_json::Value {
    serde_json::to_value(SubscribeRequest {
        protocol: ProtocolRange {
            min: EVENT_BUS_PROTOCOL_VERSION,
            max: EVENT_BUS_PROTOCOL_VERSION,
        },
        subscription: SubscriptionDeclaration {
            id: SubscriptionId::new(id).unwrap(),
            topics: vec![smelt_plugin_api::Topic::new(topic).unwrap()],
            delivery,
        },
        cursor,
    })
    .unwrap()
}

fn read_message(reader: &mut BufReader<UnixStream>) -> SubscribeMessage<serde_json::Value> {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(!line.is_empty(), "event subscription closed unexpectedly");
    serde_json::from_str(&line).unwrap()
}

#[test]
fn versioned_subscription_streams_snapshot_and_event_and_cleans_up() {
    let event_hub = super::super::new_event_hub();
    let request = request(
        "sessions",
        CORE_TOPIC_SESSION_STATE_CHANGED,
        DeliveryClass::Ephemeral,
        None,
    );
    let (server, client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let worker_hub = Arc::clone(&event_hub);
    let worker = thread::spawn(move || {
        handle_event_subscribe(
            server,
            Some(&request),
            Ok(identity(&[CORE_CAPABILITY_SESSION_READ])),
            &worker_hub,
        )
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    assert!(matches!(
        read_message(&mut reader),
        SubscribeMessage::Snapshot { .. }
    ));

    event_hub
        .publish_session(&SessionState {
            id: "session-1".to_string(),
            revision: 1,
            ..Default::default()
        })
        .unwrap();
    let SubscribeMessage::Event { envelope, .. } = read_message(&mut reader) else {
        panic!("expected versioned event");
    };
    assert_eq!(envelope.topic.as_str(), CORE_TOPIC_SESSION_STATE_CHANGED);

    client.shutdown(Shutdown::Both).unwrap();
    worker.join().unwrap();
    let topics =
        BTreeSet::from([smelt_plugin_api::Topic::new(CORE_TOPIC_SESSION_STATE_CHANGED).unwrap()]);
    let replacement = event_hub
        .subscribe(
            PluginId::new("test.protocol").unwrap(),
            SubscriptionId::new("sessions").unwrap(),
            topics,
            DeliveryClass::Ephemeral,
            &BTreeSet::from([Capability::new(CORE_CAPABILITY_SESSION_READ).unwrap()]),
        )
        .expect("disconnect must remove the prior subscription");
    drop(replacement);
}

#[test]
fn version_and_capability_rejections_are_explicit() {
    let event_hub = super::super::new_event_hub();
    let mut unsupported = request(
        "sessions",
        CORE_TOPIC_SESSION_STATE_CHANGED,
        DeliveryClass::Ephemeral,
        None,
    );
    unsupported["protocol"] = serde_json::json!({"min": 2, "max": 3});
    for (request, identity, expected) in [
        (
            unsupported,
            identity(&[CORE_CAPABILITY_SESSION_READ]),
            "unsupported event protocol range",
        ),
        (
            request(
                "sessions",
                CORE_TOPIC_SESSION_STATE_CHANGED,
                DeliveryClass::Ephemeral,
                None,
            ),
            identity(&[]),
            "missing subscribe capability",
        ),
    ] {
        let (server, client) = UnixStream::pair().unwrap();
        handle_event_subscribe(server, Some(&request), Ok(identity), &event_hub);
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        assert!(line.contains(expected), "{line}");
    }
}

#[test]
fn cursor_mismatch_is_written_as_a_structured_error_before_close() {
    let event_hub = super::super::new_event_hub();
    let request = request(
        "agent-messages",
        CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
        DeliveryClass::Durable,
        Some(99),
    );
    let (server, client) = UnixStream::pair().unwrap();
    handle_event_subscribe(
        server,
        Some(&request),
        Ok(identity(&[CORE_CAPABILITY_AGENT_MESSAGE_READ])),
        &event_hub,
    );

    let mut reader = BufReader::new(client);
    let SubscribeMessage::Error { code, message } = read_message(&mut reader) else {
        panic!("cursor mismatch should be a structured protocol error");
    };
    assert_eq!(code, SubscribeErrorCode::Rejected);
    assert!(message.contains("requested cursor does not match"));
    let mut eof = String::new();
    assert_eq!(reader.read_line(&mut eof).unwrap(), 0);
}

#[test]
fn first_party_auth_assigns_fixed_minimum_capabilities_from_peer_facts() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/peer-auth-tests");
    std::fs::create_dir_all(&root).unwrap();
    let daemon_executable = root.join("smeltd");
    let desktop_executable = root.join("smelt");
    std::fs::write(&daemon_executable, b"daemon").unwrap();
    std::fs::write(&desktop_executable, b"desktop").unwrap();
    let desktop_peer = peer(7, Some(desktop_executable.clone()));
    let (server, _client) = UnixStream::pair().unwrap();
    #[cfg(target_os = "macos")]
    let desktop = authenticate_event_connection_with_verifier(
        &serde_json::json!({
            "auth": {
                "type": "first_party",
                "kind": "desktop"
            }
        }),
        &server,
        &FakePeerIdentityProvider(desktop_peer),
        42,
        &daemon_executable,
        &FakePeerCodeIdentity {
            kernel: BTreeMap::from([(42, Ok(kernel(None, None))), (7, Ok(kernel(None, None)))]),
            certs: BTreeMap::from([
                (daemon_executable.display().to_string(), Ok(None)),
                (desktop_executable.display().to_string(), Ok(None)),
            ]),
        },
    )
    .unwrap();
    #[cfg(not(target_os = "macos"))]
    let desktop = authenticate_event_connection(
        &serde_json::json!({
            "auth": {
                "type": "first_party",
                "kind": "desktop"
            }
        }),
        &server,
        &FakePeerIdentityProvider(desktop_peer),
        42,
        &daemon_executable,
    )
    .unwrap();
    assert_eq!(desktop.plugin_id.as_str(), FIRST_PARTY_DESKTOP_PLUGIN_ID);
    assert_eq!(
        desktop.capabilities,
        BTreeSet::from([
            Capability::new(CORE_CAPABILITY_SESSION_READ).unwrap(),
            Capability::new(CORE_CAPABILITY_REMOTE_SESSIONS_READ).unwrap(),
            Capability::new(CORE_CAPABILITY_WORKSPACE_READ).unwrap(),
            Capability::new(CORE_CAPABILITY_AUTOMATION_READ).unwrap(),
        ])
    );

    let gateway = authenticate_event_client(
        &serde_json::json!({
            "auth": {
                "type": "first_party",
                "kind": "remote_gateway"
            }
        }),
        &peer(42, Some(PathBuf::from("/untrusted/smeltd"))),
        42,
        &daemon_executable,
    )
    .unwrap();
    assert_eq!(
        gateway.plugin_id.as_str(),
        FIRST_PARTY_REMOTE_GATEWAY_PLUGIN_ID
    );
    assert!(
        gateway
            .capabilities
            .contains(&Capability::new(CORE_CAPABILITY_AUTOMATION_READ).unwrap())
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn first_party_auth_rejects_kind_spoofing_and_untrusted_peers() {
    let daemon_executable = PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smeltd");
    let self_peer = peer(42, Some(daemon_executable.clone()));
    let error = authenticate_event_client(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &self_peer,
        42,
        &daemon_executable,
    )
    .unwrap_err();
    assert!(error.contains("does not match Unix peer identity"));

    #[cfg(target_os = "macos")]
    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "remote_gateway"}
        }),
        &peer(7, Some(PathBuf::from("/usr/local/bin/smelt"))),
        42,
        &daemon_executable,
        &FakePeerCodeIdentity {
            kernel: BTreeMap::from([
                (42, Ok(kernel(Some("SMELTTEAM"), Some("smeltd")))),
                (
                    7,
                    Ok(kernel(Some("EVILTEAM"), Some("com.example.attacker"))),
                ),
            ]),
            certs: BTreeMap::new(),
        },
    )
    .unwrap_err();
    #[cfg(not(target_os = "macos"))]
    let error = authenticate_event_client(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "remote_gateway"}
        }),
        &peer(7, Some(PathBuf::from("/usr/local/bin/smelt"))),
        42,
        &daemon_executable,
    )
    .unwrap_err();
    #[cfg(target_os = "macos")]
    // 签名身份在场即是唯一分类依据：非第一方签名 id 直接拒绝，basename 无权翻案。
    assert!(error.contains("is not first-party"), "{error}");
    #[cfg(not(target_os = "macos"))]
    assert!(error.contains("not an authenticated first-party"));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_auth_accepts_signed_app_desktop_and_standalone_gateway_but_not_basename_alone() {
    let daemon = PathBuf::from("/Users/test/.smelt/bin/smeltd");
    let desktop = PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt");
    let gateway = PathBuf::from("/opt/smelt/gateway");
    let impostor = PathBuf::from("/untrusted/smelt");
    let renamed_team_binary = PathBuf::from("/Applications/Other.app/Contents/MacOS/smelt");
    let verifier = FakePeerCodeIdentity {
        kernel: BTreeMap::from([
            (42, Ok(kernel(Some("SMELTTEAM"), Some("smeltd")))),
            (7, Ok(kernel(Some("SMELTTEAM"), Some("com.zzfn.smelt")))),
            (8, Ok(kernel(Some("SMELTTEAM"), Some("gateway")))),
            (
                9,
                Ok(kernel(Some("EVILTEAM"), Some("com.example.impostor"))),
            ),
            (
                10,
                Ok(kernel(Some("SMELTTEAM"), Some("com.zzfn.other-product"))),
            ),
        ]),
        certs: BTreeMap::new(),
    };
    for (kind, pid, executable) in [
        (FirstPartyClientKind::Desktop, 7, desktop),
        (FirstPartyClientKind::RemoteGateway, 8, gateway),
    ] {
        let authenticated = authenticate_event_client_with_verifier(
            &serde_json::json!({
                "auth": {
                    "type": "first_party",
                    "kind": match kind {
                        FirstPartyClientKind::Desktop => "desktop",
                        FirstPartyClientKind::RemoteGateway => "remote_gateway",
                    }
                }
            }),
            &peer(pid, Some(executable)),
            42,
            &daemon,
            &verifier,
        )
        .unwrap();
        assert_eq!(authenticated, first_party_event_client(kind));
    }

    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(9, Some(impostor)),
        42,
        &daemon,
        &verifier,
    )
    .unwrap_err();
    // 分型直接拒绝：非第一方签名 id，不再看 basename。
    assert!(error.contains("is not first-party"), "{error}");

    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(10, Some(renamed_team_binary)),
        42,
        &daemon,
        &verifier,
    )
    .unwrap_err();
    // 同上：本队签名的其他产品（com.zzfn.other-product）改名成 smelt 也不认。
    assert!(error.contains("is not first-party"), "{error}");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_auth_classifies_by_kernel_identity_when_path_is_unavailable() {
    // 2026-09-18 事故回归：磁盘二进制被换名换掉后老进程还活着，proc_pidpath
    // ENOENT（executable=None），但内核 code identity 照常可得——生产
    // first-party 鉴权必须照常通过，不能跟着文件系统状态走。
    let daemon = PathBuf::from("/Users/test/.smelt/bin/smeltd");
    let verifier = FakePeerCodeIdentity {
        kernel: BTreeMap::from([
            (42, Ok(kernel(Some("SMELTTEAM"), Some("smeltd")))),
            (7, Ok(kernel(Some("SMELTTEAM"), Some("com.zzfn.smelt")))),
            (8, Ok(kernel(Some("SMELTTEAM"), Some("gateway")))),
        ]),
        certs: BTreeMap::new(),
    };
    for (kind, pid) in [
        (FirstPartyClientKind::Desktop, 7),
        (FirstPartyClientKind::RemoteGateway, 8),
    ] {
        let authenticated = authenticate_event_client_with_verifier(
            &serde_json::json!({
                "auth": {
                    "type": "first_party",
                    "kind": match kind {
                        FirstPartyClientKind::Desktop => "desktop",
                        FirstPartyClientKind::RemoteGateway => "remote_gateway",
                    }
                }
            }),
            &peer(pid, None),
            42,
            &daemon,
            &verifier,
        )
        .unwrap();
        assert_eq!(authenticated, first_party_event_client(kind));
    }

    // 无 token 且无路径 → 无从判定身份，拒绝。
    let no_identity = PeerIdentity {
        pid: 7,
        executable: None,
        audit_token: None,
    };
    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &no_identity,
        42,
        &daemon,
        &verifier,
    )
    .unwrap_err();
    assert!(
        error.contains("neither a first-party kernel identity nor an executable path"),
        "{error}"
    );

    // 有 token 但内核查询失败（进程已死/复用）→ fail closed，不退化到路径。
    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(
            7,
            Some(PathBuf::from(
                "/Applications/Smelt.app/Contents/MacOS/smelt",
            )),
        ),
        42,
        &daemon,
        &FakePeerCodeIdentity {
            kernel: BTreeMap::from([
                (42, Ok(kernel(Some("SMELTTEAM"), Some("smeltd")))),
                (
                    7,
                    Err("csops identity for pid 7 failed: errno ESRCH".to_string()),
                ),
            ]),
            certs: BTreeMap::new(),
        },
    )
    .unwrap_err();
    assert!(error.contains("ESRCH"), "{error}");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_auth_accepts_matching_local_certificate_without_team_identifier() {
    let daemon = PathBuf::from("/Users/test/.smelt/bin/smeltd");
    let desktop = PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt");
    let authenticated = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(7, Some(desktop.clone())),
        42,
        &daemon,
        &FakePeerCodeIdentity {
            kernel: BTreeMap::from([
                (42, Ok(kernel(None, Some("smeltd")))),
                (7, Ok(kernel(None, Some("com.zzfn.smelt")))),
            ]),
            certs: BTreeMap::from([
                (
                    daemon.display().to_string(),
                    Ok(Some("local-cert".to_string())),
                ),
                (
                    desktop.display().to_string(),
                    Ok(Some("local-cert".to_string())),
                ),
            ]),
        },
    )
    .unwrap();
    assert_eq!(
        authenticated.plugin_id.as_str(),
        FIRST_PARTY_DESKTOP_PLUGIN_ID
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_auth_uses_running_pid_identity_and_rejects_team_mismatch() {
    let daemon = PathBuf::from("/Users/test/.smelt/bin/smeltd");
    let desktop = PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt");
    let verifier = FakePeerCodeIdentity {
        kernel: BTreeMap::from([
            (42, Ok(kernel(Some("SMELTTEAM"), Some("smeltd")))),
            (7, Ok(kernel(Some("OTHERTEAM"), Some("com.zzfn.smelt")))),
        ]),
        certs: BTreeMap::new(),
    };

    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(7, Some(desktop)),
        42,
        &daemon,
        &verifier,
    )
    .unwrap_err();
    assert!(error.contains("not an authenticated first-party"));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_peer_identity_reads_a_real_audit_token_from_the_socket() {
    // LOCAL_PEERTOKEN 取不到时代码会静默退化为 audit_token: None，而那会让所有鉴权全部
    // 失败。这里用真实 socket 验证内核确实返回 token，且其 pid 字段与对端一致。
    let (server, _client) = UnixStream::pair().unwrap();
    let identity = super::auth::system_peer_identity(&server).unwrap();
    assert_eq!(identity.pid, std::process::id());
    let token = identity
        .audit_token
        .expect("kernel provides an audit token for a Unix socket peer");
    assert_eq!(token.0[5], std::process::id());
    // 同一进程的 token 应与 task_info 读到的一致。
    assert_eq!(token, self_audit_token().unwrap());
    // 真实签名查询也应能用这个 token 跑通。
    assert!(SystemPeerCodeIdentity.kernel_identity(token).is_ok());
}

#[cfg(target_os = "macos")]
#[test]
fn probe_identity_print_for_spawned_copy() {
    // 只给 macos_local_signed_kernel_identity_values 派生的子进程用：
    // 父进程正常跑单测时 env 未设置，直接返回。
    if std::env::var_os("SMELTD_IDENTITY_PROBE_CHILD").is_none() {
        return;
    }
    let identity = SystemPeerCodeIdentity
        .kernel_identity(self_audit_token().unwrap())
        .expect("child identity query");
    println!(
        "SMELTD_IDENTITY_PROBE team={:?} signing_id={:?}",
        identity.team, identity.signing_id
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_local_signed_kernel_identity_values() {
    // 本地自签名进程的内核身份形状：team 应为 None（无 Apple team），
    // signing-id 应为签名时 -i 指定的值。把测试二进制拷一份、自签名后拉起来
    // 自查——这是截图场景里那类构建，不覆盖它就等于没覆盖生产。
    // 需要本机 "Smelt Local Signing" 身份（setup-codesign-identity.sh）；
    // 没有（CI 裸机）则大声跳过。codesign 卡住超过 60s 也跳过，绝不 hanging suite。
    if std::process::Command::new("security")
        .args(["find-certificate", "-c", "Smelt Local Signing"])
        .output()
        .map(|output| !output.status.success())
        .unwrap_or(true)
    {
        eprintln!("SKIP: 本机没有 Smelt Local Signing 身份");
        return;
    }
    let dir = std::env::current_dir()
        .unwrap()
        .join("target/identity-probe-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join(format!("probe-child-{}", std::process::id()));
    let _ = std::fs::remove_file(&copy);
    std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();

    let sign_copy = copy.clone();
    let sign_status = std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let _ = sender.send(
                std::process::Command::new("codesign")
                    .args([
                        "--force",
                        "--sign",
                        "Smelt Local Signing",
                        "--identifier",
                        "smeltd-probe",
                    ])
                    .arg(&sign_copy)
                    .output(),
            );
        });
        receiver.recv_timeout(std::time::Duration::from_secs(60))
    });
    let Ok(Ok(sign_output)) = sign_status else {
        eprintln!("SKIP: codesign 无响应（钥匙串可能在弹框要密码）");
        let _ = std::fs::remove_file(&copy);
        return;
    };
    assert!(
        sign_output.status.success(),
        "codesign 失败：{}",
        String::from_utf8_lossy(&sign_output.stderr)
    );

    let mut child = std::process::Command::new(&copy)
        .env("SMELTD_IDENTITY_PROBE_CHILD", "1")
        .args([
            "probe_identity_print_for_spawned_copy",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn signed copy");
    let mut output = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            None => {
                child.kill().ok();
                panic!("签名副本 60s 内没跑完，自查失败");
            }
        }
    };
    use std::io::Read;
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    let _ = std::fs::remove_file(&copy);
    assert!(status.success(), "签名副本退出码非零");
    let output = String::from_utf8_lossy(&output);
    let marker = output
        .lines()
        .find(|line| line.contains("SMELTD_IDENTITY_PROBE"))
        .unwrap_or_else(|| panic!("子进程没输出身份行：{output}"));
    assert!(
        marker.contains("team=None"),
        "本地自签名应无 team：{marker}"
    );
    assert!(
        marker.contains("signing_id=Some(\"smeltd-probe\")"),
        "内核 signing-id 应与 codesign -i 一致：{marker}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_kernel_identity_survives_binary_replacement() {
    // 活体回归：把本测试二进制文件原地换掉（rename 语义，和安装器同一招），
    // 再查自己身份必须成功且一致。
    //
    // 背景：旧实现（Security framework 动态查询）在**签名构建**于此报 -67034
    // （生产实证×2：昨天 CheckValidity，今天 SigningInformation 链）。
    // ad-hoc 下旧链条碰巧不报错（无 CMS 可比）——这也正是 dev 难以复现生产
    // 故障的原因。新路径只问内核，对构建类型无依赖，一律免疫；Fake 永远
    // 表达不出这条（它不进框架）。取代旧的 Fake 版自更新测试——那版换的是
    // 假路径里的假字节，什么都没证明。
    let exe = std::env::current_exe().expect("test binary path");
    let backup = exe.with_extension("identity-test-bak");
    let _ = std::fs::remove_file(&backup);
    std::fs::copy(&exe, &backup).expect("backup test binary");
    struct RestoreOnDrop {
        backup: PathBuf,
        target: PathBuf,
    }
    impl Drop for RestoreOnDrop {
        fn drop(&mut self) {
            // panic 也要恢复。即使没恢复 cargo 也会按 mtime 重编，自愈。
            if self.backup.exists() {
                let _ = std::fs::rename(&self.backup, &self.target);
            }
        }
    }
    let _restore = RestoreOnDrop {
        backup: backup.clone(),
        target: exe.clone(),
    };
    let verifier = SystemPeerCodeIdentity;
    let token = self_audit_token().expect("self audit token");
    let before = verifier
        .kernel_identity(token)
        .expect("identity query must succeed");
    // 原地换掉运行中二进制的文件（新 inode；运行中进程不受影响）。
    // 注意：绝不能原地写旧 inode——内核会按代码签名 crash 直接 SIGKILL 本进程。
    let replacement = exe.with_extension("identity-test-new");
    std::fs::write(&replacement, b"not-a-macho-at-all").unwrap();
    std::fs::rename(&replacement, &exe).unwrap();
    let during = verifier
        .kernel_identity(token)
        .expect("identity must survive on-disk replacement (this was -67034)");
    assert_eq!(before, during, "换文件前后内核身份必须一致");
    drop(_restore);
    assert!(!backup.exists(), "备份应已搬回原位");
    let after = verifier.kernel_identity(token).expect("identity query");
    assert_eq!(before, after);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_auth_rejects_mismatched_local_certificates() {
    // Local leg 否定：证书不同 → 拒绝（且有证书就不进 debug sibling leg）。
    let daemon = PathBuf::from("/Users/test/.smelt/bin/smeltd");
    let desktop = PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt");
    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(7, Some(desktop.clone())),
        42,
        &daemon,
        &FakePeerCodeIdentity {
            kernel: BTreeMap::from([
                (42, Ok(kernel(None, Some("smeltd")))),
                (7, Ok(kernel(None, Some("com.zzfn.smelt")))),
            ]),
            certs: BTreeMap::from([
                (
                    daemon.display().to_string(),
                    Ok(Some("local-cert-daemon".to_string())),
                ),
                (
                    desktop.display().to_string(),
                    Ok(Some("local-cert-attacker".to_string())),
                ),
            ]),
        },
    )
    .unwrap_err();
    assert!(
        error.contains("not an authenticated first-party"),
        "{error}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_auth_rejects_mixed_cert_and_certless_sides() {
    // 单边有证书（local daemon + ad-hoc peer）：与旧 `(Signed, Unsigned) =>
    // false` 一致，直接拒绝——即使 debug 构建也不进 sibling 通道。
    let daemon = PathBuf::from("/Users/test/.smelt/bin/smeltd");
    let desktop = PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt");
    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(7, Some(desktop.clone())),
        42,
        &daemon,
        &FakePeerCodeIdentity {
            kernel: BTreeMap::from([
                (42, Ok(kernel(None, Some("smeltd")))),
                (7, Ok(kernel(None, Some("com.zzfn.smelt")))),
            ]),
            certs: BTreeMap::from([
                (
                    daemon.display().to_string(),
                    Ok(Some("local-cert".to_string())),
                ),
                (desktop.display().to_string(), Ok(None)),
            ]),
        },
    )
    .unwrap_err();
    assert!(
        error.contains("not an authenticated first-party"),
        "{error}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_auth_rejects_peers_without_an_audit_token() {
    // 没有 audit token 就无法避开 pid 复用竞态，此时必须拒绝，而不是退回用 pid 查签名。
    let daemon = PathBuf::from("/Users/test/.smelt/bin/smeltd");
    let error = authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &PeerIdentity {
            pid: 7,
            executable: Some(PathBuf::from(
                "/Applications/Smelt.app/Contents/MacOS/smelt",
            )),
            audit_token: None,
        },
        42,
        &daemon,
        &FakePeerCodeIdentity {
            kernel: BTreeMap::new(),
            certs: BTreeMap::new(),
        },
    )
    .unwrap_err();
    assert!(error.contains("did not provide an audit token"), "{error}");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_unsigned_auth_is_limited_to_exact_development_siblings() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/unsigned-peer-auth-tests");
    std::fs::create_dir_all(&root).unwrap();
    let daemon = root.join("smeltd");
    let desktop = root.join("smelt");
    let elsewhere = root.join("elsewhere/smelt");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    for executable in [&daemon, &desktop, &elsewhere] {
        std::fs::write(executable, b"unsigned").unwrap();
    }
    let verifier = FakePeerCodeIdentity {
        kernel: BTreeMap::from([
            (42, Ok(kernel(None, None))),
            (7, Ok(kernel(None, None))),
            (8, Ok(kernel(None, None))),
        ]),
        certs: BTreeMap::from([
            (daemon.display().to_string(), Ok(None)),
            (desktop.display().to_string(), Ok(None)),
            (elsewhere.display().to_string(), Ok(None)),
        ]),
    };

    authenticate_event_client_with_verifier(
        &serde_json::json!({
            "auth": {"type": "first_party", "kind": "desktop"}
        }),
        &peer(7, Some(desktop)),
        42,
        &daemon,
        &verifier,
    )
    .unwrap();
    assert!(
        authenticate_event_client_with_verifier(
            &serde_json::json!({
                "auth": {"type": "first_party", "kind": "desktop"}
            }),
            &peer(8, Some(elsewhere)),
            42,
            &daemon,
            &verifier,
        )
        .is_err()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn wire_auth_rejects_capability_escalation_and_removed_plugin_auth() {
    let peer = peer(42, None);
    let daemon_executable = PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smeltd");
    let escalation = authenticate_event_client(
        &serde_json::json!({
            "auth": {
                "type": "first_party",
                "kind": "remote_gateway",
                "capabilities": ["task.read"]
            }
        }),
        &peer,
        42,
        &daemon_executable,
    )
    .unwrap_err();
    assert!(escalation.contains("unknown field"));

    let plugin = authenticate_event_client(
        &serde_json::json!({
            "auth": {
                "type": "plugin",
                "credential": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        }),
        &peer,
        42,
        &daemon_executable,
    )
    .unwrap_err();
    assert!(plugin.contains("unknown variant"));
}

#[test]
fn durable_subscription_processes_retryable_nack_and_ack() {
    let event_hub = super::super::new_event_hub();
    let published = event_hub
        .publish_agent_message_delivered("message-1", "source", "target")
        .unwrap();
    let request = request(
        "agent-messages",
        CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
        DeliveryClass::Durable,
        None,
    );
    let (server, mut client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let worker_hub = Arc::clone(&event_hub);
    let worker = thread::spawn(move || {
        handle_event_subscribe(
            server,
            Some(&request),
            Ok(identity(&[CORE_CAPABILITY_AGENT_MESSAGE_READ])),
            &worker_hub,
        )
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    assert!(matches!(
        read_message(&mut reader),
        SubscribeMessage::Cursor {
            last_acked_sequence: 0
        }
    ));
    let SubscribeMessage::Event {
        sequence: Some(sequence),
        envelope,
    } = read_message(&mut reader)
    else {
        panic!("expected durable event");
    };
    let nack = SubscribeControlMessage::Nack {
        nack: Nack {
            subscription_id: SubscriptionId::new("agent-messages").unwrap(),
            sequence,
            event_id: envelope.event_id,
            disposition: NackDisposition::Retryable,
            error_code: "busy".to_string(),
            message: "retry".to_string(),
        },
    };
    writeln!(client, "{}", serde_json::to_string(&nack).unwrap()).unwrap();
    thread::sleep(Duration::from_millis(300));
    event_hub.runtime().pump_durable_subscribers().unwrap();
    let SubscribeMessage::Event {
        sequence: Some(redelivered),
        envelope,
    } = read_message(&mut reader)
    else {
        panic!("expected retry");
    };
    assert_eq!(redelivered, sequence);
    let ack = SubscribeControlMessage::Ack {
        ack: Ack {
            subscription_id: SubscriptionId::new("agent-messages").unwrap(),
            sequence,
            event_id: envelope.event_id,
        },
    };
    writeln!(client, "{}", serde_json::to_string(&ack).unwrap()).unwrap();

    let plugin_id = PluginId::new("test.protocol").unwrap();
    let subscription_id = SubscriptionId::new("agent-messages").unwrap();
    for _ in 0..100 {
        if event_hub
            .runtime()
            .subscription_cursor(&plugin_id, &subscription_id)
            .ok()
            == Some(published.sequence)
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        event_hub
            .runtime()
            .subscription_cursor(&plugin_id, &subscription_id)
            .unwrap(),
        published.sequence
    );
    client.shutdown(Shutdown::Both).unwrap();
    worker.join().unwrap();
}

#[test]
fn invalid_ack_closes_socket_and_reclaims_reader_worker() {
    let event_hub = super::super::new_event_hub();
    let request = request(
        "agent-messages",
        CORE_TOPIC_AGENT_MESSAGE_DELIVERED,
        DeliveryClass::Durable,
        None,
    );
    let (server, mut client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let worker_hub = Arc::clone(&event_hub);
    let worker = thread::spawn(move || {
        handle_event_subscribe(
            server,
            Some(&request),
            Ok(identity(&[CORE_CAPABILITY_AGENT_MESSAGE_READ])),
            &worker_hub,
        )
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    assert!(matches!(
        read_message(&mut reader),
        SubscribeMessage::Cursor { .. }
    ));
    let invalid = SubscribeControlMessage::Ack {
        ack: Ack {
            subscription_id: SubscriptionId::new("wrong-subscription").unwrap(),
            sequence: 1,
            event_id: smelt_plugin_api::EventId::new("event-1").unwrap(),
        },
    };
    writeln!(client, "{}", serde_json::to_string(&invalid).unwrap()).unwrap();
    assert!(matches!(
        read_message(&mut reader),
        SubscribeMessage::Error { .. }
    ));
    let mut eof = String::new();
    assert_eq!(reader.read_line(&mut eof).unwrap(), 0);
    worker
        .join()
        .expect("subscription and reader workers must exit");
}

#[test]
fn plugin_invoke_timeout_caps_at_thirty_seconds() {
    assert_eq!(
        plugin_invoke_timeout(10_000),
        std::time::Duration::from_secs(10)
    );
    assert_eq!(
        plugin_invoke_timeout(300_000),
        std::time::Duration::from_millis(MAX_PLUGIN_INVOKE_MS)
    );
}

#[test]
fn plugin_ui_operations_require_authenticated_desktop() {
    let remote_gateway = first_party_event_client(FirstPartyClientKind::RemoteGateway);
    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_contributions(server, Ok(remote_gateway.clone()));
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"],
        "plugin UI operation requires authenticated desktop"
    );

    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_invoke(server, &serde_json::json!({}), Ok(remote_gateway));
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"],
        "plugin UI operation requires authenticated desktop"
    );

    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_contributions(server, Err("missing auth".to_string()));
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"], "missing auth");

    let desktop = first_party_event_client(FirstPartyClientKind::Desktop);
    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_contributions(server, Ok(desktop));
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], true);
}

#[test]
fn plugin_set_enabled_requires_authenticated_desktop() {
    let remote_gateway = first_party_event_client(FirstPartyClientKind::RemoteGateway);
    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_set_enabled(
        server,
        &serde_json::json!({
            "plugin_id": "com.example.plugin",
            "enabled": false
        }),
        Ok(remote_gateway),
    );
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"],
        "plugin UI operation requires authenticated desktop"
    );

    let desktop = first_party_event_client(FirstPartyClientKind::Desktop);
    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_set_enabled(
        server,
        &serde_json::json!({
            "plugin_id": "com.example.plugin",
            "enabled": false
        }),
        Ok(desktop),
    );
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], true);
}

#[test]
fn plugin_reload_requires_authenticated_desktop() {
    let remote_gateway = first_party_event_client(FirstPartyClientKind::RemoteGateway);
    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_reload(server, Ok(remote_gateway));
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"],
        "plugin UI operation requires authenticated desktop"
    );

    let desktop = first_party_event_client(FirstPartyClientKind::Desktop);
    let (server, client) = UnixStream::pair().unwrap();
    handle_plugin_reload(server, Ok(desktop));
    let response: serde_json::Value = serde_json::from_reader(BufReader::new(client)).unwrap();
    assert_eq!(response["ok"], true);
}
