use super::*;

/// grid keyframe 的 hex 字段必须逐字节还原。
#[test]
fn hex_roundtrip() {
    let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    assert_eq!(
        hex_decode(&hex_encode(&data)).as_deref(),
        Some(data.as_slice())
    );
    assert_eq!(hex_decode("").as_deref(), Some(&[][..]));
    assert_eq!(hex_decode("abc"), None, "奇数长度应判非法");
    assert_eq!(hex_decode("zz"), None, "非 hex 字符应判非法");
}

/// 损坏的 hex 字段（多字节 UTF-8）只判非法、绝不 panic。
#[test]
fn hex_decode_never_panics_on_multibyte_utf8() {
    assert_eq!(hex_decode("中文"), None); // 6 字节，偶数，非 hex 字符
    assert_eq!(hex_decode("a中"), None); // 1 + 3 字节，奇偶交叉
    assert_eq!(hex_decode("ab中c"), None);
}

#[test]
fn buf_detects_alt_screen() {
    assert!(buf_looks_like_alt_screen(b"\x1b[?1049hTUI"));
    assert!(!buf_looks_like_alt_screen(b"plain shell"));
}
