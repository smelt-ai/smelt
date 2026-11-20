//! JS 插件自己的文案：插件目录下的 `locales/ui.yml`，格式和 Rust crate 的一样（见
//! `docs/i18n.md`），用 rust-i18n 构建时同一个加载函数读。插件里 `t(键, 参数)` 按当前界面
//! 语言取，没有就取英文，再没有给键本身（和 `t!` 一样）。

use std::collections::BTreeMap;
use std::path::Path;

use gpui_shell::HostValue;

/// 语言代码到「键到文案」。
#[derive(Debug, Default)]
pub(super) struct Catalog(BTreeMap<String, BTreeMap<String, String>>);

/// 文案缺语言时用的语言，同各 crate 的 `i18n!(fallback = "en")`。
const FALLBACK: &str = "en";

impl Catalog {
    /// 读插件根目录下的 `locales/`；没有这个目录是空表。
    pub(super) fn load(root: &Path) -> Result<Self, String> {
        let dir = root.join("locales");
        if !dir.is_dir() {
            return Ok(Self::default());
        }
        rust_i18n_support::try_load_locales(&dir.to_string_lossy(), |_| false, true).map(Self)
    }

    /// `key` 在 `locale` 下的文案，插值按 `params` 换好。
    pub(super) fn text(&self, locale: &str, key: &str, params: &[(String, HostValue)]) -> String {
        let text = [locale, FALLBACK]
            .into_iter()
            .find_map(|locale| self.0.get(locale)?.get(key));
        match text {
            Some(text) => interpolate(text, params),
            None => key.to_string(),
        }
    }
}

/// 把 `%{name}` 换成参数里同名的值；参数里没有的原样留着（同 `t!`）。
fn interpolate(text: &str, params: &[(String, HostValue)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("%{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        match params.iter().find(|(key, _)| key == name) {
            Some((_, value)) => out.push_str(&display(value)),
            None => out.push_str(&rest[start..start + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// 参数在文案里的样子：数字照 JS 的写法（整数不带 `.0`）。
fn display(value: &HostValue) -> String {
    match value {
        HostValue::Null => "null".into(),
        HostValue::Bool(value) => value.to_string(),
        HostValue::Number(number) if number.fract() == 0. && number.abs() < 1e15 => {
            format!("{}", *number as i64)
        }
        HostValue::Number(number) => number.to_string(),
        HostValue::Str(text) => text.clone(),
        HostValue::Array(_) | HostValue::Object(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试各用一个空目录，用完删掉。
    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("smelt-catalog-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn catalog(name: &str) -> (Dir, Catalog) {
        let dir = Dir::new(name);
        std::fs::create_dir(dir.path().join("locales")).unwrap();
        std::fs::write(
            dir.path().join("locales/ui.yml"),
            r#"_version: 2
row:
  count:
    en: "%{count} sessions in %{name}"
    zh-CN: "%{name} 里有 %{count} 段会话"
  only_en:
    en: "English only"
"#,
        )
        .unwrap();
        let catalog = Catalog::load(dir.path()).unwrap();
        (dir, catalog)
    }

    #[test]
    fn texts_follow_the_locale_and_fall_back_to_english_then_the_key() {
        let (_dir, catalog) = catalog("locale");
        let params = [
            ("count".to_string(), HostValue::Number(3.)),
            ("name".to_string(), HostValue::from("smelt")),
        ];
        assert_eq!(
            catalog.text("zh-CN", "row.count", &params),
            "smelt 里有 3 段会话"
        );
        assert_eq!(
            catalog.text("en", "row.count", &params),
            "3 sessions in smelt"
        );
        assert_eq!(catalog.text("zh-CN", "row.only_en", &[]), "English only");
        assert_eq!(catalog.text("zh-CN", "row.missing", &[]), "row.missing");
    }

    #[test]
    fn missing_parameters_stay_as_written() {
        let (_dir, catalog) = catalog("missing");
        let params = [("count".to_string(), HostValue::Number(1.5))];
        assert_eq!(
            catalog.text("en", "row.count", &params),
            "1.5 sessions in %{name}"
        );
    }

    #[test]
    fn a_plugin_without_locales_has_an_empty_catalog() {
        let dir = Dir::new("empty");
        let catalog = Catalog::load(dir.path()).unwrap();
        assert_eq!(catalog.text("en", "a.b", &[]), "a.b");
    }
}
