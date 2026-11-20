// 项目的仓库对话框的样子（状态和规则在 repositories.js）。插件的根画它（见 main.js），叠在已开的
// 对话框上面：从新建会话那张里打开时，关了回到那里。

import { div } from "gpui-kit";
import { t, theme } from "smelt-plugin-api";
import { SmeltButton, SmeltCheckbox, SmeltDialog, SmeltScrollView, SmeltTextInput } from "smelt-ui";
import { typed } from "./util.js";

/** 卡片宽度。 */
const WIDTH = 620;
/** 仓库列表最高多高，再多就在里面滚。 */
const LIST_HEIGHT = 260;

const TITLES = {
  manage: "repo.title_manage",
  create: "repo.title_create",
  bind: "repo.title_bind",
  clone: "repo.title_clone",
};

const SUBMIT = {
  manage: "repo.done",
  create: "repo.create",
  bind: "repo.bind",
  clone: "repo.clone",
};

/** @param {import("./repositories.js").Repositories} dialog */
export function repositories_dialog(dialog) {
  const look = theme();
  const { busy, mode } = dialog;
  const task = dialog.task.value().trim() || t("repo.task_token");
  const field_label = (text) => typed(div(), look.type.ui).child(text);
  const hint = (text) => typed(div(), look.type.text_xsmall).text_color(look.palette.muted).child(text);
  const input = (state, label) => SmeltTextInput.new(state).aria_label(label).disabled(busy);

  let content = div().flex().flex_col().gap(16);
  if (mode === "create") {
    content = content
      .child(field_label(t("repo.task")))
      .child(input(dialog.task, t("repo.task")))
      .child(hint(t("repo.task_hint")))
      .child(field_label(t("repo.branch")))
      .child(input(dialog.branch, t("repo.branch")))
      .child(hint(t("repo.branch_hint", { task })));
  }
  if (mode === "manage" || mode === "create") {
    content = content.child(repository_list(dialog, task, look, hint)).child(entries(dialog));
  } else {
    content = form(dialog, content, look, field_label, hint, input);
  }
  if (dialog.pending_remove != null) content = content.child(remove_question(dialog, look));
  if (dialog.error != null) content = content.child(typed(div(), look.type.ui).text_color(look.palette.bad).child(dialog.error));

  return SmeltDialog.new("project-repositories")
    .open(true)
    .title(t(TITLES[mode]))
    .description(dialog.project_root)
    .w(WIDTH)
    .close_on_backdrop(false)
    .content(content)
    .footer(footer(dialog))
    .on_confirm(() => dialog.submit())
    // 写入跑着时不能关：Esc 不理。
    .on_dismiss(() => dialog.dismiss());
}

function repository_list(dialog, task, look, hint) {
  const { busy, mode } = dialog;
  const rows = div().flex().flex_col().gap_2();
  if (dialog.repositories.length === 0) {
    return rows.child(
      typed(div().py_4(), look.type.ui)
        .text_color(look.palette.muted)
        .child(t(busy ? "repo.loading" : "repo.empty")),
    );
  }
  const children = dialog.repositories.map((repo) => {
    const name = repo.name;
    const referenced = dialog.worksets
      .filter((workset) => workset.repositories.some((r) => r.name === repo.name || r.root === repo.root))
      .map((workset) => workset.task)
      .join("、");
    const location = repo.managed_link != null ? `${repo.managed_link} → ${repo.root}` : repo.root;
    const source = t(
      repo.managed_link != null ? "repo.source_link" : repo.root === dialog.project_root ? "repo.source_root" : "repo.source_existing",
    );
    return div()
      .flex()
      .items_center()
      .gap_3()
      .py_2()
      .px_2()
      .border_1()
      .border_color(look.palette.hair)
      .rounded(look.radius.row)
      .when(mode === "create", (row) =>
        row.child(
          SmeltCheckbox.new(`repo-check-${name}`)
            .checked(dialog.selected.has(name))
            .disabled(busy)
            .accessibility_label(name)
            .on_change((checked) => dialog.toggle(name, checked)),
        ),
      )
      .child(
        div()
          .flex()
          .flex_col()
          .flex_1()
          .min_w_0()
          .gap_1()
          .child(typed(div(), look.type.ui).child(`${name} · ${source}`))
          .child(hint(location))
          .when(mode === "create", (detail) => detail.child(hint(`${dialog.project_root}/worktrees/${task}/${name}`))),
      )
      .when(mode === "manage", (row) =>
        row.child(
          SmeltButton.new(`repo-remove-${name}`)
            .quiet()
            .small()
            .label(t("repo.remove"))
            .disabled(busy || referenced !== "")
            .tooltip(referenced === "" ? t("repo.remove_hint") : t("repo.in_use", { worksets: referenced }))
            .on_click(() => dialog.ask_remove(name)),
        ),
      );
  });
  return SmeltScrollView.new("project-repositories-list").max_h(LIST_HEIGHT).child(rows.children(children));
}

/** 列表下面的入口：纳入已有的、克隆，管理时还有建工作树。 */
function entries(dialog) {
  const busy = dialog.busy;
  return div()
    .flex()
    .gap_2()
    .child(SmeltButton.new("repo-bind-entry").label(t("repo.bind_more")).disabled(busy).on_click(() => dialog.switch("bind")))
    .child(SmeltButton.new("repo-clone-entry").label(t("repo.clone_more")).disabled(busy).on_click(() => dialog.switch("clone")))
    .when(dialog.mode === "manage", (row) =>
      row.child(
        SmeltButton.new("repo-create-entry")
          .label(t("menu.new_workset"))
          .disabled(busy || dialog.repositories.length === 0)
          .on_click(() => dialog.switch("create")),
      ),
    );
}

function form(dialog, content, look, field_label, hint, input) {
  const busy = dialog.busy;
  if (dialog.mode === "bind") {
    content = content
      .child(typed(div(), look.type.ui).text_color(look.palette.muted).child(t("repo.bind_hint")))
      .child(
        div().flex().child(
          SmeltButton.new("repo-choose-folder")
            .label(t("repo.choose_folder"))
            .disabled(busy)
            .on_click(() => dialog.choose_folder()),
        ),
      )
      .child(typed(div(), look.type.ui).child(dialog.source ?? t("repo.no_folder")));
  } else {
    content = content
      .child(field_label("Git URL"))
      .child(input(dialog.url, "Git URL"))
      .child(hint(t("repo.clone_hint")));
  }
  const name = dialog.repository_name();
  const destination = t(dialog.mode === "clone" ? "repo.clone_to" : "repo.bind_to", { root: dialog.project_root, name });
  return content
    .child(field_label(t("repo.name")))
    .child(input(dialog.name, t("repo.name")))
    .child(
      typed(div(), look.type.ui)
        .text_color(look.palette.muted)
        .child(t("repo.name_line", { name: name || t("repo.name_token"), destination })),
    );
}

function remove_question(dialog, look) {
  const busy = dialog.busy;
  return div()
    .flex()
    .flex_col()
    .gap_2()
    .child(typed(div(), look.type.ui).child(t("repo.remove_question", { name: dialog.pending_remove })))
    .child(
      div()
        .flex()
        .gap_2()
        .child(SmeltButton.new("repo-remove-cancel").quiet().label(t("repo.keep")).disabled(busy).on_click(() => dialog.keep()))
        .child(
          SmeltButton.new("repo-remove-confirm")
            .danger()
            .label(t("repo.confirm_remove"))
            .disabled(busy)
            .on_click(() => dialog.confirm_remove()),
        ),
    );
}

function footer(dialog) {
  const { busy, mode } = dialog;
  const submit_disabled = busy || (mode === "create" && dialog.repositories.length === 0);
  return div()
    .flex()
    .w_full()
    .justify_end()
    .gap_2()
    .when(dialog.back() != null, (row) =>
      row.child(
        SmeltButton.new("repo-form-back").quiet().small().label(t("repo.back")).disabled(busy).on_click(() => dialog.go_back()),
      ),
    )
    .when(mode !== "manage", (row) =>
      row.child(
        SmeltButton.new("repo-modal-close").quiet().small().label(t("close.confirm")).disabled(busy).on_click(() => dialog.dismiss()),
      ),
    )
    .child(
      SmeltButton.new("repo-modal-submit")
        .primary()
        .small()
        .label(t(busy ? "repo.busy" : SUBMIT[mode]))
        .loading(busy)
        .disabled(submit_disabled)
        .on_click(() => dialog.submit()),
    );
}
