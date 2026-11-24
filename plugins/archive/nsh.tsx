// 归档（NSH A2 独立 VM 版本）：复用纯逻辑 model.js 与 contract.js。
import { computed, Scroll, signal, Text, View } from "nsh";
import { Radio } from "nsh/base";
import {
  bootTask,
  call,
  DIALOG_SURFACE_CLASS,
  initHost,
  machineMeta as hostMachines,
  onSlices,
  pathClean,
  pathParent,
  pathStyle,
  projectLabels,
  projectRootOf,
  registerViews,
  run,
  safeArea,
  SmeltButton,
  SmeltDialog,
  SmeltIcon,
  SmeltPopover,
  SmeltTooltip,
  t,
  text,
  theme,
  workspaceDirectory,
} from "smelt/sdk";

import { BUTTON, BUTTON_NAME } from "./contract.js";
import { busy_key, machine_group, same_state } from "./model.js";

const ACTIVATE = "smelt.sessions/activate";
const POPOVER_WIDTH = 320;
const LIST_MAX_HEIGHT = 420;
const CHOICES_MAX_HEIGHT = 240;
const CONFIRM_WIDTH = 440;

const pathHelper = {
  clean: pathClean,
  parent: pathParent,
  project_root_of: projectRootOf,
  labels: projectLabels,
};

const archivedMachines = signal<any[]>([]);
const busySet = signal<Set<string>>(new Set());

function setBusy(key: string, active: boolean) {
  const next = new Set(busySet());
  if (active) next.add(key);
  else next.delete(key);
  busySet.set(next);
}

function rebuild() {
  const now = Math.floor(Date.now() / 1000);
  const next: any[] = [];
  for (const machine of hostMachines()) {
    const dir = workspaceDirectory(machine.machine);
    if (dir == null) continue;
    const group = machine_group(pathHelper, t, machine, pathStyle(machine.machine), dir, now);
    if (group != null) next.push(group);
  }
  if (!same_state(next, archivedMachines())) {
    archivedMachines.set(next);
  }
}

function msgOf(error: any): string {
  return error?.message ?? String(error);
}

function notifyError(body: string) {
  console.error(`[archive] ${body}`);
  run("smelt.app/notify", { subtitle: t("notify.error"), body }).catch(() => {});
}

async function restoreRow(machine: string | null, row: any, targetWorksetId: string | null) {
  const key = busy_key(machine, row.id);
  if (busySet().has(key)) return;
  setBusy(key, true);
  let revision: any;
  try {
    revision = await call(machine, "workspace.conversation.archive", {
      conversation_id: row.id,
      archived: false,
      target_workset_id: targetWorksetId,
    });
  } catch (error) {
    notifyError(t("restore.failed", { title: row.title, error: msgOf(error) }));
    return;
  } finally {
    setBusy(key, false);
  }
  if (row.session_id != null) {
    run(ACTIVATE, { machine, session_id: row.session_id, after: revision }).catch((error) =>
      notifyError(msgOf(error)),
    );
  }
}

async function removeRow(machine: string | null, row: any) {
  const key = busy_key(machine, row.id);
  if (busySet().has(key)) return;
  setBusy(key, true);
  try {
    await call(machine, "workspace.conversation.delete", { conversation_id: row.id });
  } catch (error) {
    notifyError(t("delete.failed", { title: row.title, error: msgOf(error) }));
  } finally {
    setBusy(key, false);
  }
}

onSlices(["machines_meta", "sessions_data", "locale"], () => rebuild());

bootTask(async () => {
  await initHost(["machines_meta", "sessions_data"]);
  rebuild();
  await registerViews([
    {
      name: BUTTON_NAME,
      export: BUTTON_NAME,
      title: text("view.title"),
      icon: "icons/archive.svg",
      draws_top_bar: true,
    },
  ]);
});

type DialogState =
  | { kind: "delete"; machine: string | null; row: any }
  | { kind: "restore"; machine: string | null; row: any; picked: string | null };

export function button() {
  const popoverOpen = signal(false);
  const dialog = signal<DialogState | null>(null);
  const look = theme;
  const insets = computed(() => safeArea(BUTTON).bar_insets);
  const list = computed(() => archivedMachines());
  const totalCount = computed(() => {
    let n = 0;
    for (const m of list()) for (const p of m.projects) n += p.rows.length;
    return n;
  });

  const requestRestore = (machine: string | null, row: any) => {
    popoverOpen.set(false);
    if (row.closed_workset != null) {
      dialog.set({
        kind: "restore",
        machine,
        row,
        picked: row.closed_workset.candidates[0]?.id ?? null,
      });
    } else {
      void restoreRow(machine, row, null);
    }
  };

  const requestDelete = (machine: string | null, row: any) => {
    popoverOpen.set(false);
    dialog.set({ kind: "delete", machine, row });
  };

  const confirmDialog = () => {
    const cur = dialog();
    if (!cur) return;
    if (cur.kind === "delete") {
      dialog.set(null);
      void removeRow(cur.machine, cur.row);
    } else if (cur.picked != null) {
      dialog.set(null);
      void restoreRow(cur.machine, cur.row, cur.picked);
    }
  };

  return (
    <View
      style={{
        height: "100%",
        paddingLeft: list().length > 0 ? insets().left : 0,
        paddingRight: list().length > 0 ? insets().right : 0,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
      }}
    >
      <If when={list().length > 0}>
        <SmeltPopover
          open={popoverOpen()}
          onOpenChange={({ open }: any) => popoverOpen.set(Boolean(open))}
          placement="top"
          align="start"
          offset={6}
          popupClass="rounded-xl border border-border bg-popover p-1 shadow-lg"
          popupStyle={{ width: POPOVER_WIDTH }}
          trigger={
            <SmeltButton
              quiet={true}
              selected={popoverOpen()}
              icon="icons/archive.svg"
              accessibilityLabel={t("title")}
              tooltip={t("title")}
            />
          }
        >
          <View style={{ display: "flex", flexDirection: "column", width: "100%" }}>
            <View
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 8,
                paddingLeft: 8,
                paddingRight: 8,
                paddingTop: 4,
                paddingBottom: 6,
              }}
            >
              <Text
                style={{
                  fontSize: look().type.ui_medium.size,
                  lineHeight: look().type.ui_medium.line_height,
                  fontWeight: look().type.ui_medium.weight,
                  color: look().palette.text,
                }}
              >
                {t("title")}
              </Text>
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  color: look().palette.muted,
                }}
              >
                {String(totalCount())}
              </Text>
            </View>
            <Scroll style={{ maxHeight: LIST_MAX_HEIGHT, width: "100%" }}>
              <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 8 }}>
                <Each of={list()} key={(m: any) => m.machine ?? "local"}>
                  {(machine: () => any) => {
                    const p = () => look().palette;
                    const byMachine = () => list().length > 1;
                    return (
                      <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 6 }}>
                        {byMachine() ? (
                          <View
                            style={{
                              display: "flex",
                              flexDirection: "row",
                              alignItems: "center",
                              gap: 8,
                              paddingLeft: 8,
                              paddingRight: 8,
                            }}
                          >
                            <Text
                              style={{
                                fontSize: look().type.text_xsmall_medium.size,
                                fontWeight: look().type.text_xsmall_medium.weight,
                                color: p().text,
                              }}
                            >
                              {machine().name}
                            </Text>
                            {machine().unavailable != null ? (
                              <Text style={{ fontSize: look().type.text_xsmall.size, color: p().faint }}>
                                {machine().unavailable}
                              </Text>
                            ) : null}
                          </View>
                        ) : null}

                        <Each of={machine().projects} key={(proj: any) => proj.root}>
                          {(project: () => any) => (
                            <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 2 }}>
                              <View
                                style={{ paddingLeft: 8, paddingRight: 8 }}
                                tooltip={
                                  <SmeltTooltip>
                                    <Text style={{ fontSize: look().type.text_xsmall.size, color: p().text }}>
                                      {project().root}
                                    </Text>
                                  </SmeltTooltip>
                                }
                              >
                                <Text
                                  style={{
                                    fontSize: look().type.text_xsmall.size,
                                    color: p().muted,
                                    numberOfLines: 1,
                                  }}
                                >
                                  {project().label}
                                </Text>
                              </View>

                              <Each of={project().rows} key={(r: any) => r.id}>
                                {(row: () => any) => {
                                  const isBusy = () => busySet().has(busy_key(machine().machine, row().id));
                                  return (
                                    <View
                                      class="group hover:bg-accent"
                                      style={{
                                        width: "100%",
                                        paddingLeft: 8,
                                        paddingRight: 8,
                                        paddingTop: 4,
                                        paddingBottom: 4,
                                        borderRadius: look().radius.row,
                                        display: "flex",
                                        flexDirection: "row",
                                        alignItems: "center",
                                        gap: 8,
                                        opacity: isBusy() ? 0.5 : 1,
                                      }}
                                    >
                                      <SmeltIcon
                                        path={
                                          row().is_conversation
                                            ? "icons/message-square.svg"
                                            : "icons/terminal.svg"
                                        }
                                        size={15}
                                        color={p().muted}
                                      />
                                      <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
                                        <Text
                                          style={{
                                            fontSize: look().type.ui.size,
                                            lineHeight: look().type.ui.line_height,
                                            color: p().text,
                                            numberOfLines: 1,
                                          }}
                                        >
                                          {row().title}
                                        </Text>
                                        {row().when ? (
                                          <Text
                                            style={{
                                              fontSize: look().type.text_xsmall.size,
                                              color: p().muted,
                                              numberOfLines: 1,
                                            }}
                                          >
                                            {row().when}
                                          </Text>
                                        ) : null}
                                      </View>
                                      <View
                                        class="opacity-0 group-hover:opacity-100"
                                        style={{
                                          display: "flex",
                                          flexDirection: "row",
                                          alignItems: "center",
                                          flexShrink: 0,
                                          gap: 4,
                                        }}
                                      >
                                        <SmeltButton
                                          quiet={true}
                                          small={true}
                                          disabled={isBusy()}
                                          icon="icons/restore.svg"
                                          tooltip={t("action.restore")}
                                          accessibilityLabel={t("action.restore")}
                                          onPress={(e: any) => {
                                            e?.stopPropagation?.();
                                            if (!isBusy()) requestRestore(machine().machine, row());
                                          }}
                                        />
                                        <SmeltButton
                                          quiet={true}
                                          danger={true}
                                          small={true}
                                          disabled={isBusy()}
                                          icon="icons/trash.svg"
                                          tooltip={t("action.delete")}
                                          accessibilityLabel={t("action.delete")}
                                          onPress={(e: any) => {
                                            e?.stopPropagation?.();
                                            if (!isBusy()) requestDelete(machine().machine, row());
                                          }}
                                        />
                                      </View>
                                    </View>
                                  );
                                }}
                              </Each>
                            </View>
                          )}
                        </Each>
                      </View>
                    );
                  }}
                </Each>
              </View>
            </Scroll>
          </View>
        </SmeltPopover>
      </If>

      <SmeltDialog
        open={dialog() != null}
        role="alertdialog"
        onOpenChange={({ open }: any) => {
          if (!open) dialog.set(null);
        }}
        class="m-auto"
      >
        {dialog() ? (
          <View
            class={`${DIALOG_SURFACE_CLASS} p-5 flex-col gap-3`}
            style={{ width: CONFIRM_WIDTH }}
          >
            <Text
              style={{
                fontSize: look().type.ui_medium.size + 1,
                fontWeight: 600,
                color: look().palette.text,
              }}
            >
              {dialog()!.kind === "delete" ? t("delete.confirm") : t("restore.closed_title")}
            </Text>
            <Text
              style={{
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.muted,
              }}
            >
              {dialog()!.kind === "delete"
                ? t("delete.description", { title: dialog()!.row.title })
                : t("restore.closed_description", {
                    root: dialog()!.row.closed_workset.old_root,
                  })}
            </Text>

            {dialog()!.kind === "restore" ? (
              dialog()!.row.closed_workset.candidates.length === 0 ? (
                <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                  {t("restore.no_worksets")}
                </Text>
              ) : (
                <Scroll style={{ maxHeight: CHOICES_MAX_HEIGHT, width: "100%" }}>
                  <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                    <Each
                      of={dialog()!.row.closed_workset.candidates}
                      key={(c: any) => c.id}
                    >
                      {(candidate: () => any) => {
                        const checked = () => {
                          const cur = dialog();
                          return cur?.kind === "restore" && cur.picked === candidate().id;
                        };
                        return (
                          <Radio
                            checked={checked()}
                            accessibilityLabel={candidate().label}
                            onChange={() => {
                              const prev = dialog();
                              if (prev?.kind === "restore") {
                                dialog.set({ ...prev, picked: candidate().id });
                              }
                            }}
                            style={{
                              display: "flex",
                              flexDirection: "row",
                              alignItems: "center",
                              gap: 8,
                            }}
                          >
                            <View
                              style={{
                                width: 14,
                                height: 14,
                                borderRadius: 7,
                                borderWidth: 1.5,
                                borderColor: checked()
                                  ? look().palette.link
                                  : look().palette.hair_strong,
                                display: "flex",
                                alignItems: "center",
                                justifyContent: "center",
                              }}
                            >
                              {checked() ? (
                                <View
                                  style={{
                                    width: 6,
                                    height: 6,
                                    borderRadius: 3,
                                    backgroundColor: look().palette.link,
                                  }}
                                />
                              ) : null}
                            </View>
                            <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                              {candidate().label}
                            </Text>
                          </Radio>
                        );
                      }}
                    </Each>
                  </View>
                </Scroll>
              )
            ) : null}

            <View
              style={{
                display: "flex",
                flexDirection: "row",
                justifyContent: "flex-end",
                gap: 8,
                paddingTop: 4,
              }}
            >
              <SmeltButton
                quiet={true}
                label={t("action.cancel")}
                onPress={() => dialog.set(null)}
              />
              {dialog()!.kind === "delete" ? (
                <SmeltButton
                  danger={true}
                  label={t("action.delete")}
                  onPress={confirmDialog}
                />
              ) : (
                <SmeltButton
                  primary={true}
                  disabled={dialog()!.picked == null}
                  label={t("action.restore")}
                  onPress={confirmDialog}
                />
              )}
            </View>
          </View>
        ) : null}
      </SmeltDialog>
    </View>
  );
}

export default button;
