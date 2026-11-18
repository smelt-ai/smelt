//! 命令面板：窗口上方的一张对话框，搜索框加按组列出的命令，行尾是快捷键。选一条就关掉
//! 面板、焦点回到打开前的地方，再跑那条命令。
//!
//! 搜索、键盘、分组都是 [`crate::combobox`] 的那张可搜索列表；开关、Esc、点遮罩关、焦点
//! 进出归对话框层（[`crate::dialog`]）。卡片顶边钉在窗口上方，结果变多变少时搜索框不跳。
//! 不记「最近用过」：要持久化，现在没有真实需要。

use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, AppContext as _, ParentElement as _, SharedString, Styled as _, Window, px};

use crate::combobox::{ComboboxGroup, SearchList};
use crate::dialog::{self, DialogId};

/// 面板宽。
const WIDTH: f32 = 560.;
/// 卡片顶边离窗口顶。
const TOP: f32 = 80.;
/// 列表最高，多了滚动。
const LIST_MAX_HEIGHT: f32 = 360.;

type Run = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type Close = Rc<dyn Fn(&mut Window, &mut App)>;

/// ```ignore
/// let id = CommandPalette::new(groups, |command, window, cx| run(command, window, cx))
///     .on_close(|_, cx| forget_palette(cx))
///     .open(window, cx);
/// ```
pub struct CommandPalette {
    groups: Rc<Vec<ComboboxGroup>>,
    placeholder: SharedString,
    on_run: Run,
    on_close: Option<Close>,
}

impl CommandPalette {
    /// 选项的 `id` 是命令 id，`keybind` 画在行尾。选了一条时先关面板，再调 `on_run`。
    pub fn new(
        groups: Rc<Vec<ComboboxGroup>>,
        on_run: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            groups,
            placeholder: t!("palette.placeholder").into(),
            on_run: Rc::new(on_run),
            on_close: None,
        }
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// 不管怎么关（选了命令、Esc、点遮罩、[`DialogId::close`]）都跑，推到这一轮事件之后。
    pub fn on_close(mut self, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(on_close));
        self
    }

    /// 打开，焦点交给搜索框。窗口根上没有对话框层时返回 `None`。
    pub fn open(self, window: &mut Window, cx: &mut App) -> Option<DialogId> {
        let Self {
            groups,
            placeholder,
            on_run,
            on_close,
        } = self;
        let opened: Rc<Cell<Option<DialogId>>> = Rc::default();
        let list = cx.new(|cx| {
            let mut list = SearchList::new(placeholder, px(LIST_MAX_HEIGHT), window, cx);
            list.set_groups(groups, None, cx);
            list.set_on_choose({
                let opened = opened.clone();
                move |command, window, cx| {
                    if let Some(id) = opened.get() {
                        id.close(window, cx);
                    }
                    on_run(command, window, cx);
                }
            });
            list
        });
        let id = dialog::open(window, cx, {
            let list = list.clone();
            move |card, _, _| {
                let card = card
                    .top(px(TOP))
                    .w(px(WIDTH))
                    .p_0()
                    .gap_0()
                    .child(list.clone());
                match on_close.clone() {
                    Some(on_close) => card.on_close(move |window, cx| on_close(window, cx)),
                    None => card,
                }
            }
        })?;
        opened.set(Some(id));
        let focus = list.read(cx).query_focus(cx);
        focus.focus(window, cx);
        Some(id)
    }
}
