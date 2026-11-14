//! 会话所在目录的分支名：会话状态里的 `branch` 由这里维护。
//!
//! 分支是会话目录在运行时那台机器上的事实。由运行时维护进会话状态，各端（侧栏、手机）读
//! 会话状态就有，不必各自去查、各自监听 HEAD。
//!
//! 同一个目录的会话共用一份：第一次见到这个目录时查一次分支，监听它所在仓库的 git 目录
//! （HEAD 就在那一层，非递归，各平台都只占一个 watch）；不是仓库的目录监听目录本身，等
//! `.git` 出现。最后一个会话结束或换了目录就放掉。分支变了，把这个目录下的会话状态都改了
//! 再广播，跟 [`crate::terminal_names`] 写自定义标题是一个做法。
//!
//! 记账都在一个线程里：广播状态、删会话、监听回调只往通道里发消息。广播是在提交门里调的，
//! 这里改完状态要再广播一次，不能在门里同步做。
//!
//! 不在会话目录本身、而在它上层后来 `git init` 的，看不到，直到会话换目录或重开。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender};

use notify::{RecursiveMode, Watcher as _};

use crate::event_hub::EventHubHandle;
use crate::session_state::{SessionState, bump_state_revision};
use crate::Sessions;
use crate::acp_host::AcpSessions;

enum Message {
    /// 一份会话状态刚发布出去。
    Observe {
        id: String,
        cwd: Option<String>,
        branch: Option<String>,
    },
    /// 会话从发布的状态里删掉了。
    Forget { id: String },
    /// `cwd` 的监听报了变化，重查。
    Changed { cwd: String },
}

static CHANNEL: OnceLock<Sender<Message>> = OnceLock::new();

/// 起记账线程。只在守护启动时调一次；测试不调，[`observe`]、[`forget`] 就什么都不做。
pub(crate) fn spawn(sessions: Sessions, acp_sessions: AcpSessions, event_hub: EventHubHandle) {
    let (tx, rx) = std::sync::mpsc::channel();
    if CHANNEL.set(tx.clone()).is_err() {
        return;
    }
    let worker = Worker {
        sessions,
        acp_sessions,
        event_hub,
        tx,
        tracker: Tracker::default(),
    };
    if let Err(error) = std::thread::Builder::new()
        .name("smeltd-session-branch".into())
        .spawn(move || worker.run(rx))
    {
        eprintln!("[session-branch] 起线程失败，会话不带分支名：{error}");
    }
}

/// 会话状态发布出去了（[`crate::broadcast_state`] 调）。
pub(crate) fn observe(state: &SessionState) {
    send(Message::Observe {
        id: state.id.clone(),
        cwd: state.cwd.clone().filter(|cwd| !cwd.is_empty()),
        branch: state.branch.clone(),
    });
}

/// 会话从发布的状态里删掉了。
pub(crate) fn forget(id: &str) {
    send(Message::Forget { id: id.to_string() });
}

fn send(message: Message) {
    if let Some(tx) = CHANNEL.get() {
        let _ = tx.send(message);
    }
}

/// 一个目录现在的样子：要监听哪里、分支是什么。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Head {
    /// 仓库就是它的 git 目录，不是仓库就是目录本身。
    watched: PathBuf,
    /// 不是仓库、还没有提交的仓库是 None；分离头指针是 `HEAD`（同 `git.repo_info`）。
    branch: Option<String>,
}

fn resolve(cwd: &str) -> Head {
    match smelt_git::git_dir(cwd) {
        Ok(git_dir) => Head {
            watched: git_dir,
            branch: smelt_git::load_repo_info(cwd).map(|info| info.branch),
        },
        Err(_) => Head {
            watched: PathBuf::from(cwd),
            branch: None,
        },
    }
}

/// 这批事件可能改了分支：HEAD 本身（git 写 HEAD 是先写 `HEAD.lock` 再改名，改名事件里
/// 带着 `HEAD`），或者目录里出现、删掉了 `.git`。同层的 index、FETCH_HEAD 跟分支名无关。
fn may_change_branch(paths: &[PathBuf]) -> bool {
    paths
        .iter()
        .any(|path| path.file_name().is_some_and(|name| name == "HEAD" || name == ".git"))
}

struct Dir {
    head: Head,
    sessions: usize,
    /// 放掉就停止监听。建不起来（目录没了、watch 用完）是 None，这个目录就只在会话换目录
    /// 或重开时更新。
    _watcher: Option<notify::RecommendedWatcher>,
}

/// 哪个会话在哪个目录，每个目录几个会话。只记账，不碰 git、不碰会话。
#[derive(Default)]
struct Tracker {
    cwd_of: HashMap<String, String>,
    dirs: HashMap<String, Dir>,
}

impl Tracker {
    /// 记下 `id` 在 `cwd`。第一次见到的目录用 `open` 建。返回这个目录现在的分支，没有目录
    /// 是 None。
    fn observe(
        &mut self,
        id: &str,
        cwd: Option<&str>,
        open: impl FnOnce(&str) -> Dir,
    ) -> Option<&Head> {
        if self.cwd_of.get(id).map(String::as_str) != cwd {
            self.forget(id);
            if let Some(cwd) = cwd {
                self.cwd_of.insert(id.to_string(), cwd.to_string());
                self.dirs
                    .entry(cwd.to_string())
                    .or_insert_with(|| open(cwd))
                    .sessions += 1;
            }
        }
        cwd.and_then(|cwd| self.dirs.get(cwd)).map(|dir| &dir.head)
    }

    fn forget(&mut self, id: &str) {
        let Some(cwd) = self.cwd_of.remove(id) else {
            return;
        };
        if let Some(dir) = self.dirs.get_mut(&cwd) {
            dir.sessions -= 1;
            if dir.sessions == 0 {
                self.dirs.remove(&cwd);
            }
        }
    }
}

struct Worker {
    sessions: Sessions,
    acp_sessions: AcpSessions,
    event_hub: EventHubHandle,
    /// 给监听回调用，回调把变化送回这个线程。
    tx: Sender<Message>,
    tracker: Tracker,
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        while let Ok(message) = rx.recv() {
            match message {
                Message::Observe { id, cwd, branch } => self.observe(&id, cwd.as_deref(), branch),
                Message::Forget { id } => self.tracker.forget(&id),
                Message::Changed { cwd } => self.changed(&cwd),
            }
        }
    }

    fn observe(&mut self, id: &str, cwd: Option<&str>, branch: Option<String>) {
        let tx = &self.tx;
        let head = self
            .tracker
            .observe(id, cwd, |cwd| open(cwd, tx))
            .map(|head| head.branch.clone());
        // 会话报来的和目录的不一样：刚进这个目录，或者状态被整份重建过。
        if let (Some(cwd), Some(current)) = (cwd, head)
            && current != branch
        {
            self.apply(cwd, &current);
        }
    }

    fn changed(&mut self, cwd: &str) {
        let Some(dir) = self.tracker.dirs.get_mut(cwd) else {
            return;
        };
        let head = resolve(cwd);
        if head == dir.head {
            return;
        }
        if head.watched != dir.head.watched {
            dir._watcher = watch(&head.watched, cwd, &self.tx);
        }
        let branch_changed = head.branch != dir.head.branch;
        dir.head = head;
        if branch_changed {
            let branch = dir.head.branch.clone();
            self.apply(cwd, &branch);
        }
    }

    /// 在 `cwd` 的会话，分支不是 `branch` 的都改掉再广播。
    fn apply(&self, cwd: &str, branch: &Option<String>) {
        let update = |state: &mut SessionState| {
            if state.cwd.as_deref() != Some(cwd) || state.branch == *branch {
                return None;
            }
            state.branch = branch.clone();
            bump_state_revision(state);
            Some(state.clone())
        };
        for (id, _) in self.sessions.snapshot() {
            let snapshot = self
                .sessions
                .with_live(&id, |session| update(&mut session.state.lock().unwrap()))
                .flatten();
            if let Some(snapshot) = snapshot {
                crate::broadcast_state(&self.event_hub, &snapshot);
            }
        }
        for (id, slot) in self.acp_sessions.snapshot() {
            let snapshot = self
                .acp_sessions
                .with_current(&id, &slot, |session| update(&mut session.state.lock()))
                .flatten();
            if let Some(snapshot) = snapshot {
                crate::broadcast_state(&self.event_hub, &snapshot);
            }
        }
    }
}

fn open(cwd: &str, tx: &Sender<Message>) -> Dir {
    let head = resolve(cwd);
    let watcher = watch(&head.watched, cwd, tx);
    Dir {
        head,
        sessions: 0,
        _watcher: watcher,
    }
}

fn watch(path: &Path, cwd: &str, tx: &Sender<Message>) -> Option<notify::RecommendedWatcher> {
    let tx = tx.clone();
    let key = cwd.to_string();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok_and(|event| may_change_branch(&event.paths)) {
            let _ = tx.send(Message::Changed { cwd: key.clone() });
        }
    });
    let watched = watcher.and_then(|mut watcher| {
        watcher.watch(path, RecursiveMode::NonRecursive)?;
        Ok(watcher)
    });
    match watched {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            crate::dlog(&format!(
                "[session-branch] 监听 {} 失败，{cwd} 的分支名只在会话换目录、重开时更新：{error}",
                path.display()
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(branch: &str) -> Dir {
        Dir {
            head: Head {
                watched: PathBuf::from("/repo/.git"),
                branch: Some(branch.to_string()),
            },
            sessions: 0,
            _watcher: None,
        }
    }

    /// 同一个目录的会话共用一份，最后一个走了才放掉；换目录等于离开旧目录。
    #[test]
    fn a_directory_lives_as_long_as_some_session_is_in_it() {
        let mut tracker = Tracker::default();
        let opened = std::cell::Cell::new(0);
        let open = |_: &str| {
            opened.set(opened.get() + 1);
            dir("main")
        };
        tracker.observe("a", Some("/repo"), open);
        tracker.observe("b", Some("/repo"), open);
        // 同一个会话再报一次（相位变了）不重复计数。
        tracker.observe("a", Some("/repo"), open);
        assert_eq!(opened.get(), 1);
        assert_eq!(tracker.dirs["/repo"].sessions, 2);

        tracker.observe("a", Some("/other"), open);
        assert_eq!(opened.get(), 2);
        assert_eq!(tracker.dirs["/repo"].sessions, 1);

        tracker.forget("b");
        assert!(!tracker.dirs.contains_key("/repo"));
        tracker.observe("a", None, open);
        assert!(tracker.dirs.is_empty());
        assert!(tracker.cwd_of.is_empty());
        // 没登记过的会话删掉不出错。
        tracker.forget("never");
    }

    #[test]
    fn observe_reports_the_directory_branch() {
        let mut tracker = Tracker::default();
        let head = tracker.observe("a", Some("/repo"), |_| dir("feat/x"));
        assert_eq!(head.and_then(|head| head.branch.as_deref()), Some("feat/x"));
        assert!(tracker.observe("b", None, |_| dir("main")).is_none());
    }

    #[test]
    fn only_head_and_dot_git_can_change_the_branch() {
        let git = Path::new("/repo/.git");
        assert!(may_change_branch(&[git.join("HEAD")]));
        // 改名事件两端都在：HEAD.lock → HEAD。
        assert!(may_change_branch(&[git.join("HEAD.lock"), git.join("HEAD")]));
        assert!(may_change_branch(&[PathBuf::from("/repo/.git")]));
        assert!(!may_change_branch(&[git.join("HEAD.lock")]));
        assert!(!may_change_branch(&[git.join("index")]));
        assert!(!may_change_branch(&[git.join("FETCH_HEAD")]));
        assert!(!may_change_branch(&[git.join("ORIG_HEAD")]));
        assert!(!may_change_branch(&[PathBuf::from("/repo/src/main.rs")]));
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// 不是仓库时监听目录本身；`git init`、切分支以后重查得到新分支和新的监听位置。
    #[test]
    fn resolve_follows_init_and_checkout() {
        let root = std::env::temp_dir().join(format!(
            "smelt-session-branch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let cwd = root.to_str().unwrap();
        assert_eq!(
            resolve(cwd),
            Head {
                watched: root.clone(),
                branch: None
            }
        );

        git(&root, &["init", "-q", "-b", "main"]);
        git(
            &root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--no-verify",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        let head = resolve(cwd);
        assert_eq!(head.branch.as_deref(), Some("main"));
        assert_ne!(head.watched, root);
        assert!(head.watched.join("HEAD").is_file());

        git(&root, &["checkout", "-q", "-b", "feat/x"]);
        assert_eq!(resolve(cwd).branch.as_deref(), Some("feat/x"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
