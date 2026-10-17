//! `skills.sync_control`：把教智能体使用 `smelt` 命令行的技能写进运行时那台机器的
//! `~/.agents/skills/smelt/SKILL.md`。智能体在运行时上跑，技能也得在那里。内容随版本
//! 走，没变就不写。

const CONTROL_SKILL: &str = include_str!("control_skill.md");

pub fn sync() -> std::io::Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    sync_at(&home)
}

fn sync_at(home: &std::path::Path) -> std::io::Result<()> {
    let dir = home.join(".agents").join("skills").join("smelt");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(CONTROL_SKILL) {
        let staged = dir.join("SKILL.md.next");
        std::fs::write(&staged, CONTROL_SKILL)?;
        std::fs::rename(staged, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_the_skill_once_and_keeps_it_current() {
        let home = tempfile::tempdir().unwrap();
        super::sync_at(home.path()).unwrap();
        let path = home.path().join(".agents/skills/smelt/SKILL.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), super::CONTROL_SKILL);

        std::fs::write(&path, "旧版本").unwrap();
        super::sync_at(home.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), super::CONTROL_SKILL);
    }
}
