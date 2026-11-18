//! `smelt plugin check <目录>`：不开 App，把插件加载一次、渲染一帧，过了退出 0，没过退出 1。
//!
//! 和 App 加载插件走同一套（同一个运行时、同一份 Policy 和宿主模块），只是宿主换成离线的
//! [`CheckHost`]：没有机器、没有会话、窗口不显示。查得出的：清单、主题文件、语法错误、
//! 解析不到的导入、默认导出不对、`init` 和第一次 `render` 里抛的错、样式方法和参数写错、
//! 元素用了两次、登记的视图入口不在。查不出的：布局和绘制、之后的交互和异步状态、要连上
//! 运行时才有的数据，这些还得在 App 里看（`smelt plugin status`、`~/.smelt/app.log`）。
//!
//! 顺带把给编辑器的 `gpui-kit.d.ts`、`jsconfig.json` 写进插件目录，新插件第一次 check 完
//! 就有接口声明可查。

use std::path::Path;
use std::sync::Arc;

use gpui::*;
use gpui_shell::plugin::PluginManifest;
use gpui_shell::policy;
use rust_i18n::t;
use smelt_plugin_api::js::JsPlugin;
use smelt_plugin_api::{self as api, Machine, RuntimeKey};
use smelt_runtime_client::RuntimeClient;

use super::PluginSource;

/// 检查 `dir` 里的插件，返回退出码。读清单在开 GPUI 之前做，清单坏了不用起应用。
pub(super) fn run(dir: &Path) -> i32 {
    let root = match std::fs::canonicalize(dir) {
        Ok(root) => root,
        Err(error) => {
            return fail(&t!(
                "plugins.cli_no_dir",
                path = dir.display(),
                error = error
            ));
        }
    };
    let manifest = match PluginManifest::read(&root) {
        Ok(manifest) => manifest,
        Err(error) => return fail(&t!("plugins.bad_manifest", error = error)),
    };
    if !root.join(manifest.entry()).is_file() {
        return fail(&t!("plugins.no_entry", entry = manifest.entry()));
    }
    super::log::install_stderr();
    // 结果在应用循环里报、在里面退出：开过窗口的循环不会因为窗口没显示就自己结束。
    gpui_platform::application()
        .with_assets(smelt_ui::assets::Assets)
        .run(move |cx| {
            let code = match check(&root, &manifest, cx) {
                Ok(()) => {
                    let report = serde_json::json!({
                        "ok": true,
                        "id": manifest.id(),
                        "name": manifest.name(),
                        "path": root,
                    });
                    println!("{report:#}");
                    0
                }
                Err(error) => fail(&error),
            };
            // 脚本运行时存在线程局部的注册表里；留到进程退出时再析构，它要用的别的线程局部
            // 可能已经没了。和 App 退出一样先放掉。
            super::shutdown(cx);
            std::process::exit(code);
        });
    1
}

fn fail(message: &str) -> i32 {
    eprintln!("{message}");
    1
}

fn check(root: &Path, manifest: &PluginManifest, cx: &mut App) -> Result<(), String> {
    gpui_base::init(cx);
    smelt_ui::init(cx);
    api::set_locale(api::system_locale(), cx);
    api::install(CheckHost, cx);

    let id = manifest.id();
    let themes = super::themes::read(root, manifest.entry())?;
    if !themes.is_empty() {
        let sources: Vec<&str> = themes.iter().map(String::as_str).collect();
        smelt_ui::theme_catalog::register_theme_sets(&format!("files:{id}"), &sources, cx)
            .map_err(|error| error.to_string())?;
    }
    if super::is_theme_only(manifest) {
        return Ok(());
    }

    let runtime = super::runtime(cx)
        .map_err(|error| t!("plugins.runtime_failed", error = format!("{error:#}")).into_owned())?;
    runtime.set_editor_files(true);
    let plugin = JsPlugin::for_plugin(id, manifest.name()).with_view_loader(super::view_loader(
        id,
        manifest.name(),
        root,
        PluginSource::User,
    ));
    let policy = super::plugin_policy(id, &plugin).map_err(|error| format!("{error:#}"))?;
    // 加载时解析导入、挂载时视图拿走的都是默认 Policy，同 `mount_one`。
    policy::set_default(policy);
    let options = WindowOptions {
        show: false,
        focus: false,
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(480.), px(360.)),
            cx,
        ))),
        ..Default::default()
    };
    let window = cx
        .open_window(options, |_, cx| cx.new(|_| Empty))
        .map_err(|error| format!("{error:#}"))?;
    window
        .update(cx, |_, window, cx| runtime.check(root, window, cx))
        .map_err(|error| format!("{error:#}"))?
        .map(|_| ())
        .map_err(|error| format!("{error:#}"))
}

/// 离线的宿主：只有本机、不在线、什么会话都没有、视图都没露着。插件在 `init` 和第一次
/// `render` 里读到的就是这些，读得出来就算过；要连运行时的调用照常报错。
struct CheckHost;

impl api::Host for CheckHost {
    fn machines(&self, _cx: &App) -> Vec<Machine> {
        vec![Machine {
            key: RuntimeKey::Local,
            name: String::new(),
            unavailable: None,
        }]
    }

    fn client(&self, _machine: &RuntimeKey, _cx: &App) -> Option<RuntimeClient> {
        None
    }

    fn connector(
        &self,
        _machine: &RuntimeKey,
        _cx: &App,
    ) -> Option<Arc<dyn api::client::Connector>> {
        None
    }

    fn machine_online(&self, _machine: &RuntimeKey, _cx: &App) -> bool {
        false
    }

    fn connections(&self, _cx: &App) -> api::Connections {
        api::Connections::default()
    }

    fn view_shown(&self, _id: &str, _cx: &App) -> bool {
        false
    }

    fn view_laid_out(&self, _id: &str, _cx: &App) -> bool {
        false
    }

    fn main_window(&self, _cx: &App) -> Option<AnyWindowHandle> {
        None
    }

    fn window_active(&self, _cx: &App) -> bool {
        false
    }

    fn overlay_open(&self, _cx: &App) -> bool {
        false
    }

    fn path_style(&self, _machine: &RuntimeKey, _cx: &App) -> api::model::runtime_path::PathStyle {
        api::model::runtime_path::PathStyle::LOCAL
    }

    fn session_states(
        &self,
        _machine: &RuntimeKey,
        _cx: &App,
    ) -> Vec<api::model::daemon_state::DaemonSessionState> {
        Vec::new()
    }

    fn session_presence(
        &self,
        _machine: &RuntimeKey,
        _session_id: &str,
        _cx: &App,
    ) -> Option<api::model::daemon_state::RuntimePresence> {
        None
    }

    fn workspace_directory(
        &self,
        _machine: &RuntimeKey,
        _cx: &App,
    ) -> Option<api::model::workspace_directory::WorkspaceDirectory> {
        None
    }

    fn notification_permission(&self, _cx: &App) -> api::NotificationPermission {
        api::NotificationPermission::Unsupported
    }
}
