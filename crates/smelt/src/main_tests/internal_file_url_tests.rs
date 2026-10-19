use super::{InternalFileLink, smelt_file_url_to_target};

#[test]
fn decodes_path_and_optional_line() {
    assert_eq!(
        smelt_file_url_to_target("smelt-file:///tmp/a%20b.rs#L42"),
        Some(InternalFileLink {
            runtime: None,
            path: "/tmp/a b.rs".into(),
            line: Some(42),
        })
    );
    assert!(smelt_file_url_to_target("https://example.com/a.rs").is_none());
}

/// 远程对话里的文件链接带着运行时 id，不能当成本机路径打开。
#[test]
fn remote_file_links_keep_their_runtime() {
    assert_eq!(
        smelt_file_url_to_target("smelt-file://39fafd41-9942-49b0-b9ec-7eb670d2711e/etc/hosts#L3"),
        Some(InternalFileLink {
            runtime: Some("39fafd41-9942-49b0-b9ec-7eb670d2711e".into()),
            path: "/etc/hosts".into(),
            line: Some(3),
        })
    );
}
