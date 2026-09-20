//! 插件自留地面板的 WebView 宿主。
//!
//! smelt 自己的 UI 是 GPUI 自绘的，插件不可能往里注入元素——那部分只能走声明式
//! contribution。但插件"整块自己的地盘"（看板、图表、列表）可以交给 WebView，
//! 插件想怎么写就怎么写。
//!
//! 挂载手法与 `liquid_glass.rs` 同源：从 GPUI 窗口取 raw-window-handle 找到
//! AppKit view。玻璃材质仍是 Metal view 下的 subview；插件 WKWebView 则由
//! wry 放进自己的 borderless child window，因而与 GPUI 的 `performKeyEquivalent:`
//! 和输入法上下文隔离。
//!
//! WebView 非 `Send`，且只能在主线程操作，所以实例存在 thread-local 注册表里，
//! 由 GPUI 的 render（必然在主线程）驱动。

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

/// 面板内容来源。资源一律走自定义协议从**已校验的插件包目录**读，
/// 既不起本地 HTTP 服务，也不允许 `file://` 或远程 URL。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelSource {
    /// 插件包内的静态站点根目录，入口固定 `index.html`。
    PackageRoot(PathBuf),
    /// 直接给一段 HTML（错误兜底页用）。
    Html(String),
}

/// 一个插件面板的挂载描述。`id` 同时是自定义协议的 host 段，
/// 因此每个插件的资源空间彼此隔离。
#[derive(Clone, Debug)]
pub struct PanelSpec {
    pub id: String,
    pub source: PanelSource,
    /// 注入给页面的主题变量（`--smelt-*` CSS 自定义属性）。
    pub theme: Vec<(String, String)>,
    /// 插件是否获授 `web.browse`。没有它，页面的 CSP 锁死在包内资源。
    pub allow_web_browse: bool,
    pub devtools: bool,
}

/// GPUI 逻辑像素矩形。取自元素 bounds，原点在窗口左上、y 向下；
/// wry 的 `set_bounds` 会按 parent view 的 `isFlipped` 自行翻转。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PanelRect {
    fn is_degenerate(&self) -> bool {
        !self.x.is_finite()
            || !self.y.is_finite()
            || !self.width.is_finite()
            || !self.height.is_finite()
            || self.width < 1.0
            || self.height < 1.0
    }

    /// 亚像素抖动不值得跨 FFI 调一次 `setFrame:`。
    fn nearly_eq(&self, other: &Self) -> bool {
        let close = |a: f32, b: f32| (a - b).abs() < 0.5;
        close(self.x, other.x)
            && close(self.y, other.y)
            && close(self.width, other.width)
            && close(self.height, other.height)
    }
}

/// 页面通过 `window.smelt.post(...)` 发上来的一条消息。
#[derive(Clone, Debug)]
pub struct PanelMessage {
    pub panel_id: String,
    pub body: String,
}

/// 内容视图的导航状态。
///
/// 插件面板自己的页面画地址栏和按钮，真正加载外部网页的是宿主管的这个
/// WebView——它就是浏览器本身，因此不受 `X-Frame-Options` 约束（那是给
/// `iframe` 这类嵌套浏览上下文用的）。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PanelViewState {
    pub url: String,
    pub title: String,
    pub loading: bool,
}

/// 内容视图状态变化，宿主每帧取走后转发给面板页面。
#[derive(Clone, Debug)]
pub struct PanelViewEvent {
    pub panel_id: String,
    pub state: PanelViewState,
}

#[cfg(target_os = "macos")]
mod imp;

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub(super) fn create(_: &gpui::Window, _: &PanelSpec, _: PanelRect) -> Result<(), String> {
        Err("插件 WebView 面板目前只在 macOS 上实现".into())
    }
    pub(super) fn set_bounds(_: &str, _: PanelRect) -> Result<(), String> {
        Ok(())
    }
    pub(super) fn set_visible(_: &str, _: bool) {}
    pub(super) fn activate_panel(_: &str) {}
    pub(super) fn release_focus_in(_: &gpui::Window) {}
    pub(super) fn navigate_view(_: &str, _: &str, _: PanelRect) -> Result<(), String> {
        Err("插件内容视图目前只在 macOS 上实现".into())
    }
    pub(super) fn view_command(_: &str, _: super::PanelViewCommand) -> Result<(), String> {
        Ok(())
    }
    pub(super) fn set_view_bounds(_: &str, _: PanelRect) -> Result<(), String> {
        Ok(())
    }
    pub(super) fn drain_view_events() -> Vec<super::PanelViewEvent> {
        Vec::new()
    }
    pub(super) fn close(_: &str) {}
    pub(super) fn drain_messages() -> Vec<PanelMessage> {
        Vec::new()
    }
    pub(super) fn post(_: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
}

/// 已挂载面板的上一帧状态。GPUI 每帧都会调 `sync_panel`，所以这里必须能在
/// 无变化时完全跳过跨 FFI 调用。
#[derive(Clone, Copy)]
struct PanelState {
    rect: PanelRect,
    visible: bool,
}

thread_local! {
    /// 页面 → 宿主的消息队列。IPC handler 在主线程回调，GPUI 侧每帧取走。
    static INBOX: Rc<RefCell<VecDeque<PanelMessage>>> = Rc::new(RefCell::new(VecDeque::new()));
    static STATES: RefCell<HashMap<String, PanelState>> = RefCell::new(HashMap::new());
    /// 页面脚本加载完成后才接收生命周期消息，避免 evaluate_script 把消息丢在
    /// 插件自己的监听器注册之前。
    static PANEL_READY: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// 每个可见性状态只投递一次；页面重新加载时由 panel_loading/panel_ready 清掉。
    static PANEL_VISIBILITY_DELIVERED: RefCell<HashMap<String, bool>> =
        RefCell::new(HashMap::new());
}

/// 面板从「未挂载 / 隐藏」变为可见时，子窗口必须立刻成为 key。
///
/// 侧栏或 tab 上的那次点击落在 GPUI 上，父窗口仍是 key。若不在这里抢 key，
/// 用户还要再点一次页面，AppKit 才把这次点击当成窗口激活而不是按钮命中。
/// 已经可见时不要每帧抢：用户点回对话后，焦点应留在 GPUI。
pub(crate) fn should_activate_panel_key(was_visible: Option<bool>, now_visible: bool) -> bool {
    now_visible && was_visible != Some(true)
}

/// 把面板同步到 `rect`：没挂载就创建，已挂载只在矩形变化时移动。
///
/// 由 GPUI 的 paint 阶段每帧调用，因此必须廉价且幂等。
pub fn sync_panel(window: &gpui::Window, spec: &PanelSpec, rect: PanelRect) -> Result<(), String> {
    // 面板被折叠、或父容器这一帧还没布局出来：不要创建 WebView。一个 0 尺寸的
    // WKWebView 仍然会拉起一个 web content 进程。
    if rect.is_degenerate() {
        set_panel_visible(&spec.id, false);
        return Ok(());
    }
    let previous = STATES.with(|states| states.borrow().get(&spec.id).copied());
    match previous {
        Some(state) if state.rect.nearly_eq(&rect) && state.visible => {
            // 页面可能刚刚重载，ready 握手会清掉投递缓存；这里的检查只是一条
            // 廉价的状态短路，不会每帧执行脚本。
            notify_panel_visibility(&spec.id, true);
            Ok(())
        }
        Some(state) => {
            if !state.rect.nearly_eq(&rect) {
                imp::set_bounds(&spec.id, rect)?;
            }
            if !state.visible {
                imp::set_visible(&spec.id, true);
            }
            store(&spec.id, rect, true);
            if should_activate_panel_key(Some(state.visible), true) {
                imp::activate_panel(&spec.id);
            }
            notify_panel_visibility(&spec.id, true);
            Ok(())
        }
        None => {
            imp::create(window, spec, rect)?;
            store(&spec.id, rect, true);
            if should_activate_panel_key(None, true) {
                imp::activate_panel(&spec.id);
            }
            // 创建时页面还未必完成加载，ready 握手会在可接收后补发。
            notify_panel_visibility(&spec.id, true);
            Ok(())
        }
    }
}

/// 显隐面板。GPUI 的下拉菜单/模态会被 WebView 盖住（它是原生 view，永远在
/// Metal 内容之上），所以弹层打开、或面板所在 tab 切走时，宿主必须主动把它
/// 藏起来——WebView 不会因为 GPUI 停止渲染那块区域就自己消失。
pub fn set_panel_visible(id: &str, visible: bool) {
    let (known, changed) = STATES.with(|states| {
        let mut states = states.borrow_mut();
        match states.get_mut(id) {
            Some(state) if state.visible != visible => {
                state.visible = visible;
                (true, true)
            }
            Some(_) => (true, false),
            None => (false, false),
        }
    });
    if changed {
        imp::set_visible(id, visible);
        if should_activate_panel_key(Some(!visible), visible) {
            imp::activate_panel(id);
        }
    }
    if known {
        notify_panel_visibility(id, visible);
    }
}

/// 让面板的内容视图导航到 `url`，并摆在面板内的 `rect` 位置。
///
/// 内容视图懒创建：插件不用这个能力就不会有第二个 WebView。
pub fn navigate_panel_view(id: &str, url: &str, rect: PanelRect) -> Result<(), String> {
    imp::navigate_view(id, url, rect)
}

/// 内容视图的导航动作。
pub fn panel_view_command(id: &str, command: PanelViewCommand) -> Result<(), String> {
    imp::view_command(id, command)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelViewCommand {
    Back,
    Forward,
    Reload,
    Hide,
}

/// 只移动内容视图，不改变它加载的页面（面板尺寸变化时用）。
pub fn set_panel_view_bounds(id: &str, rect: PanelRect) -> Result<(), String> {
    imp::set_view_bounds(id, rect)
}

/// 取走内容视图的状态变化，宿主负责转发给面板页面。
pub fn drain_view_events() -> Vec<PanelViewEvent> {
    imp::drain_view_events()
}

/// 把键盘焦点从面板交还给 GPUI。
///
/// 必须在"用户点到面板之外"时调用：GPUI 只在建窗时设过一次 first responder，
/// 之后从不重设，所以 WebView 抢走焦点后没人会自动收回来——表现就是点回对话
/// 框还能打英文，却打不出中文（输入法仍认着 WebView）。
pub fn release_focus(window: &gpui::Window) {
    imp::release_focus_in(window);
}

/// 销毁面板（插件停用、窗口关闭、tab 永久移除）。
pub fn close_panel(id: &str) {
    imp::close(id);
    STATES.with(|states| {
        states.borrow_mut().remove(id);
    });
    PANEL_READY.with(|ready| {
        ready.borrow_mut().remove(id);
    });
    PANEL_VISIBILITY_DELIVERED.with(|delivered| {
        delivered.borrow_mut().remove(id);
    });
}

/// 页面开始一次新的加载。生命周期状态要等页面自己的脚本 ready 后再补发。
pub fn panel_loading(id: &str) {
    PANEL_READY.with(|ready| {
        ready.borrow_mut().remove(id);
    });
    PANEL_VISIBILITY_DELIVERED.with(|delivered| {
        delivered.borrow_mut().remove(id);
    });
}

/// 页面脚本已注册 bridge，可以安全接收当前 active 状态。
pub fn panel_ready(id: &str) {
    PANEL_READY.with(|ready| {
        ready.borrow_mut().insert(id.to_string());
    });
    PANEL_VISIBILITY_DELIVERED.with(|delivered| {
        delivered.borrow_mut().remove(id);
    });
    let visible = STATES.with(|states| states.borrow().get(id).map(|state| state.visible));
    if let Some(visible) = visible {
        notify_panel_visibility(id, visible);
    }
}

fn store(id: &str, rect: PanelRect, visible: bool) {
    STATES.with(|states| {
        states
            .borrow_mut()
            .insert(id.to_string(), PanelState { rect, visible });
    });
}

fn notify_panel_visibility(id: &str, visible: bool) {
    let ready = PANEL_READY.with(|ready| ready.borrow().contains(id));
    if !ready {
        return;
    }
    let already_delivered = PANEL_VISIBILITY_DELIVERED
        .with(|delivered| delivered.borrow().get(id).copied() == Some(visible));
    if already_delivered {
        return;
    }
    let payload = serde_json::json!({
        "kind": "panel.visibility",
        "visible": visible,
    })
    .to_string();
    if imp::post(id, &payload).is_ok() {
        PANEL_VISIBILITY_DELIVERED.with(|delivered| {
            delivered.borrow_mut().insert(id.to_string(), visible);
        });
    }
}

/// 取走页面发上来的全部消息。宿主负责做 capability 检查后再转发给插件进程。
pub fn drain_messages() -> Vec<PanelMessage> {
    imp::drain_messages()
}

/// 宿主 → 页面。普通 `payload` 会作为 `smelt:message` 事件派发；保留的
/// `panel.visibility` 消息由 bridge 转成生命周期回调。
pub fn post_to_panel(id: &str, payload: &str) -> Result<(), String> {
    imp::post(id, payload)
}

/// 注入进每个面板页面的引导脚本：把主题变量落到 `:root`，并暴露最小的
/// `window.smelt` 通道和面板生命周期回调。页面拿不到 wry 的 `window.ipc`——
/// 它被这层包住，将来加 capability 检查时不必改页面。
fn bootstrap_script(spec: &PanelSpec) -> String {
    let vars = spec
        .theme
        .iter()
        .map(|(name, value)| format!("r.setProperty('--smelt-{name}', {});", json_string(value)))
        .collect::<Vec<_>>()
        .join("");
    let panel_id = json_string(&spec.id);
    // `request` 是带 id 的往返，宿主把它转成一次 Invocation 再把结果送回来。
    // `host` 同样是带 id 的往返，但收件人是宿主自己——那些页面做不到、也不该
    // 做的事（原生对话框、Finder、外部浏览器），每条都由 capability 单独门控。
    // 页面永远拿不到 wry 的 `window.ipc`——它被这一层包住，将来收紧权限
    // 不需要改动任何插件页面。
    format!(
        "(function(){{\
           var r=document.documentElement.style;{vars}\
           var t=new EventTarget();var pending={{}};var seq=0;\
           var active=true;var visibilityHandlers=[];\
           function send(m){{if(window.ipc&&window.ipc.postMessage){{window.ipc.postMessage(JSON.stringify(m));}}}}\
           function setActive(value){{\
             var next=!!value;\
             if(active===next){{return;}}\
             active=next;\
             window.smelt.active=next;\
             visibilityHandlers.slice().forEach(function(f){{try{{f(next);}}catch(_error){{}}}});\
           }}\
           window.smelt={{\
             panelId:{panel_id},\
             active:true,\
             post:function(m){{send({{event:m}});}},\
             request:function(p){{\
               var id='r'+(++seq);\
               return new Promise(function(res,rej){{\
                 pending[id]={{res:res,rej:rej}};\
                 send({{rid:id,payload:p}});\
               }});\
             }},\
             host:function(c,p){{\
               var id='h'+(++seq);\
               return new Promise(function(res,rej){{\
                 pending[id]={{res:res,rej:rej}};\
                 send({{rid:id,command:c,params:p||{{}}}});\
               }});\
             }},\
             on:function(f){{t.addEventListener('smelt:message',function(e){{f(e.detail);}});}},\
             onVisibilityChange:function(f){{\
               if(typeof f!=='function'){{return function(){{}};}}\
               visibilityHandlers.push(f);f(active);\
               return function(){{var i=visibilityHandlers.indexOf(f);if(i>=0){{visibilityHandlers.splice(i,1);}}}};\
             }},\
             view:{{\
               navigate:function(u,r){{send({{event:{{kind:'view.navigate',url:u,rect:r}}}});}},\
               setBounds:function(r){{send({{event:{{kind:'view.bounds',rect:r}}}});}},\
               back:function(){{send({{event:{{kind:'view.back'}}}});}},\
               forward:function(){{send({{event:{{kind:'view.forward'}}}});}},\
               reload:function(){{send({{event:{{kind:'view.reload'}}}});}},\
               hide:function(){{send({{event:{{kind:'view.hide'}}}});}}\
             }},\
             _emit:function(d){{\
               if(d&&d.rid&&pending[d.rid]){{\
                 var p=pending[d.rid];delete pending[d.rid];\
                 if(d.error){{p.rej(new Error(d.error));}}else{{p.res(d.result);}}\
                 return;\
               }}\
               if(d&&d.kind==='panel.visibility'){{setActive(d.visible);return;}}\
               t.dispatchEvent(new CustomEvent('smelt:message',{{detail:d}}));\
             }}\
           }};\
           send({{event:{{kind:'panel.loading'}}}});\
           if(document.readyState==='complete'){{\
             setTimeout(function(){{send({{event:{{kind:'panel.ready'}}}});}},0);\
           }}else{{\
             window.addEventListener('load',function(){{send({{event:{{kind:'panel.ready'}}}});}},{{once:true}});\
           }}\
         }})();"
    )
}

/// 最小 JSON 字符串转义。注入脚本里所有取自 spec 的值都要过它。
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degenerate_rect_is_not_mounted() {
        assert!(
            PanelRect {
                x: 0.,
                y: 0.,
                width: 0.,
                height: 200.
            }
            .is_degenerate()
        );
        assert!(
            PanelRect {
                x: f32::NAN,
                y: 0.,
                width: 320.,
                height: 200.
            }
            .is_degenerate()
        );
        assert!(
            !PanelRect {
                x: 0.,
                y: 0.,
                width: 320.,
                height: 200.
            }
            .is_degenerate()
        );
    }

    #[test]
    fn subpixel_jitter_does_not_move_the_webview() {
        let a = PanelRect {
            x: 10.0,
            y: 20.0,
            width: 300.0,
            height: 400.0,
        };
        let b = PanelRect {
            x: 10.2,
            y: 20.1,
            width: 300.0,
            height: 400.0,
        };
        let c = PanelRect { x: 11.0, ..a };
        assert!(a.nearly_eq(&b));
        assert!(!a.nearly_eq(&c));
    }

    #[test]
    fn bootstrap_includes_visibility_lifecycle_handshake() {
        let script = bootstrap_script(&PanelSpec {
            id: "com.example--panel".into(),
            source: PanelSource::Html(String::new()),
            theme: Vec::new(),
            allow_web_browse: false,
            devtools: false,
        });
        assert!(script.contains("onVisibilityChange"));
        assert!(script.contains("panel.loading"));
        assert!(script.contains("panel.ready"));
        assert!(script.contains("panel.visibility"));
    }

    #[test]
    fn injected_values_cannot_break_out_of_the_script() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("</script>"), "\"\\u003c/script\\u003e\"");
    }

    #[test]
    fn showing_a_plugin_panel_steals_key_so_the_first_click_reaches_the_page() {
        assert!(
            should_activate_panel_key(None, true),
            "首次挂载必须成为 key，否则侧栏那次点击只切了路由"
        );
        assert!(
            should_activate_panel_key(Some(false), true),
            "从隐藏恢复可见必须成为 key，否则切回插件 tab 还要再点一次"
        );
        assert!(
            !should_activate_panel_key(Some(true), true),
            "已经可见时不要每帧抢 key，否则点回对话后又被抢走"
        );
        assert!(
            !should_activate_panel_key(Some(true), false),
            "隐藏面板是把 key 交还 GPUI，不是激活面板"
        );
    }
}
