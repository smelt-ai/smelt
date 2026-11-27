//! diff：先量后读。
//!
//! 改动列表（[`diff_files`]）先给事实——路径、怎么变的、多大、几行，再按列表顺序把放得下的
//! 文件的 diff 接成一份汇总补丁；放不下的按文件另取（[`file_diff`]、[`commit_file_diff`]）。
//! 两头都先量：两侧里有一份超过 [`DIFF_LIMIT`] 的文件不交给 git 算 diff（几百 MB 的日志，
//! git 也得整个读进来算）；读 git 的输出也封顶，超了就杀掉进程，不把它收进内存。

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use smelt_core::subprocess::{Limited, LocalSubprocess, run_git_limited};
use smelt_runtime_client::protocol::git::{
    DiffScope, GitChangeStatus, GitDiffBody, GitDiffEntry, GitDiffFilesResult, GitDiffText,
    GitFileDiffResult, GitLineCounts,
};

use crate::{git_err, run_git, run_git_stdin};

/// 查看上限：两侧里有一份文件超过它，或者它的 diff 超过它，就不给正文。和打开文件的上限
/// 一样：打不开的文件，看它的 diff 也没意义。
pub const DIFF_LIMIT: u64 = smelt_core::project_files::FILE_OPEN_LIMIT;

/// 单个文件进汇总补丁的上限（两侧里较大那份）：一个大文件不该把汇总的额度占满、挤掉后面
/// 所有文件。超过的在列表里留一行，点开单独看，和代码评审工具把大 diff 默认收起一样。
pub const AGGREGATE_FILE_LIMIT: u64 = 1 << 20;

/// 一条 numstat 命令带的路径参数总字节数。全部路径一次塞进命令行会撞系统的参数长度上限。
const PATHSPEC_BATCH_BYTES: usize = 64 << 10;

/// 看文件开头多少字节判断是不是二进制：和 git 一样（`buffer_is_binary` 的 `FIRST_FEW_BYTES`）。
const BINARY_SNIFF_BYTES: u64 = 8000;

const GITLINK_MODE: &str = "160000";

/// 把 diff 交给界面前统一带上的参数：用户配置的外部 diff 工具、强制颜色都会让输出没法解析。
const PLAIN_DIFF: [&str; 2] = ["--no-ext-diff", "--no-color"];

/// 一个仓库在 `scope` 里的全部改动和汇总补丁。
pub fn diff_files(
    root: &str,
    scope: DiffScope,
    untracked_paths: &[String],
) -> Result<GitDiffFilesResult, String> {
    let mut files = diff_entries(root, scope, untracked_paths)?;
    let patch = aggregate_patch(root, scope, &mut files)?;
    Ok(GitDiffFilesResult { files, patch })
}

/// 一个仓库在 `scope` 里的全部改动，`untracked_paths` 跟在已跟踪的后面（客户端从它手上的
/// status 里取，免得两次 status 对不上；status 之后被删掉的不再列出）。
///
/// 不把子仓的文件列进父仓：子仓有自己的分组和自己的提交入口。父仓这边只列 gitlink 指针变化。
fn diff_entries(
    root: &str,
    scope: DiffScope,
    untracked_paths: &[String],
) -> Result<Vec<GitDiffEntry>, String> {
    let raw = raw_entries(root, scope, &[])?;
    let sizes = measure(root, &raw)?;
    let counted: Vec<&RawEntry> = raw
        .iter()
        .zip(&sizes)
        .filter(|(e, size)| !e.gitlink() && **size <= DIFF_LIMIT)
        .map(|(e, _)| e)
        .collect();
    let counts = numstat(root, scope, &counted)?;
    let mut entries: Vec<GitDiffEntry> = raw
        .into_iter()
        .zip(sizes)
        .map(|(e, size)| {
            let gitlink = e.gitlink();
            let body = if gitlink {
                GitDiffBody::Submodule
            } else if size > DIFF_LIMIT {
                GitDiffBody::TooLarge
            } else {
                match counts.get(&e.path) {
                    Some(Some(lines)) => GitDiffBody::Text {
                        lines: Some(*lines),
                    },
                    Some(None) => GitDiffBody::Binary,
                    None => GitDiffBody::Text { lines: None },
                }
            };
            GitDiffEntry {
                path: e.path,
                old_path: e.old_path,
                status: e.status,
                size: if gitlink { 0 } else { size },
                body,
                in_patch: false,
            }
        })
        .collect();
    entries.extend(
        untracked_paths
            .iter()
            .filter_map(|path| untracked_entry(root, path)),
    );
    Ok(entries)
}

/// 按列表顺序把放得下的文件的 diff 接起来，放进去的标上 `in_patch`。
///
/// 已跟踪的先一条命令跑完；整批超过剩下的额度就拆成一个个文件再跑，放得下的照样放进去。
/// 未跟踪的只能一个个和空文件比。每条命令都只读到剩下的额度为止。
fn aggregate_patch(
    root: &str,
    scope: DiffScope,
    files: &mut [GitDiffEntry],
) -> Result<String, String> {
    let wanted = |e: &GitDiffEntry| {
        e.size <= AGGREGATE_FILE_LIMIT
            && matches!(
                e.body,
                GitDiffBody::Text { .. } | GitDiffBody::Binary | GitDiffBody::Submodule
            )
    };
    let tracked: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].status != GitChangeStatus::Untracked && wanted(&files[i]))
        .collect();
    let mut runs: VecDeque<Vec<usize>> = pathspec_batches(files, tracked).into();
    runs.extend(
        (0..files.len())
            .filter(|&i| files[i].status == GitChangeStatus::Untracked && wanted(&files[i]))
            .map(|i| vec![i]),
    );
    let limit = usize::try_from(DIFF_LIMIT).unwrap_or(usize::MAX);
    let mut patch = String::new();
    while let Some(run) = runs.pop_front() {
        let left = limit.saturating_sub(patch.len());
        if left == 0 {
            break;
        }
        let text = match files[run[0]].status {
            GitChangeStatus::Untracked => {
                let mut args = vec!["diff"];
                args.extend(PLAIN_DIFF);
                args.extend(["--no-index", "--", "/dev/null", &files[run[0]].path]);
                limited(root, &args, left)?
            }
            _ => {
                let specs: Vec<String> = run.iter().flat_map(|&i| pathspecs(&files[i])).collect();
                let mut args = vec!["diff"];
                args.extend(PLAIN_DIFF);
                args.extend(scope.revs());
                args.push("--");
                args.extend(specs.iter().map(String::as_str));
                limited(root, &args, left)?
            }
        };
        match text {
            Some(text) => {
                patch.push_str(&text);
                for &i in &run {
                    files[i].in_patch = true;
                }
            }
            // 整批放不下：拆开，按原来的顺序排在最前面。
            None if run.len() > 1 => {
                for &i in run.iter().rev() {
                    runs.push_front(vec![i]);
                }
            }
            None => {}
        }
    }
    Ok(patch)
}

/// 一项的路径参数：重命名、复制的两头都要带，git 才认得出来。
fn pathspecs(entry: &GitDiffEntry) -> Vec<String> {
    entry
        .old_path
        .iter()
        .chain([&entry.path])
        .map(|p| literal(p))
        .collect()
}

/// 把 `indices` 按路径参数的总字节数分批（见 [`PATHSPEC_BATCH_BYTES`]）。
fn pathspec_batches(files: &[GitDiffEntry], indices: Vec<usize>) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut bytes = 0;
    for i in indices {
        let len: usize = pathspecs(&files[i]).iter().map(String::len).sum();
        match batches.last_mut() {
            Some(batch) if bytes + len <= PATHSPEC_BATCH_BYTES => batch.push(i),
            _ => {
                batches.push(vec![i]);
                bytes = 0;
            }
        }
        bytes += len;
    }
    batches
}

/// 跑出 diff 原文，最多读 `limit` 字节：超了是 `None`。
fn limited(root: &str, args: &[&str], limit: usize) -> Result<Option<String>, String> {
    match run_git_limited(&LocalSubprocess, Path::new(root), args, limit)
        .map_err(|e| format!("couldn't run git: {e}"))?
    {
        Limited::OverLimit => Ok(None),
        Limited::Done(out) if !out.success() && out.stdout.is_empty() => {
            Err(git_err(&out, "git diff failed"))
        }
        Limited::Done(out) => Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
    }
}

/// 单个文件的 diff。`old_path` 是重命名前的路径：两头都在路径里，git 才认得出这是重命名。
pub fn file_diff(
    root: &str,
    path: &str,
    old_path: Option<&str>,
    untracked: bool,
    scope: DiffScope,
) -> Result<GitFileDiffResult, String> {
    let worktree_file = Some(Path::new(root).join(path))
        .filter(|full| smelt_core::fs::is_regular_file(&smelt_core::fs::LocalFs, full))
        .map(|full| full.to_string_lossy().into_owned());
    if untracked {
        return Ok(GitFileDiffResult {
            diff: untracked_text(root, path)?,
            has_staged: false,
            is_gitlink: false,
            worktree_file,
        });
    }
    let has_staged = run_git(root, &["diff", "--staged", "--quiet", "--", &literal(path)])
        .map(|o| !o.success())
        .unwrap_or(false);
    let paths: Vec<&str> = old_path.into_iter().chain([path]).collect();
    let raw = raw_entries(root, scope, &paths)?;
    let size = measure(root, &raw)?.into_iter().max().unwrap_or(0);
    let diff = if size > DIFF_LIMIT {
        GitDiffText::TooLarge { size }
    } else {
        let specs: Vec<String> = paths.iter().map(|p| literal(p)).collect();
        let mut args = vec!["diff"];
        args.extend(PLAIN_DIFF);
        args.extend(scope.revs());
        args.push("--");
        args.extend(specs.iter().map(String::as_str));
        limited_text(root, &args, size)?
    };
    Ok(GitFileDiffResult {
        diff,
        has_staged,
        is_gitlink: raw.iter().any(RawEntry::gitlink),
        worktree_file,
    })
}

/// 单个文件在某个提交里的改动。`--format=` 去掉提交头，只留 diff 本体。
pub fn commit_file_diff(root: &str, hash: &str, path: &str) -> Result<GitDiffText, String> {
    let spec = literal(path);
    let raw = run_git(
        root,
        &[
            "show",
            "--raw",
            "-z",
            "--no-abbrev",
            "--format=",
            hash,
            "--",
            &spec,
        ],
    )
    .map_err(|e| format!("couldn't run git show: {e}"))?;
    if !raw.success() {
        return Err(git_err(&raw, "git show failed"));
    }
    // 合并提交是 `::` 开头的合并格式，几个父提交各一份：每个对象都量。
    let shas: Vec<String> = raw
        .stdout
        .split(|b| *b == 0)
        .filter_map(|field| field.strip_prefix(b":"))
        .flat_map(|header| header.split(|b| *b == b' '))
        .filter(|token| token.len() >= 40 && token.iter().all(u8::is_ascii_hexdigit))
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .filter(|sha| !is_null_sha(sha))
        .collect();
    let blobs = blob_sizes(root, shas.iter().map(String::as_str))?;
    let size = blobs.values().copied().max().unwrap_or(0);
    if size > DIFF_LIMIT {
        return Ok(GitDiffText::TooLarge { size });
    }
    let mut args = vec!["show"];
    args.extend(PLAIN_DIFF);
    args.extend(["--format=", hash, "--", &spec]);
    limited_text(root, &args, size)
}

/// `git diff --raw` 的一项。
struct RawEntry {
    status: GitChangeStatus,
    old_mode: String,
    new_mode: String,
    old_sha: String,
    new_sha: String,
    /// 重命名、复制前的路径。
    old_path: Option<String>,
    path: String,
}

impl RawEntry {
    fn gitlink(&self) -> bool {
        self.old_mode == GITLINK_MODE || self.new_mode == GITLINK_MODE
    }
}

/// `git diff <scope> --raw`，限定在 `paths`（空就是全部）。只比对象名和 stat，不算 diff。
fn raw_entries(root: &str, scope: DiffScope, paths: &[&str]) -> Result<Vec<RawEntry>, String> {
    let specs: Vec<String> = paths.iter().map(|p| literal(p)).collect();
    let mut args = vec!["diff"];
    args.extend(scope.revs());
    args.extend(["--raw", "-z", "--no-abbrev", "--"]);
    args.extend(specs.iter().map(String::as_str));
    let out = run_git(root, &args).map_err(|e| format!("couldn't run git diff: {e}"))?;
    if !out.success() {
        return Err(git_err(&out, "git diff failed"));
    }
    Ok(parse_raw(&out.stdout))
}

/// `--raw -z`：`:旧模式 新模式 旧对象 新对象 状态\0路径\0`，重命名和复制多一段新路径。
fn parse_raw(bytes: &[u8]) -> Vec<RawEntry> {
    let mut fields = bytes
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned());
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    while let Some(header) = fields.next() {
        let Some(header) = header.strip_prefix(':') else {
            continue;
        };
        let parts: Vec<&str> = header.split(' ').collect();
        let [old_mode, new_mode, old_sha, new_sha, status] = parts[..] else {
            continue;
        };
        let status = GitChangeStatus::from_letter(status.bytes().next().unwrap_or(b'M'));
        let first = fields.next().unwrap_or_default();
        let (old_path, path) =
            if matches!(status, GitChangeStatus::Renamed | GitChangeStatus::Copied) {
                (Some(first), fields.next().unwrap_or_default())
            } else {
                (None, first)
            };
        let entry = RawEntry {
            status,
            old_mode: old_mode.to_string(),
            new_mode: new_mode.to_string(),
            old_sha: old_sha.to_string(),
            new_sha: new_sha.to_string(),
            old_path,
            path,
        };
        // 冲突中的文件先出一条对象全零的 U，再出一条真正的比较：合成一项，状态留 U。
        if let Some(&at) = index.get(&entry.path) {
            let unmerged = entries[at].status == GitChangeStatus::Unmerged;
            entries[at] = RawEntry {
                status: if unmerged {
                    GitChangeStatus::Unmerged
                } else {
                    entry.status
                },
                ..entry
            };
            continue;
        }
        index.insert(entry.path.clone(), entries.len());
        entries.push(entry);
    }
    entries
}

/// 每项两侧里较大那份文件的字节数。对象用 `cat-file --batch-check` 一次量完；新的一侧
/// 在工作区（对象名全零）就看磁盘。子模块不量。
fn measure(root: &str, entries: &[RawEntry]) -> Result<Vec<u64>, String> {
    let shas = entries
        .iter()
        .filter(|e| !e.gitlink())
        .flat_map(|e| [e.old_sha.as_str(), e.new_sha.as_str()])
        .filter(|sha| !is_null_sha(sha));
    let blobs = blob_sizes(root, shas)?;
    Ok(entries
        .iter()
        .map(|e| {
            if e.gitlink() {
                return 0;
            }
            let side = |sha: &str| blobs.get(sha).copied().unwrap_or(0);
            let new = if !is_null_sha(&e.new_sha) {
                side(&e.new_sha)
            } else if e.status == GitChangeStatus::Deleted {
                0
            } else {
                std::fs::symlink_metadata(Path::new(root).join(&e.path))
                    .map(|m| if m.is_dir() { 0 } else { m.len() })
                    .unwrap_or(0)
            };
            side(&e.old_sha).max(new)
        })
        .collect())
}

/// 对象名到字节数。不存在的对象（子模块的提交不在父仓里）不在结果里。
fn blob_sizes<'a>(
    root: &str,
    shas: impl IntoIterator<Item = &'a str>,
) -> Result<HashMap<String, u64>, String> {
    let mut input = String::new();
    for sha in shas {
        input.push_str(sha);
        input.push('\n');
    }
    if input.is_empty() {
        return Ok(HashMap::new());
    }
    let out = run_git_stdin(root, &["cat-file", "--batch-check"], &input)
        .map_err(|e| format!("couldn't run git cat-file: {e}"))?;
    if !out.success() {
        return Err(git_err(&out, "git cat-file failed"));
    }
    // 每行 `对象名 类型 字节数`，或 `对象名 missing`。
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split(' ');
            let sha = parts.next()?;
            let size = parts.nth(1)?.parse().ok()?;
            Some((sha.to_string(), size))
        })
        .collect())
}

/// 增删行数，按新路径；值为 `None` 是二进制。路径分批带上，重命名的两头在同一批里。
fn numstat(
    root: &str,
    scope: DiffScope,
    entries: &[&RawEntry],
) -> Result<HashMap<String, Option<GitLineCounts>>, String> {
    let mut counts = HashMap::new();
    let mut batch: Vec<String> = Vec::new();
    let mut batch_bytes = 0;
    for entry in entries {
        let specs: Vec<String> = entry
            .old_path
            .iter()
            .chain([&entry.path])
            .map(|p| literal(p))
            .collect();
        let bytes: usize = specs.iter().map(String::len).sum();
        if !batch.is_empty() && batch_bytes + bytes > PATHSPEC_BATCH_BYTES {
            run_numstat(root, scope, &batch, &mut counts)?;
            batch.clear();
            batch_bytes = 0;
        }
        batch.extend(specs);
        batch_bytes += bytes;
    }
    if !batch.is_empty() {
        run_numstat(root, scope, &batch, &mut counts)?;
    }
    Ok(counts)
}

fn run_numstat(
    root: &str,
    scope: DiffScope,
    specs: &[String],
    counts: &mut HashMap<String, Option<GitLineCounts>>,
) -> Result<(), String> {
    let mut args = vec!["diff", "--no-ext-diff"];
    args.extend(scope.revs());
    args.extend(["--numstat", "-z", "--"]);
    args.extend(specs.iter().map(String::as_str));
    let out = run_git(root, &args).map_err(|e| format!("couldn't run git diff: {e}"))?;
    if !out.success() {
        return Err(git_err(&out, "git diff failed"));
    }
    parse_numstat(&out.stdout, counts);
    Ok(())
}

/// `--numstat -z`：`增\t删\t路径\0`；重命名时路径那格是空的，后面跟旧路径、新路径两段。
/// 二进制的增删是 `-`。
fn parse_numstat(bytes: &[u8], counts: &mut HashMap<String, Option<GitLineCounts>>) {
    let mut fields = bytes
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned());
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            fields.next();
            fields.next().unwrap_or_default()
        } else {
            path.to_string()
        };
        let lines = match (added.parse(), deleted.parse()) {
            (Ok(added), Ok(deleted)) => Some(GitLineCounts { added, deleted }),
            _ => None,
        };
        counts.insert(path, lines);
    }
}

/// 未跟踪的一项。不在了（status 之后被删）就不列。
fn untracked_entry(root: &str, path: &str) -> Option<GitDiffEntry> {
    let path = path.trim_end_matches('/');
    let full = Path::new(root).join(path);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    let (size, body) = if meta.is_dir() {
        (0, GitDiffBody::Directory)
    } else if meta.len() > DIFF_LIMIT {
        (meta.len(), GitDiffBody::TooLarge)
    } else if meta.is_file() && looks_binary(&full) {
        (meta.len(), GitDiffBody::Binary)
    } else {
        (meta.len(), GitDiffBody::Text { lines: None })
    };
    Some(GitDiffEntry {
        path: path.to_string(),
        old_path: None,
        status: GitChangeStatus::Untracked,
        size,
        body,
        in_patch: false,
    })
}

/// 开头几千字节里有 NUL 就当二进制，和 git 的判断一样。读不了的按文本，交给 diff 去报错。
fn looks_binary(path: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(BINARY_SNIFF_BYTES).read_to_end(&mut head))
        .is_ok_and(|_| head.contains(&0))
}

/// 未跟踪文件的 diff：和空文件比。目录（嵌套仓库）没有正文，界面只显示文件头。
fn untracked_text(root: &str, path: &str) -> Result<GitDiffText, String> {
    let path = path.trim_end_matches('/');
    let meta = std::fs::symlink_metadata(Path::new(root).join(path))
        .map_err(|e| format!("couldn't read {path}: {e}"))?;
    if meta.is_dir() {
        return Ok(GitDiffText::Text {
            text: String::new(),
        });
    }
    if meta.len() > DIFF_LIMIT {
        return Ok(GitDiffText::TooLarge { size: meta.len() });
    }
    let mut args = vec!["diff"];
    args.extend(PLAIN_DIFF);
    args.extend(["--no-index", "--", "/dev/null", path]);
    limited_text(root, &args, meta.len())
}

/// 跑出 diff 原文，输出到 [`DIFF_LIMIT`] 就停。`size` 是量过的文件大小，超了时原样报回去。
///
/// 失败只看「没成功且什么都没输出」：`--no-index` 有差异时退出码是 1，输出照样是 diff。
fn limited_text(root: &str, args: &[&str], size: u64) -> Result<GitDiffText, String> {
    let limit = usize::try_from(DIFF_LIMIT).unwrap_or(usize::MAX);
    Ok(match limited(root, args, limit)? {
        Some(text) => GitDiffText::Text { text },
        None => GitDiffText::TooLarge { size },
    })
}

/// 按字面匹配的路径参数：`[id].tsx` 当 glob 会连带 `i.tsx`、`d.tsx`。
fn literal(path: &str) -> String {
    format!(":(literal){path}")
}

fn is_null_sha(sha: &str) -> bool {
    sha.bytes().all(|b| b == b'0')
}
