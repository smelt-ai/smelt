//! 工作台布局：布局文件的格式、解析和校验。
//!
//! 纯数据，不碰窗口。桌面拿它搭外层 Dock，命令行拿它检查文件，两边用同一份校验。
//! 格式见 `docs/workspace-layouts.md`。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 现在认识的格式版本。
pub const FORMAT: u32 = 1;
/// 代码里的默认布局。文件不能用这个名字。
pub const DEFAULT_ID: &str = "default";
/// 布局目录名，用户的在 `~/.smelt/layouts/`。
pub const LAYOUT_DIR: &str = "layouts";

pub const SESSIONS: &str = "smelt.sessions";
pub const STAGE: &str = "smelt.stage";
pub const FILES: &str = "smelt.files";
pub const GIT: &str = "smelt.git";
pub const HISTORY: &str = "smelt.history";
pub const OTHER_PLUGINS: &str = "plugin:*";
const PLUGIN_PREFIX: &str = "plugin:";
const BUILTIN_PREFIX: &str = "smelt.";

/// 布局里能放的东西。内置组件，或者插件注册的一个面板。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Component {
    Sessions,
    Stage,
    Files,
    Git,
    History,
    Plugin(PluginPanelRef),
    /// `plugin:*`：不是组件，是个记号。布局里别处没写到的插件面板（不带 `props`），
    /// 按插件的先后插在这个位置。一套布局最多写一次，没写的话这些面板不出现。
    OtherPlugins,
}

/// `plugin:<插件 id>/<面板>`，可带 `props`。同一个面板换不同的 `props` 能摆多份。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PluginPanelRef {
    /// `gpui-shell.json` 里的 `id`。
    pub plugin: String,
    /// 插件在 `smelt.json` 里声明、用 `DockArea.register_panel` 注册的面板名。
    pub panel: String,
    pub props: Option<Props>,
}

impl PluginPanelRef {
    pub fn new(plugin: impl Into<String>, panel: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            panel: panel.into(),
            props: None,
        }
    }
}

/// 交给插件面板 `deserialize(data)` 的参数。存成键排好序的 JSON 文本：
/// 写法不同（键的先后、空白）但内容一样的两份算同一份，能拿来判重、当键。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Props(String);

impl Props {
    pub fn new(value: &Value) -> Self {
        Self(canonical(value).to_string())
    }

    pub fn value(&self) -> Value {
        serde_json::from_str(&self.0).expect("Props 里存的是自己序列化的 JSON")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 对象的键按字典序重排。`serde_json` 开了 `preserve_order`（别的依赖打开的），
/// 不重排的话键的先后会进到文本里。
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

impl Component {
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            SESSIONS => Ok(Self::Sessions),
            STAGE => Ok(Self::Stage),
            OTHER_PLUGINS => Ok(Self::OtherPlugins),
            FILES => Ok(Self::Files),
            GIT => Ok(Self::Git),
            HISTORY => Ok(Self::History),
            _ => {
                if let Some(rest) = name.strip_prefix(PLUGIN_PREFIX) {
                    return match rest.split_once('/') {
                        Some((plugin, panel))
                            if !plugin.is_empty() && !panel.is_empty() && !panel.contains('/') =>
                        {
                            Ok(Self::Plugin(PluginPanelRef::new(plugin, panel)))
                        }
                        _ => Err(format!(
                            "`{name}`：插件面板写成 `plugin:<插件 id>/<面板>`，面板名见插件的 `smelt.json`"
                        )),
                    };
                }
                if name.starts_with(BUILTIN_PREFIX) {
                    Err(format!(
                        "不认识的内置组件 `{name}`：写错了，或者是给更新版本的 smelt 写的"
                    ))
                } else {
                    Err(format!(
                        "不认识的组件 `{name}`：内置组件以 `smelt.` 开头，插件面板写成 `plugin:<插件 id>/<面板>`"
                    ))
                }
            }
        }
    }

    /// 布局文件里写的名字。插件面板不带 `props`。
    pub fn name(&self) -> String {
        match self {
            Self::Sessions => SESSIONS.into(),
            Self::Stage => STAGE.into(),
            Self::Files => FILES.into(),
            Self::Git => GIT.into(),
            Self::History => HISTORY.into(),
            Self::Plugin(panel) => format!("{PLUGIN_PREFIX}{}/{}", panel.plugin, panel.panel),
            Self::OtherPlugins => OTHER_PLUGINS.into(),
        }
    }

    /// 在一套布局里认出这个组件的键：名字，插件面板带 `props` 时接上 `?` 和 `props`。
    /// 存「点出来的状态」时拿它记选中的标签和拆分。
    pub fn key(&self) -> String {
        match self {
            Self::Plugin(PluginPanelRef {
                props: Some(props), ..
            }) => format!("{}?{}", self.name(), props.as_str()),
            _ => self.name(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// 横排，左右分。
    Row,
    /// 竖排，上下分。
    Column,
}

/// 树的节点，和 `DockLayout` 一一对应。
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Split {
        axis: Axis,
        /// 子节点和它沿拆分方向的像素大小；`None` 均分剩下的。
        children: Vec<(Node, Option<f32>)>,
    },
    Tabs {
        tabs: Vec<Tab>,
        active: usize,
    },
}

/// 标签组里的一个标签。
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub component: Component,
    /// 布局里写的标签名，只有插件面板能写。没写用插件声明的面板标题。
    pub title: Option<String>,
}

impl Tab {
    pub fn new(component: Component) -> Self {
        Self {
            component,
            title: None,
        }
    }
}

impl Node {
    fn components<'a>(&'a self, out: &mut Vec<&'a Component>) {
        match self {
            Self::Split { children, .. } => {
                for (child, _) in children {
                    child.components(out);
                }
            }
            Self::Tabs { tabs, .. } => out.extend(tabs.iter().map(|tab| &tab.component)),
        }
    }

    fn contains(&self, component: &Component) -> bool {
        let mut all = Vec::new();
        self.components(&mut all);
        all.contains(&component)
    }

    /// 贴着这块区域左上角、正在显示的组件：一路取第一个子节点，到标签组取选中的那个。
    fn top_left(&self) -> Option<&Component> {
        match self {
            Self::Split { children, .. } => children.first()?.0.top_left(),
            Self::Tabs { tabs, active } => tabs.get(*active).map(|tab| &tab.component),
        }
    }
}

/// 一个停靠区：一棵树，加上大小和开合的初值。
#[derive(Clone, Debug, PartialEq)]
pub struct DockSpec {
    pub size: Option<f32>,
    pub open: bool,
    pub node: Node,
    pub background: Option<Background>,
}

/// 一块区域的背景图：铺满区域、多出来的裁掉，压在区域里的面板底下。
#[derive(Clone, Debug, PartialEq)]
pub struct Background {
    /// 读布局文件时已经换成绝对路径（相对的是包的根目录，见 [`read_layout`]）。
    pub image: PathBuf,
    /// 图的不透明度，0 到 1；底下是区域原来的底色。
    pub opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Left,
    Center,
    Right,
    Bottom,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub name: String,
    pub description: Option<String>,
    pub copied_from: Option<String>,
    pub center: Node,
    pub center_background: Option<Background>,
    pub left: Option<DockSpec>,
    pub right: Option<DockSpec>,
    pub bottom: Option<DockSpec>,
    /// 布局内容的指纹。文件改了指纹就变，存下的「点出来的状态」随之作废。
    pub digest: String,
}

impl Layout {
    /// 代码里的默认布局：左边会话列表，中间舞台，右边文件、变更、历史和插件面板一组标签。
    pub fn default_layout() -> Self {
        let tabs = |components: Vec<Component>| Node::Tabs {
            tabs: components.into_iter().map(Tab::new).collect(),
            active: 0,
        };
        let dock = |size, components| DockSpec {
            size: Some(size),
            open: true,
            node: tabs(components),
            background: None,
        };
        let mut layout = Self {
            name: "默认".into(),
            description: Some("左边会话列表，中间舞台，右边文件、变更、历史和插件面板".into()),
            copied_from: None,
            center: tabs(vec![Component::Stage]),
            center_background: None,
            left: Some(dock(240., vec![Component::Sessions])),
            right: Some(dock(
                320.,
                vec![
                    Component::Files,
                    Component::Git,
                    Component::History,
                    Component::OtherPlugins,
                ],
            )),
            bottom: None,
            digest: String::new(),
        };
        // 没有文件，拿结构本身算指纹：改了默认布局，旧的状态自然作废。
        layout.digest = digest(&format!("{layout:?}"));
        layout
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let wire: WireLayout = serde_json::from_str(text).map_err(|error| error.to_string())?;
        wire.into_layout(digest(text))
    }

    /// 所有组件，按区域（左、中、右、下）再按树的先后。
    pub fn components(&self) -> Vec<&Component> {
        let mut out = Vec::new();
        for (_, node) in self.regions() {
            node.components(&mut out);
        }
        out
    }

    /// 布局里点名摆出来的插件面板，按出现的先后。不含 `plugin:*` 收的那些。
    pub fn plugin_panels(&self) -> Vec<&PluginPanelRef> {
        self.components()
            .into_iter()
            .filter_map(|component| match component {
                Component::Plugin(panel) => Some(panel),
                _ => None,
            })
            .collect()
    }

    /// 布局里有没有写到这个插件面板，带不带 `props` 都算。
    pub fn mentions_plugin_panel(&self, plugin: &str, panel: &str) -> bool {
        self.plugin_panels()
            .iter()
            .any(|placed| placed.plugin == plugin && placed.panel == panel)
    }

    /// 这个组件在哪个区域。
    pub fn region_of(&self, component: &Component) -> Option<Region> {
        self.regions()
            .find(|(_, node)| node.contains(component))
            .map(|(region, _)| region)
    }

    /// 中间区域贴着左上角、正在显示的组件。左停靠区收起或没有时，就是它挨着红黄绿按钮。
    pub fn center_top_left(&self) -> Option<&Component> {
        self.center.top_left()
    }

    pub fn dock(&self, region: Region) -> Option<&DockSpec> {
        match region {
            Region::Left => self.left.as_ref(),
            Region::Right => self.right.as_ref(),
            Region::Bottom => self.bottom.as_ref(),
            Region::Center => None,
        }
    }

    /// 这块区域布局里写的背景。
    pub fn background(&self, region: Region) -> Option<&Background> {
        match region {
            Region::Center => self.center_background.as_ref(),
            _ => self.dock(region)?.background.as_ref(),
        }
    }

    fn backgrounds_mut(&mut self) -> impl Iterator<Item = &mut Background> {
        [
            self.center_background.as_mut(),
            self.left.as_mut().and_then(|dock| dock.background.as_mut()),
            self.right
                .as_mut()
                .and_then(|dock| dock.background.as_mut()),
            self.bottom
                .as_mut()
                .and_then(|dock| dock.background.as_mut()),
        ]
        .into_iter()
        .flatten()
    }

    fn regions(&self) -> impl Iterator<Item = (Region, &Node)> {
        [
            self.left.as_ref().map(|dock| (Region::Left, &dock.node)),
            Some((Region::Center, &self.center)),
            self.right.as_ref().map(|dock| (Region::Right, &dock.node)),
            self.bottom
                .as_ref()
                .map(|dock| (Region::Bottom, &dock.node)),
        ]
        .into_iter()
        .flatten()
    }
}

// ---- 文件格式 ----
//
// 节点只用一个结构体收，再按字段组合判断是拆分还是标签组。用 serde 的 untagged
// 枚举时，写错一个字段只会报「哪个变体都对不上」，看不出错在哪。

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLayout {
    format: u32,
    /// 编辑器补全用的 JSON Schema 地址，宿主不看。
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    copied_from: Option<String>,
    center: WireNode,
    #[serde(default)]
    left: Option<WireNode>,
    #[serde(default)]
    right: Option<WireNode>,
    #[serde(default)]
    bottom: Option<WireNode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireNode {
    #[serde(default)]
    split: Option<WireAxis>,
    #[serde(default)]
    children: Option<Vec<WireNode>>,
    /// 每项是组件名字符串或 `{component, props, title}`，在 `into_tab` 里逐项认。
    #[serde(default)]
    tabs: Option<Vec<Value>>,
    #[serde(default)]
    active: Option<usize>,
    #[serde(default)]
    size: Option<f32>,
    #[serde(default)]
    open: Option<bool>,
    #[serde(default)]
    background: Option<WireBackground>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBackground {
    image: String,
    #[serde(default)]
    opacity: Option<f32>,
}

/// 标签的对象写法。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTab {
    component: String,
    #[serde(default)]
    props: Option<Value>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireAxis {
    Row,
    Column,
}

/// 节点在哪儿：决定 `size`、`open` 能不能写。
#[derive(Clone, Copy, PartialEq)]
enum Place {
    CenterRoot,
    DockRoot,
    SplitChild,
}

impl WireLayout {
    fn into_layout(self, digest: String) -> Result<Layout, String> {
        if self.format > FORMAT {
            return Err(format!(
                "这个布局比当前 smelt 新（format {}，这个版本只认 {FORMAT}）",
                self.format
            ));
        }
        if self.format != FORMAT {
            return Err(format!("不认识的 format {}", self.format));
        }
        if self.name.trim().is_empty() {
            return Err("name 不能为空".into());
        }
        let (center, _, center_background) = self.center.into_node("center", Place::CenterRoot)?;
        let dock = |node: Option<WireNode>, path: &str| -> Result<Option<DockSpec>, String> {
            let Some(node) = node else {
                return Ok(None);
            };
            let open = node.open.unwrap_or(true);
            let (node, size, background) = node.into_node(path, Place::DockRoot)?;
            Ok(Some(DockSpec {
                size,
                open,
                node,
                background,
            }))
        };
        let layout = Layout {
            name: self.name,
            description: self.description,
            copied_from: self.copied_from,
            left: dock(self.left, "left")?,
            center,
            center_background,
            right: dock(self.right, "right")?,
            bottom: dock(self.bottom, "bottom")?,
            digest,
        };

        let mut seen = HashSet::new();
        for component in layout.components() {
            if !seen.insert(component) {
                return Err(match component {
                    Component::Plugin(_) => format!(
                        "插件面板 `{}` 以同样的 `props` 出现了不止一次；要摆多份，给每份不同的 `props`",
                        component.name()
                    ),
                    Component::OtherPlugins => {
                        format!("`{OTHER_PLUGINS}` 出现了不止一次；其余插件面板只能收在一处")
                    }
                    _ => format!(
                        "组件 `{}` 出现了不止一次；内置组件在一套布局里只能放一处",
                        component.name()
                    ),
                });
            }
        }
        if !seen.contains(&Component::Stage) {
            return Err(format!(
                "布局里没有 `{STAGE}`：会话和各个页面都画在舞台里，不能没有它"
            ));
        }
        Ok(layout)
    }
}

impl WireNode {
    /// 返回节点、沿拆分方向的大小、区域背景（只有区域的根节点能写）。
    fn into_node(
        self,
        path: &str,
        place: Place,
    ) -> Result<(Node, Option<f32>, Option<Background>), String> {
        let at = |message: String| format!("{path}: {message}");
        if self.open.is_some() && place != Place::DockRoot {
            return Err(at("`open` 只能写在停靠区（left / right / bottom）上".into()));
        }
        let background = match self.background {
            Some(_) if place == Place::SplitChild => {
                return Err(at(
                    "`background` 只能写在区域上（center / left / right / bottom），拆分里的一块不能单独写".into(),
                ));
            }
            Some(wire) => Some(wire.into_background().map_err(at)?),
            None => None,
        };
        let size = match self.size {
            Some(_) if place == Place::CenterRoot => {
                return Err(at("center 占满剩下的地方，不能写 `size`".into()));
            }
            Some(size) if !size.is_finite() || size <= 0. => {
                return Err(at(format!("`size` 要是正数，现在是 {size}")));
            }
            size => size,
        };
        let node = match (self.split, self.children, self.tabs) {
            (Some(axis), Some(children), None) => {
                if self.active.is_some() {
                    return Err(at("`active` 只能写在标签组（tabs）上".into()));
                }
                if children.is_empty() {
                    return Err(at("`children` 是空的".into()));
                }
                let children = children
                    .into_iter()
                    .enumerate()
                    .map(|(ix, child)| {
                        child
                            .into_node(&format!("{path}/{ix}"), Place::SplitChild)
                            .map(|(node, size, _)| (node, size))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Node::Split {
                    axis: match axis {
                        WireAxis::Row => Axis::Row,
                        WireAxis::Column => Axis::Column,
                    },
                    children,
                }
            }
            (None, None, Some(tabs)) => {
                if tabs.is_empty() {
                    return Err(at("`tabs` 是空的".into()));
                }
                let active = self.active.unwrap_or(0);
                if active >= tabs.len() {
                    return Err(at(format!(
                        "`active` 是 {active}，可只有 {} 个标签",
                        tabs.len()
                    )));
                }
                let tabs = tabs
                    .into_iter()
                    .enumerate()
                    .map(|(ix, tab)| {
                        into_tab(tab).map_err(|message| at(format!("第 {ix} 个标签：{message}")))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Node::Tabs { tabs, active }
            }
            (Some(_), None, _) => return Err(at("`split` 要配 `children`".into())),
            (None, Some(_), _) => return Err(at("`children` 要配 `split`".into())),
            (Some(_), Some(_), Some(_)) => {
                return Err(at("一个节点不能既是拆分（split）又是标签组（tabs）".into()));
            }
            (None, None, None) => {
                return Err(at(
                    "节点要么是 `{\"split\", \"children\"}`，要么是 `{\"tabs\"}`".into(),
                ));
            }
        };
        Ok((node, size, background))
    }
}

impl WireBackground {
    fn into_background(self) -> Result<Background, String> {
        if self.image.trim().is_empty() {
            return Err("`background.image` 不能为空".into());
        }
        let opacity = self.opacity.unwrap_or(1.);
        if !(0. ..=1.).contains(&opacity) {
            return Err(format!(
                "`background.opacity` 要在 0 到 1 之间，现在是 {opacity}"
            ));
        }
        Ok(Background {
            image: PathBuf::from(self.image),
            opacity,
        })
    }
}

fn into_tab(value: Value) -> Result<Tab, String> {
    let wire = match value {
        Value::String(name) => return Component::parse(&name).map(Tab::new),
        object @ Value::Object(_) => {
            serde_json::from_value::<WireTab>(object).map_err(|error| error.to_string())?
        }
        _ => {
            return Err(
                "标签要么是组件名字符串，要么是 `{\"component\", \"props\", \"title\"}`".into(),
            );
        }
    };
    let mut component = Component::parse(&wire.component)?;
    let props = wire.props.filter(|props| !props.is_null());
    let title = match wire.title {
        Some(title) if title.trim().is_empty() => return Err("`title` 不能为空".into()),
        title => title,
    };
    match &mut component {
        Component::Plugin(panel) => panel.props = props.as_ref().map(Props::new),
        Component::OtherPlugins if props.is_some() || title.is_some() => {
            return Err(format!(
                "`{OTHER_PLUGINS}` 只是个位置记号，不能写 `props` 和 `title`"
            ));
        }
        builtin if props.is_some() || title.is_some() => {
            return Err(format!(
                "`{}` 是内置组件，不能写 `props` 和 `title`：只有插件面板能写",
                builtin.name()
            ));
        }
        _ => {}
    }
    Ok(Tab { component, title })
}

/// 布局内容的指纹：SHA-256 的十六进制。
fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

// ---- 点出来的状态 ----
//
// 布局文件只写初值。用户拖的宽度、开合、选中的标签按布局各存一份，格式是我们自己的，
// 带版本号；读到不认识的版本或内容不对，就当没存过，回到布局里写的初值。
//
// 拆分不按路径认：Dock 会把只剩一个子节点的拆分拆掉、把同方向的拆分拼起来，`plugin:*`
// 收进来的面板也随装了哪些插件变，路径靠不住。这里按「每个子节点最左上那个面板的键」
// 认一个拆分，对不上的就不套。放大不存：上游 Dock 放大不发布局变化，重启回到不放大。

/// 现在写的状态版本。
pub const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutState {
    pub v: u32,
    pub docks: DockStates,
    /// 拖过的拆分。
    pub splits: Vec<SplitState>,
    /// 各标签组里选中的组件，存 [`Component::key`]。
    pub active: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockStates {
    pub left: Option<DockState>,
    pub right: Option<DockState>,
    pub bottom: Option<DockState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockState {
    pub open: bool,
    pub size: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitState {
    /// 每个子节点最左上那个面板的 [`Component::key`]，拿来认出是哪个拆分。
    pub children: Vec<String>,
    /// 每个子节点沿拆分方向的像素大小，`None` 均分剩下的。和 `children` 一样长。
    pub sizes: Vec<Option<f32>>,
}

impl LayoutState {
    pub fn new(docks: DockStates, splits: Vec<SplitState>, active: Vec<String>) -> Self {
        Self {
            v: STATE_VERSION,
            docks,
            splits,
            active,
        }
    }

    /// 读存下的状态。版本不认识、内容不合法都返回 `None`，调用方回到布局的初值。
    pub fn parse(text: &str) -> Option<Self> {
        let state: Self = serde_json::from_str(text).ok()?;
        (state.v == STATE_VERSION && state.is_valid()).then_some(state)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("LayoutState 只含能序列化的字段")
    }

    pub fn dock(&self, region: Region) -> Option<DockState> {
        match region {
            Region::Left => self.docks.left,
            Region::Right => self.docks.right,
            Region::Bottom => self.docks.bottom,
            Region::Center => None,
        }
    }

    fn is_valid(&self) -> bool {
        let size_ok = |size: f32| size.is_finite() && size > 0.;
        [self.docks.left, self.docks.right, self.docks.bottom]
            .into_iter()
            .flatten()
            .all(|dock| size_ok(dock.size))
            && self.splits.iter().all(|split| {
                split.children.len() >= 2
                    && split.children.len() == split.sizes.len()
                    && split.sizes.iter().flatten().all(|size| size_ok(*size))
            })
    }
}

// ---- 布局目录 ----

/// 用户布局的目录：`~/.smelt/layouts/`。
pub fn user_layout_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join(LAYOUT_DIR))
}

/// 目录里的一个布局文件。不合法的也列出来，带上原因。
#[derive(Clone, Debug)]
pub struct LayoutEntry {
    /// 文件名去掉 `.json`。
    pub id: String,
    pub path: PathBuf,
    pub layout: Result<Layout, String>,
}

/// 读这个目录里的 `*.json`，结果按 id 排序。目录不存在就是没有布局。
/// 背景图的相对路径相对这个目录。
pub fn discover(dir: &Path) -> Vec<LayoutEntry> {
    discover_in(dir, dir)
}

/// `assets` 是背景图相对路径的起点。
fn discover_in(dir: &Path, assets: &Path) -> Vec<LayoutEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<LayoutEntry> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json") && path.is_file())
        .filter_map(|path| {
            let id = path.file_stem()?.to_str()?.to_string();
            let layout = if id == DEFAULT_ID {
                Err(format!("`{DEFAULT_ID}` 留给内置的默认布局，换个文件名"))
            } else {
                read_layout(&path, assets)
            };
            Some(LayoutEntry { id, path, layout })
        })
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries
}

/// 插件带的布局：插件目录下 `layouts/` 里的 `*.json`。id 是 `<插件 id>/<文件名>`，
/// 和用户布局（id 就是文件名，不会带 `/`）、别的插件的布局都撞不上。
/// 背景图的相对路径相对插件目录，和插件脚本里 `image()` 的路径一样。
pub fn discover_plugin(plugin: &str, plugin_root: &Path) -> Vec<LayoutEntry> {
    discover_in(&plugin_root.join(LAYOUT_DIR), plugin_root)
        .into_iter()
        .map(|entry| LayoutEntry {
            id: format!("{plugin}/{}", entry.id),
            ..entry
        })
        .collect()
}

/// 插件带的布局属于哪个插件；用户布局是 None。
pub fn layout_plugin(id: &str) -> Option<&str> {
    id.split_once('/').map(|(plugin, _)| plugin)
}

/// 读一个布局文件，把背景图的相对路径换成 `assets` 下的绝对路径。图不在就是布局有错：
/// 写错路径当场报出来，不等换上以后才发现底是空的。
pub fn read_layout(path: &Path, assets: &Path) -> Result<Layout, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("读不了：{error}"))?;
    let mut layout = Layout::parse(&text)?;
    for background in layout.backgrounds_mut() {
        let image = assets.join(&background.image);
        if !image.is_file() {
            return Err(format!("背景图 `{}` 不存在", image.display()));
        }
        background.image = image;
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Layout, String> {
        Layout::parse(text)
    }

    const KANBAN: &str = r#"{
      "format": 1,
      "name": "看板",
      "left":   { "size": 260, "tabs": ["smelt.sessions"] },
      "center": {
        "split": "row",
        "children": [
          { "size": 420, "tabs": ["plugin:com.smelt.kanban/board"] },
          { "tabs": ["smelt.stage"] }
        ]
      },
      "right":  { "size": 360, "open": false, "tabs": ["smelt.files", "smelt.git", "smelt.history", "plugin:*"] }
    }"#;

    #[test]
    fn the_design_sample_parses() {
        let layout = parse(KANBAN).unwrap();
        assert_eq!(layout.name, "看板");
        assert_eq!(
            layout.left,
            Some(DockSpec {
                size: Some(260.),
                open: true,
                node: Node::Tabs {
                    tabs: vec![Tab::new(Component::Sessions)],
                    active: 0,
                },
                background: None,
            })
        );
        assert!(!layout.right.as_ref().unwrap().open);
        let board = PluginPanelRef::new("com.smelt.kanban", "board");
        assert_eq!(layout.plugin_panels(), [&board]);
        assert!(layout.mentions_plugin_panel("com.smelt.kanban", "board"));
        assert!(!layout.mentions_plugin_panel("com.smelt.kanban", "other"));
        assert_eq!(layout.region_of(&Component::Stage), Some(Region::Center));
        assert_eq!(layout.region_of(&Component::Git), Some(Region::Right));
        assert_eq!(
            layout.region_of(&Component::OtherPlugins),
            Some(Region::Right)
        );
        assert_eq!(layout.center_top_left(), Some(&Component::Plugin(board)));
    }

    #[test]
    fn the_default_layout_puts_the_stage_top_left_of_the_center() {
        let layout = Layout::default_layout();
        assert_eq!(layout.center_top_left(), Some(&Component::Stage));
        assert_eq!(layout.region_of(&Component::Sessions), Some(Region::Left));
        for component in [
            Component::Files,
            Component::Git,
            Component::History,
            Component::OtherPlugins,
        ] {
            assert_eq!(layout.region_of(&component), Some(Region::Right));
        }
        assert!(layout.plugin_panels().is_empty());
        assert_eq!(layout.digest, Layout::default_layout().digest);
        assert_eq!(layout.digest.len(), 64);
    }

    #[test]
    fn everything_but_the_stage_is_optional() {
        let layout =
            parse(r#"{"format":1,"name":"专注","center":{"tabs":["smelt.stage"]}}"#).unwrap();
        assert_eq!(layout.region_of(&Component::Sessions), None);
        assert_eq!(layout.region_of(&Component::Files), None);
        assert_eq!(layout.region_of(&Component::OtherPlugins), None);
    }

    fn rejects(text: &str, needle: &str) {
        let error = parse(text).unwrap_err();
        assert!(error.contains(needle), "错误「{error}」里没有「{needle}」");
    }

    #[test]
    fn tool_pages_can_be_placed_on_their_own() {
        let layout = parse(
            r#"{"format":1,"name":"x",
                "center":{"split":"column","children":[
                  {"tabs":["smelt.stage"]},{"size":240,"tabs":["smelt.git","smelt.history"]}]},
                "left":{"tabs":["smelt.sessions","smelt.files"]},
                "right":{"tabs":["plugin:*"]}}"#,
        )
        .unwrap();
        assert_eq!(layout.region_of(&Component::Files), Some(Region::Left));
        assert_eq!(layout.region_of(&Component::Git), Some(Region::Center));
        assert_eq!(layout.region_of(&Component::History), Some(Region::Center));
        for component in [Component::Files, Component::Git, Component::History] {
            assert_eq!(Component::parse(&component.name()), Ok(component));
        }
    }

    #[test]
    fn other_plugins_is_a_marker_written_once() {
        let layout = parse(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage","plugin:k/board","plugin:*"]}}"#,
        )
        .unwrap();
        // 记号不算点名摆出来的插件面板。
        assert_eq!(layout.plugin_panels(), [&PluginPanelRef::new("k", "board")]);
        assert_eq!(Component::parse(OTHER_PLUGINS), Ok(Component::OtherPlugins));
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage","plugin:*"]},"right":{"tabs":["plugin:*"]}}"#,
            "`plugin:*` 出现了不止一次",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",{"component":"plugin:*","title":"插件"}]}}"#,
            "只是个位置记号",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",{"component":"plugin:*","props":{}}]}}"#,
            "只是个位置记号",
        );
    }

    #[test]
    fn keys_tell_props_apart() {
        let mut board = PluginPanelRef::new("k", "board");
        assert_eq!(Component::Plugin(board.clone()).key(), "plugin:k/board");
        board.props = Some(Props::new(&serde_json::json!({"b":1,"a":2})));
        assert_eq!(
            Component::Plugin(board).key(),
            r#"plugin:k/board?{"a":2,"b":1}"#
        );
        assert_eq!(Component::Git.key(), GIT);
    }

    #[test]
    fn the_digest_follows_the_file_text() {
        let a = r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage"]}}"#;
        let b = r#"{"format":1,"name":"y","center":{"tabs":["smelt.stage"]}}"#;
        assert_eq!(parse(a).unwrap().digest, parse(a).unwrap().digest);
        assert_ne!(parse(a).unwrap().digest, parse(b).unwrap().digest);
        assert_ne!(parse(a).unwrap().digest, Layout::default_layout().digest);
    }

    #[test]
    fn layout_state_round_trips() {
        let state = LayoutState::new(
            DockStates {
                left: Some(DockState {
                    open: false,
                    size: 260.,
                }),
                right: Some(DockState {
                    open: true,
                    size: 400.5,
                }),
                bottom: None,
            },
            vec![SplitState {
                children: vec![STAGE.into(), "plugin:k/board".into()],
                sizes: vec![Some(420.), None],
            }],
            vec![GIT.into()],
        );
        let text = state.to_json();
        assert_eq!(LayoutState::parse(&text), Some(state.clone()));
        assert_eq!(state.dock(Region::Left).map(|dock| dock.open), Some(false));
        assert_eq!(state.dock(Region::Bottom), None);
    }

    #[test]
    fn unreadable_layout_state_falls_back() {
        let ok = r#"{"v":1,"docks":{"left":null,"right":{"open":true,"size":320},"bottom":null},"splits":[],"active":[]}"#;
        assert!(LayoutState::parse(ok).is_some());
        let bad = [
            String::new(),
            "{".into(),
            "[]".into(),
            // 不认识的版本。
            ok.replace(r#""v":1"#, r#""v":2"#),
            // 不认识的字段。
            ok.replace(r#""active":[]"#, r#""active":[],"zoom":"x""#),
            // 宽度不合法。
            ok.replace(r#""size":320"#, r#""size":-1"#),
            ok.replace(r#""size":320"#, r#""size":0"#),
            // 拆分的子节点和大小对不上，或者不成拆分。
            ok.replace(
                r#""splits":[]"#,
                r#""splits":[{"children":["a","b"],"sizes":[1]}]"#,
            ),
            ok.replace(
                r#""splits":[]"#,
                r#""splits":[{"children":["a"],"sizes":[1]}]"#,
            ),
        ];
        for bad in bad {
            assert_eq!(LayoutState::parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn broken_json_is_rejected() {
        rejects("{", "EOF");
    }

    #[test]
    fn a_newer_format_is_rejected_as_newer() {
        rejects(
            r#"{"format":2,"name":"x","center":{"tabs":["smelt.stage"]}}"#,
            "比当前 smelt 新",
        );
        rejects(
            r#"{"format":0,"name":"x","center":{"tabs":["smelt.stage"]}}"#,
            "不认识的 format",
        );
    }

    #[test]
    fn the_stage_is_required() {
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.sessions"]}}"#,
            "没有 `smelt.stage`",
        );
    }

    #[test]
    fn a_builtin_may_appear_once() {
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage","smelt.stage"]}}"#,
            "内置组件在一套布局里只能放一处",
        );
    }

    #[test]
    fn a_plugin_panel_repeats_with_different_props() {
        let layout = parse(
            r#"{"format":1,"name":"x",
                "center":{"split":"row","children":[
                  {"tabs":[{"component":"plugin:k/board","props":{"machine":"local","x":[1,{"b":1,"a":2}]},"title":"本机"}]},
                  {"tabs":["smelt.stage"]},
                  {"tabs":[{"component":"plugin:k/board","props":{"machine":"m1"}}, "plugin:k/board"]}]}}"#,
        )
        .unwrap();
        let panels = layout.plugin_panels();
        assert_eq!(panels.len(), 3);
        assert_eq!(
            panels[0].props.as_ref().unwrap().value(),
            serde_json::json!({"machine":"local","x":[1,{"a":2,"b":1}]})
        );
        assert_eq!(panels[2].props, None);
        let Node::Split { children, .. } = &layout.center else {
            panic!("center 是拆分");
        };
        let Node::Tabs { tabs, .. } = &children[0].0 else {
            panic!("第一块是标签组");
        };
        assert_eq!(tabs[0].title.as_deref(), Some("本机"));
    }

    #[test]
    fn props_are_compared_by_content() {
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",
                {"component":"plugin:k/board","props":{"a":1,"b":2}},
                {"component":"plugin:k/board","props":{"b":2,"a":1}}]}}"#,
            "以同样的 `props` 出现了不止一次",
        );
        // `props: null` 和不写一样。
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",
                "plugin:k/board",{"component":"plugin:k/board","props":null}]}}"#,
            "以同样的 `props` 出现了不止一次",
        );
    }

    #[test]
    fn only_plugin_panels_take_props_and_title() {
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":[{"component":"smelt.stage","props":{}}]}}"#,
            "只有插件面板能写",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",{"component":"smelt.git","title":"变更"}]}}"#,
            "只有插件面板能写",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",{"component":"plugin:k/board","title":" "}]}}"#,
            "`title` 不能为空",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",{"component":"plugin:k/board","width":3}]}}"#,
            "width",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage",3]}}"#,
            "center: 第 1 个标签：标签要么是组件名字符串",
        );
    }

    #[test]
    fn unknown_component_names_are_rejected() {
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage","smelt.kanban"]}}"#,
            "不认识的内置组件 `smelt.kanban`",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage","kanban"]}}"#,
            "不认识的组件 `kanban`",
        );
        for name in [
            "plugin:",
            "plugin:com.smelt.kanban",
            "plugin:/board",
            "plugin:k/",
            "plugin:k/a/b",
        ] {
            rejects(
                &format!(
                    r#"{{"format":1,"name":"x","center":{{"tabs":["smelt.stage","{name}"]}}}}"#
                ),
                "插件面板写成 `plugin:<插件 id>/<面板>`",
            );
        }
    }

    #[test]
    fn empty_nodes_and_bad_active_are_rejected_with_their_path() {
        rejects(
            r#"{"format":1,"name":"x","center":{"split":"row","children":[]}}"#,
            "center: `children` 是空的",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage"]},"left":{"tabs":[]}}"#,
            "left: `tabs` 是空的",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"split":"column","children":[
                {"tabs":["smelt.stage"]},{"tabs":["smelt.git"],"active":1}]}}"#,
            "center/1: `active` 是 1",
        );
    }

    #[test]
    fn size_and_open_only_where_they_mean_something() {
        rejects(
            r#"{"format":1,"name":"x","center":{"size":10,"tabs":["smelt.stage"]}}"#,
            "不能写 `size`",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"open":false,"tabs":["smelt.stage"]}}"#,
            "`open` 只能写在停靠区",
        );
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage"]},"left":{"size":-1,"tabs":["smelt.sessions"]}}"#,
            "要是正数",
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        rejects(
            r#"{"format":1,"name":"x","center":{"tabs":["smelt.stage"],"floating":true}}"#,
            "floating",
        );
    }

    #[test]
    fn discover_lists_json_sorted_and_refuses_default() {
        let root = std::env::temp_dir().join(format!("smelt-layouts-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ok = r#"{"format":1,"name":"A","center":{"tabs":["smelt.stage"]}}"#;
        std::fs::write(root.join("a.json"), ok).unwrap();
        // 按文件名排 `a-b.json` 在 `a.json` 前面；按 id 排 `a` 在前。
        std::fs::write(root.join("a-b.json"), ok).unwrap();
        std::fs::write(root.join("default.json"), ok).unwrap();
        std::fs::write(root.join("broken.json"), "{").unwrap();
        std::fs::write(root.join("notes.txt"), "x").unwrap();

        let entries = discover(&root);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["a", "a-b", "broken", "default"]);
        assert_eq!(entries[0].layout.as_ref().unwrap().name, "A");
        assert!(entries[2].layout.is_err());
        assert!(entries[3].layout.as_ref().unwrap_err().contains("留给内置"));
        assert!(discover(&root.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plugin_layouts_are_named_after_their_plugin() {
        let root =
            std::env::temp_dir().join(format!("smelt-plugin-layouts-{}", std::process::id()));
        std::fs::create_dir_all(root.join(LAYOUT_DIR)).unwrap();
        let ok = r#"{"format":1,"name":"旅人","center":{"tabs":["smelt.stage"]}}"#;
        std::fs::write(root.join(LAYOUT_DIR).join("traveler.json"), ok).unwrap();

        let entries = discover_plugin("com.wawa.traveler", &root);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["com.wawa.traveler/traveler"]);
        assert_eq!(layout_plugin(&entries[0].id), Some("com.wawa.traveler"));
        assert_eq!(layout_plugin("my-layout"), None);
        // 插件没带布局就是没有。
        assert!(discover_plugin("x", &root.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn backgrounds_sit_on_regions() {
        let layout = parse(
            r#"{
              "format": 1, "name": "地图",
              "left":   { "tabs": ["smelt.sessions"], "background": { "image": "assets/night.svg", "opacity": 0.3 } },
              "center": { "tabs": ["smelt.stage"], "background": { "image": "assets/map.png" } }
            }"#,
        )
        .unwrap();
        assert_eq!(
            layout.background(Region::Left),
            Some(&Background {
                image: PathBuf::from("assets/night.svg"),
                opacity: 0.3,
            })
        );
        // 没写不透明度就是原样画。
        assert_eq!(layout.background(Region::Center).unwrap().opacity, 1.);
        assert_eq!(layout.background(Region::Right), None);
        assert_eq!(Layout::default_layout().background(Region::Center), None);
    }

    #[test]
    fn backgrounds_are_checked() {
        let with = |center: &str| {
            parse(&format!(r#"{{"format":1,"name":"x","center":{center}}}"#)).unwrap_err()
        };
        let in_split = with(
            r#"{"split":"row","children":[{"tabs":["smelt.stage"],"background":{"image":"a.png"}}]}"#,
        );
        assert!(in_split.contains("center/0") && in_split.contains("只能写在区域上"));
        let opacity =
            with(r#"{"tabs":["smelt.stage"],"background":{"image":"a.png","opacity":1.5}}"#);
        assert!(opacity.contains("0 到 1"));
        let empty = with(r#"{"tabs":["smelt.stage"],"background":{"image":" "}}"#);
        assert!(empty.contains("不能为空"));
        let unknown =
            with(r#"{"tabs":["smelt.stage"],"background":{"image":"a.png","fit":"contain"}}"#);
        assert!(unknown.contains("fit"));
    }

    /// 插件布局的背景图相对插件目录；图不在，布局就是有错的。
    #[test]
    fn plugin_backgrounds_resolve_against_the_plugin_root() {
        let root = tempfile::tempdir().unwrap();
        let layouts = root.path().join(LAYOUT_DIR);
        std::fs::create_dir_all(&layouts).unwrap();
        std::fs::create_dir_all(root.path().join("assets")).unwrap();
        std::fs::write(root.path().join("assets/map.svg"), "<svg/>").unwrap();
        let with = |image: &str| {
            format!(
                r#"{{"format":1,"name":"x","center":{{"tabs":["smelt.stage"],"background":{{"image":"{image}"}}}}}}"#
            )
        };
        std::fs::write(layouts.join("found.json"), with("assets/map.svg")).unwrap();
        std::fs::write(layouts.join("lost.json"), with("assets/gone.png")).unwrap();

        let entries = discover_plugin("p", root.path());
        let found = entries[0].layout.as_ref().unwrap();
        assert_eq!(
            found.background(Region::Center).unwrap().image,
            root.path().join("assets/map.svg")
        );
        let lost = entries[1].layout.as_ref().unwrap_err();
        assert!(
            lost.contains("gone.png") && lost.contains("不存在"),
            "{lost}"
        );
    }
}
