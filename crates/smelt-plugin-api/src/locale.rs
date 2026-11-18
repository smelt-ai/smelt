//! 界面语言：现在用哪种语言、换语言，以及登记给宿主、跟着语言变的字（[`Text`]）。
//!
//! 文案归各自的 crate：每个插件在自己的 `locales/ui.yml` 里写，crate 根上
//! `rust_i18n::i18n!("locales", fallback = "en");`，代码里用 `rust_i18n::t!` 取。当前语言是
//! rust-i18n 里的一个全局值，所有 crate 共用，由 [`set_locale`] 设。做法见 `docs/i18n.md`。
//!
//! 换语言后宿主发 [`HostEvent::LocaleChanged`] 并重画所有窗口。render 里现取 `t!` 的字跟着
//! 就变；存在视图状态里的字（建好的菜单、列表行）要在这条事件里重建。

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{App, SharedString};

use crate::HostEvent;

pub use smelt_model::locale::{FALLBACK_LOCALE, LOCALES, supported_locale};

/// 现在的界面语言，[`LOCALES`] 里的一个代码。只读内存，render 里可以调。
pub fn locale() -> SharedString {
    SharedString::from(rust_i18n::locale().to_string())
}

/// 换界面语言。不认识的代码按 [`supported_locale`] 归到带的语言上；和现在一样什么都不做。
/// 换了发 [`HostEvent::LocaleChanged`]，重画所有窗口。
pub fn set_locale(locale: &str, cx: &mut App) {
    let locale = supported_locale(locale);
    if *rust_i18n::locale() == *locale {
        return;
    }
    rust_i18n::set_locale(locale);
    crate::emit(HostEvent::LocaleChanged, cx);
    cx.refresh_windows();
}

/// 系统的首选语言归到带的语言上；读不到是 [`FALLBACK_LOCALE`]。
pub fn system_locale() -> &'static str {
    sys_locale::get_locale().map_or(FALLBACK_LOCALE, |tag| supported_locale(&tag))
}

/// 给人看、跟着界面语言变的一段字：登记视图、命令、页、菜单项时给的标题。
///
/// 宿主在要显示时才取（[`Text::get`]），所以换了语言不用重新登记。Rust 插件用 [`text!`]
/// 从自己的文案里取；JS 插件给一张「语言代码 → 字」的表；不用翻译的（人名、产品名）直接
/// 给字符串。
///
/// 相等按来源比：同一个 crate 的同一个键、同一张表、同样的固定字算相等，带插值的只有同一份
/// 才相等。按此刻的字比，来源不同、此刻碰巧一样的两条会被当成相等，换了语言就不对了。
/// 调试输出是此刻语言下的字。
#[derive(Clone)]
pub struct Text(Repr);

#[derive(Clone)]
enum Repr {
    Fixed(SharedString),
    /// 一个键，和取它的 crate 的查表函数（[`text!`] 生成，换 crate 就是另一个函数）。
    Keyed {
        key: &'static str,
        lookup: fn(&'static str) -> SharedString,
    },
    Localized(Arc<dyn Fn() -> SharedString + Send + Sync>),
    Translations(Arc<[(SharedString, SharedString)]>),
}

impl Text {
    /// 取时用 `lookup` 查 `key`，一般由 [`text!`] 生成。
    pub fn keyed(key: &'static str, lookup: fn(&'static str) -> SharedString) -> Self {
        Self(Repr::Keyed { key, lookup })
    }

    /// 每次取时现算，一般由带插值的 [`text!`] 生成。
    pub fn localized<S: Into<SharedString>>(
        lookup: impl Fn() -> S + Send + Sync + 'static,
    ) -> Self {
        Self(Repr::Localized(Arc::new(move || lookup().into())))
    }

    /// 一张「语言代码 → 字」的表（JS 插件给的）。取时先找当前语言，没有找
    /// [`FALLBACK_LOCALE`]，再没有用第一项；空表是空字符串。
    pub fn translations(entries: impl IntoIterator<Item = (SharedString, SharedString)>) -> Self {
        Self(Repr::Translations(entries.into_iter().collect()))
    }

    /// 此刻语言下的字。只读内存，render 里可以调。
    pub fn get(&self) -> SharedString {
        match &self.0 {
            Repr::Fixed(text) => text.clone(),
            Repr::Keyed { key, lookup } => lookup(key),
            Repr::Localized(lookup) => lookup(),
            Repr::Translations(entries) => {
                let locale = rust_i18n::locale();
                let pick = |code: &str| {
                    entries
                        .iter()
                        .find(|(locale, _)| locale.eq_ignore_ascii_case(code))
                        .map(|(_, text)| text.clone())
                };
                pick(&locale)
                    .or_else(|| pick(FALLBACK_LOCALE))
                    .or_else(|| entries.first().map(|(_, text)| text.clone()))
                    .unwrap_or_default()
            }
        }
    }
}

impl Default for Text {
    fn default() -> Self {
        Self(Repr::Fixed(SharedString::default()))
    }
}

impl From<&'static str> for Text {
    fn from(text: &'static str) -> Self {
        Self(Repr::Fixed(text.into()))
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self(Repr::Fixed(text.into()))
    }
}

impl From<SharedString> for Text {
    fn from(text: SharedString) -> Self {
        Self(Repr::Fixed(text))
    }
}

impl From<Cow<'static, str>> for Text {
    fn from(text: Cow<'static, str>) -> Self {
        Self(Repr::Fixed(text.into()))
    }
}

impl std::fmt::Debug for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.get(), f)
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.get())
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Fixed(a), Repr::Fixed(b)) => a == b,
            (
                Repr::Keyed { key, lookup },
                Repr::Keyed {
                    key: other_key,
                    lookup: other_lookup,
                },
            ) => key == other_key && std::ptr::fn_addr_eq(*lookup, *other_lookup),
            (Repr::Localized(a), Repr::Localized(b)) => Arc::ptr_eq(a, b),
            (Repr::Translations(a), Repr::Translations(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Text {}

/// 从调用处所在 crate 的文案里取字，生成跟着语言变的 [`Text`]。参数同 `rust_i18n::t!`：
/// `text!("sessions.title")`、`text!("greeting", name = name)`。键要是字面量；插值的值被
/// 闭包带走，要能跨线程共享。调用的 crate 要依赖 `rust-i18n` 并在根上
/// `rust_i18n::i18n!(…)`。
#[macro_export]
macro_rules! text {
    ($key:literal $(,)?) => {
        $crate::Text::keyed($key, |key| ::rust_i18n::t!(key).into())
    };
    ($key:literal, $($args:tt)+) => {
        $crate::Text::localized(move || ::rust_i18n::t!($key, $($args)+))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translations_fall_back_to_english_then_first() {
        let both = Text::translations([
            ("zh-CN".into(), "技能".into()),
            ("en".into(), "Skills".into()),
        ]);
        let only_french = Text::translations([("fr".into(), "Compétences".into())]);
        let empty = Text::translations([]);
        // 不碰全局语言（别的测试并行在跑），只看回落：此刻语言没有就用英文。
        let current = rust_i18n::locale().to_string();
        if current == "en" {
            assert_eq!(both.get(), "Skills");
        }
        assert_eq!(only_french.get(), "Compétences");
        assert_eq!(empty.get(), "");
        assert_eq!(Text::from("Pi").get(), "Pi");
    }

    #[test]
    fn equality_follows_the_source_not_the_current_words() {
        fn upper(key: &'static str) -> SharedString {
            key.to_uppercase().into()
        }
        fn lower(key: &'static str) -> SharedString {
            key.into()
        }
        assert_eq!(Text::keyed("a", upper), Text::keyed("a", upper));
        assert_ne!(Text::keyed("a", upper), Text::keyed("b", upper));
        assert_ne!(Text::keyed("a", upper), Text::keyed("a", lower));
        // 此刻的字一样，来源不同也不相等：换语言后会分开。
        assert_eq!(Text::keyed("A", lower).get(), Text::from("A").get());
        assert_ne!(Text::keyed("A", lower), Text::from("A"));
        let interpolated = Text::localized(|| "x");
        assert_eq!(interpolated, interpolated.clone());
        assert_ne!(interpolated, Text::localized(|| "x"));
        assert_eq!(Text::from("x"), Text::from(String::from("x")));
    }
}
