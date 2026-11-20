//! 菜单（[`smelt_ui::menu`]）：
//!
//! - 菜单项是描述，不进界面：`SmeltMenuItem`（一条命令，可带图标、打勾、危险色、禁用；
//!   `items([...])` 给了就是子菜单）、`SmeltMenuSeparator`、`SmeltMenuLabel`（小标题）。
//! - 带出菜单的：`SmeltMenuButton`（按钮点开的下拉菜单，按钮的变体、尺寸和状态同
//!   `SmeltButton`，`anchor(角)` 定菜单贴着按钮的哪个角）、`SmeltContextMenu`（子元素上右键点开）。两个都用 `items([...])` 收菜单项。
//!
//! 菜单打开时才搭，搭好就留在弹层里，比画出它的那次绘制活得久；脚本回调却只在那次绘制里
//! 有效。所以每次绘制把整份菜单项写进按控件 id 的槽（[`latest`]），点下去时按「各层名字」
//! 回槽里找这次绘制的回调；同一层里同名的按先后分（[`Step`]）。

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, ElementId, Entity, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StyleRefinement, Styled, Window, div,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, MethodDescriptor, RegistryError, anyhow,
};
use smelt_ui::button::Button;
use smelt_ui::menu::{Menu, MenuItem, MenuTrigger};

use super::carrier::{Carrier, take};
use super::{
    button, callback_method, control_id, corner_method, disabled_method, flag_method,
    id_constructor, latest, ops, string_method,
};

/// 脚本给的点击回调；只在描述它的那次绘制里有效。
type ClickHandler = Rc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>;

const ITEM: &str = "SmeltMenuItem";
const SEPARATOR: &str = "SmeltMenuSeparator";
const LABEL: &str = "SmeltMenuLabel";
const BUTTON: &str = "SmeltMenuButton";
const CONTEXT: &str = "SmeltContextMenu";

/// 菜单里的一条，脚本描述完就定了。
enum Entry {
    Item(Item),
    Label(SharedString),
    Separator,
}

struct Item {
    label: SharedString,
    icon: Option<SharedString>,
    icon_color: Option<u32>,
    image: Option<SharedString>,
    checked: Option<bool>,
    disabled: bool,
    danger: bool,
    on_click: Option<ClickHandler>,
    submenu: Vec<Entry>,
}

#[derive(Clone)]
enum ItemOp {
    Icon(String),
    IconColor(u32),
    Image(String),
    Checked(bool),
    Danger,
}

/// `items([...])` 收下的菜单项，物化时再取。
#[derive(Clone)]
struct Items(Vec<ComponentArgument>);

#[derive(Clone)]
struct Label(String);

/// 物化 `items([...])` 收下的菜单项。
fn entries(owner: &str, request: &mut MaterializeRequest<'_>) -> anyhow::Result<Vec<Entry>> {
    let arguments = ops::<Items>(request)
        .flat_map(|items| items.0.iter().cloned())
        .collect::<Vec<_>>();
    let mut entries = Vec::with_capacity(arguments.len());
    for argument in &arguments {
        let mut element = request.resolve_element(argument)?;
        let entry = take::<Entry>(
            &mut element,
            &format!("{owner}.items to hold SmeltMenuItem, SmeltMenuSeparator or SmeltMenuLabel"),
        )?;
        entries.push(entry);
    }
    Ok(entries)
}

/// 菜单里的一项在它那一层的位置：名字，和这一层里排在它前面的同名项有几个。按名字认，菜单开着时
/// 脚本重画、前面多了少了别的项，点下去找回的还是同一项；同名的（两台机器上同名的项目）按先后分。
type Step = (SharedString, usize);

/// 按各层的位置找到那一项。
fn find<'a>(entries: &'a [Entry], path: &[Step]) -> Option<&'a Item> {
    let ((label, nth), rest) = path.split_first()?;
    let item = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Item(item) if item.label == *label => Some(item),
            _ => None,
        })
        .nth(*nth)?;
    if rest.is_empty() {
        Some(item)
    } else {
        find(&item.submenu, rest)
    }
}

/// 按槽里最近一次绘制的菜单项搭菜单。命令点下去时再回槽里按名字找回调。
fn build(entries: &[Entry], path: &[Step], slot: &Entity<Vec<Entry>>) -> Menu {
    let mut seen: Vec<&SharedString> = Vec::new();
    entries.iter().fold(Menu::new(), |menu, entry| match entry {
        Entry::Separator => menu.separator(),
        Entry::Label(label) => menu.label(label.clone()),
        Entry::Item(item) => {
            let nth = seen.iter().filter(|label| ***label == item.label).count();
            seen.push(&item.label);
            let path = [path, &[(item.label.clone(), nth)]].concat();
            let mut entry = MenuItem::new(item.label.clone()).disabled(item.disabled);
            if let Some(icon) = &item.icon {
                entry = entry.icon_path(icon.clone());
            }
            if let Some(color) = item.icon_color {
                entry = entry.icon_color(color);
            }
            if let Some(image) = &item.image {
                entry = entry.image(image.clone());
            }
            if let Some(checked) = item.checked {
                entry = entry.checked(checked);
            }
            if item.danger {
                entry = entry.danger();
            }
            if !item.submenu.is_empty() {
                return menu.item(entry.submenu(build(&item.submenu, &path, slot)));
            }
            let slot = slot.clone();
            menu.item(entry.on_click(move |event, window, cx| {
                let callback = find(slot.read(cx), &path).and_then(|item| item.on_click.clone());
                if let Some(callback) = callback {
                    callback(event, window, cx);
                }
            }))
        }
    })
}

/// 下拉菜单贴着按钮的哪个角。
struct MenuAnchor(gpui::Anchor);

#[derive(IntoElement)]
struct MenuButton {
    id: ElementId,
    button: Button,
    entries: Vec<Entry>,
    anchor: Option<gpui::Anchor>,
    disabled: bool,
    style: StyleRefinement,
}

impl RenderOnce for MenuButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let slot = latest(&self.id, self.entries, window, cx);
        let mut trigger = MenuTrigger::dropdown(self.id, self.button, move |_, cx| {
            build(slot.read(cx), &[], &slot)
        })
        .disabled(self.disabled);
        if let Some(anchor) = self.anchor {
            trigger = trigger.anchor(anchor);
        }
        *trigger.style() = self.style;
        trigger
    }
}

#[derive(IntoElement)]
struct ContextMenu {
    id: ElementId,
    trigger: gpui::AnyElement,
    entries: Vec<Entry>,
    disabled: bool,
    style: StyleRefinement,
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let slot = latest(&self.id, self.entries, window, cx);
        let mut trigger = MenuTrigger::context(self.id, self.trigger, move |_, cx| {
            build(slot.read(cx), &[], &slot)
        })
        .disabled(self.disabled);
        *trigger.style() = self.style;
        trigger
    }
}

struct ItemMaterializer;

impl ComponentMaterializer for ItemMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let label = request
            .payload()
            .downcast_ref::<Label>()
            .ok_or_else(|| anyhow::anyhow!("{ITEM} received an incompatible payload"))?
            .0
            .clone();
        anyhow::ensure!(
            request.children_len() == 0,
            "{ITEM} takes a submenu through items([...]), not children"
        );
        let mut item = Item {
            label: label.into(),
            icon: None,
            icon_color: None,
            image: None,
            checked: None,
            disabled: request.disabled(),
            danger: false,
            on_click: request.on_click().map(|callback| -> ClickHandler {
                Rc::new(move |event, window, cx| callback.invoke(event, window, cx))
            }),
            submenu: Vec::new(),
        };
        for op in ops::<ItemOp>(&request) {
            match op {
                ItemOp::Icon(path) => item.icon = Some(path.clone().into()),
                ItemOp::IconColor(color) => item.icon_color = Some(*color),
                ItemOp::Image(path) => item.image = Some(path.clone().into()),
                ItemOp::Checked(checked) => item.checked = Some(*checked),
                ItemOp::Danger => item.danger = true,
            }
        }
        item.submenu = entries(ITEM, &mut request)?;
        reject_style(ITEM, &mut request)?;
        Ok(Carrier::new(Entry::Item(item)).into_any_element())
    }
}

struct SeparatorMaterializer;

impl ComponentMaterializer for SeparatorMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        reject_style(SEPARATOR, &mut request)?;
        Ok(Carrier::new(Entry::Separator).into_any_element())
    }
}

struct LabelMaterializer;

impl ComponentMaterializer for LabelMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let label = request
            .payload()
            .downcast_ref::<Label>()
            .ok_or_else(|| anyhow::anyhow!("{LABEL} received an incompatible payload"))?
            .0
            .clone();
        reject_style(LABEL, &mut request)?;
        Ok(Carrier::new(Entry::Label(label.into())).into_any_element())
    }
}

/// 菜单项只是描述，样式、子元素都没处放，给了就报错，不悄悄丢掉。
fn reject_style(owner: &str, request: &mut MaterializeRequest<'_>) -> anyhow::Result<()> {
    anyhow::ensure!(
        request.children_len() == 0,
        "{owner} does not accept children"
    );
    anyhow::ensure!(
        request.take_style() == StyleRefinement::default(),
        "{owner} is a menu description and takes no styles"
    );
    Ok(())
}

struct ButtonMaterializer;

impl ComponentMaterializer for ButtonMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        anyhow::ensure!(
            request.children_len() == 0,
            "{BUTTON} takes its menu through items([...]), not children"
        );
        let id = control_id(BUTTON, &request)?;
        let button = button::build(BUTTON, &request)?;
        let entries = entries(BUTTON, &mut request)?;
        let anchor = ops::<MenuAnchor>(&request).last().map(|anchor| anchor.0);
        Ok(MenuButton {
            id: id.into(),
            button,
            entries,
            anchor,
            disabled: request.disabled(),
            style: request.take_style(),
        }
        .into_any_element())
    }
}

struct ContextMaterializer;

impl ComponentMaterializer for ContextMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let id = control_id(CONTEXT, &request)?;
        let entries = entries(CONTEXT, &mut request)?;
        let trigger = div().children(request.take_children()?).into_any_element();
        Ok(ContextMenu {
            id: id.into(),
            trigger,
            entries,
            disabled: request.disabled(),
            style: request.take_style(),
        }
        .into_any_element())
    }
}

fn items_method(owner: &'static str) -> MethodDescriptor {
    MethodDescriptor::new(
        "items",
        vec![ArgumentDescriptor::new(
            "items",
            ArgumentSchema::Array(Box::new(ArgumentSchema::Element)),
        )],
        move |args| match args {
            [ComponentArgument::Array(items)]
                if items
                    .iter()
                    .all(|item| matches!(item, ComponentArgument::Element(_))) =>
            {
                Ok(ComponentPayload::new(Items(items.clone())))
            }
            _ => Err(format!(
                "{owner}.items expects an array of SmeltMenuItem, SmeltMenuSeparator or SmeltMenuLabel"
            )),
        },
    )
    .with_documentation(
        "Appends menu entries in order: SmeltMenuItem, SmeltMenuSeparator, SmeltMenuLabel.",
    )
}

fn label_constructor(owner: &'static str) -> ConstructorDescriptor {
    ConstructorDescriptor::new(
        owner,
        vec![ArgumentDescriptor::new("label", ArgumentSchema::String)],
        move |args| match args {
            [ComponentArgument::String(label)] if !label.trim().is_empty() => {
                Ok(ComponentPayload::new(Label(label.clone())))
            }
            _ => Err(format!("{owner}.new(label) expects a non-empty string")),
        },
    )
}

fn item_methods() -> Vec<MethodDescriptor> {
    vec![
        string_method(
            ITEM,
            "icon",
            "A line icon by asset path, such as \"icons/pencil.svg\".",
            ItemOp::Icon,
        ),
        MethodDescriptor::new(
            "icon_color",
            vec![ArgumentDescriptor::new("color", ArgumentSchema::String)],
            |args| match args {
                [ComponentArgument::String(color)] => crate::parse_hex_color(color)
                    .map(|color| ComponentPayload::new(ItemOp::IconColor(color)))
                    .map_err(|error| format!("{ITEM}.icon_color: {error}")),
                _ => Err(format!("{ITEM}.icon_color(color) expects \"#rrggbb\"")),
            },
        )
        .with_documentation(
            "A status color of the line icon itself (such as a connection light), instead of the text color: \"#rgb\" or \"#rrggbb\", such as a theme() color.",
        ),
        // 不叫 image：gpui-shell 所有元素共用的原型上 image 是头像的插槽，脚本那层先把参数换成
        // 元素 id，传进来的路径就丢了。
        string_method(
            ITEM,
            "picture",
            "A full-color icon (such as an app's own icon) drawn instead of the line icon: an asset() path, \
             or the image path a context_menu_items() entry carries.",
            ItemOp::Image,
        ),
        MethodDescriptor::new(
            "checked",
            vec![ArgumentDescriptor::new("checked", ArgumentSchema::Boolean)],
            |args| match args {
                [ComponentArgument::Boolean(checked)] => {
                    Ok(ComponentPayload::new(ItemOp::Checked(*checked)))
                }
                _ => Err(format!("{ITEM}.checked expects one boolean")),
            },
        )
        .with_documentation(
            "Shows a check mark slot; the script owns the state, the menu does not toggle it.",
        ),
        flag_method(
            "danger",
            ItemOp::Danger,
            "Uses the danger color for delete or stop entries; confirming is up to the callback.",
        ),
        disabled_method(ITEM),
        callback_method(
            ITEM,
            "on_click",
            "(event: ClickEvent, cx: Context) => void",
            "Invokes the callback when the item is chosen.",
            |_| (),
        ),
        items_method(ITEM),
    ]
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(BUTTON, Arc::new(ButtonMaterializer))
            .with_constructors(vec![id_constructor(BUTTON)])
            .with_methods(
                [
                    button::methods(BUTTON),
                    vec![
                        items_method(BUTTON),
                        corner_method(
                            BUTTON,
                            "anchor",
                            "Which corner of the menu is pinned to the button; default top_left (opens below, left-aligned). Use top_right for buttons at the right edge.",
                            MenuAnchor,
                        ),
                    ],
                ]
                .concat(),
            )
            .with_documentation(
                "A Smelt button that opens a dropdown menu of items([...]); styles apply to the trigger.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new(CONTEXT, Arc::new(ContextMaterializer))
            .with_constructors(vec![id_constructor(CONTEXT)])
            .with_methods(vec![items_method(CONTEXT), disabled_method(CONTEXT)])
            .with_documentation(
                "Wraps its children and opens a menu of items([...]) on right click; styles apply to the wrapper.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new(ITEM, Arc::new(ItemMaterializer))
            .with_constructors(vec![label_constructor(ITEM)])
            .with_methods(item_methods())
            .with_documentation(
                "A menu command, or a submenu when it has items([...]); only valid inside a menu's items.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new(SEPARATOR, Arc::new(SeparatorMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                SEPARATOR,
                Vec::new(),
                |_| Ok(ComponentPayload::new(())),
            )])
            .with_documentation(
                "A divider between menu groups; leading and repeated ones are dropped.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new(LABEL, Arc::new(LabelMaterializer))
            .with_constructors(vec![label_constructor(LABEL)])
            .with_documentation("A small heading above a group of menu items."),
    )?;
    Ok(())
}
