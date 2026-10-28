use super::{PathStyle, ProjectGroup, disambiguate_labels, project_name_for_cwd, project_root_of};

fn group(root: &str, label: &str) -> ProjectGroup {
    ProjectGroup {
        runtime: crate::RuntimeKey::Local,
        root: root.into(),
        label: label.into(),
        sessions: Vec::new(),
    }
}

/// 跑一遍消歧，取回显示名（base 就是各组当前的 label）。
fn labels(groups: Vec<ProjectGroup>) -> Vec<String> {
    labels_in(PathStyle::Posix, groups)
}

fn labels_in(style: PathStyle, mut groups: Vec<ProjectGroup>) -> Vec<String> {
    disambiguate_labels(style, &mut groups);
    groups.into_iter().map(|g| g.label).collect()
}

/// 项目名是目录末段。Windows 运行时报来的路径用 `\` 分隔，按 `/` 切会把整条路径当成名字。
#[test]
fn project_name_is_the_last_segment_on_the_runtime() {
    assert_eq!(project_name_for_cwd(PathStyle::Posix, "/a/smelt/"), "smelt");
    assert_eq!(
        project_name_for_cwd(PathStyle::Windows, r"C:\Users\w\code\xunxian-reverse"),
        "xunxian-reverse"
    );
    assert_eq!(project_name_for_cwd(PathStyle::Posix, "/"), "项目");
    assert_eq!(project_name_for_cwd(PathStyle::Windows, r"C:\"), "项目");
}

/// Windows 项目重名时同样往前补父目录段，段之间用 `\` 连。
#[test]
fn windows_duplicate_labels_get_parent_segments() {
    assert_eq!(
        labels_in(
            PathStyle::Windows,
            vec![
                group(r"C:\x\dev\smelt", "smelt"),
                group(r"D:\y\dev\smelt", "smelt")
            ]
        ),
        vec![r"x\dev · smelt", r"y\dev · smelt"]
    );
}

/// 不重名就别乱加前缀（大多数情况该保持干净的目录名）。
#[test]
fn unique_labels_are_left_alone() {
    assert_eq!(
        labels(vec![group("/a/smelt", "smelt"), group("/a/other", "other")]),
        vec!["smelt", "other"]
    );
}

/// 末段同名的两个项目必须区分得开——否则侧栏并排两个一模一样的「smelt」，
/// 用户根本分不清哪个是哪个。
#[test]
fn duplicate_labels_get_parent_segments() {
    assert_eq!(
        labels(vec![
            group("/x/dev/smelt", "smelt"),
            group("/y/work/smelt", "smelt")
        ]),
        vec!["dev · smelt", "work · smelt"]
    );
}

/// 补一段还撞车就继续往上补，直到分开。
#[test]
fn keeps_climbing_until_unique() {
    assert_eq!(
        labels(vec![
            group("/a/dev/smelt", "smelt"),
            group("/b/dev/smelt", "smelt")
        ]),
        vec!["a/dev · smelt", "b/dev · smelt"]
    );
}

/// 路径补到顶还重名（真·同路径）→ 必须收敛退出，不能在循环里空转。
#[test]
fn gives_up_instead_of_looping_forever() {
    assert_eq!(
        labels(vec![group("/smelt", "smelt"), group("/smelt", "smelt")]),
        vec!["smelt", "smelt"]
    );
}

fn projects(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// cwd 就是项目根、或落在项目根之下，都该归这个项目。
#[test]
fn cwd_belongs_to_its_project_root() {
    let p = projects(&["/Users/me/dev/smelt"]);
    assert_eq!(
        project_root_of(PathStyle::Posix, &p, "/Users/me/dev/smelt").as_deref(),
        Some("/Users/me/dev/smelt")
    );
    assert_eq!(
        project_root_of(PathStyle::Posix, &p, "/Users/me/dev/smelt/crates/smeltd").as_deref(),
        Some("/Users/me/dev/smelt")
    );
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/Users/me/dev/other"), None);
    assert_eq!(project_root_of(PathStyle::Posix, &p, ""), None);
}

/// 前缀必须卡在完整路径段上：`/a/smelt-old` 不是 `/a/smelt` 的子目录，
/// 否则名字相近的两个项目会互相吞会话。
#[test]
fn prefix_must_be_a_whole_path_segment() {
    let p = projects(&["/a/smelt"]);
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/smelt-old"), None);
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/smeltd"), None);
    assert_eq!(
        project_root_of(PathStyle::Posix, &p, "/a/smelt/sub").as_deref(),
        Some("/a/smelt")
    );
}

/// 父子项目都打开着时，会话归最深的那个（不然子项目永远空着）。
#[test]
fn deepest_matching_project_wins() {
    let p = projects(&["/a", "/a/b", "/a/b/c"]);
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/c/x").as_deref(), Some("/a/b/c"));
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/x").as_deref(), Some("/a/b"));
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/x").as_deref(), Some("/a"));
}

/// Windows 运行时的会话开在项目子目录里也归这个项目：按 `\` 切、不分大小写。
#[test]
fn windows_cwd_belongs_to_its_project_root() {
    let p = projects(&[r"C:\Users\w\code\smelt"]);
    assert_eq!(
        project_root_of(PathStyle::Windows, &p, r"C:\Users\w\code\smelt\crates").as_deref(),
        Some(r"C:\Users\w\code\smelt")
    );
    assert_eq!(
        project_root_of(PathStyle::Windows, &p, r"c:\users\w\code\SMELT").as_deref(),
        Some(r"C:\Users\w\code\smelt")
    );
    assert_eq!(
        project_root_of(PathStyle::Windows, &p, r"C:\Users\w\code\smelt-old"),
        None
    );
}

/// 结尾斜杠是路径写法差异，不该影响归属判定。
#[test]
fn trailing_slashes_are_ignored() {
    let p = projects(&["/a/b/"]);
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b").as_deref(), Some("/a/b"));
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/").as_deref(), Some("/a/b"));
    assert_eq!(project_root_of(PathStyle::Posix, &p, "/a/b/c").as_deref(), Some("/a/b"));
}

/// 面板（变更、文件树）跟着选中的项目走，舞台上的会话可能属于别的项目。
/// 评论只能交给这个项目里的会话，不能写进别的项目的终端或对话。
#[test]
fn panel_text_goes_only_to_a_session_of_the_panel_project() {
    use crate::workspace_sessions::panel_send_target;

    assert_eq!(panel_send_target(Some(2), Some(("smelt", &[1, 2]))), Ok(2));
    let refused = panel_send_target(Some(0), Some(("smelt", &[1, 2]))).unwrap_err();
    assert!(refused.contains("smelt"), "{refused}");
    assert!(
        panel_send_target(Some(0), Some(("空项目", &[]))).is_err(),
        "项目里还没有会话时也不能落到别处"
    );
    assert!(panel_send_target(None, Some(("smelt", &[1]))).is_err());
    assert_eq!(panel_send_target(Some(3), None), Ok(3));
}

#[test]
fn only_closing_a_running_session_asks_first() {
    use crate::workspace_sessions::closing_needs_confirm;

    assert!(closing_needs_confirm(crate::AgentStatus::Running));
    assert!(!closing_needs_confirm(crate::AgentStatus::Idle));
    assert!(!closing_needs_confirm(crate::AgentStatus::NeedsYou));
}
