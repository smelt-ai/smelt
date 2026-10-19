//! `project.*`、`file.*` 控制方法：文件树、查看器、项目搜索都由运行时在它那台机器上
//! 执行。业务规则在 `smelt_core::project_files`，这里只做解码和错误归类。

use smelt_core::project_files::{self, ProjectFileError};
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};
use smelt_runtime_client::protocol::files::*;

/// `method` 必须是 `project.*` / `file.*` 之一。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::ProjectTree => call::<ProjectTreeCall>(params, |p| {
            Ok(ProjectTreeResult {
                entries: project_files::tree(&p.root, &p.dir)?,
            })
        }),
        ControlMethod::ProjectSearch => {
            call::<ProjectSearchCall>(params, |p| Ok(project_files::search(&p.root, &p.query)))
        }
        ControlMethod::ProjectDeleteEntry => call::<ProjectDeleteEntryCall>(params, |p| {
            project_files::delete_entry(&p.root, &p.path)?;
            Ok(EmptyParams::default())
        }),
        ControlMethod::FileOpen => call::<FileOpenCall>(params, |p| {
            Ok(FileOpenResult {
                content: project_files::open(&p.path)?,
            })
        }),
        ControlMethod::FileSave => call::<FileSaveCall>(params, |p| {
            Ok(FileSaveResult {
                version: project_files::save(&p.path, &p.text, p.base_version.as_deref())?,
            })
        }),
        ControlMethod::SessionCompleteFiles => {
            call::<SessionCompleteFilesCall>(params, |p| Ok(project_files::complete_files(&p.cwd)))
        }
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not a file method"),
        )),
    }
}

fn call<C: ControlCall>(
    params: serde_json::Value,
    run: impl FnOnce(C::Params) -> Result<C::Result, ProjectFileError>,
) -> Result<serde_json::Value, ControlError> {
    let params = decode_params::<C::Params>(params)?;
    let result = run(params).map_err(|error| {
        let code = match error {
            ProjectFileError::OutsideProject(_) => ControlErrorCode::InvalidParams,
            ProjectFileError::Conflict => ControlErrorCode::Conflict,
            ProjectFileError::Io(_) => ControlErrorCode::OperationFailed,
        };
        ControlError::new(code, error.to_string())
    })?;
    serde_json::to_value(result)
        .map_err(|error| ControlError::new(ControlErrorCode::Internal, error.to_string()))
}
