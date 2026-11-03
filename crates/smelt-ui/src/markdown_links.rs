//! 对话消息里 Markdown 链接的点击处理：本地文件路径解析成 Smelt 内部的 file URL。
//!
//! 只在点击时解析，不改写消息原文：原文改写认不出代码块，代码里的链接也会被换掉，
//! 复制出来的也是改写后的。不解析的话，gpui-component 会把链接目标原样交给
//! `open_url`，相对文件路径在 macOS 上会被 LaunchServices 误当作应用标识并报 -50。

use gpui::{App, ClickEvent, MouseButton, SharedString, Window};

/// Markdown 里文件链接的解析基准。相对路径按 `cwd` 补全；`runtime` 是远程运行时的 id，
/// 写进链接的 host，桌面据此知道文件在哪台机器上，不会去打开本机同路径的文件。
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

/// 点击链接时要打开的地址：本地文件路径解析成 Smelt 内部的 file URL，其余原样返回。
pub fn link_target(url: &str, links: FileLinks<'_>) -> String {
    resolve_relative_file_link(url, links).unwrap_or_else(|| url.to_string())
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
        let links = FileLinks::new(cwd.as_deref(), runtime.as_deref());
        cx.open_url(&link_target(url, links));
    }
}

/// 和 gpui-component 默认的打开条件一致：左键、中键、键盘、非长按的触摸。右键留给菜单。
fn opens_link(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
        }
        ClickEvent::Keyboard(_) => true,
        ClickEvent::Touch(click) => !click.long_press,
    }
}

fn resolve_relative_file_link(target: &str, links: FileLinks<'_>) -> Option<String> {
    // 哪些算文件链接、相对路径怎么补全、`path:行号` 怎么拆，和手机共用 smelt-model 的规则。
    let link = smelt_model::file_link::parse_file_link(
        target,
        links.cwd,
        smelt_model::runtime_path::PathStyle::LOCAL,
    )?;
    let mut url = url::Url::from_file_path(&link.path).ok()?;
    // 远程运行时上的文件：host 写运行时 id。本机的不带 host。
    if let Some(runtime) = links.runtime {
        url.set_host(Some(runtime)).ok()?;
    }
    if !link.fragment.is_empty() {
        url.set_fragment(Some(&link.fragment));
    }
    // `Url::set_scheme` 会把自定义 scheme 序列化成 `smelt-file:/path`
    // （单斜杠），而 macOS URL scheme 与主进程解析器都按 authority 形式接收。
    // 从标准 file URL 替换前缀可稳定保留 `smelt-file:///absolute/path`。
    Some(url.to_string().replacen("file://", "smelt-file://", 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_files_become_internal_file_urls() {
        let links = FileLinks::new(Some("/tmp/project"), None);
        assert_eq!(
            link_target("docs/workspace.md", links),
            "smelt-file:///tmp/project/docs/workspace.md"
        );
        assert_eq!(
            link_target("/tmp/source.rs#L42", links),
            "smelt-file:///tmp/source.rs#L42"
        );
        assert_eq!(
            link_target("https://example.com", links),
            "https://example.com"
        );
    }

    #[test]
    fn absolute_files_resolve_without_session_cwd() {
        assert_eq!(
            link_target(
                "/tmp/smelt-current-notification-final.png",
                FileLinks::default()
            ),
            "smelt-file:///tmp/smelt-current-notification-final.png"
        );
        // 没有 cwd 补不全的相对路径原样交出去，不猜。
        assert_eq!(link_target("docs/a.md", FileLinks::default()), "docs/a.md");
    }

    /// grep / 编译器诊断常见的 `path:行号` 引用格式（不是 `#L行号` 片段）也要能
    /// 拆出行号——原来只认 `#`，`:2765` 会整段被当成文件名拼进路径，导致读不到
    /// 文件、误报“可能是二进制文件”（见用户反馈：点 ACP 对话里的文件引用链接）。
    #[test]
    fn colon_line_refs_resolve_to_fragment() {
        assert_eq!(
            link_target(
                "crates/smelt-acp-view/src/acp_view.rs:2765",
                FileLinks::new(Some("/tmp/project"), None),
            ),
            "smelt-file:///tmp/project/crates/smelt-acp-view/src/acp_view.rs#L2765"
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
            "smelt-file:///tmp/project/src/main.rs#L10"
        );
    }

    /// 远程对话里的文件在另一台机器上：链接带上运行时 id，桌面不会去打开本机同路径的文件。
    #[test]
    fn files_in_remote_conversations_carry_the_runtime() {
        let links = FileLinks::new(
            Some("/root/project"),
            Some("39fafd41-9942-49b0-b9ec-7eb670d2711e"),
        );
        assert_eq!(
            link_target("src/main.rs:3", links),
            "smelt-file://39fafd41-9942-49b0-b9ec-7eb670d2711e/root/project/src/main.rs#L3"
        );
        assert_eq!(
            link_target("/etc/hosts", links),
            "smelt-file://39fafd41-9942-49b0-b9ec-7eb670d2711e/etc/hosts"
        );
    }
}
