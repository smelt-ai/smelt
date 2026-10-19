// 列表里就地展开的两个表单：改作用范围、导入。
//
// 表单状态放在会话对象里，渲染函数只读它。表单画在列表视图里，点选的回调由列表传进来，
// 改完由列表重算预览、重绘自己。
//
// 不放进 `Dialog`：gpui-shell 给脚本的 `Dialog` 内容在打开时定格，而且不画按钮、不能
// 指定位置（只开放了标题和确定、取消、关闭三个回调）。

import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { Checkbox, HorizontalRadioGroup, Radio, WarningAlert } from "gpui-component";
import {
  message,
  scopeChoices,
  planScope,
  inspectImport,
  previewImport,
  pickSkillDirectory,
} from "./core.js";
import { SPACE, MONO, TRASH, button, hint, mono, pretty } from "./ui.js";

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
  const line = (label, control) =>
    h_flex()
      .items_center()
      .gap(SPACE.sm)
      .child(div().w(40).flex_none().child(hint(label, cx)))
      .child(control);
  return [
    line(
      "给谁",
      HorizontalRadioGroup.new("scope-kind")
        .selected_index(form.universal ? 0 : 1)
        .on_change((index, cx) => {
          session.setUniversal(index === 0);
          changed(cx);
        })
        .child(Radio.new("scope-kind-universal").label("所有 agent（通用）"))
        .child(Radio.new("scope-kind-agent").label("仅指定 agent")),
    ),
    ...(form.universal
      ? []
      : [
          h_flex()
            .pl(48)
            .flex_wrap()
            .gap(SPACE.sm)
            .children(
              choices.map((choice) =>
                Checkbox.new(`agent-${choice.label}`)
                  .label(choice.installed ? choice.label : `${choice.label}（未安装）`)
                  .checked(form.labels.includes(choice.label))
                  .on_change((_checked, cx) => {
                    session.toggleLabel(choice.label);
                    changed(cx);
                  }),
              ),
            ),
        ]),
    line(
      "层级",
      HorizontalRadioGroup.new("scope-level")
        .selected_index(form.project_scope ? 1 : 0)
        .on_change((index, cx) => {
          if (session.setLevel(index === 1)) changed(cx);
          else cx.notify();
        })
        .child(Radio.new("scope-level-user").label("用户级"))
        .child(
          Radio.new("scope-level-project").label(
            projectRoot ? `项目级（${pretty(session.env.home(), projectRoot)}）` : "项目级（没有打开项目）",
          ),
        ),
    ),
  ];
}

/// 预览：算着、出错、无事可做，或要执行的步骤。
/// @param {FormSession} session
export function planView(session, idle, cx) {
  const colors = cx.theme().colors;
  const plan = session.plan;
  if (plan === null) return hint("正在计算…", cx);
  if (plan.error) return div().text_xs().whitespace_normal().text_color(colors.destructive).child(plan.error);
  if (plan.steps.length === 0) return hint(idle, cx);
  return v_flex()
    .gap(2)
    .child(hint("将执行", cx))
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
      .child(hint("选一个含 SKILL.md 的目录，整个目录会被复制进来。只能是主目录里的目录。", cx))
      .child(h_flex().child(button("pick", "选择目录…", pick, "primary").loading(session.picking)));
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
        .child(button("repick", "换一个…", pick, "outline").loading(session.picking)),
    )
    .when(Boolean(info && info.description), (el) => el.child(hint(info.description, cx)));
}

/// 导入前要提醒的事：会替换同名技能，或会在项目里盖过用户级的同名技能。
/// @param {ImportSession} session
export function importWarnings(session) {
  const info = session.sourceInfo;
  const warnings = [];
  if (session.plan !== null && session.plan.replaces) {
    warnings.push(WarningAlert.new("replaces", `已有同名技能，导入会替换它，旧版本移进 ~/${TRASH}。`).size("small"));
  }
  if (info && info.has_global && session.form.project_scope) {
    warnings.push(
      WarningAlert.new(
        "global",
        `用户级已有同名「${info.name}」。装到项目级会在这个项目里盖过它；想更新用户级那份，把层级改成用户级。`,
      ).size("small"),
    );
  }
  return warnings;
}
