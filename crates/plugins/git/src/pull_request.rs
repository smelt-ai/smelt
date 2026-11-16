//! 由远端 URL 拼代码托管网站「新建 PR」页的链接。不依赖 `gh`：远程机器上的仓库、
//! 没装或没登录 `gh` 的环境都一样能用，标题和描述在网页上填。

/// 能认出来的托管网站。按主机名认：自建的 GitHub Enterprise / GitLab 主机名里通常也带
/// 这两个词；认不出来的不猜。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Forge {
    GitHub,
    GitLab,
}

/// `remote_url` 指向的仓库，从 `base` 往里合 `head` 的新建 PR 页。认不出远端时返回 `None`。
pub(crate) fn pull_request_url(remote_url: &str, base: &str, head: &str) -> Option<String> {
    let (host, path) = web_location(remote_url)?;
    let forge = if host.contains("github") {
        Forge::GitHub
    } else if host.contains("gitlab") {
        Forge::GitLab
    } else {
        return None;
    };
    let (base, head) = (encode_branch(base), encode_branch(head));
    Some(match forge {
        Forge::GitHub => format!("https://{host}/{path}/compare/{base}...{head}?expand=1"),
        Forge::GitLab => format!(
            "https://{host}/{path}/-/merge_requests/new?merge_request%5Bsource_branch%5D={head}&merge_request%5Btarget_branch%5D={base}"
        ),
    })
}

/// 远端 URL 对应的网页主机和仓库路径（不带 `.git`）。认得这几种写法：
/// `git@host:owner/repo.git`、`ssh://git@host:22/owner/repo.git`、
/// `https://user@host/owner/repo.git`。ssh 的端口不是网页的端口，丢掉；https 的保留。
fn web_location(remote_url: &str) -> Option<(String, String)> {
    let url = remote_url.trim();
    let (host, path) = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        let (authority, path) = rest.split_once('/')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        (host.to_string(), path)
    } else if let Some(rest) = url
        .strip_prefix("ssh://")
        .or_else(|| url.strip_prefix("git+ssh://"))
    {
        let (authority, path) = rest.split_once('/')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host.split_once(':').map_or(host, |(host, _)| host);
        (host.to_string(), path)
    } else {
        // scp 写法：[user@]host:path，冒号前没有 `/`。
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        (host.to_string(), path)
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && path.contains('/')).then(|| (host.to_ascii_lowercase(), path.to_string()))
}

/// 分支名放进 URL：`/` 照留（两家都认），其余非保留字符按 UTF-8 百分号编码。
fn encode_branch(branch: &str) -> String {
    let mut out = String::with_capacity(branch.len());
    for byte in branch.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::pull_request_url;

    #[test]
    fn github_remotes_in_every_form_open_the_compare_page() {
        let expected =
            Some("https://github.com/smelt-ai/smelt/compare/main...feat/x?expand=1".to_string());
        for remote in [
            "git@github.com:smelt-ai/smelt.git",
            "git@github.com:smelt-ai/smelt",
            "ssh://git@github.com:22/smelt-ai/smelt.git",
            "https://github.com/smelt-ai/smelt.git",
            "https://token@github.com/smelt-ai/smelt/",
        ] {
            assert_eq!(
                pull_request_url(remote, "main", "feat/x"),
                expected,
                "{remote}"
            );
        }
    }

    #[test]
    fn gitlab_keeps_nested_groups_and_the_https_port() {
        assert_eq!(
            pull_request_url(
                "https://gitlab.example.com:8443/group/sub/app.git",
                "develop",
                "fix/y"
            )
            .as_deref(),
            Some(
                "https://gitlab.example.com:8443/group/sub/app/-/merge_requests/new?merge_request%5Bsource_branch%5D=fix/y&merge_request%5Btarget_branch%5D=develop"
            )
        );
        assert!(
            pull_request_url(
                "ssh://git@gitlab.example.com:2222/group/app.git",
                "main",
                "a"
            )
            .unwrap()
            .starts_with("https://gitlab.example.com/group/app/-/")
        );
    }

    #[test]
    fn branch_names_are_percent_encoded_except_slashes() {
        assert_eq!(
            pull_request_url("git@github.com:o/r.git", "main", "feat/搜索 框#1").as_deref(),
            Some(
                "https://github.com/o/r/compare/main...feat/%E6%90%9C%E7%B4%A2%20%E6%A1%86%231?expand=1"
            )
        );
    }

    #[test]
    fn unknown_hosts_and_local_paths_give_no_link() {
        for remote in [
            "git@git.internal.corp:team/app.git",
            "/Users/me/remotes/app.git",
            "file:///srv/app.git",
            "https://github.com/only-owner",
        ] {
            assert_eq!(pull_request_url(remote, "main", "x"), None, "{remote}");
        }
    }
}
