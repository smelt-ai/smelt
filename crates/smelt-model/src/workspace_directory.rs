//! 运行时的工作区目录：开着哪些项目、有哪些对话。
//!
//! 运行时是唯一的写入方。客户端用单条命令改它（`workspace.conversation.*`、
//! `workspace.project.*`），经 `workspace.changed` 收整份投影，订阅首帧就带着当前这份。
//! 侧栏怎么排、折叠和固定、终端怎么分屏、当前看哪个，是每台客户端自己的界面状态，
//! 不在这里。

use serde::{Deserialize, Serialize};

use crate::agent_kind::ConversationLaunchSpec;
use crate::conversation::ConversationBinding;
use crate::runtime_path::PathStyle;

/// 整份目录。`revision` 每次有改动加一，只在同一个 `epoch` 里能比，见 [`WorkspaceRevision`]。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDirectory {
    /// 运行时这份库的随机身份。库换过（删了重建）就不同，版本号从头数。
    #[serde(default)]
    pub epoch: String,
    pub revision: u64,
    /// 打开的项目根目录，按打开的先后。
    #[serde(default)]
    pub projects: Vec<String>,
    /// 按新建的先后。
    #[serde(default)]
    pub conversations: Vec<WorkspaceConversation>,
}

/// 一段对话：侧栏里的一条。要么是一段 ACP 对话，要么是一组终端。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversation {
    /// 新建它的客户端生成，之后不再变。
    pub id: String,
    #[serde(default)]
    pub custom_title: Option<String>,
    /// 最近一次内容变化的 unix 秒。
    #[serde(default)]
    pub last_updated_at: i64,
    /// 这段对话里的终端，按客户端给的顺序。ACP 对话没有。
    #[serde(default)]
    pub terminals: Vec<WorkspaceTerminal>,
    #[serde(default)]
    pub acp: Option<WorkspaceAcp>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTerminal {
    /// 守护里的终端会话 id。没起过的终端没有，运行时用对话 id 派生一个占位。
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub launch_label: Option<String>,
    #[serde(default)]
    pub launch_cmd: Option<String>,
}

/// ACP 对话的启动参数和尚未交付的状态。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAcp {
    pub cwd: Option<String>,
    /// 守护托管用的会话 id。写入时必填。
    pub sid: String,
    /// agent 种类标识（`ConversationAgentKind::id()`）。
    pub agent: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    /// 续接用的 provider 对话 id。守护确认过之后以守护为准，客户端带来的旧值不覆盖它。
    #[serde(default)]
    pub history_session_id: Option<String>,
    pub launch: ConversationLaunchSpec,
    #[serde(default)]
    pub refresh_launch_from_settings: bool,
    #[serde(default)]
    pub conversation_binding: ConversationBinding,
    /// 最后生效的 provider 配置：`(config_id, value_id)`，按设置顺序。
    #[serde(default)]
    pub config_values: Vec<(String, String)>,
    #[serde(default)]
    pub pending_prompt: Option<String>,
    #[serde(default)]
    pub pending_delivery_id: Option<String>,
    #[serde(default)]
    pub pending_agent_preset: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub agent_definition_id: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversationIdParams {
    pub conversation_id: String,
}

/// `workspace.rename` 的参数。带 `terminal_id` 是给这段对话里的这一格终端改名，不带是给
/// 整段对话改名。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRenameParams {
    pub conversation_id: String,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProjectParams {
    pub root: String,
}

/// 目录的版本：哪一份（`epoch`）、改到第几次。命令回执也是它，没改动时是原来的版本。
///
/// 版本号只在同一个 `epoch` 里能比。`epoch` 不同说明运行时的库换过，版本号从头数，
/// 新的那份不能因为数小就被当成旧的扔掉。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRevision {
    #[serde(default)]
    pub epoch: String,
    pub revision: u64,
}

impl WorkspaceRevision {
    /// 同一份目录里比 `other` 旧。不同份之间谈不上新旧。
    pub fn is_older_than(&self, other: &Self) -> bool {
        self.epoch == other.epoch && self.revision < other.revision
    }

    /// 收到它时该不该替换手上的 `current`：换了一份，或同一份里更新。
    pub fn supersedes(&self, current: &Self) -> bool {
        self.epoch != current.epoch || self.revision > current.revision
    }
}

impl WorkspaceDirectory {
    pub fn version(&self) -> WorkspaceRevision {
        WorkspaceRevision {
            epoch: self.epoch.clone(),
            revision: self.revision,
        }
    }
}

/// cwd 归属哪个项目根：cwd 就是根、或落在根之下（必须是完整路径段，`/a/bc` 不算
/// 落在 `/a/b` 下）。多个根都匹配时取最深的那个——`~/dev/a` 和 `~/dev/a/sub` 都打开
/// 过时，子目录的会话归后者。空 cwd 或谁都不沾 → None（调用方按 cwd 自建隐式组）。
/// 路径是运行时那台机器上的，按它的系统比（`style`）。
///
/// 归属不存：桌面、手机都按这一份算法从目录里的项目和会话的 cwd 现算。
pub fn project_root_of<'a>(style: PathStyle, roots: &'a [String], cwd: &str) -> Option<&'a str> {
    if cwd.is_empty() {
        return None;
    }
    roots
        .iter()
        .map(|root| root.trim_end_matches('/'))
        .filter(|root| !root.is_empty() && style.starts_with(cwd, root))
        .max_by_key(|root| root.len())
}

/// 项目显示名：目录末段。根目录这类没有末段的叫「项目」。
pub fn project_name(style: PathStyle, root: &str) -> String {
    style.file_name(root).unwrap_or("项目").to_string()
}

/// 一组项目在同一个列表里的显示名：末段重名的逐步往前补父目录段（`dev · smelt`），
/// 直到互不相同或路径补到顶。项目都在同一个运行时上，路径按它的系统切（`style`）。
pub fn project_labels(style: PathStyle, roots: &[&str]) -> Vec<String> {
    let bases: Vec<String> = roots.iter().map(|root| project_name(style, root)).collect();
    let mut labels = bases.clone();
    let mut extra = vec![0usize; roots.len()];
    // 每轮给所有重名的多补一段；路径最深也就那么几段，8 轮足够收敛。
    for _ in 0..8 {
        let dup: Vec<usize> = (0..labels.len())
            .filter(|&i| (0..labels.len()).any(|j| j != i && labels[j] == labels[i]))
            .collect();
        if dup.is_empty() {
            break;
        }
        let mut changed = false;
        for i in dup {
            let next = extra[i] + 1;
            let candidate = label_with_parents(style, roots[i], &bases[i], next);
            if candidate != labels[i] {
                extra[i] = next;
                labels[i] = candidate;
                changed = true;
            }
        }
        // 全都补到路径顶了还重名（真同路径 / 只剩根）→ 认命，别空转。
        if !changed {
            break;
        }
    }
    labels
}

/// 往前补 `extra` 段父目录：`/a/b/smelt` + 1 → `b · smelt`。
fn label_with_parents(style: PathStyle, root: &str, base: &str, extra: usize) -> String {
    let (separators, joiner): (&[char], &str) = match style {
        PathStyle::Posix => (&['/'], "/"),
        PathStyle::Windows => (&['\\', '/'], "\\"),
    };
    let segs: Vec<&str> = root
        .split(separators)
        .filter(|s| !s.is_empty())
        .collect();
    let end = segs.len().saturating_sub(1); // base 已经代表末段
    let start = end.saturating_sub(extra);
    if start >= end {
        return base.to_string();
    }
    format!("{} · {}", segs[start..end].join(joiner), base)
}

#[cfg(test)]
mod revision_tests {
    use super::WorkspaceRevision;

    fn at(epoch: &str, revision: u64) -> WorkspaceRevision {
        WorkspaceRevision {
            epoch: epoch.into(),
            revision,
        }
    }

    /// 同一份里按版本号比；库换过之后版本号从头数，新的那份不是旧的。
    #[test]
    fn revisions_compare_only_within_one_epoch() {
        assert!(at("a", 3).is_older_than(&at("a", 4)));
        assert!(!at("a", 4).is_older_than(&at("a", 4)));
        assert!(!at("b", 1).is_older_than(&at("a", 40)), "换了库");

        assert!(at("a", 5).supersedes(&at("a", 4)));
        assert!(!at("a", 4).supersedes(&at("a", 4)), "同一份不重复收");
        assert!(!at("a", 3).supersedes(&at("a", 4)));
        assert!(at("b", 1).supersedes(&at("a", 40)), "换了库就收新的");
    }
}

#[cfg(test)]
mod project_tests {
    use super::{PathStyle, project_labels, project_name, project_root_of};

    fn labels(roots: &[&str]) -> Vec<String> {
        project_labels(PathStyle::Posix, roots)
    }

    fn projects(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// 项目名是目录末段。Windows 运行时报来的路径用 `\` 分隔，按 `/` 切会把整条路径当成名字。
    #[test]
    fn project_name_is_the_last_segment_on_the_runtime() {
        assert_eq!(project_name(PathStyle::Posix, "/a/smelt/"), "smelt");
        assert_eq!(
            project_name(PathStyle::Windows, r"C:\Users\w\code\xunxian-reverse"),
            "xunxian-reverse"
        );
        assert_eq!(project_name(PathStyle::Posix, "/"), "项目");
        assert_eq!(project_name(PathStyle::Windows, r"C:\"), "项目");
    }

    /// Windows 项目重名时同样往前补父目录段，段之间用 `\` 连。
    #[test]
    fn windows_duplicate_labels_get_parent_segments() {
        assert_eq!(
            project_labels(PathStyle::Windows, &[r"C:\x\dev\smelt", r"D:\y\dev\smelt"]),
            [r"x\dev · smelt", r"y\dev · smelt"]
        );
    }

    /// 不重名就别乱加前缀（大多数情况该保持干净的目录名）。
    #[test]
    fn unique_labels_are_left_alone() {
        assert_eq!(labels(&["/a/smelt", "/a/other"]), ["smelt", "other"]);
    }

    /// 末段同名的两个项目必须区分得开——否则并排两个一模一样的「smelt」，分不清哪个是哪个。
    #[test]
    fn duplicate_labels_get_parent_segments() {
        assert_eq!(
            labels(&["/x/dev/smelt", "/y/work/smelt"]),
            ["dev · smelt", "work · smelt"]
        );
    }

    /// 补一段还撞车就继续往上补，直到分开。
    #[test]
    fn keeps_climbing_until_unique() {
        assert_eq!(
            labels(&["/a/dev/smelt", "/b/dev/smelt"]),
            ["a/dev · smelt", "b/dev · smelt"]
        );
    }

    /// 路径补到顶还重名（真·同路径）→ 必须收敛退出，不能在循环里空转。
    #[test]
    fn gives_up_instead_of_looping_forever() {
        assert_eq!(labels(&["/smelt", "/smelt"]), ["smelt", "smelt"]);
    }

    /// cwd 就是项目根、或落在项目根之下，都该归这个项目。
    #[test]
    fn cwd_belongs_to_its_project_root() {
        let p = projects(&["/Users/me/dev/smelt"]);
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/Users/me/dev/smelt"),
            Some("/Users/me/dev/smelt")
        );
        assert_eq!(
            project_root_of(PathStyle::Posix, &p, "/Users/me/dev/smelt/crates/smeltd"),
            Some("/Users/me/dev/smelt")
        );
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/Users/me/dev/other"), None);
        assert_eq!(project_root_of(PathStyle::Posix, &p, ""), None);
    }

    /// 前缀必须卡在完整路径段上：`/a/smelt-old` 不是 `/a/smelt` 的子目录，
    /// 否则名字相近的两个项目会互相吞会话。
    #[test]
    fn prefix_must_be_a_whole_path_segment() {
        let p = projects(&["/a/smelt"]);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/smelt-old"), None);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/smeltd"), None);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/smelt/sub"), Some("/a/smelt"));
    }

    /// 父子项目都打开着时，会话归最深的那个（不然子项目永远空着）。
    #[test]
    fn deepest_matching_project_wins() {
        let p = projects(&["/a", "/a/b", "/a/b/c"]);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/c/x"), Some("/a/b/c"));
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/x"), Some("/a/b"));
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/x"), Some("/a"));
    }

    /// Windows 运行时的会话开在项目子目录里也归这个项目：按 `\` 切、不分大小写。
    #[test]
    fn windows_cwd_belongs_to_its_project_root() {
        let p = projects(&[r"C:\Users\w\code\smelt"]);
        assert_eq!(
            project_root_of(PathStyle::Windows, &p, r"C:\Users\w\code\smelt\crates"),
            Some(r"C:\Users\w\code\smelt")
        );
        assert_eq!(
            project_root_of(PathStyle::Windows, &p, r"c:\users\w\code\SMELT"),
            Some(r"C:\Users\w\code\smelt")
        );
        assert_eq!(
            project_root_of(PathStyle::Windows, &p, r"C:\Users\w\code\smelt-old"),
            None
        );
    }

    /// 结尾斜杠是路径写法差异，不该影响归属判定。
    #[test]
    fn trailing_slashes_are_ignored() {
        let p = projects(&["/a/b/"]);
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b"), Some("/a/b"));
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/"), Some("/a/b"));
        assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/c"), Some("/a/b"));
    }
}
