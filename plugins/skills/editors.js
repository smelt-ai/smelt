// 列表里就地展开的两个表单：改作用范围、导入。
//
// 表单状态放在会话对象里，渲染函数只读它。表单画在列表视图里，点选的回调由列表传进来，
// 改完由列表重算预览、重绘自己。
//
// 不放进对话框：表单要跟着预览一起变，就地展开在被改的那一行下面，看得到改的是谁。

import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { SmeltAlert, SmeltCheckbox, SmeltRadio } from "smelt-ui";
import {
  message,
  scopeChoices,
  planScope,
  inspectImport,
  previewImport,
  pickSkillDirectory,
} from "./core.js";
import { SPACE, MONO, TRASH, button, hint, mono, pretty } from "./ui.js";
import { t } from "./i18n.js";

/** @typedef {{ home(): string, projectRoot(): string | null, installed(): any[] }} Env */

/// 「给谁」「层级」的表单和它的预览。预览异步算，只认最后一次的结果。
class FormSession {
  /** @param {Env} env */
  constructor(env) {
    this.env = env;
    this.form = { universal: true, labels: [], project_scope: false };
    this.choices = scopeChoices(env.installed(), false);
    // null 表示还在算。
    this.plan = null;
    this.token = 0;
  }

  request() {
    return {
      universal: this.form.universal,
      labels: [...this.form.labels],
      project_scope: this.form.project_scope,
    };
  }

  setForm(form) {
    this.form = form;
    if (this.form.project_scope && !this.env.projectRoot()) this.form.project_scope = false;
    this.choices = scopeChoices(this.env.installed(), this.form.project_scope);
    this.plan = null;
  }

  setUniversal(universal) {
    this.form.universal = universal;
  }

  /// 没有打开项目时不能选项目级，返回 false。
  setLevel(projectScope) {
    if (projectScope && !this.env.projectRoot()) return false;
    this.form.project_scope = projectScope;
    const choices = scopeChoices(this.env.installed(), projectScope);
    const allowed = new Set(choices.map((choice) => choice.label));
    this.form.labels = this.form.labels.filter((label) => allowed.has(label));
    this.choices = choices;
    return true;
  }

  toggleLabel(label) {
    if (this.form.universal) return;
    const labels = new Set(this.form.labels);
    if (labels.has(label)) labels.delete(label);
    else labels.add(label);
    this.form.labels = [...labels];
  }

  /// 这次要算的预览，子类给出；没什么可算时返回 null。
  /// @returns {Promise<{ steps: string[], replaces: boolean, error: string | null } | null>}
  async compute() {
    return null;
  }

  /// 重算预览。
  async refresh() {
    const token = ++this.token;
    this.plan = null;
    let plan;
    try {
      plan = await this.compute();
    } catch (error) {
      plan = { steps: [], replaces: false, error: message(error) };
    }
    if (token === this.token) this.plan = plan;
  }

  ready() {
    return this.plan !== null && !this.plan.error && this.plan.steps.length > 0;
  }
}

/// 改一个 skill 的作用范围。
export class ScopeSession extends FormSession {
  constructor(env) {
    super(env);
    this.skill = null;
  }

  /// 以 skill 现在的作用范围作表单初值。
  begin(skill) {
    this.skill = skill;
    this.token++;
    this.setForm({
      universal: skill.placement !== "agent",
      labels: skill.placement === "agent" && skill.agent ? [skill.agent] : [],
      project_scope: skill.project_scope,
    });
  }

  async compute() {
    if (this.skill === null) return null;
    const steps = await planScope(this.env.home(), this.env.projectRoot(), this.skill.dir, this.request());
    return { steps, replaces: false, error: null };
  }
}

/// 导入一个目录：来源、装到哪里、将执行。
export class ImportSession extends FormSession {
  constructor(env) {
    super(env);
    this.source = null;
    this.sourceInfo = null;
    this.picking = false;
  }

  reset() {
    this.token++;
    this.source = null;
    this.sourceInfo = null;
    this.picking = false;
    this.setForm({ universal: true, labels: [], project_scope: false });
  }

  async pick() {
    const source = await pickSkillDirectory();
    if (!source) return;
    this.sourceInfo = await inspectImport(this.env.home(), source);
    this.source = source;
  }

  async compute() {
    if (this.source === null) return null;
    const preview = await previewImport(this.env.home(), this.env.projectRoot(), this.request(), this.source);
    return { ...preview, error: null };
  }

  ready() {
    return this.source !== null && this.plan !== null && !this.plan.error;
  }
}

/// 「给谁」「层级」两行单选，指定 agent 时加一排勾选。两个表单共用。表单变了调
/// `changed(cx)`，由调用方重算预览、重绘。
/// @param {FormSession} session
/// @param {(cx: any) => void} changed
export function scopeForm(session, cx, changed) {
  const form = session.form;
  const projectRoot = session.env.projectRoot();
  // 只列装了的 agent。已经勾上的留着：skill 现在就放在一个没装的 agent 目录里时，
  // 藏掉它等于悄悄改了作用范围。
  const choices = (session.choices ?? []).filter(
    (choice) => choice.installed || form.labels.includes(choice.label),
  );
  const line = (label, controls) =>
    h_flex()
      .items_center()
      .gap(SPACE.sm)
      .child(div().w(40).flex_none().child(hint(label, cx)))
      .child(h_flex().flex_wrap().gap(SPACE.md).children(controls));
  /// 一行里的一个单选：选中由表单定，点了交给 `pick(cx)`。
  const radio = (id, label, checked, pick) =>
    SmeltRadio.new(id).label(label).checked(checked).on_select((cx) => pick(cx));
  return [
    line(t("scope.for"), [
      radio("scope-kind-universal", t("scope.universal_all"), form.universal, (cx) => {
        session.setUniversal(true);
        changed(cx);
      }),
      radio("scope-kind-agent", t("scope.specific"), !form.universal, (cx) => {
        session.setUniversal(false);
        changed(cx);
      }),
    ]),
    ...(form.universal
      ? []
      : [
          h_flex()
            .pl(48)
            .flex_wrap()
            .gap(SPACE.sm)
            .children(
              choices.map((choice) =>
                SmeltCheckbox.new(`agent-${choice.label}`)
                  .label(choice.installed ? choice.label : t("scope.not_installed", { agent: choice.label }))
                  .checked(form.labels.includes(choice.label))
                  .on_change((_checked, cx) => {
                    session.toggleLabel(choice.label);
                    changed(cx);
                  }),
              ),
            ),
        ]),
    line(t("scope.level"), [
      radio("scope-level-user", t("scope.user"), !form.project_scope, (cx) => {
        session.setLevel(false);
        changed(cx);
      }),
      // 没有打开项目时选不了。
      projectRoot
        ? radio("scope-level-project", t("scope.project_at", { path: pretty(session.env.home(), projectRoot) }), form.project_scope, (cx) => {
            if (session.setLevel(true)) changed(cx);
            else cx.notify();
          })
        : SmeltRadio.new("scope-level-project").label(t("scope.project_none")).disabled(true),
    ]),
  ];
}

/// 预览：算着、出错、无事可做，或要执行的步骤。
/// @param {FormSession} session
export function planView(session, idle, cx) {
  const colors = cx.theme().colors;
  const plan = session.plan;
  if (plan === null) return hint(t("plan.computing"), cx);
  if (plan.error) return div().text_xs().whitespace_normal().text_color(colors.destructive).child(plan.error);
  if (plan.steps.length === 0) return hint(idle, cx);
  return v_flex()
    .gap(2)
    .child(hint(t("plan.title"), cx))
    .children(plan.steps.map((step) => mono(step)));
}

/// 导入的「来源」：还没选时是一个按钮，选好了显示名字、路径和「换一个」。
/// @param {ImportSession} session
/// @param {(cx: any) => void} pick
export function importSource(session, cx, pick) {
  const colors = cx.theme().colors;
  const info = session.sourceInfo;
  if (session.source === null) {
    return v_flex()
      .gap(SPACE.sm)
      .child(hint(t("import.source_hint"), cx))
      .child(h_flex().child(button("pick", t("import.pick"), pick, "primary").loading(session.picking)));
  }
  return v_flex()
    .gap(SPACE.xs)
    .child(
      h_flex()
        .items_center()
        .gap(SPACE.sm)
        .child(
          v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_sm().font_semibold().child(info ? info.name : ""))
            .child(
              div()
                .text_xs()
                .font_family(MONO)
                .truncate()
                .text_color(colors.muted_foreground)
                .child(pretty(session.env.home(), session.source)),
            ),
        )
        .child(button("repick", t("import.repick"), pick, "secondary").loading(session.picking)),
    )
    .when(Boolean(info && info.description), (el) => el.child(hint(info.description, cx)));
}

/// 导入前要提醒的事：会替换同名技能，或会在项目里盖过用户级的同名技能。
/// @param {ImportSession} session
export function importWarnings(session) {
  const info = session.sourceInfo;
  const warnings = [];
  if (session.plan !== null && session.plan.replaces) {
    warnings.push(SmeltAlert.new("replaces", t("import.replaces", { trash: TRASH })).need());
  }
  if (info && info.has_global && session.form.project_scope) {
    warnings.push(
      SmeltAlert.new(
        "global",
        t("import.shadows_user", { name: info.name }),
      ).need(),
    );
  }
  return warnings;
}
