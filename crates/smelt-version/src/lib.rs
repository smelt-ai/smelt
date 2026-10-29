//! Smelt 的版本号。不写在代码里：打包脚本从 git tag 算好（`scripts/version.sh`），经环境变量
//! `SMELT_BUILD_VERSION` 在编译时注入，规则见 docs/release.md 的「版本号」。没注入的是开发构建。

/// 完整版本号，显示和上报用。出自发版 tag 的构建就是 tag 去掉 `v`，如 `2026.1027.0-rc.1`；
/// tag 之后的提交带构建信息，如 `2026.1027.0-rc.1+g32c3172`（`32c3172` 是那个提交）。
pub const VERSION: &str = match option_env!("SMELT_BUILD_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => "0.0.0-dev",
};

/// 这个构建出自哪次发版：[`VERSION`] 去掉 `+` 后的构建信息，GitHub 上就有 `v<它>` 这个 Release。
/// 远程运行时升级按它下载。开发构建没有对应的发版，是 `None`。
pub const RELEASE: Option<&str> = match option_env!("SMELT_BUILD_VERSION") {
    Some(version) if !version.is_empty() => Some(strip_build(version)),
    _ => None,
};

/// 从发版的校验和清单（`SHA256SUMS`，`sha256sum` 的输出）里找出这个文件的校验和。
/// 每次发版的产物都列在同一个 Release 的这份清单里，远程运行时升级和桌面更新都对着它校验。
pub fn checksum_for<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (sum, name) = line.split_once(char::is_whitespace)?;
        // `sha256sum` 二进制模式在文件名前加 `*`。
        let name = name.trim_start().trim_start_matches('*');
        (name == file && sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(sum)
    })
}

const fn strip_build(version: &str) -> &str {
    let bytes = version.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            return version.split_at(i).0;
        }
        i += 1;
    }
    version
}

#[cfg(test)]
mod tests {
    use super::{checksum_for, strip_build};

    #[test]
    fn strip_build_keeps_the_release_part() {
        assert_eq!(strip_build("2026.1027.0-rc.1"), "2026.1027.0-rc.1");
        assert_eq!(strip_build("2026.1027.0-rc.1+g32c3172"), "2026.1027.0-rc.1");
        assert_eq!(strip_build("2026.1027.0+g32c3172"), "2026.1027.0");
    }

    #[test]
    fn finds_the_checksum_line_for_exactly_this_file() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let sums = format!(
            "{a}  smeltd-0.9.1-linux-aarch64.tar.gz\n{b} *smeltd-0.9.1-linux-x86_64.tar.gz\n"
        );
        assert_eq!(
            checksum_for(&sums, "smeltd-0.9.1-linux-x86_64.tar.gz"),
            Some(b.as_str())
        );
        assert_eq!(
            checksum_for(&sums, "smeltd-0.9.1-linux-aarch64.tar.gz"),
            Some(a.as_str())
        );
        assert_eq!(checksum_for(&sums, "smeltd-0.9.1-linux-x86.tar.gz"), None);
        assert_eq!(
            checksum_for("short  smeltd.tar.gz\n", "smeltd.tar.gz"),
            None
        );
    }
}
