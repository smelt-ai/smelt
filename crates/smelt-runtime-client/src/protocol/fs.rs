//! 通用文件系统方法 `fs.*`：在运行时那台机器上看文件、删文件、查卷容量、统计目录占用。
//!
//! 路径一律是那台机器上的绝对路径，运行时不按项目限范围：调用方（插件、手机）本来就可信，
//! `file.open` / `file.save` 也是按任意路径读写。列目录不排序、不滤掉任何东西，怎么排、
//! 藏哪些是调用方自己的规则。读不了就报错，不当成空目录或 0 字节。
//!
//! 目录占用统计要遍历整棵树，一次可能几十万个文件，所以由运行时扫一遍、把各目录汇总好的
//! 大小留在内存里（只留目录，文件取那一层时当场列），调用方按层取（`fs.usage.children`），
//! 扫的过程中运行时发 `fs.usage.progress`。
//! 大小口径见 [`FsUsageEntry`]。

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod, EmptyParams};
use super::files::FilePathParams;

macro_rules! fs_calls {
    ($( $(#[$doc:meta])* $call:ident => $method:ident ($params:ty) -> $result:ty; )+) => {
        $(
            $(#[$doc])*
            pub enum $call {}

            impl ControlCall for $call {
                type Params = $params;
                type Result = $result;

                const METHOD: ControlMethod = ControlMethod::$method;
            }
        )+
    };
}

fs_calls! {
    /// 一个路径本身的信息，不跟随符号链接。不存在时报 `not_found`。
    FsStatCall => FsStat(FilePathParams) -> FsStat;
    /// 目录的直接子项，原样给出：不排序、不过滤。读不了目录就报错。
    FsReadDirCall => FsReadDir(FilePathParams) -> FsReadDirResult;
    /// 删一个文件或目录（目录连同内容；符号链接只删链接本身）。`trash` 为真时移进系统
    /// 废纸篓，否则永久删除。
    FsDeleteCall => FsDelete(FsDeleteParams) -> EmptyParams;
    /// 路径所在卷的容量。
    FsVolumeCall => FsVolume(FilePathParams) -> FsVolume;
    /// 开始统计一个目录的占用，马上返回扫描 id；进度发 `fs.usage.progress`。结果留在运行时
    /// 内存里，用完调 `fs.usage.release`。
    FsUsageScanCall => FsUsageScan(FilePathParams) -> FsUsageScanResult;
    /// 扫描结果里一个目录（或扫描根）和它的直接子项。没扫完时只给进度。子目录的大小是
    /// 扫描时的；文件当场列，是现在的。扫完后删掉的不再列出，但上层总数不变。
    FsUsageChildrenCall => FsUsageChildren(FsUsageChildrenParams) -> FsUsageChildrenResult;
    /// 没扫完就停下，并丢掉结果。扫描 id 不认识也算成功。
    FsUsageReleaseCall => FsUsageRelease(FsUsageScanParams) -> EmptyParams;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FsKind {
    File,
    Dir,
    Symlink,
    /// 设备、管道、套接字之类。
    Other,
}

/// 一个路径本身（不跟随符号链接）的信息。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsStat {
    pub kind: FsKind,
    /// 符号链接指向的是什么。不是链接、或者链接指向的东西不存在时没有。
    pub target_kind: Option<FsKind>,
    /// 名义长度，字节。
    pub size: u64,
    /// 实际占的磁盘空间，字节。稀疏文件、压缩文件会比 `size` 小。拿不到时（Windows）等于 `size`。
    pub allocated: u64,
    /// 修改时间，Unix 毫秒。
    pub modified: Option<i64>,
    /// 所在设备和 inode，合起来认出「是不是同一个东西」，删除前核对用（见 [`FsIdentity`]）。
    /// 只有 Unix 有。
    pub identity: Option<FsIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsIdentity {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsDirEntry {
    pub name: String,
    #[serde(flatten)]
    pub stat: FsStat,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsReadDirResult {
    /// 列的过程中消失的子项不在里面。
    pub entries: Vec<FsDirEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsDeleteParams {
    pub path: String,
    /// 移进系统废纸篓（可以找回），不永久删除。
    #[serde(default)]
    pub trash: bool,
    /// 给了就先核对：路径现在指向的必须还是这个东西，不是就报 `conflict`、什么都不删。
    /// 防止看到列表之后、确认删除之前，同一个路径换成了别的东西。
    #[serde(default)]
    pub expect: Option<FsIdentity>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsVolume {
    /// 卷的挂载点。
    pub mount: String,
    /// 卷名。
    pub name: String,
    /// 文件系统类型，如 `apfs`、`ext4`。
    pub file_system: String,
    /// 总容量，字节。
    pub total: u64,
    /// 可用空间，字节。macOS 上和访达一致，含系统需要时能清掉的空间（可清除空间）。
    pub available: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsUsageScanResult {
    pub scan: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsUsageScanParams {
    pub scan: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsUsageChildrenParams {
    pub scan: String,
    /// 扫描根或它下面的一个目录，绝对路径。
    pub path: String,
}

/// 扫描结果里的一项。大小口径：
///
/// - 按实际占的块算（同 [`FsStat::allocated`]），目录自己占的块也算进去。
/// - 硬链接、经不同路径到达的同一个目录（macOS 的 firmlink）只算第一次遇到的那处。
/// - 不跟随符号链接，链接只算它自己。
/// - 不进别的卷。macOS 上系统卷和数据卷算同一个。
/// - APFS 克隆文件共享的块查不出来，各算各的，所以总数可能比实际占用大。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsUsageEntry {
    pub name: String,
    pub kind: FsKind,
    /// 这一项连同下面所有东西占的空间，字节。
    pub size: u64,
    /// 这一项连同下面的文件数（不含目录）。
    pub files: u64,
    /// 这一项下面读不了（没权限等）的目录数。不为 0 时 `size` 偏小。
    pub unreadable: u64,
    pub modified: Option<i64>,
    pub identity: Option<FsIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FsUsageChildrenResult {
    /// 扫完了。没扫完时 `entry`、`children` 都是空的，只看进度。
    pub done: bool,
    /// 到目前为止数过的文件数和字节数（整次扫描）。
    pub progress: FsUsageProgress,
    /// 请求的那个目录本身。
    pub entry: Option<FsUsageEntry>,
    /// 它的直接子项，按 `size` 从大到小。
    pub children: Vec<FsUsageEntry>,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct FsUsageProgress {
    pub files: u64,
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_entry_flattens_stat_fields() {
        let entry = FsDirEntry {
            name: "a.txt".into(),
            stat: FsStat {
                kind: FsKind::File,
                target_kind: None,
                size: 3,
                allocated: 4096,
                modified: Some(1),
                identity: Some(FsIdentity { dev: 1, ino: 2 }),
            },
        };
        let wire = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "name": "a.txt",
                "kind": "file",
                "target_kind": null,
                "size": 3,
                "allocated": 4096,
                "modified": 1,
                "identity": { "dev": 1, "ino": 2 },
            })
        );
        assert_eq!(serde_json::from_value::<FsDirEntry>(wire).unwrap(), entry);
    }

    #[test]
    fn delete_defaults_to_permanent_without_identity_check() {
        let params: FsDeleteParams =
            serde_json::from_value(serde_json::json!({ "path": "/a" })).unwrap();
        assert!(!params.trash);
        assert_eq!(params.expect, None);
    }
}
