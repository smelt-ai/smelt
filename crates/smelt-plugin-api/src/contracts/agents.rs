//! 智能体（来源 `smelt.agents`）：每台机器上的智能体定义，和跟智能体开新对话的入口。

use crate::Command;

/// 智能体页。打开时的参数都可以不给：
/// `{"machine": …, "agent": "<id>", "tab": "config" | "memory", "new_conversation": true}`。
/// `agent` 打开那个智能体的编辑页（在 `machine` 上，不给是本机），`tab` 是停在哪个页签；
/// `new_conversation` 是从「新对话」过来、还没选默认智能体。
pub const VIEW: &str = "smelt.agents/main";

/// 跟默认智能体开一段新对话；没有默认就去智能体页选，一个都没有就去建。不带参数，
/// 标题里写着默认智能体（「新对话 · 写作助手」），侧栏顶上的按钮和 ⌘N 都是它。
pub const NEW_CONVERSATION: Command<(), ()> = Command::new("smelt.agents/new-conversation");
