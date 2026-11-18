//! 会话行首的小人头像：填会话列表的插槽 [`SESSION_AVATAR`]，每段会话一份实例。演什么、是谁
//! 都从 [`Cast`] 读。参数在第一次 `open` 时定下，之后不换会话。

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div, px,
};
use smelt_plugin_api::ui::motion::{ambient_application_active, ambient_motion_enabled};
use smelt_plugin_api::{self as api, PluginView, ViewHandle, ViewSpec};

use crate::buddy::{CANVAS, Portrait};
use crate::cast::{Cast, ConversationKey};
use crate::frames::Pacer;
use crate::peers::sessions::{SESSION_AVATAR, SessionAvatarArgs};

pub(crate) fn spec() -> ViewSpec {
    ViewSpec {
        name: "avatar".into(),
        title: smelt_plugin_api::text!("avatar"),
        icon: crate::ICON.into(),
        draws_top_bar: false,
        instances: true,
        settings: None,
        fills: vec![SESSION_AVATAR.into()],
        build: Rc::new(|_, cx| Rc::new(cx.new(Avatar::new)) as Rc<dyn ViewHandle>),
    }
}

struct Avatar {
    cast: Entity<Cast>,
    /// 第一次 `open` 给的会话；参数不对、还没 `open` 时是 `None`，什么都不画。
    key: Option<ConversationKey>,
    pacer: Rc<Pacer>,
    /// 正画着的那一帧。
    portrait: Portrait,
    _cast: Subscription,
    _release: Subscription,
}

impl Avatar {
    fn new(cx: &mut Context<Self>) -> Self {
        let cast = Cast::global(cx);
        // 换了小人、状态变了（新的一出）都在 `Cast` 上通知；演到一半的帧由 `pacer` 按节拍要。
        let _cast = cx.observe(&cast, |_, _, cx| cx.notify());
        let _release = cx.on_release(|this: &mut Self, cx| this.portrait.release(cx));
        Self {
            cast,
            key: None,
            pacer: Rc::default(),
            portrait: Portrait::default(),
            _cast,
            _release,
        }
    }
}

impl PluginView for Avatar {
    fn open(&mut self, args: &api::serde_json::Value, _: &mut Window, cx: &mut Context<Self>) {
        if self.key.is_some() {
            return;
        }
        match api::serde_json::from_value::<SessionAvatarArgs>(args.clone()) {
            Ok(args) => {
                self.key = Some(ConversationKey {
                    machine: args.machine,
                    id: args.conversation,
                });
                cx.notify();
            }
            Err(error) => eprintln!("[buddies] 头像的参数不对：{error}"),
        }
    }
}

impl Render for Avatar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let canvas = div().relative().size(px(CANVAS)).flex_shrink_0();
        let Some(key) = &self.key else {
            return canvas;
        };
        // 应用、窗口在后台，或系统要求减弱动态效果时定格，不要下一帧。
        let moving = ambient_motion_enabled(
            ambient_application_active(cx),
            window.is_window_active(),
            true,
        ) && !cx.reduce_motion();
        let cast = self.cast.read(cx);
        let clock = cast.clock();
        // 按所在的格取样，同一格里画面不变（`frames`）。
        let tick = clock.tick(cx.background_executor().now());
        let buddy = cast.buddy_of(key);
        let (figure, animating) = cast.stage().draw(
            key,
            buddy,
            clock.at(tick),
            moving,
            &mut self.portrait,
            window,
        );
        // 要不要下一帧放到 prepaint 里定：那时才知道这个小人露没露在可见区里。
        let pacer = self.pacer.clone();
        let view = cx.entity().downgrade();
        let pace = gpui::canvas(
            move |bounds, window, _| pacer.drew(tick, animating, bounds, clock, view, window),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        canvas.child(figure).child(pace)
    }
}
