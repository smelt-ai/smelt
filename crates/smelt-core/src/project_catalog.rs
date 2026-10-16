//! Smelt 当前打开项目及其本地 Git checkout 目录。
//!
//! 本地路径只能来自用户在 Smelt 中明确打开过的项目，不扫描用户 home 或其它
//! 未授权目录。项目根可以是单个 Git checkout，也可以是包含多个直接子仓库的
//! workspace（例如 frontend + backend）。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct LocalProjectRegistry {
    pub projects: Vec<LocalProjectBinding>,
}

#[derive(Clone, Debug)]
pub struct LocalProjectBinding {
    /// 用户在 Smelt 中打开的项目根目录。
    pub root: String,
    /// 该项目根下已发现并校验过 remote 的 Git 仓库。
    pub repositories: Vec<LocalRepositoryBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRepositoryBinding {
    /// 仓库 checkout 的绝对路径。
    pub path: String,
    /// 主 canonical remote key：host/path，不包含凭据、scheme 和 `.git`。
    ///
    /// 作为该 checkout 的稳定身份，被 worktree 目录名与 workspace id 复用，
    /// 因此选取规则必须确定（`origin` > `upstream` > 其余按名称排序取首个）。
    pub remote_key: String,
    /// 该 checkout 上全部 remote 的 canonical key（含 [`Self::remote_key`]）。
    ///
    /// 一个仓库可以同时挂多个 remote（例如内部 GitLab + 公开镜像，且都不叫
    /// `origin`），远端资源引用其中任意一个都应当能匹配到本地 checkout。
    pub remote_keys: Vec<String>,
}

impl LocalRepositoryBinding {
    /// 用单个 remote key 构造（测试与外部调用方使用）。
    pub fn new(path: impl Into<String>, remote_key: impl Into<String>) -> Self {
        let remote_key = remote_key.into();
        Self {
            path: path.into(),
            remote_keys: vec![remote_key.clone()],
            remote_key,
        }
    }

    /// 任意一个 remote 命中即视为匹配。
    pub fn matches_remote_key(&self, wanted: &str) -> bool {
        self.remote_key == wanted || self.remote_keys.iter().any(|key| key == wanted)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositoryResolution {
    Matched(Vec<LocalRepositoryBinding>),
    Missing(Vec<String>),
    Ambiguous {
        remote_key: String,
        candidates: Vec<String>,
    },
}

/// 分别解析任务需要的每个 remote，不要求这些 checkout 位于同一个父目录。
/// 这允许用户把 A/B 作为两个独立的 Smelt 项目打开，任务仍能组成一个执行 workspace。
pub fn resolve_repositories(
    registry: &LocalProjectRegistry,
    repo_urls: &[String],
) -> RepositoryResolution {
    let mut wanted = Vec::new();
    let mut seen = BTreeSet::new();
    for key in repo_urls.iter().filter_map(|url| canonical_repo_key(url)) {
        if seen.insert(key.clone()) {
            wanted.push(key);
        }
    }
    if wanted.is_empty() {
        return RepositoryResolution::Missing(Vec::new());
    }

    let mut selected = Vec::new();
    for remote_key in wanted {
        let mut candidates = Vec::new();
        for project in &registry.projects {
            if !Path::new(&project.root).is_dir() {
                continue;
            }
            for repository in &project.repositories {
                if repository.matches_remote_key(&remote_key)
                    && !candidates
                        .iter()
                        .any(|candidate: &LocalRepositoryBinding| candidate.path == repository.path)
                {
                    candidates.push(repository.clone());
                }
            }
        }
        match candidates.len() {
            0 => return RepositoryResolution::Missing(vec![remote_key]),
            1 => selected.push(candidates.remove(0)),
            _ => {
                return RepositoryResolution::Ambiguous {
                    remote_key,
                    candidates: candidates
                        .into_iter()
                        .map(|candidate| candidate.path)
                        .collect(),
                };
            }
        }
    }
    RepositoryResolution::Matched(selected)
}

/// 返回某个已登记 checkout 所属的 Smelt 项目根。任务 cwd 使用这个根可以通过
/// “已明确打开项目”的授权校验，即使该 checkout 是多仓库 workspace 的子目录。
pub fn project_root_for_repository(
    registry: &LocalProjectRegistry,
    repository_path: &Path,
) -> Option<PathBuf> {
    let target = normalized_path(repository_path);
    registry.projects.iter().find_map(|project| {
        project
            .repositories
            .iter()
            .any(|repository| normalized_path(Path::new(&repository.path)) == target)
            .then(|| PathBuf::from(&project.root))
    })
}

/// 判断一个已登记项目是否是包含多个 Git checkout 的本地 workspace。
/// 该信息主要供原地执行/兼容逻辑做目录级并发判断；新 Issue 会为每个 repo 创建子 worktree。
pub fn is_multi_repository_project(registry: &LocalProjectRegistry, root: &Path) -> bool {
    if !root.is_dir() {
        return false;
    }
    let root = normalized_path(root);
    registry.projects.iter().any(|project| {
        project.repositories.len() > 1 && normalized_path(Path::new(&project.root)) == root
    })
}

fn normalized_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// 把 HTTPS、SSH 和 scp shorthand 统一为 host/path，保留 host 避免不同 Git 服务的
/// 同名仓库碰撞；同时丢弃 HTTPS URL 中可能存在的访问凭据。
pub fn canonical_repo_key(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let (host, path): (String, String) = if let Some(rest) = raw.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string())
    } else if raw.contains("://") {
        let url = url::Url::parse(raw).ok()?;
        (
            url.host_str()?.to_string(),
            url.path().trim_start_matches('/').to_string(),
        )
    } else {
        let (host, path) = raw.split_once('/')?;
        (host.to_string(), path.to_string())
    };

    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(&path)
        .trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    if host.trim().is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{path}", host.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("smelt-project-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn project(root: &str, repos: &[(&str, &str)]) -> LocalProjectBinding {
        LocalProjectBinding {
            root: root.into(),
            repositories: repos
                .iter()
                .map(|(path, remote_key)| LocalRepositoryBinding::new(*path, *remote_key))
                .collect(),
        }
    }

    #[test]
    fn matches_any_remote_when_repo_has_no_origin() {
        // 多 remote 且没有 origin：issue 引用其中任意一个 remote 都应匹配。
        let repository = LocalRepositoryBinding {
            path: "/tmp/smelt".into(),
            remote_key: "gitlab.example.com/acme/smelt".into(),
            remote_keys: vec![
                "gitlab.example.com/acme/smelt".into(),
                "github.com/smelt-ai/smelt".into(),
            ],
        };
        let registry = LocalProjectRegistry {
            projects: vec![LocalProjectBinding {
                root: "/tmp".into(),
                repositories: vec![repository.clone()],
            }],
        };
        assert!(repository.matches_remote_key("github.com/smelt-ai/smelt"));
        assert!(!repository.matches_remote_key("github.com/other/repo"));

        // 目录必须真实存在才会被纳入候选，这里只校验纯匹配函数。
        let _ = registry;
    }

    #[test]
    fn canonical_repo_key_handles_https_and_ssh() {
        assert_eq!(
            canonical_repo_key("https://token@gitlab.example.com/acme/smelt.git"),
            Some("gitlab.example.com/acme/smelt".into())
        );
        assert_eq!(
            canonical_repo_key("git@gitlab.example.com:acme/smelt.git"),
            Some("gitlab.example.com/acme/smelt".into())
        );
    }

    #[test]
    fn different_hosts_do_not_collide() {
        assert_ne!(
            canonical_repo_key("https://github.com/org/app"),
            canonical_repo_key("https://gitlab.com/org/app")
        );
    }

    #[test]
    fn resolves_all_repositories_in_one_open_project() {
        let root = temp_project("workspace");
        let registry = LocalProjectRegistry {
            projects: vec![project(
                root.to_str().unwrap(),
                &[
                    ("A", "git.example.com/org/a"),
                    ("B", "git.example.com/org/b"),
                ],
            )],
        };
        let urls = vec![
            "git@git.example.com:org/a.git".into(),
            "https://git.example.com/org/b".into(),
        ];
        assert!(matches!(
            resolve_repositories(&registry, &urls),
            RepositoryResolution::Matched(_)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn multiple_candidates_are_not_guessed() {
        let root_a = temp_project("a");
        let root_b = temp_project("b");
        let registry = LocalProjectRegistry {
            projects: vec![
                project(
                    root_a.to_str().unwrap(),
                    &[("A", "git.example.com/org/app")],
                ),
                project(
                    root_b.to_str().unwrap(),
                    &[("B", "git.example.com/org/app")],
                ),
            ],
        };
        let urls = vec!["https://git.example.com/org/app".into()];
        assert!(matches!(
            resolve_repositories(&registry, &urls),
            RepositoryResolution::Ambiguous { .. }
        ));
        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(root_b);
    }

    #[test]
    fn detects_multi_repository_workspace() {
        let root = temp_project("multi");
        let registry = LocalProjectRegistry {
            projects: vec![project(
                root.to_str().unwrap(),
                &[
                    ("A", "git.example.com/org/a"),
                    ("B", "git.example.com/org/b"),
                ],
            )],
        };
        assert!(is_multi_repository_project(&registry, &root));
        assert!(!is_multi_repository_project(&registry, &root.join("A")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_repositories_independently_across_open_projects() {
        let root_a = temp_project("separate-a");
        let root_b = temp_project("separate-b");
        let registry = LocalProjectRegistry {
            projects: vec![
                project(
                    root_a.to_str().unwrap(),
                    &[("/checkout/a", "git.example.com/org/a")],
                ),
                project(
                    root_b.to_str().unwrap(),
                    &[("/checkout/b", "git.example.com/org/b")],
                ),
            ],
        };
        let urls = vec![
            "https://git.example.com/org/a.git".into(),
            "git@git.example.com:org/b.git".into(),
        ];
        let RepositoryResolution::Matched(repositories) = resolve_repositories(&registry, &urls)
        else {
            panic!("A/B 应分别匹配到两个已打开项目");
        };
        assert_eq!(
            repositories
                .iter()
                .map(|repository| repository.remote_key.as_str())
                .collect::<Vec<_>>(),
            vec!["git.example.com/org/a", "git.example.com/org/b"]
        );
        assert_eq!(
            project_root_for_repository(&registry, Path::new("/checkout/a")),
            Some(root_a.clone())
        );
        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(root_b);
    }

    #[test]
    fn repository_resolution_reports_ambiguous_single_remote() {
        let root_a = temp_project("ambiguous-a");
        let root_b = temp_project("ambiguous-b");
        let registry = LocalProjectRegistry {
            projects: vec![
                project(
                    root_a.to_str().unwrap(),
                    &[("/checkout/a", "git.example.com/org/app")],
                ),
                project(
                    root_b.to_str().unwrap(),
                    &[("/checkout/b", "git.example.com/org/app")],
                ),
            ],
        };
        assert!(matches!(
            resolve_repositories(&registry, &["https://git.example.com/org/app".into()]),
            RepositoryResolution::Ambiguous { .. }
        ));
        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(root_b);
    }
}
