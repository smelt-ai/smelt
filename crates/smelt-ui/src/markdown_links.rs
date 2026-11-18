//! 对话消息里 Markdown 链接的点击和悬停提示：本地文件交给应用登记的打开者，其余交给系统；
//! 悬停时提示点下去打开的是什么。
//!
//! 只在点击时解析，不改写消息原文：原文改写认不出代码块，代码里的链接也会被换掉，
//! 复制出来的也是改写后的。不解析的话，gpui-base 的 `TextView` 会把链接目标原样交给
//! `App::open_url`，相对文件路径在 macOS 上会被 LaunchServices 误当作应用标识并报 -50。
//!
//! 文件链接在进程里直接交给 [`set_file_link_handler`] 登记的处理函数，不绕系统的 URL
//! 协议：那要在每个平台注册协议、再由系统回调应用，Linux、Windows 上还不一定注册得上。

use std::rc::Rc;

use gpui::{AnyView, App, ClickEvent, Global, MouseButton, SharedString, Styled as _, Window, px};

use crate::tooltip::Tooltip;

/// Markdown 里文件链接的解析基准。相对路径按 `cwd` 补全；`runtime` 是远程运行时的 id，
/// 跟着链接交出去，应用据此知道文件在哪台机器上，不会去打开本机同路径的文件。
#[derive(Clone, Copy, Debug, Default)]
pub struct FileLinks<'a> {
    cwd: Option<&'a str>,
    runtime: Option<&'a str>,
}

impl<'a> FileLinks<'a> {
    pub fn new(cwd: Option<&'a str>, runtime: Option<&'a str>) -> Self {
        Self { cwd, runtime }
    }
}

/// 点中的本地文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLinkTarget {
    /// 文件所在的远程运行时 id；本机是 `None`。
    pub runtime: Option<String>,
    /// 那台机器上的绝对路径。
    pub path: String,
    /// 行号（1 基）。
    pub line: Option<u32>,
}

/// 点击链接时要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// 本地文件，交给 [`set_file_link_handler`] 登记的处理函数。
    File(FileLinkTarget),
    /// 别的（网址、补不全的相对路径），原样交给系统。
    Url(String),
}

/// 解析点中的链接。
pub fn link_target(url: &str, links: FileLinks<'_>) -> LinkTarget {
    // 哪些算文件链接、相对路径怎么补全、`path:行号` 怎么拆，和手机共用 smelt-model 的规则。
    match smelt_model::file_link::parse_file_link(
        url,
        links.cwd,
        smelt_model::runtime_path::PathStyle::LOCAL,
    ) {
        Some(link) => LinkTarget::File(FileLinkTarget {
            runtime: links.runtime.map(str::to_owned),
            line: link.line(),
            path: link.path,
        }),
        None => LinkTarget::Url(url.to_string()),
    }
}

struct FileLinkHandler(Rc<dyn Fn(FileLinkTarget, &mut App)>);

impl Global for FileLinkHandler {}

/// 登记点中文件链接时怎么打开。应用启动时登记一次；没登记时点文件链接什么都不做。
pub fn set_file_link_handler(handler: impl Fn(FileLinkTarget, &mut App) + 'static, cx: &mut App) {
    cx.set_global(FileLinkHandler(Rc::new(handler)));
}

/// 给 `TextView::on_link_click` 的处理函数，见 [`link_target`]。
pub(crate) fn link_opener(
    links: FileLinks<'_>,
) -> impl Fn(&SharedString, &ClickEvent, &mut Window, &mut App) + Send + Sync + 'static {
    let cwd = links.cwd.map(str::to_owned);
    let runtime = links.runtime.map(str::to_owned);
    move |url, event, _window, cx| {
        if !opens_link(event) {
            return;
        }
        match link_target(url, FileLinks::new(cwd.as_deref(), runtime.as_deref())) {
            LinkTarget::File(target) => {
                if let Some(handler) = cx.try_global::<FileLinkHandler>().map(|h| h.0.clone()) {
                    handler(target, cx);
                }
            }
            LinkTarget::Url(url) => cx.open_url(&url),
        }
    }
}

/// 给 `TextView::link_tooltip` 的提示：悬停时说清点下去会打开什么。
pub(crate) fn link_tooltip(
    links: FileLinks<'_>,
) -> impl Fn(&SharedString, &mut Window, &mut App) -> AnyView + Send + Sync + 'static {
    let cwd = links.cwd.map(str::to_owned);
    let runtime = links.runtime.map(str::to_owned);
    move |url, window, cx| {
        let links = FileLinks::new(cwd.as_deref(), runtime.as_deref());
        // 网址可以很长，限宽折行，不让提示横穿窗口。
        Tooltip::new(link_description(url, links))
            .max_w(px(TOOLTIP_MAX_WIDTH))
            .build(window, cx)
    }
}

const TOOLTIP_MAX_WIDTH: f32 = 420.;

/// 文件是补全后的绝对路径，有行号带上 `:行号`；别的原样。消息里写的常是相对路径或
/// `#L42`，提示里给出真正要打开的那个。
fn link_description(url: &str, links: FileLinks<'_>) -> String {
    match link_target(url, links) {
        LinkTarget::File(FileLinkTarget {
            path,
            line: Some(line),
            ..
        }) => format!("{path}:{line}"),
        LinkTarget::File(target) => target.path,
        LinkTarget::Url(url) => url,
    }
}

/// 和 gpui-base `TextView` 默认的打开条件一致：左键、中键、键盘、非长按的触摸。右键留给菜单。
fn opens_link(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
        }
        ClickEvent::Keyboard(_) => true,
        ClickEvent::Touch(click) => !click.long_press,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(runtime: Option<&str>, path: &str, line: Option<u32>) -> LinkTarget {
        LinkTarget::File(FileLinkTarget {
            runtime: runtime.map(str::to_owned),
            path: path.to_string(),
            line,
        })
    }

    #[test]
    fn local_files_become_file_targets() {
        let links = FileLinks::new(Some("/tmp/project"), None);
        assert_eq!(
            link_target("docs/workspace.md", links),
            file(None, "/tmp/project/docs/workspace.md", None)
        );
        assert_eq!(
            link_target("/tmp/source.rs#L42", links),
            file(None, "/tmp/source.rs", Some(42))
        );
        assert_eq!(
            link_target("/tmp/a b.rs", links),
            file(None, "/tmp/a b.rs", None)
        );
        assert_eq!(
            link_target("https://example.com", links),
            LinkTarget::Url("https://example.com".into())
        );
    }

    #[test]
    fn tooltip_names_what_a_click_opens() {
        let links = FileLinks::new(Some("/tmp/project"), None);
        assert_eq!(
            link_description("docs/workspace.md", links),
            "/tmp/project/docs/workspace.md"
        );
        assert_eq!(
            link_description("/tmp/source.rs#L42", links),
            "/tmp/source.rs:42"
        );
        assert_eq!(
            link_description("https://example.com/a?b=1", links),
            "https://example.com/a?b=1"
        );
    }

    #[test]
    fn absolute_files_resolve_without_session_cwd() {
        assert_eq!(
            link_target(
                "/tmp/smelt-current-notification-final.png",
                FileLinks::default()
            ),
            file(None, "/tmp/smelt-current-notification-final.png", None)
        );
        // 没有 cwd 补不全的相对路径原样交出去，不猜。
        assert_eq!(
            link_target("docs/a.md", FileLinks::default()),
            LinkTarget::Url("docs/a.md".into())
        );
    }

    /// grep / 编译器诊断常见的 `path:行号` 引用格式（不是 `#L行号` 片段）也要能拆出行号，
    /// 不能把 `:2765` 当成文件名的一部分。
    #[test]
    fn colon_line_refs_resolve_to_a_line() {
        assert_eq!(
            link_target(
                "crates/app/src/view.rs:2765",
                FileLinks::new(Some("/tmp/project"), None),
            ),
            file(None, "/tmp/project/crates/app/src/view.rs", Some(2765))
        );
    }

    /// `path:行号:列号` 形式（列号可选的第二段）也只取行号，列号丢弃。
    #[test]
    fn colon_line_col_refs_take_line_not_col() {
        assert_eq!(
            link_target(
                "src/main.rs:10:5",
                FileLinks::new(Some("/tmp/project"), None)
            ),
            file(None, "/tmp/project/src/main.rs", Some(10))
        );
    }

    /// 远程对话里的文件在另一台机器上：带上运行时 id，应用不会去打开本机同路径的文件。
    #[test]
    fn files_in_remote_conversations_carry_the_runtime() {
        let remote = "39fafd41-9942-49b0-b9ec-7eb670d2711e";
        let links = FileLinks::new(Some("/root/project"), Some(remote));
        assert_eq!(
            link_target("src/main.rs:3", links),
            file(Some(remote), "/root/project/src/main.rs", Some(3))
        );
        assert_eq!(
            link_target("/etc/hosts", links),
            file(Some(remote), "/etc/hosts", None)
        );
    }

    #[gpui::test]
    fn clicked_files_go_to_the_registered_handler(cx: &mut gpui::TestAppContext) {
        let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let opened = opened.clone();
            set_file_link_handler(move |target, _| opened.borrow_mut().push(target), cx);
        });
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let open = link_opener(FileLinks::new(Some("/repo"), None));
            let click = ClickEvent::Keyboard(Default::default());
            open(&"src/lib.rs:7".into(), &click, window, cx);
        });
        assert_eq!(
            *opened.borrow(),
            [FileLinkTarget {
                runtime: None,
                path: "/repo/src/lib.rs".into(),
                line: Some(7),
            }]
        );
    }
}
