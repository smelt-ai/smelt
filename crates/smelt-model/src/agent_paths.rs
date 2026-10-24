//! 智能体相关目录的位置：space、记忆目录、旧版工作台对话目录。只算路径，不建目录。
//! 路径以运行时那台机器的 `SMELT_HOME` 为准。

use std::path::{Path, PathBuf};

/// 运行时自带 Bun 启动器（`~/.smelt/runtime/bin/bun`）的 `~/` 写法。写进外部配置（如 Pi
/// `mcp.json` 的 `command`）的路径必须是这一条，Pi 会展开开头的 `~/`。
pub const MANAGED_BUN_LAUNCHER_HOME_PATH: &str = "~/.smelt/runtime/bin/bun";

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

fn agent_spaces_root() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("agents"))
}

pub fn is_agent_space(path: &Path) -> bool {
    smelt_paths::smelt_home().is_some_and(|home| is_agent_space_in(&home, path))
}

/// 同 [`is_agent_space`]，`smelt_home` 是路径所在那台机器的 `SMELT_HOME`。客户端判断别的
/// 机器上的路径时用它：拿自己的主目录去比，远程机器上的 space 永远对不上。
pub fn is_agent_space_in(smelt_home: &Path, path: &Path) -> bool {
    path.starts_with(smelt_home.join("agents"))
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
    smelt_paths::smelt_home()
        .is_some_and(|home| is_workbench_conversation_workspace_in(&home, path))
}

/// 同 [`is_workbench_conversation_workspace`]，按路径所在那台机器的 `SMELT_HOME` 算。
pub fn is_workbench_conversation_workspace_in(smelt_home: &Path, path: &Path) -> bool {
    path.starts_with(smelt_home.join("workspaces").join("conversations"))
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
