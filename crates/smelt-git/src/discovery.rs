//! 仓库发现：把「一个目录里到底有哪些 git 仓库」这件事独立出来。
//!
//! **为什么不复用父仓 `git status`**：旧实现唯一的数据源是项目根的 `git status`，
//! 发现 gitlink 后递归进去把子仓文件摊回父仓列表。这条路只能看到父仓愿意报告的
//! 东西——被 `.gitignore` 忽略的独立仓库、工作区干净的子仓、平级仓库全都不可见；
//! 更糟的是摊平后文件失去仓库身份，写操作（commit/push）只能落回父仓，导致子仓的
//! 改动能暂存却永远提交不出去。
//!
//! 所以发现与展示必须解耦：这里只回答「有哪些仓库」，每个仓库之后各自跑自己的
//! status、各自提交。参考 VS Code `extensions/git` 的 `Model.openRepository`——
//! 多条互相独立的发现来源，统一收口到一处做真根解析、去重和守门。
//!
//! 来源本身是注册点（[`DiscoverySource`]）而不是写死的 if-else：新增一种来源
//! （例如用户显式配置的路径列表、或插件贡献的来源）只需注册，不改这里的主干。

use smelt_core::fs::FileSystem;
use smelt_core::subprocess::Subprocess;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 仓库在工作区里的身份。UI 用它区分图标与分组标题，写操作不看它——
/// 任何 kind 的仓库都是独立的提交单位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoKind {
    /// 项目根自己。
    Root,
    /// 父仓 `.gitmodules` 声明的 submodule。
    Submodule,
    /// 落在项目根内、但不是 submodule 的独立仓库（含被 `.gitignore` 忽略的，
    /// 以及检出在项目根内的 worktree）。
    Nested,
}

/// 一个已确认的仓库。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredRepo {
    /// 仓库真实根（`git rev-parse --show-toplevel` 的结果，已 canonicalize）。
    pub root: PathBuf,
    /// 相对项目根的路径。项目根自己是空串。仅用于展示与排序。
    pub rel_path: String,
    pub kind: RepoKind,
}

/// 发现候选：来源产出的原始条目，还没做真根解析和去重。
#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub kind: RepoKind,
}

#[derive(Clone, Debug)]
pub struct DiscoveryOptions {
    /// 子目录扫描深度。1 = 只看项目根的直接子目录。
    ///
    /// 默认 2 而不是 1：常见布局里子仓不止一层（`vendor/lib`、`packages/x`）。
    /// 也不是无限——monorepo 里往下无限扫是纯浪费。配合「进了仓库就不再下钻」
    /// 的规则，这个深度只作用在还没归属到任何仓库的目录上。
    pub max_depth: usize,
    /// 扫描时跳过的目录名。默认值是依赖/产物目录——它们里面的 git 仓库
    /// （npm 包自带的 `.git`、cargo 缓存）不是用户的工作对象。
    pub ignored_dirs: Vec<String>,
    /// 最多返回多少个仓库。超出后截断并置位 [`Discovery::truncated`]，
    /// 而不是继续扫下去把 UI 和磁盘一起拖垮。
    pub max_repos: usize,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            max_depth: 2,
            ignored_dirs: [
                "node_modules",
                "target",
                "dist",
                "build",
                "vendor/bundle",
                ".venv",
                "venv",
                "__pycache__",
                ".next",
                ".cache",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            max_repos: 10,
        }
    }
}

/// 来源在产出候选时能用到的东西。
pub struct DiscoveryContext<'a> {
    /// 项目根，已 canonicalize。
    pub project_root: &'a Path,
    pub fs: &'a dyn FileSystem,
    pub subprocess: &'a dyn Subprocess,
    pub options: &'a DiscoveryOptions,
}

/// 一条仓库发现来源。
///
/// 来源只负责「指出可能是仓库的目录」，不负责判断真假、去重或守门——那些统一在
/// [`discover_with_sources`] 里做，保证所有来源受同一套规则约束。
pub trait DiscoverySource {
    /// 诊断用的稳定名字。
    fn name(&self) -> &'static str;

    fn candidates(&self, ctx: &DiscoveryContext<'_>) -> Vec<Candidate>;
}

/// 发现结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    /// 项目根排第一，其余按相对路径字典序。
    pub repos: Vec<DiscoveredRepo>,
    /// 是否因为 `max_repos` 截断过。UI 要据此提示用户，而不是假装列全了。
    pub truncated: bool,
}

// ===================== 内置来源 =====================

/// 项目根自己。
pub struct ProjectRootSource;

impl DiscoverySource for ProjectRootSource {
    fn name(&self) -> &'static str {
        "project-root"
    }

    fn candidates(&self, ctx: &DiscoveryContext<'_>) -> Vec<Candidate> {
        vec![Candidate {
            path: ctx.project_root.to_path_buf(),
            kind: RepoKind::Root,
        }]
    }
}

/// 子目录广度优先扫描。覆盖平级仓库、vendor 里的独立仓库、被 `.gitignore`
/// 忽略的仓库，以及检出在项目根内的 worktree。
pub struct SubdirectoryScanSource;

impl DiscoverySource for SubdirectoryScanSource {
    fn name(&self) -> &'static str {
        "subdirectory-scan"
    }

    fn candidates(&self, ctx: &DiscoveryContext<'_>) -> Vec<Candidate> {
        let mut out = Vec::new();
        let mut queue = vec![(ctx.project_root.to_path_buf(), 0usize)];
        while let Some((dir, depth)) = queue.pop() {
            if depth >= ctx.options.max_depth {
                continue;
            }
            let Ok(entries) = ctx.fs.read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                if !entry.is_dir || entry.name == ".git" {
                    continue;
                }
                if ctx.options.ignored_dirs.iter().any(|d| d == &entry.name) {
                    continue;
                }
                // `.git` 可能是目录（普通仓库）也可能是文件（submodule / worktree），
                // 两种都算候选；到底是不是仓库根由 rev-parse 定夺。
                if ctx.fs.exists(&entry.path.join(".git")) {
                    out.push(Candidate {
                        path: entry.path.clone(),
                        kind: RepoKind::Nested,
                    });
                    // 不再往仓库内部下钻：里面的东西归它自己管，继续扫只会把
                    // 子仓的依赖目录、嵌套 worktree 当成工作区的仓库列出来。
                    // 子仓自己的 submodule 由该仓库自己的发现负责。
                    continue;
                }
                queue.push((entry.path, depth + 1));
            }
        }
        out
    }
}

/// `.gitmodules` 声明的 submodule。
///
/// 独立于子目录扫描的价值有两个：能标出未初始化的条目（磁盘上没有 `.git`，
/// 扫描看不到），以及能发现深于 `max_depth` 的 submodule。
pub struct SubmoduleSource;

impl DiscoverySource for SubmoduleSource {
    fn name(&self) -> &'static str {
        "gitmodules"
    }

    fn candidates(&self, ctx: &DiscoveryContext<'_>) -> Vec<Candidate> {
        let modules = ctx.project_root.join(".gitmodules");
        if !ctx.fs.exists(&modules) {
            return Vec::new();
        }
        // 用 git 自己解析，不手搓 ini：路径可能带引号、续行和转义。
        let Ok(out) = crate::run_git_with(
            ctx.subprocess,
            &ctx.project_root.to_string_lossy(),
            &[
                "config",
                "-f",
                ".gitmodules",
                "--get-regexp",
                r"^submodule\..*\.path$",
            ],
        ) else {
            return Vec::new();
        };
        if !out.success() {
            return Vec::new();
        }
        let mut candidates = Vec::new();
        for line in out.stdout_str().lines() {
            let Some((_, rel)) = line.split_once(' ') else {
                continue;
            };
            let rel = rel.trim();
            if rel.is_empty() {
                continue;
            }
            let path = ctx.project_root.join(rel);
            // 没检出的 submodule 不是仓库：没有 `.git`，跑不了任何 git 命令，
            // 既不能提交也不能看 diff。把它们算进来只会占满 `max_repos` 配额，
            // 把真正能操作的仓库挤掉。
            if !ctx.fs.exists(&path.join(".git")) {
                continue;
            }
            candidates.push(Candidate {
                path,
                kind: RepoKind::Submodule,
            });
        }
        candidates
    }
}

/// 默认来源集合。调用方可以在前后追加自己的来源，顺序决定 kind 的归属优先级
/// （先被确认的候选赢，后来的同一仓库会被去重丢掉）。
pub fn default_sources() -> Vec<Box<dyn DiscoverySource>> {
    vec![
        Box::new(ProjectRootSource),
        // submodule 排在通用扫描之前：同一个目录两边都会报，先到的 kind 生效，
        // 我们要的是更精确的「这是 submodule」而不是笼统的 Nested。
        Box::new(SubmoduleSource),
        Box::new(SubdirectoryScanSource),
    ]
}

// ===================== 收口 =====================

/// 用指定来源集合做一次发现。
///
/// 所有来源的候选都在这里统一过一遍：真根解析 → 边界检查 → 去重 → 上限截断。
pub fn discover_with_sources(
    fs: &dyn FileSystem,
    subprocess: &dyn Subprocess,
    project_root: &Path,
    options: &DiscoveryOptions,
    sources: &[Box<dyn DiscoverySource>],
) -> Discovery {
    let Ok(project_root) = fs.canonicalize(project_root) else {
        return Discovery::default();
    };
    let ctx = DiscoveryContext {
        project_root: &project_root,
        fs,
        subprocess,
        options,
    };

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut repos: Vec<DiscoveredRepo> = Vec::new();
    let mut truncated = false;

    for source in sources {
        for candidate in source.candidates(&ctx) {
            let Some(repo) = confirm(&ctx, &candidate) else {
                continue;
            };
            if !seen.insert(repo.root.clone()) {
                continue;
            }
            if repos.len() >= options.max_repos {
                truncated = true;
                continue;
            }
            repos.push(repo);
        }
    }

    // 项目根置顶，其余按相对路径排，保证 UI 顺序稳定（来源顺序不该泄漏到界面上）。
    repos.sort_by(|a, b| match (a.kind, b.kind) {
        (RepoKind::Root, RepoKind::Root) => a.rel_path.cmp(&b.rel_path),
        (RepoKind::Root, _) => std::cmp::Ordering::Less,
        (_, RepoKind::Root) => std::cmp::Ordering::Greater,
        _ => a.rel_path.cmp(&b.rel_path),
    });

    Discovery { repos, truncated }
}

/// 用默认来源集合做一次发现。
pub fn discover(
    fs: &dyn FileSystem,
    subprocess: &dyn Subprocess,
    project_root: &Path,
    options: &DiscoveryOptions,
) -> Discovery {
    discover_with_sources(fs, subprocess, project_root, options, &default_sources())
}

/// 候选 → 确认过的仓库。不合格的一律丢弃，不抛错：发现是尽力而为的，
/// 单个目录探测失败不能让整个列表消失。
fn confirm(ctx: &DiscoveryContext<'_>, candidate: &Candidate) -> Option<DiscoveredRepo> {
    if !ctx.fs.is_dir(&candidate.path) {
        return None;
    }
    let out = crate::run_git_with(
        ctx.subprocess,
        &candidate.path.to_string_lossy(),
        &["rev-parse", "--show-toplevel"],
    )
    .ok()?;
    if !out.success() {
        return None;
    }
    let toplevel = out.stdout_str().trim().to_string();
    if toplevel.is_empty() {
        return None;
    }
    // rev-parse 返回的是真盘路径（macOS 上 /var → /private/var），两边都
    // canonicalize 才能比较；否则项目根判定会全线误判。
    let root = ctx.fs.canonicalize(Path::new(&toplevel)).ok()?;

    // 守门：不接受项目根之外的仓库。候选目录如果本身不是仓库根，rev-parse 会
    // 返回它的祖先——项目根在某个大仓库里时，这会把整个大仓库拖进来。
    // 对应 VS Code 的 `openRepositoryInParentFolders`，我们选择直接不打开。
    let rel = rel_path(ctx.project_root, &root)?;

    Some(DiscoveredRepo {
        root,
        rel_path: rel,
        kind: candidate.kind,
    })
}

/// 相对项目根的路径；不在项目根内返回 None。
fn rel_path(project_root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(project_root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests;
