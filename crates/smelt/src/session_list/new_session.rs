//! 项目行「+」的新建选择器。
//!
//! 选择器只有一个入口，打开后直接列出具体的启动动作：常用、终端、对话。
//! 「终端 / 对话」是动作的属性，不再额外占一层模式切换。Pin 绑定到动作本身，
//! 所以同一个 Agent 的对话和终端可以分别置顶。
//!
//! 「有哪些动作、叫什么、怎么分组、Pin 存哪」全部在 `smelt_model::new_session`，
//! 移动端读的是同一份；这里只负责画出来，以及把动作接到桌面的会话工作区上。

use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, App, Context, ElementId, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, RenderOnce,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, px,
};
use gpui_base::actions::{Cancel, Confirm, SelectDown, SelectUp};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::popover::{Popover, PopoverState};
use gpui_component::{Icon, IconName, IndexPath, Sizable as _, StyledExt as _};
use smelt_ui::IconAction as _;

use crate::settings::{
    AcpRuntimeState, AgentDefinition, LaunchEntry, active_launch_entries, icon_for_launch_command,
};
use crate::workspace_sessions::NewAcpSessionRequest;
use crate::{RuntimeKey, Workspace, ui_theme};
use smelt_model::acp_conn::AcpRuntimeDiagnostics;
use smelt_model::agent_kind::{ConversationAgentKind, ConversationLaunchSpec, TerminalAgentKind};
use smelt_model::new_session::{
    NewSessionAction, NewSessionPreferences, NewSessionTarget, PinParams, SECTION_LABELS,
    build_sections, new_session_actions,
};
use smelt_model::session_control::AcpAgentOption;

use super::row::{acp_provider_icon, provider_icon};

// 新建选择器是快速菜单，不应继承内容卡片的尺寸。所有高度集中在这里，
// 这样压缩菜单时不会只改外壳而让行内容互相挤压。
const PICKER_WIDTH: f32 = 288.;
const PICKER_ROW_HEIGHT: f32 = 32.;
const PICKER_SECTION_HEIGHT: f32 = 21.;
const PICKER_EMPTY_HEIGHT: f32 = 56.;
const PICKER_FOOTER_HEIGHT: f32 = 24.;
const PICKER_MAX_VISIBLE_ROWS: usize = 7;

/// Pin 偏好的客户端缓存。偏好存在运行时（桌面和手机共用），第一次用到时异步取回；
/// 取回之前按没有 Pin 显示。
#[derive(Clone, Debug, Default)]
struct NewSessionPins {
    preferences: NewSessionPreferences,
    requested: bool,
}

impl gpui::Global for NewSessionPins {}

fn current_new_session_preferences(cx: &mut App) -> NewSessionPreferences {
    let pins = cx.default_global::<NewSessionPins>();
    if !pins.requested {
        pins.requested = true;
        cx.spawn(async move |cx| {
            let fetched = cx
                .background_executor()
                .spawn(async {
                    crate::runtime_call::call::<smelt_model::control_api::NewSessionPinsGet>(
                        smelt_runtime_client::protocol::control::EmptyParams::default(),
                        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                    )
                })
                .await;
            cx.update(|cx| match fetched {
                Ok(preferences) => {
                    cx.default_global::<NewSessionPins>().preferences = preferences;
                    cx.refresh_windows();
                }
                // 下次打开再试。
                Err(_) => cx.default_global::<NewSessionPins>().requested = false,
            });
        })
        .detach();
    }
    cx.global::<NewSessionPins>().preferences.clone()
}

/// 远程运行时那台机器上的启动项和 CLI 探测结果。启动项按运行时各存一份，装没装
/// 也只有那台机器说了算，所以不能拿本机的去凑。每次打开菜单从那台机器重新取一次，
/// 取回之前沿用上一次的。
#[derive(Clone, Debug, Default)]
struct RemoteLaunchCatalog {
    entries: Vec<LaunchEntry>,
    diagnostics: AcpRuntimeDiagnostics,
}

/// 一个远程运行时的目录：上次读到的内容，和这次读得怎么样。菜单据此说明眼下看到的
/// 是什么：还在读、读不到，和那台机器真的什么都没装，看起来都是只剩空白终端。
#[derive(Clone, Debug, Default)]
struct RemoteCatalogEntry {
    catalog: Option<RemoteLaunchCatalog>,
    reading: CatalogReading,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum CatalogReading {
    #[default]
    InFlight,
    Done,
    Failed,
}

#[derive(Default)]
struct RemoteLaunchCatalogs(std::collections::HashMap<RuntimeKey, RemoteCatalogEntry>);

impl gpui::Global for RemoteLaunchCatalogs {}

/// 菜单底部的说明：目录不是最新时说清楚现在显示的是什么，是最新的就不说。
fn remote_catalog_notice(runtime_name: &str, entry: Option<&RemoteCatalogEntry>) -> Option<String> {
    let entry = entry?;
    let have_catalog = entry.catalog.is_some();
    match (&entry.reading, have_catalog) {
        (CatalogReading::Done, _) => None,
        (CatalogReading::InFlight, false) => {
            Some(format!("正在读取「{runtime_name}」上装了哪些工具…"))
        }
        (CatalogReading::InFlight, true) => {
            Some(format!("正在刷新「{runtime_name}」，先显示上次读到的"))
        }
        (CatalogReading::Failed, false) => {
            Some(format!("连不上「{runtime_name}」，只能开空白终端"))
        }
        (CatalogReading::Failed, true) => {
            Some(format!("连不上「{runtime_name}」，显示的是上次读到的"))
        }
    }
}

fn refresh_remote_launch_catalog(runtime: crate::runtimes::Runtime, cx: &mut App) {
    if runtime.is_local() {
        return;
    }
    let key = runtime.key().clone();
    let client = runtime.client().clone();
    cx.default_global::<RemoteLaunchCatalogs>()
        .0
        .entry(key.clone())
        .or_default()
        .reading = CatalogReading::InFlight;
    cx.spawn(async move |cx| {
        let fetched = cx
            .background_executor()
            .spawn(async move {
                let empty = smelt_runtime_client::protocol::control::EmptyParams::default();
                let settings = client.call::<smelt_model::control_api::LaunchGet>(
                    &empty,
                    crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                )?;
                let diagnostics = client.call::<smelt_model::control_api::AgentCliInspect>(
                    &empty,
                    crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                )?;
                Ok::<_, smelt_runtime_client::protocol::control::ControlClientError>(
                    RemoteLaunchCatalog {
                        entries: settings.entries,
                        diagnostics,
                    },
                )
            })
            .await;
        // 取不到（离线、超时）就留着上一次的，标成读取失败，下次打开再取。
        cx.update(|cx| {
            let entry = cx
                .default_global::<RemoteLaunchCatalogs>()
                .0
                .entry(key)
                .or_default();
            match fetched {
                Ok(catalog) => {
                    entry.catalog = Some(catalog);
                    entry.reading = CatalogReading::Done;
                }
                Err(_) => entry.reading = CatalogReading::Failed,
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// 先在本地切换让菜单立刻有反应，再交给运行时；运行时回来的是切换后的权威值。
fn toggle_new_session_pin(key: &str, cx: &mut App) {
    cx.default_global::<NewSessionPins>()
        .preferences
        .toggle(key);
    let params = PinParams {
        key: key.to_string(),
    };
    cx.spawn(async move |cx| {
        let toggled = cx
            .background_executor()
            .spawn(async move {
                crate::runtime_call::call::<smelt_model::control_api::NewSessionPinsToggle>(
                    params,
                    crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                )
            })
            .await;
        cx.update(|cx| match toggled {
            Ok(preferences) => {
                cx.default_global::<NewSessionPins>().preferences = preferences;
                cx.refresh_windows();
            }
            Err(error) => crate::status_item::notify_error(format!(
                "保存常用项失败：{}",
                crate::runtime_call::error_text(error)
            )),
        });
    })
    .detach();
}

fn picker_item_icon(target: &NewSessionTarget) -> Icon {
    match target {
        NewSessionTarget::Conversation { agent_kind, .. } => {
            ConversationAgentKind::from_id(agent_kind)
                .map(acp_provider_icon)
                .unwrap_or_else(|| Icon::new(IconName::Bot))
        }
        NewSessionTarget::Terminal { command, provider } => provider
            .as_deref()
            .and_then(TerminalAgentKind::from_id)
            .or_else(|| TerminalAgentKind::from_command_prefix(command))
            .map(|agent| provider_icon(Some(agent)))
            .unwrap_or_else(|| Icon::new(icon_for_launch_command(command))),
        NewSessionTarget::BlankTerminal => Icon::new(IconName::SquareTerminal),
    }
}

/// 桌面这一侧的「有哪些对话入口」：产品智能体在前，裸引擎与 profile 在后。
/// 只产出身份与显示名，启动规格在真正创建时由 [`conversation_launch`] 解析，
/// 免得同一份配置被读成两种命令。
/// `local`：项目在本机运行时上。智能体定义和 profile 目前只读本机运行时的配置，远程
/// 运行时上的项目只给裸引擎，免得拿本机的定义去另一台机器上起。
fn conversation_options(local: bool, cx: &App) -> Vec<AcpAgentOption> {
    let config = cx.global::<crate::settings::AgentHostState>();
    let mut options = Vec::new();
    for definition in config.agents.iter().filter(|_| local) {
        if !definition.is_conversation_ready() {
            continue;
        }
        let Some(kind) = definition.engine_kind() else {
            continue;
        };
        options.push(AcpAgentOption {
            id: format!("agent:{}", definition.id),
            kind: kind.id().to_string(),
            label: definition.name.trim().to_string(),
            profile: false,
            agent_definition_id: Some(definition.id.clone()),
            launch: kind.default_launch(),
            history_dir: None,
        });
    }
    for kind in ConversationAgentKind::ALL
        .into_iter()
        .filter(|kind| kind.is_bare_kind())
    {
        options.push(AcpAgentOption {
            id: kind.id().to_string(),
            kind: kind.id().to_string(),
            label: kind.label().to_string(),
            profile: false,
            agent_definition_id: None,
            launch: kind.default_launch(),
            history_dir: None,
        });
    }
    for profile in config.all_profiles().filter(|_| local) {
        let Some(kind) = profile.kind() else {
            continue;
        };
        if config.profile_launch_spec(profile).is_err() {
            continue;
        }
        options.push(AcpAgentOption {
            id: format!("profile:{}", profile.id),
            kind: kind.id().to_string(),
            label: profile.label.clone(),
            profile: true,
            agent_definition_id: None,
            launch: kind.default_launch(),
            history_dir: None,
        });
    }
    options
}

/// 对话动作在桌面怎么起：从 option id 反查当前 GUI 配置。启动规格永远现查，
/// 不缓存进菜单项——用户在设置里改完命令/环境变量，下一次新建就该生效。
type ConversationLaunchChoice = (
    ConversationAgentKind,
    Option<ConversationLaunchSpec>,
    Option<String>,
    Option<AgentDefinition>,
);

fn conversation_launch(option_id: &str, cx: &App) -> Option<ConversationLaunchChoice> {
    let config = cx.global::<crate::settings::AgentHostState>();
    if let Some(definition_id) =
        smelt_model::session_control::agent_definition_id_from_option_id(option_id)
    {
        let definition = config
            .agents
            .iter()
            .find(|definition| definition.id == definition_id)?;
        let (agent, launch) = config.resolve_agent(definition).ok()?;
        return Some((agent, Some(launch), None, Some(definition.clone())));
    }
    if let Some(profile_id) = option_id.strip_prefix("profile:") {
        let profile = config
            .all_profiles()
            .find(|profile| profile.id == profile_id)?;
        let kind = profile.kind()?;
        let launch = config.profile_launch_spec(profile).ok()?;
        return Some((kind, Some(launch), Some(profile_id.to_string()), None));
    }
    // 裸引擎不带 override：启动规格由会话工作区按当前配置自己解析。
    ConversationAgentKind::from_id(option_id).map(|kind| (kind, None, None, None))
}

/// 探测只在 Popover 打开时执行，避免每次侧栏重绘都重建动态候选集。
/// 启动项和装没装都以项目所在的那台机器为准：本机读常驻的配置与探测结果，远程读
/// [`RemoteLaunchCatalogs`]。
fn available_new_session_actions(runtime: &RuntimeKey, cx: &App) -> Vec<NewSessionAction> {
    let local = *runtime == RuntimeKey::Local;
    let conversations = conversation_options(local, cx);
    if local {
        let diagnostics = cx
            .try_global::<AcpRuntimeState>()
            .and_then(|state| state.diagnostics.clone());
        return new_session_actions(
            &conversations,
            active_launch_entries(cx).as_slice(),
            diagnostics.as_ref(),
        );
    }
    let catalog = cx
        .try_global::<RemoteLaunchCatalogs>()
        .and_then(|catalogs| catalogs.0.get(runtime))
        .and_then(|entry| entry.catalog.as_ref());
    remote_new_session_actions(&conversations, catalog)
}

/// 远程还没取回过时按「什么都没装」处理，只剩空白终端。放行的话会把本机的清单
/// 当成那台机器的，点了起不来。
fn remote_new_session_actions(
    conversations: &[AcpAgentOption],
    catalog: Option<&RemoteLaunchCatalog>,
) -> Vec<NewSessionAction> {
    let empty = RemoteLaunchCatalog::default();
    let catalog = catalog.unwrap_or(&empty);
    new_session_actions(conversations, &catalog.entries, Some(&catalog.diagnostics))
}

/// 新建菜单的状态与绘制。
///
/// 这里故意不用 `gpui_component::List`：它内部的虚拟列表会在每次弹层重绘时
/// 额外测量并预绘制一份行。工作区终端持续刷新时，这条路径会让弹层行出现叠画。
/// 菜单项目数量很小，直接绘制固定高度的普通行更可靠，也更容易保证 Pin 和文本
/// 共用同一棵布局树。
struct NewSessionPickerState {
    workspace: Entity<Workspace>,
    runtime: crate::runtimes::Runtime,
    cwd: Option<String>,
    source_actions: Vec<NewSessionAction>,
    sections: [Vec<NewSessionAction>; 3],
    selected_index: Option<IndexPath>,
    popover: Option<WeakEntity<PopoverState>>,
    focus_handle: FocusHandle,
}

impl NewSessionPickerState {
    fn new(
        workspace: Entity<Workspace>,
        runtime: crate::runtimes::Runtime,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            workspace,
            runtime,
            cwd,
            source_actions: Vec::new(),
            sections: [Vec::new(), Vec::new(), Vec::new()],
            selected_index: None,
            popover: None,
            focus_handle: cx.focus_handle(),
        }
    }

    fn item_at(&self, index: IndexPath) -> Option<&NewSessionAction> {
        self.sections.get(index.section)?.get(index.row)
    }

    fn first_index(&self) -> Option<IndexPath> {
        self.sections
            .iter()
            .enumerate()
            .find(|(_, items)| !items.is_empty())
            .map(|(section, _)| IndexPath::default().section(section))
    }

    fn index_for_key(&self, key: &str) -> Option<IndexPath> {
        self.sections
            .iter()
            .enumerate()
            .find_map(|(section, items)| {
                items
                    .iter()
                    .position(|item| item.key == key)
                    .map(|row| IndexPath::default().section(section).row(row))
            })
    }

    fn indices(&self) -> impl Iterator<Item = IndexPath> + '_ {
        self.sections
            .iter()
            .enumerate()
            .flat_map(|(section, items)| {
                (0..items.len()).map(move |row| IndexPath::default().section(section).row(row))
            })
    }

    fn set_selected_index(&mut self, index: Option<IndexPath>, cx: &mut Context<Self>) {
        if self.selected_index != index {
            self.selected_index = index;
            cx.notify();
        }
    }

    fn select_next(&mut self, cx: &mut Context<Self>) {
        let indices: Vec<_> = self.indices().collect();
        if indices.is_empty() {
            self.set_selected_index(None, cx);
            return;
        }
        let next = self
            .selected_index
            .and_then(|selected| indices.iter().position(|index| *index == selected))
            .map(|position| (position + 1) % indices.len())
            .unwrap_or(0);
        self.set_selected_index(Some(indices[next]), cx);
    }

    fn select_previous(&mut self, cx: &mut Context<Self>) {
        let indices: Vec<_> = self.indices().collect();
        if indices.is_empty() {
            self.set_selected_index(None, cx);
            return;
        }
        let previous = self
            .selected_index
            .and_then(|selected| indices.iter().position(|index| *index == selected))
            .map(|position| position.checked_sub(1).unwrap_or(indices.len() - 1))
            .unwrap_or(0);
        self.set_selected_index(Some(indices[previous]), cx);
    }

    fn toggle_pin(&mut self, key: &str, cx: &mut Context<Self>) {
        let selected_key = self
            .selected_index
            .and_then(|index| self.item_at(index))
            .map(|item| item.key.clone());
        toggle_new_session_pin(key, cx);
        let preferences = current_new_session_preferences(cx);
        self.sections = build_sections(&self.source_actions, &preferences).into_array();
        self.selected_index = selected_key
            .as_deref()
            .and_then(|selected| self.index_for_key(selected))
            .or_else(|| self.first_index());
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self
            .selected_index
            .and_then(|index| self.item_at(index))
            .map(|action| action.target.clone())
        else {
            return;
        };
        let cwd = self.cwd.clone();
        let runtime = self.runtime.clone();
        let conversation = match &target {
            NewSessionTarget::Conversation {
                agent_option_id, ..
            } => {
                let Some(resolved) = conversation_launch(agent_option_id, cx) else {
                    // 配置在弹层开着的时候被改没了：静默关掉比建出一场引擎不明的
                    // 对话安全。
                    self.dismiss(window, cx);
                    return;
                };
                Some(resolved)
            }
            _ => None,
        };
        self.workspace.update(cx, |workspace, cx| match target {
            NewSessionTarget::Conversation { .. } => {
                let Some((agent, launch_override, profile_id, agent_definition)) = conversation
                else {
                    return;
                };
                workspace.add_acp_session(
                    NewAcpSessionRequest {
                        runtime,
                        agent,
                        launch_override,
                        profile_id,
                        agent_definition,
                        fallback_cwd: cwd,
                        pending_prompt: None,
                        automation_id: None,
                        activate: true,
                    },
                    window,
                    cx,
                )
            }
            NewSessionTarget::Terminal { command, provider } => workspace.add_session_with_launch(
                runtime,
                cwd,
                Some(LaunchEntry {
                    label: String::new(),
                    command,
                    provider,
                }),
                cx,
            ),
            NewSessionTarget::BlankTerminal => workspace.add_session(runtime, cwd, cx),
        });
        self.dismiss(window, cx);
    }

    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        if let Some(popover) = &self.popover {
            let _ = popover.update(cx, |popover, cx| popover.dismiss(window, cx));
        }
    }
}

fn sync_picker_state(
    state: &mut NewSessionPickerState,
    actions: &[NewSessionAction],
    popover: WeakEntity<PopoverState>,
    cx: &mut Context<NewSessionPickerState>,
) {
    let preferences = current_new_session_preferences(cx);
    let current_selection = state.selected_index;
    let selected_key = current_selection
        .and_then(|index| state.item_at(index))
        .map(|action| action.key.clone());
    let next_sections = build_sections(actions, &preferences).into_array();
    let source_changed = state.source_actions != actions;
    let sections_changed = state.sections != next_sections;
    let content_changed = source_changed || sections_changed;
    state.source_actions = actions.to_vec();
    state.sections = next_sections;
    state.popover = Some(popover);

    let next_selection = selected_key
        .as_deref()
        .and_then(|key| state.index_for_key(key))
        .or_else(|| state.first_index());
    if next_selection != current_selection {
        state.selected_index = next_selection;
        cx.notify();
    } else if content_changed {
        cx.notify();
    }
}

impl Focusable for NewSessionPickerState {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NewSessionPickerState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = cx.entity();
        let selected_index = self.selected_index;
        let mut content = div()
            .id("new-session-picker-state")
            .key_context("List")
            .track_focus(&self.focus_handle)
            .w_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.select_next(cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.select_previous(cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.dismiss(window, cx)));

        for (section, items) in self.sections.clone().into_iter().enumerate() {
            if items.is_empty() {
                continue;
            }
            let Some(label) = SECTION_LABELS.get(section) else {
                continue;
            };
            content = content.child(
                div()
                    .h(px(PICKER_SECTION_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_size(px(10.))
                    .font_semibold()
                    .text_color(gpui::rgb(ui_theme::text_faint()))
                    .child(*label),
            );

            for (row, item) in items.into_iter().enumerate() {
                let index = IndexPath::default().section(section).row(row);
                let item_key = item.key.clone();
                let pin_icon = if item.pinned {
                    IconName::StarFill
                } else {
                    IconName::Star
                };
                let pin_tip = if item.pinned {
                    "取消常用"
                } else {
                    "加入常用"
                };
                let pin_state = state.clone();
                let row_state = state.clone();
                let hover_state = state.clone();
                let selected = selected_index == Some(index);
                let pin_key = item_key.clone();
                let pin_button = Button::new("new-session-picker-pin")
                    .custom(ButtonCustomVariant::new(cx))
                    .ghost()
                    .xsmall()
                    .flex_shrink_0()
                    .icon(pin_icon)
                    .icon_action(pin_tip)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(move |_event, _, cx| {
                        cx.stop_propagation();
                        pin_state.update(cx, |state, cx| state.toggle_pin(&pin_key, cx));
                    });

                let row_label = format!("{} · {}", item.label, item.kind.label());
                content = content.child(
                    div()
                        .id((ElementId::from("new-session-picker-item"), item_key.clone()))
                        .role(gpui::Role::Button)
                        .aria_label(row_label.clone())
                        .h(px(PICKER_ROW_HEIGHT))
                        .mx_1()
                        .px_2()
                        .rounded(ui_theme::row_radius())
                        .min_w_0()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .when(selected, |this| this.bg(gpui::rgb(ui_theme::bg_hover())))
                        .hover(|this| this.bg(gpui::rgb(ui_theme::bg_hover())))
                        .on_hover(move |hovered, window, cx| {
                            if *hovered {
                                hover_state.update(cx, |state, cx| {
                                    state.set_selected_index(Some(index), cx);
                                });
                            }
                            let _ = window;
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            row_state.update(cx, |state, cx| {
                                state.set_selected_index(Some(index), cx);
                                state.confirm(window, cx);
                            });
                        })
                        .child(
                            picker_item_icon(&item.target)
                                .size(px(15.))
                                .flex_shrink_0()
                                .text_color(gpui::rgb(ui_theme::text_muted())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_x_hidden()
                                .truncate()
                                .text_size(px(12.))
                                .child(row_label),
                        )
                        .child(pin_button),
                );
            }
        }

        if self.indices().next().is_none() {
            content = content.child(
                div()
                    .h(px(PICKER_EMPTY_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(gpui::rgb(ui_theme::text_faint()))
                    .child("没有可用的启动项"),
            );
        }

        content
    }
}

#[derive(IntoElement)]
pub(super) struct NewSessionPicker {
    workspace: Entity<Workspace>,
    runtime: crate::runtimes::Runtime,
    cwd: Option<String>,
    trigger: Button,
}

/// 构造与会话数完全同槽的「+」触发器；命中区比旧版 xsmall 更大，但可见性仍只由
/// 项目行 hover 切换，避免每个项目常驻一个高对比操作。
/// 按钮 id 跟项目路径走。下标会在拖动排序后指到别的项目。
pub(super) fn new_session_picker(
    workspace: Entity<Workspace>,
    runtime: crate::runtimes::Runtime,
    cwd: String,
    cx: &App,
) -> NewSessionPicker {
    let trigger_id = ElementId::from((ElementId::from("proj-new"), cwd.clone()));
    NewSessionPicker {
        workspace,
        runtime,
        cwd: (!cwd.is_empty()).then_some(cwd),
        trigger: Button::new(trigger_id)
            .custom(ButtonCustomVariant::new(cx))
            .opacity(0.0)
            .group_hover(super::PROJ_HEADER_GROUP, |style| style.opacity(1.0))
            .small()
            .size(px(super::PROJECT_NEW_BUTTON_SIZE))
            .icon(IconName::Plus)
            .icon_action("新建会话"),
    }
}

impl RenderOnce for NewSessionPicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let workspace = self.workspace;
        let cwd = self.cwd;
        let runtime = self.runtime;
        let runtime_key = runtime.key().clone();
        let refresh_runtime = runtime.clone();
        // 项目排序会改变下标。状态以运行时加项目路径为主键，拖动后同一个列表位置不能
        // 把新会话建到旧项目，两个运行时上的同名路径也不能共用一份。
        let picker_root = format!(
            "{:?}:{}",
            runtime.key(),
            cwd.as_deref().unwrap_or("default")
        );
        let picker_key =
            ElementId::from((ElementId::from("new-session-picker"), picker_root.clone()));
        let picker_state_key =
            ElementId::from((ElementId::from("new-session-picker-state"), picker_root));
        let picker = window.use_keyed_state(picker_state_key, cx, {
            move |_, cx| NewSessionPickerState::new(workspace, runtime, cwd, cx)
        });
        let focus_handle = picker.read(cx).focus_handle(cx);

        Popover::new(picker_key)
            .anchor(Anchor::TopLeft)
            .w(px(PICKER_WIDTH))
            .p_0()
            .trigger(self.trigger)
            .track_focus(&focus_handle)
            .on_open_change(move |open, _, cx| {
                if *open {
                    refresh_remote_launch_catalog(refresh_runtime.clone(), cx);
                }
            })
            .content(move |_, _window, cx| {
                let actions = available_new_session_actions(&runtime_key, cx);
                let footer = remote_catalog_notice(
                    &crate::runtimes::runtime_name(&runtime_key, cx),
                    cx.try_global::<RemoteLaunchCatalogs>()
                        .and_then(|catalogs| catalogs.0.get(&runtime_key)),
                )
                .unwrap_or_else(|| "↑↓ 选择 · Enter 创建 · Esc 关闭".to_string());
                let popover = cx.entity();
                picker.update(cx, |state, cx| {
                    sync_picker_state(state, &actions, popover.downgrade(), cx)
                });

                let (item_count, section_count) = {
                    let state = picker.read(cx);
                    (
                        state
                            .sections
                            .iter()
                            .map(|items| items.len())
                            .sum::<usize>(),
                        state
                            .sections
                            .iter()
                            .filter(|items| !items.is_empty())
                            .count(),
                    )
                };
                // 行数较多时滚动，常用/终端/对话标题各占一小段固定高度。
                let list_height = if item_count == 0 {
                    PICKER_EMPTY_HEIGHT
                } else {
                    (item_count.min(PICKER_MAX_VISIBLE_ROWS) as f32 * PICKER_ROW_HEIGHT)
                        + (section_count as f32 * PICKER_SECTION_HEIGHT)
                };

                div()
                    .w_full()
                    // Popover 本身是无样式宿主；必须给内容实色背景，否则
                    // 选中行的半透明强调色会把后面的终端文字透进来，形成叠字。
                    .bg(ui_theme::glass_floating())
                    .border_1()
                    .border_color(ui_theme::card_stroke())
                    .rounded(px(8.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("new-session-picker-scroll")
                            .h(px(list_height))
                            .max_h(px(list_height))
                            .py_1()
                            .overflow_y_scroll()
                            .child(picker.clone()),
                    )
                    .child(
                        div()
                            .h(px(PICKER_FOOTER_HEIGHT))
                            .px_2()
                            .border_t_1()
                            .border_color(ui_theme::hairline())
                            .flex()
                            .items_center()
                            .text_size(px(9.))
                            .text_color(gpui::rgb(ui_theme::text_faint()))
                            .child(footer),
                    )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_model::acp_conn::RuntimeExecutable;
    use smelt_model::new_session::BLANK_TERMINAL_KEY;

    fn bare_engines() -> Vec<AcpAgentOption> {
        [ConversationAgentKind::Claude, ConversationAgentKind::Codex]
            .into_iter()
            .map(|kind| AcpAgentOption {
                id: kind.id().to_string(),
                kind: kind.id().to_string(),
                label: kind.label().to_string(),
                profile: false,
                agent_definition_id: None,
                launch: kind.default_launch(),
                history_dir: None,
            })
            .collect()
    }

    fn keys(actions: &[NewSessionAction]) -> Vec<&str> {
        actions.iter().map(|action| action.key.as_str()).collect()
    }

    #[test]
    fn the_footer_says_why_a_remote_menu_is_short() {
        let reading = |reading, catalog: bool| RemoteCatalogEntry {
            catalog: catalog.then(RemoteLaunchCatalog::default),
            reading,
        };
        assert_eq!(remote_catalog_notice("Linux", None), None, "本机没有这一说");
        assert_eq!(
            remote_catalog_notice("Linux", Some(&reading(CatalogReading::Done, true))),
            None
        );
        assert_eq!(
            remote_catalog_notice("Linux", Some(&reading(CatalogReading::InFlight, false))),
            Some("正在读取「Linux」上装了哪些工具…".to_string())
        );
        assert_eq!(
            remote_catalog_notice("Linux", Some(&reading(CatalogReading::Failed, false))),
            Some("连不上「Linux」，只能开空白终端".to_string())
        );
        assert_eq!(
            remote_catalog_notice("Linux", Some(&reading(CatalogReading::Failed, true))),
            Some("连不上「Linux」，显示的是上次读到的".to_string())
        );
    }

    #[test]
    fn a_remote_not_yet_inspected_offers_only_a_blank_terminal() {
        let actions = remote_new_session_actions(&bare_engines(), None);
        assert_eq!(keys(&actions), vec![BLANK_TERMINAL_KEY]);
    }

    #[test]
    fn a_remote_offers_its_own_entries_and_only_what_it_has_installed() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(
            TerminalAgentKind::Claude,
            RuntimeExecutable {
                program: "claude".into(),
                path: Some("/root/.local/bin/claude".into()),
                ..Default::default()
            },
        );
        diagnostics.record_terminal(TerminalAgentKind::Codex, RuntimeExecutable::default());
        let catalog = RemoteLaunchCatalog {
            entries: vec![
                LaunchEntry {
                    label: "Claude Code".into(),
                    command: "claude".into(),
                    provider: Some("claude".into()),
                },
                LaunchEntry {
                    label: "Codex".into(),
                    command: "codex".into(),
                    provider: Some("codex".into()),
                },
                LaunchEntry {
                    label: "部署".into(),
                    command: "./deploy.sh".into(),
                    provider: None,
                },
            ],
            diagnostics,
        };
        let actions = remote_new_session_actions(&bare_engines(), Some(&catalog));
        assert_eq!(
            keys(&actions),
            vec![
                "terminal:claude:claude",
                "terminal::./deploy.sh",
                BLANK_TERMINAL_KEY,
                "conversation:claude",
            ]
        );
    }
}
