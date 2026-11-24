// 项目的仓库对话框（NSH A2 版）：复用纯逻辑 `../repositories.js`。

import { computed, Scroll, Text, View } from "nsh";
import { Checkbox, Input } from "nsh/base";
import { DIALOG_SURFACE_CLASS, SmeltButton, SmeltDialog, SmeltIcon } from "smelt/sdk";

import { icon } from "../util.js";
import { list, MemoryInputState, t, theme, tick } from "./bridge.ts";

const WIDTH = 620;
const LIST_HEIGHT = 260;

const TITLES: Record<string, string> = {
  manage: "repo.title_manage",
  create: "repo.title_create",
  bind: "repo.title_bind",
  clone: "repo.title_clone",
};

const SUBMIT: Record<string, string> = {
  manage: "repo.done",
  create: "repo.create",
  bind: "repo.bind",
  clone: "repo.clone",
};

function FieldInput(props: {
  field: () => MemoryInputState | null | undefined;
  label: string;
  disabled: boolean;
  autoFocus?: boolean;
  onCancel?: () => void;
}) {
  const look = () => theme();
  const value = () => {
    tick();
    return props.field()?.value() ?? "";
  };
  return (
    <Input
      ref={(el: any) => {
        if (props.autoFocus) el?.focus?.();
      }}
      autoFocus={Boolean(props.autoFocus)}
      value={value()}
      placeholder={props.field()?.placeholder ?? ""}
      disabled={props.disabled}
      accessibilityLabel={props.label}
      onChange={({ value: next }: { value: string }) => props.field()?.set_value(next)}
      onSubmit={() => props.field()?.submit()}
      onKeyPress={(e: any) => {
        if (e?.key === "Escape") props.onCancel?.();
      }}
      class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
      style={{
        height: 30,
        fontSize: look().type.ui.size,
        lineHeight: look().type.ui.line_height,
      }}
    />
  );
}

export function RepositoriesDialog() {
  const look = () => theme();
  const dialog = () => {
    tick();
    return list.repositories;
  };
  const isOpen = computed(() => {
    tick();
    return list.repositories != null;
  });
  const mode = computed<string>(() => dialog()?.mode ?? "manage");
  const busy = computed(() => Boolean(dialog()?.busy));
  const repos = computed(() => dialog()?.repositories ?? []);
  const taskToken = computed(() => dialog()?.task?.value()?.trim() || t("repo.task_token"));
  const submitDisabled = computed(() => busy() || (mode() === "create" && repos().length === 0));
  const repoName = computed(() => dialog()?.repository_name?.() ?? "");
  const destination = computed(() => {
    const d = dialog();
    if (!d) return "";
    return t(mode() === "clone" ? "repo.clone_to" : "repo.bind_to", {
      root: d.project_root,
      name: repoName(),
    });
  });

  return (
    <SmeltDialog
      open={isOpen()}
      closeOnBackdropPress={false}
      accessibilityLabel={isOpen() ? t(TITLES[mode()] ?? "repo.title_manage") : ""}
      onOpenChange={({ open }) => {
        if (!open) dialog()?.dismiss();
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS} p-5`}
      style={{ width: WIDTH, gap: 16 }}
    >
      <If when={isOpen()}>
        <View style={{ display: "flex", flexDirection: "column", gap: 14, width: "100%" }}>
          {/* 标题与项目路径 */}
          <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <Text
              style={{
                fontSize: look().type.ui_medium.size + 1,
                fontWeight: 600,
                color: look().palette.text,
              }}
            >
              {t(TITLES[mode()] ?? "repo.title_manage")}
            </Text>
            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted, numberOfLines: 1 }}>
              {dialog()?.project_root ?? ""}
            </Text>
          </View>

          {/* create 模式的任务/分支/基准表单 */}
          <If when={mode() === "create"}>
            <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>{t("repo.task")}</Text>
              <FieldInput
                field={() => dialog()?.task}
                label={t("repo.task")}
                disabled={busy()}
                autoFocus={true}
                onCancel={() => dialog()?.dismiss()}
              />
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("repo.task_hint")}
              </Text>

              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>{t("repo.branch")}</Text>
              <FieldInput
                field={() => dialog()?.branch}
                label={t("repo.branch")}
                disabled={busy()}
                onCancel={() => dialog()?.dismiss()}
              />
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("repo.branch_hint", { task: taskToken() })}
              </Text>

              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>{t("repo.base")}</Text>
              <FieldInput
                field={() => dialog()?.base}
                label={t("repo.base")}
                disabled={busy()}
                onCancel={() => dialog()?.dismiss()}
              />
            </View>
          </If>

          {/* manage / create 模式的仓库列表 + 入口按钮 */}
          <If
            when={mode() === "manage" || mode() === "create"}
            else={
              /* bind / clone 表单 */
              <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                <If
                  when={mode() === "bind"}
                  else={
                    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                      <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>Git URL</Text>
                      <FieldInput
                        field={() => dialog()?.url}
                        label="Git URL"
                        disabled={busy()}
                        autoFocus={true}
                        onCancel={() => dialog()?.dismiss()}
                      />
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                        {t("repo.clone_hint")}
                      </Text>
                    </View>
                  }
                >
                  <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                    <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                      {t("repo.bind_hint")}
                    </Text>
                    <View style={{ display: "flex", flexDirection: "row" }}>
                      <SmeltButton
                        small={true}
                        label={t("repo.choose_folder")}
                        disabled={busy()}
                        onPress={() => dialog()?.choose_folder()}
                      />
                    </View>
                    <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                      {dialog()?.source ?? t("repo.no_folder")}
                    </Text>
                  </View>
                </If>
                <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>{t("repo.name")}</Text>
                <FieldInput
                  field={() => dialog()?.name}
                  label={t("repo.name")}
                  disabled={busy()}
                  autoFocus={mode() === "bind"}
                  onCancel={() => dialog()?.dismiss()}
                />
                <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                  {t("repo.name_line", { name: repoName() || t("repo.name_token"), destination: destination() })}
                </Text>
              </View>
            }
          >
            <View style={{ display: "flex", flexDirection: "column", gap: 14 }}>
              <If
                when={repos().length > 0}
                else={
                  <View style={{ paddingVertical: 12 }}>
                    <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                      {t(busy() ? "repo.loading" : "repo.empty")}
                    </Text>
                  </View>
                }
              >
                <Scroll style={{ maxHeight: LIST_HEIGHT, width: "100%" }}>
                  <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                    <Each of={repos()} key={(r: any) => r.name}>
                      {(repo: () => any) => {
                        const r = () => repo();
                        const referenced = () =>
                          (dialog()?.worksets ?? [])
                            .filter((w: any) =>
                              w.repositories.some((wr: any) => wr.name === r().name || wr.root === r().root),
                            )
                            .map((w: any) => w.task)
                            .join("、");
                        const location = () =>
                          r().managed_link != null ? `${r().managed_link} → ${r().root}` : r().root;
                        const source = () =>
                          t(
                            r().managed_link != null
                              ? "repo.source_link"
                              : r().root === dialog()?.project_root
                                ? "repo.source_root"
                                : "repo.source_existing",
                          );
                        const checked = () => Boolean(dialog()?.selected?.has(r().name));
                        return (
                          <View
                            style={{
                              display: "flex",
                              flexDirection: "row",
                              alignItems: "center",
                              gap: 10,
                              paddingHorizontal: 10,
                              paddingVertical: 8,
                              borderWidth: 1,
                              borderColor: look().palette.hair,
                              borderRadius: look().radius.row,
                            }}
                          >
                            <If when={mode() === "create"}>
                              <Checkbox
                                checked={checked()}
                                disabled={busy()}
                                accessibilityLabel={r().name}
                                onChange={({ checked: next }: any) => dialog()?.toggle(r().name, Boolean(next))}
                                indicatorStyle={{
                                  width: 16,
                                  height: 16,
                                  borderRadius: 4,
                                  borderWidth: 1,
                                  borderColor: checked() ? look().palette.link : look().palette.hair_strong,
                                  backgroundColor: checked() ? look().palette.link : "transparent",
                                  display: "flex",
                                  alignItems: "center",
                                  justifyContent: "center",
                                }}
                                indicator={
                                  checked() ? (
                                    <SmeltIcon path={icon("check")} size={11} color={look().palette.on_ink} />
                                  ) : undefined
                                }
                              />
                            </If>

                            <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
                              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                                {`${r().name} · ${source()}`}
                              </Text>
                              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                                {location()}
                              </Text>
                              <If when={mode() === "create"}>
                                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                                  {`${dialog()?.project_root ?? ""}/worktrees/${taskToken()}/${r().name}`}
                                </Text>
                              </If>
                            </View>

                            <If when={mode() === "manage"}>
                              <SmeltButton
                                quiet={true}
                                small={true}
                                label={t("repo.remove")}
                                disabled={busy() || referenced() !== ""}
                                tooltip={
                                  referenced() === ""
                                    ? t("repo.remove_hint")
                                    : t("repo.in_use", { worksets: referenced() })
                                }
                                onPress={() => dialog()?.ask_remove(r().name)}
                              />
                            </If>
                          </View>
                        );
                      }}
                    </Each>
                  </View>
                </Scroll>
              </If>

              <View style={{ display: "flex", flexDirection: "row", gap: 8 }}>
                <SmeltButton
                  small={true}
                  label={t("repo.bind_more")}
                  disabled={busy()}
                  onPress={() => dialog()?.switch("bind")}
                />
                <SmeltButton
                  small={true}
                  label={t("repo.clone_more")}
                  disabled={busy()}
                  onPress={() => dialog()?.switch("clone")}
                />
                <If when={mode() === "manage"}>
                  <SmeltButton
                    small={true}
                    label={t("menu.new_workset")}
                    disabled={busy() || repos().length === 0}
                    onPress={() => dialog()?.switch("create")}
                  />
                </If>
              </View>
            </View>
          </If>

          {/* 取消关联二次确认 */}
          <If when={dialog()?.pending_remove != null}>
            <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                {t("repo.remove_question", { name: dialog()?.pending_remove ?? "" })}
              </Text>
              <View style={{ display: "flex", flexDirection: "row", gap: 8 }}>
                <SmeltButton
                  quiet={true}
                  small={true}
                  label={t("repo.keep")}
                  disabled={busy()}
                  onPress={() => dialog()?.keep()}
                />
                <SmeltButton
                  danger={true}
                  small={true}
                  label={t("repo.confirm_remove")}
                  disabled={busy()}
                  onPress={() => dialog()?.confirm_remove()}
                />
              </View>
            </View>
          </If>

          <If when={dialog()?.error != null}>
            <Text style={{ fontSize: look().type.ui.size, color: look().palette.bad }}>
              {dialog()?.error ?? ""}
            </Text>
          </If>

          {/* 底部按钮行 */}
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              justifyContent: "flex-end",
              alignItems: "center",
              gap: 8,
              paddingTop: 4,
            }}
          >
            <If when={dialog()?.back?.() != null}>
              <SmeltButton
                quiet={true}
                small={true}
                label={t("repo.back")}
                disabled={busy()}
                onPress={() => dialog()?.go_back()}
              />
            </If>
            <If when={mode() !== "manage"}>
              <SmeltButton
                quiet={true}
                small={true}
                label={t("close.confirm")}
                disabled={busy()}
                onPress={() => dialog()?.dismiss()}
              />
            </If>
            <SmeltButton
              primary={true}
              small={true}
              label={t(busy() ? "repo.busy" : (SUBMIT[mode()] ?? "repo.done"))}
              loading={busy()}
              disabled={submitDisabled()}
              onPress={() => dialog()?.submit()}
            />
          </View>
        </View>
      </If>
    </SmeltDialog>
  );
}
