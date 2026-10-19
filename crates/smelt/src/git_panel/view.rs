//! Git 面板页面：确认弹窗、窄版 SOURCE CONTROL、日志页。
//!
//! 只组 UI。写缓存、跑 git、改 Workspace 字段的方法留在 `workspace.rs`，
//! 由按钮 listener 调用。

use super::*;
use gpui_component::tab::{Tab, TabBar};

impl Workspace {
    /// 「丢弃文件改动」确认弹窗。未跟踪文件是直接删盘，措辞要比 restore 更重。
    pub fn render_discard_file_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some((_, seen)) = self.discard_file_target.as_ref() else {
            return div().into_any_element();
        };
        let path = &seen.path;
        let untracked = seen.untracked();
        let (title, body) = if untracked {
            (
                "确定删除这个新文件吗？",
                format!("{path} 从未被 git 跟踪过，删了就是彻底删除。"),
            )
        } else {
            (
                "确定丢弃这个文件的更改吗？",
                format!("{path} 会被还原成 HEAD 的样子，已暂存的部分一并还原。"),
            )
        };

        let content = v_flex()
            .child(Self::modal_title(fg, title))
            .child(div().text_sm().text_color(muted).child(body))
            .child(
                div()
                    .text_sm()
                    .text_color(accent_text)
                    .child("不进 reflog，找不回来。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-discard-file",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_discard_file(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-discard-file",
                        if untracked {
                            "删除文件"
                        } else {
                            "丢弃更改"
                        },
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_discard_file(cx),
                        cx,
                    )),
            );
        self.modal_shell(
            380.,
            true,
            content,
            crate::overlay::ModalLayer::DiscardFile,
            cx,
        )
    }

    /// 「丢弃这一块」确认弹窗。用危险配色，文案点明不可恢复——这个操作直接覆写
    /// 工作区文件，既不进索引也不进 reflog，点完就真没了。
    pub fn render_discard_hunk_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some((_, idx)) = self.discard_hunk_target.as_ref() else {
            return div().into_any_element();
        };
        let file = self
            .git_diff
            .as_ref()
            .map(|d| d.path.clone())
            .unwrap_or_default();

        let content = v_flex()
            .child(Self::modal_title(fg, "确定丢弃这一块更改吗？"))
            .child(div().text_sm().text_color(muted).child(format!(
                "{file} 的第 {} 块更改会被还原成更改前的样子。",
                idx + 1
            )))
            .child(
                div()
                    .text_sm()
                    .text_color(accent_text)
                    .child("直接改工作区文件，不进暂存区也不进 reflog——丢了就找不回来。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-discard-hunk",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_discard_hunk(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-discard-hunk",
                        "丢弃这一块",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_discard_hunk(cx),
                        cx,
                    )),
            );
        self.modal_shell(
            380.,
            true,
            content,
            crate::overlay::ModalLayer::DiscardHunk,
            cx,
        )
    }

    /// 「删除分支」确认弹窗。
    pub fn render_delete_branch_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some((_, branch, remote)) = self.delete_branch_target.as_ref() else {
            return div().into_any_element();
        };
        let remote = *remote;

        let content = v_flex()
            .child(Self::modal_title(
                fg,
                if remote {
                    "确定删除这个远端分支吗？"
                } else {
                    "确定删除这个分支吗？"
                },
            ))
            .child(div().text_sm().text_color(muted).child(if remote {
                format!("{branch} 会从远端仓库删除，其他人拉取后本地也会跟着消失。")
            } else {
                format!("{branch} 只删本地引用，工作区文件不受影响。")
            }))
            .child(div().text_sm().text_color(accent_text).child(if remote {
                "影响所有协作者，删之前确认没人在用。"
            } else {
                "有未合并的提交时 git 会拒绝删除，不会强删。"
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-branch",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_branch(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-delete-branch",
                        "删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_branch(cx),
                        cx,
                    )),
            );
        self.modal_shell(
            380.,
            true,
            content,
            crate::overlay::ModalLayer::DeleteBranch,
            cx,
        )
    }

    /// 「丢弃全部改动」确认弹窗（照 render_discard_file_confirm，措辞更重——连未
    /// 跟踪文件一起删）。
    pub fn render_discard_all_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        if self.discard_all_target.is_none() {
            return div().into_any_element();
        }
        let content = v_flex()
            .child(Self::modal_title(fg, "丢弃工作区全部更改？"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("已跟踪文件还原成 HEAD，未暂存 / 未跟踪的新文件会被删除。"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(accent_text)
                    .child("不进 reflog，找不回来。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-discard-all",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_discard_all(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-discard-all",
                        "全部丢弃",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_discard_all(cx),
                        cx,
                    )),
            );
        self.modal_shell(
            400.,
            true,
            content,
            crate::overlay::ModalLayer::DiscardAll,
            cx,
        )
    }

    /// Tool Panel 的窄版 GIT 面板：变更（STAGED / CHANGES）+ 操作
    ///（提交/推送）。打开全部改动后切到 DIFF 预览子视图
    ///（聚合行 + 文件级暂存按钮）。
    pub(crate) fn git_narrow_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> AnyElement {
        use crate::ui_theme;

        let Some(root) = self.active_project_root(cx) else {
            let remote = self.remote_project_notice(cx);
            let (title, hint) = match remote.as_deref() {
                Some(notice) => ("远程项目", notice.to_string()),
                None => ("没有项目", "打开一个项目后再看更改".to_string()),
            };
            return v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(rgb(ui_theme::text_bright()))
                                .child(title),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(ui_theme::text_faint()))
                                .child(hint),
                        ),
                )
                .into_any_element();
        };

        // ---- 列表 ----
        // 头部（分支、↑N↓M、stash、同步菜单）展示的是**当前提交目标仓库**的状态，
        // 不是项目根的。否则在子仓上提交时，头部会显示另一个仓库的分支与领先/落后。
        let active_repo = self.active_git_repo_root(&root);
        let status = self.git_status.get(&active_repo).map(|(_, d)| d);
        let status_refreshing = self.git_status_inflight.contains(&active_repo);
        let status_failure_count = self
            .git_status_failures
            .get(&active_repo)
            .copied()
            .unwrap_or_default();
        let (ahead, behind) = status
            .as_ref()
            .map(|d| (d.ahead, d.behind))
            .unwrap_or((0, 0));
        let stash_n = status.as_ref().map(|d| d.stash_count).unwrap_or(0);
        let has_changes = status.as_ref().is_some_and(|d| !d.files.is_empty());
        let ws_ops = cx.entity();
        let root_ops = root.clone();
        // 进行中反馈：点了拉取/获取几秒内没动静会以为没反应，op 跑着时按钮直接显示
        // 「拉取中…」。
        let busy = self.git_writes.get(&active_repo).copied();
        // 同步入口 = 把本来就显眼的绿色 ↑N↓M 直接做成可点下拉（带下拉箭头），
        // 替代之前那个不起眼的「⇅」。点开是获取/拉取/暂存/恢复/丢弃全部。
        let sync_label = match busy {
            Some(op) => op.chip().to_string(),
            None => format!("↑{ahead} ↓{behind}"),
        };
        let sync_color = if busy.is_some() {
            rgb(ui_theme::yellow())
        } else if ahead + behind > 0 {
            rgb(ui_theme::green())
        } else {
            rgb(ui_theme::text_faint())
        };
        let tab_view = cx.entity();
        // 仓库集合要在头部之前算好：历史页的仓库选择器和改动页的分组都用它。
        let repo_set = self.git_repos.get(&root).map(|(_, set)| set.clone());
        let repos: Vec<DiscoveredRepo> = match &repo_set {
            Some(set) if !set.repos.is_empty() => set.repos.clone(),
            // 发现还没回来：先当单仓渲染，首帧不至于空白。
            _ => vec![DiscoveredRepo {
                root: std::path::PathBuf::from(&root),
                rel_path: String::new(),
                kind: RepoKind::Root,
            }],
        };
        let show_repo_headers = repos.len() > 1;
        // 仓库选择器的选项：(显示名, 仓库根)。
        let repo_label_of = |repo: &DiscoveredRepo| -> String {
            if repo.rel_path.is_empty() {
                std::path::Path::new(&root)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| root.clone())
            } else {
                repo.rel_path.clone()
            }
        };
        let repo_choices: Vec<(String, String)> = repos
            .iter()
            .map(|repo| (repo_label_of(repo), repo.root.to_string_lossy().to_string()))
            .collect();
        let active_repo_label = repos
            .iter()
            .find(|repo| repo.root.to_string_lossy() == active_repo)
            .map(repo_label_of)
            .unwrap_or_else(|| active_repo.clone());
        let ws_log_repo = cx.entity();

        let view_tabs = TabBar::new("git-view-tabs")
            .underline()
            .xsmall()
            .selected_index(match self.git_tab {
                GitTab::Changes => 0,
                GitTab::Log => 1,
            })
            .on_click(move |ix, _window, cx| {
                tab_view.update(cx, |workspace, cx| {
                    let tab = if *ix == 0 {
                        GitTab::Changes
                    } else {
                        GitTab::Log
                    };
                    if workspace.git_tab == tab {
                        return;
                    }
                    workspace.git_tab = tab;
                    if tab == GitTab::Changes {
                        workspace.reset_git_diff_view();
                    }
                    cx.notify();
                });
            })
            .child(Tab::new().label("更改"))
            .child(Tab::new().label("历史"));
        let scope_view = cx.entity();
        let scope_control = Button::new("git-diff-scope")
            .ghost()
            .xsmall()
            .label(self.diff_scope.label())
            .dropdown_caret(true)
            .text_color(rgb(ui_theme::text_muted()))
            .dropdown_menu(move |mut menu, _window, _cx| {
                for scope in [DiffScope::All, DiffScope::Staged, DiffScope::Unstaged] {
                    let view = scope_view.clone();
                    menu = menu.item(PopupMenuItem::new(scope.label()).on_click(
                        move |_event, _window, cx| {
                            view.update(cx, |workspace, cx| {
                                workspace.set_diff_scope(scope, cx);
                            });
                        },
                    ));
                }
                menu
            });
        // 改动 / 历史是长期可见的同级视图，不属于操作菜单；与标题合并到同一行，
        // 避免旧的第二条 tab 栏挤压本就有限的 Git 内容高度。页签条铺满这一行，
        // 它自己的底线就是分隔线；作用范围、仓库、同步这些操作收在右侧。
        let actions = h_flex()
            .gap_2()
            .when(self.git_tab == GitTab::Changes, |bar| {
                bar.child(scope_control)
            })
            // 历史是"某个仓库的历史"。多仓工作区里不写明是谁的，看到的提交
            // 就完全没法对应到项目里的哪一块。
            .when(self.git_tab == GitTab::Log && repo_choices.len() > 1, |bar| {
                bar.child(
                    Button::new("git-log-repo")
                        .ghost()
                        .xsmall()
                        .label(active_repo_label)
                        .dropdown_caret(true)
                        .text_color(rgb(ui_theme::text_muted()))
                        .dropdown_menu(move |mut menu, _window, _cx| {
                            for (label, repo_root) in repo_choices.clone() {
                                let ws = ws_log_repo.clone();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_ev, _window, cx| {
                                        let root = repo_root.clone();
                                        ws.update(cx, |workspace, cx| {
                                            workspace.set_active_git_repo(root, cx);
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                )
            })
            // 历史页还没有分仓概念，仍用当前目标仓库的操作菜单；改动页的操作
            // 已经下放到每个仓库自己那一行，这里不再摆一个"作用于谁不明确"的全局按钮。
            .when(self.git_tab == GitTab::Log, |bar| {
                bar.child(
                    Button::new("git-sync-menu")
                        .ghost()
                        .xsmall()
                        .label(sync_label)
                        .dropdown_caret(busy.is_none())
                        .font_family(crate::terminal_view::font_family())
                        .text_color(sync_color)
                        .dropdown_menu(move |menu, _window, _cx| {
                            git_ops_menu_items(
                                menu,
                                ws_ops.clone(),
                                root_ops.clone(),
                                ahead,
                                behind,
                                stash_n,
                                has_changes,
                            )
                        }),
                )
            });
        let header = view_tabs
            .w_full()
            .h(ui_theme::height::lg())
            .flex_shrink_0()
            .px_3()
            .suffix(actions);

        if self.git_tab == GitTab::Log {
            return v_flex()
                .flex_1()
                .min_h_0()
                .child(header)
                .child(self.render_git_log_tab(active_repo, cx))
                .into_any_element();
        }

        // 多仓工作区：逐仓拿自己的 status，各自分组。文件不再被摊平成“项目根的文件”，
        // 因此暂存和提交总是发生在同一个仓库里。

        let ws = cx.entity();
        // 一组改动：复用全屏页的 build_git_tree（JetBrains 式目录树 + 单链压缩），
        // 折叠状态也共用同一份 git_tree_collapsed，两处视图折叠同步。
        // 目录行 = caret + 目录名（淡）；文件行 = 前缀符号（staged「+」绿 /
        // 已跟踪改动「~」橙 / 未跟踪「?」绿）+ 文件名 + 右侧状态字母。
        let collapsed = self.git_tree_collapsed.clone();
        // uniform_list 的渲染闭包必须持有数据快照。按仓库嵌套投影，行渲染时先用 &str
        // 取内层 map 再查 path，不用每行拼 tuple key 分配两个 String。
        let mut index_pending: HashMap<String, HashMap<String, PendingGitIndexOp>> = HashMap::new();
        for ((pending_root, path), pending) in &self.git_index_pending {
            index_pending
                .entry(pending_root.clone())
                .or_default()
                .insert(path.clone(), *pending);
        }

        // 变更行统一高度，交给 GPUI 的 uniform_list 只构造可视区附近的元素。
        // 旧实现把每个文件的 Checkbox、右键菜单和文本树一次性挂进 flex 树，176
        // 个文件时会让 Taffy 在每帧重排整棵树。
        // 哪些改动条目其实是子模块指针。
        //
        // 发现器只解析了项目根的 `.gitmodules`，所以这份路径集合是**相对项目根**的，
        // 只能用来标注项目根自己的 status；拿去匹配子仓内的文件会张冠李戴。
        let root_gitlinks: HashSet<String> = repos
            .iter()
            .filter(|r| matches!(r.kind, RepoKind::Submodule))
            .map(|r| r.rel_path.clone())
            .collect();
        let no_gitlinks: HashSet<String> = HashSet::new();

        let mut narrow_rows = Vec::new();
        let mut total_changes = 0usize;
        // 有没有任何值得让列表存在的事：改动、待推送、待拉取、压着的 stash。
        let mut anything_to_do = false;
        for repo in &repos {
            let repo_root = repo.root.to_string_lossy().to_string();
            let repo_status = self.git_status.get(&repo_root).map(|(_, d)| d);
            let (staged, changed) = match repo_status {
                Some(d) => split_staged_and_changed(&d.files),
                None => (Vec::new(), Vec::new()),
            };
            total_changes += staged.len() + changed.len();

            // 干净的仓库不占列表空间，但当前提交目标除外——否则用户看不到
            // 自己正在往哪个仓库提交。
            let is_active = repo_root == active_repo;
            let has_changes = !staged.is_empty() || !changed.is_empty();
            anything_to_do |= repo_status.is_some_and(|d| {
                repo_needs_attention(has_changes, d.ahead, d.behind, d.stash_count)
            });

            let repo_root: Rc<str> = Rc::from(repo_root.as_str());
            let repo_collapsed = self.git_repo_collapsed.contains(repo_root.as_ref());
            if show_repo_headers {
                narrow_rows.push(NarrowGitRow::RepoHeader {
                    root: repo_root.clone(),
                    label: if repo.rel_path.is_empty() {
                        std::path::Path::new(&root)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| root.clone())
                    } else {
                        repo.rel_path.clone()
                    },
                    branch: repo_status
                        .map(|d| d.branch.clone())
                        .unwrap_or_else(|| "…".to_string()),
                    kind: repo.kind,
                    active: is_active,
                    collapsed: repo_collapsed,
                    ahead: repo_status.map(|d| d.ahead).unwrap_or(0),
                    behind: repo_status.map(|d| d.behind).unwrap_or(0),
                    stash_n: repo_status.map(|d| d.stash_count).unwrap_or(0),
                    has_changes,
                });
            }
            // 折起来的仓库只留标题行。
            if show_repo_headers && repo_collapsed {
                continue;
            }
            // 干净的仓库不摆提交区——没东西可提交。但仓库行本身一定留着：
            // 「没有改动」不等于「没事可做」，本地攒着待推送的提交、远端有待拉取的
            // 提交、栈里压着 stash，都只能从那一行的操作菜单进去。
            // 之前整段跳过干净仓库，结果提交完最后一笔改动，仓库连同推送入口一起消失。
            let show_commit_box = has_changes || is_active;
            if let Some(input) = self
                .commit_msg_inputs
                .get(repo_root.as_ref())
                .filter(|_| show_commit_box)
            {
                let commit_text = input.read(cx).value().to_string();
                let has_text = !commit_text.trim().is_empty();
                let has_staged = !staged.is_empty();
                let error = self.commit_errors.get(repo_root.as_ref()).cloned();
                let (input_h, row_h) =
                    commit_box_metrics(&commit_text, error.as_deref(), has_text && !has_staged);
                narrow_rows.push(NarrowGitRow::CommitBox {
                    root: repo_root.clone(),
                    input: input.clone(),
                    has_text,
                    has_staged,
                    git_write: self.git_writes.get(repo_root.as_ref()).copied(),
                    ahead: repo_status.map(|d| d.ahead).unwrap_or(0),
                    error,
                    depth: usize::from(show_repo_headers),
                    input_h,
                    row_h,
                });
            }
            // 多仓时文件比仓库行缩进一级，层级才看得出来；单仓不白白浪费横向空间。
            let depth = usize::from(show_repo_headers);
            let gitlinks = if repo.rel_path.is_empty() {
                &root_gitlinks
            } else {
                &no_gitlinks
            };
            if !staged.is_empty() {
                append_narrow_git_group(
                    &mut narrow_rows,
                    NarrowGitGroup {
                        title: format!("暂存的更改 · {}", staged.len()),
                        files: &staged,
                        key: "narrow-staged",
                        is_staged_group: true,
                        root: repo_root.clone(),
                        depth,
                        gitlinks,
                    },
                    &collapsed,
                );
            }
            if !changed.is_empty() {
                append_narrow_git_group(
                    &mut narrow_rows,
                    NarrowGitGroup {
                        title: format!("更改 · {}", changed.len()),
                        files: &changed,
                        key: "narrow-changed",
                        is_staged_group: false,
                        root: repo_root.clone(),
                        depth,
                        gitlinks,
                    },
                    &collapsed,
                );
            }
        }
        // 真的无事可做时才当空列表处理（显示"工作区干净"）。只看改动数是不够的：
        // 仓库可能干净但攒着待推送的提交，那一行的操作菜单是唯一的推送入口。
        if total_changes == 0 && !anything_to_do {
            narrow_rows.clear();
        }
        let list: AnyElement = if narrow_rows.is_empty() {
            let message = git_empty_list_message(status, status_refreshing, status_failure_count);
            div()
                .id("narrow-git-list-empty")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .text_sm()
                .text_color(rgb(ui_theme::text_faint()))
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .text_color(rgb(ui_theme::text_bright()))
                        .child(if message == "工作区干净" {
                            "工作区干净"
                        } else {
                            message
                        }),
                )
                .when(message == "工作区干净", |col| {
                    col.child(
                        div()
                            .text_xs()
                            .text_color(rgb(ui_theme::text_faint()))
                            .child("没有要提交的更改"),
                    )
                })
                .into_any_element()
        } else {
            // 仓库行、提交框和文件行高度不同，用变高虚拟列表，只构造视口附近的行。
            let row_w = px(self.tool_panel_w.max(160.));
            let sizes = Rc::new(
                narrow_rows
                    .iter()
                    .map(|row| size(row_w, px(narrow_git_row_height(row))))
                    .collect::<Vec<_>>(),
            );
            let rows = Rc::new(narrow_rows);
            let ws_for_rows = ws.clone();
            let collapsed_for_rows = collapsed;
            let index_pending = Rc::new(index_pending);
            div()
                .id("narrow-git-list-host")
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    v_virtual_list(ws, "narrow-git-list", sizes, move |_, range, _, _| {
                        let rows = rows.clone();
                        let ws_for_rows = ws_for_rows.clone();
                        let collapsed_for_rows = collapsed_for_rows.clone();
                        let index_pending = index_pending.clone();
                        range
                            .map(|ix| {
                                // 虚拟列表用 layout_as_root 排每一行，只给可用宽度、不强制
                                // 行宽：行根没有显式宽度就按内容收缩，提交框会被挤成一个字宽。
                                // 在这里统一撑满列宽，各类行不用各自记得加 w_full。
                                v_flex()
                                    .w_full()
                                    .child(render_narrow_git_row(
                                        &rows[ix],
                                        &ws_for_rows,
                                        &collapsed_for_rows,
                                        &index_pending,
                                    ))
                                    .into_any_element()
                            })
                            .collect()
                    })
                    .size_full()
                    .track_scroll(&self.git_list_scroll),
                )
                .vertical_scrollbar(&self.git_list_scroll)
                .into_any_element()
        };
        // 发现被上限截断时必须说清楚。默不作声会让用户以为没列出来的仓库
        // 没改动，而事实是根本没看。
        let list: AnyElement = if repo_set.as_ref().is_some_and(|set| set.truncated) {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(ui_theme::text_faint()))
                        .child("仓库太多，只列出了一部分"),
                )
                .into_any_element()
        } else {
            list
        };

        let list_and_commit = v_flex()
            .flex_1()
            .min_h_0()
            .pt_2()
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child("变更"),
            )
            .child(list);
        // 有打开的 diff：内嵌显示（diff 在左占主宽度，改动列表 + commit 框收窄到
        // 右边），跟 Files 面板「内容左、树右」同一个视觉套路——点了改动不再
        // 提升到舞台另开一页（见 open_diff 的改动）。
        // Git 改动树和 Files 文件树共用一份已持久化的侧树宽度。拖外层 Tool Panel
        // 不能按比例重算它；只有拖这份树自身的分隔线时才会改变。
        let list_w = self.file_tree_w.clamp(
            crate::tool_panel::MIN_FILE_TREE_WIDTH,
            crate::tool_panel::MAX_FILE_TREE_WIDTH,
        );
        // diff 栏恒在：选中干净仓库时把它整块撤掉，会让面板布局在切仓库时跳来跳去，
        // 还让人以为预览功能坏了。没有可显示的 diff 就画成空态。
        let changes_body: AnyElement = {
            let empty_hint = if total_changes > 0 {
                "← 选择文件查看更改"
            } else {
                "没有文件更改"
            };
            let diff_pane = self.git_diff_pane(empty_hint, cx);
            div()
                .flex_1()
                .relative()
                .min_h_0()
                .overflow_hidden()
                .child(self.file_tree_resize_listener(cx))
                .flex()
                .child(
                    div()
                        .size_full()
                        .flex()
                        .child(div().flex_1().min_w_0().min_h_0().flex().child(diff_pane))
                        .child(self.file_tree_resize_handle("git-narrow-diff-split", cx))
                        .child(
                            div()
                                .w(px(list_w))
                                .flex_none()
                                .min_w_0()
                                .min_h_0()
                                .flex()
                                .child(
                                    div()
                                        .size_full()
                                        .min_h_0()
                                        .flex()
                                        .flex_col()
                                        .child(list_and_commit),
                                ),
                        ),
                )
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .child(header)
            .child(changes_body)
            .into_any_element()
    }

    /// GIT 面板「日志」子标签：提交历史 + 分支图三栏（分支树 / 图+列表 / 详情），
    /// 停靠 / 舞台展开态共用（root 由调用方保证非空）。
    pub(crate) fn render_git_log_tab(
        &mut self,
        root: String,
        cx: &mut Context<Workspace>,
    ) -> AnyElement {
        // `root` 现在是当前选中的仓库根（不是项目根），分支与 HEAD 都读它自己的缓存。
        let branches = self.branches.get(&root).map(|(_, b)| b);
        // 当前检出的分支：日志默认看它，分支树里也要标出来。复用 Git 页已有的
        // status 缓存，不另跑一次 git。
        let head_branch = self
            .git_status
            .get(&root)
            .map(|(_, d)| d.branch_name().to_string())
            .filter(|b| !b.is_empty());
        // 三栏都可拖拽：窗口窄的时候能自己腾地方，写死宽度的话中间的提交
        // 列表会被挤得没法看。
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(
                h_resizable("git-log-split")
                    .with_state(&self.git_log_resize)
                    // 左：分支树
                    .child(
                        resizable_panel()
                            .size(px(200.))
                            .size_range(px(140.)..Pixels::MAX)
                            .child(
                                div()
                                    .size_full()
                                    .min_h_0()
                                    .child(crate::git_log_view::branch_tree(
                                        Some(root.clone()),
                                        branches,
                                        &self.git_log.scope,
                                        head_branch.clone(),
                                        cx,
                                    )),
                            ),
                    )
                    // 中：分支图 + 提交列表
                    // wrapper 必须 .flex()：div 默认 Block，里面 flex_1 根节点会
                    // 高度塌 0（同 Git 改动页 diff 面板的坑）。
                    .child(
                        resizable_panel().child(
                            div()
                                .size_full()
                                .flex()
                                .min_w_0()
                                .min_h_0()
                                .child(crate::git_log_view::git_log_view(
                                    Some(root.clone()),
                                    &self.git_log,
                                    head_branch,
                                    cx,
                                )),
                        ),
                    )
                    // 右：提交详情
                    .child(
                        resizable_panel()
                            .size(px(380.))
                            .size_range(px(240.)..Pixels::MAX)
                            .child(div().size_full().flex().min_w_0().min_h_0().child(
                                crate::git_log_view::commit_detail_pane(
                                    Some(root),
                                    &self.git_log,
                                    cx,
                                ),
                            )),
                    ),
            )
            .into_any_element()
    }
}
