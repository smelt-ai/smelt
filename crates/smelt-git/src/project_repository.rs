//! Project repository adoption without moving or deleting source repositories.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use smelt_core::fs::{FileSystem as _, LocalFs};
use smelt_runtime_client::protocol::reason::ControlReason;

use crate::GitError;

#[derive(Clone, Debug)]
pub struct BoundRepository {
    pub root: PathBuf,
    pub managed_link: Option<PathBuf>,
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.chars().any(char::is_control) {
        return Err("The repository name can't contain control characters".into());
    }
    crate::worktree::validate_segment(name, "The repository name")
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub fn repository_root(path: &Path) -> Result<PathBuf, GitError> {
    let path = fs::canonicalize(path)
        .map_err(|error| format!("couldn't read the repository directory: {error}"))?;
    let text = path.to_str().ok_or("the repository path isn't UTF-8")?;
    let output = crate::run_git(text, &["rev-parse", "--show-toplevel"])
        .map_err(|error| format!("couldn't check the Git repository: {error}"))?;
    if !output.success() {
        return Err(GitError::because(ControlReason::RepositoryNotGit));
    }
    let top = fs::canonicalize(String::from_utf8_lossy(&output.stdout).trim())
        .map_err(|error| format!("couldn't resolve the Git root: {error}"))?;
    if top != path {
        return Err(GitError::because(ControlReason::RepositoryNotRoot {
            root: top.display().to_string(),
        }));
    }
    Ok(path)
}

fn destination(project: &Path, name: &str) -> Result<PathBuf, GitError> {
    validate_name(name)?;
    let project =
        fs::canonicalize(project).map_err(|error| format!("couldn't read the project: {error}"))?;
    let repos = project.join("repos");
    fs::create_dir_all(&repos)
        .map_err(|error| format!("couldn't create the repos directory: {error}"))?;
    if fs::canonicalize(&repos).map_err(|error| error.to_string())? != repos {
        return Err("The project's repos directory can't be a symlink to somewhere else".into());
    }
    let target = repos.join(name);
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            return Err(GitError::because(ControlReason::RepositoryNameTaken {
                path: target.display().to_string(),
            }));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("couldn't check the target path: {error}").into()),
    }
    Ok(target)
}

pub fn bind(project: &Path, source: &Path, name: &str) -> Result<BoundRepository, GitError> {
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
        .map_err(|error| format!("couldn't create the symlink: {error}"))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&root, &target).map_err(|error| {
        format!("couldn't create the symlink; check Developer Mode or symlink permissions: {error}")
    })?;
    Ok(BoundRepository {
        root,
        managed_link: Some(target),
    })
}

pub fn clone_repository(
    project: &Path,
    url: &str,
    name: &str,
) -> Result<BoundRepository, GitError> {
    let url = url.trim();
    if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_control) {
        return Err(GitError::because(ControlReason::RepositoryUrlInvalid));
    }
    let target = destination(project, name)?;
    exclude_managed_directory(project, "repos")?;
    // Reserve the destination exclusively; Git accepts an existing empty directory.
    fs::create_dir(&target)
        .map_err(|error| format!("couldn't create the clone target: {error}"))?;
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
            // Never recursively delete a failed clone: it may contain useful data.
            let kept = match fs::remove_dir(&target) {
                Ok(()) => None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => Some(target.display().to_string()),
            };
            // Do not echo stderr: Git may include credentials embedded in the URL.
            Err(match result {
                Ok(_) => GitError::because(ControlReason::RepositoryCloneFailed { kept }),
                Err(error) => {
                    let mut message = format!("couldn't prepare or start the Git clone: {error}");
                    if let Some(kept) = kept {
                        message.push_str(&format!(
                            "; kept the clone directory {kept}. To retry, use another name or check that directory first"
                        ));
                    }
                    message.into()
                }
            })
        }
    }
}

pub fn remove_link(project: &Path, link: &Path, root: &Path) -> Result<(), String> {
    let project = fs::canonicalize(project).map_err(|error| error.to_string())?;
    if link.parent() != Some(project.join("repos").as_path()) {
        return Err(
            "The symlink isn't inside the project's repos directory; refusing to remove it".into(),
        );
    }
    let metadata = match fs::symlink_metadata(link) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if fs::canonicalize(project.join("repos")).map_err(|error| error.to_string())?
        != project.join("repos")
    {
        return Err("The repos directory was replaced by a symlink; refusing to delete".into());
    }
    if !metadata.file_type().is_symlink() || fs::read_link(link).map_err(|e| e.to_string())? != root
    {
        return Err(
            "The symlink was replaced or repointed; refusing to delete. Check it by hand".into(),
        );
    }
    #[cfg(unix)]
    fs::remove_file(link).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    fs::remove_dir(link).map_err(|error| error.to_string())?;
    Ok(())
}

pub fn exclude_managed_directory(project: &Path, name: &str) -> Result<(), GitError> {
    let project = fs::canonicalize(project).map_err(|error| error.to_string())?;
    if !project.join(".git").exists() {
        return Ok(());
    }
    let root = repository_root(&project)?;
    let output = crate::run_git(
        root.to_str().ok_or("the project path isn't UTF-8")?,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ],
    )
    .map_err(|error| error.to_string())?;
    if !output.success() {
        return Err("couldn't locate Git's local exclude file".into());
    }
    let exclude = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let existing = match fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string().into()),
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
