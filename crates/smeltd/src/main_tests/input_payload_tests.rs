use super::*;

#[test]
fn data_string_becomes_utf8_bytes() {
    let v = serde_json::json!({ "data": "hello" });
    assert_eq!(input_payload(&v), Some(b"hello".to_vec()));
}

#[test]
fn control_chars_in_json_string_work() {
    // Ctrl+C = \u0003；xterm onData + JSON.stringify 就是这条路
    let v = serde_json::json!({ "data": "" });
    assert_eq!(input_payload(&v), Some(vec![0x03]));
}

#[test]
fn empty_or_missing_data_is_none() {
    assert_eq!(input_payload(&serde_json::json!({ "data": "" })), None);
    assert_eq!(input_payload(&serde_json::json!({})), None);
    assert_eq!(input_payload(&serde_json::json!({ "data": null })), None);
}
