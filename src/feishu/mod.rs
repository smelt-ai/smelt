//! feishu：飞书内部 API 客户端，原生移植自本机 lark_tools(Python)。
//! 走 im/gateway 的 protobuf 协议，用本地 session cookie 以**个人身份只读**消息。
//!
//! 设计红线（务必遵守）：
//! - 消息正文只落本地、**绝不**外发给 LLM；
//! - session 值**绝不**写入日志、stdout 或 global.md；
//! - 仅手动 `smelt feishu` 触发，不进 observe 后台常驻（session 无刷新，过期需人工扫码）。
#![allow(dead_code)] // 子系统渐进开发中，部分 API 在后续阶段接线

pub mod discovery;
pub mod gateway;
pub mod login;
pub mod messages;
pub mod proto;

use anyhow::Result;
use clap::Subcommand;

/// `smelt feishu <action>` 的子动作。
#[derive(Subcommand)]
pub enum FeishuAction {
    /// 扫码登录飞书，获取并保存 session
    Login,
    /// 检查飞书登录态是否有效（探活，不读任何内容）
    Auth,
    /// 列出你最近的活跃会话（只显示名称，不暴露 chatId）
    Chats,
    /// 读取指定会话的最近消息
    Messages {
        /// 会话 ID（chatId）
        chat_id: String,
        /// 读取条数
        #[arg(long, default_value_t = 20)]
        count: u64,
    },
    /// 拉取最近个人消息到本地（阶段 4，正文只落本地）
    Pull,
}

pub async fn run(action: FeishuAction) -> Result<()> {
    // 登录命令负责「创建」session，不能要求已有 session / 租户。
    if let FeishuAction::Login = action {
        return login::run().await;
    }

    let session = gateway::load_session()?;
    gateway::tenant_origin()?; // 入口校验租户配置，未配置时给出明确错误而非误判登录态
    match action {
        FeishuAction::Login => unreachable!("已在上方处理"),
        FeishuAction::Auth => {
            if gateway::check_auth(&session).await {
                println!("✅ 飞书登录态有效");
            } else {
                println!("❌ 飞书登录态无效或已过期，请用 lark_cli.py login 重新扫码");
            }
        }
        FeishuAction::Chats => {
            let convs = discovery::discover(&session).await?;
            println!("发现 {} 个活跃会话：", convs.len());
            for c in &convs {
                let kind = if c.is_group { "群 " } else { "单聊" };
                let name = if c.name.is_empty() {
                    "(单聊，对方姓名待 pull 时解析)".to_string()
                } else {
                    c.name.clone()
                };
                println!("  [{kind}] {name}");
            }
        }
        FeishuAction::Messages { chat_id, count } => {
            let msgs = messages::fetch(&session, &chat_id, count).await?;
            println!("读取到 {} 条消息：", msgs.len());
            for m in &msgs {
                let t = chrono::DateTime::from_timestamp(m.timestamp as i64, 0)
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                println!("[{t}] {}: {}", m.sender_id, m.content);
            }
        }
        FeishuAction::Pull => {
            println!("（阶段 4：自动发现会话 + 落本地库尚未实现）");
        }
    }
    Ok(())
}
