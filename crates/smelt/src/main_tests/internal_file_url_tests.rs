use super::smelt_file_url_to_target;

#[test]
fn decodes_path_and_optional_line() {
    assert_eq!(
        smelt_file_url_to_target("smelt-file:///tmp/a%20b.rs#L42"),
        Some(("/tmp/a b.rs".into(), Some(42)))
    );
    assert!(smelt_file_url_to_target("https://example.com/a.rs").is_none());
}
