use super::*;

#[test]
fn approve_is_bare_enter() {
    assert_eq!(action_payload(Some("approve"), None), Ok(b"\r".to_vec()));
}

#[test]
fn deny_is_bare_escape_not_arrow_navigation() {
    // 故意不测"按几次下方向键"——这条路本身就不成立（菜单选项数量不是常数，
    // 见模块注释「远程操控」一节）。Esc 不依赖菜单结构。
    assert_eq!(action_payload(Some("deny"), None), Ok(b"\x1b".to_vec()));
}

#[test]
fn reply_appends_enter_after_text() {
    assert_eq!(
        action_payload(Some("reply"), Some("不用了，换个方式")),
        Ok("不用了，换个方式\r".as_bytes().to_vec())
    );
}

#[test]
fn reply_without_text_is_rejected() {
    assert_eq!(action_payload(Some("reply"), None), Err("需要非空 text"));
    assert_eq!(
        action_payload(Some("reply"), Some("")),
        Err("需要非空 text")
    );
}

#[test]
fn unknown_kind_returns_err() {
    assert_eq!(
        action_payload(Some("do_a_barrel_roll"), None),
        Err("未知 kind")
    );
    assert_eq!(action_payload(None, None), Err("未知 kind"));
}
