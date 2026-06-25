//! smelt：Mac 上的个人知识蒸馏引擎。

mod brief;
mod chat;
mod claude;
mod db;
mod digest;
mod feishu;
mod git;
mod init;
mod install;
mod merge;
mod model;
mod observe;
mod render;
mod scan;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "smelt", about = "个人知识蒸馏引擎", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 交互式首次配置：写 config.toml、可选注册自启
    Init,
    /// 启动后台采集守护进程
    Observe,
    /// 手动触发一次蒸馏
    Digest,
    /// 对已有 instincts 做语义去重合并
    Merge,
    /// 飞书（内部 API，复用本地 session）
    Feishu {
        #[command(subcommand)]
        action: feishu::FeishuAction,
    },
    /// 预览分身发现的最近改动文件（采集范围 / 隐私核对）
    Scan,
    /// 预览从 Claude Code 会话提取到的提问（采集 / 隐私核对）
    Sessions,
    /// 预览从 git 提交提取到的工作流行为（采集核对）
    Git,
    /// 打印当前 instincts
    Show,
    /// 注册 Mac LaunchAgent 开机自启
    Install,
    /// 和你的数字分身对话（基于已蒸馏的 instincts）
    Chat,
    /// 主动简报：分身主动产出「我观察到的你 + 该注意的事」
    Brief,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init => init::run()?,
        Command::Observe => observe::run().await?,
        Command::Digest => digest::run().await?,
        Command::Merge => merge::run().await?,
        Command::Feishu { action } => feishu::run(action).await?,
        Command::Scan => scan::run()?,
        Command::Sessions => claude::run()?,
        Command::Git => git::run()?,
        Command::Show => show()?,
        Command::Install => install::run()?,
        Command::Chat => chat::run().await?,
        Command::Brief => brief::run().await?,
    }
    Ok(())
}

fn show() -> Result<()> {
    let conn = db::open()?;
    let items = db::list_by_confidence(&conn)?;
    if items.is_empty() {
        println!("还没有 instinct。先跑 `smelt digest`。");
        return Ok(());
    }
    for it in items {
        println!(
            "[{:.2}] ({}) {}  <{}> x{}",
            it.confidence,
            it.scope.as_str(),
            it.content,
            it.domain.join(","),
            it.evidence_count
        );
    }
    Ok(())
}
