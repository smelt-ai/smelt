//! 技能面板的画法：只读视图上缓存好的状态。
//!
//! 只有一层列表，不切页：点一行就地展开，作用范围表单在展开区里再展开一层；「导入」展开在
//! 列表顶部。问题就标在出问题的那一行上，不另设汇总横幅。只有删除、清理这类确认在窗口中间
//! 问一句。

use std::path::Path;
use std::time::SystemTime;

use gpui::prelude::FluentBuilder;
use gpui::*;
use rust_i18n::t;
use smelt_plugin_api::model::chrono;
use smelt_plugin_api::model::font_config;
use smelt_plugin_api::ui::alert::Alert;
use smelt_plugin_api::ui::badge::Badge;
use smelt_plugin_api::ui::button::Button;
use smelt_plugin_api::ui::choice::{Checkbox, Radio};
use smelt_plugin_api::ui::dialog::Confirm;
use smelt_plugin_api::ui::kit::{RADIUS, TYPE, TypeStyled as _};
use smelt_plugin_api::ui::menu::{Menu, MenuItem, MenuTrigger};
use smelt_plugin_api::ui::scrollbar::{Scrollable, ScrollbarAxis};
use smelt_plugin_api::ui::{IconName, TextInput, ui_theme};

use crate::ops;
use crate::scan::{Access, Placement, Skill, SkillCopy};
use crate::{FormKind, SkillsPanel};

const XS: f32 = 4.;
const SM: f32 = 8.;
const MD: f32 = 12.;
const LG: f32 = 16.;
/// 失效链接在确认框里最多列几条。
const BROKEN_PREVIEW: usize = 5;

fn muted() -> Hsla {
    rgb(ui_theme::muted()).into()
}

fn id(prefix: &str, path: &Path) -> ElementId {
    ElementId::Name(format!("{prefix}-{}", path.display()).into())
}

fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .type_style(TYPE.text_xsmall)
        .text_color(muted())
        .child(text.into())
}

fn mono(text: impl Into<SharedString>) -> Div {
    div()
        .type_style(TYPE.mono)
        .font_family(font_config::font_family())
        .child(text.into())
}

/// 小标记。不给状态是中性灰；状态色只给状态。
fn tag(text: impl Into<SharedString>) -> Badge {
    Badge::new().child(text.into())
}

/// 有边框的一块，放表单或列表。
fn boxed() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SM))
        .p(px(MD))
        .border_1()
        .border_color(ui_theme::hair())
        .rounded(px(RADIUS.lg))
}

/// 小节：标题一行，右边可以放一个操作。
fn section(title: impl Into<SharedString>, action: Option<AnyElement>) -> Div {
    div().flex().flex_col().gap(px(SM)).child(
        div()
            .flex()
            .items_center()
            .gap(px(SM))
            .child(
                div()
                    .flex_1()
                    .type_style(TYPE.text_xsmall_medium)
                    .text_color(muted())
                    .child(title.into()),
            )
            .children(action),
    )
}

fn edited(modified: Option<SystemTime>) -> String {
    match modified {
        Some(time) => chrono::DateTime::<chrono::Local>::from(time)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => t!("time.unknown").into_owned(),
    }
}

/// 副本放在哪：通用 / 旧位置 / 某家 agent。
fn place_label(placement: Placement) -> String {
    match placement {
        Placement::Universal => t!("place.universal").into_owned(),
        Placement::Legacy => t!("place.legacy").into_owned(),
        Placement::Agent(label) => label.to_string(),
    }
}

impl Render for SkillsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let scan = self.scan.clone();
        let shown: Vec<&Skill> = scan
            .skills
            .iter()
            .filter(|skill| {
                query.is_empty()
                    || skill.name.to_lowercase().contains(&query)
                    || skill.description.to_lowercase().contains(&query)
            })
            .collect();

        let mut list = div().flex().flex_col().pb(px(LG));
        if self.importing {
            list = list.child(self.render_import(cx));
        }
        if scan.skills.is_empty() {
            list = list.child(empty(t!("empty.none_title"), t!("empty.none_body")));
        } else if shown.is_empty() {
            list = list.child(empty(
                t!("empty.no_match_title"),
                t!("empty.no_match_body", query = query),
            ));
        }
        for project_scope in [true, false] {
            let group: Vec<&Skill> = shown
                .iter()
                .copied()
                .filter(|skill| skill.project_scope == project_scope)
                .collect();
            if group.is_empty() {
                continue;
            }
            list = list.child(self.group_label(project_scope, group.len()));
            for skill in group {
                list = list.child(self.render_row(skill, cx));
            }
        }
        // 失效链接不属于任何技能，搜索时不列。
        if query.is_empty() && !scan.broken.is_empty() {
            list = list.child(self.render_broken(cx));
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .text_color(rgb(ui_theme::text()))
            .child(self.render_header(cx))
            .children(self.render_notice(cx))
            .child(
                Scrollable::new("skills-list", list, &self.scroll, ScrollbarAxis::Vertical)
                    .flex_1()
                    .min_h_0(),
            )
    }
}

fn empty(title: impl Into<SharedString>, body: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(XS))
        .p(px(LG))
        .child(div().type_style(TYPE.ui_medium).child(title.into()))
        .child(hint(body))
}

impl SkillsPanel {
    /// 顶上一条：搜索、导入、更多。
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let entity = cx.entity();
        let repair_all = self.scan.skills.iter().any(crate::needs_repair);
        let trash = self.trash.clone();
        let more = MenuTrigger::dropdown(
            "skills-more",
            Button::new("skills-more-button")
                .quiet()
                .small()
                .icon(IconName::Ellipsis)
                .accessibility_label(t!("menu.more")),
            move |_, _| {
                let refresh = entity.clone();
                let open = entity.clone();
                let trash = trash.clone();
                let mut menu = Menu::new()
                    .item(MenuItem::new(t!("menu.refresh")).on_click(move |_, _, cx| {
                        refresh.update(cx, |this, cx| this.refresh(cx));
                    }))
                    .item(
                        MenuItem::new(t!("menu.open_trash")).on_click(move |_, _, cx| {
                            open.update(cx, |this, cx| this.open_path(&trash, true, cx));
                        }),
                    );
                if repair_all {
                    let repair = entity.clone();
                    menu = menu.item(MenuItem::new(t!("action.repair_all")).on_click(
                        move |_, _, cx| repair.update(cx, |this, cx| this.repair_all(cx)),
                    ));
                }
                menu
            },
        );
        let toggle = cx.entity();
        let import = Button::new("skills-import")
            .small()
            .label(if self.importing {
                t!("import.hide")
            } else {
                t!("import.button")
            })
            .on_click(move |_, _, cx| toggle.update(cx, |this, cx| this.toggle_import(cx)));
        let import = if self.importing {
            import.secondary()
        } else {
            import.primary()
        };
        div()
            .flex()
            .items_center()
            .gap(px(SM))
            .px(px(LG))
            .py(px(SM))
            .border_b_1()
            .border_color(ui_theme::hair())
            .child(
                div().flex_1().min_w_0().child(
                    TextInput::new(&self.search)
                        .small()
                        .cleanable(true)
                        .aria_label(t!("search.label")),
                ),
            )
            .child(import)
            .child(more)
    }

    /// 上一次操作的结果：出错用提示块，做完了只要一行弱色的字。
    fn render_notice(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let notice = self.notice.as_ref()?;
        let entity = cx.entity();
        let body = if notice.error {
            Alert::error("skills-notice", notice.text.clone()).into_any_element()
        } else {
            hint(notice.text.clone()).into_any_element()
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(SM))
                .px(px(LG))
                .pt(px(SM))
                .child(div().flex_1().min_w_0().child(body))
                .child(
                    Button::new("skills-notice-close")
                        .quiet()
                        .small()
                        .label(t!("notice.close"))
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.notice = None;
                                cx.notify();
                            });
                        }),
                ),
        )
    }

    fn group_label(&self, project_scope: bool, count: usize) -> Div {
        let mut label = div()
            .flex()
            .gap(px(XS))
            .px(px(LG))
            .pt(px(MD))
            .pb(px(XS))
            .type_style(TYPE.small)
            .text_color(muted())
            .child(if project_scope {
                t!("scope.project")
            } else {
                t!("scope.user")
            })
            .child(count.to_string());
        if project_scope && let (Some(home), Some(project)) = (&self.home, &self.project) {
            label = label.child(format!("· {}", crate::scan::pretty(home, project)));
        }
        label
    }

    fn pretty(&self, path: &Path) -> String {
        match &self.home {
            Some(home) => crate::scan::pretty(home, path),
            None => path.display().to_string(),
        }
    }

    // ------------------------------------------------------------ 行

    /// 一行，点开就地展开详情。收起时不建展开区。
    fn render_row(&self, skill: &Skill, cx: &mut Context<Self>) -> Div {
        let open = self.expanded.as_deref() == Some(skill.dir.as_path());
        let entity = cx.entity();
        let dir = skill.dir.clone();
        let mut lines = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SM))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .type_style(TYPE.ui_medium)
                            .child(skill.name.clone()),
                    )
                    .child(chip(skill)),
            );
        // 展开后下面有完整描述，这里不再重复一行截断的。
        if !open {
            let description = if skill.description.is_empty() {
                t!("no_description").into_owned()
            } else {
                skill.description.clone()
            };
            lines = lines.child(
                div()
                    .truncate()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted())
                    .child(description),
            );
        }
        let header = div()
            .id(id("row", &skill.dir))
            .flex()
            .items_center()
            .gap(px(MD))
            .px(px(LG))
            .py(px(SM))
            .cursor_pointer()
            .when(open, |row| row.bg(ui_theme::row_selected()))
            .hover(|row| row.bg(ui_theme::row_hover()))
            .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.toggle_row(&dir, cx)))
            .child(lines)
            .child(summary(skill));
        div()
            .flex()
            .flex_col()
            .child(header)
            .when(open, |row| row.child(self.render_detail(skill, cx)))
    }

    /// 展开区，从上到下：完整描述 → 同名冲突 → 可用 agent → 作用范围 → 路径和删除。看到
    /// 什么就在什么旁边改。
    fn render_detail(&self, skill: &Skill, cx: &mut Context<Self>) -> Div {
        let conflicted = !skill.conflicts.is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(MD))
            .px(px(LG))
            .pt(px(XS))
            .pb(px(MD))
            .border_b_1()
            .border_color(ui_theme::hair())
            .when(!skill.description.is_empty(), |detail| {
                detail.child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .child(skill.description.clone()),
                )
            })
            .when(conflicted, |detail| {
                detail.child(self.render_conflict(skill, cx))
            })
            .child(self.render_coverage(skill, cx))
            .when(!conflicted, |detail| {
                detail.child(self.render_scope_line(skill, cx))
            })
            .child(self.render_footer(skill, cx))
    }

    /// 可用 agent：能用的只写名字（读 `.agents`、经链接、本体对用户都是「能用」），缺链接就
    /// 在标题右边补。
    fn render_coverage(&self, skill: &Skill, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        if skill.placement == Placement::Legacy {
            let migrate = Button::new(id("migrate", &skill.dir))
                .primary()
                .small()
                .label(t!("migrate.button"))
                .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.migrate(cx)));
            return section(t!("coverage.title"), Some(migrate.into_any_element()))
                .child(hint(t!("coverage.legacy")));
        }
        let missing: Vec<&str> = skill
            .coverage
            .iter()
            .filter(|row| row.access == Access::Missing)
            .map(|row| row.agent)
            .collect();
        let repair = (!missing.is_empty()).then(|| {
            let target = skill.clone();
            Button::new(id("repair", &skill.dir))
                .primary()
                .small()
                .label(if missing.len() == 1 {
                    t!("coverage.repair_one", agent = missing[0])
                } else {
                    t!("coverage.repair_many")
                })
                .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.repair(&target, cx)))
                .into_any_element()
        });
        let body = if skill.coverage.is_empty() {
            hint(t!("coverage.none"))
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(px(XS))
                .children(skill.coverage.iter().map(|row| {
                    if row.access == Access::Missing {
                        tag(t!("coverage.row_missing", agent = row.agent)).need()
                    } else {
                        tag(row.agent).ok()
                    }
                }))
        };
        section(t!("coverage.title"), repair).child(body)
    }

    /// 作用范围：现状一句话，右边「修改」，表单就在这一行下面展开。
    fn render_scope_line(&self, skill: &Skill, cx: &mut Context<Self>) -> Div {
        let who = match skill.placement {
            Placement::Agent(label) => t!("scope.only", agent = label),
            _ => t!("scope.universal_all"),
        };
        let level = if skill.project_scope {
            t!("scope.project")
        } else {
            t!("scope.user")
        };
        let entity = cx.entity();
        let toggle = Button::new(id("scope", &skill.dir))
            .secondary()
            .small()
            .label(if self.editing {
                t!("scope.hide")
            } else {
                t!("scope.change")
            })
            .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.toggle_scope(cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(SM))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SM))
                    .child(
                        div()
                            .flex_none()
                            .type_style(TYPE.text_xsmall_medium)
                            .text_color(muted())
                            .child(t!("scope.title")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_style(TYPE.text_xsmall)
                            .child(format!("{who} · {level}")),
                    )
                    .child(toggle),
            )
            .when(self.editing, |line| {
                line.child(self.render_scope_editor(cx))
            })
    }

    /// 作用范围表单：给谁、层级、将执行的步骤、取消和应用。
    fn render_scope_editor(&self, cx: &mut Context<Self>) -> Div {
        let cancel = cx.entity();
        let apply = cx.entity();
        boxed()
            .children(self.render_form(FormKind::Scope, cx))
            .child(plan_view(
                &self.scope.plan,
                Some(t!("scope.unchanged").into_owned()),
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SM))
                    .child(
                        Button::new("skills-scope-cancel")
                            .secondary()
                            .small()
                            .label(t!("cancel"))
                            .on_click(move |_, _, cx| {
                                cancel.update(cx, |this, cx| this.toggle_scope(cx))
                            }),
                    )
                    .child(
                        Button::new("skills-scope-apply")
                            .primary()
                            .small()
                            .label(t!("scope.apply"))
                            .disabled(!self.scope.ready())
                            .on_click(move |_, _, cx| {
                                apply.update(cx, |this, cx| this.apply_scope(cx))
                            }),
                    ),
            )
    }

    /// 最后一行：路径和修改时间（点路径在文件管理器里打开），右边删除。
    fn render_footer(&self, skill: &Skill, cx: &mut Context<Self>) -> Div {
        let open = cx.entity();
        let dir = skill.dir.clone();
        let path = div()
            .id(id("path", &skill.dir))
            .flex_1()
            .min_w_0()
            .truncate()
            .type_style(TYPE.mono)
            .font_family(font_config::font_family())
            .text_color(muted())
            .cursor_pointer()
            .hover(|path| path.text_color(rgb(ui_theme::text())).underline())
            .on_click(move |_, _, cx| open.update(cx, |this, cx| this.open_path(&dir, false, cx)))
            .child(format!(
                "{} · {}",
                self.pretty(&skill.dir),
                edited(skill.modified)
            ));
        let entity = cx.entity();
        let target = skill.clone();
        let trash = self.pretty(&self.trash);
        let delete = Button::new(id("trash", &skill.dir))
            .quiet()
            .small()
            .label(t!("delete.button"))
            .on_click(move |_, window, cx| {
                let entity = entity.clone();
                let target = target.clone();
                Confirm::new(
                    t!("delete.title", name = target.name),
                    t!("delete.confirm"),
                    move |_, cx| {
                        entity.update(cx, |this, cx| this.trash(&target, cx));
                        true
                    },
                )
                .description(t!("delete.body", trash = trash))
                .open(window, cx);
            });
        div()
            .flex()
            .items_center()
            .gap(px(SM))
            .pt(px(SM))
            .border_t_1()
            .border_color(ui_theme::hair())
            .child(path)
            .child(delete)
    }

    /// 同名冲突：每份一个单选「以它为准」，下面是它在哪；右上角留下选中的一份。
    fn render_conflict(&self, skill: &Skill, cx: &mut Context<Self>) -> Div {
        let entity = cx.entity();
        let target = skill.clone();
        let keep_button = Button::new(id("keep", &skill.dir))
            .primary()
            .small()
            .label(t!("conflict.keep"))
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| this.resolve_conflict(&target, cx))
            });
        let copies: Vec<SkillCopy> = std::iter::once(skill.as_copy())
            .chain(skill.conflicts.iter().cloned())
            .collect();
        let keep = self.keep.get(&skill.dir).unwrap_or(&skill.dir);
        let choices = copies.iter().map(|copy| {
            let entity = cx.entity();
            let owner = skill.dir.clone();
            let picked = copy.dir.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    Radio::new(id("keep-copy", &copy.dir))
                        .label(format!(
                            "{} · {}",
                            place_label(copy.placement),
                            edited(copy.modified)
                        ))
                        .checked(*keep == copy.dir)
                        .on_select(move |_, cx| {
                            entity.update(cx, |this, cx| {
                                this.keep.insert(owner.clone(), picked.clone());
                                cx.notify();
                            })
                        }),
                )
                .child(mono(self.pretty(&copy.dir)).pl(px(24.)).text_color(muted()))
        });
        section(
            t!("conflict.title", name = skill.name),
            Some(keep_button.into_any_element()),
        )
        .child(hint(t!(
            "conflict.hint",
            count = copies.len(),
            trash = self.pretty(&self.trash)
        )))
        .child(boxed().children(choices))
    }

    // ------------------------------------------------------------ 表单

    /// 「给谁」「层级」两行单选，指定 agent 时加一排勾选。两个表单共用。
    fn render_form(&self, kind: FormKind, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let request = &self.form(kind).request;
        let prefix = match kind {
            FormKind::Scope => "scope",
            FormKind::Import => "import",
        };
        let radio = |name: &str, label: SharedString, checked: bool| {
            Radio::new(SharedString::from(format!("skills-{prefix}-{name}")))
                .label(label)
                .checked(checked)
        };
        let line = |label: SharedString, controls: Vec<AnyElement>| {
            div()
                .flex()
                .items_center()
                .gap(px(SM))
                .child(div().w(px(56.)).flex_none().child(hint(label)))
                .child(div().flex().flex_wrap().gap(px(MD)).children(controls))
                .into_any_element()
        };
        let universal = cx.entity();
        let specific = cx.entity();
        let mut rows = vec![line(
            t!("scope.for").into(),
            vec![
                radio(
                    "universal",
                    t!("scope.universal_all").into(),
                    request.universal,
                )
                .on_select(move |_, cx| {
                    universal.update(cx, |this, cx| this.set_universal(kind, true, cx))
                })
                .into_any_element(),
                radio("specific", t!("scope.specific").into(), !request.universal)
                    .on_select(move |_, cx| {
                        specific.update(cx, |this, cx| this.set_universal(kind, false, cx))
                    })
                    .into_any_element(),
            ],
        )];
        if !request.universal {
            // 只列在用的 agent。已经勾上的留着：技能现在就放在一个没装的 agent 目录里时，
            // 藏掉它等于悄悄改了作用范围。
            let choices = ops::scope_choices(self.installed(), request.project_scope)
                .into_iter()
                .filter(|(label, installed)| *installed || request.labels.contains(label));
            let boxes = choices.map(|(label, installed)| {
                let entity = cx.entity();
                Checkbox::new(SharedString::from(format!("skills-{prefix}-agent-{label}")))
                    .label(if installed {
                        label.to_string()
                    } else {
                        t!("scope.not_installed", agent = label).into_owned()
                    })
                    .checked(request.labels.contains(&label))
                    .on_change(move |_, _, cx| {
                        entity.update(cx, |this, cx| this.toggle_label(kind, label, cx))
                    })
            });
            rows.push(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(SM))
                    .pl(px(64.))
                    .children(boxes)
                    .into_any_element(),
            );
        }
        let user = cx.entity();
        let project = match (&self.home, &self.project) {
            (Some(home), Some(root)) => {
                let entity = cx.entity();
                radio(
                    "project",
                    t!("scope.project_at", path = crate::scan::pretty(home, root)).into(),
                    request.project_scope,
                )
                .on_select(move |_, cx| {
                    entity.update(cx, |this, cx| this.set_level(kind, true, cx))
                })
            }
            // 没有打开项目时选不了。
            _ => radio("project", t!("scope.project_none").into(), false).disabled(true),
        };
        rows.push(line(
            t!("scope.level").into(),
            vec![
                radio("user", t!("scope.user").into(), !request.project_scope)
                    .on_select(move |_, cx| {
                        user.update(cx, |this, cx| this.set_level(kind, false, cx))
                    })
                    .into_any_element(),
                project.into_any_element(),
            ],
        ));
        rows
    }

    /// 列表顶部的导入面板：来源 → 装到哪里 → 将执行。
    fn render_import(&self, cx: &mut Context<Self>) -> Div {
        let import = &self.import;
        let pick = cx.entity();
        let pick_button = Button::new("skills-import-pick")
            .small()
            .loading(import.picking)
            .on_click(move |_, window, cx| {
                pick.update(cx, |this, cx| this.pick_import(window, cx))
            });
        let source = match (&import.source, &import.info) {
            (Some(source), Some(info)) => div()
                .flex()
                .flex_col()
                .gap(px(XS))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SM))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().type_style(TYPE.ui_medium).child(info.name.clone()))
                                .child(mono(self.pretty(source)).truncate().text_color(muted())),
                        )
                        .child(pick_button.secondary().label(t!("import.repick"))),
                )
                .when(!info.description.is_empty(), |source| {
                    source.child(hint(info.description.clone()))
                }),
            _ => div()
                .flex()
                .flex_col()
                .gap(px(SM))
                .child(hint(t!("import.source_hint")))
                .child(
                    div()
                        .flex()
                        .child(pick_button.primary().label(t!("import.pick"))),
                ),
        };

        let mut plan = section(t!("plan.title"), None);
        if import.source.is_none() {
            plan = plan.child(hint(t!("import.plan_pending")));
        } else {
            plan = plan.child(plan_view(&import.form.plan, None));
        }
        if matches!(&import.form.plan, Some(Ok(preview)) if preview.replaces) {
            plan = plan.child(Alert::need(
                "skills-import-replaces",
                t!("import.replaces", trash = self.pretty(&self.trash)),
            ));
        }
        if let Some(info) = &import.info
            && info.has_global
            && import.form.request.project_scope
        {
            plan = plan.child(Alert::need(
                "skills-import-shadows",
                t!("import.shadows_user", name = info.name),
            ));
        }

        let cancel = cx.entity();
        let submit = cx.entity();
        let ready = import.source.is_some() && import.form.ready();
        div()
            .flex()
            .flex_col()
            .gap(px(MD))
            .m(px(LG))
            .mb_0()
            .child(div().type_style(TYPE.ui_medium).child(t!("import.title")))
            .child(
                boxed()
                    .child(section(t!("import.source"), None).child(source))
                    .child(
                        section(t!("import.destination"), None)
                            .children(self.render_form(FormKind::Import, cx)),
                    )
                    .child(plan)
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(SM))
                            .child(
                                Button::new("skills-import-cancel")
                                    .secondary()
                                    .small()
                                    .label(t!("cancel"))
                                    .on_click(move |_, _, cx| {
                                        cancel.update(cx, |this, cx| this.toggle_import(cx))
                                    }),
                            )
                            .child(
                                Button::new("skills-import-submit")
                                    .primary()
                                    .small()
                                    .label(t!("import.button"))
                                    .disabled(!ready)
                                    .on_click(move |_, _, cx| {
                                        submit.update(cx, |this, cx| this.submit_import(cx))
                                    }),
                            ),
                    ),
            )
    }

    // ------------------------------------------------------------ 失效链接

    /// 列表最后一组：失效链接。指向的目录已经不存在，只删链接本身。
    fn render_broken(&self, cx: &mut Context<Self>) -> Div {
        let broken = &self.scan.broken;
        let mut listed: Vec<String> = broken
            .iter()
            .take(BROKEN_PREVIEW)
            .map(|link| self.pretty(&link.path))
            .collect();
        if broken.len() > listed.len() {
            listed.push(t!("broken.more", count = broken.len()).into_owned());
        }
        let entity = cx.entity();
        let count = broken.len();
        let body = t!("broken.confirm_body", paths = listed.join("\n")).into_owned();
        let prune = Button::new("skills-prune")
            .quiet()
            .small()
            .label(t!("broken.button"))
            .on_click(move |_, window, cx| {
                let entity = entity.clone();
                Confirm::new(
                    match count {
                        1 => t!("broken.confirm_title.one"),
                        count => t!("broken.confirm_title.other", count = count),
                    },
                    t!("broken.confirm"),
                    move |_, cx| {
                        entity.update(cx, |this, cx| this.prune(cx));
                        true
                    },
                )
                .description(body.clone())
                .open(window, cx);
            });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(XS))
                    .px(px(LG))
                    .pt(px(MD))
                    .pb(px(XS))
                    .type_style(TYPE.small)
                    .text_color(muted())
                    .child(t!("broken.title"))
                    .child(count.to_string())
                    .child(div().flex_1())
                    .child(prune),
            )
            .children(broken.iter().map(|link| {
                mono(self.pretty(&link.path))
                    .px(px(LG))
                    .py(px(2.))
                    .truncate()
                    .text_color(muted())
            }))
    }
}

/// 作用范围标签：通用 / 指定 agent / 旧位置 / 冲突。
fn chip(skill: &Skill) -> Badge {
    if !skill.conflicts.is_empty() {
        return tag(t!("place.conflict", count = skill.conflicts.len() + 1)).bad();
    }
    match skill.placement {
        Placement::Agent(label) => tag(label),
        Placement::Legacy => tag(t!("place.legacy")).need(),
        Placement::Universal => tag(t!("place.universal")),
    }
}

/// 列表右侧的一句话：有几个 agent 能用，或者哪里不对。
fn summary(skill: &Skill) -> AnyElement {
    let missing: Vec<&str> = skill
        .coverage
        .iter()
        .filter(|row| row.access == Access::Missing)
        .map(|row| row.agent)
        .collect();
    if !skill.conflicts.is_empty() {
        return div()
            .flex_none()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::bad()))
            .child(t!("status.choose"))
            .into_any_element();
    }
    if skill.placement == Placement::Legacy {
        return tag(t!("status.unread")).need().into_any_element();
    }
    match missing.as_slice() {
        [] => div()
            .flex_none()
            .type_style(TYPE.text_xsmall)
            .text_color(muted())
            .child(match skill.coverage.len() {
                1 => t!("status.agents.one"),
                count => t!("status.agents.other", count = count),
            })
            .into_any_element(),
        [one] => tag(t!("status.missing_one", agent = one))
            .need()
            .into_any_element(),
        many => tag(t!("status.missing_many", count = many.len()))
            .need()
            .into_any_element(),
    }
}

/// 预览：算着、出错、无事可做，或要执行的步骤。`idle` 是没步骤时说的话。
fn plan_view(plan: &Option<Result<crate::Preview, String>>, idle: Option<String>) -> Div {
    match plan {
        None => hint(t!("plan.computing")),
        Some(Err(error)) => div()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::bad()))
            .child(error.clone()),
        Some(Ok(preview)) if preview.steps.is_empty() => hint(idle.unwrap_or_default()),
        Some(Ok(preview)) => div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(hint(t!("plan.title")))
            .children(preview.steps.iter().map(|step| mono(step.clone()))),
    }
}
