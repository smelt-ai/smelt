//! brief：主动简报。分身综合画像 + 当前活动快照（git 未推送 / 冷落项目 / 最近文件），
//! 主动产出一份「我观察到的你 + 今天该注意的事」，而不是等你来问。

use crate::db;
use crate::digest;
use crate::git;
use crate::model::Scope;
use crate::scan;
use anyhow::Result;

pub async fn run() -> Result<()> {
    let key = digest::api_key()?;

    // ① 画像：取高置信的全局习惯作为「我是谁」。
    let conn = db::open()?;
    let instincts = db::list_by_confidence(&conn)?;
    let profile = instincts
        .iter()
        .filter(|it| it.scope == Scope::Global)
        .take(8)
        .map(|it| format!("- {}", it.content))
        .collect::<Vec<_>>()
        .join("\n");

    // ② git 硬信号：未推送、最后提交多久前。
    let mut git_text = String::new();
    for s in git::repo_signals() {
        let branch = s.branch.as_deref().unwrap_or("?");
        let last = s.last_commit.as_deref().unwrap_or("未知");
        git_text.push_str(&format!("- {}（{branch}）：最后提交 {last}", s.name));
        if s.unpushed > 0 {
            git_text.push_str(&format!("，有 {} 个提交未推送", s.unpushed));
        }
        git_text.push('\n');
    }
    if git_text.is_empty() {
        git_text.push_str("（无活跃仓库）\n");
    }

    // ③ 最近文件活动（可选）。
    let scan_text = scan::recent_files_report().ok().flatten().unwrap_or_default();

    if profile.is_empty() {
        println!("还没有画像，先跑 `smelt digest` 提炼一次再来。");
        return Ok(());
    }

    let prompt = format!(
        "你是 TA 的数字分身。请基于下面 TA 的画像和当前活动快照，**主动**写一份简短的简报给 TA。\n\
         要求：\n\
         1. 开头一句话点出我最近在专注什么。\n\
         2. 然后列 2-4 条**值得我注意的事**——优先具体可行动的（未推送的提交、好久没碰的项目、\
         看起来活跃但可能被我遗漏的线索）。\n\
         用第一人称、像我在对自己说话，简洁直接、不要客套和套话。\n\n\
         === 我的画像 ===\n{profile}\n\n\
         === git 状态 ===\n{git_text}\n\
         === 最近改动的文件 ===\n{scan_text}"
    );

    let brief = digest::chat(&key, &prompt).await?;
    println!("{brief}");
    Ok(())
}
