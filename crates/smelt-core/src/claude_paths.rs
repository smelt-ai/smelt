//! Claude Code 本地存储路径规则：项目目录编码和 projects 根目录。
//! 历史会话扫描只用这一份，不许再复制编码规则。
//!
//! Claude Code 支持用 `CLAUDE_CONFIG_DIR` 环境变量把整个 `~/.claude` 换成
//! 别的目录（多账号场景常用）；没设就还是默认位置，绝大多数用户零行为变化。

use std::path::PathBuf;

/// 项目目录编码规则：Claude Code 把项目路径里的 `/` 和 `.` 都换成 `-`
/// （已经拿 codux 的实现 `project_path.replace('/', '-').replace('.', '-')` 印证过，
/// 跟本机实测的编码目录名完全对得上）。
pub fn project_dir(cwd: &str) -> String {
    cwd.replace(['/', '.'], "-")
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

fn claude_home(override_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    if let Some(dir) = crate::login_env::claude_config_dir() {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
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
}
