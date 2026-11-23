// 项目的仓库对话框（NSH A2 版）：复用纯逻辑 `../repositories.js`。

import { Scroll, Text, View } from "nsh";
import { Checkbox, Dialog, Input } from "nsh/base";
import { SmeltButton, SmeltIcon } from "smelt/sdk";

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

function FieldInput(props: { field: MemoryInputState; label: string; disabled: boolean }) {
  const look = () => theme();
  return (
    <Input
      value={props.field.value()}
      placeholder={props.field.placeholder}
      disabled={props.disabled}
      accessibilityLabel={props.label}
      onChange={({ value }: { value: string }) => props.field.set_value(value)}
      onSubmit={() => props.field.submit()}
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

  return (
    <Dialog
      open={dialog() != null}
      animation="fade"
      closeOnBackdropPress={false}
      accessibilityLabel={dialog() ? t(TITLES[dialog().mode] ?? "repo.title_manage") : ""}
      backdropClass="bg-black/40"
      onOpenChange={({ open }) => {
        if (!open) dialog()?.dismiss();
      }}
      class="m-auto rounded-(--radius) border border-border bg-popover p-5"
      style={{ width: WIDTH, gap: 16 }}
    >
      {(() => {
        const d = dialog();
        if (!d) return null;
        const p = look().palette;
        const busy = Boolean(d.busy);
        const mode: string = d.mode;
        const task = d.task.value().trim() || t("repo.task_token");
        const submitDisabled = busy || (mode === "create" && d.repositories.length === 0);
        const repoName = d.repository_name();
        const destination = t(mode === "clone" ? "repo.clone_to" : "repo.bind_to", {
          root: d.project_root,
          name: repoName,
        });

        return (
          <View style={{ display: "flex", flexDirection: "column", gap: 14, width: "100%" }}>
            {/* 标题与项目路径 */}
            <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
              <Text
                style={{
                  fontSize: look().type.ui_medium.size + 1,
                  fontWeight: 600,
                  color: p.text,
                }}
              >
                {t(TITLES[mode] ?? "repo.title_manage")}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: p.muted, numberOfLines: 1 }}>
                {d.project_root}
              </Text>
            </View>

            {/* create 模式的任务/分支/基准表单 */}
            {mode === "create" ? (
              <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                <Text style={{ fontSize: look().type.ui.size, color: p.text }}>{t("repo.task")}</Text>
                <FieldInput field={d.task} label={t("repo.task")} disabled={busy} />
                <Text style={{ fontSize: look().type.text_xsmall.size, color: p.muted }}>
                  {t("repo.task_hint")}
                </Text>

                <Text style={{ fontSize: look().type.ui.size, color: p.text }}>{t("repo.branch")}</Text>
                <FieldInput field={d.branch} label={t("repo.branch")} disabled={busy} />
                <Text style={{ fontSize: look().type.text_xsmall.size, color: p.muted }}>
                  {t("repo.branch_hint", { task })}
                </Text>

                <Text style={{ fontSize: look().type.ui.size, color: p.text }}>{t("repo.base")}</Text>
                <FieldInput field={d.base} label={t("repo.base")} disabled={busy} />
              </View>
            ) : null}

            {/* manage / create 模式的仓库列表 + 入口按钮 */}
            {mode === "manage" || mode === "create" ? (
              <>
                {d.repositories.length === 0 ? (
                  <View style={{ paddingVertical: 12 }}>
                    <Text style={{ fontSize: look().type.ui.size, color: p.muted }}>
                      {t(busy ? "repo.loading" : "repo.empty")}
                    </Text>
                  </View>
                ) : (
                  <Scroll style={{ maxHeight: LIST_HEIGHT, width: "100%" }}>
                    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                      <Each of={d.repositories} key={(r: any) => r.name}>
                        {(repo: () => any) => {
                          const r = () => repo();
                          const referenced = () =>
                            d.worksets
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
                                : r().root === d.project_root
                                  ? "repo.source_root"
                                  : "repo.source_existing",
                            );
                          const checked = () => d.selected.has(r().name);
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
                                borderColor: p.hair,
                                borderRadius: look().radius.row,
                              }}
                            >
                              {mode === "create" ? (
                                <Checkbox
                                  checked={checked()}
                                  disabled={busy}
                                  accessibilityLabel={r().name}
                                  onChange={({ checked: next }: any) => d.toggle(r().name, Boolean(next))}
                                  indicatorStyle={{
                                    width: 16,
                                    height: 16,
                                    borderRadius: 4,
                                    borderWidth: 1,
                                    borderColor: checked() ? p.link : p.hair_strong,
                                    backgroundColor: checked() ? p.link : "transparent",
                                    display: "flex",
                                    alignItems: "center",
                                    justifyContent: "center",
                                  }}
                                  indicator={
                                    checked() ? (
                                      <SmeltIcon path={icon("check")} size={11} color={p.on_ink} />
                                    ) : undefined
                                  }
                                />
                              ) : null}

                              <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
                                <Text style={{ fontSize: look().type.ui.size, color: p.text }}>
                                  {`${r().name} · ${source()}`}
                                </Text>
                                <Text style={{ fontSize: look().type.text_xsmall.size, color: p.muted }}>
                                  {location()}
                                </Text>
                                {mode === "create" ? (
                                  <Text style={{ fontSize: look().type.text_xsmall.size, color: p.muted }}>
                                    {`${d.project_root}/worktrees/${task}/${r().name}`}
                                  </Text>
                                ) : null}
                              </View>

                              {mode === "manage" ? (
                                <SmeltButton
                                  quiet={true}
                                  small={true}
                                  label={t("repo.remove")}
                                  disabled={busy || referenced() !== ""}
                                  tooltip={
                                    referenced() === ""
                                      ? t("repo.remove_hint")
                                      : t("repo.in_use", { worksets: referenced() })
                                  }
                                  onPress={() => d.ask_remove(r().name)}
                                />
                              ) : null}
                            </View>
                          );
                        }}
                      </Each>
                    </View>
                  </Scroll>
                )}

                <View style={{ display: "flex", flexDirection: "row", gap: 8 }}>
                  <SmeltButton
                    small={true}
                    label={t("repo.bind_more")}
                    disabled={busy}
                    onPress={() => d.switch("bind")}
                  />
                  <SmeltButton
                    small={true}
                    label={t("repo.clone_more")}
                    disabled={busy}
                    onPress={() => d.switch("clone")}
                  />
                  {mode === "manage" ? (
                    <SmeltButton
                      small={true}
                      label={t("menu.new_workset")}
                      disabled={busy || d.repositories.length === 0}
                      onPress={() => d.switch("create")}
                    />
                  ) : null}
                </View>
              </>
            ) : (
              /* bind / clone 表单 */
              <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                {mode === "bind" ? (
                  <>
                    <Text style={{ fontSize: look().type.ui.size, color: p.muted }}>
                      {t("repo.bind_hint")}
                    </Text>
                    <View style={{ display: "flex", flexDirection: "row" }}>
                      <SmeltButton
                        small={true}
                        label={t("repo.choose_folder")}
                        disabled={busy}
                        onPress={() => d.choose_folder()}
                      />
                    </View>
                    <Text style={{ fontSize: look().type.ui.size, color: p.text }}>
                      {d.source ?? t("repo.no_folder")}
                    </Text>
                  </>
                ) : (
                  <>
                    <Text style={{ fontSize: look().type.ui.size, color: p.text }}>Git URL</Text>
                    <FieldInput field={d.url} label="Git URL" disabled={busy} />
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: p.muted }}>
                      {t("repo.clone_hint")}
                    </Text>
                  </>
                )}
                <Text style={{ fontSize: look().type.ui.size, color: p.text }}>{t("repo.name")}</Text>
                <FieldInput field={d.name} label={t("repo.name")} disabled={busy} />
                <Text style={{ fontSize: look().type.ui.size, color: p.muted }}>
                  {t("repo.name_line", { name: repoName || t("repo.name_token"), destination })}
                </Text>
              </View>
            )}

            {/* 取消关联二次确认 */}
            {d.pending_remove != null ? (
              <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                <Text style={{ fontSize: look().type.ui.size, color: p.text }}>
                  {t("repo.remove_question", { name: d.pending_remove })}
                </Text>
                <View style={{ display: "flex", flexDirection: "row", gap: 8 }}>
                  <SmeltButton
                    quiet={true}
                    small={true}
                    label={t("repo.keep")}
                    disabled={busy}
                    onPress={() => d.keep()}
                  />
                  <SmeltButton
                    danger={true}
                    small={true}
                    label={t("repo.confirm_remove")}
                    disabled={busy}
                    onPress={() => d.confirm_remove()}
                  />
                </View>
              </View>
            ) : null}

            {d.error != null ? (
              <Text style={{ fontSize: look().type.ui.size, color: p.bad }}>{d.error}</Text>
            ) : null}

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
              {d.back() != null ? (
                <SmeltButton
                  quiet={true}
                  small={true}
                  label={t("repo.back")}
                  disabled={busy}
                  onPress={() => d.go_back()}
                />
              ) : null}
              {mode !== "manage" ? (
                <SmeltButton
                  quiet={true}
                  small={true}
                  label={t("close.confirm")}
                  disabled={busy}
                  onPress={() => d.dismiss()}
                />
              ) : null}
              <SmeltButton
                primary={true}
                small={true}
                label={t(busy ? "repo.busy" : SUBMIT[mode] ?? "repo.done")}
                loading={busy}
                disabled={submitDisabled}
                onPress={() => d.submit()}
              />
            </View>
          </View>
        );
      })()}
    </Dialog>
  );
}
