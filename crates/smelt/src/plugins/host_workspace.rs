//! `smelt-workspace`：smelt 注册给全部插件的工作台 HostModule。
//!
//! 插件读得到主目录和工作台当前项目，能请宿主弹系统的目录选择框。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_shell::{HostError, HostModule, HostObject, HostResult, HostValue};

use super::host_fs::path_value;

/// 宿主上下文：插件读得到的工作台状态。由工作台在绘制时写入（纯内存），
/// 插件经 `smelt-workspace.context()` 读取。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct HostContext {
    pub(super) project_root: Option<PathBuf>,
    /// 每次 `project_root` 变化加一。插件拿它判断要不要重扫，不必比较路径。
    pub(super) generation: u64,
}

const WORKSPACE_DECLARATIONS: &str = r#"
export interface HostContext { home: string; project_root: string | null; generation: number }
/** 主目录和工作台当前项目。generation 在项目变化时加一。 */
export function context(): HostContext;
/** 弹出系统的目录选择框。取消时为 null。 */
export function pick_directory(prompt?: string): Promise<string | null>;
"#;

/// 在访达里打开用官方的 `cx.open_path`，不在这里重复提供。
pub(super) fn workspace_module(home: PathBuf, context: Rc<RefCell<HostContext>>) -> HostModule {
    HostModule::new("smelt-workspace")
        .declarations(WORKSPACE_DECLARATIONS)
        .function("context", move |_| {
            Ok(context_value(&home, &context.borrow()))
        })
        .async_function("pick_directory", |arguments| {
            let prompt = match arguments.get(0).filter(|value| !value.is_null()) {
                Some(_) => Some(arguments.string(0)?.to_owned()),
                None => None,
            };
            pick_directory(prompt)
        })
}

fn context_value(home: &Path, context: &HostContext) -> HostValue {
    HostObject::new()
        .field("home", home.to_string_lossy().into_owned())
        .field(
            "project_root",
            context
                .project_root
                .as_ref()
                .map(|root| root.to_string_lossy().into_owned()),
        )
        .field("generation", context.generation)
        .into()
}

/// 选目录的同步半截在主线程上打开系统选择框；等用户选完是在后台。
/// 选中什么都只是一个路径，不额外授权：主目录外的目录照样读不了。
fn pick_directory(
    prompt: Option<String>,
) -> Result<impl std::future::Future<Output = HostResult> + Send + 'static, HostError> {
    let receiver = gpui_shell::with_current_app(|cx| {
        cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: prompt.map(Into::into),
        })
    })
    .ok_or_else(|| HostError::new("选目录只能在事件或任务里调用"))?;
    Ok(async move {
        match receiver.await {
            Ok(Ok(Some(paths))) => Ok(paths
                .into_iter()
                .next()
                .map_or(HostValue::Null, |path| path_value(&path))),
            Ok(Ok(None)) | Err(_) => Ok(HostValue::Null),
            Ok(Err(error)) => Err(HostError::new(format!("无法打开目录选择框：{error}"))),
        }
    })
}
