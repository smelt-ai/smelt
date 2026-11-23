use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ListAlignment, ListOffset,
    ListState, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, canvas,
    div, list as virtual_list, prelude::FluentBuilder as _, px, rgb, svg,
};
use smelt_plugin_api::ui::{
    Button,
    kit::{TYPE, TypeStyled as _},
    truncated_line::truncated_line,
    ui_theme,
};
use smelt_plugin_api::{self as api, HostEvent, PluginView};

use crate::model::{self, Activity, Room, Worker};
use crate::{VIEW, actors, layout, motion, scene};

#[derive(Clone, Copy)]
struct Timing {
    now: Instant,
    tick: u64,
    animate: bool,
    schedule: bool,
}

pub(crate) struct Office {
    #[cfg(test)]
    built_workers: std::cell::Cell<usize>,
    rooms: Vec<Room>,
    cast: actors::Cast,
    assets: Rc<scene::Assets>,
    list: ListState,
    rows: Vec<layout::Row>,
    columns: usize,
    rem_size: Option<gpui::Pixels>,
    #[cfg(test)]
    preview_time: Option<Instant>,
    notices: Vec<String>,
    visible: bool,
    error: Option<String>,
    epoch: Instant,
    pacer: Rc<motion::Pacer>,
    _events: Vec<Subscription>,
}

impl Office {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|office, cx| office.assets.release(cx))
            .detach();
        let events = cx.subscribe(
            &api::events(cx),
            |office: &mut Self, _, event, cx| match event {
                HostEvent::SessionsChanged | HostEvent::MachinesChanged => {
                    office.refresh(cx);
                }
                HostEvent::LocaleChanged => {
                    office.refresh(cx);
                    office.list.remeasure();
                    cx.notify();
                }
                HostEvent::ShownViewsChanged => {
                    office.visible = api::host(cx).view_shown(VIEW, cx);
                    if !office.visible {
                        office.pacer.begin(0);
                    }
                    cx.notify();
                }
                HostEvent::WindowActivated => cx.notify(),
                _ => {}
            },
        );
        let mut office = Self::empty(
            cx.background_executor().now(),
            api::host(cx).view_shown(VIEW, cx),
        );
        office._events = vec![events];
        office.refresh(cx);
        office
    }

    fn empty(epoch: Instant, visible: bool) -> Self {
        Self {
            #[cfg(test)]
            built_workers: Default::default(),
            rooms: Vec::new(),
            cast: actors::Cast::default(),
            assets: Rc::new(scene::Assets::new()),
            list: ListState::new(0, ListAlignment::Top, px(0.)).with_uniform_item_height(px(240.)),
            rows: Vec::new(),
            columns: 1,
            rem_size: None,
            #[cfg(test)]
            preview_time: None,
            notices: Vec::new(),
            visible,
            error: None,
            epoch,
            pacer: Rc::default(),
            _events: Vec::new(),
        }
    }

    fn reflow(&mut self) {
        let rows = layout::rows(
            self.rooms.iter().map(|room| room.workers.len()),
            self.columns,
        );
        if self.rows != rows {
            let anchor = self.list.logical_scroll_top();
            let anchored_row = self.rows.get(anchor.item_ix);
            let item_ix = rows.iter().position(|row| match (anchored_row, row) {
                (Some(layout::Row::Header(old)), layout::Row::Header(new)) => old == new,
                (
                    Some(layout::Row::Workers {
                        room: old, workers, ..
                    }),
                    layout::Row::Workers {
                        room: new,
                        workers: next,
                        ..
                    },
                ) => old == new && next.contains(&workers.start),
                _ => false,
            });
            self.list.reset_with_uniform_height(rows.len(), px(240.));
            if !rows.is_empty() {
                self.list.scroll_to(ListOffset {
                    item_ix: item_ix.unwrap_or_else(|| anchor.item_ix.min(rows.len() - 1)),
                    ..anchor
                });
            }
            self.rows = rows;
        }
    }

    #[cfg(test)]
    fn preview(rooms: Vec<Room>, epoch: Instant, time: Option<Instant>) -> Self {
        let mut office = Self::empty(epoch, false);
        if time.is_some() {
            office.cast.sync(&rooms, epoch);
        }
        office.rooms = rooms;
        office.preview_time = time;
        office.reflow();
        office
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let host = api::host(cx);
        let mut rooms = Vec::new();
        let mut notices = Vec::new();
        for machine in host.machines(cx) {
            let Some(directory) = host.workspace_directory(&machine.key, cx) else {
                notices.push(match machine.unavailable {
                    Some(reason) => rust_i18n::t!(
                        "office.unavailable",
                        machine = machine.name,
                        reason = reason
                    )
                    .into_owned(),
                    None => rust_i18n::t!("office.syncing", machine = machine.name).into_owned(),
                });
                continue;
            };
            rooms.extend(model::rooms_for_machine(
                &machine,
                host.machine_online(&machine.key, cx),
                host.path_style(&machine.key, cx),
                &directory,
                &host.session_states(&machine.key, cx),
            ));
        }
        if self.rooms != rooms || self.notices != notices {
            self.cast.sync(&rooms, cx.background_executor().now());
            let headers_changed = self
                .rooms
                .iter()
                .map(|r| &r.unavailable)
                .ne(rooms.iter().map(|r| &r.unavailable));
            self.rooms = rooms;
            self.reflow();
            if headers_changed {
                self.list.remeasure();
            }
            self.notices = notices;
            cx.notify();
        }
    }

    fn activate(&mut self, machine: &api::RuntimeKey, conversation: &str, cx: &mut Context<Self>) {
        self.error = None;
        let task = api::OPEN_VIEW.run(&model::open_request(machine, conversation), cx);
        cx.spawn(async move |view, cx| {
            let error = match task.await {
                Ok(true) => return,
                Ok(false) => rust_i18n::t!("office.open_failed").into_owned(),
                Err(error) => error,
            };
            let _ = view.update(cx, |office, cx| {
                office.error = Some(error);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn header(&self, room: &Room, index: usize) -> AnyElement {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .type_style(TYPE.ui_medium)
                            .text_color(rgb(ui_theme::text()))
                            .child(truncated_line(if room.name.is_empty() {
                                rust_i18n::t!("office.ungrouped").into_owned()
                            } else {
                                room.name.clone()
                            })),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .type_style(TYPE.small)
                            .text_color(rgb(ui_theme::muted()))
                            .child(truncated_line(room.machine_name.clone())),
                    ),
            )
            .children(room.unavailable.as_ref().map(|reason| {
                div()
                    .type_style(TYPE.small)
                    .text_color(rgb(ui_theme::muted()))
                    .child(reason.clone())
            }))
            .pb_2()
            .when(index > 0, |view| view.pt_5())
            .into_any_element()
    }

    fn row(&self, index: usize, timing: Timing, cx: &Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        match row {
            layout::Row::Header(room) => self.header(&self.rooms[*room], *room),
            layout::Row::Workers {
                room,
                workers,
                first,
                last,
            } => {
                let room_index = *room;
                let room = &self.rooms[room_index];
                let first = *first;
                let last = *last;
                let columns = self.columns.min(room.workers.len());
                let light = ui_theme::is_light();
                let assets = self.assets.clone();
                div()
                    .relative()
                    .w_full()
                    .max_w(px(layout::room_width(columns)))
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                scene::floor(bounds, light, columns, first, last, &assets, window);
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .relative()
                            .px_3()
                            .pt(px(if first { 28. } else { 0. }))
                            .pb(px(if last { 16. } else { 8. }))
                            .flex()
                            .gap_2()
                            .children(workers.clone().map(|index| {
                                self.worker(room, &room.workers[index], room_index, timing, cx)
                            })),
                    )
                    .into_any_element()
            }
        }
    }

    fn worker(
        &self,
        room: &Room,
        worker: &Worker,
        index: usize,
        timing: Timing,
        cx: &Context<Self>,
    ) -> AnyElement {
        #[cfg(test)]
        self.built_workers.set(self.built_workers.get() + 1);
        let activity = worker.activity;
        let look = worker.look;
        let light = ui_theme::is_light();
        let assets = self.assets.clone();
        let pacer = self.pacer.clone();
        let view = cx.entity().downgrade();
        let epoch = self.epoch;
        let frame = self.cast.frame(
            &room.machine,
            &worker.conversation,
            activity,
            timing.now,
            timing.animate,
        );
        let animate = timing.schedule && frame.animating;
        let title = worker.title.clone();
        let label = activity.label();
        let tooltip = [
            Some(title.clone()),
            Some(worker.provider.clone()),
            Some(label.clone()),
            worker.detail.clone(),
            Some(room.root.clone()),
        ]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        let machine = room.machine.clone();
        let conversation = worker.conversation.clone();
        let color = if activity.needs_you() {
            if activity == Activity::Failed {
                ui_theme::bad()
            } else {
                ui_theme::need()
            }
        } else if activity.running() {
            ui_theme::run()
        } else {
            ui_theme::muted()
        };
        let accessible = format!("{title}, {}, {label}", worker.provider);
        let content = div()
            .w(px(layout::WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(
                canvas(
                    move |bounds, window, _| {
                        if animate {
                            pacer.painted(bounds, epoch, view, window);
                        }
                    },
                    move |bounds, _, window, _| {
                        scene::workstation(bounds, activity, look, frame, light, &assets, window);
                    },
                )
                .w(px(layout::WIDTH))
                .h(px(layout::HEIGHT)),
            )
            .child(
                div()
                    .w_full()
                    .type_style(TYPE.ui_medium)
                    .text_color(rgb(ui_theme::text()))
                    .child(truncated_line(title)),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .type_style(TYPE.small)
                    .text_color(rgb(color))
                    .children(worker.icon.as_ref().map(|icon| {
                        svg()
                            .path(icon.clone())
                            .size(px(12.))
                            .flex_shrink_0()
                            .text_color(rgb(color))
                    }))
                    .child(
                        div()
                            .min_w_0()
                            .child(truncated_line(format!("{} · {label}", worker.provider))),
                    ),
            );
        #[cfg(test)]
        let content = {
            let selector = format!("office-worker-{}", worker.conversation);
            content.debug_selector(move || selector)
        };
        Button::new(SharedString::from(format!(
            "office-{index}-{}",
            worker.conversation
        )))
        .quiet()
        .accessibility_label(accessible)
        .tooltip(tooltip)
        .child(content)
        .on_click(cx.listener(move |office, _, _, cx| office.activate(&machine, &conversation, cx)))
        .into_any_element()
    }
}

impl PluginView for Office {
    fn open(&mut self, _: &api::serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
        cx.notify();
    }
}

impl Render for Office {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        self.built_workers.set(0);
        if self.rem_size.replace(window.rem_size()) != Some(window.rem_size()) {
            self.list.remeasure();
        }
        let now = cx.background_executor().now();
        let moving = motion::enabled(self.visible, window, cx);
        let timing = Timing {
            now,
            tick: motion::tick(self.epoch, now),
            animate: moving,
            schedule: moving,
        };
        #[cfg(test)]
        let timing = self.preview_time.map_or(timing, |now| Timing {
            now,
            tick: motion::tick(self.epoch, now),
            animate: true,
            schedule: false,
        });
        self.pacer.begin(timing.tick);
        let view = cx.entity();
        let list = virtual_list(self.list.clone(), move |index, _, cx| {
            view.update(cx, |office, cx| office.row(index, timing, cx))
        })
        .w_full()
        .flex_1()
        .min_h_0();
        let view = cx.entity().downgrade();
        div()
            .id("pixel-office")
            .size_full()
            .min_w_0()
            .relative()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(ui_theme::bg()))
            .p_4()
            .flex()
            .flex_col()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let columns = layout::columns(f32::from(bounds.size.width) - 32.);
                        let _ = view.update(cx, |office, cx| {
                            if office.columns != columns {
                                office.columns = columns;
                                office.reflow();
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(self.error.as_ref().map(|error| {
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::bad()))
                    .child(error.clone())
            }))
            .children(self.notices.iter().map(|notice| {
                div()
                    .type_style(TYPE.ui)
                    .text_color(rgb(ui_theme::muted()))
                    .child(notice.clone())
            }))
            .when(self.rooms.is_empty() && self.notices.is_empty(), |view| {
                view.child(
                    div()
                        .py_8()
                        .type_style(TYPE.ui)
                        .text_color(rgb(ui_theme::muted()))
                        .child(rust_i18n::t!("office.empty").into_owned()),
                )
            })
            .child(list)
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "snapshots.rs"]
mod snapshots;

#[cfg(all(test, target_os = "macos"))]
#[path = "benchmarks.rs"]
mod benchmarks;

#[cfg(test)]
mod tests;
