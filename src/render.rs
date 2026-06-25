//! render：把 DB 中的 instincts 渲染为 markdown，供 Claude Code 读取。
//! 全局 → ~/.smelt/global.md；项目级 → ~/.smelt/projects/<name>/instincts.md。

use crate::db;
use crate::model::{Instinct, Scope};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 渲染一组 instinct 为 markdown。
fn render_md(title: &str, items: &[Instinct]) -> String {
    let mut md = format!(
        "# {title}\n\n> 本文件由 smelt 自动生成，请勿手动编辑。\n\n"
    );
    if items.is_empty() {
        md.push_str("_（暂无）_\n");
    } else {
        for it in items {
            md.push_str(&format!("- **[{:.2}]** {}", it.confidence, it.content));
            if !it.domain.is_empty() {
                md.push_str(&format!(" `{}`", it.domain.join("/")));
            }
            md.push_str(&format!(" _(×{})_\n", it.evidence_count));
        }
    }
    md
}

/// 渲染所有 Global 作用域的 instinct 到 ~/.smelt/global.md。
pub fn write_global() -> Result<PathBuf> {
    let conn = db::open()?;
    let items = db::list_by_confidence(&conn)?;
    let globals: Vec<Instinct> = items
        .into_iter()
        .filter(|it| it.scope == Scope::Global)
        .collect();

    let md = render_md("Smelt Instincts", &globals);
    let path = db::smelt_dir()?.join("global.md");
    std::fs::write(&path, md)?;
    Ok(path)
}

/// 渲染各项目的 Project 作用域 instinct 到 ~/.smelt/projects/<name>/instincts.md。
pub fn write_projects() -> Result<Vec<PathBuf>> {
    let conn = db::open()?;
    let items = db::list_by_confidence(&conn)?;

    // 按项目分组（仅 Project 作用域且带 project 标识的）。
    let mut by_project: BTreeMap<String, Vec<Instinct>> = BTreeMap::new();
    for it in items {
        if it.scope == Scope::Project {
            if let Some(p) = it.project.clone() {
                by_project.entry(p).or_default().push(it);
            }
        }
    }

    let base = db::smelt_dir()?.join("projects");
    let mut paths = Vec::new();
    for (project, insts) in by_project {
        let dir = base.join(sanitize_name(&project));
        std::fs::create_dir_all(&dir)?;
        let md = render_md(&format!("Smelt · {project}"), &insts);
        let path = dir.join("instincts.md");
        std::fs::write(&path, md)?;
        paths.push(path);
    }
    Ok(paths)
}

/// 把项目名清理成安全的目录名。
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
