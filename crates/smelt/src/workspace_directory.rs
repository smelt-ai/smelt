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
//! - 命令和推送走两条连接，先后没保证：比已经处理过的、比命令回执里的版本旧的目录都是
//!   旧的，不处理，等新的推送。否则刚建成的对话会被旧目录当成别处关掉摘掉，刚关掉的又被
//!   带回来。版本号只在同一份库（`epoch`）里比；库换过就按一份新目录对账：见过的都忘掉，
//!   本地开着的对话发新建写回去。
//! - 名字只从目录来（改名走 `workspace.rename`，名字写在哪由运行时定）：发出去的对话不带
//!   名字，收到的目录里名字变了就报给界面，自己改名的回声也一样。
//!
//! 项目不在这里：项目列表就是最近一份目录里的那份，开关项目直接发命令，等推送回来
//! 再变（见 `Workspace::request_project_open`）。对话要对账，是因为它在这边有活着的
//! 终端和界面、内容一直在变；项目只有一个路径，不需要。

use std::collections::{BTreeSet, HashMap, HashSet};

use smelt_model::workspace_directory::{WorkspaceConversation, WorkspaceRevision};

#[derive(Default)]
pub(crate) struct DirectorySync {
    /// 最近一份目录的版本和里面的对话 id。没收到之前不发任何命令：内存里没有的不代表
    /// 运行时上没有。
    listed: Option<(WorkspaceRevision, HashSet<String>)>,
    /// 在目录里见过的对话（含这边新建成功的）。对话 id 不会重用，见过就一直算见过。
    known_conversations: HashSet<String>,
    /// 每段对话这边上次发出（或读到时）的样子，不带名字。只在发出成功时变。
    sent: HashMap<String, WorkspaceConversation>,
    pending_deletes: BTreeSet<String>,
    /// 命令回执里运行时确认过的最新版本。同一份库里比它旧的目录不含这边已经做成的改动。
    confirmed: Option<WorkspaceRevision>,
    /// 上一份目录里各段对话、各格终端的名字。下一份里变了才报。
    names: HashMap<String, Option<String>>,
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
    /// 名字变了的对话：(对话 id, 新名字)。
    pub(crate) retitled: Vec<(String, Option<String>)>,
    /// 名字变了的终端：(守护里的终端 id, 新名字)。
    pub(crate) renamed_terminals: Vec<(String, Option<String>)>,
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
    /// 做成的命令回执里最新的目录版本。
    pub(crate) revision: Option<WorkspaceRevision>,
}

impl DirectoryOutcome {
    pub(crate) fn confirm(&mut self, revision: WorkspaceRevision) {
        self.revision = Some(latest(self.revision.take(), revision));
    }
}

/// 回执按发出的先后到：同一份库里取大的，换了库以后来的为准。
fn latest(current: Option<WorkspaceRevision>, next: WorkspaceRevision) -> WorkspaceRevision {
    match current {
        Some(current) if next.is_older_than(&current) => current,
        _ => next,
    }
}

impl DirectorySync {
    pub(crate) fn loaded(&self) -> bool {
        self.listed.is_some()
    }

    /// 已经对过账的那份目录的版本。
    pub(crate) fn version(&self) -> Option<&WorkspaceRevision> {
        self.listed.as_ref().map(|(version, _)| version)
    }

    /// 这份目录比已经处理过的、或比命令回执确认过的旧：不处理，更新的推送随后就到。
    /// 不同份库之间不比（见 [`WorkspaceRevision::is_older_than`]）。
    pub(crate) fn is_stale(&self, version: &WorkspaceRevision) -> bool {
        self.confirmed
            .as_ref()
            .is_some_and(|confirmed| version.is_older_than(confirmed))
            || self.version().is_some_and(|seen| version.is_older_than(seen))
    }

    /// 收到一份目录里的对话。`local_conversations` 是本地已有的对话（含还在恢复的）。
    /// 调用方先用 [`Self::is_stale`] 挡掉旧的。
    pub(crate) fn receive(
        &mut self,
        version: WorkspaceRevision,
        conversations: Vec<WorkspaceConversation>,
        local_conversations: &HashSet<String>,
    ) -> Inbound {
        let first = self.listed.is_none();
        if self.version().is_some_and(|seen| seen.epoch != version.epoch) {
            // 运行时的库换过：旧库里见过的、发过的都不算数。本地开着的对话不当成别处关掉，
            // 下一轮发新建写进新库。
            self.known_conversations.clear();
            self.sent.clear();
            self.names.clear();
        }
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
        let mut renamed_terminals = Vec::new();
        for conversation in conversations {
            if self.pending_deletes.contains(&conversation.id) {
                continue;
            }
            let local = local_conversations.contains(&conversation.id);
            if self.note_name(&conversation.id, &conversation.custom_title) && local {
                retitled.push((conversation.id.clone(), conversation.custom_title.clone()));
            }
            for terminal in &conversation.terminals {
                if let Some(id) = &terminal.id
                    && self.note_name(id, &terminal.custom_title)
                    && local
                {
                    renamed_terminals.push((id.clone(), terminal.custom_title.clone()));
                }
            }
            if !local {
                // 读到的样子就是这边的起点：恢复出来没改过的不必再发一遍。
                self.sent
                    .insert(conversation.id.clone(), without_names(&conversation));
                added.push(conversation);
            }
        }
        self.known_conversations.extend(ids.iter().cloned());
        for id in &removed {
            self.sent.remove(id);
        }
        self.listed = Some((version, ids));
        Inbound {
            first,
            added,
            removed,
            retitled,
            renamed_terminals,
        }
    }

    /// 记下一个名字，和上一份目录里的不一样时返回 `true`。
    fn note_name(&mut self, id: &str, name: &Option<String>) -> bool {
        match self.names.get(id) {
            Some(previous) if previous == name => false,
            _ => {
                let first_sight = !self.names.contains_key(id);
                self.names.insert(id.to_string(), name.clone());
                // 第一次见到、本来也没名字的，不算变了。
                !(first_sight && name.is_none())
            }
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
        // 只有运行时拒收的（没有回执）时按手上这份算。
        let same_database = outcome.revision.as_ref().is_none_or(|revision| {
            self.version()
                .is_some_and(|seen| seen.epoch == revision.epoch)
        });
        if let Some(revision) = outcome.revision {
            self.confirmed = Some(latest(self.confirmed.take(), revision));
        }
        if !same_database {
            // 回执和手上的目录不是同一份库：那边做成的不一定在这份里。只记版本，下一轮
            // 按收到的目录重新算要发什么（重发的新建、删除运行时都当没事）。
            return;
        }
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

/// 这段对话不带名字的样子：发给运行时、和上次发出的比，都不看名字（名字只从目录来）。
pub(crate) fn without_names(conversation: &WorkspaceConversation) -> WorkspaceConversation {
    let mut conversation = conversation.clone();
    conversation.custom_title = None;
    for terminal in &mut conversation.terminals {
        terminal.custom_title = None;
    }
    if let Some(acp) = &mut conversation.acp {
        acp.session_title = None;
    }
    conversation
}

/// 按这台客户端记的顺序排：记过的按记的位置，没记过的（别处新建的）按原来的先后排在后面。
pub(crate) fn order_by_saved<T>(
    items: Vec<T>,
    saved: &[String],
    key: impl Fn(&T) -> &str,
) -> Vec<T> {
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

    fn at(revision: u64) -> WorkspaceRevision {
        in_epoch("e", revision)
    }

    fn in_epoch(epoch: &str, revision: u64) -> WorkspaceRevision {
        WorkspaceRevision {
            epoch: epoch.into(),
            revision,
        }
    }

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
        let inbound = sync.receive(at(1), vec![conversation("a", Some("A"))], &HashSet::new());
        assert!(inbound.first);
        assert_eq!(inbound.added, [conversation("a", Some("A"))]);

        let commands = sync
            .commands(&[without_names(&conversation("a", Some("A")))])
            .unwrap();
        assert!(commands.is_empty());
    }

    /// 这边新建的发新建；建成之前收到的目录里没有它，也不能当成别处删了。
    #[test]
    fn a_local_conversation_is_created_and_survives_an_older_directory() {
        let mut sync = DirectorySync::default();
        sync.receive(at(2), Vec::new(), &HashSet::new());

        let local = conversation("new", None);
        let commands = sync.commands(std::slice::from_ref(&local)).unwrap();
        assert_eq!(commands.creates, std::slice::from_ref(&local));

        let inbound = sync.receive(at(3), Vec::new(), &ids(&["new"]));
        assert!(inbound.removed.is_empty());

        sync.record(DirectoryOutcome {
            created: vec![local.clone()],
            ..DirectoryOutcome::default()
        });
        assert!(sync.commands(&[local]).unwrap().is_empty());
    }

    /// 建成之后，比回执旧的目录（重连时的快照晚于回执到）里没有它：那是旧的，不处理，
    /// 不能当成别处关掉了把它摘掉。
    #[test]
    fn a_directory_older_than_a_confirmed_create_is_stale() {
        let mut sync = DirectorySync::default();
        sync.receive(at(2), Vec::new(), &HashSet::new());
        let local = conversation("new", None);
        sync.record(DirectoryOutcome {
            created: vec![local],
            revision: Some(at(3)),
            ..DirectoryOutcome::default()
        });
        assert!(sync.is_stale(&at(2)), "比回执旧");
        assert!(!sync.is_stale(&at(3)), "带着这次新建的那份照常处理");
        let inbound = sync.receive(at(3), vec![conversation("new", None)], &ids(&["new"]));
        assert!(inbound.removed.is_empty() && inbound.added.is_empty());
    }

    /// 关掉之后，比回执旧的目录里还有它：不处理，不能把关掉的对话带回来。
    #[test]
    fn a_directory_older_than_a_confirmed_delete_is_stale() {
        let mut sync = DirectorySync::default();
        sync.receive(at(4), vec![conversation("a", None)], &ids(&["a"]));
        sync.close_conversation("a");
        sync.record(DirectoryOutcome {
            deleted: vec!["a".into()],
            revision: Some(at(5)),
            ..DirectoryOutcome::default()
        });
        assert!(sync.is_stale(&at(4)));
        assert!(!sync.is_stale(&at(5)));
    }

    /// 运行时的库换过（删了重建）：新目录版本号从头数也照收；本地开着的对话不当成别处
    /// 关掉，发新建写进新库。
    #[test]
    fn a_rebuilt_database_is_not_stale_and_gets_local_conversations_back() {
        let mut sync = DirectorySync::default();
        sync.receive(at(40), vec![conversation("a", None)], &HashSet::new());
        sync.record(DirectoryOutcome {
            settled: vec![conversation("a", None)],
            revision: Some(at(41)),
            ..DirectoryOutcome::default()
        });

        let rebuilt = in_epoch("rebuilt", 0);
        assert!(!sync.is_stale(&rebuilt));
        let inbound = sync.receive(rebuilt, Vec::new(), &ids(&["a"]));
        assert!(inbound.removed.is_empty(), "不能把本地开着的全摘掉");
        let commands = sync.commands(&[conversation("a", None)]).unwrap();
        assert_eq!(commands.creates, [conversation("a", None)]);

        // 写回新库以后，在新的那份里照常比新旧。
        sync.record(DirectoryOutcome {
            created: vec![conversation("a", None)],
            revision: Some(in_epoch("rebuilt", 1)),
            ..DirectoryOutcome::default()
        });
        assert!(sync.is_stale(&in_epoch("rebuilt", 0)));
        assert!(sync.commands(&[conversation("a", None)]).unwrap().is_empty());
    }

    /// 回执来自另一份库（库刚换、新目录还没到）：只记版本不记账，等新目录到了重新算。
    #[test]
    fn a_receipt_from_another_database_is_not_booked() {
        let mut sync = DirectorySync::default();
        sync.receive(at(5), Vec::new(), &HashSet::new());
        sync.record(DirectoryOutcome {
            created: vec![conversation("x", None)],
            revision: Some(in_epoch("rebuilt", 1)),
            ..DirectoryOutcome::default()
        });
        assert_eq!(
            sync.commands(&[conversation("x", None)]).unwrap().creates,
            [conversation("x", None)],
            "建在另一份库里的不算这份里有"
        );
        assert!(sync.is_stale(&in_epoch("rebuilt", 0)));
    }

    /// 运行时拒收的没有回执版本，照样记成发过，不会每次保存都重发。
    #[test]
    fn a_rejected_update_is_booked_without_a_receipt() {
        let mut sync = DirectorySync::default();
        sync.receive(at(1), vec![conversation("a", None)], &ids(&["a"]));
        let mut changed = conversation("a", None);
        changed.last_updated_at = 9;
        sync.record(DirectoryOutcome {
            settled: vec![changed.clone()],
            ..DirectoryOutcome::default()
        });
        assert!(sync.commands(&[changed]).unwrap().is_empty());
    }

    /// 已经处理过的版本之前的也是旧的。
    #[test]
    fn a_directory_older_than_the_last_one_is_stale() {
        let mut sync = DirectorySync::default();
        assert!(!sync.is_stale(&at(0)), "还没收到过时什么都收");
        sync.receive(at(7), Vec::new(), &HashSet::new());
        assert!(sync.is_stale(&at(6)));
        assert!(!sync.is_stale(&at(8)));
    }

    /// 别处关掉的对话本地摘掉，之后不再被当成这边新建的发回去。
    #[test]
    fn a_conversation_closed_elsewhere_is_removed_and_not_recreated() {
        let mut sync = DirectorySync::default();
        sync.receive(at(4), vec![conversation("a", None)], &HashSet::new());
        let inbound = sync.receive(at(5), Vec::new(), &ids(&["a"]));
        assert_eq!(inbound.removed, ["a"]);
        // 摘掉之前又保存了一次：不能建回去。
        let commands = sync.commands(&[conversation("a", None)]).unwrap();
        assert!(commands.creates.is_empty() && commands.updates.is_empty());
    }

    /// 别处新开的对话交给恢复。
    #[test]
    fn a_conversation_opened_elsewhere_is_added() {
        let mut sync = DirectorySync::default();
        sync.receive(at(6), Vec::new(), &HashSet::new());
        let inbound = sync.receive(at(7), vec![conversation("b", None)], &HashSet::new());
        assert!(!inbound.first);
        assert_eq!(inbound.added, [conversation("b", None)]);
    }

    /// 只发这边变了的；收到的推送和这边算出来的不一样（别处的派生字段），不会因此发回去。
    #[test]
    fn only_local_changes_are_sent() {
        let mut sync = DirectorySync::default();
        sync.receive(at(8), vec![conversation("a", None)], &HashSet::new());
        let mut theirs = conversation("a", None);
        theirs.last_updated_at = 99;
        sync.receive(at(9), vec![theirs], &ids(&["a"]));
        assert!(
            sync.commands(&[conversation("a", None)])
                .unwrap()
                .is_empty()
        );

        let mut mine = conversation("a", None);
        mine.last_updated_at = 5;
        assert_eq!(sync.commands(&[mine.clone()]).unwrap().updates, [mine]);
    }

    /// 名字只从目录来：变了报一次（自己改名的回声也报，界面就靠它更新），没变不报；
    /// 终端分屏的名字一样。
    #[test]
    fn names_from_the_directory_are_reported_when_they_change() {
        let mut sync = DirectorySync::default();
        let mut split = conversation("t", None);
        split.terminals = vec![smelt_model::workspace_directory::WorkspaceTerminal {
            id: Some("pane".into()),
            ..Default::default()
        }];
        sync.receive(at(1),
            vec![conversation("a", Some("old")), split.clone()],
            &HashSet::new(),
        );

        let inbound = sync.receive(at(2),
            vec![conversation("a", Some("new")), split.clone()],
            &ids(&["a", "t"]),
        );
        assert_eq!(
            inbound.retitled,
            [("a".to_string(), Some("new".to_string()))]
        );
        assert!(inbound.renamed_terminals.is_empty());

        split.terminals[0].custom_title = Some("测试".into());
        let inbound = sync.receive(at(3),
            vec![conversation("a", Some("new")), split.clone()],
            &ids(&["a", "t"]),
        );
        assert!(inbound.retitled.is_empty(), "没变不报");
        assert_eq!(
            inbound.renamed_terminals,
            [("pane".to_string(), Some("测试".to_string()))]
        );
    }

    /// 发出去的对话不带名字：本地名字和目录里的不一样，也不会因此发改动。
    #[test]
    fn names_never_cause_an_update() {
        let mut sync = DirectorySync::default();
        sync.receive(at(1),
            vec![conversation("a", Some("目录里的"))],
            &HashSet::new(),
        );
        assert!(
            sync.commands(&[without_names(&conversation("a", Some("本地的")))])
                .unwrap()
                .is_empty()
        );
    }

    /// 关掉的对话发删除，删成之前推送里的旧目录不会把它带回来。
    #[test]
    fn a_pending_delete_is_sent_until_confirmed() {
        let mut sync = DirectorySync::default();
        sync.receive(at(13), vec![conversation("a", None)], &HashSet::new());
        sync.close_conversation("a");
        let inbound = sync.receive(at(14), vec![conversation("a", None)], &HashSet::new());
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
        assert_eq!(
            order_by_saved(items, &saved, |item| *item),
            ["c", "a", "b", "d"]
        );
    }
}
