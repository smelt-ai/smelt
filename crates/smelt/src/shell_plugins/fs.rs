//! `smelt-fs` / `smelt-workspace`：smelt 借给 gpui-shell 插件的两个 HostModule。
//!
//! gpui-shell 自带的 `fs` 只有 readFile/writeFile/readdir/exists/unlink/rmdir/mkdir，
//! 没有 lstat、readlink、symlink、rename——而「技能」这类插件的核心正是软链和原子
//! 移动。它的路径校验器是 crate 私有的，manifest 里的授权根也写不了 `~`，所以这里
//! 自带一套门控，授权来自插件目录里的 `smelt-grant.json`：
//!
//! ```json
//! { "fs": { "read": ["~/.claude"], "write": ["~/.agents/skills", "${project}/.agents/skills"] } }
//! ```
//!
//! 只认 `~/` 与 `${project}/` 两种前缀：插件不能点名任意绝对路径。`${project}` 在
//! 每次调用时按宿主当前项目展开，没有项目时这些根不存在。
//!
//! 门控规则：
//! - 路径必须是绝对路径，且不含 `..`。
//! - 「操作链接本身」的调用（lstat、readlink、symlink 的落点、rename、unlink）只
//!   规范化父目录，最后一段不跟随——否则无法区分本体与引用。
//! - 「读内容」的调用（stat、read_text、realpath、list）跟随软链，真实路径也必须
//!   落在授权根内：指向根外的软链读不穿。
//! - 规范化途中遇到失效软链一律拒绝，不把它当作「还不存在的目录」顺着建出去。
//!
//! 原型阶段的已知缺口：检查与操作之间没有用 openat 一类的手段消除竞态。

use std::cell::RefCell;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::UNIX_EPOCH;

use gpui_shell::{HostArguments, HostError, HostModule, HostObject, HostResult, HostValue};

/// 插件目录里声明 smelt 专属授权的文件。gpui-shell 的 manifest 拒绝未知字段，
/// 所以不能写进 `gpui-shell.json`。
pub(crate) const GRANT_FILE: &str = "smelt-grant.json";

const HOME_PREFIX: &str = "~/";
const PROJECT_PREFIX: &str = "${project}/";
/// `read_text` 的上限：读的是 SKILL.md 这类说明文件，不是任意大文件。
const MAX_TEXT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Read,
    Write,
}

/// `smelt-grant.json` 解析后的授权：原样保留模式串，调用时再展开。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FsGrant {
    read: Vec<String>,
    write: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantFile {
    #[serde(default)]
    fs: GrantFsFile,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantFsFile {
    #[serde(default)]
    read: Vec<String>,
    #[serde(default)]
    write: Vec<String>,
}

impl FsGrant {
    /// 读插件目录里的授权文件。没有这个文件 = 什么都不授予。
    pub(crate) fn read(plugin_dir: &Path) -> Result<Self, String> {
        let path = plugin_dir.join(GRANT_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("读取 {} 失败：{error}", path.display())),
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        let file: GrantFile = serde_json::from_str(text)
            .map_err(|error| format!("{GRANT_FILE} 格式错误：{error}"))?;
        for pattern in file.fs.read.iter().chain(&file.fs.write) {
            validate_pattern(pattern)?;
        }
        Ok(Self {
            read: file.fs.read,
            write: file.fs.write,
        })
    }
}

fn validate_pattern(pattern: &str) -> Result<(), String> {
    let rest = pattern
        .strip_prefix(HOME_PREFIX)
        .or_else(|| pattern.strip_prefix(PROJECT_PREFIX))
        .ok_or_else(|| format!("授权根 `{pattern}` 必须以 `~/` 或 `${{project}}/` 开头"))?;
    let relative = Path::new(rest);
    let valid = !rest.is_empty()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if valid {
        Ok(())
    } else {
        Err(format!("授权根 `{pattern}` 只能是不含 `.`、`..` 的相对段"))
    }
}

/// 宿主上下文：插件读得到的工作台状态。由工作台在绘制时写入（纯内存），
/// 插件经 `smelt-workspace.context()` 读取，`${project}` 授权根也按它展开。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostContext {
    pub(crate) project_root: Option<PathBuf>,
    /// 每次 `project_root` 变化加一。插件拿它判断要不要重扫，不必比较路径。
    pub(crate) generation: u64,
}

/// 一个插件的文件门控。每个插件一份，授权互不渗透。
pub(crate) struct HostFs {
    grant: FsGrant,
    home: PathBuf,
    context: Rc<RefCell<HostContext>>,
}

impl HostFs {
    pub(crate) fn new(grant: FsGrant, home: PathBuf, context: Rc<RefCell<HostContext>>) -> Self {
        Self {
            grant,
            home,
            context,
        }
    }

    /// 展开并规范化授权根。根本身不存在没关系（比如还没建的 `~/.agents/skills`），
    /// 规范化到最深的已存在祖先为止。
    fn roots(&self, access: Access) -> Vec<PathBuf> {
        let project = self.context.borrow().project_root.clone();
        let patterns: Vec<&String> = match access {
            // 可写的地方当然可读。
            Access::Read => self.grant.read.iter().chain(&self.grant.write).collect(),
            Access::Write => self.grant.write.iter().collect(),
        };
        patterns
            .into_iter()
            .filter_map(|pattern| {
                let expanded = if let Some(rest) = pattern.strip_prefix(HOME_PREFIX) {
                    self.home.join(rest)
                } else {
                    project
                        .as_ref()?
                        .join(pattern.strip_prefix(PROJECT_PREFIX)?)
                };
                canonical_prefix(&expanded).ok()
            })
            .collect()
    }

    fn check(&self, resolved: &Path, access: Access) -> Result<(), HostError> {
        if self
            .roots(access)
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            Ok(())
        } else {
            let verb = match access {
                Access::Read => "读取",
                Access::Write => "写入",
            };
            Err(HostError::new(format!(
                "未授权{verb} {}（见插件的 {GRANT_FILE}）",
                resolved.display()
            )))
        }
    }

    /// 操作链接本身：父目录规范化，最后一段原样保留。
    fn nofollow(&self, raw: &str, access: Access) -> Result<PathBuf, HostError> {
        let path = lexical(raw)?;
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(HostError::new(format!("不能操作根目录：{raw}")));
        };
        let resolved = canonical_prefix(parent)?.join(name);
        self.check(&resolved, access)?;
        Ok(resolved)
    }

    /// 读内容：完全跟随软链。目标不存在时返回 `None`，交给调用方回 null。
    fn follow(&self, raw: &str) -> Result<Option<PathBuf>, HostError> {
        // 先按「不跟随」过一遍：落点本身不在授权范围里就不该去碰它。
        let at = self.nofollow(raw, Access::Read)?;
        match fs::canonicalize(&at) {
            Ok(real) => {
                self.check(&real, Access::Read)?;
                Ok(Some(real))
            }
            Err(_) => Ok(None),
        }
    }

    fn list(&self, raw: &str) -> HostResult {
        let Some(dir) = self.follow(raw)? else {
            return Ok(HostValue::Array(Vec::new()));
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(HostValue::Array(Vec::new()));
        };
        let mut rows: Vec<(String, &'static str)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                // DirEntry::file_type 不跟随软链。
                let kind = entry.file_type().map_or("other", kind_of);
                Some((name, kind))
            })
            .collect();
        rows.sort();
        Ok(HostValue::Array(
            rows.into_iter()
                .map(|(name, kind)| {
                    HostObject::new()
                        .field("name", name)
                        .field("kind", kind)
                        .into()
                })
                .collect(),
        ))
    }

    fn lstat(&self, raw: &str) -> HostResult {
        let at = self.nofollow(raw, Access::Read)?;
        Ok(fs::symlink_metadata(&at).map_or(HostValue::Null, |meta| stat_value(&meta)))
    }

    fn stat(&self, raw: &str) -> HostResult {
        let Some(real) = self.follow(raw)? else {
            return Ok(HostValue::Null);
        };
        Ok(fs::metadata(&real).map_or(HostValue::Null, |meta| stat_value(&meta)))
    }

    fn realpath(&self, raw: &str) -> HostResult {
        Ok(self
            .follow(raw)?
            .map_or(HostValue::Null, |real| path_value(&real)))
    }

    fn readlink(&self, raw: &str) -> HostResult {
        let at = self.nofollow(raw, Access::Read)?;
        Ok(fs::read_link(&at).map_or(HostValue::Null, |target| path_value(&target)))
    }

    fn read_text(&self, raw: &str) -> HostResult {
        let Some(real) = self.follow(raw)? else {
            return Ok(HostValue::Null);
        };
        let Ok(meta) = fs::metadata(&real) else {
            return Ok(HostValue::Null);
        };
        if !meta.is_file() {
            return Ok(HostValue::Null);
        }
        if meta.len() > MAX_TEXT_BYTES {
            return Err(HostError::new(format!("{} 太大，拒绝读取", real.display())));
        }
        fs::read_to_string(&real)
            .map(HostValue::Str)
            .map_err(|error| HostError::new(format!("读取 {} 失败：{error}", real.display())))
    }

    /// 在 `at` 建一条指向 `target` 的软链。`target` 必须存在且在可读范围内：
    /// 不允许插件借软链给别的程序指一条通往授权范围外的路。
    fn symlink(&self, target: &str, at: &str) -> HostResult {
        let at = self.nofollow(at, Access::Write)?;
        let target = lexical(target)?;
        let real = fs::canonicalize(&target)
            .map_err(|_| HostError::new(format!("链接目标不存在：{}", target.display())))?;
        self.check(&real, Access::Read)?;
        std::os::unix::fs::symlink(&target, &at)
            .map(|()| HostValue::Null)
            .map_err(|error| HostError::new(format!("创建链接 {} 失败：{error}", at.display())))
    }

    fn rename(&self, from: &str, to: &str) -> HostResult {
        let from = self.nofollow(from, Access::Write)?;
        let to = self.nofollow(to, Access::Write)?;
        fs::rename(&from, &to)
            .map(|()| HostValue::Null)
            .map_err(|error| {
                HostError::new(format!(
                    "移动 {} → {} 失败：{error}",
                    from.display(),
                    to.display()
                ))
            })
    }

    /// 只删文件或软链本身。删目录请移进回收目录（rename），不提供递归删除。
    fn unlink(&self, raw: &str) -> HostResult {
        let at = self.nofollow(raw, Access::Write)?;
        let meta = fs::symlink_metadata(&at)
            .map_err(|error| HostError::new(format!("{} 不存在：{error}", at.display())))?;
        if meta.is_dir() {
            return Err(HostError::new(format!(
                "{} 是目录，unlink 只删文件或软链",
                at.display()
            )));
        }
        fs::remove_file(&at)
            .map(|()| HostValue::Null)
            .map_err(|error| HostError::new(format!("删除 {} 失败：{error}", at.display())))
    }

    fn mkdir(&self, raw: &str) -> HostResult {
        let path = canonical_prefix(&lexical(raw)?)?;
        self.check(&path, Access::Write)?;
        fs::create_dir_all(&path)
            .map(|()| HostValue::Null)
            .map_err(|error| HostError::new(format!("创建 {} 失败：{error}", path.display())))
    }

    fn context(&self) -> HostValue {
        let context = self.context.borrow();
        HostObject::new()
            .field("home", self.home.to_string_lossy().into_owned())
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
}

const FS_DECLARATIONS: &str = r#"
export type EntryKind = "dir" | "file" | "symlink" | "other";
export interface Entry { name: string; kind: EntryKind }
export interface Stat { kind: EntryKind; mtime_ms: number }
export function list(dir: string): Entry[];
export function lstat(path: string): Stat | null;
export function stat(path: string): Stat | null;
export function realpath(path: string): string | null;
export function readlink(path: string): string | null;
export function read_text(path: string): string | null;
export function symlink(target: string, at: string): null;
export function rename(from: string, to: string): null;
export function unlink(path: string): null;
export function mkdir(path: string): null;
"#;

const WORKSPACE_DECLARATIONS: &str = r#"
export interface HostContext { home: string; project_root: string | null; generation: number }
export function context(): HostContext;
export function reveal(path: string): null;
"#;

fn one(arguments: &HostArguments) -> Result<&str, HostError> {
    arguments.string(0)
}

pub(crate) fn fs_module(host: Rc<HostFs>) -> HostModule {
    let module = HostModule::new("smelt-fs").declarations(FS_DECLARATIONS);
    let register = |module: HostModule,
                    name: &str,
                    call: fn(&HostFs, &HostArguments) -> HostResult|
     -> HostModule {
        let host = host.clone();
        module.function(name, move |arguments| call(&host, arguments))
    };
    let module = register(module, "list", |host, args| host.list(one(args)?));
    let module = register(module, "lstat", |host, args| host.lstat(one(args)?));
    let module = register(module, "stat", |host, args| host.stat(one(args)?));
    let module = register(module, "realpath", |host, args| host.realpath(one(args)?));
    let module = register(module, "readlink", |host, args| host.readlink(one(args)?));
    let module = register(module, "read_text", |host, args| host.read_text(one(args)?));
    let module = register(module, "symlink", |host, args| {
        host.symlink(args.string(0)?, args.string(1)?)
    });
    let module = register(module, "rename", |host, args| {
        host.rename(args.string(0)?, args.string(1)?)
    });
    let module = register(module, "unlink", |host, args| host.unlink(one(args)?));
    register(module, "mkdir", |host, args| host.mkdir(one(args)?))
}

pub(crate) fn workspace_module(host: Rc<HostFs>) -> HostModule {
    let context_host = host.clone();
    HostModule::new("smelt-workspace")
        .declarations(WORKSPACE_DECLARATIONS)
        .function("context", move |_| Ok(context_host.context()))
        .function("reveal", move |arguments| {
            let at = host.nofollow(arguments.string(0)?, Access::Read)?;
            gpui_shell::with_current_app(|cx| cx.reveal_path(&at))
                .ok_or_else(|| HostError::new("reveal 只能在事件或任务里调用"))?;
            Ok(HostValue::Null)
        })
}

/// 绝对路径、去掉 `.`、拒绝 `..`。不碰磁盘。
fn lexical(raw: &str) -> Result<PathBuf, HostError> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(HostError::new(format!("需要绝对路径：{raw}")));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(HostError::new(format!("路径不能含 `..`：{raw}")));
    }
    Ok(path.components().collect())
}

/// 把最深的已存在祖先规范化，其余段原样拼回（它们不存在，也就没有软链可跟）。
///
/// 中途碰到失效软链（lstat 得到、canonicalize 不了）直接拒绝：否则它会被当成
/// 「还不存在的目录」，随后 mkdir 顺着它把目录建到授权范围之外。
fn canonical_prefix(path: &Path) -> Result<PathBuf, HostError> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(real) => {
                let mut out = real;
                for name in missing.iter().rev() {
                    out.push(name);
                }
                return Ok(out);
            }
            Err(_) if fs::symlink_metadata(existing).is_ok() => {
                return Err(HostError::new(format!(
                    "路径经过失效的软链：{}",
                    existing.display()
                )));
            }
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    missing.push(name.to_owned());
                    existing = parent;
                }
                _ => return Ok(path.to_path_buf()),
            },
        }
    }
}

fn kind_of(file_type: fs::FileType) -> &'static str {
    if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "dir"
    } else if file_type.is_file() {
        "file"
    } else {
        "other"
    }
}

fn stat_value(meta: &fs::Metadata) -> HostValue {
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |duration| duration.as_millis() as f64);
    HostObject::new()
        .field("kind", kind_of(meta.file_type()))
        .field("mtime_ms", mtime_ms)
        .into()
}

fn path_value(path: &Path) -> HostValue {
    HostValue::Str(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时目录充当 home。canonicalize 一下：macOS 的临时目录在 /private 下有软链前缀。
    struct Sandbox {
        home: PathBuf,
    }

    impl Sandbox {
        fn new() -> Self {
            let home = std::env::temp_dir()
                .join(format!("smelt-shell-fs-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(&home).unwrap();
            Self {
                home: fs::canonicalize(&home).unwrap(),
            }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.home.join(relative)
        }

        fn s(&self, relative: &str) -> String {
            self.path(relative).to_string_lossy().into_owned()
        }

        fn host(&self, grant: &str) -> HostFs {
            HostFs::new(
                FsGrant::parse(grant).unwrap(),
                self.home.clone(),
                Rc::new(RefCell::new(HostContext::default())),
            )
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    const GRANT: &str =
        r#"{ "fs": { "read": ["~/.claude"], "write": ["~/.agents/skills", "~/.claude/skills"] } }"#;

    #[test]
    fn grant_rejects_paths_outside_home_and_project() {
        for bad in [r#""/etc""#, r#""~/../x""#, r#""~/""#, r#""relative""#] {
            let text = format!(r#"{{ "fs": {{ "read": [{bad}] }} }}"#);
            assert!(FsGrant::parse(&text).is_err(), "{bad} 应被拒绝");
        }
        assert!(FsGrant::parse(r#"{ "fs": { "write": ["${project}/.agents/skills"] } }"#).is_ok());
    }

    #[test]
    fn paths_must_be_absolute_without_parent_segments() {
        let sandbox = Sandbox::new();
        let host = sandbox.host(GRANT);
        assert!(host.lstat(".agents/skills").is_err());
        let escape = format!("{}/../outside", sandbox.s(".agents/skills"));
        assert!(host.lstat(&escape).is_err());
    }

    #[test]
    fn writes_outside_the_write_roots_are_refused() {
        let sandbox = Sandbox::new();
        let host = sandbox.host(GRANT);
        assert!(host.mkdir(&sandbox.s(".agents/skills/foo")).is_ok());
        assert!(sandbox.path(".agents/skills/foo").is_dir());
        // ~/.claude 只读。
        assert!(host.mkdir(&sandbox.s(".claude/other")).is_err());
        assert!(host.mkdir(&sandbox.s("elsewhere")).is_err());
    }

    #[test]
    fn a_symlink_out_of_the_roots_cannot_be_read_through() {
        let sandbox = Sandbox::new();
        let host = sandbox.host(GRANT);
        fs::create_dir_all(sandbox.path("secret")).unwrap();
        fs::write(sandbox.path("secret/SKILL.md"), "secret").unwrap();
        fs::create_dir_all(sandbox.path(".claude/skills")).unwrap();
        std::os::unix::fs::symlink(sandbox.path("secret"), sandbox.path(".claude/skills/leak"))
            .unwrap();

        // 链接本身看得见……
        let lstat = host.lstat(&sandbox.s(".claude/skills/leak")).unwrap();
        assert_eq!(
            lstat.get("kind").and_then(HostValue::as_str),
            Some("symlink")
        );
        // ……内容读不穿。
        assert!(
            host.read_text(&sandbox.s(".claude/skills/leak/SKILL.md"))
                .is_err()
        );
        assert!(host.list(&sandbox.s(".claude/skills/leak")).is_err());
    }

    #[test]
    fn a_bridge_into_the_roots_reads_through() {
        let sandbox = Sandbox::new();
        let host = sandbox.host(GRANT);
        host.mkdir(&sandbox.s(".agents/skills/foo")).unwrap();
        fs::write(
            sandbox.path(".agents/skills/foo/SKILL.md"),
            "---\nname: foo\n---\n",
        )
        .unwrap();
        host.mkdir(&sandbox.s(".claude/skills")).unwrap();
        host.symlink(
            &sandbox.s(".agents/skills/foo"),
            &sandbox.s(".claude/skills/foo"),
        )
        .unwrap();

        let text = host
            .read_text(&sandbox.s(".claude/skills/foo/SKILL.md"))
            .unwrap();
        assert!(text.as_str().unwrap().contains("name: foo"));
        assert_eq!(
            host.readlink(&sandbox.s(".claude/skills/foo")).unwrap(),
            HostValue::Str(sandbox.s(".agents/skills/foo"))
        );
        let listing = host.list(&sandbox.s(".claude/skills")).unwrap();
        let first = &listing.as_array().unwrap()[0];
        assert_eq!(
            first.get("kind").and_then(HostValue::as_str),
            Some("symlink")
        );
    }

    #[test]
    fn symlink_targets_must_exist_inside_the_roots() {
        let sandbox = Sandbox::new();
        let host = sandbox.host(GRANT);
        host.mkdir(&sandbox.s(".claude/skills")).unwrap();
        fs::create_dir_all(sandbox.path("outside")).unwrap();
        assert!(
            host.symlink(&sandbox.s("outside"), &sandbox.s(".claude/skills/x"))
                .is_err()
        );
        assert!(
            host.symlink(
                &sandbox.s(".agents/skills/missing"),
                &sandbox.s(".claude/skills/y")
            )
            .is_err()
        );
    }

    #[test]
    fn a_dangling_link_is_never_followed_by_mkdir() {
        let sandbox = Sandbox::new();
        let host = sandbox.host(GRANT);
        fs::create_dir_all(sandbox.path(".agents/skills")).unwrap();
        // 插件自己建不出这种链接（symlink 要求目标存在于根内），但磁盘随时会被
        // 别人改：直接用 std 造一条指向根外、目标不存在的悬空链接。
        std::os::unix::fs::symlink(
            sandbox.path("ghost"),
            sandbox.path(".agents/skills/dangling"),
        )
        .unwrap();
        assert!(
            host.mkdir(&sandbox.s(".agents/skills/dangling/child"))
                .is_err()
        );
        assert!(!sandbox.path("ghost").exists());
        // 悬空链接本身仍可 lstat、可删。
        assert!(
            !host
                .lstat(&sandbox.s(".agents/skills/dangling"))
                .unwrap()
                .is_null()
        );
        assert!(
            host.stat(&sandbox.s(".agents/skills/dangling"))
                .unwrap()
                .is_null()
        );
        host.unlink(&sandbox.s(".agents/skills/dangling")).unwrap();
    }

    #[test]
    fn rename_moves_within_the_write_roots_and_unlink_refuses_directories() {
        let sandbox = Sandbox::new();
        let host = sandbox.host(GRANT);
        host.mkdir(&sandbox.s(".agents/skills/a")).unwrap();
        host.rename(
            &sandbox.s(".agents/skills/a"),
            &sandbox.s(".agents/skills/b"),
        )
        .unwrap();
        assert!(sandbox.path(".agents/skills/b").is_dir());
        assert!(host.unlink(&sandbox.s(".agents/skills/b")).is_err());
        assert!(
            host.rename(&sandbox.s(".agents/skills/b"), &sandbox.s("stolen"))
                .is_err()
        );
    }

    #[test]
    fn project_roots_follow_the_host_context() {
        let sandbox = Sandbox::new();
        let context = Rc::new(RefCell::new(HostContext::default()));
        let host = HostFs::new(
            FsGrant::parse(r#"{ "fs": { "write": ["${project}/.agents/skills"] } }"#).unwrap(),
            sandbox.home.clone(),
            context.clone(),
        );
        let target = sandbox.s("repo/.agents/skills/x");
        assert!(
            host.mkdir(&target).is_err(),
            "没有项目时 ${{project}} 根不存在"
        );
        context.borrow_mut().project_root = Some(sandbox.path("repo"));
        host.mkdir(&target).unwrap();
        assert!(sandbox.path("repo/.agents/skills/x").is_dir());
    }
}
