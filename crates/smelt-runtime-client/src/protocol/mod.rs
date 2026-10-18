//! smeltd 的 wire 契约。服务端和所有客户端共用同一份定义。

pub mod control;
pub mod daemon;
pub mod desktop;
pub mod files;
pub mod git;
pub mod history;
pub mod workspace;

use std::io::{self, Write};

/// 写一行 JSON：先整体序列化，再一次写出。
///
/// 不要用 `writeln!(stream, "{value}")`：`Value` 的 `Display` 边格式化边写，每个小片段
/// 都是一次 `send`。Linux 上每次 send 占一块 skb，几 KB 的回包就能把 socket 发送缓冲
/// 占满；对端若先写完再读（测试里常见），双方就卡死。
pub fn write_json_line(writer: &mut impl Write, value: &impl serde::Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line)
}
