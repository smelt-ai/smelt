//! Project repository adoption without moving or deleting source repositories.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use smelt_core::fs::{FileSystem as _, LocalFs};

#[derive(Clone, Debug)]
pub struct BoundRepository {
    pub root: PathBuf,
    pub managed_link: Option<PathBuf>,
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.chars().any(char::is_control) {
        return Err("仓库名称不能包含控制字符".into());
    }
    crate::worktree::validate_segment(name, "仓库名称")
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub fn repository_root(path: &Path) -> Result<PathBuf, String> {
    let path = fs::canonicalize(path).map_err(|error| format!("无法读取仓库目录：{error}"))?;
    let text = path.to_str().ok_or("仓库路径不是 UTF-8")?;
    let output = crate::run_git(text, &["rev-parse", "--show-toplevel"])
        .map_err(|error| format!("无法检查 Git 仓库：{error}"))?;
    if !output.success() {
        return Err("请选择已有 Git 仓库的根目录，而不是普通文件夹".into());
    }
    let top = fs::canonicalize(String::from_utf8_lossy(&output.stdout).trim())
        .map_err(|error| format!("无法解析 Git 根目录：{error}"))?;
    if top != path {
        return Err(format!("请选择 Git 仓库根目录：{}", top.display()));
    }
    Ok(path)
}

fn destination(project: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let project = fs::canonicalize(project).map_err(|error| format!("无法读取项目：{error}"))?;
    let repos = project.join("repos");
    fs::create_dir_all(&repos).map_err(|error| format!("无法创建 repos 目录：{error}"))?;
    if fs::canonicalize(&repos).map_err(|error| error.to_string())? != repos {
        return Err("项目的 repos 目录不能是指向其它位置的软链接".into());
    }
    let target = repos.join(name);
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err(format!("目录名已被占用，请修改名称：{}", target.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("无法检查目标路径：{error}")),
    }
    Ok(target)
}

pub fn bind(project: &Path, source: &Path, name: &str) -> Result<BoundRepository, String> {
    validate_name(name)?;
    let project = fs::canonicalize(project).map_err(|error| error.to_string())?;
    let root = repository_root(source)?;
    if root.starts_with(&project) {
        return Ok(BoundRepository {
            root,
            managed_link: None,
        });
    }
    let target = destination(&project, name)?;
    exclude_managed_directory(&project, "repos")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, &target)
        .map_err(|error| format!("创建软链接失败：{error}"))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&root, &target)
        .map_err(|error| format!("创建软链接失败，请检查开发者模式或符号链接权限：{error}"))?;
    Ok(BoundRepository {
        root,
        managed_link: Some(target),
    })
}

pub fn clone_repository(project: &Path, url: &str, name: &str) -> Result<BoundRepository, String> {
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_control) {
        return Err("请填写有效的 Git URL".into());
    }
    let target = destination(project, name)?;
    exclude_managed_directory(project, "repos")?;
    // Reserve the destination exclusively; Git accepts an existing empty directory.
    fs::create_dir(&target).map_err(|error| format!("无法创建克隆目标：{error}"))?;
    // Git's clone destination must not carry Windows' verbatim path prefix.
    let output = LocalFs.canonicalize(&target).and_then(|git_target| {
        std::process::Command::new("git")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["clone", "--", url])
            .arg(git_target)
            .output()
    });
    match output {
        Ok(output) if output.status.success() => Ok(BoundRepository {
            root: repository_root(&target)?,
            managed_link: None,
        }),
        result => {
            // Do not echo stderr: Git may include credentials embedded in the URL.
            let reason = match result {
                Ok(_) => "Git 克隆失败，请检查 URL、网络和 SSH/凭据配置".to_string(),
                Err(error) => format!("无法准备或启动 Git 克隆：{error}"),
            };
            // Never recursively delete a failed clone: it may contain useful data.
            let retained = match fs::remove_dir(&target) {
                Ok(()) => String::new(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(_) => format!(
                    "；已保留克隆目录 {}，重试请换名称或先检查该目录",
                    target.display()
                ),
            };
            Err(format!("{reason}{retained}"))
        }
    }
}

pub fn remove_link(project: &Path, link: &Path, root: &Path) -> Result<(), String> {
    let project = fs::canonicalize(project).map_err(|error| error.to_string())?;
    if link.parent() != Some(project.join("repos").as_path()) {
        return Err("软链接不在项目的 repos 目录内，拒绝移除".into());
    }
    let metadata = match fs::symlink_metadata(link) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if fs::canonicalize(project.join("repos")).map_err(|error| error.to_string())?
        != project.join("repos")
    {
        return Err("repos 目录已被替换为软链接，拒绝删除".into());
    }
    if !metadata.file_type().is_symlink() || fs::read_link(link).map_err(|e| e.to_string())? != root
    {
        return Err("软链接已被替换或改指向，拒绝删除；请手动检查".into());
    }
    #[cfg(unix)]
    fs::remove_file(link).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    fs::remove_dir(link).map_err(|error| error.to_string())?;
    Ok(())
}

pub fn exclude_managed_directory(project: &Path, name: &str) -> Result<(), String> {
    let project = fs::canonicalize(project).map_err(|error| error.to_string())?;
    if !project.join(".git").exists() {
        return Ok(());
    }
    let root = repository_root(&project)?;
    let output = crate::run_git(
        root.to_str().ok_or("项目路径不是 UTF-8")?,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ],
    )
    .map_err(|error| error.to_string())?;
    if !output.success() {
        return Err("无法定位 Git 本地忽略文件".into());
    }
    let exclude = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let existing = match fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    let pattern = format!("/{name}/");
    if !existing.lines().any(|line| line == pattern) {
        if let Some(parent) = exclude.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(exclude)
            .map_err(|e| e.to_string())?;
        writeln!(file, "\n{pattern}").map_err(|error| error.to_string())?;
    }
    Ok(())
}
