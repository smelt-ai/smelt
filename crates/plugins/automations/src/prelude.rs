//! 各模块共用的引用。页面拆在几个文件里，彼此的条目都经这里互相看见。

pub(crate) use chrono::{Local, TimeZone, Utc};
pub(crate) use gpui::prelude::FluentBuilder;
pub(crate) use gpui::*;
pub(crate) use gpui_component::button::{Toggle, ToggleGroup, ToggleVariants as _};
pub(crate) use gpui_component::input::{
    Input, InputEvent, InputState, NumberInput, Textarea, TextareaState,
};
pub(crate) use gpui_component::menu::{DropdownMenu, PopupMenuItem};
pub(crate) use gpui_component::tab::{Tab, TabBar};
pub(crate) use gpui_component::{
    ActiveTheme as _, FocusableExt as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _,
};
pub(crate) use smelt_plugin_api::{self as api, RuntimeKey};

pub(crate) use api::model::agent_definition::AgentDefinition;
pub(crate) use api::model::agent_kind::ConversationAgentKind;
pub(crate) use api::model::automation_projection::MachineAutomations;
pub(crate) use api::ui::truncated_line::truncated_line;
pub(crate) use api::ui::{Button, IconAction as _, ListRow, markdown_mermaid, ui_theme};

pub(crate) use crate::editor::*;
pub(crate) use crate::format::*;
pub(crate) use crate::kinds::*;
pub(crate) use crate::page::*;
pub(crate) use crate::projection::automations;
pub(crate) use crate::run_detail::{AutomationRunDetail, RunDetailKey};
pub(crate) use crate::widgets::*;
