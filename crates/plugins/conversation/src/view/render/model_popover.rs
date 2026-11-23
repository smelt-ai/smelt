//! 输入栏模型与会话配置弹层：模型搜索、Provider 胶囊切换、星舰推理强度舞台滑轨与底部模式分段。

use std::cmp::Reverse;

use gpui::{
    Anchor, App, Context, Entity, EntityInputHandler as _, FocusHandle, IntoElement, KeyBinding,
    KeyDownEvent, Keystroke, Render, RenderOnce, SharedString, Subscription, WeakEntity, Window,
    div, px, rgb,
};
use rust_i18n::t;
use smelt_plugin_api::model::acp_conn::{ModelProviderGroup, ModelState, SessionConfigState};
use smelt_plugin_api::model::agent_kind::ConversationAgentKind;
use smelt_plugin_api::ui::effort_slider::{
    EffortSlider, EffortTier, effort_dot_color, effort_progress,
};
use smelt_plugin_api::ui::field::{InputEvent, InputState, TextInput};
use smelt_plugin_api::ui::keybind::Keybind;
use smelt_plugin_api::ui::kit::{GLYPHS, RADIUS, TYPE};
use smelt_plugin_api::ui::popover::{Popover, PopoverState};
use smelt_plugin_api::ui::{Button, Icon, Segment, Segmented, Switch, ui_theme};

use super::*;

const MODEL_POPOVER_WIDTH: f32 = 344.;
const MODEL_LIST_MAX_HEIGHT: f32 = 176.;
const MODEL_POPOVER_KEY_CONTEXT: &str = "SmeltModelPopover";

gpui::actions!(smelt_model_popover, [ModelPrev, ModelNext, ModelChoose]);

/// 注册模型弹层的键盘导航绑定（上下移动、回车选定）。
pub(crate) fn bind_model_popover_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", ModelPrev, Some(MODEL_POPOVER_KEY_CONTEXT)),
        KeyBinding::new("ctrl-p", ModelPrev, Some(MODEL_POPOVER_KEY_CONTEXT)),
        KeyBinding::new("down", ModelNext, Some(MODEL_POPOVER_KEY_CONTEXT)),
        KeyBinding::new("ctrl-n", ModelNext, Some(MODEL_POPOVER_KEY_CONTEXT)),
        KeyBinding::new("enter", ModelChoose, Some(MODEL_POPOVER_KEY_CONTEXT)),
    ]);
}

/// 识别哪项会话配置是「推理强度」（渲染成互动星舰滑轨）。
/// 优先认 ACP 协议声明的 `category: thought_level` / 运行时 `SmeltConfig::ThinkingLevel`，
/// 旧版守护未带该标记时按智能体声明的 `thinking_key` 兜底。
pub(crate) fn is_thinking_config(
    config: &SessionConfigState,
    agent: ConversationAgentKind,
) -> bool {
    if config.is_switch() || config.options.len() < 2 {
        return false;
    }
    config.is_thought_level() || agent.task_params().thinking_key == Some(config.config_id.as_str())
}

/// 当前配置里选中的选项下标；对不上时回退 0。
fn selected_option_index(config: &SessionConfigState) -> usize {
    config
        .options
        .iter()
        .position(|(_, name)| name == &config.current_name)
        .unwrap_or(0)
}

/// 不分大小写的子序列打分：连着、落在词头的字分高；不匹配返回 `None`。
pub(crate) fn subsequence_score(haystack: &str, query: &str) -> Option<i32> {
    let query: Vec<char> = query
        .trim()
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = haystack.chars().collect();
    let folded: Vec<char> = chars
        .iter()
        .copied()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let mut best = None;
    for start in (0..folded.len()).filter(|&i| folded[i] == query[0]) {
        let mut total = 0;
        let mut prev: Option<usize> = None;
        let mut matched = 0;
        for i in start..folded.len() {
            if matched == query.len() {
                break;
            }
            if folded[i] != query[matched] {
                continue;
            }
            let mut points = 10;
            if prev.is_some_and(|p| p + 1 == i) {
                points += 12;
            }
            let is_word_start = i.checked_sub(1).map(|p| chars[p]).is_none_or(|p| {
                p.is_whitespace()
                    || matches!(p, '-' | '_' | '/' | '.' | ':' | '(' | ')')
                    || (p.is_lowercase() && chars[i].is_uppercase())
            });
            if is_word_start {
                points += 8;
            }
            let skipped = prev.map_or(i, |p| i - p - 1);
            total += points - skipped as i32;
            prev = Some(i);
            matched += 1;
        }
        if matched == query.len() {
            best = best.max(Some(total));
        }
    }
    best
}

/// 列表里可见的一条模型候选。
#[derive(Clone, Debug, PartialEq, Eq)]
struct VisibleModel {
    value: String,
    label: String,
    provider_tag: Option<String>,
}

/// 计算当前展示在列表里的模型条目：有搜索词时跨 Provider 过滤排序，无搜索词时展示当前 Provider。
fn build_visible_models(
    model: Option<&ModelState>,
    active_provider_id: Option<&str>,
    query: &str,
) -> Vec<VisibleModel> {
    let Some(model) = model else {
        return Vec::new();
    };
    let trimmed = query.trim();
    if model.provider_groups.len() > 1 {
        if trimmed.is_empty() {
            let group = active_provider_id
                .and_then(|id| model.provider_groups.iter().find(|g| g.id == id))
                .or_else(|| selected_provider_group(model))
                .or_else(|| model.provider_groups.first());
            let Some(group) = group else {
                return Vec::new();
            };
            return group
                .options
                .iter()
                .map(|(value, name)| VisibleModel {
                    value: value.clone(),
                    label: name.clone(),
                    provider_tag: None,
                })
                .collect();
        }
        let mut scored: Vec<(i32, usize, VisibleModel)> = Vec::new();
        let mut order = 0usize;
        for group in &model.provider_groups {
            for (value, name) in &group.options {
                let name_score = subsequence_score(name, trimmed);
                let combo_score =
                    subsequence_score(&format!("{} {name}", group.name), trimmed).map(|s| s / 2);
                if let Some(points) = name_score.max(combo_score) {
                    scored.push((
                        points,
                        order,
                        VisibleModel {
                            value: value.clone(),
                            label: name.clone(),
                            provider_tag: Some(group.name.clone()),
                        },
                    ));
                }
                order += 1;
            }
        }
        scored.sort_by_key(|(points, ix, _)| (Reverse(*points), *ix));
        return scored.into_iter().map(|(_, _, item)| item).collect();
    }

    let options: &[(String, String)] = if let Some(group) = model.provider_groups.first() {
        group.options.as_slice()
    } else {
        model.options.as_slice()
    };
    let grouped = !model.provider_groups.is_empty();
    let mut scored: Vec<(i32, usize, VisibleModel)> = Vec::new();
    for (ix, (value, name)) in options.iter().enumerate() {
        let label = if grouped {
            name.clone()
        } else {
            model_label_with_provider(value, name)
        };
        if let Some(points) =
            subsequence_score(&label, trimmed).max(subsequence_score(value, trimmed).map(|s| s / 2))
        {
            scored.push((
                points,
                ix,
                VisibleModel {
                    value: value.clone(),
                    label,
                    provider_tag: None,
                },
            ));
        }
    }
    if !trimmed.is_empty() {
        scored.sort_by_key(|(points, ix, _)| (Reverse(*points), *ix));
    }
    scored.into_iter().map(|(_, _, item)| item).collect()
}

/// 短选项是否适合和标题摆在同一行（例如「访问权限 · 需审批 | 完全访问」）。
fn config_fits_inline(config: &SessionConfigState) -> bool {
    if config.options.len() != 2 {
        return false;
    }
    let total_chars: usize = config
        .options
        .iter()
        .map(|(v, n)| config.option_label(v, n).chars().count())
        .sum();
    total_chars <= 14
}

/// 等宽 `Segmented`（每段左右 12px）能否不截断所有标签；4 项且含 `Accept edits` 这类较长词时改按内容配比。
fn config_fits_equal_segments(config: &SessionConfigState) -> bool {
    let max_chars = config
        .options
        .iter()
        .map(|(v, n)| config.option_label(v, n).chars().count())
        .max()
        .unwrap_or(0);
    match config.options.len() {
        0..=2 => max_chars <= 14,
        3 => max_chars <= 10,
        4 => max_chars <= 7,
        _ => false,
    }
}

fn quick_keystroke(num: usize, is_mac: bool) -> Option<Keystroke> {
    const MAC_KEYS: [&str; 9] = [
        "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
    ];
    const OTHER_KEYS: [&str; 9] = [
        "ctrl-1", "ctrl-2", "ctrl-3", "ctrl-4", "ctrl-5", "ctrl-6", "ctrl-7", "ctrl-8", "ctrl-9",
    ];
    let spec = if is_mac {
        MAC_KEYS.get(num.checked_sub(1)?)?
    } else {
        OTHER_KEYS.get(num.checked_sub(1)?)?
    };
    Keystroke::parse(spec).ok()
}

fn open_popover_keystroke(is_mac: bool) -> Option<Keystroke> {
    let spec = if is_mac {
        "cmd-shift-m"
    } else {
        "ctrl-shift-m"
    };
    Keystroke::parse(spec).ok()
}

/// 弹层里的交互状态（搜索词、当前浏览的 Provider、键盘高亮行），挂在 keyed entity 上。
struct ModelPopoverPanel {
    query: Entity<InputState>,
    active_provider: Option<String>,
    highlighted: Option<usize>,
    snapshot: PanelSnapshot,
    popover: Option<WeakEntity<PopoverState>>,
    _query_sub: Subscription,
}

#[derive(Clone)]
struct PanelSnapshot {
    view: WeakEntity<ConversationView>,
    agent: ConversationAgentKind,
    model_state: Option<ModelState>,
    extra_configs: Vec<SessionConfigState>,
    pending_config_values: Vec<(String, String)>,
    next_turn_hint: bool,
    still: bool,
}

impl ModelPopoverPanel {
    fn new(snapshot: PanelSnapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder: SharedString = t!("config.search_models").into_owned().into();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let query_sub = cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.highlighted = Some(0);
                cx.notify();
            }
        });
        let active_provider = snapshot
            .model_state
            .as_ref()
            .and_then(selected_provider_group)
            .map(|group| group.id.clone());
        Self {
            query,
            active_provider,
            highlighted: None,
            snapshot,
            popover: None,
            _query_sub: query_sub,
        }
    }

    fn query_focus(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }

    fn reset_on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.active_provider = self
            .snapshot
            .model_state
            .as_ref()
            .and_then(selected_provider_group)
            .map(|group| group.id.clone());
        self.highlighted = None;
        cx.notify();
    }

    fn sync_snapshot(
        &mut self,
        snapshot: PanelSnapshot,
        popover: Option<WeakEntity<PopoverState>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(popover) = popover {
            self.popover = Some(popover);
        }
        let changed = self.snapshot.agent != snapshot.agent
            || self.snapshot.model_state != snapshot.model_state
            || self.snapshot.extra_configs != snapshot.extra_configs
            || self.snapshot.pending_config_values != snapshot.pending_config_values
            || self.snapshot.next_turn_hint != snapshot.next_turn_hint
            || self.snapshot.still != snapshot.still;
        // 如果当前浏览的 provider 不再存在，回到真实选中那组。
        if let Some(ref active) = self.active_provider {
            let exists = snapshot
                .model_state
                .as_ref()
                .is_some_and(|m| m.provider_groups.iter().any(|g| &g.id == active));
            if !exists {
                self.active_provider = snapshot
                    .model_state
                    .as_ref()
                    .and_then(selected_provider_group)
                    .map(|group| group.id.clone());
            }
        } else {
            self.active_provider = snapshot
                .model_state
                .as_ref()
                .and_then(selected_provider_group)
                .map(|group| group.id.clone());
        }
        self.snapshot = snapshot;
        if changed {
            cx.notify();
        }
    }

    fn visible_models(&self, cx: &App) -> Vec<VisibleModel> {
        let query = self.query.read(cx).value();
        build_visible_models(
            self.snapshot.model_state.as_ref(),
            self.active_provider.as_deref(),
            query.as_ref(),
        )
    }

    fn total_model_count(&self) -> usize {
        let Some(model) = self.snapshot.model_state.as_ref() else {
            return 0;
        };
        if !model.provider_groups.is_empty() {
            model.provider_groups.iter().map(|g| g.options.len()).sum()
        } else {
            model.options.len()
        }
    }

    fn can_switch_model(&self) -> bool {
        self.total_model_count() > 1
    }

    fn choose_model(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_switch_model() {
            return;
        }
        let Some(config_id) = self
            .snapshot
            .model_state
            .as_ref()
            .map(|m| m.config_id.clone())
        else {
            return;
        };
        let _ = self.snapshot.view.update(cx, |view, view_cx| {
            view.select_config_option(config_id, value, view_cx);
            view.model_popover_open = false;
        });
        if let Some(popover) = &self.popover {
            let _ = popover.update(cx, |state, cx| state.dismiss(window, cx));
        }
    }

    fn select_config(&self, config_id: String, value: String, cx: &mut Context<Self>) {
        let _ = self.snapshot.view.update(cx, |view, view_cx| {
            view.select_config_option(config_id, value, view_cx);
        });
    }

    fn step_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.visible_models(cx).len();
        if count == 0 || !self.can_switch_model() {
            return;
        }
        let next = match self.highlighted {
            Some(cur) => ((cur as isize + delta).rem_euclid(count as isize)) as usize,
            None => {
                if delta >= 0 {
                    0
                } else {
                    count - 1
                }
            }
        };
        self.highlighted = Some(next);
        cx.notify();
    }

    fn on_prev(&mut self, _: &ModelPrev, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.update(cx, |state, cx| {
            state.marked_text_range(window, cx).is_some()
        }) {
            return;
        }
        self.step_highlight(-1, cx);
    }

    fn on_next(&mut self, _: &ModelNext, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.update(cx, |state, cx| {
            state.marked_text_range(window, cx).is_some()
        }) {
            return;
        }
        self.step_highlight(1, cx);
    }

    fn on_choose(&mut self, _: &ModelChoose, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.update(cx, |state, cx| {
            state.marked_text_range(window, cx).is_some()
        }) {
            return;
        }
        let visible = self.visible_models(cx);
        if visible.is_empty() || !self.can_switch_model() {
            return;
        }
        let ix = self.highlighted.unwrap_or(0).min(visible.len() - 1);
        self.choose_model(visible[ix].value.clone(), window, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !event.keystroke.modifiers.platform || event.keystroke.modifiers.shift {
            return;
        }
        let Some(digit) = event.keystroke.key.parse::<usize>().ok() else {
            return;
        };
        if !(1..=9).contains(&digit) {
            return;
        }
        let visible = self.visible_models(cx);
        if let Some(model) = visible.get(digit - 1) {
            self.choose_model(model.value.clone(), window, cx);
            cx.stop_propagation();
        }
    }
}

impl Render for ModelPopoverPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_mac = cx.platform_info().os == gpui::OperatingSystem::Mac;
        let query_text = self.query.read(cx).value();
        let total_models = self.total_model_count();
        let can_switch_model = total_models > 1;
        let visible_models = self.visible_models(cx);
        let current_model_value = self
            .snapshot
            .model_state
            .as_ref()
            .map(|m| m.current_value.as_str());
        let provider_groups: &[ModelProviderGroup] = self
            .snapshot
            .model_state
            .as_ref()
            .map_or(&[], |m| m.provider_groups.as_slice());

        let mut panel = div()
            .id("acp-model-popover-body")
            .key_context(MODEL_POPOVER_KEY_CONTEXT)
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_choose))
            .capture_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .w_full();

        // 1. 顶部搜索框（多模型时显示）
        if can_switch_model {
            let shortcut = open_popover_keystroke(is_mac);
            panel = panel
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px(px(10.))
                        .h(px(36.))
                        .child(
                            div().flex_1().min_w_0().child(
                                TextInput::new(&self.query)
                                    .prefix(
                                        Icon::new(GLYPHS.search)
                                            .size(px(14.))
                                            .text_color(rgb(ui_theme::muted())),
                                    )
                                    .bare()
                                    .h(px(34.)),
                            ),
                        )
                        .when_some(shortcut, |row, key| {
                            row.child(Keybind::new([key]).backdrop(ui_theme::surface()))
                        }),
                )
                .child(div().h(px(1.)).bg(ui_theme::hair()));
        }

        // 2. 多 Provider 横向胶囊切换条（搜索时隐藏，因为搜索已跨 Provider）
        if provider_groups.len() > 1 && query_text.trim().is_empty() {
            let active_id = self.active_provider.as_deref();
            let pills = provider_groups.iter().map(|group| {
                let selected = active_id == Some(group.id.as_str());
                let group_id = group.id.clone();
                div()
                    .id((gpui::ElementId::from("acp-provider-pill"), group.id.clone()))
                    .role(gpui::Role::Button)
                    .aria_selected(selected)
                    .flex_shrink_0()
                    .h(px(23.))
                    .px(px(9.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .type_style(TYPE.small_medium)
                    .when(selected, |pill| {
                        pill.bg(rgb(ui_theme::ink()))
                            .text_color(rgb(ui_theme::on_ink()))
                    })
                    .when(!selected, |pill| {
                        pill.bg(ui_theme::row_hover())
                            .text_color(rgb(ui_theme::muted()))
                            .hover(|h| {
                                h.bg(ui_theme::row_selected())
                                    .text_color(rgb(ui_theme::text()))
                            })
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active_provider = Some(group_id.clone());
                        // 切 Provider 标签时把首选或同名模型预高亮，按 Enter 可直接切过去。
                        this.highlighted = this.snapshot.model_state.as_ref().and_then(|model| {
                            let group = model.provider_groups.iter().find(|g| g.id == group_id)?;
                            let target = provider_switch_value(group, &model.current_name)?;
                            group.options.iter().position(|(value, _)| value == target)
                        });
                        cx.notify();
                    }))
                    .child(group.name.clone())
            });
            panel = panel.child(
                div()
                    .id("acp-provider-strip")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .pt(px(8.))
                    .pb(px(2.))
                    .overflow_x_scroll()
                    .children(pills),
            );
        }

        // 3. 模型候选列表
        if total_models > 0 {
            let model_pending = self.snapshot.next_turn_hint
                && self.snapshot.model_state.as_ref().is_some_and(|m| {
                    config_selection_is_pending(&self.snapshot.pending_config_values, &m.config_id)
                });
            if model_pending {
                panel = panel.child(
                    div()
                        .px(px(10.))
                        .pt(px(6.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .type_style(TYPE.small_medium)
                        .text_color(rgb(ui_theme::need()))
                        .child(composer_config_section_label(
                            &t!("config.model"),
                            true,
                            true,
                        )),
                );
            }
            let mut list = div()
                .id("acp-model-list")
                .flex()
                .flex_col()
                .p(px(6.))
                .gap(px(1.))
                .max_h(px(MODEL_LIST_MAX_HEIGHT))
                .overflow_y_scroll();

            if visible_models.is_empty() {
                list = list.child(
                    div()
                        .py(px(14.))
                        .flex()
                        .justify_center()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(ui_theme::faint()))
                        .child(t!("config.no_models").into_owned()),
                );
            } else {
                for (ix, item) in visible_models.iter().enumerate() {
                    let is_current = current_model_value == Some(item.value.as_str());
                    let is_highlighted = self.highlighted == Some(ix);
                    let shortcut = if can_switch_model && ix < 9 {
                        quick_keystroke(ix + 1, is_mac)
                    } else {
                        None
                    };
                    let value_for_click = item.value.clone();
                    let row = div()
                        .id((gpui::ElementId::from("acp-model-row"), item.value.clone()))
                        .role(gpui::Role::ListBoxOption)
                        .aria_selected(is_current)
                        .aria_label(item.label.clone())
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(30.))
                        .px(px(8.))
                        .rounded(px(RADIUS.md))
                        .when(is_current || is_highlighted, |row| {
                            row.bg(ui_theme::row_selected())
                        })
                        .when(!is_current && !is_highlighted && can_switch_model, |row| {
                            row.hover(|h| h.bg(ui_theme::row_hover()))
                        })
                        .when(!can_switch_model, |row| row.opacity(0.65))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .type_style(if is_current {
                                    TYPE.ui_medium
                                } else {
                                    TYPE.ui_compact
                                })
                                .text_color(rgb(ui_theme::text()))
                                .child(truncated_line(item.label.clone())),
                        )
                        .when_some(item.provider_tag.clone(), |row, tag| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .type_style(TYPE.small)
                                    .text_color(rgb(ui_theme::faint()))
                                    .child(tag),
                            )
                        })
                        .when(is_current, |row| {
                            row.child(
                                Icon::new(GLYPHS.check)
                                    .size(px(13.))
                                    .flex_shrink_0()
                                    .text_color(rgb(ui_theme::text())),
                            )
                        })
                        .when_some(shortcut, |row, key| {
                            row.child(Keybind::new([key]).backdrop(ui_theme::surface()))
                        })
                        .when(can_switch_model, |row| {
                            row.on_click(cx.listener(move |this, _, window, cx| {
                                this.choose_model(value_for_click.clone(), window, cx);
                            }))
                        });
                    list = list.child(row);
                }
            }
            panel = panel.child(list);
        }

        // 4. 会话配置区：严格按运行时实际返回的 `extra_configs` 顺序逐项渲染。
        if !self.snapshot.extra_configs.is_empty() {
            let mut footer = div()
                .when(total_models > 0, |el| {
                    el.border_t_1().border_color(ui_theme::hair())
                })
                .px(px(10.))
                .py(px(9.))
                .flex()
                .flex_col()
                .gap(px(9.));

            for cfg in &self.snapshot.extra_configs {
                let pending = self.snapshot.next_turn_hint
                    && config_selection_is_pending(
                        &self.snapshot.pending_config_values,
                        &cfg.config_id,
                    );
                if is_thinking_config(cfg, self.snapshot.agent) {
                    let tiers: Vec<EffortTier> = cfg
                        .options
                        .iter()
                        .map(|(value, name)| {
                            EffortTier::new(value.clone(), cfg.option_label(value, name))
                        })
                        .collect();
                    let selected = selected_option_index(cfg);
                    let config_id = cfg.config_id.clone();
                    let option_values: Vec<String> =
                        cfg.options.iter().map(|(value, _)| value.clone()).collect();
                    let this_weak = cx.entity().downgrade();
                    footer = footer.child(
                        EffortSlider::new(
                            (
                                gpui::ElementId::from("acp-effort-slider"),
                                cfg.config_id.clone(),
                            ),
                            cfg.display_name(),
                            tiers,
                        )
                        .selected(selected)
                        .pending_hint(pending.then(|| t!("config.next_turn").into_owned()))
                        .still(self.snapshot.still)
                        .on_select(move |ix, _, cx| {
                            if let Some(value) = option_values.get(ix) {
                                let config_id = config_id.clone();
                                let value = value.clone();
                                let _ = this_weak.update(cx, |panel, cx| {
                                    panel.select_config(config_id, value, cx);
                                });
                            }
                        }),
                    );
                    continue;
                }
                let title = cfg.display_name();
                let pending_el = pending.then(|| {
                    div()
                        .type_style(TYPE.small_medium)
                        .text_color(rgb(ui_theme::need()))
                        .child(t!("config.next_turn").into_owned())
                });

                if cfg.is_switch() {
                    let is_on = cfg.boolean.unwrap_or_else(|| {
                        session_config_current_value(cfg)
                            .is_some_and(|v| matches!(v, "true" | "on"))
                    });
                    let on_val = cfg
                        .options
                        .iter()
                        .find(|(v, _)| matches!(v.as_str(), "true" | "on"))
                        .map(|(v, _)| v.clone())
                        .unwrap_or_else(|| "true".into());
                    let off_val = cfg
                        .options
                        .iter()
                        .find(|(v, _)| matches!(v.as_str(), "false" | "off"))
                        .map(|(v, _)| v.clone())
                        .unwrap_or_else(|| "false".into());
                    let config_id = cfg.config_id.clone();
                    let this_weak = cx.entity().downgrade();
                    footer = footer.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .type_style(TYPE.text_xsmall_medium)
                                            .text_color(rgb(ui_theme::muted()))
                                            .truncate()
                                            .child(title.clone()),
                                    )
                                    .children(pending_el),
                            )
                            .child(
                                Switch::new((
                                    gpui::ElementId::from("acp-config-switch"),
                                    cfg.config_id.clone(),
                                ))
                                .checked(is_on)
                                .accessibility_label(title)
                                .on_change(
                                    move |checked, _, cx| {
                                        let value = if checked {
                                            on_val.clone()
                                        } else {
                                            off_val.clone()
                                        };
                                        let config_id = config_id.clone();
                                        let _ = this_weak.update(cx, |panel, cx| {
                                            panel.select_config(config_id, value, cx);
                                        });
                                    },
                                ),
                            ),
                    );
                } else if cfg.options.len() <= 4 && config_fits_equal_segments(cfg) {
                    let segments: Vec<Segment> = cfg
                        .options
                        .iter()
                        .map(|(value, name)| Segment::new(cfg.option_label(value, name)))
                        .collect();
                    let selected = selected_option_index(cfg);
                    let option_values: Vec<String> =
                        cfg.options.iter().map(|(v, _)| v.clone()).collect();
                    let config_id = cfg.config_id.clone();
                    let this_weak = cx.entity().downgrade();
                    let inline = config_fits_inline(cfg);
                    let segmented = Segmented::new((
                        gpui::ElementId::from("acp-config-seg"),
                        cfg.config_id.clone(),
                    ))
                    .segments(segments)
                    .selected(Some(selected))
                    .when(!inline, |seg| seg.w_full())
                    .on_select(move |ix, _, cx| {
                        if let Some(value) = option_values.get(ix) {
                            let config_id = config_id.clone();
                            let value = value.clone();
                            let _ = this_weak.update(cx, |panel, cx| {
                                panel.select_config(config_id, value, cx);
                            });
                        }
                    });
                    if inline {
                        footer = footer.child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .type_style(TYPE.text_xsmall_medium)
                                                .text_color(rgb(ui_theme::muted()))
                                                .truncate()
                                                .child(title),
                                        )
                                        .children(pending_el),
                                )
                                .child(segmented),
                        );
                    } else {
                        footer = footer.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(5.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .type_style(TYPE.text_xsmall_medium)
                                                .text_color(rgb(ui_theme::muted()))
                                                .child(title),
                                        )
                                        .children(pending_el),
                                )
                                .child(segmented),
                        );
                    }
                } else if cfg.options.len() <= 4 {
                    // 含较长标签（如 `Accept edits`）的 3~4 项配置：按内容自然分宽的分段条，不截断省略号。
                    let selected = selected_option_index(cfg);
                    let items = cfg.options.iter().enumerate().map(|(ix, (value, name))| {
                        let active = ix == selected;
                        let label = cfg.option_label(value, name);
                        let config_id = cfg.config_id.clone();
                        let value = value.clone();
                        let this_weak = cx.entity().downgrade();
                        div()
                            .id((
                                gpui::ElementId::from("acp-config-seg-opt"),
                                format!("{}:{value}", cfg.config_id),
                            ))
                            .role(gpui::Role::Button)
                            .aria_selected(active)
                            .flex_grow(1.)
                            .h(px(26.))
                            .px(px(8.))
                            .rounded(px(RADIUS.sm))
                            .flex()
                            .items_center()
                            .justify_center()
                            .whitespace_nowrap()
                            .type_style(if active {
                                TYPE.text_xsmall_medium
                            } else {
                                TYPE.text_xsmall
                            })
                            .when(active, |pill| {
                                pill.bg(rgb(ui_theme::bg()))
                                    .border_1()
                                    .border_color(ui_theme::hair_strong())
                                    .text_color(rgb(ui_theme::text()))
                            })
                            .when(!active, |pill| {
                                pill.text_color(rgb(ui_theme::muted())).hover(|h| {
                                    h.bg(rgb(ui_theme::soft_hover()))
                                        .text_color(rgb(ui_theme::text()))
                                })
                            })
                            .on_click(move |_, _, cx| {
                                let _ = this_weak.update(cx, |panel, cx| {
                                    panel.select_config(config_id.clone(), value.clone(), cx);
                                });
                            })
                            .child(label)
                    });
                    footer = footer.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .type_style(TYPE.text_xsmall_medium)
                                            .text_color(rgb(ui_theme::muted()))
                                            .child(title),
                                    )
                                    .children(pending_el),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .w_full()
                                    .p(px(1.))
                                    .rounded(px(RADIUS.md))
                                    .bg(rgb(ui_theme::soft()))
                                    .border_1()
                                    .border_color(ui_theme::hair())
                                    .children(items),
                            ),
                    );
                } else {
                    // 兜底：超过 4 项的非常规 ACP 选择项，以紧凑胶囊组平铺，不丢任何配置。
                    let selected = selected_option_index(cfg);
                    let pills = cfg.options.iter().enumerate().map(|(ix, (value, name))| {
                        let active = ix == selected;
                        let label = cfg.option_label(value, name);
                        let config_id = cfg.config_id.clone();
                        let value = value.clone();
                        let this_weak = cx.entity().downgrade();
                        div()
                            .id((
                                gpui::ElementId::from("acp-config-opt"),
                                format!("{}:{value}", cfg.config_id),
                            ))
                            .role(gpui::Role::Button)
                            .aria_selected(active)
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(px(RADIUS.sm))
                            .flex()
                            .items_center()
                            .type_style(if active {
                                TYPE.small_medium
                            } else {
                                TYPE.small
                            })
                            .when(active, |pill| {
                                pill.bg(rgb(ui_theme::ink()))
                                    .text_color(rgb(ui_theme::on_ink()))
                            })
                            .when(!active, |pill| {
                                pill.bg(ui_theme::row_hover())
                                    .text_color(rgb(ui_theme::muted()))
                                    .hover(|h| {
                                        h.bg(ui_theme::row_selected())
                                            .text_color(rgb(ui_theme::text()))
                                    })
                            })
                            .on_click(move |_, _, cx| {
                                let _ = this_weak.update(cx, |panel, cx| {
                                    panel.select_config(config_id.clone(), value.clone(), cx);
                                });
                            })
                            .child(label)
                    });
                    footer = footer.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .type_style(TYPE.text_xsmall_medium)
                                            .text_color(rgb(ui_theme::muted()))
                                            .child(title),
                                    )
                                    .children(pending_el),
                            )
                            .child(div().flex().flex_wrap().gap(px(4.)).children(pills)),
                    );
                }
            }
            panel = panel.child(footer);
        }

        panel
    }
}

/// 输入栏底部模型触发胶囊 + 新弹层。用 `RenderOnce` 接管 `window.use_keyed_state` 与焦点管理。
#[derive(IntoElement)]
pub(super) struct ModelPopoverControl {
    id: gpui::ElementId,
    view: WeakEntity<ConversationView>,
    agent: ConversationAgentKind,
    model_state: Option<ModelState>,
    extra_configs: Vec<SessionConfigState>,
    pending_config_values: Vec<(String, String)>,
    next_turn_hint: bool,
    model_label: SharedString,
    has_pending_notice: bool,
    open: bool,
    still: bool,
}

impl ModelPopoverControl {
    #[expect(
        clippy::too_many_arguments,
        reason = "groups composer model popover inputs passed directly from render_composer"
    )]
    pub(super) fn new(
        id: impl Into<gpui::ElementId>,
        view: WeakEntity<ConversationView>,
        agent: ConversationAgentKind,
        model_state: Option<ModelState>,
        extra_configs: Vec<SessionConfigState>,
        pending_config_values: Vec<(String, String)>,
        next_turn_hint: bool,
        model_label: impl Into<SharedString>,
        has_pending_notice: bool,
        open: bool,
        still: bool,
    ) -> Self {
        Self {
            id: id.into(),
            view,
            agent,
            model_state,
            extra_configs,
            pending_config_values,
            next_turn_hint,
            model_label: model_label.into(),
            has_pending_notice,
            open,
            still,
        }
    }
}

impl RenderOnce for ModelPopoverControl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let thinking_summary = self
            .extra_configs
            .iter()
            .find(|cfg| is_thinking_config(cfg, self.agent))
            .map(|cfg| {
                let ix = selected_option_index(cfg);
                let p = effort_progress(ix, cfg.options.len());
                (effort_dot_color(p), cfg.current_label())
            });

        let mut trigger = Button::new("acp-model-pill")
            .quiet()
            .small()
            .dropdown_caret(true)
            .accessibility_label(self.model_label.clone())
            .when(self.has_pending_notice, |button| {
                button.text_color(rgb(ui_theme::need()))
            });
        if let Some((dot_rgb, effort_label)) = thinking_summary {
            trigger = trigger
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(rgb(dot_rgb)),
                )
                .child(self.model_label.clone())
                .child(
                    div()
                        .type_style(TYPE.small)
                        .text_color(rgb(if self.has_pending_notice {
                            ui_theme::need()
                        } else {
                            ui_theme::faint()
                        }))
                        .child(format!("· {effort_label}")),
                );
        } else {
            trigger = trigger.label(self.model_label.clone());
        }

        let snapshot = PanelSnapshot {
            view: self.view.clone(),
            agent: self.agent,
            model_state: self.model_state,
            extra_configs: self.extra_configs,
            pending_config_values: self.pending_config_values,
            next_turn_hint: self.next_turn_hint,
            still: self.still,
        };
        let panel = window.use_keyed_state((self.id.clone(), "panel"), cx, {
            let snapshot = snapshot.clone();
            move |window, cx| ModelPopoverPanel::new(snapshot, window, cx)
        });
        panel.update(cx, |panel, cx| panel.sync_snapshot(snapshot, None, cx));
        let can_search = panel.read(cx).can_switch_model();
        let query_focus = panel.read(cx).query_focus(cx);

        let view_for_open = self.view.clone();
        let panel_for_open = panel.clone();
        Popover::new(self.id)
            .anchor(Anchor::BottomLeft)
            .open(self.open)
            .when(can_search, |pop| pop.track_focus(&query_focus))
            .on_open_change(move |open, window, cx| {
                let open = *open;
                if open {
                    panel_for_open.update(cx, |panel, cx| panel.reset_on_open(window, cx));
                }
                let _ = view_for_open.update(cx, |view, cx| {
                    view.model_popover_open = open;
                    if open {
                        view.usage_popover_open = false;
                        view.background_tasks_open = false;
                        view.subagents_open = false;
                    }
                    cx.notify();
                });
            })
            .w(px(MODEL_POPOVER_WIDTH))
            .max_w(px(MODEL_POPOVER_WIDTH))
            .p_0()
            .rounded(px(RADIUS.xl))
            .trigger(trigger)
            .content(move |_, _, cx| {
                let popover = cx.entity().downgrade();
                panel.update(cx, |panel, _| panel.popover = Some(popover));
                panel
            })
    }
}
