// 智能体插件（来源 `smelt.agents`）NSH 入口：
// - `main`：智能体目录页与智能体详情编辑页（含配置、插件扩展插槽与记忆页签）
// - `settings`：设置里的「Agent」页（本机 CLI、Hooks、自定义 Agent、新建菜单启动项）

import { computed, View } from "nsh";
import { machineMeta, SmeltConfirmDialog, t } from "smelt/sdk";

import { AgentCatalogView } from "./catalog.tsx";
import { AgentEditorView } from "./editor.tsx";
import { AgentSettingsView } from "./settings.tsx";
import {
  agentHostsSignal,
  confirmDialog,
  editingAgent,
  getAgentDefinition,
  initPlugin,
} from "./state.ts";

initPlugin();

export function main() {
  const isEditing = computed(() => {
    machineMeta();
    agentHostsSignal();
    const ref = editingAgent();
    if (!ref) return false;
    return getAgentDefinition(ref.machine, ref.id) != null;
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
      <If when={isEditing()} else={<AgentCatalogView />}>
        <AgentEditorView />
      </If>
      <SmeltConfirmDialog
        open={confirmDialog() != null}
        title={confirmDialog()?.title ?? ""}
        description={confirmDialog()?.description ?? ""}
        confirmLabel={confirmDialog()?.action ?? ""}
        cancelLabel={t("common.cancel")}
        danger={Boolean(confirmDialog()?.danger)}
        onConfirm={() => {
          const fn = confirmDialog()?.onConfirm;
          confirmDialog.set(null);
          fn?.();
        }}
        onCancel={() => confirmDialog.set(null)}
      />
    </View>
  );
}

export function settings() {
  return <AgentSettingsView />;
}
