//! 教智能体用 `smelt` 命令行的技能现在随 smelt-pi 发布（`packages/pi-agent/skills/smelt`），
//! 智能体勾选了才传给 Pi。以前每次启动都把它写进 `~/.agents/skills/smelt`，那是 Pi、
//! Codex、Cursor 等共用的发现目录，等于替用户全局装上了。这里把那份旧副本清掉。
//!
//! 只删我们写过的：`SKILL.md` 的内容得是历史上某一版，目录里也没有别的东西。用户改过的
//! 留着。技能插件替 Claude Code 补的那条指向它的软链一起删，否则会悬空。

use std::path::Path;

use sha2::{Digest, Sha256};

/// 历史上写进 `~/.agents/skills/smelt/SKILL.md` 的每一版内容的 SHA-256。
const LEGACY_SKILL_SHA256: &[&str] = &[
    "0eee6dfe2ce2ce0ffa844302d6fe2eb021a5998b1510eba9e9d0174c03373d95",
    "39969ae0bcaf4406ab5cd1d57e064d7a960ee7242b5cd7456cbf8582ef4c11db",
    "646e5a8556f78268387beb18dce89ed8b16cac3d1ca03a9c9466b433267ab22c",
    "7d572642a3c6ee613e4c1241792e7d06f511fa8cee9972c8a235414cdd49c76e",
    "8dcf5b8ffc4f822b42f195b9e0aa089cf81172e96f5cc45850836c69e02fd283",
    "bc319747224e4cc33e9a541505d710c174da5c82706736b3dd4bb22d2ac1597f",
    "e0cd5c0183b1d134dabb1bb044d4ff214bc6c76a83a80f51e2ec7ce4d1e21be2",
    "e23c5e2dcab7737efc758e708fd1eb615f1b57471fe3a84dfc22dc3576e51af0",
];

pub fn remove_legacy_copy() {
    if let Some(home) = dirs::home_dir() {
        remove_legacy_copy_at(&home, LEGACY_SKILL_SHA256);
    }
}

fn remove_legacy_copy_at(home: &Path, ours: &[&str]) {
    let dir = home.join(".agents").join("skills").join("smelt");
    if !is_ours(&dir, ours) {
        return;
    }
    let bridge = home.join(".claude").join("skills").join("smelt");
    if std::fs::read_link(&bridge).is_ok_and(|target| target == dir) {
        let _ = std::fs::remove_file(&bridge);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn is_ours(dir: &Path, ours: &[&str]) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if !meta.is_dir() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    // 写的时候先落 `SKILL.md.next` 再改名，中断了会留下它。
    let only_ours = entries.flatten().all(|entry| {
        matches!(
            entry.file_name().to_str(),
            Some("SKILL.md" | "SKILL.md.next")
        )
    });
    let Ok(contents) = std::fs::read(dir.join("SKILL.md")) else {
        return false;
    };
    only_ours && ours.contains(&sha256(&contents).as_str())
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITTEN: &str = "---\nname: smelt\n---\n我们写过的一版\n";

    fn remove(home: &Path) {
        remove_legacy_copy_at(home, &[sha256(WRITTEN.as_bytes()).as_str()]);
    }

    fn legacy_copy(home: &Path, contents: &str) -> std::path::PathBuf {
        let dir = home.join(".agents/skills/smelt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), contents).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn removes_the_copy_we_wrote_and_its_claude_bridge() {
        let home = tempfile::tempdir().unwrap();
        let dir = legacy_copy(home.path(), WRITTEN);
        let bridge = home.path().join(".claude/skills/smelt");
        std::fs::create_dir_all(bridge.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&dir, &bridge).unwrap();

        remove(home.path());

        assert!(!dir.exists());
        assert!(std::fs::symlink_metadata(&bridge).is_err());
    }

    #[test]
    fn keeps_a_copy_the_user_changed() {
        let home = tempfile::tempdir().unwrap();
        let dir = legacy_copy(home.path(), "用户自己改过");
        remove(home.path());
        assert!(dir.join("SKILL.md").is_file());

        // 内容是我们的，但用户往目录里放了别的东西，也留着。
        std::fs::write(dir.join("SKILL.md"), WRITTEN).unwrap();
        std::fs::write(dir.join("notes.md"), "用户的").unwrap();
        remove(home.path());
        assert!(dir.join("notes.md").is_file());
    }
}
