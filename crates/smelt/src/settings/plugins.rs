//! Bundled 插件启用状态与设置页清单。

use super::*;
use smelt_core::plugin_enablement::PluginEnablement as StoredPluginEnablement;

/// 设置页列出的一份已安装插件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub user_installed: bool,
    pub load_error: Option<String>,
}

/// 设置页与侧栏读取的插件开关 + 已安装清单。
#[derive(Clone, Default)]
pub struct PluginEnablementState {
    pub enablement: StoredPluginEnablement,
    pub catalog: Vec<InstalledPlugin>,
    pub pending_install: Option<smelt_plugin_host::PluginInstallPlan>,
}

impl Global for PluginEnablementState {}

impl PluginEnablementState {
    pub fn load() -> Self {
        Self {
            enablement: StoredPluginEnablement::load(),
            catalog: discover_installed_plugins(),
            pending_install: None,
        }
    }

    pub fn describe_capability(capability: &str) -> (&'static str, &'static str) {
        match capability {
            "session.read" => ("读取会话", "读取本机终端会话及其状态"),
            "remote_sessions.read" => ("读取远程会话", "读取远程设备上的会话列表和状态"),
            "workspace.read" => ("读取工作区", "读取当前工作区中的文件和目录"),
            "workspace.create" => ("创建工作区", "创建新的工作区或工作目录"),
            "workspace.release" => ("释放工作区", "关闭并释放已有工作区"),
            "project.read" => ("读取项目", "读取项目元数据和项目列表"),
            "agent.message.read" => ("读取 Agent 消息", "读取 Agent 之间交换的消息"),
            "session.input.route" => ("路由会话输入", "接收并处理发往会话的输入"),
            "web.browse" => ("浏览网页", "代表你打开并读取网页内容"),
            "ui.contribute" => ("扩展界面", "向 Smelt 注册页面、Tab 或其他界面入口"),
            "ui.context.read" => ("读取界面上下文", "读取当前页面、选择项等界面状态"),
            "ui.dialog" => ("显示对话框", "在 Smelt 中打开提示或确认对话框"),
            "fs.reveal" => ("在访达中显示", "在系统文件管理器中定位文件或目录"),
            _ => ("未知权限", "当前版本的 Smelt 不认识这项权限，请谨慎确认"),
        }
    }

    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        self.enablement.is_enabled(plugin_id)
    }

    pub fn refresh_catalog(&mut self) {
        self.catalog = discover_installed_plugins();
    }
}

fn discover_installed_plugins() -> Vec<InstalledPlugin> {
    let mut plugins = terminal::discover_installed_plugin_packages()
        .into_iter()
        .filter_map(Result::ok)
        .map(|package| InstalledPlugin {
            id: package.manifest().id.as_str().to_string(),
            name: package.manifest().name.clone(),
            version: package.manifest().version.clone(),
            user_installed: matches!(
                package.provenance(),
                smelt_plugin_host::PluginProvenance::UserInstalled { .. }
            ),
            load_error: None,
        })
        .collect::<Vec<_>>();
    if let Some(root) = smelt_paths::smelt_home() {
        for record in smelt_plugin_host::list_user_plugins(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if plugins.iter().any(|plugin| plugin.id == record.id) {
                continue;
            }
            plugins.push(InstalledPlugin {
                id: record.id,
                name: record.name,
                version: record.version,
                user_installed: true,
                load_error: record.error,
            });
        }
    }
    plugins.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    plugins
}

/// 守护里各插件的运行状态快照。设置页只渲染它，不自己去连守护。
#[derive(Clone, Default)]
pub struct PluginRuntimeStatuses {
    pub by_id: std::collections::HashMap<String, smelt_plugin_host::PluginStatus>,
    /// 查询本身失败时的原因（守护没跑、连不上等）。
    pub error: Option<String>,
}

impl Global for PluginRuntimeStatuses {}

/// 带节流的状态刷新：设置页每帧都会重建，不能每次都去连守护。
pub fn refresh_plugin_statuses_throttled(cx: &mut App) {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    const INTERVAL: Duration = Duration::from_secs(2);
    thread_local! {
        static LAST: Cell<Option<Instant>> = const { Cell::new(None) };
    }
    let now = Instant::now();
    let due = LAST.with(|last| match last.get() {
        Some(previous) if now.duration_since(previous) < INTERVAL => false,
        _ => {
            last.set(Some(now));
            true
        }
    });
    if due {
        refresh_plugin_statuses(cx);
    }
}

/// 到守护拉一次插件状态。跨进程 IO，放后台执行器，回主线程写 global。
pub fn refresh_plugin_statuses(cx: &mut App) {
    cx.spawn(async move |cx| {
        let outcome = cx
            .background_spawn(async move { terminal::plugin_statuses() })
            .await;
        cx.update(|cx| {
            let snapshot = match outcome {
                Ok(statuses) => PluginRuntimeStatuses {
                    by_id: statuses
                        .into_iter()
                        .map(|status| (status.plugin_id.as_str().to_string(), status))
                        .collect(),
                    error: None,
                },
                Err(error) => PluginRuntimeStatuses {
                    by_id: Default::default(),
                    error: Some(error),
                },
            };
            cx.set_global(snapshot);
            cx.refresh_windows();
        });
    })
    .detach();
}

/// 把插件状态说成一句人话，附上失败原因。
///
/// 光说"未就绪"没有价值——插件起不来的原因（签名不匹配、进程崩溃、被停用）
/// 本来就在守护手里，不摊开的话用户只能干瞪眼。
pub fn describe_plugin_status(status: Option<&smelt_plugin_host::PluginStatus>) -> String {
    use smelt_plugin_host::PluginLifecycleState as State;
    let Some(status) = status else {
        return "未在守护中登记（守护可能没运行，或插件集尚未同步）".into();
    };
    let base = match &status.state {
        State::Ready { pid } => format!("运行中（pid {pid}）"),
        State::Discovered => "已发现，尚未启动".into(),
        State::Starting => "正在启动".into(),
        State::Backoff { attempt } => format!("启动失败，正在重试（第 {attempt} 次）"),
        State::Failed => "启动失败".into(),
        State::Stopped => "已停止".into(),
        State::Disabled => "已停用".into(),
    };
    match &status.last_error {
        Some(error) if !matches!(status.state, State::Ready { .. }) => format!("{base}：{error}"),
        _ => base,
    }
}

/// 插件在侧栏设置菜单中声明的账户快捷操作。
#[derive(Clone)]
pub(crate) struct SidebarAccountMenuEntry {
    pub plugin_id: String,
    pub settings_section_id: String,
    pub settings_title: String,
    pub actions: Vec<smelt_plugin_api::SettingsActionView>,
}

pub(crate) fn sidebar_account_menu_entries(cx: &App) -> Vec<SidebarAccountMenuEntry> {
    crate::plugin_ui::refresh_presentations(cx);
    crate::plugin_ui::sidebar_account_menu_presentations(cx)
        .into_iter()
        .map(|account| SidebarAccountMenuEntry {
            plugin_id: account.plugin_id,
            settings_section_id: account.settings_section_id,
            settings_title: account.settings_title,
            actions: account.actions,
        })
        .collect()
}

pub fn apply_plugin_enabled(plugin_id: &str, enabled: bool, cx: &mut App) {
    let mut state = cx
        .try_global::<PluginEnablementState>()
        .cloned()
        .unwrap_or_else(PluginEnablementState::load);
    state.enablement.set_enabled(plugin_id, enabled);
    if let Err(error) = state.enablement.save() {
        eprintln!("[plugins] 保存启用状态失败: {error}");
    }
    cx.set_global(state);
    // 停用的插件必须立刻从 tab 栏消失，它的 WebView 也要一并收掉。
    crate::plugin_ui::refresh(cx);
    // 守护那边起停进程需要一点时间，这里先清掉旧快照，避免设置页继续显示
    // 上一轮的状态；下一次节流刷新会拉到新的。
    cx.set_global(PluginRuntimeStatuses::default());

    let plugin_id = plugin_id.to_string();
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = terminal::plugin_set_enabled(&plugin_id, enabled) {
                eprintln!("[plugins] 同步守护启用状态失败: {error}");
            }
        })
        .detach();
    cx.refresh_windows();
}

#[cfg(test)]
mod plugin_status_tests {
    use super::describe_plugin_status;
    use smelt_plugin_host::{PluginLifecycleState as State, PluginStatus};

    fn status(state: State, last_error: Option<&str>) -> PluginStatus {
        PluginStatus {
            plugin_id: smelt_plugin_api::PluginId::new("com.example").unwrap(),
            name: "Example".into(),
            version: "1.0.0".into(),
            state,
            last_error: last_error.map(str::to_owned),
        }
    }

    #[test]
    fn a_running_plugin_reports_its_pid() {
        let text = describe_plugin_status(Some(&status(State::Ready { pid: 42 }, None)));
        assert!(text.contains("运行中") && text.contains("42"));
    }

    #[test]
    fn a_failed_plugin_carries_the_reason() {
        // 光说"启动失败"没有价值——原因就在守护手里，必须摊开。
        let text = describe_plugin_status(Some(&status(State::Failed, Some("signature mismatch"))));
        assert!(text.contains("启动失败"));
        assert!(text.contains("signature mismatch"));
    }

    #[test]
    fn a_running_plugin_does_not_show_a_stale_error() {
        // last_error 是"上一次"的失败；已经跑起来了就不该再显示它。
        let text =
            describe_plugin_status(Some(&status(State::Ready { pid: 7 }, Some("上一轮的崩溃"))));
        assert!(!text.contains("上一轮的崩溃"));
    }

    #[test]
    fn an_unregistered_plugin_points_at_the_daemon() {
        // 守护没跑、或插件集没同步时，问题不在插件本身，得说清楚。
        let text = describe_plugin_status(None);
        assert!(text.contains("守护"));
    }
}
