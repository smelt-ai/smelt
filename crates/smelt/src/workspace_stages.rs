//! 多块舞台。布局里每块 `smelt.stage` 各显示一样东西：一个会话或一个页面（智能体、自动化、
//! 能力库、远程桌面），收哪几类由它 `info.panel` 里的 `accepts` 定。
//!
//! 有焦点的那块显示工作台的「当前」：当前会话（按对话 id 记）和 `nav` 的页。标题、文件、变更、
//! 上一个 / 下一个会话、通知都跟着它。焦点在哪块、别的几块各显示着什么，记在 [`Stages`] 里，
//! 只在内存（舞台在 Dock 里没有稳定的名字，见 `docs/workspace-layouts.md`「几块舞台」）。
//! 有焦点的那块不在那儿另记，所以每样东西只存一处。
//!
//! 改「当前」都先经这里分舞台：项目会话走 [`Workspace::set_current_session`]，页面走
//! [`Workspace::route_to_stage`]。布局变了、会话没了、恢复完，[`Workspace::settle_stages`]
//! 按新的事实重新放一遍。
//!
//! 只有一块舞台的布局里焦点总在它身上，别处没有东西。

use std::collections::HashMap;

use gpui::{App, Context, Window};
use gpui_component::dock::PanelId;
use serde_json::Value;

use crate::plugins::LayoutStage;
use crate::{Session, SessionKind, Workspace, WorkspaceRoute};

/// 舞台上能放的东西，三类：终端会话、对话、页面（智能体、自动化、能力库、远程桌面）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StageContent {
    Terminal,
    Conversation,
    Page,
}

impl StageContent {
    pub(crate) const ALL: [Self; 3] = [Self::Terminal, Self::Conversation, Self::Page];

    /// `accepts` 里写的名字。
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Conversation => "conversation",
            Self::Page => "page",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// 一块舞台收哪几类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Accepts {
    pub(crate) terminal: bool,
    pub(crate) conversation: bool,
    pub(crate) page: bool,
}

impl Accepts {
    pub(crate) const ALL: Self = Self {
        terminal: true,
        conversation: true,
        page: true,
    };
    const NONE: Self = Self {
        terminal: false,
        conversation: false,
        page: false,
    };

    pub(crate) fn contains(self, kind: StageContent) -> bool {
        match kind {
            StageContent::Terminal => self.terminal,
            StageContent::Conversation => self.conversation,
            StageContent::Page => self.page,
        }
    }

    fn insert(&mut self, kind: StageContent) {
        match kind {
            StageContent::Terminal => self.terminal = true,
            StageContent::Conversation => self.conversation = true,
            StageContent::Page => self.page = true,
        }
    }

    /// 舞台 `info.panel` 里的 `accepts`：没写、写 `null` 是全收；是名字的数组就收这几类，
    /// 不认识的名字忽略（别的版本可能多一类）。
    pub(crate) fn from_info(info: &Value) -> Self {
        let Some(names) = info.get("accepts").and_then(Value::as_array) else {
            return Self::ALL;
        };
        let mut accepts = Self::NONE;
        for kind in names
            .iter()
            .filter_map(Value::as_str)
            .filter_map(StageContent::parse)
        {
            accepts.insert(kind);
        }
        accepts
    }
}

/// 没有焦点的舞台显示着的东西，记的是工作台认得的 id：会话的对话 id，或者页面的名字。
/// 有焦点的那块不记这个，它显示的是工作台的当前会话或页面。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StageItem {
    Session(String),
    Page(String),
}

/// 一块舞台：运行时的 id（见 `workspace_shell::stage_id`）、收哪几类、它在 Dock 里的面板。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StageSpec {
    pub(crate) id: String,
    pub(crate) accepts: Accepts,
    pub(crate) panel: PanelId,
}

/// 几块舞台和它们显示着什么。只在内存里，跟着 Dock 的树重建（[`Stages::set_specs`]）。
#[derive(Default)]
pub(crate) struct Stages {
    /// 按 Dock 树的先后。
    specs: Vec<StageSpec>,
    /// 有焦点的那块。没有、或者那块没了是第一块。
    focus: Option<String>,
    /// 没有焦点的舞台显示着的东西。
    items: HashMap<String, StageItem>,
    /// 有过焦点的先后，最近的在后。要换一块舞台放东西时先挑最近用过的。
    recent: Vec<String>,
}

impl Stages {
    /// 换成 Dock 现在的几块舞台。没了的那几块记着的东西、焦点、先后都丢掉。
    pub(crate) fn set_specs(&mut self, specs: Vec<StageSpec>) {
        let known = |id: &str| specs.iter().any(|spec| spec.id == id);
        self.items.retain(|id, _| known(id));
        self.recent.retain(|id| known(id));
        if self.focus.as_deref().is_some_and(|id| !known(id)) {
            self.focus = None;
        }
        self.specs = specs;
    }

    /// 所有舞台，按 Dock 树的先后。
    pub(crate) fn specs(&self) -> &[StageSpec] {
        &self.specs
    }

    /// 这块舞台在 Dock 里的面板。
    pub(crate) fn panel_of(&self, id: &str) -> Option<PanelId> {
        self.spec(id).map(|spec| spec.panel)
    }

    fn spec(&self, id: &str) -> Option<&StageSpec> {
        self.specs.iter().find(|spec| spec.id == id)
    }

    /// 有焦点的舞台。布局里一块舞台都没有时是空串。
    pub(crate) fn focused(&self) -> &str {
        self.focus
            .as_deref()
            .or_else(|| self.specs.first().map(|spec| spec.id.as_str()))
            .unwrap_or_default()
    }

    /// 把焦点给 `id` 这块，记一次用过。不是舞台时什么也不做，返回 false。
    pub(crate) fn focus(&mut self, id: &str) -> bool {
        if self.spec(id).is_none() {
            return false;
        }
        self.focus = Some(id.to_string());
        self.recent.retain(|other| other != id);
        self.recent.push(id.to_string());
        true
    }

    /// 这块舞台收不收这一类。不是舞台时是 false。
    pub(crate) fn accepts(&self, id: &str, kind: StageContent) -> bool {
        self.spec(id)
            .is_some_and(|spec| spec.accepts.contains(kind))
    }

    /// 没有焦点的这块舞台显示着什么。
    pub(crate) fn item(&self, id: &str) -> Option<&StageItem> {
        self.items.get(id)
    }

    /// 记下（`None` 是清掉）这块舞台显示着什么。不是舞台时什么也不做。
    pub(crate) fn set_item(&mut self, id: &str, item: Option<StageItem>) {
        if self.spec(id).is_none() {
            return;
        }
        match item {
            Some(item) => self.items.insert(id.to_string(), item),
            None => self.items.remove(id),
        };
    }

    /// 显示着这样东西的舞台。
    pub(crate) fn holding(&self, item: &StageItem) -> Option<&str> {
        self.specs
            .iter()
            .find(|spec| self.items.get(&spec.id) == Some(item))
            .map(|spec| spec.id.as_str())
    }

    /// 要放 `kind` 时用哪块，见 [`pick_stage`]。
    fn pick(&self, kind: StageContent) -> Option<String> {
        pick_stage(&self.specs, self.focused(), &self.recent, kind)
    }
}

/// 一块舞台此刻显示什么，画舞台时用。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StageShows {
    /// 项目会话，`sessions` 的下标。
    Session(usize),
    Page(WorkspaceRoute),
    Empty,
}

/// 要放 `kind` 时用哪块舞台：有焦点的那块收就用它；否则收它的里面最近有过焦点的；
/// 都没有过焦点就按布局的先后取第一块。
pub(crate) fn pick_stage(
    stages: &[StageSpec],
    focused: &str,
    recent: &[String],
    kind: StageContent,
) -> Option<String> {
    let accepts = |id: &str| {
        stages
            .iter()
            .any(|stage| stage.id == id && stage.accepts.contains(kind))
    };
    if accepts(focused) {
        return Some(focused.to_string());
    }
    recent
        .iter()
        .rev()
        .find(|id| accepts(id))
        .cloned()
        .or_else(|| {
            stages
                .iter()
                .find(|stage| stage.accepts.contains(kind))
                .map(|stage| stage.id.clone())
        })
}

/// 会话能不能单独上舞台、归哪一类。智能体的对话在智能体那一页里，产品对话不单独显示，
/// 都不上。
pub(crate) fn session_stage_content(session: &Session, cx: &App) -> Option<StageContent> {
    if session.is_product_conversation(cx) || session.is_agent_conversation(cx) {
        return None;
    }
    Some(match session.kind {
        SessionKind::Term { .. } => StageContent::Terminal,
        SessionKind::Conversation(_) => StageContent::Conversation,
    })
}

fn session_item(session: &Session) -> StageItem {
    StageItem::Session(session.conversation_id.clone())
}

/// 页面的记法。会话那一页不是页面，看会话本身。
fn page_item(route: &WorkspaceRoute) -> Option<StageItem> {
    (!route.is_session()).then(|| StageItem::Page(crate::settings::enum_name(route)))
}

/// 记下的页面名认出来的页面。认不出（别的版本写的）、或者写的是会话那一页，是 None。
fn item_route(name: &str) -> Option<WorkspaceRoute> {
    crate::settings::enum_from_name::<WorkspaceRoute>(name).filter(|route| !route.is_session())
}

impl Workspace {
    /// 有焦点的舞台。
    pub(crate) fn focused_stage(&self) -> String {
        self.stages.focused().to_string()
    }

    fn session_by_conversation(&self, id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.conversation_id == id)
    }

    /// 记下的这样东西还在不在、归哪一类。会话关了、页面名认不出就是 None。
    fn item_content(&self, item: &StageItem, cx: &App) -> Option<StageContent> {
        match item {
            StageItem::Session(id) => {
                session_stage_content(&self.sessions[self.session_by_conversation(id)?], cx)
            }
            StageItem::Page(name) => item_route(name).map(|_| StageContent::Page),
        }
    }

    /// 有焦点的舞台现在显示的东西，焦点换走时它留在原处。没有就是 `None`。
    fn current_on_stage(&self, cx: &App) -> Option<StageItem> {
        match self.active_tab() {
            WorkspaceRoute::Session => self
                .cur()
                .filter(|session| session_stage_content(session, cx).is_some())
                .map(session_item),
            route => page_item(route),
        }
    }

    /// 把焦点换到 `to`：有焦点的那块现在显示的留在原处，`to` 原来显示的清掉（调用方马上
    /// 把新的当前放上去）。`to` 不是舞台、已经有焦点时什么也不做。
    fn move_stage_focus(&mut self, to: &str, cx: &App) {
        let from = self.focused_stage();
        if from == to {
            return;
        }
        let current = self.current_on_stage(cx);
        if !self.stages.focus(to) {
            return;
        }
        self.stages.set_item(to, None);
        self.stages.set_item(&from, current);
    }

    /// `item` 要成为当前之前：它显示在别的舞台上就把焦点换过去；有焦点的舞台不收这一类，
    /// 就换到收它的那块。
    fn route_item_to_stage(&mut self, item: &StageItem, kind: StageContent, cx: &App) {
        let target = self
            .stages
            .holding(item)
            .map(str::to_string)
            .or_else(|| self.stages.pick(kind));
        if let Some(target) = target {
            self.move_stage_focus(&target, cx);
        }
    }

    /// 切到第 `ix` 个会话之前调，见 [`Self::route_item_to_stage`]。不单独上舞台的会话不动。
    pub(crate) fn route_session_to_stage(&mut self, ix: usize, cx: &App) {
        let Some(session) = self.sessions.get(ix) else {
            return;
        };
        let Some(kind) = session_stage_content(session, cx) else {
            return;
        };
        let item = session_item(session);
        self.route_item_to_stage(&item, kind, cx);
    }

    /// 把第 `ix` 个会话设成当前：先分到该放它的舞台，再记成当前、记一次看过；能单独上舞台的
    /// 换到会话页，让它显示出来（智能体的对话跟着智能体那一页，页面不动）。选会话、新建会话
    /// 都走这里。对齐界面（`sync_session_ui`）由调用方做。
    pub(crate) fn set_current_session(&mut self, ix: usize, cx: &App) {
        let Some(session) = self.sessions.get(ix) else {
            return;
        };
        let stands_alone = session_stage_content(session, cx).is_some();
        self.route_session_to_stage(ix, cx);
        self.point_current_at(ix);
        self.note_session_viewed(ix);
        if stands_alone {
            // 远程桌面离开舞台就放掉它的连接；显示在别的舞台上还看得见，不放。
            if !self.page_parked(&WorkspaceRoute::Desktop) {
                self.desktop = None;
            }
            self.nav.set_active(WorkspaceRoute::Session);
        }
    }

    /// 换到 `tab` 之前调。`Session` 那一页放的是当前会话。
    pub(crate) fn route_to_stage(&mut self, tab: &WorkspaceRoute, cx: &App) {
        match page_item(tab) {
            Some(item) => self.route_item_to_stage(&item, StageContent::Page, cx),
            None => {
                if let Some(ix) = self.current_ix() {
                    self.route_session_to_stage(ix, cx);
                }
            }
        }
    }

    /// 显示这个页面的舞台：它是当前页就是有焦点的那块，显示在别处就是那块。都不是时按有焦点
    /// 的算（页面马上要换到那儿）。
    pub(crate) fn page_stage(&self, route: &WorkspaceRoute) -> &str {
        if self.active_tab() == route {
            return self.stages.focused();
        }
        page_item(route)
            .and_then(|item| self.stages.holding(&item))
            .unwrap_or_else(|| self.stages.focused())
    }

    /// 这个页面显示在没有焦点的舞台上（看得见）。
    pub(crate) fn page_parked(&self, route: &WorkspaceRoute) -> bool {
        page_item(route).is_some_and(|item| self.stages.holding(&item).is_some())
    }

    /// 这个会话放得进有焦点的舞台：能单独上舞台、这块收这一类、别的舞台上也没显示着它。
    pub(crate) fn fits_focused_stage(&self, session: &Session, cx: &App) -> bool {
        session_stage_content(session, cx)
            .is_some_and(|kind| self.stages.accepts(self.stages.focused(), kind))
            && self.stages.holding(&session_item(session)).is_none()
    }

    /// 关掉了有焦点的舞台上的会话、它又没有能接着放的：把焦点交给最近用过的、显示着东西的
    /// 那块，它显示的变成当前，原来那块空着。没有这样的舞台就是 false，什么都没改。
    /// 不动键盘焦点：`remove_session` 里没有窗口，下一帧照常对齐。
    pub(crate) fn take_over_parked_stage(&mut self, cx: &App) -> bool {
        let stages = &self.stages;
        let layout_order = stages.specs().iter().map(|stage| &stage.id);
        let found = stages
            .recent
            .iter()
            .rev()
            .chain(layout_order)
            .find_map(|id| {
                let item = stages.item(id)?;
                self.item_content(item, cx)?;
                Some((id.clone(), item.clone()))
            });
        let Some((id, item)) = found else {
            return false;
        };
        self.stages.focus(&id);
        self.stages.set_item(&id, None);
        match item {
            StageItem::Session(conversation) => {
                self.current_conversation = Some(conversation);
                self.nav.set_active(WorkspaceRoute::Session);
            }
            StageItem::Page(name) => {
                if let Some(route) = item_route(&name) {
                    self.nav.set_active(route);
                }
            }
        }
        true
    }

    /// 这块舞台此刻看得见（是所在组显示着的那块、没被别的组的放大挡住、不在收起的 dock 里）。
    /// 读的是上次对齐（`sync_layout_visibility`）时记下的，只读内存。
    pub(crate) fn stage_shown(&self, id: &str) -> bool {
        self.stages
            .panel_of(id)
            .is_some_and(|panel| self.shell_shown.contains(&panel))
    }

    /// 有焦点的舞台此刻看得见。
    pub(crate) fn focused_stage_shown(&self) -> bool {
        self.stage_shown(self.stages.focused())
    }

    /// 让有焦点的舞台露出来。
    pub(crate) fn reveal_focused_stage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.stages.panel_of(self.stages.focused()) {
            self.reveal_panel(panel, window, cx);
        }
    }

    /// 在一块没有焦点的舞台里按下（或者插件 `layout.focus`）：它显示的变成当前，焦点跟着
    /// 换过去。空舞台不抢焦点。
    pub(crate) fn focus_stage(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.focused_stage() == id {
            return;
        }
        match self.stages.item(id).cloned() {
            Some(StageItem::Session(conversation)) => {
                if let Some(ix) = self.session_by_conversation(&conversation) {
                    self.activate(ix, window, cx);
                }
            }
            Some(StageItem::Page(name)) => {
                if let Some(route) = item_route(&name) {
                    self.reveal_workspace_tab(route, cx);
                    // 键盘焦点别留在刚才那块舞台的终端里。
                    window.focus(&self.focus_handle, cx);
                    self.save_state(cx);
                    cx.notify();
                }
            }
            None => {}
        }
    }

    /// 这块舞台此刻显示什么。只读内存，画舞台时调。
    pub(crate) fn stage_shows(&self, id: &str, cx: &App) -> StageShows {
        let (shows, kind) = if id == self.stages.focused() {
            match self.active_tab() {
                WorkspaceRoute::Session => {
                    let Some(ix) = self.current_ix() else {
                        return StageShows::Empty;
                    };
                    match session_stage_content(&self.sessions[ix], cx) {
                        Some(kind) => (StageShows::Session(ix), kind),
                        None => return StageShows::Empty,
                    }
                }
                route => (StageShows::Page(route.clone()), StageContent::Page),
            }
        } else {
            match self.stages.item(id) {
                Some(StageItem::Session(conversation)) => {
                    let Some(ix) = self.session_by_conversation(conversation) else {
                        return StageShows::Empty;
                    };
                    let Some(kind) = session_stage_content(&self.sessions[ix], cx) else {
                        return StageShows::Empty;
                    };
                    (StageShows::Session(ix), kind)
                }
                Some(StageItem::Page(name)) => match item_route(name) {
                    Some(route) => (StageShows::Page(route), StageContent::Page),
                    None => return StageShows::Empty,
                },
                None => return StageShows::Empty,
            }
        };
        // `settle_stages` 保证了不会放错；这里再挡一道，宁可空着也不把东西画进不收它的舞台。
        if self.stages.accepts(id, kind) {
            shows
        } else {
            StageShows::Empty
        }
    }

    /// 没有焦点、此刻看得见的舞台上显示着的会话：（会话下标，舞台 id）。插件的 `stage` 用。
    pub(crate) fn parked_sessions(&self) -> Vec<(usize, String)> {
        self.stages
            .specs()
            .iter()
            .filter(|stage| self.stage_shown(&stage.id))
            .filter_map(|stage| match self.stages.item(&stage.id)? {
                StageItem::Session(conversation) => Some((
                    self.session_by_conversation(conversation)?,
                    stage.id.clone(),
                )),
                StageItem::Page(_) => None,
            })
            .collect()
    }

    /// 给插件的 `layout().stages`。只读内存。
    pub(crate) fn stage_infos(&self, cx: &App) -> Vec<LayoutStage> {
        self.stages
            .specs()
            .iter()
            .map(|stage| LayoutStage {
                id: stage.id.clone(),
                accepts: StageContent::ALL
                    .into_iter()
                    .filter(|kind| stage.accepts.contains(*kind))
                    .map(StageContent::name)
                    .collect(),
                focused: stage.id == self.stages.focused(),
                page: match self.stage_shows(&stage.id, cx) {
                    StageShows::Page(route) => Some(crate::settings::enum_name(&route)),
                    StageShows::Session(_) | StageShows::Empty => None,
                },
            })
            .collect()
    }

    /// 会话、布局变了以后理一遍，让每块舞台显示的都是它收的、互不重复：
    /// - 当前会话（或页面）显示在了别的舞台上，焦点就换到那块；有焦点的舞台不收当前的，
    ///   焦点换到收它的那块；
    /// - 别的舞台上记着的东西没了、放错了舞台、和别处重复的，清掉。会话还没恢复完时，
    ///   找不到的会话先留着，它可能还没接上；
    /// - 空着的舞台放进它收的、最近看过、别处没显示的会话。
    pub(crate) fn settle_stages(&mut self, cx: &App) {
        let stages = self.stages.specs().to_vec();
        let restored = self.sessions_restore_settled();
        let current = self.current_on_stage(cx);
        let mut focused = self.stages.focused().to_string();
        if let Some(current) = &current {
            if let Some(home) = self.stages.holding(current) {
                focused = home.to_string();
            }
            if let Some(kind) = self.item_content(current, cx)
                && !self.stages.accepts(&focused, kind)
                && let Some(home) = pick_stage(&stages, &focused, &self.stages.recent, kind)
            {
                focused = home;
            }
        }

        // 先定每块舞台显示什么，再一起记下。
        let mut taken: Vec<StageItem> = current.into_iter().collect();
        let mut shows: Vec<(String, Option<StageItem>)> = Vec::new();
        for stage in &stages {
            if stage.id == focused {
                shows.push((stage.id.clone(), None));
                continue;
            }
            let kept = self.stages.item(&stage.id).filter(|item| {
                let fits = match self.item_content(item, cx) {
                    // 远程桌面看哪台机器不落盘，重启后没有可显示的。
                    Some(kind) => {
                        stage.accepts.contains(kind)
                            && (self.desktop.is_some()
                                || page_item(&WorkspaceRoute::Desktop).as_ref() != Some(*item))
                    }
                    None => !restored && matches!(item, StageItem::Session(_)),
                };
                fits && !taken.contains(item)
            });
            if let Some(item) = kept {
                taken.push(item.clone());
            }
            shows.push((stage.id.clone(), kept.cloned()));
        }
        for (stage, (_, shown)) in stages.iter().zip(&mut shows) {
            if stage.id == focused || shown.is_some() {
                continue;
            }
            let fill = self
                .recent_sessions
                .iter()
                .rev()
                .filter_map(|ui_id| self.sessions.iter().find(|s| s.ui_id == *ui_id))
                .chain(self.sessions.iter())
                .find(|session| {
                    session_stage_content(session, cx)
                        .is_some_and(|kind| stage.accepts.contains(kind))
                        && !taken.contains(&session_item(session))
                });
            if let Some(session) = fill {
                let item = session_item(session);
                taken.push(item.clone());
                *shown = Some(item);
            }
        }

        // 焦点没换就不记一次用过，免得只是理一遍也改了先后。
        if focused != self.stages.focused() {
            self.stages.focus(&focused);
        }
        for (id, shown) in shows {
            self.stages.set_item(&id, shown);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TERMINAL: Accepts = Accepts {
        terminal: true,
        conversation: false,
        page: false,
    };
    const CHAT: Accepts = Accepts {
        terminal: false,
        conversation: true,
        page: true,
    };

    fn spec(id: &str, accepts: Accepts) -> StageSpec {
        StageSpec {
            id: id.into(),
            accepts,
            panel: PanelId::from_u64(0),
        }
    }

    fn stages() -> Vec<StageSpec> {
        vec![spec("term", TERMINAL), spec("chat", CHAT)]
    }

    #[test]
    fn the_focused_stage_wins_when_it_takes_the_kind() {
        let all = vec![spec("a", Accepts::ALL), spec("b", Accepts::ALL)];
        assert_eq!(
            pick_stage(&all, "b", &[], StageContent::Terminal).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn a_kind_the_focused_stage_refuses_goes_to_the_stage_that_takes_it() {
        assert_eq!(
            pick_stage(&stages(), "term", &[], StageContent::Conversation).as_deref(),
            Some("chat")
        );
        assert_eq!(
            pick_stage(&stages(), "chat", &[], StageContent::Terminal).as_deref(),
            Some("term")
        );
        assert_eq!(
            pick_stage(&stages(), "term", &[], StageContent::Page).as_deref(),
            Some("chat")
        );
    }

    #[test]
    fn among_several_takers_the_most_recently_focused_wins() {
        let three = vec![
            spec("term", TERMINAL),
            spec("left", CHAT),
            spec("right", CHAT),
        ];
        assert_eq!(
            pick_stage(&three, "term", &[], StageContent::Conversation).as_deref(),
            Some("left"),
            "都没有过焦点时按布局先后"
        );
        let recent = vec!["right".to_string(), "left".to_string(), "term".to_string()];
        assert_eq!(
            pick_stage(&three, "term", &recent, StageContent::Conversation).as_deref(),
            Some("left")
        );
        let recent = vec!["left".to_string(), "right".to_string(), "term".to_string()];
        assert_eq!(
            pick_stage(&three, "term", &recent, StageContent::Conversation).as_deref(),
            Some("right")
        );
    }

    #[test]
    fn pages_are_recorded_by_name_and_sessions_are_not_pages() {
        assert_eq!(
            page_item(&WorkspaceRoute::Agents),
            Some(StageItem::Page("agents".into()))
        );
        assert_eq!(page_item(&WorkspaceRoute::Session), None);
        assert_eq!(item_route("automations"), Some(WorkspaceRoute::Automations));
        assert_eq!(item_route("session"), None);
        assert_eq!(item_route("gone"), None);
    }

    #[test]
    fn accepts_come_from_the_panel_info() {
        assert_eq!(Accepts::from_info(&Value::Null), Accepts::ALL);
        assert_eq!(Accepts::from_info(&json!({})), Accepts::ALL);
        assert_eq!(
            Accepts::from_info(&json!({ "accepts": null })),
            Accepts::ALL
        );
        assert_eq!(
            Accepts::from_info(&json!({ "accepts": ["terminal", "hologram"] })),
            TERMINAL,
            "不认识的名字忽略"
        );
        assert_eq!(
            Accepts::from_info(&json!({ "accepts": ["conversation", "page"] })),
            CHAT
        );
        assert_eq!(Accepts::from_info(&json!({ "accepts": [] })), Accepts::NONE);
    }

    #[test]
    fn stages_forget_what_the_new_tree_no_longer_has() {
        let mut stages = Stages::default();
        stages.set_specs(vec![spec("a", Accepts::ALL), spec("b", Accepts::ALL)]);
        assert_eq!(stages.focused(), "a", "没点过是第一块");
        assert!(stages.focus("b"));
        assert!(!stages.focus("gone"));
        stages.set_item("a", Some(StageItem::Page("agents".into())));
        stages.set_item("gone", Some(StageItem::Page("agents".into())));
        assert_eq!(stages.holding(&StageItem::Page("agents".into())), Some("a"));

        stages.set_specs(vec![spec("c", Accepts::ALL), spec("a", Accepts::ALL)]);
        assert_eq!(stages.focused(), "c", "有焦点的那块没了，回到第一块");
        assert_eq!(stages.item("a"), Some(&StageItem::Page("agents".into())));
        assert!(stages.recent.is_empty());
    }
}
