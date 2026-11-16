//! 一个运行时的工作区目录在这边的只读缓存：收到推送时算出本地要增删哪些对话、哪些名字
//! 要跟上。只有数据，不碰界面，界面那一侧见 `workspace_persist`。
//!
//! 目录只有运行时写。这边在改动发生的那一刻发单条操作（新建、关掉、换终端格子、改名、
//! 归档、置顶），不拿本地的样子和目录对账，等推送回来再变。规则：
//!
//! - 目录里见过、现在没了的对话是别处关掉的，本地摘掉；这边没见过、目录里也没有的是这边
//!   刚建、还没推回来的，留着。
//! - 关掉对话发删除，回执回来之前推送里的旧目录不会把它带回来；回执回来以后以推送为准。
//! - 操作和推送走两条连接，先后没保证：比已经处理过的、比操作回执里的版本旧的目录都是
//!   旧的，不处理，等新的推送。否则刚建成的对话会被旧目录当成别处关掉摘掉，刚关掉的又被
//!   带回来。版本号只在同一份库（`epoch`）里比；库换过就按一份新目录处理：见过的都忘掉，
//!   本地开着、新库里没有的对话报给界面重新建。
//! - 名字只从目录来：收到的就是名字该有的样子，界面和自己显示的不一样就换上。
//!
//! 项目不在这里：项目列表就是最近一份目录里的那份，开关项目直接发命令，等推送回来
//! 再变（见 `Workspace::request_project_open`）。

use std::collections::{HashMap, HashSet};

use smelt_model::workspace_directory::{WorkspaceConversation, WorkspaceRevision};

#[derive(Default)]
pub(crate) struct DirectoryCache {
    /// 已经处理过的最近一份目录的版本。没收到之前本地不摘任何对话。
    listed: Option<WorkspaceRevision>,
    /// 这份库里见过的对话。对话 id 不会重用，见过就一直算见过。
    seen: HashSet<String>,
    /// 这边关掉、删除回执还没回来的对话。
    closing: HashSet<String>,
    /// 操作回执里运行时确认过的最新版本。同一份库里比它旧的目录不含这边已经做成的改动。
    confirmed: Option<WorkspaceRevision>,
    archived: Vec<WorkspaceConversation>,
    pinned: HashSet<String>,
    /// 最近一份目录里各段对话的终端格子 id，按树里的顺序。
    terminals: HashMap<String, Vec<String>>,
}

/// 收到一份目录之后本地要做的事。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Inbound {
    /// 这个运行时的第一份目录：按冷启动恢复。
    pub(crate) first: bool,
    /// 本地还没有的对话，按目录里的顺序。
    pub(crate) added: Vec<WorkspaceConversation>,
    /// 别处关掉、归档的对话，和删掉的归档。
    pub(crate) removed: Vec<String>,
    /// 本地开着的对话在目录里的名字：(对话 id, 名字)。
    pub(crate) titles: Vec<(String, Option<String>)>,
    /// 本地开着的对话里各格终端在目录里的名字：(守护里的终端 id, 名字)。
    pub(crate) terminal_titles: Vec<(String, Option<String>)>,
    /// 运行时的库换过，本地开着、新库里没有的对话：重新建进去。
    pub(crate) recreate: Vec<String>,
}

/// 一批操作的回执，回主线程记账。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Receipts {
    /// 做成的操作回执里最新的目录版本。
    pub(crate) revision: Option<WorkspaceRevision>,
    /// 发完了的删除，不管成没成：没成的已经报给用户，之后以推送为准。
    pub(crate) deletes_done: Vec<String>,
}

impl Receipts {
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

impl DirectoryCache {
    pub(crate) fn archived(&self) -> &[WorkspaceConversation] {
        &self.archived
    }

    pub(crate) fn is_pinned(&self, id: &str) -> bool {
        self.pinned.contains(id)
    }

    pub(crate) fn loaded(&self) -> bool {
        self.listed.is_some()
    }

    /// 已经处理过的那份目录的版本。
    pub(crate) fn version(&self) -> Option<&WorkspaceRevision> {
        self.listed.as_ref()
    }

    /// 最近一份目录里这段对话的终端格子 id。目录里没有这段对话时是 `None`。
    pub(crate) fn terminal_ids(&self, conversation_id: &str) -> Option<&[String]> {
        self.terminals.get(conversation_id).map(Vec::as_slice)
    }

    /// 这份目录比已经处理过的、或比操作回执确认过的旧：不处理，更新的推送随后就到。
    /// 不同份库之间不比（见 [`WorkspaceRevision::is_older_than`]）。
    pub(crate) fn is_stale(&self, version: &WorkspaceRevision) -> bool {
        self.confirmed
            .as_ref()
            .is_some_and(|confirmed| version.is_older_than(confirmed))
            || self
                .version()
                .is_some_and(|seen| version.is_older_than(seen))
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
        let rebuilt = self
            .version()
            .is_some_and(|seen| seen.epoch != version.epoch);
        if rebuilt {
            // 运行时的库换过：旧库里见过的不算数，本地开着的对话不当成别处关掉。
            self.seen.clear();
        }
        let ids = conversations
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect::<HashSet<_>>();
        self.pinned = conversations
            .iter()
            .filter(|c| c.archived_at.is_none() && c.pinned_at.is_some())
            .map(|c| c.id.clone())
            .collect();
        let (mut archived, conversations): (Vec<_>, Vec<_>) = conversations
            .into_iter()
            .partition(|c| c.archived_at.is_some());
        archived.sort_by(|a, b| b.archived_at.cmp(&a.archived_at).then(a.id.cmp(&b.id)));
        let previous_archived = std::mem::replace(&mut self.archived, archived);
        let archived_ids: HashSet<_> = self.archived.iter().map(|c| c.id.as_str()).collect();
        let mut removed = local_conversations
            .iter()
            .filter(|id| {
                archived_ids.contains(id.as_str())
                    || (self.seen.contains(*id) && !ids.contains(*id))
            })
            .cloned()
            .collect::<Vec<_>>();
        for conversation in previous_archived {
            if !ids.contains(&conversation.id) && !removed.contains(&conversation.id) {
                removed.push(conversation.id);
            }
        }
        let recreate = if rebuilt {
            local_conversations
                .iter()
                .filter(|id| !ids.contains(*id) && !self.closing.contains(*id))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let mut added = Vec::new();
        let mut titles = Vec::new();
        let mut terminal_titles = Vec::new();
        self.terminals.clear();
        for conversation in conversations {
            if self.closing.contains(&conversation.id) {
                continue;
            }
            self.terminals.insert(
                conversation.id.clone(),
                conversation
                    .terminals
                    .iter()
                    .filter_map(|terminal| terminal.id.clone())
                    .collect(),
            );
            if !local_conversations.contains(&conversation.id) {
                added.push(conversation);
                continue;
            }
            titles.push((conversation.id.clone(), conversation.custom_title.clone()));
            terminal_titles.extend(conversation.terminals.iter().filter_map(|terminal| {
                Some((terminal.id.clone()?, terminal.custom_title.clone()))
            }));
        }
        self.seen.extend(ids);
        self.listed = Some(version);
        Inbound {
            first,
            added,
            removed,
            titles,
            terminal_titles,
            recreate,
        }
    }

    /// 本地关掉了一段对话：删除回执回来之前，推送里的旧目录不会把它带回来。
    pub(crate) fn close_conversation(&mut self, id: &str) {
        self.closing.insert(id.to_string());
    }

    pub(crate) fn record(&mut self, receipts: Receipts) {
        if let Some(revision) = receipts.revision {
            self.confirmed = Some(latest(self.confirmed.take(), revision));
        }
        for id in receipts.deletes_done {
            self.closing.remove(&id);
        }
    }
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
    use smelt_model::workspace_directory::WorkspaceTerminal;

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

    #[test]
    fn archive_removes_local_views_without_recreating_them_and_restore_adds_them() {
        let mut cache = DirectoryCache::default();
        let active = conversation("c1", Some("title"));
        cache.receive(at(1), vec![active.clone()], &ids(&["c1"]));
        let mut archived = active.clone();
        archived.archived_at = Some(10);
        let inbound = cache.receive(at(2), vec![archived.clone()], &ids(&["c1"]));
        assert_eq!(inbound.removed, ["c1"]);
        assert!(inbound.added.is_empty());
        assert_eq!(cache.archived(), &[archived]);
        let restored = cache.receive(at(3), vec![active], &ids(&[]));
        assert_eq!(restored.added.len(), 1);
        assert!(cache.archived().is_empty());
    }

    #[test]
    fn cold_start_does_not_materialize_archives_or_leave_pending_views() {
        let mut cache = DirectoryCache::default();
        let mut archived = conversation("c1", None);
        archived.archived_at = Some(10);
        let inbound = cache.receive(at(1), vec![archived], &ids(&["c1"]));
        assert!(inbound.first);
        assert!(inbound.added.is_empty());
        assert_eq!(inbound.removed, ["c1"]);
    }

    #[test]
    fn deleting_an_archive_reports_removal_for_client_metadata_cleanup() {
        let mut cache = DirectoryCache::default();
        let mut archived = conversation("c1", None);
        archived.archived_at = Some(10);
        cache.receive(at(1), vec![archived], &ids(&[]));
        let inbound = cache.receive(at(2), vec![], &ids(&[]));
        assert_eq!(inbound.removed, ["c1"]);
        assert!(cache.archived().is_empty());
    }

    #[test]
    fn pin_state_follows_the_directory() {
        let mut cache = DirectoryCache::default();
        let mut pinned = conversation("c1", None);
        pinned.pinned_at = Some(10);
        cache.receive(at(1), vec![pinned.clone()], &ids(&[]));
        assert!(cache.is_pinned("c1"));
        pinned.pinned_at = None;
        cache.receive(at(2), vec![pinned], &ids(&["c1"]));
        assert!(!cache.is_pinned("c1"));
    }

    /// 这边刚建、还没推回来的对话不会被推送摘掉；回执之后旧目录也不再处理。
    #[test]
    fn a_local_conversation_survives_a_directory_without_it() {
        let mut cache = DirectoryCache::default();
        cache.receive(at(1), vec![], &ids(&[]));
        let inbound = cache.receive(at(2), vec![], &ids(&["new"]));
        assert!(inbound.removed.is_empty());

        cache.record(Receipts {
            revision: Some(at(4)),
            ..Receipts::default()
        });
        assert!(cache.is_stale(&at(3)), "比新建回执旧");
        assert!(!cache.is_stale(&at(4)));
        let inbound = cache.receive(at(4), vec![conversation("new", None)], &ids(&["new"]));
        assert!(inbound.added.is_empty());
        assert!(inbound.removed.is_empty());
    }

    #[test]
    fn a_directory_older_than_the_last_one_is_stale() {
        let mut cache = DirectoryCache::default();
        cache.receive(at(5), vec![], &ids(&[]));
        assert!(cache.is_stale(&at(4)));
        assert!(!cache.is_stale(&at(5)));
        assert!(!cache.is_stale(&at(6)));
    }

    /// 库换过：版本不比，见过的忘掉，本地开着、新库里没有的报给界面重建。
    #[test]
    fn a_rebuilt_database_is_not_stale_and_gets_local_conversations_back() {
        let mut cache = DirectoryCache::default();
        cache.receive(
            in_epoch("old", 40),
            vec![conversation("a", None)],
            &ids(&[]),
        );
        cache.record(Receipts {
            revision: Some(in_epoch("old", 41)),
            ..Receipts::default()
        });
        assert!(!cache.is_stale(&in_epoch("new", 1)));
        let inbound = cache.receive(in_epoch("new", 1), vec![], &ids(&["a"]));
        assert!(inbound.removed.is_empty(), "不当成别处关掉");
        assert_eq!(inbound.recreate, ["a"]);

        let inbound = cache.receive(in_epoch("new", 2), vec![], &ids(&["a"]));
        assert!(inbound.recreate.is_empty(), "同一份库里不再报");
    }

    #[test]
    fn a_conversation_closed_elsewhere_is_removed_and_not_recreated() {
        let mut cache = DirectoryCache::default();
        cache.receive(at(1), vec![conversation("a", None)], &ids(&[]));
        let inbound = cache.receive(at(2), vec![], &ids(&["a"]));
        assert_eq!(inbound.removed, ["a"]);
        assert!(inbound.recreate.is_empty());
    }

    #[test]
    fn a_conversation_opened_elsewhere_is_added() {
        let mut cache = DirectoryCache::default();
        cache.receive(at(1), vec![], &ids(&[]));
        let inbound = cache.receive(at(2), vec![conversation("b", None)], &ids(&[]));
        assert_eq!(inbound.added, [conversation("b", None)]);
    }

    /// 关掉的对话在删除回执回来之前，推送里的旧目录不会把它带回来；回来之后以推送为准。
    #[test]
    fn a_closing_conversation_is_not_added_back_until_the_delete_is_done() {
        let mut cache = DirectoryCache::default();
        cache.receive(at(13), vec![conversation("a", None)], &ids(&["a"]));
        cache.close_conversation("a");
        let inbound = cache.receive(at(14), vec![conversation("a", None)], &ids(&[]));
        assert!(inbound.added.is_empty());
        assert!(cache.terminal_ids("a").is_none());

        cache.record(Receipts {
            revision: None,
            deletes_done: vec!["a".to_string()],
        });
        let inbound = cache.receive(at(15), vec![conversation("a", None)], &ids(&[]));
        assert_eq!(inbound.added.len(), 1, "删除没成：目录里还有就接回来");
    }

    #[test]
    fn names_of_local_conversations_and_terminals_are_reported() {
        let mut cache = DirectoryCache::default();
        let mut split = conversation("t", None);
        split.terminals = vec![WorkspaceTerminal {
            id: Some("pane".into()),
            custom_title: Some("测试".into()),
            ..Default::default()
        }];
        let inbound = cache.receive(
            at(1),
            vec![
                conversation("a", Some("名字")),
                split,
                conversation("b", None),
            ],
            &ids(&["a", "t"]),
        );
        assert_eq!(
            inbound.titles,
            [
                ("a".to_string(), Some("名字".to_string())),
                ("t".to_string(), None)
            ]
        );
        assert_eq!(
            inbound.terminal_titles,
            [("pane".to_string(), Some("测试".to_string()))]
        );
        assert_eq!(cache.terminal_ids("t"), Some(&["pane".to_string()][..]));
        assert_eq!(cache.terminal_ids("a"), Some(&[][..]));
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
