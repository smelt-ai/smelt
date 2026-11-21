//! 活动上下文：用户眼下在看什么。显示会话的插件发布，别的插件读：
//! 文件、Git 这类按它决定看哪个项目，通知按它判断哪段会话已经看过。存在这里的只是最后
//! 发布的那一份，宿主不解读。

use gpui::{App, Global};

use crate::{HostEvent, Project, RuntimeKey};

/// 用户眼下在看的东西。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveContext {
    /// 当前项目，可能在远程机器上；没有项目是 `None`。会话视图换成别的页时还是原来那个。
    pub project: Option<Project>,
    /// 露着的会话。会话视图被挡住、收起，换成别的页，或者没有会话时是 `None`。
    pub session: Option<ViewedSession>,
}

/// 露着的一段会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewedSession {
    pub machine: RuntimeKey,
    /// 守护会话 id（终端是那一格），同 [`crate::Host::session_states`] 里的。
    pub id: String,
    /// 最新的内容在眼前。对话往上翻着看历史时是 `false`；终端总是 `true`。
    pub at_latest: bool,
}

#[derive(Default)]
struct ActiveContextSlot(ActiveContext);

impl Global for ActiveContextSlot {}

/// 发布活动上下文。和上次一样就什么都不做，变了发 [`HostEvent::ActiveContextChanged`]。
/// 只在事件里调，不在 render 里调。
pub fn set_active_context(context: ActiveContext, cx: &mut App) {
    // 先只读比较：可写地取全局（`default_global`）就算它变了，读过它的视图都要重画、观察者都
    // 收到通知，没变不能碰。
    if active_context(cx) == context {
        return;
    }
    cx.default_global::<ActiveContextSlot>().0 = context;
    crate::emit(HostEvent::ActiveContextChanged, cx);
}

/// 最后发布的活动上下文；还没人发布过是空的。只读内存，render 里可以调。
pub fn active_context(cx: &App) -> ActiveContext {
    cx.try_global::<ActiveContextSlot>()
        .map(|slot| slot.0.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn publishing_the_same_context_twice_changes_nothing(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(active_context(cx), ActiveContext::default());
            let context = ActiveContext {
                project: Some(Project {
                    machine: RuntimeKey::Remote("box".into()),
                    root: "/repo".into(),
                }),
                session: Some(ViewedSession {
                    machine: RuntimeKey::Remote("box".into()),
                    id: "s1".into(),
                    at_latest: true,
                }),
            };
            set_active_context(context.clone(), cx);
            assert_eq!(active_context(cx), context);
            set_active_context(ActiveContext::default(), cx);
            assert_eq!(active_context(cx), ActiveContext::default());
        });
    }

    /// 没变不碰全局：可写地取一下就算它变了，观察者和读过它的视图都会被惊动。
    #[gpui::test]
    fn publishing_the_same_context_leaves_the_global_untouched(cx: &mut gpui::TestAppContext) {
        let writes = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| {
            let writes = writes.clone();
            cx.observe_global::<ActiveContextSlot>(move |_| writes.set(writes.get() + 1))
                .detach();
        });
        cx.update(|cx| set_active_context(ActiveContext::default(), cx));
        assert_eq!(writes.get(), 0, "还没人发布过，发布空的不算变");
        let context = ActiveContext {
            project: None,
            session: Some(ViewedSession {
                machine: RuntimeKey::Local,
                id: "s1".into(),
                at_latest: true,
            }),
        };
        cx.update(|cx| set_active_context(context.clone(), cx));
        assert_eq!(writes.get(), 1);
        cx.update(|cx| set_active_context(context, cx));
        assert_eq!(writes.get(), 1, "和上次一样");
    }
}
