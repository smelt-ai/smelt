//! Claude Code 本地存储路径规则：项目目录编码和 projects 根目录。
//! 历史会话扫描只用这一份，不许再复制编码规则。
//!
//! Claude Code 支持用 `CLAUDE_CONFIG_DIR` 环境变量把整个 `~/.claude` 换成
//! 别的目录（多账号场景常用）；没设就还是默认位置，绝大多数用户零行为变化。

use std::path::PathBuf;

/// 项目目录编码规则：Claude Code 把绝对路径里非 ASCII 字母/数字的字符都换成 `-`
/// （如 `/workspace/alice.smith/dev/project` → `-workspace-alice-smith-dev-project`，
/// Windows `E:\AI\smelt` → `E--AI-smelt`、`C:\Users\a\my_app` → `C--Users-a-my-app`）。
/// 先剥掉 Windows `\\?\` 逐字前缀与非根目录末尾的分隔符，保证带/不带尾斜杠和 `\\?\`
/// 前缀的工作区落到同一个编码目录。
pub fn project_dir(cwd: &str) -> String {
    normalize_project_cwd(cwd)
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn normalize_project_cwd(cwd: &str) -> String {
    let mut path = strip_verbatim_prefix(cwd);
    while path.ends_with(['/', '\\']) {
        let is_posix_root = path.len() == 1;
        let bytes = path.as_bytes();
        let is_drive_root = bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        if is_posix_root || is_drive_root {
            break;
        }
        path.pop();
    }
    path
}

fn strip_verbatim_prefix(cwd: &str) -> String {
    let Some(rest) = cwd
        .strip_prefix(r"\\?\")
        .or_else(|| cwd.strip_prefix("//?/"))
    else {
        return cwd.to_string();
    };
    if rest.get(..4).is_some_and(|prefix| {
        prefix.eq_ignore_ascii_case(r"UNC\") || prefix.eq_ignore_ascii_case("UNC/")
    }) {
        format!(r"\\{}", &rest[4..])
    } else if rest.len() >= 2
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && rest.as_bytes()[1] == b':'
    {
        rest.to_string()
    } else {
        cwd.to_string()
    }
}

/// `override_dir`：多 workspace 场景下某个 profile 显式指定的 `CLAUDE_CONFIG_DIR`
/// 覆盖值（已展开 `~`，见 `smelt_core::workspace_override`）——同一台机器可能
/// 同时用着好几个 Claude workspace（比如默认 `.claude` 和 `.claude-quant`），
/// 光靠进程全局环境变量分不清"当前要读哪一个"，调用方（历史会话页选中的
/// tab、ACP 续接预检用的启动命令）必须显式传进来。`None` = 走默认解析（进程
/// 环境变量 → 登录 shell 探测 → `~/.claude`），行为跟改造前完全一致。
pub fn projects_root(override_dir: Option<&str>) -> PathBuf {
    claude_home(override_dir).join("projects")
}

/// 默认解析下的 Claude Code 配置目录（通常是 `~/.claude`）。
pub fn config_home() -> PathBuf {
    claude_home(None)
}

fn claude_home(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    if let Some(dir) = crate::login_env::claude_config_dir() {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dir_replaces_slashes_and_dots() {
        assert_eq!(
            project_dir("/workspace/alice.smith/dev/project"),
            "-workspace-alice-smith-dev-project"
        );
    }

    #[test]
    fn project_dir_encodes_windows_paths_consistently() {
        assert_eq!(project_dir(r"E:\AI\smelt"), "E--AI-smelt");
        assert_eq!(project_dir("E:/AI/smelt"), "E--AI-smelt");
        assert_eq!(project_dir(r"E:\AI\smelt\"), "E--AI-smelt");
        assert_eq!(project_dir(r"\\?\E:\AI\smelt"), "E--AI-smelt");
        assert_eq!(project_dir(r"E:\"), "E--");
        assert_eq!(project_dir("E:/"), "E--");
        assert_eq!(
            project_dir(r"C:\Users\alice.smith\my_project"),
            "C--Users-alice-smith-my-project"
        );
        assert_eq!(
            project_dir(r"C:\Users\alice\my project"),
            "C--Users-alice-my-project"
        );
        assert_eq!(project_dir("/"), "-");
    }
}
