//! `fs.*` 控制方法：通用文件操作和目录占用统计。业务规则在 `smelt_core::fs_ops`、
//! `smelt_core::disk_usage`，这里只做解码、错误归类，以及管着在跑和扫完的统计。
//!
//! 扫完的结果留在内存里，直到调用方 `fs.usage.release` 或运行时退出。一棵几十万个文件的
//! 树要上百 MB，所以同时留着的有上限，满了就拒绝新的，不悄悄丢掉别人正在看的。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use smelt_core::disk_usage::{self, ScanControl, UsageTree};
use smelt_core::fs_ops::{self, FsError};
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, EmptyParams, decode_params,
};
use smelt_runtime_client::protocol::fs::*;

use crate::event_hub::EventHubHandle;

/// 同时留着的统计（在扫的加扫完的）最多这么多。
const MAX_SCANS: usize = 4;

struct Scan {
    control: Arc<ScanControl>,
    /// 扫完才有。
    tree: Mutex<Option<UsageTree>>,
}

fn scans() -> &'static Mutex<HashMap<String, Arc<Scan>>> {
    static SCANS: OnceLock<Mutex<HashMap<String, Arc<Scan>>>> = OnceLock::new();
    SCANS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `method` 必须是 `fs.*` 之一。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
    event_hub: &EventHubHandle,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::FsStat => {
            call::<FsStatCall>(params, |p| fs_ops::stat(&p.path).map_err(fs_error))
        }
        ControlMethod::FsReadDir => call::<FsReadDirCall>(params, |p| {
            Ok(FsReadDirResult {
                entries: fs_ops::read_dir(&p.path).map_err(fs_error)?,
            })
        }),
        ControlMethod::FsDelete => call::<FsDeleteCall>(params, |p| {
            fs_ops::delete(&p).map_err(fs_error)?;
            Ok(EmptyParams::default())
        }),
        ControlMethod::FsVolume => {
            call::<FsVolumeCall>(params, |p| fs_ops::volume(&p.path).map_err(fs_error))
        }
        ControlMethod::FsUsageScan => {
            call::<FsUsageScanCall>(params, |p| start_scan(p.path, event_hub.clone()))
        }
        ControlMethod::FsUsageChildren => call::<FsUsageChildrenCall>(params, children),
        ControlMethod::FsUsageRelease => call::<FsUsageReleaseCall>(params, |p| {
            let removed = scans().lock().expect("scans lock poisoned").remove(&p.scan);
            if let Some(scan) = removed {
                scan.control.cancel();
            }
            Ok(EmptyParams::default())
        }),
        other => Err(ControlError::new(
            ControlErrorCode::Internal,
            format!("`{other}` is not an fs method"),
        )),
    }
}

fn start_scan(root: String, event_hub: EventHubHandle) -> Result<FsUsageScanResult, ControlError> {
    // 根不存在、不是绝对路径的当场报错，不等扫描线程。
    fs_ops::stat(&root).map_err(fs_error)?;
    let id = uuid::Uuid::new_v4().to_string();
    let scan = Arc::new(Scan {
        control: Arc::new(ScanControl::default()),
        tree: Mutex::new(None),
    });
    {
        let mut scans = scans().lock().expect("scans lock poisoned");
        if scans.len() >= MAX_SCANS {
            return Err(ControlError::new(
                ControlErrorCode::Busy,
                format!(
                    "{MAX_SCANS} disk usage scans are already held; release one with fs.usage.release first"
                ),
            ));
        }
        scans.insert(id.clone(), scan.clone());
    }
    let scan_id = id.clone();
    let spawned = std::thread::Builder::new()
        .name("smeltd-disk-usage".into())
        .spawn(move || {
            let report = |progress: FsUsageProgress, done: bool| {
                if let Err(error) = event_hub.publish_fs_usage_progress(
                    &scan_id,
                    progress.files,
                    progress.bytes,
                    done,
                ) {
                    eprintln!("[fs-usage] 发进度失败：{error}");
                }
            };
            let result =
                disk_usage::scan(&root, &scan.control, &|progress| report(progress, false));
            match result {
                Ok(Some(tree)) => *scan.tree.lock().expect("scan tree lock poisoned") = Some(tree),
                Ok(None) => {}
                Err(error) => {
                    // 开始前查过根，这里多半是扫的过程中根被删了。扫描当作结束，结果为空。
                    eprintln!("[fs-usage] 扫 {root} 失败：{error}");
                    scans()
                        .lock()
                        .expect("scans lock poisoned")
                        .remove(&scan_id);
                }
            }
            report(scan.control.progress(), true);
        });
    if let Err(error) = spawned {
        scans().lock().expect("scans lock poisoned").remove(&id);
        return Err(ControlError::new(
            ControlErrorCode::OperationFailed,
            format!("couldn't start the scan: {error}"),
        ));
    }
    Ok(FsUsageScanResult { scan: id })
}

fn children(params: FsUsageChildrenParams) -> Result<FsUsageChildrenResult, ControlError> {
    let scan = scans()
        .lock()
        .expect("scans lock poisoned")
        .get(&params.scan)
        .cloned()
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("no disk usage scan {}", params.scan),
            )
        })?;
    let progress = scan.control.progress();
    let tree = scan.tree.lock().expect("scan tree lock poisoned");
    let Some(tree) = tree.as_ref() else {
        return Ok(FsUsageChildrenResult {
            done: false,
            progress,
            entry: None,
            children: Vec::new(),
        });
    };
    let (entry, children) = tree.children(&params.path).ok_or_else(|| {
        ControlError::new(
            ControlErrorCode::NotFound,
            format!("{} isn't in this scan", params.path),
        )
    })?;
    Ok(FsUsageChildrenResult {
        done: true,
        progress,
        entry: Some(entry),
        children,
    })
}

fn fs_error(error: FsError) -> ControlError {
    let code = match error {
        FsError::InvalidPath(_) => ControlErrorCode::InvalidParams,
        FsError::NotFound(_) => ControlErrorCode::NotFound,
        FsError::PermissionDenied(_) => ControlErrorCode::PermissionDenied,
        FsError::Changed(_) => ControlErrorCode::Conflict,
        FsError::Io(_) => ControlErrorCode::OperationFailed,
    };
    ControlError::new(code, error.to_string())
}

fn call<C: ControlCall>(
    params: serde_json::Value,
    run: impl FnOnce(C::Params) -> Result<C::Result, ControlError>,
) -> Result<serde_json::Value, ControlError> {
    let result = run(decode_params::<C::Params>(params)?)?;
    serde_json::to_value(result)
        .map_err(|error| ControlError::new(ControlErrorCode::Internal, error.to_string()))
}
