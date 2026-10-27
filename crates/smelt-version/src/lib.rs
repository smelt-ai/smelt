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
    use super::strip_build;

    #[test]
    fn strip_build_keeps_the_release_part() {
        assert_eq!(strip_build("2026.1027.0-rc.1"), "2026.1027.0-rc.1");
        assert_eq!(strip_build("2026.1027.0-rc.1+g32c3172"), "2026.1027.0-rc.1");
        assert_eq!(strip_build("2026.1027.0+g32c3172"), "2026.1027.0");
    }
}
