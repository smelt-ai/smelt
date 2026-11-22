//! 盯着一个放插件的目录：里面装、改、删一个插件，只把那一个重新打包送进引擎，不用重编 App。
//!
//! 打包阻塞，在自己的一条线程上做。没有事时它睡在 channel 上；面板关掉，监听和发送端都放掉，
//! 线程随之结束。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

use notify::Watcher as _;
use nsh_core::element::description::SetDescription;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    host::Bridge,
    plugins::{Change, ENTRY, Plugins},
};

/// 一次保存常是一串事件：静下来这么久才重装，一串只装一次。
const QUIET: Duration = Duration::from_millis(200);

enum Input {
    /// 这个插件的文件变了；`None` 是说不清哪个，全都重看一遍。
    Changed(Option<String>),
    /// 引擎停了。装到一半停的，那个插件就是起因。
    Stopped { loading: Option<String> },
}

pub(crate) struct Live {
    inputs: mpsc::Sender<Input>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Live {
    /// `stage` 是只归这一份用的临时目录，结束时删掉。改动经 `changes` 交给主线程。
    pub fn start(
        sources: PathBuf,
        stage: PathBuf,
        shell: &'static str,
        ui_sets: Vec<SetDescription>,
        bridge: Bridge,
        changes: UnboundedSender<Change>,
    ) -> Self {
        let (inputs, received) = mpsc::channel();
        // 先盯上再读第一遍，中间改的不会漏。
        let watcher = watch(&sources, inputs.clone())
            .inspect_err(|error| {
                eprintln!("[nsh-pilot] 盯不住 {}：{error}", sources.display());
            })
            .ok();
        std::thread::spawn(move || {
            match Plugins::new(&stage, shell, ui_sets) {
                Ok(plugins) => Worker {
                    sources,
                    plugins,
                    bridge,
                    changes,
                    stopped: false,
                }
                .run(&received),
                Err(error) => eprintln!("[nsh-pilot] 外壳打不了包：{error}"),
            }
            if let Err(error) = fs::remove_dir_all(&stage) {
                eprintln!("[nsh-pilot] 删不掉 {}：{error}", stage.display());
            }
        });
        Self {
            inputs,
            _watcher: watcher,
        }
    }

    /// 报引擎停了。
    pub fn stopped(&self, loading: Option<String>) {
        // 收的一头只在外壳打不了包时先没，那时也没有引擎可停。
        let _ = self.inputs.send(Input::Stopped { loading });
    }
}

struct Worker {
    sources: PathBuf,
    plugins: Plugins,
    bridge: Bridge,
    changes: UnboundedSender<Change>,
    stopped: bool,
}

impl Worker {
    fn run(&mut self, inputs: &mpsc::Receiver<Input>) {
        for id in self.on_disk() {
            self.reload(&id);
        }
        if self.changes.send(self.plugins.start()).is_err() {
            return;
        }
        while let Ok(first) = inputs.recv() {
            let mut batch = vec![first];
            loop {
                match inputs.recv_timeout(QUIET) {
                    Ok(next) => batch.push(next),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            if !self.handle(batch) {
                return;
            }
        }
    }

    /// 主线程那头没了是 `false`。
    fn handle(&mut self, batch: Vec<Input>) -> bool {
        let mut changed = BTreeSet::new();
        let mut restart = false;
        for input in batch {
            match input {
                Input::Changed(Some(id)) => {
                    changed.insert(id);
                }
                Input::Changed(None) => {
                    changed.extend(self.on_disk());
                    changed.extend(self.plugins.installed().map(str::to_owned));
                }
                Input::Stopped { loading: Some(id) } => {
                    eprintln!("[nsh-pilot] 装 {id} 时引擎停了：去掉它重起，改好保存再装上");
                    self.plugins.remove(&id);
                    restart = true;
                }
                Input::Stopped { loading: None } => {
                    // 指不出是谁，马上重起只会再停一次：等有人改了插件再起。
                    eprintln!("[nsh-pilot] 引擎停了，指不出是哪个插件：改动任一插件就重起");
                    self.stopped = true;
                }
            }
        }
        let updates: Vec<Change> = changed.iter().filter_map(|id| self.reload(id)).collect();
        let updates = if restart || (self.stopped && !updates.is_empty()) {
            self.stopped = false;
            vec![self.plugins.start()]
        } else {
            updates
        };
        updates
            .into_iter()
            .all(|change| self.changes.send(change).is_ok())
    }

    fn reload(&mut self, id: &str) -> Option<Change> {
        let dir = self.sources.join(id);
        if !dir.join(ENTRY).is_file() {
            let installed = self.plugins.installed().any(|installed| installed == id);
            return installed.then(|| self.plugins.remove(id));
        }
        let loaded = texts(&dir).and_then(|texts| Ok((self.plugins.install(id, &dir)?, texts)));
        match loaded {
            Ok((change, texts)) => {
                // 先换文案再送脚本：它一起来就会来取。
                self.bridge.set_texts(id, texts);
                Some(change)
            }
            Err(error) => {
                eprintln!("[nsh-pilot] {id} 没换上，上一份照常跑：{error}");
                None
            }
        }
    }

    fn on_disk(&self) -> BTreeSet<String> {
        let entries = match fs::read_dir(&self.sources) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("[nsh-pilot] 读不了 {}：{error}", self.sources.display());
                return BTreeSet::new();
            }
        };
        entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect()
    }
}

/// `locales/` 里的文案（语言 → 键 → 字），和 gpui-shell 那套插件用同一个加载函数读。
fn texts(dir: &Path) -> Result<serde_json::Value, String> {
    let dir = dir.join("locales");
    let texts: BTreeMap<String, BTreeMap<String, String>> = if dir.is_dir() {
        rust_i18n_support::try_load_locales(&dir.to_string_lossy(), |_| false, true)?
    } else {
        BTreeMap::new()
    };
    Ok(serde_json::json!(texts))
}

fn watch(
    sources: &Path,
    inputs: mpsc::Sender<Input>,
) -> notify::Result<notify::RecommendedWatcher> {
    // FSEvents 报解开软链后的路径。
    let root = fs::canonicalize(sources)?;
    let matched = root.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            let _ = inputs.send(Input::Changed(None));
            return;
        };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        if event.paths.is_empty() {
            let _ = inputs.send(Input::Changed(None));
        }
        for path in &event.paths {
            let Ok(relative) = path.strip_prefix(&matched) else {
                continue;
            };
            let parts: Vec<_> = relative
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect();
            if parts.iter().any(|part| part.starts_with('.')) {
                continue;
            }
            let _ = inputs.send(Input::Changed(parts.into_iter().next()));
        }
    })?;
    watcher.watch(&root, notify::RecursiveMode::Recursive)?;
    Ok(watcher)
}
