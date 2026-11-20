//! `SmeltViewInstance`：把一个按实例建的视图（[`crate::ViewSpec::instances`]，如终端、对话）
//! 嵌进脚本自己的界面，是 [`crate::create_view`] 的 JS 映射。脚本按视图 id 和参数要一份，
//! 摆在哪、多大由脚本的样式定。
//!
//! 实例跟着元素走：同一位置（脚本的 `key` 或树里的位置）连着几帧都画它，就一直是同一份；
//! 不画了就放掉，换了视图 id 或参数就换一份新的。实例在绘制之后的事件里建，不在 render
//! 里建；视图还没登记（插件没装上、登记在后）先空着，等登记变了再建。

use std::sync::Arc;

use gpui::{
    App, Context, ElementId, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Subscription, Window, div,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow,
};
use smelt_ui::Alert;

use super::{finish_leaf, ops, string_method};
use crate::{HostEvent, ViewInstance};

const OWNER: &str = "SmeltViewInstance";

/// 要哪个视图、带什么参数。两者都一样才是同一份实例。
#[derive(Clone, Debug, PartialEq)]
struct Target {
    view: String,
    args: serde_json::Value,
}

/// 构造时给的视图 id。
#[derive(Clone)]
struct ViewId(String);

/// `id(name)`：实例的身份。不给时按树里的位置认，前面多一行、少一行就换一份新实例；
/// 列表里的实例（如会话行的头像）要给，换位置时还是同一份。
#[derive(Clone)]
struct Key(String);

/// `args(json)` 解析好的参数，脚本调用时就解析，写错当场报给脚本。
#[derive(Clone)]
struct Args(serde_json::Value);

struct ViewInstanceMaterializer;

impl ComponentMaterializer for ViewInstanceMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let ViewId(view) = request
            .payload()
            .downcast_ref::<ViewId>()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?;
        let args = ops::<Args>(&request)
            .last()
            .map_or(serde_json::Value::Null, |args| args.0.clone());
        let id = ops::<Key>(&request).last().map_or_else(
            || request.element_id().clone(),
            |Key(key)| ElementId::Name(key.clone().into()),
        );
        let embed = Embed {
            id,
            target: Target { view, args },
        };
        // 实例自己不开放样式，套一层放脚本给的布局样式。
        finish_leaf(OWNER, request, div().flex().child(embed))
    }
}

#[derive(IntoElement)]
struct Embed {
    id: ElementId,
    target: Target,
}

impl RenderOnce for Embed {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let target = self.target;
        let holder = window.use_keyed_state((self.id.clone(), "smelt-ui-view"), cx, {
            let target = target.clone();
            move |window, cx| Holder::new(target, window, cx)
        });
        // 脚本换了视图或参数：记下，换实例留到绘制之后。
        if holder.read(cx).target != target {
            holder.update(cx, |holder, cx| {
                holder.target = target;
                cx.defer_in(window, Holder::sync);
            });
        }
        let holder = holder.read(cx);
        match (&holder.built, &holder.problem) {
            (Some((_, instance)), _) => div().size_full().flex().child(instance.view()),
            (None, Some(problem)) => div()
                .size_full()
                .child(Alert::error(self.id, problem.clone())),
            (None, None) => div().size_full(),
        }
    }
}

/// 按元素留着的那份实例。元素不再画，它和实例一起放掉。
struct Holder {
    target: Target,
    /// 建好的实例和建它时的目标。
    built: Option<(Target, ViewInstance)>,
    /// 建不出来的原因，画在原地给写插件的人看。
    problem: Option<SharedString>,
    _registry: Option<Subscription>,
}

impl Holder {
    fn new(target: Target, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let registry = crate::try_events(cx).map(|events| {
            cx.subscribe_in(&events, window, |holder, _, event, window, cx| {
                if *event == HostEvent::ViewsChanged {
                    holder.sync(window, cx);
                }
            })
        });
        cx.defer_in(window, Self::sync);
        Self {
            target,
            built: None,
            problem: None,
            _registry: registry,
        }
    }

    /// 让实例对上眼下的目标：已经对上、视图还登记着就不动（别的插件再登记不打断它）；
    /// 不然放掉旧的，登记着就按目标新建一份。
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let registered = crate::is_registered(&self.target.view, cx);
        if registered
            && self
                .built
                .as_ref()
                .is_some_and(|(built, _)| *built == self.target)
        {
            return;
        }
        self.built = None;
        self.problem = None;
        if registered {
            match crate::create_view(&self.target.view, &self.target.args, window, cx) {
                Ok(instance) => self.built = Some((self.target.clone(), instance)),
                Err(error) => self.problem = Some(error.into()),
            }
        }
        cx.notify();
    }
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(ViewInstanceMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![ArgumentDescriptor::new("view", ArgumentSchema::String)],
                |args| match args {
                    [ComponentArgument::String(view)] if !view.trim().is_empty() => {
                        Ok(ComponentPayload::new(ViewId(view.clone())))
                    }
                    [ComponentArgument::String(_)] => {
                        Err(format!("{OWNER} view id must not be empty"))
                    }
                    _ => Err(format!("{OWNER}(view) expects one string")),
                },
            )])
            .with_methods(vec![
                string_method(
                    OWNER,
                    "id",
                    "The instance's stable identity. Without it the instance follows its place in \
                     the tree, so a row inserted above builds a new one; give it in lists.",
                    Key,
                ),
                MethodDescriptor::new(
                    "args",
                    vec![ArgumentDescriptor::new("json", ArgumentSchema::String)],
                    |args| match args {
                        [ComponentArgument::String(json)] => serde_json::from_str(json)
                            .map(|args| ComponentPayload::new(Args(args)))
                            .map_err(|error| format!("{OWNER}.args expects JSON text: {error}")),
                        _ => Err(format!("{OWNER}.args expects one string")),
                    },
                )
                .with_documentation(
                    "Arguments handed to the view, as JSON text (`JSON.stringify({...})`); \
                     the view's provider documents them. Changing them builds a new instance.",
                ),
            ])
            .with_documentation(
                "Embeds a fresh instance of a registered per-instance view, such as a terminal, \
                 by view id. The instance lives while this element keeps being rendered at the \
                 same place with the same view and arguments; size it with styles. Until the \
                 view is registered it stays empty.",
            ),
    )?;
    Ok(())
}
