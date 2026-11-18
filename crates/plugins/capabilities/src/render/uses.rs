//! 技能、扩展、MCP 每行的「给智能体用」：点开列出这台机器上的智能体，勾上就让它用这一项。
//! 勾选经智能体插件改它的配置（[`crate::agents::set_use`]），一次只改一处，回来前别的勾选
//! 先置灰。

use super::*;
use crate::agents::{Usable, settings_on};
use api::ui::Checkbox;
use api::ui::popover::Popover;
use smelt_plugin_api::ui::kit::TYPE;

const POPOVER_WIDTH: f32 = 240.;

/// 「给智能体用」按钮和它的弹层。智能体插件没装（没有配置可改）时不画。
pub(super) fn uses_button(
    entity: &Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    usable: Usable,
    key: &str,
    cx: &App,
) -> Option<AnyElement> {
    settings_on(&page.machine, cx)?;
    let entity = entity.clone();
    let machine = page.machine.clone();
    let id = SharedString::from(format!("capabilities-uses-{key}"));
    Some(
        Popover::new(id.clone())
            .anchor(Anchor::TopRight)
            .w(px(POPOVER_WIDTH))
            .p_1()
            .trigger(
                Button::new(id)
                    .small()
                    .label(t!("uses.button"))
                    .icon(IconName::ChevronDown),
            )
            .content(move |_, _, cx| uses_card(&entity, &machine, &usable, cx))
            .into_any_element(),
    )
}

/// 弹层内容：开着时每帧搭一次，读的是缓存里最新的那份。
fn uses_card(
    entity: &Entity<CapabilitiesPage>,
    machine: &RuntimeKey,
    usable: &Usable,
    cx: &App,
) -> AnyElement {
    let muted = rgb(ui_theme::muted());
    let pending = entity.read(cx).agent_edit_pending;
    let agents = settings_on(machine, cx)
        .map(|settings| settings.agents.as_slice())
        .unwrap_or_default();
    let mut card = div().flex().flex_col().w_full().gap_0p5().child(
        div()
            .px_2()
            .pt_1()
            .pb_1()
            .type_style(TYPE.text_xsmall)
            .text_color(muted)
            .child(t!("uses.title").into_owned()),
    );
    if agents.is_empty() {
        return card
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .type_style(TYPE.text_xsmall)
                    .text_color(muted)
                    .child(t!("uses.no_agents").into_owned()),
            )
            .into_any_element();
    }
    for agent in agents {
        let on = usable.used_by(agent);
        let entity = entity.clone();
        let usable = usable.clone();
        let agent_id = agent.id.clone();
        card = card.child(
            div().px_2().py_1().child(
                Checkbox::new(SharedString::from(format!(
                    "capabilities-uses-{}",
                    agent.id
                )))
                .label(agent.name.clone())
                .checked(on)
                .disabled(pending)
                .on_change(move |checked, _, cx| {
                    let usable = usable.clone();
                    let agent_id = agent_id.clone();
                    entity.update(cx, |page, cx| {
                        page.set_agent_use(usable, &agent_id, checked, cx);
                    });
                }),
            ),
        );
    }
    card.into_any_element()
}

/// 「谁在用」那一句：没人用、或谁在用。智能体配置还没收到时 `None`，不写。
pub(super) fn used_by_line(machine: &RuntimeKey, usable: &Usable, cx: &App) -> Option<String> {
    settings_on(machine, cx)?;
    let names = usable.user_names(machine, cx);
    Some(if names.is_empty() {
        t!("plugins.unused").into_owned()
    } else {
        let separator = t!("plugins.user_separator");
        t!("plugins.used_by", names = names.join(separator.as_ref())).into_owned()
    })
}

impl CapabilitiesPage {
    /// 让那台机器上的一个智能体用 / 不用这一项。上一次还没回来时不发（见 [`crate::agents::set_use`]）。
    pub(crate) fn set_agent_use(
        &mut self,
        usable: Usable,
        agent_id: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if self.agent_edit_pending {
            return;
        }
        let Some(edit) = crate::agents::set_use(&self.machine, &usable, agent_id, on, cx) else {
            return;
        };
        self.agent_edit_pending = true;
        self.agent_edit_error = None;
        let epoch = self.epoch;
        cx.notify();
        cx.spawn(async move |page, cx| {
            let result = edit.await;
            page.update(cx, |page, cx| {
                // 换了机器：上一台的结果不落到这一台。
                if page.epoch != epoch {
                    return;
                }
                page.agent_edit_pending = false;
                page.agent_edit_error = result.err();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// 上一次「给智能体用」没改成：就地写在列表上面。
pub(super) fn agent_edit_error(page: &CapabilitiesPage) -> Option<Div> {
    page.agent_edit_error.as_ref().map(|error| {
        div()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::bad()))
            .child(t!("uses.failed", error = error).into_owned())
    })
}
