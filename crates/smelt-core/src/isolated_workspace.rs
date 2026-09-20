//! Plugin-owned isolated Git workspaces.

use crate::project_catalog::LocalRepositoryBinding;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use smelt_plugin_api::PluginResourceRef;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

const MANIFEST_VERSION: u16 = 1;
const MANIFEST_FILE: &str = "workspace.json";
const INSTRUCTIONS_FILE: &str = "AGENTS.md";

static WORKSPACE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn workspace_lock() -> &'static Mutex<()> {
    WORKSPACE_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedWorkspace {
    pub id: String,
    pub owner: PluginResourceRef,
    pub branch_label: String,
    pub workspace_dir: String,
    pub repositories: Vec<IsolatedWorkspaceRepository>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedWorkspaceRepository {
    pub remote_key: String,
    pub repo_root: String,
    pub worktree_dir: String,
    pub branch_name: String,
    pub base_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateIsolatedWorkspaceResult {
    pub workspace: IsolatedWorkspace,
    pub created: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsolatedWorkspaceErrorKind {
    Invalid,
    NotFound,
    Conflict,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsolatedWorkspaceError {
    kind: IsolatedWorkspaceErrorKind,
    message: String,
}

impl IsolatedWorkspaceError {
    pub fn kind(&self) -> IsolatedWorkspaceErrorKind {
        self.kind
    }

    fn new(kind: IsolatedWorkspaceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(IsolatedWorkspaceErrorKind::Invalid, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(IsolatedWorkspaceErrorKind::Conflict, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(IsolatedWorkspaceErrorKind::Internal, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(IsolatedWorkspaceErrorKind::NotFound, message)
    }
}

impl fmt::Display for IsolatedWorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IsolatedWorkspaceError {}

#[derive(Clone, Debug)]
pub struct IsolatedWorkspaceStore {
    root: PathBuf,
}

impl IsolatedWorkspaceStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn for_current_user() -> Result<Self, IsolatedWorkspaceError> {
        let root = smelt_paths::smelt_home()
            .map(|root| root.join("workspaces"))
            .ok_or_else(|| {
                IsolatedWorkspaceError::internal("cannot determine Smelt workspace directory")
            })?;
        Ok(Self::new(root))
    }

    pub fn create(
        &self,
        owner: &PluginResourceRef,
        repositories: &[LocalRepositoryBinding],
        branch_label: &str,
    ) -> Result<CreateIsolatedWorkspaceResult, IsolatedWorkspaceError> {
        let branch_label = validate_branch_label(branch_label)?;
        let repositories = prepare_repositories(repositories)?;
        let _guard = workspace_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let id = workspace_id(owner);
        let workspace_dir = self.root.join(&id);

        if workspace_dir.exists() {
            if !workspace_dir.is_dir() {
                return Err(IsolatedWorkspaceError::conflict(format!(
                    "isolated workspace path is not a directory: {}",
                    workspace_dir.display()
                )));
            }
            let workspace = load_manifest(&workspace_dir)?;
            validate_definition(
                &workspace,
                owner,
                branch_label,
                &repositories,
                &workspace_dir,
            )?;
            validate_workspace(&workspace, &workspace_dir)?;
            return Ok(CreateIsolatedWorkspaceResult {
                workspace,
                created: false,
            });
        }

        let repos_dir = workspace_dir.join("repos");
        fs::create_dir_all(&repos_dir).map_err(|error| {
            IsolatedWorkspaceError::internal(format!(
                "cannot create isolated workspace {}: {error}",
                repos_dir.display()
            ))
        })?;

        let mut created = Vec::new();
        let result = (|| {
            let mut workspace_repositories = Vec::with_capacity(repositories.len());
            for repository in &repositories {
                let repository_id =
                    workspace_repository_id(&id, &repository.repo_root, &repository.remote_key);
                let branch_name = workspace_branch_name(branch_label, &repository_id);
                let worktree_dir = repos_dir.join(workspace_repository_dir_name(
                    &repository.remote_key,
                    &repository_id,
                ));
                if worktree_dir.exists() {
                    return Err(IsolatedWorkspaceError::conflict(format!(
                        "isolated worktree path already exists: {}",
                        worktree_dir.display()
                    )));
                }
                if git_branch_exists(&repository.repo_root, &branch_name)? {
                    return Err(IsolatedWorkspaceError::conflict(format!(
                        "isolated workspace branch already exists without its manifest: {branch_name}"
                    )));
                }
                let (base_commit, base_ref) = resolve_base_commit(repository)?;
                let mut command = Command::new("git");
                command
                    .arg("-C")
                    .arg(&repository.repo_root)
                    .args(["worktree", "add", "-b"])
                    .arg(&branch_name)
                    .arg(&worktree_dir)
                    .arg(&base_commit);
                run_git(command, "create isolated worktree")?;
                created.push(CreatedWorktree {
                    repo_root: repository.repo_root.clone(),
                    worktree_dir: worktree_dir.clone(),
                    branch_name: branch_name.clone(),
                });
                workspace_repositories.push(IsolatedWorkspaceRepository {
                    remote_key: repository.remote_key.clone(),
                    repo_root: repository.repo_root.to_string_lossy().into_owned(),
                    worktree_dir: worktree_dir.to_string_lossy().into_owned(),
                    branch_name,
                    base_ref,
                });
            }

            let workspace = IsolatedWorkspace {
                id,
                owner: owner.clone(),
                branch_label: branch_label.to_string(),
                workspace_dir: workspace_dir.to_string_lossy().into_owned(),
                repositories: workspace_repositories,
            };
            write_instructions(&workspace)?;
            write_manifest(&workspace_dir, &workspace)?;
            Ok(CreateIsolatedWorkspaceResult {
                workspace,
                created: true,
            })
        })();

        if result.is_err() {
            rollback_created_worktrees(&created);
            if let Err(error) = fs::remove_dir_all(&workspace_dir)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!(
                    "[isolated-workspace] cannot remove failed workspace {}: {error}",
                    workspace_dir.display()
                );
            }
        }
        result
    }

    pub fn release(&self, owner: &PluginResourceRef) -> Result<bool, IsolatedWorkspaceError> {
        let _guard = workspace_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let workspace_dir = self.root.join(workspace_id(owner));
        if !workspace_dir.exists() {
            return Ok(false);
        }
        if !workspace_dir.is_dir() {
            return Err(IsolatedWorkspaceError::conflict(format!(
                "isolated workspace path is not a directory: {}",
                workspace_dir.display()
            )));
        }
        let workspace = load_manifest(&workspace_dir)?;
        validate_identity(&workspace, owner, &workspace_dir)?;
        validate_owned_entries(&workspace, &workspace_dir)?;

        for repository in &workspace.repositories {
            let worktree_dir = Path::new(&repository.worktree_dir);
            if !worktree_dir.exists() {
                continue;
            }
            validate_workspace_repository(repository, &workspace, &workspace_dir)?;
            if git_worktree_is_dirty(worktree_dir)? {
                return Err(IsolatedWorkspaceError::conflict(format!(
                    "isolated worktree contains uncommitted files: {}",
                    worktree_dir.display()
                )));
            }
        }

        for repository in &workspace.repositories {
            let worktree_dir = Path::new(&repository.worktree_dir);
            if !worktree_dir.exists() {
                continue;
            }
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(&repository.repo_root)
                .args(["worktree", "remove"])
                .arg(worktree_dir);
            run_git(command, "release isolated worktree")?;
        }

        remove_workspace_container(&workspace_dir)?;
        Ok(true)
    }

    pub fn load(
        &self,
        owner: &PluginResourceRef,
    ) -> Result<Option<IsolatedWorkspace>, IsolatedWorkspaceError> {
        let _guard = workspace_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let workspace_dir = self.root.join(workspace_id(owner));
        if !workspace_dir.exists() {
            return Ok(None);
        }
        if !workspace_dir.is_dir() {
            return Err(IsolatedWorkspaceError::conflict(format!(
                "isolated workspace path is not a directory: {}",
                workspace_dir.display()
            )));
        }
        let workspace = load_manifest(&workspace_dir)?;
        validate_identity(&workspace, owner, &workspace_dir)?;
        validate_workspace(&workspace, &workspace_dir)?;
        Ok(Some(workspace))
    }

    pub fn contains(&self, workspace_dir: &Path) -> bool {
        if !workspace_dir.is_dir()
            || !same_path(workspace_dir, &self.root.join(file_name(workspace_dir)))
        {
            return false;
        }
        load_manifest(workspace_dir).is_ok_and(|workspace| {
            workspace.id == file_name(workspace_dir)
                && workspace_id(&workspace.owner) == workspace.id
                && same_path(Path::new(&workspace.workspace_dir), workspace_dir)
        })
    }
}

pub fn is_managed_workspace_dir(workspace_dir: &Path) -> bool {
    IsolatedWorkspaceStore::for_current_user().is_ok_and(|store| store.contains(workspace_dir))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceManifest {
    version: u16,
    workspace: IsolatedWorkspace,
}

#[derive(Clone)]
struct PreparedRepository {
    remote_key: String,
    repo_root: PathBuf,
    remote_keys: Vec<String>,
}

struct CreatedWorktree {
    repo_root: PathBuf,
    worktree_dir: PathBuf,
    branch_name: String,
}

fn validate_branch_label(branch_label: &str) -> Result<&str, IsolatedWorkspaceError> {
    let branch_label = branch_label.trim();
    if branch_label.is_empty()
        || branch_label.len() > 128
        || branch_label.chars().any(char::is_control)
    {
        return Err(IsolatedWorkspaceError::invalid(
            "isolated workspace branch_label is invalid",
        ));
    }
    Ok(branch_label)
}

fn prepare_repositories(
    repositories: &[LocalRepositoryBinding],
) -> Result<Vec<PreparedRepository>, IsolatedWorkspaceError> {
    if repositories.is_empty() {
        return Err(IsolatedWorkspaceError::invalid(
            "isolated workspace requires at least one repository",
        ));
    }
    let mut prepared = Vec::new();
    let mut identities = BTreeSet::new();
    for repository in repositories {
        if repository.remote_key.trim().is_empty() {
            return Err(IsolatedWorkspaceError::invalid(
                "isolated workspace repository has no remote key",
            ));
        }
        let checkout = Path::new(&repository.path);
        if !checkout.is_dir() {
            return Err(IsolatedWorkspaceError::not_found(format!(
                "repository checkout is unavailable: {}",
                checkout.display()
            )));
        }
        let top_level = git_top_level(checkout)?;
        let repo_root = git_common_root(&top_level).unwrap_or(top_level);
        let identity = (
            repository.remote_key.clone(),
            repo_root.to_string_lossy().into_owned(),
        );
        if !identities.insert(identity) {
            continue;
        }
        prepared.push(PreparedRepository {
            remote_key: repository.remote_key.clone(),
            repo_root,
            remote_keys: repository.remote_keys.clone(),
        });
    }
    prepared.sort_by(|left, right| {
        left.remote_key
            .cmp(&right.remote_key)
            .then_with(|| left.repo_root.cmp(&right.repo_root))
    });
    for pair in prepared.windows(2) {
        if pair[0].remote_key == pair[1].remote_key {
            return Err(IsolatedWorkspaceError::conflict(format!(
                "repository remote matches multiple checkouts: {}",
                pair[0].remote_key
            )));
        }
    }
    Ok(prepared)
}

fn validate_definition(
    workspace: &IsolatedWorkspace,
    owner: &PluginResourceRef,
    branch_label: &str,
    repositories: &[PreparedRepository],
    workspace_dir: &Path,
) -> Result<(), IsolatedWorkspaceError> {
    validate_identity(workspace, owner, workspace_dir)?;
    if workspace.branch_label != branch_label {
        return Err(IsolatedWorkspaceError::conflict(
            "isolated workspace owner is already bound to a different branch_label",
        ));
    }
    let expected = repositories
        .iter()
        .map(|repository| {
            (
                repository.remote_key.as_str(),
                normalized_path(&repository.repo_root),
            )
        })
        .collect::<Vec<_>>();
    let actual = workspace
        .repositories
        .iter()
        .map(|repository| {
            (
                repository.remote_key.as_str(),
                normalized_path(Path::new(&repository.repo_root)),
            )
        })
        .collect::<Vec<_>>();
    if expected != actual {
        return Err(IsolatedWorkspaceError::conflict(
            "isolated workspace owner is already bound to a different repository set",
        ));
    }
    Ok(())
}

fn validate_identity(
    workspace: &IsolatedWorkspace,
    owner: &PluginResourceRef,
    workspace_dir: &Path,
) -> Result<(), IsolatedWorkspaceError> {
    let expected_id = workspace_id(owner);
    if workspace.owner != *owner
        || workspace.id != expected_id
        || workspace_dir.file_name().and_then(|name| name.to_str()) != Some(expected_id.as_str())
        || !same_path(Path::new(&workspace.workspace_dir), workspace_dir)
    {
        return Err(IsolatedWorkspaceError::conflict(
            "isolated workspace manifest does not match its owner or directory",
        ));
    }
    Ok(())
}

fn validate_workspace(
    workspace: &IsolatedWorkspace,
    workspace_dir: &Path,
) -> Result<(), IsolatedWorkspaceError> {
    if workspace.repositories.is_empty() {
        return Err(IsolatedWorkspaceError::conflict(
            "isolated workspace manifest has no repositories",
        ));
    }
    for repository in &workspace.repositories {
        validate_workspace_repository(repository, workspace, workspace_dir)?;
    }
    Ok(())
}

fn validate_workspace_repository(
    repository: &IsolatedWorkspaceRepository,
    workspace: &IsolatedWorkspace,
    workspace_dir: &Path,
) -> Result<(), IsolatedWorkspaceError> {
    let repository_id = workspace_repository_id(
        &workspace.id,
        Path::new(&repository.repo_root),
        &repository.remote_key,
    );
    let expected_dir = workspace_dir
        .join("repos")
        .join(workspace_repository_dir_name(
            &repository.remote_key,
            &repository_id,
        ));
    let worktree_dir = Path::new(&repository.worktree_dir);
    if !worktree_dir.is_dir() || !same_path(worktree_dir, &expected_dir) {
        return Err(IsolatedWorkspaceError::conflict(format!(
            "isolated worktree is missing or outside its workspace: {}",
            worktree_dir.display()
        )));
    }
    let actual_top_level = git_top_level(worktree_dir)?;
    let actual_root = git_common_root(&actual_top_level).unwrap_or(actual_top_level);
    if !same_path(&actual_root, Path::new(&repository.repo_root)) {
        return Err(IsolatedWorkspaceError::conflict(format!(
            "isolated worktree belongs to a different repository: {}",
            worktree_dir.display()
        )));
    }
    if current_branch(worktree_dir).as_deref() != Some(repository.branch_name.as_str()) {
        return Err(IsolatedWorkspaceError::conflict(format!(
            "isolated worktree branch was changed: {}",
            worktree_dir.display()
        )));
    }
    Ok(())
}

fn validate_owned_entries(
    workspace: &IsolatedWorkspace,
    workspace_dir: &Path,
) -> Result<(), IsolatedWorkspaceError> {
    let allowed_root = BTreeSet::from([
        MANIFEST_FILE.to_string(),
        INSTRUCTIONS_FILE.to_string(),
        "repos".to_string(),
    ]);
    for entry in read_directory(workspace_dir)? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !allowed_root.contains(&name) {
            return Err(IsolatedWorkspaceError::conflict(format!(
                "isolated workspace contains an unmanaged entry: {}",
                entry.path().display()
            )));
        }
    }
    let repos_dir = workspace_dir.join("repos");
    let allowed_repositories = workspace
        .repositories
        .iter()
        .filter_map(|repository| {
            Path::new(&repository.worktree_dir)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect::<BTreeSet<_>>();
    if repos_dir.exists() {
        for entry in read_directory(&repos_dir)? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !allowed_repositories.contains(&name) {
                return Err(IsolatedWorkspaceError::conflict(format!(
                    "isolated workspace contains an unmanaged repository entry: {}",
                    entry.path().display()
                )));
            }
        }
    }
    Ok(())
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, IsolatedWorkspaceError> {
    fs::read_dir(path)
        .map_err(|error| {
            IsolatedWorkspaceError::internal(format!(
                "cannot read isolated workspace directory {}: {error}",
                path.display()
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            IsolatedWorkspaceError::internal(format!(
                "cannot inspect isolated workspace directory {}: {error}",
                path.display()
            ))
        })
}

fn load_manifest(workspace_dir: &Path) -> Result<IsolatedWorkspace, IsolatedWorkspaceError> {
    let path = workspace_dir.join(MANIFEST_FILE);
    let raw = fs::read(&path).map_err(|error| {
        let kind = if error.kind() == std::io::ErrorKind::NotFound {
            IsolatedWorkspaceErrorKind::Conflict
        } else {
            IsolatedWorkspaceErrorKind::Internal
        };
        IsolatedWorkspaceError::new(
            kind,
            format!(
                "cannot read isolated workspace manifest {}: {error}",
                path.display()
            ),
        )
    })?;
    let manifest: WorkspaceManifest = serde_json::from_slice(&raw).map_err(|error| {
        IsolatedWorkspaceError::conflict(format!(
            "isolated workspace manifest is invalid {}: {error}",
            path.display()
        ))
    })?;
    if manifest.version != MANIFEST_VERSION {
        return Err(IsolatedWorkspaceError::conflict(format!(
            "unsupported isolated workspace manifest version {}",
            manifest.version
        )));
    }
    Ok(manifest.workspace)
}

fn write_manifest(
    workspace_dir: &Path,
    workspace: &IsolatedWorkspace,
) -> Result<(), IsolatedWorkspaceError> {
    let path = workspace_dir.join(MANIFEST_FILE);
    let temporary = workspace_dir.join(format!(
        ".{MANIFEST_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let encoded = serde_json::to_vec_pretty(&WorkspaceManifest {
        version: MANIFEST_VERSION,
        workspace: workspace.clone(),
    })
    .map_err(|error| {
        IsolatedWorkspaceError::internal(format!(
            "cannot encode isolated workspace manifest: {error}"
        ))
    })?;
    let write_result = (|| -> Result<(), IsolatedWorkspaceError> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                IsolatedWorkspaceError::internal(format!(
                    "cannot create isolated workspace manifest {}: {error}",
                    temporary.display()
                ))
            })?;
        file.write_all(&encoded).map_err(|error| {
            IsolatedWorkspaceError::internal(format!(
                "cannot write isolated workspace manifest {}: {error}",
                temporary.display()
            ))
        })?;
        file.sync_all().map_err(|error| {
            IsolatedWorkspaceError::internal(format!(
                "cannot sync isolated workspace manifest {}: {error}",
                temporary.display()
            ))
        })?;
        fs::rename(&temporary, &path).map_err(|error| {
            IsolatedWorkspaceError::internal(format!(
                "cannot commit isolated workspace manifest {}: {error}",
                path.display()
            ))
        })
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    write_result
}

fn write_instructions(workspace: &IsolatedWorkspace) -> Result<(), IsolatedWorkspaceError> {
    let path = Path::new(&workspace.workspace_dir).join(INSTRUCTIONS_FILE);
    let mut instructions = String::from(concat!(
        "# Smelt isolated workspace\n\n",
        "This directory is a task workspace, not a Git repository. Each directory under `repos/` is an independent Git worktree.\n\n",
        "Use `git -C <repo-dir> ...` for Git commands and do not modify the source checkouts.\n\n",
    ));
    for repository in &workspace.repositories {
        instructions.push_str(&format!(
            "- `{}`: `{}`\n",
            repository.remote_key, repository.worktree_dir
        ));
    }
    fs::write(&path, instructions).map_err(|error| {
        IsolatedWorkspaceError::internal(format!(
            "cannot write isolated workspace instructions {}: {error}",
            path.display()
        ))
    })
}

fn remove_workspace_container(workspace_dir: &Path) -> Result<(), IsolatedWorkspaceError> {
    let repos_dir = workspace_dir.join("repos");
    if let Err(error) = fs::remove_dir(&repos_dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(IsolatedWorkspaceError::conflict(format!(
            "cannot remove isolated workspace repository directory {}: {error}",
            repos_dir.display()
        )));
    }
    for path in [
        workspace_dir.join(INSTRUCTIONS_FILE),
        workspace_dir.join(MANIFEST_FILE),
    ] {
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(IsolatedWorkspaceError::internal(format!(
                "cannot remove isolated workspace file {}: {error}",
                path.display()
            )));
        }
    }
    fs::remove_dir(workspace_dir).map_err(|error| {
        IsolatedWorkspaceError::conflict(format!(
            "cannot remove isolated workspace directory {}: {error}",
            workspace_dir.display()
        ))
    })
}

fn rollback_created_worktrees(created: &[CreatedWorktree]) {
    for worktree in created.iter().rev() {
        let mut remove = Command::new("git");
        remove
            .arg("-C")
            .arg(&worktree.repo_root)
            .args(["worktree", "remove", "--force"])
            .arg(&worktree.worktree_dir);
        if let Err(error) = run_git(remove, "roll back isolated worktree") {
            eprintln!("[isolated-workspace] {error}");
            continue;
        }
        let mut branch = Command::new("git");
        branch
            .arg("-C")
            .arg(&worktree.repo_root)
            .args(["branch", "-D"])
            .arg(&worktree.branch_name);
        if let Err(error) = run_git(branch, "roll back isolated workspace branch") {
            eprintln!("[isolated-workspace] {error}");
        }
    }
}

fn workspace_id(owner: &PluginResourceRef) -> String {
    short_hash(&[
        owner.plugin_id.as_str(),
        owner.resource_type.as_str(),
        owner.resource_id.as_str(),
    ])
}

fn workspace_repository_id(workspace_id: &str, repo_root: &Path, remote_key: &str) -> String {
    short_hash(&[workspace_id, &repo_root.to_string_lossy(), remote_key])
}

fn short_hash(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"smelt.isolated-workspace.v1\0");
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    let encoded = format!("{:x}", digest.finalize());
    encoded[..20].to_string()
}

fn workspace_repository_dir_name(remote_key: &str, repository_id: &str) -> String {
    let name = remote_key
        .rsplit('/')
        .next()
        .map(|name| name.strip_suffix(".git").unwrap_or(name))
        .unwrap_or("repo");
    format!("{}-{}", branch_component(name), &repository_id[..8])
}

fn workspace_branch_name(branch_label: &str, repository_id: &str) -> String {
    format!(
        "smelt-{}-{}",
        branch_component(branch_label),
        &repository_id[..8]
    )
}

fn branch_component(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character == '.' && output.ends_with('.') {
            continue;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
            output.push(character);
        } else {
            output.push('-');
        }
        if output.len() >= 80 {
            break;
        }
    }
    let mut output = output.trim_matches('.').to_string();
    if output.ends_with(".lock") {
        output.truncate(output.len() - ".lock".len());
        output.push_str("-lock");
    }
    if output.is_empty() {
        "workspace".to_string()
    } else {
        output
    }
}

fn git_top_level(path: &Path) -> Result<PathBuf, IsolatedWorkspaceError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"]);
    let output = run_git_output(&mut command, "resolve repository root")?;
    let root = PathBuf::from(output.trim());
    root.canonicalize().map_err(|error| {
        IsolatedWorkspaceError::internal(format!(
            "cannot canonicalize repository root {}: {error}",
            root.display()
        ))
    })
}

fn git_common_root(worktree_path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let common_dir = PathBuf::from(&raw);
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        worktree_path.join(common_dir)
    };
    let common_dir = common_dir.canonicalize().ok()?;
    (common_dir.file_name().and_then(|name| name.to_str()) == Some(".git"))
        .then(|| common_dir.parent().map(Path::to_path_buf))
        .flatten()
}

fn current_branch(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty() && branch != "HEAD").then_some(branch)
}

fn resolve_base_commit(
    repository: &PreparedRepository,
) -> Result<(String, String), IsolatedWorkspaceError> {
    let Some(remote) = primary_remote(repository) else {
        return Err(IsolatedWorkspaceError::conflict(format!(
            "cannot identify the Git remote for {}",
            repository.remote_key
        )));
    };
    let symbolic = format!("refs/remotes/{remote}/HEAD");
    let output = Command::new("git")
        .arg("-C")
        .arg(&repository.repo_root)
        .args(["symbolic-ref", &symbolic])
        .output()
        .ok();
    let default_ref = output
        .filter(|output| output.status.success())
        .and_then(|output| {
            let reference = String::from_utf8_lossy(&output.stdout).trim().to_string();
            reference.strip_prefix("refs/remotes/").map(str::to_string)
        })
        .or_else(|| {
            [format!("{remote}/main"), format!("{remote}/master")]
                .into_iter()
                .find(|reference| git_rev_parse_commit(&repository.repo_root, reference).is_some())
        })
        .ok_or_else(|| {
            IsolatedWorkspaceError::conflict(format!(
                "cannot resolve the default branch for repository {}",
                repository.repo_root.display()
            ))
        })?;
    let commit = git_rev_parse_commit(&repository.repo_root, &default_ref).ok_or_else(|| {
        IsolatedWorkspaceError::conflict(format!(
            "default branch {default_ref} does not resolve to a commit"
        ))
    })?;
    Ok((commit, default_ref))
}

fn primary_remote(repository: &PreparedRepository) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&repository.repo_root)
        .arg("remote")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut names = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.sort_by_key(|name| {
        let rank = match name.as_str() {
            "origin" => 0,
            "upstream" => 1,
            _ => 2,
        };
        (rank, name.clone())
    });
    let fallback = names.first().cloned();
    names
        .into_iter()
        .find(|name| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&repository.repo_root)
                .args(["remote", "get-url", name])
                .output();
            let Ok(output) = output else {
                return false;
            };
            if !output.status.success() {
                return false;
            }
            let url = String::from_utf8_lossy(&output.stdout);
            crate::project_catalog::canonical_repo_key(&url).is_some_and(|key| {
                key == repository.remote_key || repository.remote_keys.contains(&key)
            })
        })
        .or(fallback)
}

fn git_rev_parse_commit(path: &Path, reference: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{reference}^{{commit}}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

fn git_branch_exists(path: &Path, branch: &str) -> Result<bool, IsolatedWorkspaceError> {
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .status()
        .map_err(|error| {
            IsolatedWorkspaceError::internal(format!(
                "cannot check isolated workspace branch {branch}: {error}"
            ))
        })?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(IsolatedWorkspaceError::internal(format!(
            "cannot check isolated workspace branch {branch}"
        ))),
    }
}

fn git_worktree_is_dirty(path: &Path) -> Result<bool, IsolatedWorkspaceError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain", "--untracked-files=all"]);
    Ok(!run_git_output(&mut command, "inspect isolated worktree")?
        .trim()
        .is_empty())
}

fn run_git(mut command: Command, operation: &str) -> Result<(), IsolatedWorkspaceError> {
    let output = command.output().map_err(|error| {
        IsolatedWorkspaceError::internal(format!("cannot run Git to {operation}: {error}"))
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(IsolatedWorkspaceError::conflict(format!(
        "cannot {operation}{}",
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    )))
}

fn run_git_output(
    command: &mut Command,
    operation: &str,
) -> Result<String, IsolatedWorkspaceError> {
    let output = command.output().map_err(|error| {
        IsolatedWorkspaceError::internal(format!("cannot run Git to {operation}: {error}"))
    })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(IsolatedWorkspaceError::conflict(format!(
        "cannot {operation}{}",
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    )))
}

fn same_path(left: &Path, right: &Path) -> bool {
    normalized_path(left) == normalized_path(right)
}

fn normalized_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::{PluginId, PluginResourceId, PluginResourceType};
    use std::fs;
    use std::process::Command;

    fn owner(resource_id: &str) -> PluginResourceRef {
        PluginResourceRef {
            plugin_id: PluginId::new("com.example.workspace").unwrap(),
            resource_type: PluginResourceType::new("issue").unwrap(),
            resource_id: PluginResourceId::new(resource_id).unwrap(),
        }
    }

    fn fixture() -> (PathBuf, PathBuf, IsolatedWorkspaceStore) {
        let root =
            std::env::temp_dir().join(format!("smelt-isolated-workspace-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let upstream = root.join("upstream.git");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&upstream).unwrap();
        run_git(&upstream, &["init", "--quiet", "--bare", "-b", "main"]);
        run_git(&source, &["init", "--quiet"]);
        run_git(&source, &["config", "user.email", "smelt-test@example.com"]);
        run_git(&source, &["config", "user.name", "Smelt Test"]);
        fs::write(source.join("README.md"), "base\n").unwrap();
        run_git(&source, &["add", "README.md"]);
        run_git(&source, &["commit", "--quiet", "-m", "initial"]);
        run_git(&source, &["branch", "-M", "main"]);
        run_git(
            &source,
            &["remote", "add", "origin", upstream.to_str().unwrap()],
        );
        run_git(&source, &["push", "--quiet", "-u", "origin", "main"]);
        run_git(&source, &["remote", "set-head", "origin", "--auto"]);
        let store = IsolatedWorkspaceStore::new(root.join("workspaces"));
        (root, source, store)
    }

    fn repository(source: &Path, remote_key: &str) -> LocalRepositoryBinding {
        LocalRepositoryBinding::new(source.to_string_lossy().into_owned(), remote_key)
    }

    fn run_git(path: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap()
                .success(),
            "git {args:?} failed"
        );
    }

    #[test]
    fn create_is_idempotent_but_rejects_a_changed_definition() {
        let (root, source, store) = fixture();
        let owner = owner("issue-1");
        let repositories = vec![repository(&source, "example.com/team/repo")];

        let first = store.create(&owner, &repositories, "MUL-42").unwrap();
        assert!(first.created);
        assert!(
            Path::new(&first.workspace.workspace_dir)
                .join("workspace.json")
                .is_file()
        );
        assert_eq!(store.load(&owner).unwrap(), Some(first.workspace.clone()));

        let second = store.create(&owner, &repositories, "MUL-42").unwrap();
        assert!(!second.created);
        assert_eq!(second.workspace, first.workspace);

        let changed_label = store.create(&owner, &repositories, "MUL-43").unwrap_err();
        assert_eq!(changed_label.kind(), IsolatedWorkspaceErrorKind::Conflict);

        let changed_repositories = store
            .create(
                &owner,
                &[repository(&source, "example.com/team/other")],
                "MUL-42",
            )
            .unwrap_err();
        assert_eq!(
            changed_repositories.kind(),
            IsolatedWorkspaceErrorKind::Conflict
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn release_refuses_dirty_worktrees_and_is_idempotent() {
        let (root, source, store) = fixture();
        let owner = owner("issue-2");
        let created = store
            .create(
                &owner,
                &[repository(&source, "example.com/team/repo")],
                "MUL-44",
            )
            .unwrap();
        let worktree = Path::new(&created.workspace.repositories[0].worktree_dir);
        fs::write(worktree.join("dirty.txt"), "do not delete\n").unwrap();

        let dirty = store.release(&owner).unwrap_err();
        assert_eq!(dirty.kind(), IsolatedWorkspaceErrorKind::Conflict);
        assert!(worktree.is_dir());

        fs::remove_file(worktree.join("dirty.txt")).unwrap();
        assert!(store.release(&owner).unwrap());
        assert!(!Path::new(&created.workspace.workspace_dir).exists());
        assert!(!store.release(&owner).unwrap());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn root_outputs_do_not_break_loading_but_block_release() {
        let (root, source, store) = fixture();
        let owner = owner("issue-4");
        let created = store
            .create(
                &owner,
                &[repository(&source, "example.com/team/repo")],
                "MUL-46",
            )
            .unwrap();
        let output = Path::new(&created.workspace.workspace_dir).join("result.txt");
        fs::write(&output, "keep me\n").unwrap();

        assert_eq!(store.load(&owner).unwrap(), Some(created.workspace));
        assert_eq!(
            store.release(&owner).unwrap_err().kind(),
            IsolatedWorkspaceErrorKind::Conflict
        );
        assert!(output.is_file());
        fs::remove_dir_all(root).ok();
    }
}
