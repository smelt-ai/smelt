//! 浏览器页：看运行时上常驻 Chrome 的画面，点「接管」后能点、能打字（设计文档 D9）。
//!
//! 画面在收帧线程上解码成 `RenderImage`，渲染只读缓存。换上新图之后才确认这一帧，网络和
//! 界面哪边慢就按哪边的节奏出帧。默认只看：输入、导航、切标签都要先接管，运行时那边也
//! 按这个判断，这里的按钮只是给人看的。

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, IconName, Sizable, StyledExt as _, h_flex};
use smelt_runtime_client::browser::{BrowserConnection, BrowserEvent};
use smelt_runtime_client::protocol::browser::{
    BrowserCommand, BrowserInput, BrowserState, BrowserTab, KeyEventKind,
    MouseButton as WireButton, MouseEventKind, modifiers as wire_modifiers,
};

use crate::runtimes::Runtime;
use crate::ui_theme;

enum Status {
    Connecting,
    Live,
    Failed(String),
}

struct Screen {
    image: Arc<RenderImage>,
    /// 页面 CSS 视口的宽高：点击位置按它换算回页面坐标。
    css: (f32, f32),
}

enum Update {
    Frame(Screen),
    /// 这一帧解不出来，照样确认，不然运行时不推下一帧。
    Undecodable,
    State(BrowserState),
    Closed(Option<String>),
}

pub(crate) struct BrowserView {
    runtime: Runtime,
    connection: Option<Arc<BrowserConnection>>,
    status: Status,
    screen: Option<Screen>,
    /// 上一张图。新图至少画过一帧才从 atlas 拿掉，免得那一帧还在用它。
    retired: Option<Arc<RenderImage>>,
    state: BrowserState,
    address: Entity<InputState>,
    focus_handle: FocusHandle,
    /// 输入法正在合成的拼音，上屏之前不发给浏览器。
    marked_text: Option<String>,
    /// 画面区域在窗口里的位置，绘制时记下，换算鼠标坐标用。
    screen_bounds: Rc<Cell<Bounds<Pixels>>>,
    _address_sub: Subscription,
    _release: Subscription,
    _stream: Option<Task<()>>,
}

impl BrowserView {
    pub(crate) fn new(runtime: Runtime, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address =
            cx.new(|cx| InputState::new(window, cx).placeholder("网址（接管后回车打开）"));
        let address_sub = cx.subscribe_in(
            &address,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let url = normalize_address(&input.read(cx).value());
                    this.command(BrowserCommand::Navigate { url });
                }
            },
        );
        let release = cx.on_release(|this: &mut Self, cx| {
            for image in [
                this.screen.take().map(|screen| screen.image),
                this.retired.take(),
            ]
            .into_iter()
            .flatten()
            {
                cx.drop_image(image, None);
            }
        });
        let mut view = Self {
            runtime,
            connection: None,
            status: Status::Connecting,
            screen: None,
            retired: None,
            state: BrowserState::default(),
            address,
            focus_handle: cx.focus_handle(),
            marked_text: None,
            screen_bounds: Rc::new(Cell::new(Bounds::default())),
            _address_sub: address_sub,
            _release: release,
            _stream: None,
        };
        view.connect(window, cx);
        view
    }

    pub(crate) fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status = Status::Connecting;
        self.connection = None;
        let client = self.runtime.client().clone();
        let (sender, updates) = smol::channel::unbounded::<Update>();
        self._stream = Some(cx.spawn_in(window, async move |this, cx| {
            let attached = cx
                .background_executor()
                .spawn(async move {
                    BrowserConnection::attach(&client, move |event| {
                        let update = match event {
                            BrowserEvent::Frame {
                                css_width,
                                css_height,
                                jpeg,
                            } => match decode_frame(&jpeg) {
                                Some(image) => Update::Frame(Screen {
                                    image,
                                    css: (css_width as f32, css_height as f32),
                                }),
                                None => Update::Undecodable,
                            },
                            BrowserEvent::State(state) => Update::State(state),
                            BrowserEvent::Closed(error) => Update::Closed(error),
                        };
                        let _ = sender.send_blocking(update);
                    })
                })
                .await;
            let alive = this.update(cx, |view, cx| {
                match attached {
                    Ok(connection) => {
                        view.connection = Some(connection);
                        view.status = Status::Live;
                    }
                    Err(error) => view.status = Status::Failed(error),
                }
                cx.notify();
            });
            if alive.is_err() {
                return;
            }
            while let Ok(update) = updates.recv().await {
                if this
                    .update_in(cx, |view, window, cx| view.apply(update, window, cx))
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn apply(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        match update {
            Update::Frame(screen) => {
                if let Some(old) = self.retired.take() {
                    cx.drop_image(old, Some(window));
                }
                self.retired = self.screen.replace(screen).map(|screen| screen.image);
                self.ack();
            }
            Update::Undecodable => self.ack(),
            Update::State(state) => {
                let url = active_tab(&state).map(|tab| tab.url.clone());
                let editing = self.address.read(cx).focus_handle(cx).is_focused(window);
                if let Some(url) = url
                    && !editing
                    && self.address.read(cx).value() != url
                {
                    self.address
                        .update(cx, |input, cx| input.set_value(url, window, cx));
                }
                self.state = state;
            }
            Update::Closed(error) => {
                self.connection = None;
                self.status = Status::Failed(match error {
                    Some(error) => format!("和浏览器的连接断了：{error}"),
                    None => "和浏览器的连接断了".to_string(),
                });
            }
        }
        cx.notify();
    }

    fn ack(&self) {
        if let Some(connection) = &self.connection {
            let _ = connection.ack_frame();
        }
    }

    fn command(&self, command: BrowserCommand) {
        if let Some(connection) = &self.connection {
            let _ = connection.command(&command);
        }
    }

    /// 只在接管时发：运行时也会挡，这里先不浪费流量。
    fn input(&self, input: BrowserInput) {
        if self.state.controlling
            && let Some(connection) = &self.connection
        {
            let _ = connection.input(&input);
        }
    }

    /// 窗口坐标换成页面 CSS 坐标；落在画面外（留黑边的地方）返回 None。
    fn page_point(&self, position: Point<Pixels>) -> Option<(f64, f64)> {
        let screen = self.screen.as_ref()?;
        page_point(self.screen_bounds.get(), screen.css, position)
    }

    fn mouse(
        &self,
        event: MouseEventKind,
        position: Point<Pixels>,
        button: WireButton,
        click_count: usize,
        modifiers: &Modifiers,
    ) {
        let Some((x, y)) = self.page_point(position) else {
            return;
        };
        self.input(BrowserInput::Mouse {
            event,
            x,
            y,
            button,
            click_count: click_count as u32,
            delta_x: 0.0,
            delta_y: 0.0,
            modifiers: wire_mods(modifiers),
        });
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.controlling || !self.focus_handle.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        // 输入法合成中：退格、回车、数字归输入法，上屏的文字走 replace_text_in_range。
        if self.marked_text.is_some() && !mods.platform {
            cx.stop_propagation();
            return;
        }
        // Cmd+V 粘贴本机剪贴板：远程浏览器的剪贴板在另一台机器上。
        if mods.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.input(BrowserInput::Text { text });
            }
            cx.stop_propagation();
            return;
        }
        let Some((key, code, key_code, text)) = wire_key(&keystroke.key, mods) else {
            return;
        };
        let modifiers = wire_mods(mods);
        for kind in [KeyEventKind::Down, KeyEventKind::Up] {
            self.input(BrowserInput::Key {
                event: kind,
                key: key.clone(),
                code: code.clone(),
                key_code,
                text: if kind == KeyEventKind::Down {
                    text.clone()
                } else {
                    None
                },
                modifiers,
            });
        }
        cx.stop_propagation();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let controlling = self.state.controlling;
        let live = matches!(self.status, Status::Live);
        let nav_button =
            |id: &'static str, icon: IconName, label: &'static str, command: BrowserCommand| {
                Button::new(id)
                    .ghost()
                    .small()
                    .icon(icon)
                    .tooltip(label)
                    .disabled(!controlling)
                    .on_click(cx.listener(move |this, _, _, _| this.command(command.clone())))
            };
        h_flex()
            .w_full()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(ui_theme::hairline())
            .child(nav_button(
                "browser-back",
                IconName::ChevronLeft,
                "后退",
                BrowserCommand::Back,
            ))
            .child(nav_button(
                "browser-forward",
                IconName::ChevronRight,
                "前进",
                BrowserCommand::Forward,
            ))
            .child(nav_button(
                "browser-reload",
                IconName::Redo,
                "刷新",
                BrowserCommand::Reload,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.address).small().disabled(!controlling)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(if controlling {
                        ui_theme::yellow()
                    } else {
                        ui_theme::text_muted()
                    }))
                    .child(if controlling { "接管中" } else { "只看" }),
            )
            .child({
                let button = Button::new("browser-control")
                    .small()
                    .disabled(!live)
                    .label(if controlling { "交还" } else { "接管" })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.command(BrowserCommand::Control { on: !controlling });
                        if !controlling {
                            window.focus(&this.focus_handle, cx);
                        }
                    }));
                if controlling {
                    button.outline()
                } else {
                    button.primary()
                }
            })
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let controlling = self.state.controlling;
        let active = self.state.active.clone();
        h_flex()
            .w_full()
            .gap_1()
            .px_3()
            .py_1()
            .overflow_x_hidden()
            .children(self.state.tabs.iter().map(|tab| {
                let selected = active.as_deref() == Some(tab.id.as_str());
                let activate = tab.id.clone();
                let close = tab.id.clone();
                h_flex()
                    .id(ElementId::from(SharedString::from(format!(
                        "browser-tab-{}",
                        tab.id
                    ))))
                    .max_w(px(220.))
                    .min_w_0()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .when(selected, |d| d.bg(ui_theme::overlay(0x14)))
                    .text_color(rgb(if selected {
                        ui_theme::text_bright()
                    } else {
                        ui_theme::text_muted()
                    }))
                    .when(controlling && !selected, |d| {
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.command(BrowserCommand::ActivateTab {
                                    id: activate.clone(),
                                })
                            }))
                    })
                    .child(div().min_w_0().truncate().child(tab_label(tab)))
                    .when(controlling, |d| {
                        d.child(
                            Button::new(ElementId::from(SharedString::from(format!(
                                "browser-tab-close-{}",
                                tab.id
                            ))))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("关闭标签页")
                            .on_click(cx.listener(
                                move |this, _, _, _| {
                                    this.command(BrowserCommand::CloseTab { id: close.clone() })
                                },
                            )),
                        )
                    })
            }))
            .when(controlling, |d| {
                d.child(
                    Button::new("browser-new-tab")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("新标签页")
                        .on_click(
                            cx.listener(|this, _, _, _| this.command(BrowserCommand::NewTab)),
                        ),
                )
            })
    }
}

impl EntityInputHandler for BrowserView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.marked_text.as_ref()?;
        let units: Vec<u16> = text.encode_utf16().collect();
        let start = range_utf16.start.min(units.len());
        let end = range_utf16.end.clamp(start, units.len());
        if start != range_utf16.start || end != range_utf16.end {
            *adjusted = Some(start..end);
        }
        String::from_utf16(&units[start..end]).ok()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let len = self
            .marked_text
            .as_ref()
            .map(|text| text.encode_utf16().count())
            .unwrap_or(0);
        Some(UTF16Selection {
            range: len..len,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_text = None;
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.input(BrowserInput::Text {
                text: text.to_string(),
            });
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // 不知道网页里光标在哪：候选窗放在画面左下角附近，不挡住正中。
        Some(Bounds {
            origin: element_bounds.bottom_left() - point(px(0.), px(40.)),
            size: size(px(2.), px(20.)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controlling = self.state.controlling;
        let entity = cx.entity();
        let focus = self.focus_handle.clone();
        let bounds_cell = Rc::clone(&self.screen_bounds);
        let notice = match &self.status {
            Status::Connecting => Some("正在连接浏览器…".to_string()),
            Status::Failed(error) => Some(error.clone()),
            Status::Live if self.screen.is_none() => Some("等浏览器出第一帧画面…".to_string()),
            Status::Live => None,
        };
        let failed = matches!(self.status, Status::Failed(_));

        let mut screen = div()
            .id("browser-screen")
            .flex_1()
            .min_h_0()
            .relative()
            .bg(rgb(ui_theme::bg_stage()))
            .track_focus(&self.focus_handle)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.on_key(event, window, cx)
                }),
            )
            .on_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                window.focus(&this.focus_handle, cx);
                this.mouse(
                    MouseEventKind::Pressed,
                    event.position,
                    wire_button(event.button),
                    event.click_count,
                    &event.modifiers,
                );
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                this.mouse(
                    MouseEventKind::Moved,
                    event.position,
                    event.pressed_button.map(wire_button).unwrap_or_default(),
                    0,
                    &event.modifiers,
                );
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, _| {
                let Some((x, y)) = this.page_point(event.position) else {
                    return;
                };
                let delta = event.delta.pixel_delta(window.line_height());
                // gpui 往上滑是正，网页滚轮往下滚是正。
                this.input(BrowserInput::Mouse {
                    event: MouseEventKind::Wheel,
                    x,
                    y,
                    button: WireButton::None,
                    click_count: 0,
                    delta_x: -f64::from(f32::from(delta.x)),
                    delta_y: -f64::from(f32::from(delta.y)),
                    modifiers: wire_mods(&event.modifiers),
                });
            }))
            .when(controlling, |d| d.cursor(CursorStyle::Arrow))
            .child(
                canvas(
                    move |bounds, _, _| bounds_cell.set(bounds),
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
                    },
                )
                .absolute()
                .inset_0(),
            );
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            screen = screen.on_mouse_up(
                button,
                cx.listener(move |this, event: &MouseUpEvent, _, _| {
                    this.mouse(
                        MouseEventKind::Released,
                        event.position,
                        wire_button(button),
                        event.click_count,
                        &event.modifiers,
                    );
                }),
            );
        }
        let screen = screen
            .when_some(self.screen.as_ref(), |d, screen| {
                d.child(
                    img(screen.image.clone())
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            })
            .when_some(notice, |d, notice| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_sm()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(notice)
                        .when(failed, |d| {
                            d.child(
                                Button::new("browser-reconnect")
                                    .small()
                                    .label("重新连接")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.connect(window, cx);
                                        cx.notify();
                                    })),
                            )
                        }),
                )
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .when(!self.state.tabs.is_empty(), |d| {
                d.child(self.render_tabs(cx))
            })
            .child(screen)
    }
}

impl crate::Workspace {
    /// 打开某台运行时的浏览器页。同一台已经开着就接着用，换一台就重连。
    pub(crate) fn open_browser(
        &mut self,
        runtime: Runtime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reuse = self
            .browser
            .as_ref()
            .is_some_and(|view| view.read(cx).runtime().key() == runtime.key());
        if !reuse {
            self.browser = Some(cx.new(|cx| BrowserView::new(runtime, window, cx)));
        }
        self.activate_workspace_tab(crate::WorkspaceRoute::Browser, window, cx);
    }

    pub(crate) fn render_browser_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(view) = self.browser.clone() else {
            return div().into_any_element();
        };
        let name = crate::runtimes::runtime_name(view.read(cx).runtime().key(), cx);
        let header = crate::agents::product_page_chrome(self.chrome_left_guard(window))
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_medium()
                    .text_color(rgb(ui_theme::text_bright()))
                    .child(format!("浏览器 · {name}")),
            );
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(ui_theme::bg_stage()))
            .child(header)
            .child(div().flex_1().min_h_0().child(view))
            .into_any_element()
    }
}

fn active_tab(state: &BrowserState) -> Option<&BrowserTab> {
    let active = state.active.as_deref()?;
    state.tabs.iter().find(|tab| tab.id == active)
}

fn tab_label(tab: &BrowserTab) -> String {
    if !tab.title.trim().is_empty() {
        return tab.title.clone();
    }
    if tab.url.is_empty() || tab.url == "about:blank" {
        return "新标签页".to_string();
    }
    tab.url.clone()
}

/// 地址栏里没写协议的按 https 补上，跟浏览器地址栏一样。
fn normalize_address(input: &str) -> String {
    let input = input.trim();
    if input.contains("://") || input == "about:blank" {
        input.to_string()
    } else {
        format!("https://{input}")
    }
}

/// JPEG 解成 gpui 要的 BGRA 纹理。在收帧线程上跑，不占界面线程。
fn decode_frame(jpeg: &[u8]) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
    let mut pixels = decoded.into_rgba8();
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

/// 画面按比例缩放、居中摆在 `bounds` 里（留黑边）。把窗口坐标换成页面 CSS 坐标；
/// 落在黑边上返回 None。
fn page_point(
    bounds: Bounds<Pixels>,
    css: (f32, f32),
    position: Point<Pixels>,
) -> Option<(f64, f64)> {
    let (css_w, css_h) = css;
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    if css_w <= 0.0 || css_h <= 0.0 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let scale = (w / css_w).min(h / css_h);
    let left = f32::from(bounds.origin.x) + (w - css_w * scale) / 2.0;
    let top = f32::from(bounds.origin.y) + (h - css_h * scale) / 2.0;
    let x = (f32::from(position.x) - left) / scale;
    let y = (f32::from(position.y) - top) / scale;
    ((0.0..=css_w).contains(&x) && (0.0..=css_h).contains(&y)).then_some((x as f64, y as f64))
}

fn wire_button(button: MouseButton) -> WireButton {
    match button {
        MouseButton::Left => WireButton::Left,
        MouseButton::Right => WireButton::Right,
        MouseButton::Middle => WireButton::Middle,
        _ => WireButton::None,
    }
}

/// 修饰键。Cmd 当 Ctrl 发：运行时上的浏览器在 Linux 上，快捷键认的是 Ctrl。
fn wire_mods(mods: &Modifiers) -> u32 {
    let mut bits = 0;
    if mods.alt {
        bits |= wire_modifiers::ALT;
    }
    if mods.control || mods.platform {
        bits |= wire_modifiers::CTRL;
    }
    if mods.shift {
        bits |= wire_modifiers::SHIFT;
    }
    bits
}

/// gpui 的按键名换成 DOM 的 key / code / keyCode 和按下时插入的文字。可打印的字符不在
/// 这里：它们走输入法接口作为文字发。带 Cmd/Ctrl 的字母和数字是快捷键，在这里发。
fn wire_key(key: &str, mods: &Modifiers) -> Option<(String, String, u32, Option<String>)> {
    let named = |name: &str, code: u32, text: Option<&str>| {
        Some((
            name.to_string(),
            name.to_string(),
            code,
            text.map(str::to_string),
        ))
    };
    match key {
        "enter" => named("Enter", 13, Some("\r")),
        "backspace" => named("Backspace", 8, None),
        "tab" => named("Tab", 9, None),
        "escape" => named("Escape", 27, None),
        "delete" => named("Delete", 46, None),
        "home" => named("Home", 36, None),
        "end" => named("End", 35, None),
        "pageup" => named("PageUp", 33, None),
        "pagedown" => named("PageDown", 34, None),
        "left" => named("ArrowLeft", 37, None),
        "up" => named("ArrowUp", 38, None),
        "right" => named("ArrowRight", 39, None),
        "down" => named("ArrowDown", 40, None),
        _ if (mods.platform || mods.control) && key.chars().count() == 1 => {
            let ch = key.chars().next()?.to_ascii_uppercase();
            let code = if ch.is_ascii_alphabetic() {
                format!("Key{ch}")
            } else if ch.is_ascii_digit() {
                format!("Digit{ch}")
            } else {
                return None;
            };
            Some((key.to_string(), code, ch as u32, None))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    // 不能 `use super::*`：那会带进 `gpui::*` 里的 `test` 宏，盖掉内置的 `#[test]`。
    use super::{normalize_address, page_point, wire_key, wire_modifiers, wire_mods};
    use gpui::{Bounds, Modifiers, Pixels, point, px, size};

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    /// 画面比区域宽：上下留黑边，点击按缩放比例换回页面坐标，点在黑边上不算。
    #[test]
    fn clicks_map_back_to_page_coordinates() {
        // 区域 800×800，页面 1600×900 → 缩放 0.5，画面高 450，上下各留 175。
        let area = bounds(100.0, 50.0, 800.0, 800.0);
        assert_eq!(
            page_point(area, (1600.0, 900.0), point(px(100.0), px(225.0))),
            Some((0.0, 0.0))
        );
        assert_eq!(
            page_point(area, (1600.0, 900.0), point(px(500.0), px(450.0))),
            Some((800.0, 450.0))
        );
        assert_eq!(
            page_point(area, (1600.0, 900.0), point(px(500.0), px(100.0))),
            None
        );
        assert_eq!(
            page_point(area, (0.0, 0.0), point(px(500.0), px(450.0))),
            None
        );
    }

    #[test]
    fn function_keys_and_shortcuts_map_to_dom_keys() {
        let none = Modifiers::default();
        assert_eq!(
            wire_key("enter", &none),
            Some(("Enter".into(), "Enter".into(), 13, Some("\r".into())))
        );
        assert_eq!(wire_key("left", &none).unwrap().2, 37);
        // 普通字母走输入法，不在这里发。
        assert_eq!(wire_key("a", &none), None);
        let cmd = Modifiers {
            platform: true,
            ..Default::default()
        };
        assert_eq!(
            wire_key("a", &cmd),
            Some(("a".into(), "KeyA".into(), 65, None))
        );
        assert_eq!(wire_mods(&cmd), wire_modifiers::CTRL);
    }

    mod view {
        use super::super::BrowserView;
        use crate::runtimes::Runtime;
        use gpui::{Modifiers, MouseButton, TestAppContext, px};
        use smelt_client::runtime::RuntimeKey;
        use smelt_runtime_client::RuntimeClient;
        use smelt_runtime_client::protocol::browser::{
            BrowserAttachReply, BrowserCommand, BrowserInput, BrowserState, BrowserTab,
            CLIENT_COMMAND, CLIENT_FRAME_ACK, CLIENT_INPUT, MouseEventKind, SERVER_FRAME,
            SERVER_STATE, encode_screen_frame, read_frame, write_frame, write_json_frame,
        };
        use smelt_runtime_client::protocol::write_json_line;
        use std::io::Read as _;
        use std::os::unix::net::UnixStream;
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};

        fn state(controlling: bool) -> BrowserState {
            BrowserState {
                tabs: vec![BrowserTab {
                    id: "p1".into(),
                    title: "首页".into(),
                    url: "https://example.com/".into(),
                }],
                active: Some("p1".into()),
                controlling,
            }
        }

        /// 一张真 JPEG（160×90），声明页面视口 1600×900：画面是缩小过的，点击要按视口换算。
        fn jpeg() -> Vec<u8> {
            let image = image::RgbImage::from_pixel(160, 90, image::Rgb([40, 120, 200]));
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new(&mut out)
                .encode_image(&image)
                .unwrap();
            out
        }

        /// 假运行时：回 ok、推状态和一帧画面；之后把客户端发来的帧原样交给测试，
        /// 收到接管命令就回一份新状态。
        fn fake_runtime() -> (Runtime, mpsc::Receiver<(u8, Vec<u8>)>) {
            let (client_side, mut server) = UnixStream::pair().unwrap();
            let slot = Arc::new(Mutex::new(Some(client_side)));
            let client = RuntimeClient::new(move || {
                slot.lock()
                    .unwrap()
                    .take()
                    .ok_or_else(|| std::io::Error::other("只连一次"))
            });
            let (received, inbox) = mpsc::channel();
            std::thread::spawn(move || {
                let mut byte = [0u8; 1];
                while server.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {}
                write_json_line(
                    &mut server,
                    &BrowserAttachReply {
                        ok: true,
                        error: None,
                    },
                )
                .unwrap();
                write_json_frame(&mut server, SERVER_STATE, &state(false)).unwrap();
                write_frame(
                    &mut server,
                    SERVER_FRAME,
                    &encode_screen_frame(1600, 900, &jpeg()),
                )
                .unwrap();
                while let Ok(Some((kind, payload))) = read_frame(&mut server) {
                    if kind == CLIENT_COMMAND
                        && let Ok(BrowserCommand::Control { on }) = serde_json::from_slice(&payload)
                    {
                        write_json_frame(&mut server, SERVER_STATE, &state(on)).unwrap();
                    }
                    if received.send((kind, payload)).is_err() {
                        return;
                    }
                }
            });
            (Runtime::new(RuntimeKey::Remote("rt".into()), client), inbox)
        }

        /// 等到 `done` 成立：收帧线程是真线程，得让出真时间给它。
        fn wait(
            cx: &mut gpui::VisualTestContext,
            mut done: impl FnMut(&mut gpui::VisualTestContext) -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
                assert!(Instant::now() < deadline, "等太久了");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn inputs(inbox: &mpsc::Receiver<(u8, Vec<u8>)>, kinds: &mut Vec<u8>) -> Vec<BrowserInput> {
            let mut out = Vec::new();
            while let Ok((kind, payload)) = inbox.recv_timeout(Duration::from_millis(200)) {
                kinds.push(kind);
                if kind == CLIENT_INPUT {
                    out.push(serde_json::from_slice(&payload).unwrap());
                }
            }
            out
        }

        /// 画面能解出来画上；没接管时点了不发，接管后点击换算成页面坐标发给运行时。
        #[gpui::test]
        fn clicks_reach_the_page_only_after_taking_control(cx: &mut TestAppContext) {
            // 收帧走真套接字和真线程，得让调度器接受外部线程的唤醒。
            cx.executor().allow_parking();
            cx.update(gpui_component::init);
            let (runtime, inbox) = fake_runtime();
            let (view, cx) = cx.add_window_view(|window, cx| BrowserView::new(runtime, window, cx));
            wait(cx, |cx| view.read_with(cx, |view, _| view.screen.is_some()));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = view.read_with(cx, |view, _| view.screen_bounds.get());
            assert!(bounds.size.width > px(100.), "{bounds:?}");
            let center = bounds.center();

            let mut kinds = Vec::new();
            cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::default());
            assert!(inputs(&inbox, &mut kinds).is_empty(), "没接管时不能发输入");
            assert_eq!(
                kinds.first(),
                Some(&CLIENT_FRAME_ACK),
                "画上之后要确认这一帧"
            );

            view.update(cx, |view, _| {
                view.command(BrowserCommand::Control { on: true })
            });
            wait(cx, |cx| {
                view.read_with(cx, |view, _| view.state.controlling)
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::default());
            let sent = inputs(&inbox, &mut kinds);
            let clicks: Vec<_> = sent
                .iter()
                .filter_map(|input| match input {
                    BrowserInput::Mouse { event, x, y, .. }
                        if matches!(event, MouseEventKind::Pressed | MouseEventKind::Released) =>
                    {
                        Some((*event, x.round(), y.round()))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                clicks,
                vec![
                    (MouseEventKind::Pressed, 800.0, 450.0),
                    (MouseEventKind::Released, 800.0, 450.0)
                ],
                "{sent:?}"
            );
        }
    }

    #[test]
    fn bare_addresses_get_https() {
        assert_eq!(normalize_address("example.com"), "https://example.com");
        assert_eq!(normalize_address(" http://a.b/c "), "http://a.b/c");
        assert_eq!(normalize_address("about:blank"), "about:blank");
    }
}
