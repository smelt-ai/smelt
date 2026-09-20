use super::{
    AcpSaved, PaneState, ProjectGroup, SessionState, SplitAxis, disambiguate_labels,
    project_root_of, remove_projects_under, session_state_cwd,
};

fn group(root: &str, label: &str) -> ProjectGroup {
    ProjectGroup {
        root: root.into(),
        label: label.into(),
        sessions: Vec::new(),
    }
}

/// 跑一遍消歧，取回显示名（base 就是各组当前的 label）。
fn labels(mut groups: Vec<ProjectGroup>) -> Vec<String> {
    let bases: Vec<String> = groups.iter().map(|g| g.label.clone()).collect();
    disambiguate_labels(&mut groups, &bases);
    groups.into_iter().map(|g| g.label).collect()
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

/// worktree 的显示名本来就是「仓库 · 分支」，消歧时整体当末段，前缀补在最前。
#[test]
fn worktree_labels_keep_their_branch_suffix() {
    assert_eq!(
        labels(vec![
            group("/x/wt/smelt", "smelt · feat"),
            group("/y/wt/smelt", "smelt · feat"),
        ]),
        vec!["x/wt · smelt · feat", "y/wt · smelt · feat"]
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

fn leaf(cwd: &str) -> PaneState {
    PaneState::Leaf {
        cwd: Some(cwd.into()),
        id: None,
        custom_title: None,
        launch_label: None,
        launch_cmd: None,
    }
}

fn projects(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// cwd 就是项目根、或落在项目根之下，都该归这个项目。
#[test]
fn cwd_belongs_to_its_project_root() {
    let p = projects(&["/Users/me/dev/smelt"]);
    assert_eq!(
        project_root_of(&p, "/Users/me/dev/smelt").as_deref(),
        Some("/Users/me/dev/smelt")
    );
    assert_eq!(
        project_root_of(&p, "/Users/me/dev/smelt/crates/smeltd").as_deref(),
        Some("/Users/me/dev/smelt")
    );
    assert_eq!(project_root_of(&p, "/Users/me/dev/other"), None);
    assert_eq!(project_root_of(&p, ""), None);
}

/// 前缀必须卡在完整路径段上：`/a/smelt-old` 不是 `/a/smelt` 的子目录，
/// 否则名字相近的两个项目会互相吞会话。
#[test]
fn prefix_must_be_a_whole_path_segment() {
    let p = projects(&["/a/smelt"]);
    assert_eq!(project_root_of(&p, "/a/smelt-old"), None);
    assert_eq!(project_root_of(&p, "/a/smeltd"), None);
    assert_eq!(
        project_root_of(&p, "/a/smelt/sub").as_deref(),
        Some("/a/smelt")
    );
}

/// 父子项目都打开着时，会话归最深的那个（不然子项目永远空着）。
#[test]
fn deepest_matching_project_wins() {
    let p = projects(&["/a", "/a/b", "/a/b/c"]);
    assert_eq!(project_root_of(&p, "/a/b/c/x").as_deref(), Some("/a/b/c"));
    assert_eq!(project_root_of(&p, "/a/b/x").as_deref(), Some("/a/b"));
    assert_eq!(project_root_of(&p, "/a/x").as_deref(), Some("/a"));
}

/// 结尾斜杠是路径写法差异，不该影响归属判定。
#[test]
fn trailing_slashes_are_ignored() {
    let p = projects(&["/a/b/"]);
    assert_eq!(project_root_of(&p, "/a/b").as_deref(), Some("/a/b"));
    assert_eq!(project_root_of(&p, "/a/b/").as_deref(), Some("/a/b"));
    assert_eq!(project_root_of(&p, "/a/b/c").as_deref(), Some("/a/b"));
}

#[test]
fn deleting_worktree_removes_project_even_if_session_close_readded_it() {
    let mut p = projects(&[
        "/repo",
        "/repo-worktrees/feature",
        "/repo-worktrees/feature/sub",
    ]);

    remove_projects_under(&mut p, "/repo-worktrees/feature/");

    assert_eq!(p, projects(&["/repo"]));
}

/// 旧存档迁移：项目列表从会话 cwd 反推，终端会话取分屏树里第一个叶子的 cwd。
#[test]
fn legacy_archive_cwd_comes_from_first_leaf() {
    let ss = SessionState {
        layout: PaneState::Split {
            axis: SplitAxis::H,
            children: vec![leaf("/a/proj"), leaf("/b/other")],
            sizes: Vec::new(),
        },
        active: 0,
        last_updated_at: 0,
        custom_title: None,
        acp: None,
        route: None,
    };
    assert_eq!(session_state_cwd(&ss).as_deref(), Some("/a/proj"));
}

/// ACP 会话的 cwd 存在自己的元数据里，layout 只是占位叶子，别取错。
#[test]
fn acp_archive_cwd_comes_from_acp_meta() {
    let ss = SessionState {
        layout: leaf("/placeholder"),
        active: 0,
        last_updated_at: 0,
        custom_title: None,
        acp: Some(AcpSaved {
            cwd: Some("/a/acp-proj".into()),
            launch: smelt_core::agent_kind::ConversationLaunchSpec::from_command("claude --acp"),
            profile_id: None,
            agent: None,
            agent_definition_id: None,
            history_session_id: None,
            sid: None,
            refresh_launch_from_settings: false,
            fork_origin: None,
            conversation_binding: smelt_core::conversation::ConversationBinding::Direct,
            agent_session: None,
            config_values: Vec::new(),
            pending_prompt: None,
            pending_delivery_id: None,
            pending_agent_preset: None,
            automation_id: None,
            session_title: None,
        }),
        route: None,
    };
    assert_eq!(session_state_cwd(&ss).as_deref(), Some("/a/acp-proj"));
}
