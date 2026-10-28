//! 和一个运行时的工作区目录同步的账本：收到推送时算出本地要增删什么，保存时算出要发
//! 哪些命令，命令发完记账。只有数据，不碰界面，界面那一侧见 `workspace_persist`。
//!
//! 目录归运行时，这边只发单条命令（新建、修改、关掉对话，开关项目）。规则：
//!
//! - 只把「这边上次发出之后又变了的」对话发出去，比的是这边上次发出的样子（`sent`），
//!   不是收到的推送。两台客户端各自算出的派生字段不同，也不会为它来回互改。
//! - 这边没见过、目录里也没有的对话是这边新建的，发新建；在目录里见过、现在没了的是
//!   别处关掉的，本地摘掉，不再建回去。
//! - 关掉对话、关闭项目是显式命令，发成功之前一直留着，重连后接着发。

use std::collections::{BTreeSet, HashMap, HashSet};

use smelt_model::workspace_directory::{WorkspaceConversation, WorkspaceDirectory};

#[derive(Default)]
pub(crate) struct DirectorySync {
    /// 最近收到的目录。没收到之前不发任何命令：内存里没有的不代表运行时上没有。
    directory: Option<WorkspaceDirectory>,
    /// 在目录里见过的对话（含这边新建成功的）。
    known_conversations: HashSet<String>,
    known_projects: HashSet<String>,
    /// 每段对话这边上次发出（或读到时）的样子。只在发出成功时变。
    sent: HashMap<String, WorkspaceConversation>,
    pending_deletes: BTreeSet<String>,
    pending_project_closes: BTreeSet<String>,
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
    pub(crate) projects_added: Vec<String>,
    pub(crate) projects_removed: Vec<String>,
}

/// 这一轮要发的命令。后台按这个顺序发：先删、再关项目、开项目、建、改。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DirectoryCommands {
    pub(crate) deletes: Vec<String>,
    pub(crate) project_closes: Vec<String>,
    pub(crate) project_opens: Vec<String>,
    pub(crate) creates: Vec<WorkspaceConversation>,
    pub(crate) updates: Vec<WorkspaceConversation>,
}

impl DirectoryCommands {
    pub(crate) fn is_empty(&self) -> bool {
        self.deletes.is_empty()
            && self.project_closes.is_empty()
            && self.project_opens.is_empty()
            && self.creates.is_empty()
            && self.updates.is_empty()
    }
}

/// 发完的结果，回主线程记账。没列进来的命令是没发成功的，下一轮再发。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct DirectoryOutcome {
    pub(crate) deleted: Vec<String>,
    pub(crate) projects_closed: Vec<String>,
    pub(crate) projects_opened: Vec<String>,
    pub(crate) created: Vec<WorkspaceConversation>,
    /// 改成功的，和运行时拒收（参数不对）的：后者再发也一样，记成发过，等内容变了再发。
    pub(crate) settled: Vec<WorkspaceConversation>,
}

impl DirectorySync {
    pub(crate) fn loaded(&self) -> bool {
        self.directory.is_some()
    }

    pub(crate) fn directory(&self) -> Option<&WorkspaceDirectory> {
        self.directory.as_ref()
    }

    /// 收到一份目录。`local_conversations` 是本地已有的对话（含还在恢复的），
    /// `local_projects` 是本地开着的项目。
    pub(crate) fn receive(
        &mut self,
        directory: WorkspaceDirectory,
        local_conversations: &HashSet<String>,
        local_projects: &[String],
    ) -> Inbound {
        let first = self.directory.is_none();
        let ids = directory
            .conversations
            .iter()
            .map(|conversation| conversation.id.as_str())
            .collect::<HashSet<_>>();
        let removed = local_conversations
            .iter()
            .filter(|id| self.known_conversations.contains(*id) && !ids.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let mut added = Vec::new();
        let mut retitled = Vec::new();
        for conversation in &directory.conversations {
            if self.pending_deletes.contains(&conversation.id) {
                continue;
            }
            if !local_conversations.contains(&conversation.id) {
                // 读到的样子就是这边的起点：恢复出来没改过的不必再发一遍。
                self.sent
                    .insert(conversation.id.clone(), conversation.clone());
                added.push(conversation.clone());
                continue;
            }
            let sent_title = self
                .sent
                .get(&conversation.id)
                .map(|sent| &sent.custom_title);
            if sent_title.is_some_and(|title| *title != conversation.custom_title) {
                retitled.push((conversation.id.clone(), conversation.custom_title.clone()));
            }
        }
        let dir_projects = directory.projects.iter().collect::<HashSet<_>>();
        let projects_added = directory
            .projects
            .iter()
            .filter(|root| {
                !local_projects.contains(root) && !self.pending_project_closes.contains(*root)
            })
            .cloned()
            .collect();
        let projects_removed = local_projects
            .iter()
            .filter(|root| self.known_projects.contains(*root) && !dir_projects.contains(root))
            .cloned()
            .collect();
        self.known_conversations
            .extend(directory.conversations.iter().map(|c| c.id.clone()));
        self.known_projects.extend(directory.projects.iter().cloned());
        for id in &removed {
            self.sent.remove(id);
        }
        self.directory = Some(directory);
        Inbound {
            first,
            added,
            removed,
            retitled,
            projects_added,
            projects_removed,
        }
    }

    /// 按本地现在的样子算这一轮要发的命令。目录还没到时不发。
    pub(crate) fn commands(
        &self,
        desired: &[WorkspaceConversation],
        local_projects: &[String],
    ) -> Option<DirectoryCommands> {
        let directory = self.directory.as_ref()?;
        let ids = directory
            .conversations
            .iter()
            .map(|conversation| conversation.id.as_str())
            .collect::<HashSet<_>>();
        let mut commands = DirectoryCommands {
            deletes: self.pending_deletes.iter().cloned().collect(),
            project_closes: self.pending_project_closes.iter().cloned().collect(),
            ..DirectoryCommands::default()
        };
        commands.project_opens = local_projects
            .iter()
            .filter(|root| {
                !directory.projects.contains(root) && !self.known_projects.contains(*root)
            })
            .cloned()
            .collect();
        for conversation in desired {
            if self.pending_deletes.contains(&conversation.id) {
                continue;
            }
            if ids.contains(conversation.id.as_str()) {
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

    pub(crate) fn close_project(&mut self, root: &str) {
        self.pending_project_closes.insert(root.to_string());
    }

    pub(crate) fn record(&mut self, outcome: DirectoryOutcome) {
        for id in outcome.deleted {
            self.pending_deletes.remove(&id);
            self.sent.remove(&id);
        }
        for root in outcome.projects_closed {
            self.pending_project_closes.remove(&root);
        }
        self.known_projects.extend(outcome.projects_opened);
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

    fn directory(
        revision: u64,
        projects: &[&str],
        conversations: Vec<WorkspaceConversation>,
    ) -> WorkspaceDirectory {
        WorkspaceDirectory {
            revision,
            projects: projects.iter().map(|root| root.to_string()).collect(),
            conversations,
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
        assert_eq!(sync.commands(&[conversation("a", None)], &roots(&["/r"])), None);
    }

    /// 第一份目录全部交给恢复；恢复出来没改过的不再发。
    #[test]
    fn the_first_directory_is_restored_and_not_echoed_back() {
        let mut sync = DirectorySync::default();
        let inbound = sync.receive(
            directory(3, &["/r"], vec![conversation("a", Some("A"))]),
            &HashSet::new(),
            &[],
        );
        assert!(inbound.first);
        assert_eq!(inbound.added, [conversation("a", Some("A"))]);
        assert_eq!(inbound.projects_added, ["/r"]);

        let commands = sync
            .commands(&[conversation("a", Some("A"))], &roots(&["/r"]))
            .unwrap();
        assert!(commands.is_empty());
    }

    /// 这边新建的发新建；建成之前收到的目录里没有它，也不能当成别处删了。
    #[test]
    fn a_local_conversation_is_created_and_survives_an_older_directory() {
        let mut sync = DirectorySync::default();
        sync.receive(directory(1, &[], Vec::new()), &HashSet::new(), &[]);

        let local = conversation("new", None);
        let commands = sync.commands(&[local.clone()], &[]).unwrap();
        assert_eq!(commands.creates, [local.clone()]);

        let inbound = sync.receive(directory(2, &[], Vec::new()), &ids(&["new"]), &[]);
        assert!(inbound.removed.is_empty());

        sync.record(DirectoryOutcome {
            created: vec![local.clone()],
            ..DirectoryOutcome::default()
        });
        assert!(sync.commands(&[local], &[]).unwrap().is_empty());
    }

    /// 别处关掉的对话本地摘掉，之后不再被当成这边新建的发回去。
    #[test]
    fn a_conversation_closed_elsewhere_is_removed_and_not_recreated() {
        let mut sync = DirectorySync::default();
        sync.receive(
            directory(1, &[], vec![conversation("a", None)]),
            &HashSet::new(),
            &[],
        );
        let inbound = sync.receive(directory(2, &[], Vec::new()), &ids(&["a"]), &[]);
        assert_eq!(inbound.removed, ["a"]);
        // 摘掉之前又保存了一次：不能建回去。
        let commands = sync.commands(&[conversation("a", None)], &[]).unwrap();
        assert!(commands.creates.is_empty() && commands.updates.is_empty());
    }

    /// 别处新开的对话交给恢复。
    #[test]
    fn a_conversation_opened_elsewhere_is_added() {
        let mut sync = DirectorySync::default();
        sync.receive(directory(1, &[], Vec::new()), &HashSet::new(), &[]);
        let inbound = sync.receive(
            directory(2, &[], vec![conversation("b", None)]),
            &HashSet::new(),
            &[],
        );
        assert!(!inbound.first);
        assert_eq!(inbound.added, [conversation("b", None)]);
    }

    /// 只发这边变了的；收到的推送和这边算出来的不一样（别处的派生字段），不会因此发回去。
    #[test]
    fn only_local_changes_are_sent() {
        let mut sync = DirectorySync::default();
        sync.receive(
            directory(1, &[], vec![conversation("a", None)]),
            &HashSet::new(),
            &[],
        );
        let mut theirs = conversation("a", None);
        theirs.last_updated_at = 99;
        sync.receive(directory(2, &[], vec![theirs]), &ids(&["a"]), &[]);
        assert!(
            sync.commands(&[conversation("a", None)], &[])
                .unwrap()
                .is_empty()
        );

        let mut mine = conversation("a", None);
        mine.last_updated_at = 5;
        assert_eq!(
            sync.commands(&[mine.clone()], &[]).unwrap().updates,
            [mine]
        );
    }

    /// 别处改的名字接过来；这边改过的名字不会被自己发出去的回声当成别处改的。
    #[test]
    fn titles_changed_elsewhere_are_adopted() {
        let mut sync = DirectorySync::default();
        sync.receive(
            directory(1, &[], vec![conversation("a", Some("old"))]),
            &HashSet::new(),
            &[],
        );
        let inbound = sync.receive(
            directory(2, &[], vec![conversation("a", Some("theirs"))]),
            &ids(&["a"]),
            &[],
        );
        assert_eq!(inbound.retitled, [("a".to_string(), Some("theirs".to_string()))]);

        sync.record(DirectoryOutcome {
            settled: vec![conversation("a", Some("mine"))],
            ..DirectoryOutcome::default()
        });
        let echo = sync.receive(
            directory(3, &[], vec![conversation("a", Some("mine"))]),
            &ids(&["a"]),
            &[],
        );
        assert!(echo.retitled.is_empty());
    }

    /// 关掉的对话发删除，删成之前推送里的旧目录不会把它带回来。
    #[test]
    fn a_pending_delete_is_sent_until_confirmed() {
        let mut sync = DirectorySync::default();
        sync.receive(
            directory(1, &[], vec![conversation("a", None)]),
            &HashSet::new(),
            &[],
        );
        sync.close_conversation("a");
        let inbound = sync.receive(
            directory(2, &[], vec![conversation("a", None)]),
            &HashSet::new(),
            &[],
        );
        assert!(inbound.added.is_empty());
        assert_eq!(sync.commands(&[], &[]).unwrap().deletes, ["a"]);

        sync.record(DirectoryOutcome {
            deleted: vec!["a".to_string()],
            ..DirectoryOutcome::default()
        });
        assert!(sync.commands(&[], &[]).unwrap().is_empty());
    }

    /// 项目：这边新开的发打开；别处关掉的本地摘掉；这边关掉的发关闭，关成之前不被带回来。
    #[test]
    fn projects_follow_the_same_rules() {
        let mut sync = DirectorySync::default();
        sync.receive(directory(1, &["/a", "/b"], Vec::new()), &HashSet::new(), &[]);
        assert_eq!(
            sync.commands(&[], &roots(&["/a", "/b", "/new"]))
                .unwrap()
                .project_opens,
            ["/new"]
        );

        let inbound = sync.receive(
            directory(2, &["/a"], Vec::new()),
            &HashSet::new(),
            &roots(&["/a", "/b", "/new"]),
        );
        assert_eq!(inbound.projects_removed, ["/b"]);

        sync.close_project("/a");
        let inbound = sync.receive(directory(3, &["/a"], Vec::new()), &HashSet::new(), &[]);
        assert!(inbound.projects_added.is_empty());
        assert_eq!(sync.commands(&[], &[]).unwrap().project_closes, ["/a"]);
    }

    #[test]
    fn saved_order_wins_and_unknown_items_keep_their_order_after_it() {
        let items = vec!["a", "b", "c", "d"];
        let saved = roots(&["c", "a"]);
        assert_eq!(order_by_saved(items, &saved, |item| *item), ["c", "a", "b", "d"]);
    }
}
