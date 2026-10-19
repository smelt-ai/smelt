//! 项目文件与文件查看器的运行时实现：`project.*`、`file.*` 控制方法背后的业务规则。
//!
//! 浏览、删除、搜索以项目为范围，越出项目根的路径一律拒绝；查看器按路径打开和
//! 保存（见 `smelt_runtime_client::protocol::files`）。

use std::path::{Component, Path};

use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use smelt_runtime_client::protocol::files::{
    Base64Bytes, CompleteFilesResult, FileContent, ProjectEntry, ProjectSearchHit,
    ProjectSearchResult, is_previewable_image,
};

use crate::fs::LocalFs;

/// 查看器能打开的最大文本文件。再大的多半是日志或数据，编辑器也扛不住。
pub const FILE_OPEN_LIMIT: u64 = 8 * 1024 * 1024;
/// 查看器能预览的最大图片。截图、设计稿都在这以内；客户端还会再降采样。
pub const IMAGE_OPEN_LIMIT: u64 = 20 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum ProjectFileError {
    /// 路径不在项目根之内，或者不是规范的绝对路径。
    OutsideProject(String),
    /// 保存时磁盘内容已经不是客户端打开时的那一版。
    Conflict,
    Io(String),
}

impl std::fmt::Display for ProjectFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideProject(path) => write!(f, "{path} 不在项目内"),
            Self::Conflict => f.write_str("文件已被外部修改"),
            Self::Io(message) => f.write_str(message),
        }
    }
}

/// `path` 是否落在 `root` 之内（含 `root` 本身）。按路径组件比较，不跟随符号链接：
/// 规则管的是客户端传来的路径长什么样，不是磁盘上指向哪里。
fn within(root: &str, path: &str) -> bool {
    let (root, path) = (Path::new(root), Path::new(path));
    root.is_absolute()
        && path.is_absolute()
        && !path.components().any(|c| matches!(c, Component::ParentDir))
        && path.starts_with(root)
}

fn require_within(root: &str, path: &str) -> Result<(), ProjectFileError> {
    if within(root, path) {
        Ok(())
    } else {
        Err(ProjectFileError::OutsideProject(path.to_string()))
    }
}

/// 项目里一个目录的直接子项。排序和噪音过滤沿用 [`crate::fs::list_dir`]。
pub fn tree(root: &str, dir: &str) -> Result<Vec<ProjectEntry>, ProjectFileError> {
    require_within(root, dir)?;
    Ok(crate::fs::list_dir(&LocalFs, Path::new(dir))
        .into_iter()
        .map(|entry| ProjectEntry {
            name: entry.name,
            is_dir: entry.is_dir,
        })
        .collect())
}

/// 删除项目里的一个文件或目录。目录连同内容一起删；指向目录的符号链接只删链接本身。
pub fn delete_entry(root: &str, path: &str) -> Result<(), ProjectFileError> {
    require_within(root, path)?;
    if Path::new(path) == Path::new(root) {
        return Err(ProjectFileError::OutsideProject(path.to_string()));
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| ProjectFileError::Io(e.to_string()))?;
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| ProjectFileError::Io(format!("删除 {path} 失败：{e}")))
}

fn content_version(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 打开文件查看或编辑。可预览的图片按扩展名认，给原始字节。
pub fn open(path: &str) -> Result<FileContent, ProjectFileError> {
    let meta = std::fs::metadata(path).map_err(|e| ProjectFileError::Io(e.to_string()))?;
    let image = is_previewable_image(path);
    let limit = if image {
        IMAGE_OPEN_LIMIT
    } else {
        FILE_OPEN_LIMIT
    };
    if meta.len() > limit {
        return Ok(FileContent::TooLarge {
            size: meta.len(),
            limit,
        });
    }
    let bytes = std::fs::read(path).map_err(|e| ProjectFileError::Io(e.to_string()))?;
    if image {
        return Ok(FileContent::Image {
            data: Base64Bytes(bytes),
        });
    }
    let version = content_version(&bytes);
    Ok(match String::from_utf8(bytes) {
        Ok(text) => FileContent::Text { text, version },
        Err(_) => FileContent::Binary,
    })
}

/// 保存。带了 `base_version` 就先比对磁盘当前内容，对不上返回冲突、不写入；文件
/// 读不出来（比如已被删掉）时直接写，等于重建它。返回写入后的版本。
pub fn save(
    path: &str,
    text: &str,
    base_version: Option<&str>,
) -> Result<String, ProjectFileError> {
    if let Some(base) = base_version
        && let Ok(current) = std::fs::read(path)
        && content_version(&current) != base
    {
        return Err(ProjectFileError::Conflict);
    }
    std::fs::write(path, text).map_err(|e| ProjectFileError::Io(e.to_string()))?;
    Ok(content_version(text.as_bytes()))
}

/// 搜索命中数上限：触顶即停并标记截断，避免超大仓遍历/渲染失控。
const SEARCH_HIT_LIMIT: usize = 200;
/// 内容搜索跳过的单文件大小上限（512KB）：更大的多半是数据/构建产物，逐行扫不划算。
const SEARCH_MAX_FILE_BYTES: u64 = 512 * 1024;

/// 后台遍历项目搜索 query（大小写不敏感）：文件名命中或文件内容逐行命中。
/// 使用 `ignore` crate 遵循 `.gitignore`、`.git/info/exclude`、全局 gitignore 与隐藏文件规则，
/// 兜底跳过 .git/node_modules/target/.DS_Store，并采用流式行读取避免大文件占用过多内存。
/// 文件名命中排在内容命中前。
///
/// 不支持中途取消：客户端换了查询只是丢弃旧结果，这一轮由命中上限和遍历本身收住。
pub fn search(root: &str, query: &str) -> ProjectSearchResult {
    let needle = query.to_lowercase();
    let mut name_hits: Vec<ProjectSearchHit> = Vec::new();
    let mut content_hits: Vec<ProjectSearchHit> = Vec::new();
    let root_path = std::path::Path::new(root);
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .ignore(true)
        .parents(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if matches!(
                name.as_ref(),
                ".git" | "node_modules" | "target" | ".DS_Store"
            ) {
                return false;
            }
            true
        })
        .build();

    for entry in walker.flatten() {
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }

        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        let rel = path
            .strip_prefix(root_path)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let abs = path.to_string_lossy().to_string();

        // 1. 文件名命中：直接记一条（不再看内容），命中行留空
        if name.to_lowercase().contains(&needle) {
            name_hits.push(ProjectSearchHit {
                path: abs,
                rel,
                line: None,
            });
            if name_hits.len() + content_hits.len() >= SEARCH_HIT_LIMIT {
                truncated = true;
                break;
            }
            continue;
        }

        // 2. 内容命中：跳过大文件
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > SEARCH_MAX_FILE_BYTES {
            continue;
        }

        // 流式逐行扫描，避免将大文件全部读入内存；含 NUL 视为二进制并中断
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let mut reader = std::io::BufReader::new(file);
        let mut line_buf = String::new();
        let mut line_no = 0usize;

        use std::io::BufRead as _;
        while let Ok(n) = reader.read_line(&mut line_buf) {
            if n == 0 {
                break;
            }
            line_no += 1;
            // 含 NUL 视为二进制，不逐行扫
            if line_buf.as_bytes().contains(&0) {
                break;
            }
            if line_buf.to_lowercase().contains(&needle) {
                // 预览行去掉首尾空白并截断，避免超长行撑爆列表
                let preview: String = line_buf.trim().chars().take(200).collect();
                content_hits.push(ProjectSearchHit {
                    path: abs,
                    rel,
                    line: Some((line_no, preview)),
                });
                if name_hits.len() + content_hits.len() >= SEARCH_HIT_LIMIT {
                    truncated = true;
                }
                break;
            }
            line_buf.clear();
        }

        if truncated {
            break;
        }
    }

    // 保持相对路径顺序稳定
    name_hits.sort_by(|a, b| a.rel.cmp(&b.rel));
    content_hits.sort_by(|a, b| a.rel.cmp(&b.rel));

    name_hits.extend(content_hits);
    ProjectSearchResult {
        hits: name_hits,
        truncated,
    }
}

/// `@` 补全候选的上限。超大 monorepo 一次全给会让每次打开补全都传几 MB。
const COMPLETE_FILES_LIMIT: usize = 50_000;

/// `@` 补全候选：列一个目录下的文件（相对路径）。优先 `git ls-files`：它天然尊重
/// `.gitignore`，不会把 `target/` 里几万个构建产物灌进补全菜单；非 git 目录
/// 退回浅层遍历，只走两层，够用且不会在大目录上卡住。
pub fn complete_files(cwd: &str) -> CompleteFilesResult {
    let files = git_ls_files(cwd).unwrap_or_else(|| {
        let mut out = Vec::new();
        walk(Path::new(cwd), Path::new(cwd), 0, &mut out);
        out
    });
    let mut entries = with_parent_directories(files);
    let truncated = entries.len() > COMPLETE_FILES_LIMIT;
    entries.truncate(COMPLETE_FILES_LIMIT);
    CompleteFilesResult { entries, truncated }
}

fn with_parent_directories(files: Vec<String>) -> Vec<String> {
    let mut entries = BTreeSet::new();
    for file in files {
        let mut parent = std::path::Path::new(&file).parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            if let Some(path_text) = path.to_str() {
                entries.insert(format!("{path_text}/"));
            }
            parent = path.parent();
        }
        entries.insert(file);
    }
    entries.into_iter().collect()
}

fn git_ls_files(cwd: &str) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            cwd,
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let list: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    (!list.is_empty()).then_some(list)
}

fn walk(root: &std::path::Path, dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
    if depth > 2 || out.len() > 2000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, depth + 1, out);
        } else if let Some(rel) = path.strip_prefix(root).ok().and_then(|p| p.to_str()) {
            out.push(rel.to_string());
        }
    }
}

/// `workspace.browse_dirs`：列出一个目录的子项，给客户端的目录选择器用。隐藏项不列。
pub fn browse_dirs(
    params: &smelt_runtime_client::protocol::workspace::BrowseDirsParams,
) -> Result<smelt_runtime_client::protocol::workspace::BrowseDirsResult, ProjectFileError> {
    use smelt_runtime_client::protocol::workspace::{BrowseDirsResult, BrowseEntry};

    let home = dirs::home_dir().ok_or_else(|| ProjectFileError::Io("找不到用户主目录".into()))?;
    let requested = params
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.clone());
    let dir = std::fs::canonicalize(&requested).map_err(|error| {
        ProjectFileError::Io(format!("打不开 {}：{error}", requested.display()))
    })?;
    if !dir.is_dir() {
        return Err(ProjectFileError::Io(format!("不是目录：{}", dir.display())));
    }
    let mut entries = std::fs::read_dir(&dir)
        .map_err(|error| ProjectFileError::Io(format!("读不了 {}：{error}", dir.display())))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            // 跟着软链走：链到目录的也当目录列出来。
            let is_dir = std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir());
            (is_dir || params.include_files).then(|| BrowseEntry {
                path: entry.path().to_string_lossy().into_owned(),
                name,
                is_dir,
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(BrowseDirsResult {
        parent: dir
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned()),
        path: dir.to_string_lossy().into_owned(),
        home: home.to_string_lossy().into_owned(),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsing_lists_visible_dirs_first_and_files_only_on_request() {
        use smelt_runtime_client::protocol::workspace::BrowseDirsParams;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("beta")).unwrap();
        std::fs::create_dir(root.path().join("Alpha")).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();
        std::fs::write(root.path().join("notes.md"), "x").unwrap();

        let dirs_only = browse_dirs(&BrowseDirsParams {
            path: Some(root.path().to_string_lossy().into_owned()),
            include_files: false,
        })
        .unwrap();
        let names: Vec<_> = dirs_only.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "beta"]);
        assert!(dirs_only.parent.is_some());

        let with_files = browse_dirs(&BrowseDirsParams {
            path: Some(root.path().to_string_lossy().into_owned()),
            include_files: true,
        })
        .unwrap();
        let last = with_files.entries.last().unwrap();
        assert_eq!((last.name.as_str(), last.is_dir), ("notes.md", false));
    }

    /// `@` 的候选源必须真能列出文件——列空了菜单就是个空壳，而且不会报错。
    #[test]
    fn lists_files_under_cwd() {
        let cwd = env!("CARGO_MANIFEST_DIR");
        let files = complete_files(cwd).entries;
        assert!(!files.is_empty(), "本 crate 目录下不该一个文件都列不出来");
        assert!(
            files.iter().any(|f| f.ends_with("project_files.rs")),
            "应当列出本文件，实际前几条：{:?}",
            &files[..files.len().min(5)]
        );
        assert!(
            files.iter().any(|entry| entry.ends_with('/')),
            "文件补全也应包含可继续下钻的目录"
        );
        // git ls-files 尊重 .gitignore：构建产物不该混进补全菜单。
        assert!(
            !files.iter().any(|f| f.starts_with("target/")),
            "target/ 不该出现"
        );
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-project-files-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths_outside_the_project_are_rejected() {
        assert!(within("/repo", "/repo"));
        assert!(within("/repo", "/repo/src/main.rs"));
        assert!(!within("/repo", "/repository/x"), "前缀相同但不是子目录");
        assert!(!within("/repo", "/repo/../etc/passwd"));
        assert!(!within("/repo", "src/main.rs"), "必须是绝对路径");
        assert!(matches!(
            tree("/repo", "/etc"),
            Err(ProjectFileError::OutsideProject(_))
        ));
    }

    #[test]
    fn deleting_the_project_root_itself_is_refused() {
        let root = temp_dir("delete-root");
        let r = root.to_str().unwrap();
        assert!(matches!(
            delete_entry(r, r),
            Err(ProjectFileError::OutsideProject(_))
        ));
        assert!(root.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn delete_entry_removes_files_and_whole_directories() {
        let root = temp_dir("delete");
        std::fs::create_dir_all(root.join("dir/nested")).unwrap();
        std::fs::write(root.join("dir/nested/a.txt"), "a").unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        let r = root.to_str().unwrap();

        delete_entry(r, root.join("b.txt").to_str().unwrap()).unwrap();
        delete_entry(r, root.join("dir").to_str().unwrap()).unwrap();

        assert!(!root.join("b.txt").exists());
        assert!(!root.join("dir").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_reports_binary_and_oversized_files_without_content() {
        let root = temp_dir("open");
        let text = root.join("a.txt");
        let binary = root.join("b.bin");
        std::fs::write(&text, "hello").unwrap();
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        let picture = root.join("shot.png");
        std::fs::write(&picture, [0x89, b'P', b'N', b'G']).unwrap();
        assert_eq!(
            open(picture.to_str().unwrap()).unwrap(),
            FileContent::Image {
                data: Base64Bytes(vec![0x89, b'P', b'N', b'G'])
            }
        );

        let FileContent::Text { text: body, .. } = open(text.to_str().unwrap()).unwrap() else {
            panic!("UTF-8 文件应作为文本打开");
        };
        assert_eq!(body, "hello");
        assert_eq!(open(binary.to_str().unwrap()).unwrap(), FileContent::Binary);

        let big = root.join("big.log");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(FILE_OPEN_LIMIT + 1).unwrap();
        assert!(matches!(
            open(big.to_str().unwrap()).unwrap(),
            FileContent::TooLarge { .. }
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    /// agent 在用户编辑期间改了文件：带旧版本保存必须被拒绝，不能悄悄覆盖；
    /// 用户确认覆盖（不带版本）才写。
    #[test]
    fn save_refuses_to_overwrite_a_file_changed_since_it_was_opened() {
        let root = temp_dir("save");
        let file = root.join("a.txt");
        std::fs::write(&file, "v1").unwrap();
        let path = file.to_str().unwrap();
        let FileContent::Text { version, .. } = open(path).unwrap() else {
            panic!("应作为文本打开");
        };

        std::fs::write(&file, "changed by agent").unwrap();
        assert_eq!(
            save(path, "mine", Some(&version)),
            Err(ProjectFileError::Conflict)
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "changed by agent");

        let forced = save(path, "mine", None).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
        save(path, "mine again", Some(&forced)).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine again");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn search_respects_gitignore_and_matches_content() {
        let temp = temp_dir("search");
        let src_dir = temp.join("src");
        let target_dir = temp.join("target");
        let venv_dir = temp.join(".venv");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::create_dir_all(&venv_dir).unwrap();

        std::fs::write(temp.join(".gitignore"), "target/\n.venv/\nignored.txt\n").unwrap();
        std::fs::write(
            src_dir.join("main.rs"),
            "fn main() {\n    println!(\"hello smelt_search\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            temp.join("README.md"),
            "# Welcome to smelt_search project\n",
        )
        .unwrap();
        std::fs::write(target_dir.join("build.rs"), "smelt_search target file\n").unwrap();
        std::fs::write(venv_dir.join("lib.py"), "smelt_search venv file\n").unwrap();
        std::fs::write(temp.join("ignored.txt"), "smelt_search ignored text\n").unwrap();

        let result = search(temp.to_str().unwrap(), "smelt_search");
        assert!(!result.truncated);

        let hit_paths: Vec<_> = result.hits.iter().map(|h| h.rel.as_str()).collect();
        assert!(
            hit_paths.contains(&"README.md"),
            "README.md should be matched"
        );
        assert!(
            hit_paths.contains(&"src/main.rs"),
            "src/main.rs should be matched"
        );
        assert!(
            !hit_paths.iter().any(|p| p.starts_with("target/")),
            "target should be ignored"
        );
        assert!(
            !hit_paths.iter().any(|p| p.starts_with(".venv/")),
            ".venv should be ignored"
        );
        assert!(
            !hit_paths.contains(&"ignored.txt"),
            "ignored.txt should be ignored"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }
}
