//! 终端标题 Braille spinner 判定——GUI 与 smeltd 共用，单独成文件避免 smeltd
//! 为用一个函数去整份编译 `osc.rs`（OscScan 等只服务 workspace）。

/// 标题是否以 Braille spinner（U+2801–U+28FF，盲文块非空白帧）开头——终端协议约定，
/// 任何遵守此约定的 agent（Claude Code 等）都能被识别，不是某家私有格式。
pub fn title_starts_with_spinner(title: &str) -> bool {
    title
        .chars()
        .next()
        .is_some_and(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_starts_with_spinner_matches_braille_range() {
        assert!(title_starts_with_spinner("⠋ doing something"));
        assert!(!title_starts_with_spinner("plain title"));
        assert!(!title_starts_with_spinner(""));
    }
}
