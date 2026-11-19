//! 技能：这台电脑上用户级和当前项目里的 agent 技能。看谁能用、补兼容链接、改作用范围、
//! 处理同名副本、迁移旧位置、导入、删除。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。技能读写的是本机磁盘，只认本机项目；
//! 磁盘是唯一真相：每次写完都重扫，界面不留业务副本。
//!
//! 磁盘操作都在后台线程做，结果缓存在视图上，render 只画缓存。什么时候重扫全靠事件：建出来、
//! 露出来、本机项目换了、做完一次写操作、手动刷新。agent 自己往目录里放技能没有通知，所以
//! 每次露出来都重扫一遍，不在后台定时扫。

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::*;
use rust_i18n::t;
use smelt_plugin_api::ui::field::{InputEvent, InputState};
use smelt_plugin_api::{
    self as api, HostEvent, PluginView, RuntimeKey, ViewHandle, ViewSpec, serde_json, text,
};

mod agents;
mod fsx;
mod ops;
mod render;
mod scan;
#[cfg(all(test, unix))]
mod tests;

use ops::{ScopeRequest, SourceInfo};
use scan::{Places, Scan, Skill};

rust_i18n::i18n!("locales", fallback = "en");

const OWNER: &str = "smelt.skills";
const VIEW_ID: &str = "smelt.skills/main";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    api::register_views(
        OWNER,
        vec![ViewSpec {
            name: "main".into(),
            title: text!("view.title"),
            icon: api::ui::IconName::BookOpen.path(),
            draws_top_bar: false,
            instances: false,
            settings: None,
            fills: Vec::new(),
            build: Rc::new(|window, cx| {
                Rc::new(cx.new(|cx| SkillsPanel::new(window, cx))) as Rc<dyn ViewHandle>
            }),
        }],
        cx,
    )
    .expect("技能的视图表写错了");
}

/// 按数量挑 `.one` / `.other`。
fn plural(key: &str, count: usize) -> String {
    let form = if count == 1 { "one" } else { "other" };
    t!(&format!("{key}.{form}"), count = count).into_owned()
}

/// 几个名字连成一串，分隔符跟语言走。
fn join(names: &[impl AsRef<str>]) -> String {
    names
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(&t!("list.separator"))
}

/// 缺兼容链接、能一键补上的技能。有同名冲突的先选保留哪份，不在这里补。
fn needs_repair(skill: &Skill) -> bool {
    !skill.missing_bridges.is_empty() && skill.conflicts.is_empty()
}

/// 上一次操作的结果。
struct Notice {
    error: bool,
    text: SharedString,
}

/// 预览：要做的几步，导入时还有会不会换掉同名技能。
struct Preview {
    steps: Vec<String>,
    replaces: bool,
}

/// 「给谁」「层级」的表单和它的预览。预览在后台算，只认最后一次的结果。
struct Form {
    request: ScopeRequest,
    /// `None` 是还在算（或没什么可算）。
    plan: Option<Result<Preview, String>>,
    token: u64,
}

impl Form {
    fn new() -> Self {
        Self {
            request: ScopeRequest {
                universal: true,
                labels: Vec::new(),
                project_scope: false,
            },
            plan: None,
            token: 0,
        }
    }

    /// 有步骤可做才能提交。
    fn ready(&self) -> bool {
        matches!(&self.plan, Some(Ok(preview)) if !preview.steps.is_empty())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormKind {
    Scope,
    Import,
}

/// 导入面板：来源，和装到哪里的表单。
struct Import {
    form: Form,
    source: Option<PathBuf>,
    info: Option<SourceInfo>,
    picking: bool,
}

pub struct SkillsPanel {
    /// 取不到用户主目录时 `None`，面板只说一句出错。
    home: Option<PathBuf>,
    trash: PathBuf,
    /// 当前项目在本机时的根目录（解开软链后的）。
    project: Option<PathBuf>,
    scan: Rc<Scan>,
    /// 扫一次加一：晚到的旧结果丢掉。
    scan_gen: u64,
    notice: Option<Notice>,
    /// 展开的是哪一行（技能目录）。一次只展开一行。
    expanded: Option<PathBuf>,
    /// 展开那一行的作用范围表单开着没有。
    editing: bool,
    scope: Form,
    importing: bool,
    import: Import,
    /// 同名冲突里各自选了留哪一份：主条目目录 → 选中的副本目录。
    keep: HashMap<PathBuf, PathBuf>,
    search: Entity<InputState>,
    scroll: ScrollHandle,
    /// 上次知道的是否看得见：从看不见变成看得见才重扫。
    shown: bool,
    _subscriptions: Vec<Subscription>,
}

/// 当前项目在本机时的根目录；远程项目或没有项目是 `None`。
fn local_project(cx: &App) -> Option<PathBuf> {
    api::active_context(cx)
        .project
        .filter(|project| project.machine == RuntimeKey::Local)
        .map(|project| PathBuf::from(project.root))
}

impl SkillsPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("search.placeholder")));
        let events = api::events(cx);
        let subscriptions = vec![
            cx.subscribe_in(&events, window, |this, _, event: &HostEvent, window, cx| {
                this.on_host_event(event, window, cx);
            }),
            // 过滤在内存里做，键入只要重画。
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        // 取真实路径：扫描结果里的目录都是解开软链后的，比较时两边要一致。
        let home = dirs::home_dir().map(|home| fsx::canonical(&home));
        let trash = api::paths::smelt_home()
            .or_else(|| home.as_ref().map(|home| home.join(".smelt")))
            .unwrap_or_default()
            .join("trash")
            .join("skills");
        let mut panel = Self {
            home,
            trash,
            project: None,
            scan: Rc::default(),
            scan_gen: 0,
            notice: None,
            expanded: None,
            editing: false,
            scope: Form::new(),
            importing: false,
            import: Import {
                form: Form::new(),
                source: None,
                info: None,
                picking: false,
            },
            keep: HashMap::new(),
            search,
            scroll: ScrollHandle::new(),
            shown: api::host(cx).view_shown(VIEW_ID, cx),
            _subscriptions: subscriptions,
        };
        panel.reload(cx);
        panel
    }

    fn on_host_event(&mut self, event: &HostEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HostEvent::LocaleChanged => {
                self.search.update(cx, |search, cx| {
                    search.set_placeholder(t!("search.placeholder"), window, cx);
                });
                cx.notify();
            }
            HostEvent::ShownViewsChanged => {
                let shown = api::host(cx).view_shown(VIEW_ID, cx);
                let appeared = shown && !self.shown;
                self.shown = shown;
                if appeared {
                    self.reload(cx);
                }
            }
            HostEvent::ActiveContextChanged => {
                let project = local_project(cx).map(|root| fsx::canonical(&root));
                if project != self.project {
                    self.reload(cx);
                }
            }
            _ => {}
        }
    }

    fn places(&self) -> Option<Places> {
        Some(Places {
            home: self.home.clone()?,
            trash: self.trash.clone(),
            project: self.project.clone(),
        })
    }

    fn installed(&self) -> &BTreeSet<&'static str> {
        &self.scan.installed
    }

    fn skill_at(&self, dir: &Path) -> Option<&Skill> {
        self.scan.skills.iter().find(|skill| skill.dir == dir)
    }

    // ------------------------------------------------------------ 读盘

    /// 重扫磁盘，然后让展开的那一行和表单对上磁盘。
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.project = local_project(cx).map(|root| fsx::canonical(&root));
        let Some(places) = self.places() else {
            self.notice = Some(Notice {
                error: true,
                text: t!("error.no_home").into(),
            });
            cx.notify();
            return;
        };
        self.scan_gen += 1;
        let generation = self.scan_gen;
        let task = cx.background_spawn(async move { scan::scan(&places) });
        cx.spawn(async move |this, cx| {
            let scan = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.scan_gen == generation {
                    this.apply_scan(scan, cx);
                }
            });
        })
        .detach();
    }

    fn apply_scan(&mut self, scan: Scan, cx: &mut Context<Self>) {
        self.scan = Rc::new(scan);
        if let Some(dir) = &self.expanded
            && self.skill_at(dir).is_none()
        {
            self.expanded = None;
            self.editing = false;
        }
        let live: BTreeSet<&PathBuf> = self.scan.skills.iter().map(|skill| &skill.dir).collect();
        self.keep.retain(|dir, _| live.contains(dir));
        if self.editing {
            self.begin_scope(cx);
        } else if self.importing {
            self.refresh_plan(FormKind::Import, cx);
        }
        cx.notify();
    }

    // ------------------------------------------------------------ 表单

    fn form_mut(&mut self, kind: FormKind) -> &mut Form {
        match kind {
            FormKind::Scope => &mut self.scope,
            FormKind::Import => &mut self.import.form,
        }
    }

    fn form(&self, kind: FormKind) -> &Form {
        match kind {
            FormKind::Scope => &self.scope,
            FormKind::Import => &self.import.form,
        }
    }

    /// 作用范围表单以展开那一行现在的作用范围作初值，并算好预览。
    fn begin_scope(&mut self, cx: &mut Context<Self>) {
        let Some(skill) = self.expanded.as_deref().and_then(|dir| self.skill_at(dir)) else {
            return;
        };
        self.scope.request = ScopeRequest::of(skill);
        self.refresh_plan(FormKind::Scope, cx);
    }

    fn set_universal(&mut self, kind: FormKind, universal: bool, cx: &mut Context<Self>) {
        self.form_mut(kind).request.universal = universal;
        self.refresh_plan(kind, cx);
    }

    /// 换层级时把这一层级没有目录的 agent 去掉。没有打开项目时选不了项目级。
    fn set_level(&mut self, kind: FormKind, project_scope: bool, cx: &mut Context<Self>) {
        if project_scope && self.project.is_none() {
            return;
        }
        let allowed: Vec<&'static str> = ops::scope_choices(self.installed(), project_scope)
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        let request = &mut self.form_mut(kind).request;
        request.project_scope = project_scope;
        request.labels.retain(|label| allowed.contains(label));
        self.refresh_plan(kind, cx);
    }

    fn toggle_label(&mut self, kind: FormKind, label: &'static str, cx: &mut Context<Self>) {
        let request = &mut self.form_mut(kind).request;
        if request.universal {
            return;
        }
        match request.labels.iter().position(|picked| *picked == label) {
            Some(index) => {
                request.labels.remove(index);
            }
            None => request.labels.push(label),
        }
        self.refresh_plan(kind, cx);
    }

    /// 表单变了：预览要重算（读盘，放后台）。
    fn refresh_plan(&mut self, kind: FormKind, cx: &mut Context<Self>) {
        let Some(places) = self.places() else {
            return;
        };
        let target = match kind {
            FormKind::Scope => self.expanded.clone(),
            FormKind::Import => self.import.source.clone(),
        };
        let form = self.form_mut(kind);
        form.token += 1;
        form.plan = None;
        let token = form.token;
        let request = form.request.clone();
        cx.notify();
        let Some(target) = target else {
            return;
        };
        let task = cx.background_spawn(async move {
            match kind {
                FormKind::Scope => {
                    ops::plan_scope(&places, &target, &request).map(|actions| Preview {
                        steps: actions
                            .iter()
                            .map(|action| ops::describe(&places, action))
                            .collect(),
                        replaces: false,
                    })
                }
                FormKind::Import => {
                    ops::plan_import(&places, &request, &target).map(|plan| Preview {
                        steps: plan
                            .actions
                            .iter()
                            .map(|action| ops::describe(&places, action))
                            .collect(),
                        replaces: plan.replaces,
                    })
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let plan = task.await;
            let _ = this.update(cx, |this, cx| {
                let form = this.form_mut(kind);
                if form.token == token {
                    form.plan = Some(plan);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ 写操作

    /// 后台执行一次写操作，然后重扫：磁盘才是唯一真相。`done` 拿到成功的结果，可以给出替换
    /// 默认「完成」的那句话，也可以挪展开、收表单。
    fn perform<T: Send + 'static>(
        &mut self,
        label: String,
        body: impl FnOnce(&Places) -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&mut Self, T) -> Option<Notice> + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(places) = self.places() else {
            return;
        };
        let task = cx.background_spawn(async move { body(&places) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(value) => done(this, value).unwrap_or_else(|| Notice {
                        error: false,
                        text: t!("notice.done", action = label).into(),
                    }),
                    Err(error) => Notice {
                        error: true,
                        text: t!("notice.failed", action = label, error = error).into(),
                    },
                });
                this.reload(cx);
            });
        })
        .detach();
    }

    /// 打开或收起展开那一行的作用范围表单。打开时以磁盘上的状态作初值。
    fn toggle_scope(&mut self, cx: &mut Context<Self>) {
        self.editing = !self.editing;
        if self.editing {
            self.importing = false;
            self.begin_scope(cx);
        }
        cx.notify();
    }

    /// 打开或收起导入面板。每次打开都从头来。
    fn toggle_import(&mut self, cx: &mut Context<Self>) {
        self.importing = !self.importing;
        if self.importing {
            self.editing = false;
            self.import = Import {
                form: Form::new(),
                source: None,
                info: None,
                picking: false,
            };
        }
        cx.notify();
    }

    /// 点一行：展开或收起。换一行时，上一行没提交的作用范围表单一起丢掉。
    fn toggle_row(&mut self, dir: &Path, cx: &mut Context<Self>) {
        self.expanded = if self.expanded.as_deref() == Some(dir) {
            None
        } else {
            Some(dir.to_path_buf())
        };
        self.editing = false;
        cx.notify();
    }

    /// 写操作把技能挪到了新目录：展开跟过去（重扫完它就在了）。
    fn follow(&mut self, destination: PathBuf) {
        self.expanded = Some(destination);
    }

    fn apply_scope(&mut self, cx: &mut Context<Self>) {
        let Some(skill) = self.expanded.as_deref().and_then(|dir| self.skill_at(dir)) else {
            return;
        };
        if !self.scope.ready() {
            return;
        }
        let dir = skill.dir.clone();
        let request = self.scope.request.clone();
        self.perform(
            t!("action.rescope", name = skill.name).into_owned(),
            move |places| ops::apply_scope(places, &dir, &request),
            |this, destination| {
                this.editing = false;
                this.follow(destination);
                None
            },
            cx,
        );
    }

    fn pick_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.import.picking {
            return;
        }
        self.import.picking = true;
        cx.notify();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("import.pick_confirm").into()),
        });
        let places = self.places();
        cx.spawn_in(window, async move |this, cx| {
            let picked = match rx.await {
                Ok(Ok(paths)) => Ok(paths.and_then(|paths| paths.into_iter().next())),
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Ok(None),
            };
            let inspected = match (picked, places) {
                (Ok(Some(source)), Some(places)) => {
                    let task = cx.background_spawn(async move {
                        ops::inspect_import(&places, &source).map(|info| (source, info))
                    });
                    Some(task.await)
                }
                (Ok(_), _) => None,
                (Err(error), _) => Some(Err(error)),
            };
            let _ = this.update(cx, |this, cx| {
                this.import.picking = false;
                match inspected {
                    Some(Ok((source, info))) => {
                        this.import.source = Some(source);
                        this.import.info = Some(info);
                        this.refresh_plan(FormKind::Import, cx);
                    }
                    Some(Err(error)) => {
                        this.notice = Some(Notice {
                            error: true,
                            text: t!("notice.pick_failed", error = error).into(),
                        });
                    }
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 导入，然后展开装好的那个技能。
    fn submit_import(&mut self, cx: &mut Context<Self>) {
        let (Some(source), Some(info)) = (self.import.source.clone(), &self.import.info) else {
            return;
        };
        if !self.import.form.ready() {
            return;
        }
        let request = self.import.form.request.clone();
        self.perform(
            t!("action.import", name = info.name).into_owned(),
            move |places| ops::import(places, &request, &source),
            |this, destination| {
                this.importing = false;
                this.follow(destination);
                None
            },
            cx,
        );
    }

    fn repair(&mut self, skill: &Skill, cx: &mut Context<Self>) {
        let missing: Vec<&str> = skill
            .coverage
            .iter()
            .filter(|row| row.access == scan::Access::Missing)
            .map(|row| row.agent)
            .collect();
        let label = t!("action.repair", name = skill.name, agents = join(&missing)).into_owned();
        let skill = skill.clone();
        self.perform(
            label,
            move |places| ops::repair_bridges(places, &skill),
            |_, ()| None,
            cx,
        );
    }

    fn repair_all(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<Skill> = self
            .scan
            .skills
            .iter()
            .filter(|skill| needs_repair(skill))
            .cloned()
            .collect();
        self.perform(
            t!("action.repair_all").into_owned(),
            move |places| {
                missing
                    .iter()
                    .try_for_each(|skill| ops::repair_bridges(places, skill))
            },
            |_, ()| None,
            cx,
        );
    }

    fn trash(&mut self, skill: &Skill, cx: &mut Context<Self>) {
        let label = t!("delete.action", name = skill.name).into_owned();
        let skill = skill.clone();
        self.perform(
            label,
            move |places| ops::trash_skill(places, &skill),
            |_, ()| None,
            cx,
        );
    }

    fn prune(&mut self, cx: &mut Context<Self>) {
        let broken = self.scan.broken.clone();
        self.perform(
            t!("broken.action").into_owned(),
            move |_| ops::prune_links(&broken),
            |_, ()| None,
            cx,
        );
    }

    fn resolve_conflict(&mut self, skill: &Skill, cx: &mut Context<Self>) {
        let dir = skill.dir.clone();
        let keep = self.keep.get(&dir).cloned().unwrap_or_else(|| dir.clone());
        self.perform(
            t!("conflict.action", name = skill.name).into_owned(),
            move |places| ops::resolve_conflict(places, &dir, &keep),
            |_, ()| None,
            cx,
        );
    }

    fn migrate(&mut self, cx: &mut Context<Self>) {
        self.perform(
            t!("migrate.action").into_owned(),
            ops::migrate_legacy,
            |_, outcome| Some(migration_notice(&outcome)),
            cx,
        );
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        self.reload(cx);
    }

    /// 在系统文件管理器里打开。回收目录可能还没建过，先建出来。
    fn open_path(&mut self, path: &Path, create: bool, cx: &mut Context<Self>) {
        if create && let Err(error) = std::fs::create_dir_all(path) {
            self.notice = Some(Notice {
                error: true,
                text: error.to_string().into(),
            });
            cx.notify();
            return;
        }
        cx.open_with_system(path);
    }
}

/// 迁移的结果：迁走几个、几个迁不动。迁不动要处理，用出错的样子。
fn migration_notice(outcome: &ops::Migration) -> Notice {
    let mut lines = Vec::new();
    if !outcome.migrated.is_empty() {
        lines.push(
            t!(
                &format!(
                    "migrate.moved.{}",
                    if outcome.migrated.len() == 1 {
                        "one"
                    } else {
                        "other"
                    }
                ),
                count = outcome.migrated.len(),
                names = join(&outcome.migrated)
            )
            .into_owned(),
        );
    }
    if !outcome.skipped.is_empty() {
        lines.push(
            t!(
                &format!(
                    "migrate.skipped.{}",
                    if outcome.skipped.len() == 1 {
                        "one"
                    } else {
                        "other"
                    }
                ),
                count = outcome.skipped.len(),
                names = join(&outcome.skipped)
            )
            .into_owned(),
        );
    }
    if lines.is_empty() {
        lines.push(t!("migrate.none").into_owned());
    }
    Notice {
        error: !outcome.skipped.is_empty(),
        text: lines.join("\n").into(),
    }
}

impl PluginView for SkillsPanel {
    fn open(&mut self, _args: &serde_json::Value, _window: &mut Window, cx: &mut Context<Self>) {
        // 打开就是要看：不等「看得见的视图变了」那条事件。
        self.shown = true;
        self.reload(cx);
    }
}
