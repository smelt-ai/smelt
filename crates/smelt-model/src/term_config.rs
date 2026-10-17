//! alacritty `Term` 的解析语义配置 —— GUI 与 smeltd 共用。
//!
//! smeltd 持有终端的权威 `Term`，GUI 用 keyframe 重建一份副本后继续解析同一串 PTY
//! 字节。两边对同一字节流的**解析语义**必须一致，否则 daemon 这份「真相」本身就是
//! 错的、keyframe 也无从还原。历史教训：GUI 开了 `kitty_keyboard`、daemon 没开，
//! daemon 的 Term 把 `CSI > 1 u` 静默丢掉，重连后 Shift+Enter 退化成裸回车。
//!
//! 只放影响解析结果的项；滚动历史长度、断词字符等各端自己的取舍在此基础上覆盖。

use alacritty_terminal::term::Config;

/// 两端共同的解析语义。调用方用 `..emulation_config()` 在其上覆盖本端专属项。
pub fn emulation_config() -> Config {
    Config {
        // 默认 false 时 alacritty 在 push_keyboard_mode 里直接 return，
        // DISAMBIGUATE_ESC_CODES 永远置不上。
        kitty_keyboard: true,
        ..Config::default()
    }
}
