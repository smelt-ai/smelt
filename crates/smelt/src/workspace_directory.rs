//! 和一个运行时的工作区目录同步对话的账本：收到推送时算出本地要增删哪些对话，保存时
//! 算出要发哪些命令，命令发完记账。只有数据，不碰界面，界面那一侧见 `workspace_persist`。
//!
//! 目录归运行时，这边只发单条命令（新建、修改、关掉对话）。规则：
//!
//! - 只把「这边上次发出之后又变了的」对话发出去，比的是这边上次发出的样子（`sent`），
//!   不是收到的推送。两台客户端各自算出的派生字段不同，也不会为它来回互改。
//! - 这边没见过、目录里也没有的对话是这边新建的，发新建；在目录里见过、现在没了的是
//!   别处关掉的，本地摘掉，不再建回去。
//! - 关掉对话是显式命令，发成功之前一直留着，重连后接着发。
//!
//! 项目不在这里：项目列表就是最近一份目录里的那份，开关项目直接发命令，等推送回来
//! 再变（见 `Workspace::request_project_open`）。对话要对账，是因为它在这边有活着的
//! 终端和界面、内容一直在变；项目只有一个路径，不需要。

use std::collections::{BTreeSet, HashMap, HashSet};

use smelt_model::workspace_directory::WorkspaceConversation;

#[derive(Default)]
pub(crate) struct DirectorySync {
    /// 最近一份目录的版本号和里面的对话 id。没收到之前不发任何命令：内存里没有的不代表
    /// 运行时上没有。
    listed: Option<(u64, HashSet<String>)>,
    /// 在目录里见过的对话（含这边新建成功的）。对话 id 不会重用，见过就一直算见过。
    known_conversations: HashSet<String>,
    /// 每段对话这边上次发出（或读到时）的样子。只在发出成功时变。
    sent: HashMap<String, WorkspaceConversation>,
    pending_deletes: BTreeSet<String>,
}

/// 收到一份目录之后本地要做的事。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Inbound {
    /// 这个运行时的第一份目录：按冷启动恢复。
    pub(crate) first: bool,
    /// 本地还没有的对话，按目录里的顺序。
    pub(crate) added: Vec<WorkspaceConversation>,
    /// 别处关掉的对话 id。
    pub(crate) removed: Vec<String>,
    /// 别处改了名的对话：(对话 id, 新名字)。
    pub(crate) retitled: Vec<(String, Option<String>)>,
}

/// 这一轮要发的对话命令。后台按这个顺序发：先删、再建、再改。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DirectoryCommands {
    pub(crate) deletes: Vec<String>,
    pub(crate) creates: Vec<WorkspaceConversation>,
    pub(crate) updates: Vec<WorkspaceConversation>,
}

impl DirectoryCommands {
    pub(crate) fn is_empty(&self) -> bool {
        self.deletes.is_empty() && self.creates.is_empty() && self.updates.is_empty()
    }
}

/// 发完的结果，回主线程记账。没列进来的命令是没发成功的，下一轮再发。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct DirectoryOutcome {
    pub(crate) deleted: Vec<String>,
    pub(crate) created: Vec<WorkspaceConversation>,
    /// 改成功的，和运行时拒收（参数不对）的：后者再发也一样，记成发过，等内容变了再发。
    pub(crate) settled: Vec<WorkspaceConversation>,
}

impl DirectorySync {
    pub(crate) fn loaded(&self) -> bool {
        self.listed.is_some()
    }

    /// 已经对过账的那份目录的版本号。
    pub(crate) fn revision(&self) -> Option<u64> {
        self.listed.as_ref().map(|(revision, _)| *revision)
    }

    /// 收到一份目录里的对话。`local_conversations` 是本地已有的对话（含还在恢复的）。
    pub(crate) fn receive(
        &mut self,
        revision: u64,
        conversations: Vec<WorkspaceConversation>,
        local_conversations: &HashSet<String>,
    ) -> Inbound {
        let first = self.listed.is_none();
        let ids = conversations
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect::<HashSet<_>>();
        let removed = local_conversations
            .iter()
            .filter(|id| self.known_conversations.contains(*id) && !ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        let mut added = Vec::new();
        let mut retitled = Vec::new();
        for conversation in conversations {
            if self.pending_deletes.contains(&conversation.id) {
                continue;
            }
            if !local_conversations.contains(&conversation.id) {
                // 读到的样子就是这边的起点：恢复出来没改过的不必再发一遍。
                self.sent
                    .insert(conversation.id.clone(), conversation.clone());
                added.push(conversation);
                continue;
            }
            let sent_title = self
                .sent
                .get(&conversation.id)
                .map(|sent| &sent.custom_title);
            if sent_title.is_some_and(|title| *title != conversation.custom_title) {
                retitled.push((conversation.id, conversation.custom_title));
            }
        }
        self.known_conversations.extend(ids.iter().cloned());
        for id in &removed {
            self.sent.remove(id);
        }
        self.listed = Some((revision, ids));
        Inbound {
            first,
            added,
            removed,
            retitled,
        }
    }

    /// 按本地现在的样子算这一轮要发的命令。目录还没到时不发。
    pub(crate) fn commands(&self, desired: &[WorkspaceConversation]) -> Option<DirectoryCommands> {
        let (_, listed) = self.listed.as_ref()?;
        let mut commands = DirectoryCommands {
            deletes: self.pending_deletes.iter().cloned().collect(),
            ..DirectoryCommands::default()
        };
        for conversation in desired {
            if self.pending_deletes.contains(&conversation.id) {
                continue;
            }
            if listed.contains(&conversation.id) {
                if self.sent.get(&conversation.id) != Some(conversation) {
                    commands.updates.push(conversation.clone());
                }
            } else if !self.known_conversations.contains(&conversation.id) {
                commands.creates.push(conversation.clone());
            }
        }
        Some(commands)
    }

    /// 本地关掉了一段对话：发删除，发成功之前不会被推送里的旧目录带回来。
    pub(crate) fn close_conversation(&mut self, id: &str) {
        self.pending_deletes.insert(id.to_string());
    }

    pub(crate) fn record(&mut self, outcome: DirectoryOutcome) {
        for id in outcome.deleted {
            self.pending_deletes.remove(&id);
            self.sent.remove(&id);
        }
        for conversation in outcome.created {
            self.known_conversations.insert(conversation.id.clone());
            self.sent.insert(conversation.id.clone(), conversation);
        }
        for conversation in outcome.settled {
            self.sent.insert(conversation.id.clone(), conversation);
        }
    }
}

/// 按这台客户端记的顺序排：记过的按记的位置，没记过的（别处新建的）按原来的先后排在后面。
pub(crate) fn order_by_saved<T>(items: Vec<T>, saved: &[String], key: impl Fn(&T) -> &str) -> Vec<T> {
    let rank = saved
        .iter()
        .enumerate()
        .map(|(position, id)| (id.as_str(), position))
        .collect::<HashMap<_, _>>();
    let mut indexed = items.into_iter().enumerate().collect::<Vec<_>>();
    indexed.sort_by_key(|(original, item)| {
        rank.get(key(item))
            .map_or((1, *original), |position| (0, *position))
    });
    indexed.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(id: &str, title: Option<&str>) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.to_string(),
            custom_title: title.map(str::to_string),
            ..WorkspaceConversation::default()
        }
    }

    fn ids(items: &[&str]) -> HashSet<String> {
        items.iter().map(|id| id.to_string()).collect()
    }

    fn roots(items: &[&str]) -> Vec<String> {
        items.iter().map(|root| root.to_string()).collect()
    }

    /// 目录到之前什么都不发：空的内存不代表运行时上没有对话。
    #[test]
    fn nothing_is_sent_before_the_first_directory() {
        let sync = DirectorySync::default();
        assert_eq!(sync.commands(&[conversation("a", None)]), None);
    }

    /// 第一份目录全部交给恢复；恢复出来没改过的不再发。
    #[test]
    fn the_first_directory_is_restored_and_not_echoed_back() {
        let mut sync = DirectorySync::default();
        let inbound = sync.receive(1, vec![conversation("a", Some("A"))], &HashSet::new());
        assert!(inbound.first);
        assert_eq!(inbound.added, [conversation("a", Some("A"))]);

        let commands = sync.commands(&[conversation("a", Some("A"))]).unwrap();
        assert!(commands.is_empty());
    }

    /// 这边新建的发新建；建成之前收到的目录里没有它，也不能当成别处删了。
    #[test]
    fn a_local_conversation_is_created_and_survives_an_older_directory() {
        let mut sync = DirectorySync::default();
        sync.receive(2, Vec::new(), &HashSet::new());

        let local = conversation("new", None);
        let commands = sync.commands(std::slice::from_ref(&local)).unwrap();
        assert_eq!(commands.creates, std::slice::from_ref(&local));

        let inbound = sync.receive(3, Vec::new(), &ids(&["new"]));
        assert!(inbound.removed.is_empty());

        sync.record(DirectoryOutcome {
            created: vec![local.clone()],
            ..DirectoryOutcome::default()
        });
        assert!(sync.commands(&[local]).unwrap().is_empty());
    }

    /// 别处关掉的对话本地摘掉，之后不再被当成这边新建的发回去。
    #[test]
    fn a_conversation_closed_elsewhere_is_removed_and_not_recreated() {
        let mut sync = DirectorySync::default();
        sync.receive(4, vec![conversation("a", None)], &HashSet::new());
        let inbound = sync.receive(5, Vec::new(), &ids(&["a"]));
        assert_eq!(inbound.removed, ["a"]);
        // 摘掉之前又保存了一次：不能建回去。
        let commands = sync.commands(&[conversation("a", None)]).unwrap();
        assert!(commands.creates.is_empty() && commands.updates.is_empty());
    }

    /// 别处新开的对话交给恢复。
    #[test]
    fn a_conversation_opened_elsewhere_is_added() {
        let mut sync = DirectorySync::default();
        sync.receive(6, Vec::new(), &HashSet::new());
        let inbound = sync.receive(7, vec![conversation("b", None)], &HashSet::new());
        assert!(!inbound.first);
        assert_eq!(inbound.added, [conversation("b", None)]);
    }

    /// 只发这边变了的；收到的推送和这边算出来的不一样（别处的派生字段），不会因此发回去。
    #[test]
    fn only_local_changes_are_sent() {
        let mut sync = DirectorySync::default();
        sync.receive(8, vec![conversation("a", None)], &HashSet::new());
        let mut theirs = conversation("a", None);
        theirs.last_updated_at = 99;
        sync.receive(9, vec![theirs], &ids(&["a"]));
        assert!(sync.commands(&[conversation("a", None)]).unwrap().is_empty());

        let mut mine = conversation("a", None);
        mine.last_updated_at = 5;
        assert_eq!(sync.commands(&[mine.clone()]).unwrap().updates, [mine]);
    }

    /// 别处改的名字接过来；这边改过的名字不会被自己发出去的回声当成别处改的。
    #[test]
    fn titles_changed_elsewhere_are_adopted() {
        let mut sync = DirectorySync::default();
        sync.receive(10, vec![conversation("a", Some("old"))], &HashSet::new());
        let inbound = sync.receive(11, vec![conversation("a", Some("theirs"))], &ids(&["a"]));
        assert_eq!(inbound.retitled, [("a".to_string(), Some("theirs".to_string()))]);

        sync.record(DirectoryOutcome {
            settled: vec![conversation("a", Some("mine"))],
            ..DirectoryOutcome::default()
        });
        let echo = sync.receive(12, vec![conversation("a", Some("mine"))], &ids(&["a"]));
        assert!(echo.retitled.is_empty());
    }

    /// 关掉的对话发删除，删成之前推送里的旧目录不会把它带回来。
    #[test]
    fn a_pending_delete_is_sent_until_confirmed() {
        let mut sync = DirectorySync::default();
        sync.receive(13, vec![conversation("a", None)], &HashSet::new());
        sync.close_conversation("a");
        let inbound = sync.receive(14, vec![conversation("a", None)], &HashSet::new());
        assert!(inbound.added.is_empty());
        assert_eq!(sync.commands(&[]).unwrap().deletes, ["a"]);

        sync.record(DirectoryOutcome {
            deleted: vec!["a".to_string()],
            ..DirectoryOutcome::default()
        });
        assert!(sync.commands(&[]).unwrap().is_empty());
    }

    #[test]
    fn saved_order_wins_and_unknown_items_keep_their_order_after_it() {
        let items = vec!["a", "b", "c", "d"];
        let saved = roots(&["c", "a"]);
        assert_eq!(order_by_saved(items, &saved, |item| *item), ["c", "a", "b", "d"]);
    }
}
