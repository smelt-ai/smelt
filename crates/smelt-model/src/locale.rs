//! 界面语言代码：App 带哪几种语言、系统给的语言标签归到哪一种。桌面和手机共用；当前语言
//! 是 rust-i18n 的全局值，由各端自己设（桌面见 `smelt_plugin_api::set_locale`）。

/// App 带的界面语言：(语言代码, 这种语言自己的叫法)。叫法不翻译，用户在哪种语言下都认得出
/// 自己的语言。第一项是文案缺了时回落的语言。
pub const LOCALES: &[(&str, &str)] = &[("en", "English"), ("zh-CN", "简体中文")];

/// 文案缺了时用的语言。
pub const FALLBACK_LOCALE: &str = "en";

/// 把 BCP 47 语言标签（`zh-Hans-CN`、`zh_CN`、`en-US`……）归到 [`LOCALES`] 里的一个代码。
/// 中文都归到简体中文：现在只带这一种中文，繁体读者看简体比看英文近。别的语言是英文。
pub fn supported_locale(tag: &str) -> &'static str {
    let tag = tag.trim();
    if let Some((code, _)) = LOCALES
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(tag))
    {
        return code;
    }
    let language = tag.split(['-', '_']).next().unwrap_or_default();
    if language.eq_ignore_ascii_case("zh") {
        "zh-CN"
    } else {
        FALLBACK_LOCALE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_fold_onto_bundled_locales() {
        assert_eq!(supported_locale("zh-CN"), "zh-CN");
        assert_eq!(supported_locale("zh-cn"), "zh-CN");
        assert_eq!(supported_locale("zh-Hans-CN"), "zh-CN");
        assert_eq!(supported_locale("zh_TW"), "zh-CN");
        assert_eq!(supported_locale("en"), "en");
        assert_eq!(supported_locale("en-GB"), "en");
        assert_eq!(supported_locale("fr-FR"), "en");
        assert_eq!(supported_locale(""), "en");
    }
}
