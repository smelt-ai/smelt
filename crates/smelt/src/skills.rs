//! Skills 面板数据源：扫 `~/.claude/skills/*/SKILL.md`（用户级）与
//! `<项目>/.claude/skills/*/SKILL.md`（项目级），读 YAML frontmatter 的
//! `name` / `description`。
//!
//! **只读**：Claude Code 没有「启用/停用某个 skill」的开关机制（settings.json 里
//! 的 `enabledPlugins` 管的是插件，不是 skill），所以这里不做开关——放一个拨了
//! 不生效的开关比不放更糟。面板的价值在「看清有哪些能力 + 一键把 /name 填进
//! 当前会话」。
//!
//! 跟 claude_memory.rs 同一个套路：纯数据函数，后台线程扫盘，render 只读缓存。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use gpui::AppContext;
use gpui_component::input::Input;

use crate::ui_theme;

/// 一条 skill。
#[derive(Clone)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// true = 项目级（`<项目>/.claude/skills`），false = 用户级（`~/.claude/skills`）。
    pub project_scope: bool,
    /// skill 所在目录（含 `SKILL.md` 的那一层），编辑/删除都对着它操作。
    pub dir: PathBuf,
}

/// 扫描用户级 + 项目级 skills（阻塞读盘，调用方放后台线程）。
pub fn scan_skills(project_cwd: Option<&str>) -> Vec<SkillEntry> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        collect_dir(&home.join(".claude/skills"), false, &mut out);
    }
    if let Some(cwd) = project_cwd {
        collect_dir(&PathBuf::from(cwd).join(".claude/skills"), true, &mut out);
    }
    // 项目级在前（更贴近手头的活），组内按名字排。
    out.sort_by(|a, b| {
        b.project_scope
            .cmp(&a.project_scope)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn collect_dir(dir: &PathBuf, project_scope: bool, out: &mut Vec<SkillEntry>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let md = path.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&md) else {
            continue;
        };
        let (name, description) = parse_frontmatter(&text);
        // frontmatter 缺 name 就退回目录名——目录名本来就是 skill 的调用名。
        let name = name.unwrap_or_else(|| {
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        if name.is_empty() {
            continue;
        }
        out.push(SkillEntry {
            name,
            description: description.unwrap_or_default(),
            project_scope,
            dir: path,
        });
    }
}

/// 校验 skill 名（同时是目录名和 frontmatter `name`，两者本该一致）：非空、
/// 不超长、只允许字母/数字/连字符/下划线——避免路径穿越（`..`、`/`）或写出
/// Claude 不认的名字。
pub fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("名称不能为空".into());
    }
    if name.len() > 64 {
        return Err("名称过长（最多 64 个字符）".into());
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err("只能包含字母、数字、连字符(-)和下划线(_)".into());
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err("不能以 - 或 _ 开头".into());
    }
    Ok(())
}

/// 某个作用域的 skills 根目录：项目级要求 `project_cwd` 已知，否则返回 `None`
/// （调用方应该在项目未打开时禁掉「项目级」这个选项）。
fn skills_root(project_cwd: Option<&str>, project_scope: bool) -> Option<PathBuf> {
    if project_scope {
        project_cwd.map(|c| PathBuf::from(c).join(".claude/skills"))
    } else {
        dirs::home_dir().map(|h| h.join(".claude/skills"))
    }
}

/// 把字符串写成 YAML 双引号标量：能安全塞进 frontmatter 单行字段，不管原文
/// 有没有冒号、引号——省得再判断要不要加引号。
fn quote_scalar(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

/// 创建一个新 skill：校验名字、确认目标目录不存在（避免覆盖同名 skill）、
/// 写最小可用的 `SKILL.md`。返回新目录路径。
pub fn create_skill(
    project_cwd: Option<&str>,
    project_scope: bool,
    name: &str,
    description: &str,
) -> Result<PathBuf, String> {
    let name = name.trim();
    validate_skill_name(name)?;
    let root = skills_root(project_cwd, project_scope).ok_or("项目未打开，无法创建项目级 skill")?;
    let dir = root.join(name);
    if dir.exists() {
        return Err(format!("已存在同名 skill：{}", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{e}"))?;
    let description = description.trim();
    let content = format!(
        "---\nname: {}\ndescription: {}\n---\n\n# {}\n\n在这里写这个 skill 的具体使用说明和步骤。\n",
        quote_scalar(name),
        quote_scalar(description),
        name
    );
    std::fs::write(dir.join("SKILL.md"), content).map_err(|e| format!("写入 SKILL.md 失败：{e}"))?;
    Ok(dir)
}

/// 更新一个已有 skill：改名字/描述，保留 frontmatter 里的其它字段和正文不动。
/// 名字变了就顺手把目录也改名（目录名 = 调用名是约定），目标目录已存在则报错
/// 而不是静默覆盖。
pub fn update_skill(dir: &Path, name: &str, description: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    validate_skill_name(name)?;
    let md_path = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&md_path).map_err(|e| format!("读取 SKILL.md 失败：{e}"))?;
    let new_text = rewrite_frontmatter(&text, name, description.trim());
    std::fs::write(&md_path, new_text).map_err(|e| format!("写入 SKILL.md 失败：{e}"))?;

    let cur_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    if cur_name != name {
        let new_dir = dir.with_file_name(name);
        if new_dir.exists() {
            return Err(format!("已存在同名 skill：{}", new_dir.display()));
        }
        std::fs::rename(dir, &new_dir).map_err(|e| format!("重命名目录失败：{e}"))?;
        return Ok(new_dir);
    }
    Ok(dir.to_path_buf())
}

/// 删除一个 skill 目录。删前确认目录里确有 `SKILL.md`——避免误删传进来的
/// 任意目录（比如上层状态没同步好，指向了别的路径）。
pub fn delete_skill(dir: &Path) -> Result<(), String> {
    if !dir.join("SKILL.md").exists() {
        return Err("目录中没有 SKILL.md，拒绝删除".into());
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("删除失败：{e}"))
}

/// 重写 frontmatter 的 `name` / `description` 两个字段，其余字段（含未知的
/// 自定义 key）原样保留在原位置之后；正文（`---` 之后的部分）完全不动。
/// 没有 frontmatter 的文件会被补上一段。
fn rewrite_frontmatter(text: &str, name: &str, description: &str) -> String {
    let header = format!(
        "---\nname: {}\ndescription: {}\n",
        quote_scalar(name),
        quote_scalar(description)
    );
    let raw_lines: Vec<&str> = text.lines().collect();
    if raw_lines.first().map(|l| l.trim()) != Some("---") {
        return format!("{header}---\n\n{text}");
    }
    let end_idx = raw_lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i);
    let Some(end_idx) = end_idx else {
        return format!("{header}---\n\n{text}");
    };
    let body = raw_lines[end_idx + 1..].join("\n");

    // 保留原 frontmatter 里除 name/description（含其折叠续行）外的其它字段。
    let mut kept = Vec::new();
    let mut i = 1;
    while i < end_idx {
        let line = raw_lines[i];
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            if let Some((k, _)) = line.split_once(':') {
                if matches!(k.trim(), "name" | "description") {
                    i += 1;
                    while i < end_idx
                        && (raw_lines[i].starts_with(' ') || raw_lines[i].starts_with('\t'))
                    {
                        i += 1;
                    }
                    continue;
                }
            }
        }
        kept.push(line);
        i += 1;
    }

    let mut out = header;
    for l in kept {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str("---\n");
    let body_trimmed = body.trim_start_matches('\n');
    if !body_trimmed.is_empty() {
        out.push('\n');
        out.push_str(body_trimmed);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// 从 YAML frontmatter 里取 `name` / `description`，只认最简形态
/// （`key: value`，值可带引号、可跨行缩进续行）——skill 的 frontmatter 就这几个
/// 标量字段，引全套 YAML 解析器不值当。
fn parse_frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut description) = (None, None);
    // 当前正在续行的字段（YAML 折叠行：下一行有缩进即为上一行的续写）。
    let mut pending: Option<&'static str> = None;
    let mut buf = String::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if indented {
            if pending.is_some() {
                buf.push(' ');
                buf.push_str(line.trim());
            }
            continue;
        }
        // 新字段开始：先把上一段收尾。
        if let Some(key) = pending.take() {
            let v = unquote(buf.trim());
            match key {
                "name" => name = Some(v),
                _ => description = Some(v),
            }
            buf.clear();
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        match k.trim() {
            "name" => {
                pending = Some("name");
                buf.push_str(v.trim());
            }
            "description" => {
                pending = Some("description");
                buf.push_str(v.trim());
            }
            _ => {}
        }
    }
    if let Some(key) = pending {
        let v = unquote(buf.trim());
        match key {
            "name" => name = Some(v),
            _ => description = Some(v),
        }
    }
    (name, description)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// render 侧缓存类型别名（与 usage_cache 等同款：(取得时刻, 数据)）。
pub type SkillsCache = Option<(std::time::Instant, Rc<Vec<SkillEntry>>)>;

impl crate::Workspace {
    /// SKILLS 面板：确保缓存新鲜（>30s 或换了项目就后台重扫）。
    /// 跟 ensure_memory_list 同一套模板——读盘绝不放在 render 里同步做。
    pub(crate) fn ensure_skills(&mut self, cwd: Option<String>, cx: &mut gpui::Context<Self>) {
        use std::time::{Duration, Instant};
        let fresh = self
            .skills_cache
            .as_ref()
            .is_some_and(|(t, _)| t.elapsed() < Duration::from_secs(30))
            && self.skills_cache_cwd == cwd;
        if fresh || self.skills_inflight {
            return;
        }
        self.skills_inflight = true;
        let scan_cwd = cwd.clone();
        cx.spawn(async move |this, cx| {
            let c = scan_cwd.clone();
            let list = cx
                .background_executor()
                .spawn(async move { scan_skills(c.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.skills_inflight = false;
                this.skills_cache_cwd = scan_cwd;
                this.skills_cache = Some((Instant::now(), Rc::new(list)));
                cx.notify();
            });
        })
        .detach();
    }

    /// 把 `/skill-name` 送进当前会话：终端直接敲进去（不回车，留给人补参数）；
    /// ACP 会话填进输入框并聚焦。没有活动会话就什么都不做。
    pub(crate) fn send_skill_to_session(
        &mut self,
        cmd: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(sess) = self.sessions.get(self.active_session) else {
            return;
        };
        match &sess.kind {
            crate::SessionKind::Acp(view) => {
                let view = view.clone();
                let text = cmd.to_string();
                view.update(cx, |v, cx| v.insert_prompt_text(&text, window, cx));
            }
            crate::SessionKind::Term { active, .. } => {
                let pane = active.clone();
                let text = cmd.to_string();
                pane.update(cx, |tv, cx| tv.type_text(&text, cx));
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    /// 打开「新建 skill」弹窗。`default_project_scope`：有活动项目就默认建项目级，
    /// 否则只能建用户级。
    pub(crate) fn open_create_skill_modal(
        &mut self,
        default_project_scope: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        use gpui_component::input::InputState;
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("my-skill-name"));
        let desc_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("触发条件与用途，一句话说清楚"));
        name_input.update(cx, |s, cx| s.focus(window, cx));
        self.skill_modal = Some(SkillModalState {
            editing: None,
            project_scope: default_project_scope,
            name_input,
            desc_input,
            error: None,
        });
        cx.notify();
    }

    /// 打开「编辑 skill」弹窗，预填现有的名字/描述。
    pub(crate) fn open_edit_skill_modal(
        &mut self,
        entry: &SkillEntry,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        use gpui_component::input::InputState;
        let name_input =
            cx.new(|cx| InputState::new(window, cx).default_value(entry.name.clone()));
        let desc_input =
            cx.new(|cx| InputState::new(window, cx).default_value(entry.description.clone()));
        name_input.update(cx, |s, cx| s.focus(window, cx));
        self.skill_modal = Some(SkillModalState {
            editing: Some(entry.dir.clone()),
            project_scope: entry.project_scope,
            name_input,
            desc_input,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_skill_modal(&mut self, cx: &mut gpui::Context<Self>) {
        self.skill_modal = None;
        cx.notify();
    }

    /// 提交新建/编辑弹窗：校验失败就把错误文案摆在弹窗里，不关闭；成功则关闭弹窗
    /// 并让 SKILLS 面板重新扫盘（清缓存，下次 render 触发 ensure_skills）。
    pub(crate) fn submit_skill_modal(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(modal) = self.skill_modal.as_ref() else {
            return;
        };
        let name = modal.name_input.read(cx).value().trim().to_string();
        let description = modal.desc_input.read(cx).value().trim().to_string();
        let editing = modal.editing.clone();
        let project_scope = modal.project_scope;
        let cwd = self.cur().and_then(|s| s.cwd(cx));

        let result = match editing {
            Some(dir) => update_skill(&dir, &name, &description).map(|_| ()),
            None => create_skill(cwd.as_deref(), project_scope, &name, &description).map(|_| ()),
        };

        match result {
            Ok(()) => {
                self.skills_cache = None;
                self.skill_modal = None;
            }
            Err(msg) => {
                if let Some(modal) = self.skill_modal.as_mut() {
                    modal.error = Some(msg);
                }
            }
        }
        cx.notify();
    }

    /// 弹出「删除 skill」二次确认。
    pub(crate) fn request_delete_skill(&mut self, entry: &SkillEntry, cx: &mut gpui::Context<Self>) {
        self.skill_delete_target = Some(entry.clone());
        cx.notify();
    }

    pub(crate) fn cancel_delete_skill(&mut self, cx: &mut gpui::Context<Self>) {
        self.skill_delete_target = None;
        cx.notify();
    }

    /// 真正执行删除：成功/失败都关掉确认弹窗（失败暂不额外提示——跟
    /// perform_delete_file 同一套克制处理，避免再叠一层错误弹窗）。
    pub(crate) fn confirm_delete_skill(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(target) = self.skill_delete_target.take() else {
            return;
        };
        let _ = delete_skill(&target.dir);
        self.skills_cache = None;
        cx.notify();
    }

    /// 「新建/编辑 skill」弹窗：与 render_rename_session 同款视觉，正文换成
    /// 名字 + 描述两个输入框 + 作用域切换（仅新建时可选，编辑时作用域已固定）。
    pub(crate) fn render_skill_modal(&self, cx: &mut gpui::Context<Self>) -> gpui::Div {
        use gpui::prelude::FluentBuilder;
        use gpui::*;
        use gpui_component::*;
        let Some(modal) = self.skill_modal.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            crate::Workspace::modal_accent_colors(false);
        let is_edit = modal.editing.is_some();
        let has_project = self.cur().and_then(|s| s.cwd(cx)).is_some();
        let project_scope = modal.project_scope;

        let mut content = v_flex()
            .child(
                div()
                    .font_bold()
                    .text_color(fg)
                    .text_lg()
                    .child(if is_edit { "编辑 skill" } else { "新建 skill" }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("名称（目录名与调用名，字母/数字/连字符/下划线）"),
            )
            .child(Input::new(&modal.name_input))
            .child(div().text_xs().text_color(muted).child("描述（何时该用它）"))
            .child(Input::new(&modal.desc_input));

        if !is_edit {
            let e_user = cx.entity();
            let e_project = cx.entity();
            content = content.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().text_color(muted).child("作用域"))
                    .child(
                        div()
                            .id("skill-scope-user")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .cursor_pointer()
                            .when(!project_scope, |d| {
                                d.bg(tint).text_color(accent_text)
                            })
                            .when(project_scope, |d| d.text_color(muted))
                            .child("用户级")
                            .on_click(move |_ev, _window, cx: &mut App| {
                                e_user.update(cx, |ws: &mut crate::Workspace, cx| {
                                    if let Some(m) = ws.skill_modal.as_mut() {
                                        m.project_scope = false;
                                    }
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        div()
                            .id("skill-scope-project")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .when(has_project, |d| d.cursor_pointer())
                            .when(!has_project, |d| d.opacity(0.4))
                            .when(project_scope, |d| d.bg(tint).text_color(accent_text))
                            .when(!project_scope, |d| d.text_color(muted))
                            .child("项目级")
                            .on_click(move |_ev, _window, cx: &mut App| {
                                e_project.update(cx, |ws: &mut crate::Workspace, cx| {
                                    if ws.cur().and_then(|s| s.cwd(cx)).is_some() {
                                        if let Some(m) = ws.skill_modal.as_mut() {
                                            m.project_scope = true;
                                        }
                                        cx.notify();
                                    }
                                });
                            }),
                    ),
            );
        }

        if let Some(err) = modal.error.as_ref() {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(Hsla::from(rgb(ui_theme::red())))
                    .child(err.clone()),
            );
        }

        content = content.child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(crate::Workspace::modal_button(
                    "cancel-skill-modal",
                    "取消",
                    neutral_bg,
                    neutral_hover,
                    fg,
                    |this, _, _, cx| this.close_skill_modal(cx),
                    cx,
                ))
                .child(crate::Workspace::modal_button(
                    "confirm-skill-modal",
                    if is_edit { "保存" } else { "创建" },
                    tint,
                    hover,
                    accent_text,
                    |this, _, _, cx| this.submit_skill_modal(cx),
                    cx,
                )),
        );

        crate::Workspace::modal_shell(380., false, content, cx)
    }

    /// 「删除 skill」二次确认弹窗：明确写出作用域 + 路径，危险操作走红色强调。
    pub(crate) fn render_delete_skill_confirm(&self, cx: &mut gpui::Context<Self>) -> gpui::Div {
        use gpui::*;
        use gpui_component::*;
        let Some(target) = self.skill_delete_target.as_ref() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            crate::Workspace::modal_accent_colors(true);
        let scope = if target.project_scope {
            "项目级"
        } else {
            "用户级"
        };

        let content = v_flex()
            .child(
                div()
                    .font_bold()
                    .text_color(fg)
                    .text_lg()
                    .child("确定删除这个 skill 吗？"),
            )
            .child(
                div().text_sm().text_color(muted).child(format!(
                    "将永久删除{}「{}」，位于 {}，此操作不可撤销。",
                    scope,
                    target.name,
                    target.dir.display()
                )),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(crate::Workspace::modal_button(
                        "cancel-delete-skill",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_skill(cx),
                        cx,
                    ))
                    .child(crate::Workspace::modal_button(
                        "confirm-delete-skill",
                        "确定删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_skill(cx),
                        cx,
                    )),
            );
        crate::Workspace::modal_shell(360., true, content, cx)
    }
}

/// 「新建/编辑 skill」弹窗状态。`editing` 为 `None` 表示新建，`Some(dir)` 表示
/// 正在编辑该目录下的 skill（提交走 [`update_skill`] 而非 [`create_skill`]）。
pub(crate) struct SkillModalState {
    pub editing: Option<PathBuf>,
    pub project_scope: bool,
    pub name_input: gpui::Entity<gpui_component::input::InputState>,
    pub desc_input: gpui::Entity<gpui_component::input::InputState>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_quoted_and_folded_frontmatter() {
        let text = "---\nname: commit-work\ndescription: \"Create commits:\n  split into logical commits.\"\n---\n\n# body\n";
        let (name, desc) = super::parse_frontmatter(text);
        assert_eq!(name.as_deref(), Some("commit-work"));
        assert_eq!(
            desc.as_deref(),
            Some("Create commits: split into logical commits.")
        );
    }

    #[test]
    fn ignores_files_without_frontmatter() {
        let (name, desc) = super::parse_frontmatter("# 没有 frontmatter\n");
        assert!(name.is_none() && desc.is_none());
    }

    #[test]
    fn validates_skill_name() {
        assert!(super::validate_skill_name("commit-work").is_ok());
        assert!(super::validate_skill_name("").is_err());
        assert!(super::validate_skill_name("../evil").is_err());
        assert!(super::validate_skill_name("has space").is_err());
        assert!(super::validate_skill_name("-leading-dash").is_err());
    }

    #[test]
    fn rewrite_frontmatter_preserves_body_and_unknown_fields() {
        let text = "---\nname: old-name\ndescription: old desc\nversion: 2\n---\n\n# Body\n\ncontent here\n";
        let out = super::rewrite_frontmatter(text, "new-name", "new desc");
        assert!(out.starts_with("---\nname: \"new-name\"\ndescription: \"new desc\"\n"));
        assert!(out.contains("version: 2\n"));
        assert!(out.contains("# Body\n\ncontent here\n"));
        assert!(!out.contains("old-name"));
        assert!(!out.contains("old desc"));
    }

    #[test]
    fn create_update_delete_skill_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("smelt-skill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cwd = tmp.to_string_lossy().into_owned();

        let dir = super::create_skill(Some(&cwd), true, "my-skill", "does a thing").unwrap();
        assert!(dir.join("SKILL.md").exists());

        // 同名再建应报错，不覆盖。
        assert!(super::create_skill(Some(&cwd), true, "my-skill", "x").is_err());

        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].name, "my-skill");
        assert_eq!(scanned[0].description, "does a thing");

        let new_dir = super::update_skill(&dir, "renamed-skill", "new desc").unwrap();
        assert!(new_dir.join("SKILL.md").exists());
        assert!(!dir.exists());
        let scanned = super::scan_skills(Some(&cwd));
        let scanned: Vec<_> = scanned.into_iter().filter(|s| s.project_scope).collect();
        assert_eq!(scanned[0].name, "renamed-skill");
        assert_eq!(scanned[0].description, "new desc");

        super::delete_skill(&new_dir).unwrap();
        assert!(!new_dir.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
