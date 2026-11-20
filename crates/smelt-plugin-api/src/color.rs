//! 插件接口里颜色的写法：`#rgb` 或 `#rrggbb`，和 gpui-shell 元素样式（`bg("#rrggbb")`）、`theme()`
//! 给的一样，脚本拿主题色直接传。Rust 这边是 `0xRRGGBB`。JS 组件的颜色参数、菜单栏图标这类线上结构
//! 都经这里转，不各写一份。
//!
//! 只收不透明的：收颜色的这几处（键帽底色、图标色）底下的组件都只认 `0xRRGGBB`，带透明度的报错，
//! 不悄悄丢掉。

/// `#rgb` / `#rrggbb` → `0xRRGGBB`。
pub fn parse_hex_color(text: &str) -> Result<u32, String> {
    let invalid = || format!("`{text}` is not an opaque color; expected #rgb or #rrggbb");
    let hex = text.strip_prefix('#').ok_or_else(invalid)?;
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).map_err(|_| invalid()),
        3 => {
            let value = u32::from_str_radix(hex, 16).map_err(|_| invalid())?;
            let (r, g, b) = ((value >> 8) & 0xf, (value >> 4) & 0xf, value & 0xf);
            Ok((r * 0x11) << 16 | (g * 0x11) << 8 | b * 0x11)
        }
        _ => Err(invalid()),
    }
}

/// `0xRRGGBB` → `#rrggbb`。
pub fn hex_color(color: u32) -> String {
    format!("#{:06x}", color & 0xff_ffff)
}

/// 线上结构里的颜色字段：`#[serde(with = "crate::color::hex")]`，写法同 [`parse_hex_color`]。
pub(crate) mod hex {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(color: &u32, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::hex_color(*color))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::parse_hex_color(&text).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_spellings_scripts_get_from_the_theme() {
        assert_eq!(parse_hex_color("#1a2B3c"), Ok(0x1a2b3c));
        assert_eq!(parse_hex_color("#f80"), Ok(0xff8800));
        assert_eq!(hex_color(0x1a2b3c), "#1a2b3c");
        assert_eq!(hex_color(0x00000f), "#00000f");
    }

    #[test]
    fn refuses_what_it_cannot_draw() {
        for bad in [
            "1a2b3c",
            "#1a2b3c80",
            "#12",
            "#gg0000",
            "#+12345",
            "red",
            "",
        ] {
            assert!(parse_hex_color(bad).is_err(), "{bad:?}");
        }
    }
}
