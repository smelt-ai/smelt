//! 执行过程里一次工具调用的样子：动词句和图标。照对话视图的写法，运行详情和现场读起来一致。

use api::model::acp_chat::ToolKind;
use gpui_component::{Icon, IconName};
use smelt_plugin_api as api;

/// 时间线上单条工具的动词句。
pub(crate) fn compact_tool_headline(kind: ToolKind, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return tool_kind_label(kind).to_string();
    }
    match kind {
        ToolKind::Fetch => format!("打开了 {}", compact_fetch_target(title)),
        ToolKind::Search => format!("搜索 {title}"),
        ToolKind::Read => format!("读取了 {}", compact_path_leaf(title)),
        ToolKind::Edit => format!("修改了 {}", compact_path_leaf(title)),
        ToolKind::Delete => format!("删除了 {}", compact_path_leaf(title)),
        ToolKind::Move => format!("移动了 {}", compact_path_leaf(title)),
        ToolKind::Execute => format!("运行了 {}", compact_command_leaf(title)),
        ToolKind::Image => format!("处理了 {}", compact_path_leaf(title)),
        _ => title.to_string(),
    }
}

/// 工具调用的图标按它做的事挑：打开页面是地球，搜索是放大镜；思考、评审、整理、切模式是
/// Agent 自己的步骤，用星号；等待是时钟，说不清是什么的工具是扳手。
pub(crate) fn process_icon(kind: &ToolKind) -> Icon {
    match kind {
        ToolKind::Fetch => IconName::Globe.into(),
        ToolKind::Search => IconName::Search.into(),
        ToolKind::Execute => IconName::SquareTerminal.into(),
        ToolKind::Read | ToolKind::Move | ToolKind::Image => IconName::File.into(),
        ToolKind::Edit => Icon::empty().path("smelt-icons/square-pen.svg"),
        ToolKind::Delete => Icon::empty().path("smelt-icons/trash.svg"),
        ToolKind::Collaborate => IconName::Bot.into(),
        ToolKind::Review | ToolKind::Compact | ToolKind::Think | ToolKind::SwitchMode => {
            IconName::Asterisk.into()
        }
        ToolKind::Wait => Icon::new(gpui_kit_assets::IconName::Clock),
        ToolKind::Other => Icon::new(gpui_kit_assets::IconName::Wrench),
    }
}

/// 简短英文标签，跟工具在协议里的调用名对齐。
fn tool_kind_label(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "Read",
        ToolKind::Edit => "Edit",
        ToolKind::Delete => "Delete",
        ToolKind::Move => "Move",
        ToolKind::Search => "Search",
        ToolKind::Execute => "Bash",
        ToolKind::Fetch => "Fetch",
        ToolKind::Think => "Think",
        ToolKind::SwitchMode => "Mode",
        ToolKind::Collaborate => "Agent",
        ToolKind::Review => "Review",
        ToolKind::Image => "Image",
        ToolKind::Compact => "Compact",
        ToolKind::Wait => "Wait",
        _ => "Tool",
    }
}

fn compact_fetch_target(title: &str) -> String {
    let stripped = title
        .strip_prefix("https://")
        .or_else(|| title.strip_prefix("http://"))
        .unwrap_or(title);
    stripped
        .strip_prefix("www.")
        .unwrap_or(stripped)
        .to_string()
}

fn compact_path_leaf(title: &str) -> String {
    title
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(title)
        .to_string()
}

fn compact_command_leaf(title: &str) -> String {
    title
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or(title)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::compact_tool_headline;
    use smelt_plugin_api::model::acp_chat::ToolKind;

    #[test]
    fn headlines_name_the_target_not_the_full_path() {
        assert_eq!(
            compact_tool_headline(ToolKind::Read, "/repo/src/lib.rs"),
            "读取了 lib.rs"
        );
        assert_eq!(
            compact_tool_headline(ToolKind::Fetch, "https://www.example.com/a"),
            "打开了 example.com/a"
        );
        assert_eq!(
            compact_tool_headline(ToolKind::Execute, "cargo test\n--all"),
            "运行了 cargo test"
        );
        assert_eq!(compact_tool_headline(ToolKind::Execute, "  "), "Bash");
    }
}
