//! 展台演示：独立的数据根，和主人的 `~/.smelt`、智能体配置分开。
//!
//! `Smelt Demo.app`（`Info.plist` 的 `SmeltDemo` / bundle id）和开发时的
//! `SMELT_DEMO=1`、`--demo` 走同一条隔离。进程环境只改一次；测试走 [`plan`]，
//! 不调用 [`activate`]（那会改本进程的 `HOME`）。

mod seed;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use seed::{
    notes_committed, notes_working, project_name, showcase_entries, showcase_permission,
};

pub const BUNDLE_ID: &str = "com.zzfn.smelt.demo";
pub const DISPLAY_NAME: &str = "Smelt Demo";
/// 展台窗口逻辑点。录屏的人用系统工具对准这扇窗口。
pub const WINDOW_WIDTH: f32 = 1440.;
pub const WINDOW_HEIGHT: f32 = 900.;
pub const SHOWCASE_CONVERSATION_ID: &str = "demo-lumen-conversation";
pub const SHOWCASE_AGENT_ID: &str = "demo-lumen-editor";
pub const DEMO_ROOT_MARKER: &str = ".smelt-demo-root";

const SEEDED_MARKER: &str = ".demo-seeded";

/// 子进程必须继续落在演示目录里的变量。登录 shell 的快照不能把它们改回主人的家。
const PROTECTED_ENV: &[&str] = &[
    "HOME",
    "SMELT_HOME",
    "SMELT_DEMO",
    "SMELT_DEMO_HOME",
    "SMELT_DEMO_ROUTE",
    "SMELT_DEMO_LIVE_AGENTS",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GROK_HOME",
    "COPILOT_HOME",
    "ZDOTDIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
];

const UNSET_ENV: &[&str] = &["SSH_AUTH_SOCK", "SSH_AGENT_PID", "GIT_SSH_COMMAND"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoRoute {
    Workspace,
    Agents,
    Automations,
    Conversation,
    SettingsAgentRuntime,
}

impl DemoRoute {
    pub fn id(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Agents => "agents",
            Self::Automations => "automations",
            Self::Conversation => "conversation",
            Self::SettingsAgentRuntime => "settings-agent-runtime",
        }
    }
}

/// 纯输入。`process_home_is_demo_root` 由调用方看过标记文件后再传入，计划本身不读盘。
pub struct ActivationInput<'a> {
    pub args: &'a [String],
    pub demo_flag: Option<&'a str>,
    pub demo_home: Option<&'a str>,
    pub demo_route: Option<&'a str>,
    pub process_home: Option<&'a Path>,
    pub process_home_is_demo_root: bool,
    pub passwd_home: &'a Path,
    pub temp_dir: &'a Path,
    pub pid: u32,
    pub bundle_demo: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DemoPlan {
    pub home: PathBuf,
    pub route: DemoRoute,
}

impl DemoPlan {
    pub fn smelt_home(&self) -> PathBuf {
        self.home.join(".smelt")
    }

    pub fn project_dir(&self) -> PathBuf {
        self.home.join("projects").join(project_name())
    }
}

static STATE: OnceLock<Option<DemoPlan>> = OnceLock::new();

/// 是否走演示隔离。渲染路径只读这个缓存，不读盘。
pub fn is_active() -> bool {
    STATE.get().is_some_and(|slot| slot.is_some())
}

pub fn route() -> DemoRoute {
    STATE
        .get()
        .and_then(|slot| slot.as_ref())
        .map(|plan| plan.route)
        .unwrap_or(DemoRoute::Workspace)
}

/// 演示项目目录。隔离生效后才有意义。
pub fn project_dir() -> PathBuf {
    match STATE.get() {
        Some(Some(plan)) => plan.project_dir(),
        _ => PathBuf::from("projects").join(project_name()),
    }
}

/// 在碰 `~/.smelt`、更新锁、登录 shell 探测之前调用一次。
///
/// 失败就退出：继续跑会写进主人的家。
pub fn activate() {
    let _ = STATE.get_or_init(|| {
        let Some(plan) = plan_from_process() else {
            return None;
        };
        if let Err(error) = install(&plan) {
            eprintln!("演示模式无法隔离数据目录：{error}");
            std::process::exit(1);
        }
        Some(plan)
    });
}

pub fn plan(input: &ActivationInput<'_>) -> Option<DemoPlan> {
    if !requested(input) {
        return None;
    }
    Some(DemoPlan {
        home: choose_home(input),
        route: choose_route(input.args, input.demo_route),
    })
}

pub fn parse_route_id(raw: &str) -> Option<DemoRoute> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "workspace" | "session" => Some(DemoRoute::Workspace),
        "agents" => Some(DemoRoute::Agents),
        "automations" => Some(DemoRoute::Automations),
        "conversation" => Some(DemoRoute::Conversation),
        "settings-agent-runtime" => Some(DemoRoute::SettingsAgentRuntime),
        _ => None,
    }
}

pub fn flag_on(value: Option<&str>) -> bool {
    matches!(
        value
            .map(str::trim)
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes")
    )
}

/// 密钥类变量名。`SMELT_*` 留给隔离本身，不在这里清掉。
pub fn is_secret_env_key(key: &str) -> bool {
    if key.starts_with("SMELT_") {
        return false;
    }
    let upper = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "API_KEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn requested(input: &ActivationInput<'_>) -> bool {
    input.bundle_demo || flag_on(input.demo_flag) || args_have_demo(input.args)
}

fn args_have_demo(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--demo")
}

fn choose_route(args: &[String], env_route: Option<&str>) -> DemoRoute {
    route_from_args(args)
        .or_else(|| env_route.and_then(parse_route_id))
        .unwrap_or(DemoRoute::Workspace)
}

fn route_from_args(args: &[String]) -> Option<DemoRoute> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--demo-route=") {
            return parse_route_id(value).or(Some(DemoRoute::Workspace));
        }
        if arg == "--demo-route" {
            let value = args.next().map(String::as_str).unwrap_or("");
            return parse_route_id(value).or(Some(DemoRoute::Workspace));
        }
    }
    None
}

fn choose_home(input: &ActivationInput<'_>) -> PathBuf {
    if input.process_home_is_demo_root
        && let Some(home) = input.process_home
        && home.is_absolute()
        && !path_has_parent_dir(home)
        && !overlaps_real_home(home, input.passwd_home)
    {
        return home.to_path_buf();
    }
    if let Some(override_home) = input
        .demo_home
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(override_home);
        if path.is_absolute()
            && !path_has_parent_dir(&path)
            && !overlaps_real_home(&path, input.passwd_home)
        {
            return path;
        }
    }
    let fallback = input
        .temp_dir
        .join(format!("smelt-demo-home-{}", input.pid));
    if fallback.is_absolute()
        && !path_has_parent_dir(&fallback)
        && !overlaps_real_home(&fallback, input.passwd_home)
    {
        return fallback;
    }
    PathBuf::from(format!("/tmp/smelt-demo-home-{}", input.pid))
}

fn path_has_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn overlaps_real_home(path: &Path, passwd_home: &Path) -> bool {
    // 空的 passwd 家不能拿来判断：`starts_with("")` 对任何路径都成立，结果是
    // 所有覆盖目录都被拒绝，退回临时目录。这是安全的方向。
    if passwd_home.as_os_str().is_empty() {
        return true;
    }
    located_in_real_home(path, passwd_home)
}

fn located_in_real_home(path: &Path, passwd_home: &Path) -> bool {
    if passwd_home.as_os_str().is_empty() {
        return false;
    }
    if path.starts_with(passwd_home) {
        return true;
    }
    [".smelt", ".claude", ".codex", "Library"]
        .into_iter()
        .any(|name| {
            let sensitive = passwd_home.join(name);
            path == sensitive || path.starts_with(&sensitive)
        })
}

fn plan_from_process() -> Option<DemoPlan> {
    let passwd_home = passwd_home().unwrap_or_default();
    let process_home = std::env::var_os("HOME").map(PathBuf::from);
    let process_home_is_demo_root = process_home
        .as_ref()
        .is_some_and(|home| home.join(DEMO_ROOT_MARKER).is_file());
    let demo_flag = std::env::var("SMELT_DEMO").ok();
    let demo_home = std::env::var("SMELT_DEMO_HOME").ok();
    let demo_route = std::env::var("SMELT_DEMO_ROUTE").ok();
    let args = std::env::args().collect::<Vec<_>>();
    plan(&ActivationInput {
        args: &args,
        demo_flag: demo_flag.as_deref(),
        demo_home: demo_home.as_deref(),
        demo_route: demo_route.as_deref(),
        process_home: process_home.as_deref(),
        process_home_is_demo_root,
        passwd_home: &passwd_home,
        temp_dir: &std::env::temp_dir(),
        pid: std::process::id(),
        bundle_demo: bundle_marks_demo(),
    })
}

fn install(plan: &DemoPlan) -> Result<(), String> {
    // 先看解析后的路径。符号链接或 `..` 不能把种子写进主人的家。
    reject_if_resolved_into_real_home(&plan.home)?;
    std::fs::create_dir_all(&plan.home).map_err(|error| error.to_string())?;
    let resolved = std::fs::canonicalize(&plan.home).map_err(|error| error.to_string())?;
    reject_if_in_real_home(&resolved)?;
    for relative in [
        ".claude",
        ".codex",
        ".grok",
        ".copilot",
        ".config",
        ".local/share",
        ".smelt",
    ] {
        std::fs::create_dir_all(plan.home.join(relative)).map_err(|error| error.to_string())?;
    }
    let marker = plan.home.join(DEMO_ROOT_MARKER);
    std::fs::write(&marker, "smelt demo root\n").map_err(|error| error.to_string())?;

    let home = plan.home.to_string_lossy();
    let smelt_home = plan.smelt_home();
    let smelt_home = smelt_home.to_string_lossy();
    // 先钉住隔离变量，再清密钥。清的时候跳过 SMELT_*，不会把刚写上的旗标清掉。
    unsafe {
        std::env::set_var("HOME", home.as_ref());
        std::env::set_var("SMELT_HOME", smelt_home.as_ref());
        std::env::set_var("SMELT_DEMO", "1");
        std::env::set_var("SMELT_DEMO_ROUTE", plan.route.id());
        std::env::set_var("CLAUDE_CONFIG_DIR", plan.home.join(".claude"));
        std::env::set_var("CODEX_HOME", plan.home.join(".codex"));
        std::env::set_var("GROK_HOME", plan.home.join(".grok"));
        std::env::set_var("COPILOT_HOME", plan.home.join(".copilot"));
        std::env::set_var("ZDOTDIR", plan.home.as_os_str());
        std::env::set_var("XDG_CONFIG_HOME", plan.home.join(".config"));
        std::env::set_var("XDG_DATA_HOME", plan.home.join(".local/share"));
    }
    scrub_process_env();
    seed::seed_into(&plan.home)
}

fn reject_if_resolved_into_real_home(home: &Path) -> Result<(), String> {
    if home.exists() {
        let resolved = std::fs::canonicalize(home).map_err(|error| error.to_string())?;
        return reject_if_in_real_home(&resolved);
    }
    let mut ancestor = home;
    let mut pending = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            break;
        };
        pending.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            break;
        };
        if parent == ancestor {
            break;
        }
        ancestor = parent;
    }
    if !ancestor.exists() {
        return Ok(());
    }
    let mut resolved = std::fs::canonicalize(ancestor).map_err(|error| error.to_string())?;
    for name in pending.into_iter().rev() {
        resolved.push(name);
    }
    reject_if_in_real_home(&resolved)
}

fn reject_if_in_real_home(path: &Path) -> Result<(), String> {
    let Some(passwd) = passwd_home() else {
        return Ok(());
    };
    if located_in_real_home(path, &passwd) {
        return Err(format!(
            "演示目录解析后落在真实家目录里：{}",
            path.display()
        ));
    }
    Ok(())
}

fn scrub_process_env() {
    let keys = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| should_scrub(key))
        .collect::<Vec<_>>();
    unsafe {
        for key in keys {
            std::env::remove_var(key);
        }
    }
}

fn should_scrub(key: &OsStr) -> bool {
    let Some(text) = key.to_str() else {
        return false;
    };
    UNSET_ENV.contains(&text) || is_secret_env_key(text)
}

fn bundle_marks_demo() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let Some(contents) = executable.parent().and_then(|macos| macos.parent()) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(contents.join("Info.plist")) else {
        return false;
    };
    plist_bool_true(&text, "SmeltDemo")
        || plist_string(&text, "CFBundleIdentifier").as_deref() == Some(BUNDLE_ID)
}

fn plist_bool_true(text: &str, key: &str) -> bool {
    let needle = format!("<key>{key}</key>");
    let Some(index) = text.find(&needle) else {
        return false;
    };
    let rest = text[index + needle.len()..].trim_start();
    rest.starts_with("<true/>") || rest.starts_with("<true />")
}

fn plist_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("<key>{key}</key>");
    let index = text.find(&needle)? + needle.len();
    let rest = text[index..].trim_start();
    let rest = rest.strip_prefix("<string>")?;
    let end = rest.find("</string>")?;
    Some(rest[..end].trim().to_string())
}

#[cfg(unix)]
fn passwd_home() -> Option<PathBuf> {
    // 必须在改 HOME 之前取。`dirs::home_dir` 先看 HOME，演示子进程的 HOME 已经是临时目录。
    unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if entry.is_null() {
            return None;
        }
        let directory = std::ffi::CStr::from_ptr((*entry).pw_dir);
        let text = directory.to_string_lossy();
        (!text.is_empty()).then(|| PathBuf::from(text.as_ref()))
    }
}

#[cfg(not(unix))]
fn passwd_home() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(args: &'a [String], passwd: &'a Path, temp: &'a Path) -> ActivationInput<'a> {
        ActivationInput {
            args,
            demo_flag: None,
            demo_home: None,
            demo_route: None,
            process_home: None,
            process_home_is_demo_root: false,
            passwd_home: passwd,
            temp_dir: temp,
            pid: 42,
            bundle_demo: false,
        }
    }

    #[test]
    fn stays_inactive_without_a_flag() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        assert!(plan(&input(&[], &passwd, &temp)).is_none());
    }

    #[test]
    fn demo_flag_uses_a_temp_home_outside_the_passwd_home() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        let mut spec = input(&[], &passwd, &temp);
        spec.demo_flag = Some("1");
        let plan = plan(&spec).expect("flag turns demo on");
        assert_eq!(plan.home, temp.join("smelt-demo-home-42"));
        assert!(!plan.home.starts_with(&passwd));
    }

    #[test]
    fn override_inside_the_passwd_home_is_rejected() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        let mut spec = input(&[], &passwd, &temp);
        spec.demo_flag = Some("yes");
        spec.demo_home = Some("/Users/me/booth");
        let plan = plan(&spec).expect("demo still starts");
        assert_eq!(plan.home, temp.join("smelt-demo-home-42"));

        spec.demo_home = Some("/Users/me/.claude/demo");
        assert_eq!(
            plan(&spec).expect("rejected override").home,
            temp.join("smelt-demo-home-42")
        );
        spec.demo_home = Some("relative/demo");
        assert_eq!(
            plan(&spec).expect("relative override").home,
            temp.join("smelt-demo-home-42")
        );
    }

    #[test]
    fn override_outside_the_passwd_home_is_used() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        let mut spec = input(&[], &passwd, &temp);
        spec.bundle_demo = true;
        spec.demo_home = Some("/var/booth/smelt-demo");
        let plan = plan(&spec).expect("bundle");
        assert_eq!(plan.home, PathBuf::from("/var/booth/smelt-demo"));
    }

    #[test]
    fn existing_demo_root_is_reused() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        let current = PathBuf::from("/tmp/smelt-demo-home-7");
        let mut spec = input(&[], &passwd, &temp);
        spec.demo_flag = Some("true");
        spec.process_home = Some(&current);
        spec.process_home_is_demo_root = true;
        let plan = plan(&spec).expect("reuse");
        assert_eq!(plan.home, current);
    }

    #[test]
    fn marked_home_inside_the_passwd_home_is_not_reused() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        let current = PathBuf::from("/Users/me");
        let mut spec = input(&[], &passwd, &temp);
        spec.demo_flag = Some("1");
        spec.process_home = Some(&current);
        spec.process_home_is_demo_root = true;
        assert_eq!(
            plan(&spec).expect("fallback").home,
            temp.join("smelt-demo-home-42")
        );
    }

    #[test]
    fn override_with_parent_dir_is_rejected() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        let mut spec = input(&[], &passwd, &temp);
        spec.demo_flag = Some("1");
        spec.demo_home = Some("/tmp/booth/../../Users/me/.smelt");
        assert_eq!(
            plan(&spec).expect("fallback").home,
            temp.join("smelt-demo-home-42")
        );
    }

    #[test]
    fn temp_dir_inside_the_passwd_home_uses_system_tmp() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/Users/me/Library/Caches");
        let mut spec = input(&[], &passwd, &temp);
        spec.demo_flag = Some("1");
        assert_eq!(
            plan(&spec).expect("fallback").home,
            PathBuf::from("/tmp/smelt-demo-home-42")
        );
    }

    #[test]
    fn route_comes_from_the_cli_before_the_env() {
        let passwd = PathBuf::from("/Users/me");
        let temp = PathBuf::from("/tmp");
        let args = vec![
            "smelt".into(),
            "--demo".into(),
            "--demo-route=conversation".into(),
        ];
        let mut spec = input(&args, &passwd, &temp);
        spec.demo_route = Some("agents");
        let plan = plan(&spec).expect("cli");
        assert_eq!(plan.route, DemoRoute::Conversation);

        spec.args = &[];
        spec.demo_flag = Some("1");
        spec.demo_route = Some("session");
        assert_eq!(plan(&spec).expect("env").route, DemoRoute::Workspace);
        spec.demo_route = Some("settings-agent-runtime");
        assert_eq!(
            plan(&spec).expect("settings").route,
            DemoRoute::SettingsAgentRuntime
        );
        spec.demo_route = Some("nope");
        assert_eq!(plan(&spec).expect("unknown").route, DemoRoute::Workspace);
    }

    #[test]
    fn secret_keys_are_named_without_touching_smelt_vars() {
        assert!(is_secret_env_key("OPENAI_API_KEY"));
        assert!(is_secret_env_key("AWS_SECRET_ACCESS_KEY"));
        assert!(is_secret_env_key("GITHUB_TOKEN"));
        assert!(!is_secret_env_key("SMELT_DEMO"));
        assert!(!is_secret_env_key("SMELT_API_KEY"));
        assert!(!is_secret_env_key("PATH"));
        assert!(should_scrub(OsStr::new("SSH_AUTH_SOCK")));
    }

    #[test]
    fn plist_markers_recognize_the_demo_bundle() {
        let text = r#"<key>CFBundleIdentifier</key>
        <string>com.zzfn.smelt.demo</string>
        <key>SmeltDemo</key>
        <true/>"#;
        assert!(plist_bool_true(text, "SmeltDemo"));
        assert_eq!(
            plist_string(text, "CFBundleIdentifier").as_deref(),
            Some(BUNDLE_ID)
        );
        assert!(!plist_bool_true(
            "<key>SmeltDemo</key><false/>",
            "SmeltDemo"
        ));
    }

    #[test]
    fn protected_env_keeps_home_and_agent_dirs() {
        assert!(PROTECTED_ENV.contains(&"HOME"));
        assert!(PROTECTED_ENV.contains(&"SMELT_HOME"));
        assert!(PROTECTED_ENV.contains(&"CLAUDE_CONFIG_DIR"));
        assert!(PROTECTED_ENV.contains(&"CODEX_HOME"));
        assert!(PROTECTED_ENV.contains(&"ZDOTDIR"));
    }

    #[test]
    fn empty_passwd_home_rejects_every_override() {
        let passwd = PathBuf::new();
        let temp = PathBuf::from("/tmp");
        let mut spec = input(&[], &passwd, &temp);
        spec.demo_flag = Some("1");
        spec.demo_home = Some("/var/booth");
        assert_eq!(
            plan(&spec).expect("fallback").home,
            temp.join("smelt-demo-home-42")
        );
    }
}
