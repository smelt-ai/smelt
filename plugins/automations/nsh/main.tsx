// 自动化插件（`smelt.automations`）入口：导出主视图 `main`。
import { computed, Text, View } from "nsh";
import { SmeltConfirmDialog, t } from "smelt/sdk";
import { deleteConfirmDescription, runsForAutomation, sameAutomationRef } from "../model.js";
import { CatalogView } from "./catalog.tsx";
import { DrillHeader, EditorForm, RunHistoryColumn } from "./editor.tsx";
import { RunPane } from "./run_detail.tsx";
import {
  bootAutomations,
  confirmDeleteAutomation,
  currentTheme,
  deletePrompt,
  editor,
  machineAutomationsOn,
  route,
  selectedRunId,
  storeVersion,
} from "./state.ts";

bootAutomations();

function DrillView() {
  const look = currentTheme;
  const curRoute = computed(() => route());

  const activeEditor = computed(() => {
    const r = curRoute();
    const ed = editor();
    return r && ed && sameAutomationRef(ed, r) ? ed : null;
  });

  const runs = computed(() => {
    storeVersion();
    const r = curRoute();
    if (!r) return [];
    return runsForAutomation(machineAutomationsOn(r.machine), r.id);
  });

  const selectedRun = computed(() => {
    const id = selectedRunId();
    if (!id) return null;
    return runs().find((item: any) => item.id === id) ?? null;
  });

  const loadingPlaceholder = () => (
    <View style={{ flex: 1, padding: 20 }}>
      <Text
        style={{
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: look().palette.muted,
        }}
      >
        {t("catalog.loading")}
      </Text>
    </View>
  );

  return (
    <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}>
      <DrillHeader />
      <If
        when={runs().length === 0}
        else={
          <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "row" }}>
            <RunHistoryColumn
              machine={curRoute()?.machine ?? null}
              runs={runs()}
              showTask={activeEditor() != null}
            />
            <If
              when={selectedRun() != null}
              else={
                <If when={activeEditor() != null} else={loadingPlaceholder()}>
                  <EditorForm editor={activeEditor()!} />
                </If>
              }
            >
              <RunPane machine={curRoute()?.machine ?? null} run={selectedRun()!} />
            </If>
          </View>
        }
      >
        <If when={activeEditor() != null} else={loadingPlaceholder()}>
          <EditorForm editor={activeEditor()!} />
        </If>
      </If>
    </View>
  );
}

export function main() {
  const deleteDescription = computed(() => {
    const p = deletePrompt();
    return p ? deleteConfirmDescription(p.name, p.runCount, t) : "";
  });

  return (
    <View
      style={{
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        flexDirection: "column",
      }}
    >
      <If when={route() == null} else={<DrillView />}>
        <CatalogView />
      </If>
      <SmeltConfirmDialog
        open={deletePrompt() != null}
        title={t("delete.title")}
        description={deleteDescription()}
        confirmLabel={t("delete.confirm")}
        cancelLabel={t("delete.cancel")}
        danger={true}
        onConfirm={confirmDeleteAutomation}
        onCancel={() => deletePrompt.set(null)}
      />
    </View>
  );
}

export default main;
