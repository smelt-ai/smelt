//! Pi 的凭据：列 provider、跑 OAuth 登录、注销。
//!
//! **为什么要起一个子进程。** Pi 的 `/login` 只活在它自己的交互式 TUI 里，RPC
//! 协议没有任何 auth 方法，CLI 也没有对应子命令。而 OAuth 流程本身（PKCE、
//! 回环回调服务器、device code 轮询、各家的令牌兑换端点）是 pi-ai 里几千行会
//! 随版本变的东西。在 Rust 里重写一份，等于承诺跟着 Pi 的每次协议调整改一遍，
//! 且写下的凭据形状一旦对不上，跑会话的那个 Pi 就登录不上。所以这里复用受管
//! 运行时里的 `src/auth-main.ts`，让 pi-ai 自己跑流程、自己写 `auth.json`。
//!
//! **为什么是双向流而不是一次调用。** 授权 URL、device code 要在流程中途推给
//! 用户，粘贴回来的 code 又要送回流程；浏览器回调和手工粘贴还会互相抢跑（谁先
//! 到算谁的，另一个被撤回）。这只能是一条边跑边说话的流：子进程 stdout 出事件，
//! stdin 进回答。
//!
//! 子进程 stdout 里的非协议行一律忽略——依赖库偶尔往那儿打日志，因为一行噪声
//! 把用户正在做的授权掐掉是不可接受的。

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

/// 受管运行时里的登录入口，相对 pi-agent 根目录。
const AUTH_SCRIPT: &str = "src/auth-main.ts";

/// 一次「问一句、答一句」的助手命令最多等这么久。首次调用可能连带装运行时，
/// 所以给得比一次网络往返宽。
const ONE_SHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// 一个 provider 当前的凭据状态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiAuthStatus {
    /// 订阅登录过，`auth.json` 里是 OAuth 令牌。
    Oauth,
    /// 有 API key——存在 `auth.json` 里，或来自环境变量、云厂商的环境凭据。
    ApiKey,
    #[default]
    None,
}

impl PiAuthStatus {
    pub fn configured(self) -> bool {
        !matches!(self, PiAuthStatus::None)
    }
}

/// 一个内置 provider 的登录能力与现状。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiAuthProvider {
    pub id: String,
    pub name: String,
    /// OAuth 那条路的按钮文案，例如「Sign in with Kimi Code」。
    #[serde(default)]
    pub oauth_name: Option<String>,
    /// API key 那条路的名字，例如「Anthropic API key」。
    #[serde(default)]
    pub api_key_name: Option<String>,
    /// OAuth 走的是订阅（Claude Pro/Max、Copilot 这类），不是按量计费的 key。
    #[serde(default)]
    pub subscription: bool,
    #[serde(default)]
    pub supports_oauth: bool,
    #[serde(default)]
    pub supports_api_key: bool,
    #[serde(default)]
    pub status: PiAuthStatus,
    /// 凭据从哪儿来，例如 "OAuth"、"ANTHROPIC_API_KEY"、"~/.aws/credentials"。
    #[serde(default)]
    pub source: Option<String>,
}

/// 登录方式。`Models.login` 按它选 `provider.auth.oauth` 还是 `auth.apiKey`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiLoginMethod {
    OAuth,
    ApiKey,
}

impl PiLoginMethod {
    fn as_arg(self) -> &'static str {
        match self {
            PiLoginMethod::OAuth => "oauth",
            PiLoginMethod::ApiKey => "api_key",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiPromptKind {
    Text,
    Secret,
    Select,
    ManualCode,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct PiPromptOption {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct PiAuthLink {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// 登录流程推给宿主的一步。
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PiLoginEvent {
    Providers {
        providers: Vec<PiAuthProvider>,
    },
    /// 某个 provider 当前凭据能用的模型目录。
    Models {
        models: Vec<crate::provider_api::DiscoveredModel>,
    },
    /// 去浏览器完成授权。宿主负责打开它——pi-ai 自己不开浏览器。
    AuthUrl {
        url: String,
        #[serde(default)]
        instructions: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    DeviceCode {
        user_code: String,
        verification_uri: String,
        #[serde(default)]
        interval_seconds: Option<u64>,
        #[serde(default)]
        expires_in_seconds: Option<u64>,
    },
    Info {
        message: String,
        #[serde(default)]
        links: Vec<PiAuthLink>,
    },
    Progress {
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    Prompt {
        id: String,
        prompt_type: PiPromptKind,
        message: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        options: Vec<PiPromptOption>,
    },
    /// 这个提问不用答了（浏览器回调先到了）。界面必须据此把输入框收掉。
    PromptDone {
        id: String,
    },
    Done,
    Error {
        message: String,
    },
    /// 助手报了一种这个版本还不认识的事件。忽略它即可——运行时可以比宿主新，
    /// 一个新事件不该让正在跑的登录变成失败。
    #[serde(other)]
    Unknown,
}

/// 解析子进程的一行输出。非协议行返回 `None`。
///
/// 「解析不了就当噪声」只对**不是协议行**的东西成立：依赖库偶尔往 stdout 打
/// 日志，不该因此掐掉用户正在做的授权。但一行明明写着 `"event":"device_code"`
/// 却反序列化失败，是两侧协议对不上——静默丢掉它，界面就永远停在「稍后会给出
/// 授权链接」上，没有任何线索。所以这种情况必须变成一条看得见的错误。
pub fn parse_login_event(line: &str) -> Option<PiLoginEvent> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let name = value.get("event")?.as_str()?.to_string();
    match serde_json::from_value(value) {
        Ok(PiLoginEvent::Unknown) => None,
        Ok(event) => Some(event),
        Err(error) => Some(PiLoginEvent::Error {
            message: format!("无法解析登录事件 {name}：{error}"),
        }),
    }
}

fn spawn_auth_process(
    args: &[&str],
    status: &dyn Fn(&str),
) -> Result<
    (
        std::process::Child,
        crate::managed_runtime::ManagedPiRuntime,
    ),
    String,
> {
    let (runtime, script) = crate::managed_runtime::sync_managed_pi_tool(AUTH_SCRIPT, status)?;
    let mut command = std::process::Command::new(&runtime.bun);
    command
        .arg(&script)
        .args(args)
        .current_dir(
            script
                .parent()
                .and_then(|src| src.parent())
                .unwrap_or(&script),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    runtime.inherit_into(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("启动 Pi 凭据助手失败：{error}"))?;
    Ok((child, runtime))
}

/// 收尾一个已经结束/要结束的子进程，把 stderr 拼成人能看的错误。
fn failure_reason(child: &mut std::process::Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    let stderr = stderr.trim();
    if stderr.is_empty() {
        "Pi 凭据助手没有输出结果".to_string()
    } else {
        stderr.lines().rev().take(3).collect::<Vec<_>>().join("；")
    }
}

/// 跑一次「问一句、答一句」的助手命令。
///
/// `pick` 从事件里挑出想要的结果；返回 `None` 表示继续读。子进程报的 `error`
/// 事件优先于退出码——它带的是 pi-ai 那边的原话。
fn run_once<T>(
    args: &[&str],
    status: &dyn Fn(&str),
    pick: impl Fn(PiLoginEvent) -> Option<T>,
) -> Result<T, String> {
    let (mut child, _runtime) = spawn_auth_process(args, status)?;
    let stdout = child.stdout.take().ok_or("Pi 凭据助手没有 stdout")?;
    // 看门狗单独一条线程：超时判断不能挂在「收到一行输出」上——真正会卡死的
    // 情形恰恰是子进程一声不吭（装运行时时网络吊住、等一个永远不来的回调），
    // 那时读循环停在 read 上，永远走不到超时检查。
    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = std::thread::spawn({
        let child = Arc::clone(&child);
        let timed_out = Arc::clone(&timed_out);
        let finished = Arc::clone(&finished);
        move || {
            let deadline = std::time::Instant::now() + ONE_SHOT_TIMEOUT;
            while std::time::Instant::now() < deadline {
                if finished.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if finished.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }
    });

    let mut result = None;
    let mut failure = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        match parse_login_event(&line) {
            Some(PiLoginEvent::Error { message }) => failure = Some(message),
            Some(event) => {
                if let Some(picked) = pick(event) {
                    result = Some(picked);
                }
            }
            None => {}
        }
    }
    finished.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = watchdog.join();
    let mut child = match child.lock() {
        Ok(child) => child,
        Err(poisoned) => poisoned.into_inner(),
    };
    let _ = child.wait();
    if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(format!("Pi 凭据助手超时（{}）", args.join(" ")));
    }
    match (result, failure) {
        (Some(result), _) => Ok(result),
        (None, Some(message)) => Err(message),
        (None, None) => Err(failure_reason(&mut child)),
    }
}

/// 列出所有内置 provider 及其凭据现状。
///
/// 同步调用，会起一个子进程并可能触发首次运行时安装，必须在后台线程里跑。
pub fn list_providers(status: &dyn Fn(&str)) -> Result<Vec<PiAuthProvider>, String> {
    run_once(&["list"], status, |event| match event {
        PiLoginEvent::Providers { providers } => Some(providers),
        _ => None,
    })
}

/// 列出某个 provider 当前凭据能用的模型。
pub fn list_models(
    provider_id: &str,
    status: &dyn Fn(&str),
) -> Result<Vec<crate::provider_api::DiscoveredModel>, String> {
    run_once(&["models", provider_id], status, |event| match event {
        PiLoginEvent::Models { models } => Some(models),
        _ => None,
    })
}

/// 删掉一个 provider 的凭据。
pub fn logout(provider_id: &str, status: &dyn Fn(&str)) -> Result<(), String> {
    run_once(&["logout", provider_id], status, |event| {
        matches!(event, PiLoginEvent::Done).then_some(())
    })
}

/// 一次进行中的登录。
///
/// 拿着它就等于拿着那个子进程：`Drop` 会杀掉它。这不是保险起见——回环回调服务器
/// 和 device code 轮询都活在子进程里，用户关掉对话框却留着它继续监听端口，下一次
/// 登录会因为端口被占直接失败。
pub struct PiLoginSession {
    child: Arc<Mutex<std::process::Child>>,
    stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    _runtime: crate::managed_runtime::ManagedPiRuntime,
    /// 用户主动取消过。之后子进程报的 abort 错误不该再当失败展示。
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

/// 把子进程 stdout 的每一行搬成事件，返回这条流是否**自己给出了结论**。
///
/// 「流结束」不等于「登录失败」：助手报完 `done` 就退出，这是成功的正常收尾。
/// 曾经这里无条件在流末尾补一条错误，于是成功的 `done` 立刻被一条
/// 「没有输出结果」顶掉——界面上登录成功和失败长得一模一样。补错误只对
/// **没有结论就断掉**的流成立（子进程崩了、被杀了）。
///
/// `emit` 返回 false 表示收件方没了，停止读取。
fn pump_login_events(reader: impl BufRead, mut emit: impl FnMut(PiLoginEvent) -> bool) -> bool {
    let mut settled = false;
    for line in reader.lines().map_while(Result::ok) {
        let Some(event) = parse_login_event(&line) else {
            continue;
        };
        settled |= matches!(event, PiLoginEvent::Done | PiLoginEvent::Error { .. });
        if !emit(event) {
            return settled;
        }
    }
    settled
}

impl PiLoginSession {
    /// 起一个登录流程，返回会话与事件流。
    ///
    /// 同步调用（可能触发运行时安装），在后台线程里起；事件流交给 UI 侧消费。
    ///
    /// `ask_all` 关掉助手的提问代答（企业版域名、登录方式这类「几乎只有一个
    /// 正确答案」的步骤），把每一问都交回给用户。
    pub fn start(
        provider_id: &str,
        method: PiLoginMethod,
        ask_all: bool,
        status: &dyn Fn(&str),
    ) -> Result<
        (
            Self,
            futures::channel::mpsc::UnboundedReceiver<PiLoginEvent>,
        ),
        String,
    > {
        let mut args = vec!["login", provider_id, "--type", method.as_arg()];
        if ask_all {
            args.push("--ask-all");
        }
        let (mut child, runtime) = spawn_auth_process(&args, status)?;
        let stdout = child.stdout.take().ok_or("Pi 凭据助手没有 stdout")?;
        let stdin = child.stdin.take();
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let session = Self {
            child: Arc::new(Mutex::new(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            _runtime: runtime,
            cancelled: cancelled.clone(),
        };
        let child_handle = session.child.clone();
        std::thread::spawn(move || {
            let settled = pump_login_events(BufReader::new(stdout), |event| {
                // 发送失败 = 界面已经不在了，别把子进程留在那儿占着回调端口。
                sender.unbounded_send(event).is_ok()
            });
            if !settled && !cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                let reason = child_handle
                    .lock()
                    .map(|mut child| failure_reason(&mut child))
                    .unwrap_or_else(|_| "Pi 凭据助手异常退出".to_string());
                let _ = sender.unbounded_send(PiLoginEvent::Error { message: reason });
            }
            if let Ok(mut child) = child_handle.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
        });
        Ok((session, receiver))
    }

    /// 回答一个提问。
    pub fn answer(&self, prompt_id: &str, value: &str) {
        self.send(&serde_json::json!({
            "command": "prompt_response",
            "id": prompt_id,
            "value": value,
        }));
    }

    /// 放弃登录。子进程会撤掉回调服务器并退出。
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.send(&serde_json::json!({ "command": "cancel" }));
    }

    fn send(&self, command: &serde_json::Value) {
        let Ok(mut guard) = self.stdin.lock() else {
            return;
        };
        let Some(stdin) = guard.as_mut() else {
            return;
        };
        // 写失败只意味着子进程已经走了，事件流那边会给出结论。
        let _ = writeln!(stdin, "{command}");
        let _ = stdin.flush();
    }
}

impl Drop for PiLoginSession {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // 先关 stdin：子进程读到 EOF 会自己收拾回调服务器再退。
        if let Ok(mut guard) = self.stdin.lock() {
            guard.take();
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端冒烟：真的装一次受管运行时、真的起 bun、真的问一遍 provider 列表。
    /// 会下载依赖、耗时几十秒，所以默认不跑；改动助手脚本或运行时版本后手动
    /// `cargo test -p smelt-core pi_auth -- --ignored --nocapture` 验一次。
    #[test]
    #[ignore = "需要网络与受管运行时安装"]
    fn the_helper_really_lists_providers() {
        let providers = list_providers(&|status| println!("{status}")).expect("列出 provider");
        assert!(
            providers.iter().any(|provider| provider.id == "anthropic"
                && provider.supports_oauth
                && provider.subscription),
            "anthropic 应当支持订阅登录：{providers:?}"
        );
        assert!(providers.len() > 10, "内置 provider 不该只有这么几个");
    }

    fn pump(lines: &str) -> (Vec<PiLoginEvent>, bool) {
        let mut events = Vec::new();
        let settled = pump_login_events(std::io::Cursor::new(lines.to_string()), |event| {
            events.push(event);
            true
        });
        (events, settled)
    }

    /// 成功收尾的流不能被再补一条错误。
    ///
    /// 助手报完 `done` 就退出，这是正常结束。曾经流末尾无条件补一条
    /// 「没有输出结果」，把刚设好的成功状态覆盖成失败——用户登录成功了，界面
    /// 却既不给成功提示也不刷新状态。
    #[test]
    fn a_stream_that_reported_done_needs_no_epilogue() {
        let (events, settled) =
            pump("{\"event\":\"progress\",\"message\":\"换取令牌…\"}\n{\"event\":\"done\"}\n");
        assert!(settled, "收到 done 的流已经有结论了");
        assert_eq!(events.len(), 2);
    }

    /// 助手自己报的错也是结论，不该被退出码的兜底盖掉——它带的是原话。
    #[test]
    fn a_reported_error_is_also_a_conclusion() {
        let (_, settled) = pump("{\"event\":\"error\",\"message\":\"Login cancelled\"}\n");
        assert!(settled);
    }

    /// 一声不吭就断掉的流没有结论，得由调用方去查子进程为什么走了。
    #[test]
    fn a_stream_that_just_stops_has_no_conclusion() {
        let (events, settled) = pump("{\"event\":\"progress\",\"message\":\"启动回调服务器\"}\n");
        assert!(!settled, "只有进度、没有结论：{events:?}");
        assert!(!pump("").1);
    }

    /// 真的起一次 GitHub Copilot 登录，断言宿主收得到设备码。
    ///
    /// 上面那些解析测试用的是我手写的 JSON，而两侧命名一旦分头演进，手写的
    /// 样例会跟着错误的一侧走。这条把助手真实的输出接到真实的解析上——
    /// 设备码事件走完整条链路，才算协议真的对得上。
    ///
    /// 走网络（要向 GitHub 申请设备码），默认不跑。
    #[test]
    #[ignore = "需要网络与受管运行时安装"]
    fn a_real_login_delivers_a_device_code_to_the_host() {
        let (session, events) =
            PiLoginSession::start("github-copilot", PiLoginMethod::OAuth, false, &|status| {
                println!("{status}")
            })
            .expect("启动登录");
        let collected = futures::executor::block_on(async {
            let mut events = events;
            let mut collected = Vec::new();
            // 设备码要等一次 GitHub 往返；拿到就走，不必等轮询超时。
            while let Some(event) = futures::StreamExt::next(&mut events).await {
                let done = matches!(
                    event,
                    PiLoginEvent::DeviceCode { .. } | PiLoginEvent::Error { .. }
                );
                collected.push(event);
                if done {
                    break;
                }
            }
            collected
        });
        session.cancel();
        let device_code = collected.iter().find_map(|event| match event {
            PiLoginEvent::DeviceCode { user_code, .. } => Some(user_code.clone()),
            _ => None,
        });
        assert!(
            device_code.is_some_and(|code| !code.is_empty()),
            "应当收到非空设备码，实际收到：{collected:?}"
        );
        // 企业版域名那一问必须已经被代答掉，否则用户会卡在一个空输入框上。
        assert!(
            !collected
                .iter()
                .any(|event| matches!(event, PiLoginEvent::Prompt { .. })),
            "登录 github.com 不该向用户提问：{collected:?}"
        );
    }

    /// 每一种事件都要能整条落地。
    ///
    /// 这些 JSON 逐字对应 `packages/pi-agent/src/auth-protocol.ts` 里的
    /// `HostEvent`。曾经 `device_code` 的字段在 TS 那边是 camelCase、在这边是
    /// snake_case，解析失败被当成噪声丢掉，界面就永远停在等待上——所以每个
    /// 可选字段这里都断言成 `Some`：只测「解析没报错」抓不到少一个键。
    #[test]
    fn every_event_shape_survives_the_round_trip() {
        assert_eq!(
            parse_login_event(
                r#"{"event":"device_code","userCode":"552E-B413","verificationUri":"https://github.com/login/device","intervalSeconds":5,"expiresInSeconds":898}"#
            ),
            Some(PiLoginEvent::DeviceCode {
                user_code: "552E-B413".into(),
                verification_uri: "https://github.com/login/device".into(),
                interval_seconds: Some(5),
                expires_in_seconds: Some(898),
            })
        );
        assert_eq!(
            parse_login_event(
                r#"{"event":"auth_url","url":"https://claude.ai/oauth","instructions":"paste the code"}"#
            ),
            Some(PiLoginEvent::AuthUrl {
                url: "https://claude.ai/oauth".into(),
                instructions: Some("paste the code".into()),
            })
        );
        assert_eq!(
            parse_login_event(
                r#"{"event":"prompt","id":"1","promptType":"select","message":"Select login method:","placeholder":"company.ghe.com","options":[{"id":"browser","label":"Browser login","description":"default"}]}"#
            ),
            Some(PiLoginEvent::Prompt {
                id: "1".into(),
                prompt_type: PiPromptKind::Select,
                message: "Select login method:".into(),
                placeholder: Some("company.ghe.com".into()),
                options: vec![PiPromptOption {
                    id: "browser".into(),
                    label: "Browser login".into(),
                    description: Some("default".into()),
                }],
            })
        );
        assert_eq!(
            parse_login_event(
                r#"{"event":"info","message":"almost there","links":[{"url":"https://pi.dev","label":"docs"}]}"#
            ),
            Some(PiLoginEvent::Info {
                message: "almost there".into(),
                links: vec![PiAuthLink {
                    url: "https://pi.dev".into(),
                    label: Some("docs".into()),
                }],
            })
        );
        assert_eq!(
            parse_login_event(r#"{"event":"prompt_done","id":"1"}"#),
            Some(PiLoginEvent::PromptDone { id: "1".into() })
        );
        assert_eq!(
            parse_login_event(r#"{"event":"done"}"#),
            Some(PiLoginEvent::Done)
        );

        let models = parse_login_event(
            r#"{"event":"models","models":[{"id":"gpt-5","name":"GPT-5","contextWindow":400000,"maxTokens":128000}]}"#,
        );
        let Some(PiLoginEvent::Models { models }) = models else {
            panic!("模型事件没解析出来：{models:?}");
        };
        assert_eq!(models[0].id, "gpt-5");
        assert_eq!(models[0].context_window, Some(400_000));
        assert_eq!(models[0].max_tokens, Some(128_000));

        let providers = parse_login_event(
            r#"{"event":"providers","providers":[{"id":"github-copilot","name":"GitHub Copilot","oauthName":"Sign in","apiKeyName":"token","subscription":true,"supportsOauth":true,"supportsApiKey":true,"status":"oauth","source":"OAuth"}]}"#,
        );
        let Some(PiLoginEvent::Providers { providers }) = providers else {
            panic!("provider 事件没解析出来：{providers:?}");
        };
        assert!(providers[0].supports_oauth && providers[0].subscription);
        assert_eq!(providers[0].oauth_name.as_deref(), Some("Sign in"));
        assert_eq!(providers[0].source.as_deref(), Some("OAuth"));
    }

    /// 一行自称是协议事件、却读不动，必须说出来。
    ///
    /// 当成噪声吞掉，界面会一直停在「等待授权链接」，用户和日志都拿不到线索。
    #[test]
    fn a_malformed_protocol_line_surfaces_instead_of_vanishing() {
        let event = parse_login_event(r#"{"event":"device_code","userCode":"ABC"}"#);
        let Some(PiLoginEvent::Error { message }) = event else {
            panic!("协议不匹配应当变成错误：{event:?}");
        };
        assert!(
            message.contains("device_code"),
            "错误要指出是哪种事件：{message}"
        );
    }

    #[test]
    fn protocol_lines_become_events() {
        assert_eq!(
            parse_login_event(r#"{"event":"progress","message":"换取令牌…"}"#),
            Some(PiLoginEvent::Progress {
                message: "换取令牌…".into()
            })
        );
        assert_eq!(
            parse_login_event(
                r#"{"event":"prompt","id":"1","promptType":"manual_code","message":"贴 code","placeholder":"http://localhost:53692/callback"}"#
            ),
            Some(PiLoginEvent::Prompt {
                id: "1".into(),
                prompt_type: PiPromptKind::ManualCode,
                message: "贴 code".into(),
                placeholder: Some("http://localhost:53692/callback".into()),
                options: Vec::new(),
            })
        );
        assert_eq!(
            parse_login_event(r#"{"event":"prompt_done","id":"1"}"#),
            Some(PiLoginEvent::PromptDone { id: "1".into() })
        );
        assert_eq!(
            parse_login_event(r#"{"event":"done"}"#),
            Some(PiLoginEvent::Done)
        );
    }

    #[test]
    fn noise_on_stdout_is_ignored_not_fatal() {
        // 依赖库偶尔往 stdout 打日志。因为一行噪声把用户正在做的授权掐掉，
        // 比忽略它糟得多。
        assert_eq!(parse_login_event("Debugger attached."), None);
        assert_eq!(parse_login_event(""), None);
        assert_eq!(
            parse_login_event(r#"{"event":"unknown_future_thing"}"#),
            None
        );
        assert_eq!(parse_login_event("{ not json"), None);
    }

    #[test]
    fn provider_list_keeps_both_login_methods() {
        let event = parse_login_event(
            r#"{"event":"providers","providers":[{"id":"github-copilot","name":"GitHub Copilot","oauthName":"GitHub Copilot","apiKeyName":"GitHub Copilot token","subscription":true,"supportsOauth":true,"supportsApiKey":true,"status":"oauth","source":"OAuth"}]}"#,
        );
        let Some(PiLoginEvent::Providers { providers }) = event else {
            panic!("应解析出 provider 列表");
        };
        let copilot = &providers[0];
        assert_eq!(copilot.id, "github-copilot");
        assert!(copilot.supports_oauth && copilot.supports_api_key);
        assert!(copilot.subscription);
        assert_eq!(copilot.status, PiAuthStatus::Oauth);
        assert!(copilot.status.configured());
        assert_eq!(copilot.source.as_deref(), Some("OAuth"));
    }

    #[test]
    fn a_provider_without_credentials_reports_none() {
        let event = parse_login_event(
            r#"{"event":"providers","providers":[{"id":"deepseek","name":"DeepSeek","apiKeyName":"DeepSeek API key","subscription":false,"supportsOauth":false,"supportsApiKey":true,"status":"none"}]}"#,
        );
        let Some(PiLoginEvent::Providers { providers }) = event else {
            panic!("应解析出 provider 列表");
        };
        assert_eq!(providers[0].status, PiAuthStatus::None);
        assert!(!providers[0].status.configured());
        assert_eq!(providers[0].oauth_name, None);
    }
}
