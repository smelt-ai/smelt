//! 智能体相关目录的位置：space、记忆目录、旧版工作台对话目录。只算路径，不建目录。
//! 路径以运行时那台机器的 `SMELT_HOME` 为准。

use std::path::{Path, PathBuf};

/// smelt-agent 启动名的 `~/` 写法。MCP 预设写进 Pi `mcp.json` 的 `command` 就是它，配上
/// [`BUN_BE_BUN`] 把 smelt-agent 当 Bun 用（`smelt-agent x --bun 包@版本`）：Bun 编在
/// smelt-agent 里，不用另装一份。Unix 上它是指向当前版本的链接，Pi CLI 自己也执行得了；
/// Windows 上是指针文件，smelt-agent 起 MCP 时认出这条路径、换成它自己
/// （`agent/src/pi-mcp-runtime.ts`）。Pi 会展开开头的 `~/`。
pub const SMELT_AGENT_LAUNCHER_HOME_PATH: &str = "~/.smelt/bin/smelt-agent";

/// Bun 编译出的程序带上这个环境变量就当 Bun 命令行用。
pub const BUN_BE_BUN: (&str, &str) = ("BUN_BE_BUN", "1");

/// 智能体自己的 space 根目录：`~/.smelt/agents/<id>`。
///
/// 同一个智能体的所有会话共用它作为工作目录。以前每开一次对话就建一个随机
/// uuid 空目录，等于把「进程 cwd」和「智能体的长期上下文」混为一谈——agent
/// 攒下的文件、笔记、AGENTS.md 下次开对话就找不到了。space 根让这些东西真正
/// 属于智能体。
pub fn agent_space_root(agent_definition_id: &str) -> Option<PathBuf> {
    agent_space_in(&agent_spaces_root()?, agent_definition_id)
}

/// `spaces_root` 下这个智能体的 space；id 不能安全地当目录名时返回 `None`。
pub fn agent_space_in(spaces_root: &Path, agent_definition_id: &str) -> Option<PathBuf> {
    let id = agent_definition_id.trim();
    if id.is_empty() || id.contains('/') || id.contains('\\') || id == "." || id == ".." {
        return None;
    }
    Some(spaces_root.join(id))
}

/// 智能体自己的记忆目录：`~/.smelt/agents/<id>/memory`。
///
/// 跟着智能体走，不跟着工作目录走：同一个智能体在自己的 space、自动化目录还是用户项目
/// 里运行，读写的都是这一份。格式和规则见 `docs/agent-memory.md`。
pub fn agent_memory_dir(agent_definition_id: &str) -> Option<PathBuf> {
    agent_space_root(agent_definition_id).map(|space| space.join("memory"))
}

/// 跨智能体的用户全局记忆：`~/.smelt/memory`，只放用户本人和用户纠正过的做法。
pub fn user_memory_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("memory"))
}

/// 所有智能体 space 的根：`~/.smelt/agents`。运行时把它随智能体配置推给客户端
/// （`AgentHostSettings::agent_spaces_root`），客户端据此认出别的机器上的 space。
pub fn agent_spaces_root() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("agents"))
}

pub fn is_agent_space(path: &Path) -> bool {
    let Some(home) = smelt_paths::smelt_home().and_then(|home| home.to_str().map(str::to_owned))
    else {
        return false;
    };
    path.to_str()
        .is_some_and(|path| is_agent_space_in(&home, path))
}

/// 同 [`is_agent_space`]，按给定的 `SMELT_HOME` 算。两条都是运行时路径，按 `/` 和 `\`
/// 切开比较，不经过 `std::path`。
pub(crate) fn is_agent_space_in(smelt_home: &str, path: &str) -> bool {
    under(&format!("{}/agents", trim_slash(smelt_home)), path)
}

/// 从 space 路径反解出智能体 id。
///
/// 工具面板、历史页这类只拿得到一个 cwd 的地方，靠它认出「现在是在某个智能体
/// 的上下文里」，从而按智能体而不是按裸引擎来处理。只认 space 根本身，子目录
/// 不算：那是智能体的工作产物，不是另一个智能体。
pub fn agent_definition_id_for_space(path: &Path) -> Option<String> {
    let root = agent_spaces_root()?;
    let relative = path.strip_prefix(&root).ok()?;
    let mut parts = relative.components();
    let id = parts.next()?.as_os_str().to_str()?.to_string();
    if parts.next().is_some() || id.is_empty() {
        return None;
    }
    Some(id)
}

/// 工作台「新对话」的托管目录：`~/.smelt/workspaces/conversations`。
/// 不属于项目会话，不能沿用当前打开的仓库。
pub fn workbench_conversation_workspace_root() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("workspaces").join("conversations"))
}

pub fn is_workbench_conversation_workspace(path: &Path) -> bool {
    let Some(home) = smelt_paths::smelt_home().and_then(|home| home.to_str().map(str::to_owned))
    else {
        return false;
    };
    path.to_str()
        .is_some_and(|path| is_workbench_conversation_workspace_in(&home, path))
}

/// 同 [`is_workbench_conversation_workspace`]，按给定的 `SMELT_HOME` 算。
pub(crate) fn is_workbench_conversation_workspace_in(smelt_home: &str, path: &str) -> bool {
    under(
        &format!("{}/workspaces/conversations", trim_slash(smelt_home)),
        path,
    )
}

/// 同 [`agent_definition_id_for_space`]，`spaces_root` 是路径所在那台机器上 space 的根
/// （[`agent_spaces_root`]，那台机器的运行时报的）。远程机器上的 cwd 要拿那台机器的根去比；
/// 两条都是运行时路径，按 `/` 和 `\` 切开比较，不经过 `std::path`。
pub fn agent_definition_id_in_spaces(spaces_root: &str, path: &str) -> Option<String> {
    let root = format!("{}/", slash_form(spaces_root));
    let id = slash_form(path).strip_prefix(&root)?.to_string();
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

fn trim_slash(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

/// `path` 等于 `root`，或落在 `root` 的下一级完整路径段里。`/proj` 不包含 `/project`。
///
/// 比较前把 `\` 收成 `/`：Windows 本机路径用反斜杠，远程机器的路径用正斜杠，两边要对上。
fn under(root: &str, path: &str) -> bool {
    let path = slash_form(path);
    let root = slash_form(root);
    path == root || path.starts_with(&format!("{root}/"))
}

fn slash_form(path: &str) -> String {
    trim_slash(path).replace('\\', "/")
}

/// Pi 子智能体的子会话目录：`~/.smelt/subagents`。不放进 Pi 的会话目录：历史列表会把
/// 那里每份记录都当成一段独立的对话；Pi profile 还会挪那个目录。
pub fn subagent_sessions_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("subagents"))
}

/// 一个子智能体的子会话：`~/.smelt/subagents/<id>.jsonl`。只按子智能体 id 定，
/// 父会话回退或分叉换了会话 id 也找得到。id 不能安全地当文件名时返回 `None`。
pub fn subagent_session_file(subagent_id: &str) -> Option<PathBuf> {
    subagent_session_file_in(&subagent_sessions_dir()?, subagent_id)
}

pub fn subagent_session_file_in(dir: &Path, subagent_id: &str) -> Option<PathBuf> {
    let id = subagent_id.trim();
    let safe = !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    safe.then(|| dir.join(format!("{id}.jsonl")))
}

#[cfg(test)]
mod tests {
    use super::{
        agent_definition_id_in_spaces, is_agent_space_in, is_workbench_conversation_workspace_in,
    };

    #[test]
    fn backslash_paths_count_as_the_same_space() {
        let home = r"C:\Users\me\.smelt";
        assert!(is_agent_space_in(
            home,
            r"C:\Users\me\.smelt\agents\space-agent"
        ));
        assert!(is_agent_space_in(
            home,
            r"C:\Users\me\.smelt\agents\space-agent\notes.md"
        ));
        assert!(!is_agent_space_in(
            home,
            r"C:\Users\me\.smelt\agents-evil\space-agent"
        ));
        assert!(is_workbench_conversation_workspace_in(
            home,
            r"C:\Users\me\.smelt\workspaces\conversations\chat-1",
        ));
        assert!(is_agent_space_in(
            "/Users/me/.smelt",
            "/Users/me/.smelt/agents/space-agent"
        ));
    }

    #[test]
    fn space_agent_id_on_another_machine_uses_that_machines_spaces_root() {
        let root = "/home/u/.smelt/agents";
        assert_eq!(
            agent_definition_id_in_spaces(root, "/home/u/.smelt/agents/writer/").as_deref(),
            Some("writer")
        );
        // 子目录是智能体的工作产物，不是另一个智能体。
        assert_eq!(
            agent_definition_id_in_spaces(root, "/home/u/.smelt/agents/writer/notes"),
            None
        );
        assert_eq!(
            agent_definition_id_in_spaces(root, "/home/u/.smelt/agents"),
            None
        );
        assert_eq!(
            agent_definition_id_in_spaces(root, "/home/u/.smelt/agents-x/a"),
            None
        );
        assert_eq!(
            agent_definition_id_in_spaces(root, "/home/u/code/app"),
            None
        );
        assert_eq!(
            agent_definition_id_in_spaces(
                r"C:\Users\me\.smelt\agents\",
                r"C:\Users\me\.smelt\agents\a"
            )
            .as_deref(),
            Some("a")
        );
    }
}
