//! 命令面板（Cmd+K）：列出要进面板的命令，选一条就跑。
//!
//! 面板是 `smelt_ui::CommandPalette`，开在对话框层上：Esc、点遮罩、焦点进出都归那一层。
//! 选了命令时先关面板（焦点当场回到打开前的地方），再跑命令，命令自己挪的焦点不会被盖掉。
//! 这里只记开着的那张，好让 Cmd+K 再按一次关掉、让插件里的原生网页视图让开。

use std::cell::Cell;
use std::rc::Rc;

use gpui::Keystroke;
use smelt_plugin_api::CommandInfo;
use smelt_ui::dialog::DialogId;
use smelt_ui::{ComboboxGroup, ComboboxOption, CommandPalette};

use super::*;

impl Workspace {
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let groups = Rc::new(palette_groups(palette_commands(cx), cx));
        let workspace = cx.weak_entity();
        let opened: Rc<Cell<Option<DialogId>>> = Rc::default();
        let id = CommandPalette::new(groups, |command, window, cx| {
            let handled =
                smelt_plugin_api::invoke(command, &serde_json::Value::Null, Some(window), cx);
            crate::workspace_commands::finish(handled, cx);
        })
        .on_close({
            let opened = opened.clone();
            move |_, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    // 关掉后又开了一张时，这是旧那张的回调，不动新的。
                    if this.palette.is_some() && this.palette == opened.get() {
                        this.palette = None;
                        cx.notify();
                    }
                });
            }
        })
        .open(window, cx);
        opened.set(id);
        self.palette = id;
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.palette.take() else {
            return;
        };
        id.close(window, cx);
        cx.notify();
    }
}

/// 面板里列的命令：登记过、要进面板的。只读内存。
pub(crate) fn palette_commands(cx: &App) -> Vec<CommandInfo> {
    smelt_plugin_api::commands(cx)
        .into_iter()
        .filter(|command| command.palette)
        .collect()
}

/// 按来源分组，组按第一次出现的先后；行尾是现在生效的键（改过键的用改过的）。
fn palette_groups(commands: Vec<CommandInfo>, cx: &App) -> Vec<ComboboxGroup> {
    let keymap = cx.global::<crate::keymap::KeymapState>();
    let os = cx.platform_info().os;
    let mut groups: Vec<(SharedString, Vec<ComboboxOption>)> = Vec::new();
    for command in commands {
        let keybind = keymap
            .find(&command.id)
            .and_then(|bound| bound.key.as_deref())
            .and_then(|spec| Keystroke::parse_with_os(spec, os).ok());
        let group = command.group.get();
        let option = ComboboxOption::new(command.id.clone(), command.title.get())
            .keywords([group.clone(), command.id])
            .keybind(keybind);
        match groups.iter_mut().find(|(name, _)| *name == group) {
            Some((_, options)) => options.push(option),
            None => groups.push((group, vec![option])),
        }
    }
    groups
        .into_iter()
        .map(|(group, options)| ComboboxGroup::new(Some(group), options))
        .collect()
}
