// 能力库插件（smelt.capabilities）入口：
// 登记 3 个视图（能力库 main、设置→模型 models、设置→记忆 memory）、机器菜单项与打开命令、
// `pi_config.changed` 运行时话题，并启动一次性事件驱动的自动模型目录刷新。

import { View } from "nsh";
import {
  bootTask,
  initHost,
  onCommand,
  onEvent,
  registerCommands,
  registerContextMenuItems,
  registerTopics,
  registerViews,
  run,
  t,
  text,
} from "smelt/sdk";
import {
  AGENT_SETTINGS_CHANGED,
  MACHINE_MENU,
  MEMORY_VIEW,
  MEMORY_VIEW_NAME,
  MODELS_VIEW,
  MODELS_VIEW_NAME,
  OPEN_ON_MACHINE,
  OPEN_ON_MACHINE_NAME,
  TOPIC_PI_CONFIG_CHANGED,
  VIEW,
  VIEW_NAME,
} from "../contract.js";
import { MEMORY_SETTINGS_KEYWORDS, MODELS_SETTINGS_KEYWORDS } from "../model.js";
import { McpTab } from "./mcp.tsx";
import { MemoryTab } from "./memory.tsx";
import { ModelsTab } from "./models.tsx";
import { PluginsTab } from "./plugins.tsx";
import {
  applyAgentSettingsEvent,
  createPageController,
  onAgentCommandsChanged,
  type PageController,
  startRefreshAutoProvidersOnceOnline,
} from "./state.ts";
import { ConfirmDialog, PageToolbar } from "./ui.tsx";

// 三个视图各持有一份独立状态（与 Rust CapabilitiesPage 同构）。
const mainPage = createPageController(VIEW, ["skills", "extensions", "mcp"]);
const modelsPage = createPageController(MODELS_VIEW, ["models"]);
const memoryPage = createPageController(MEMORY_VIEW, ["memory"]);
const allPages: PageController[] = [mainPage, modelsPage, memoryPage];

function pageByViewId(viewId: string): PageController | null {
  for (const page of allPages) {
    if (page.viewId === viewId) return page;
  }
  return null;
}

// ---- 命令与宿主事件 ----

onCommand(async ({ name, args }) => {
  if (name === OPEN_ON_MACHINE_NAME) {
    const machine = args?.machine ?? null;
    const opened = await run("smelt.app/open-view", {
      view: VIEW,
      args: { machine },
    });
    if (!opened) {
      throw new Error(t("error.no_place"));
    }
    return null;
  }
  throw new Error(`Unknown command: ${name}`);
});

onEvent(
  (event: any) => {
    if (!event || typeof event !== "object") return;
    switch (event.type) {
      case "view_opened": {
        const target = pageByViewId(String(event.view ?? ""));
        target?.onOpen(event.args);
        break;
      }
      case "shown_views_changed": {
        for (const page of allPages) page.onShownChanged();
        break;
      }
      case "machines_changed": {
        for (const page of allPages) page.onMachinesChanged();
        break;
      }
      case "runtime_topic": {
        if (event.topic === TOPIC_PI_CONFIG_CHANGED) {
          for (const page of allPages) {
            page.onPiConfigChanged(event.machine ?? null, event.message);
          }
        }
        break;
      }
      case "reconnected": {
        for (const page of allPages) {
          page.onReconnected(event.machine ?? null);
        }
        break;
      }
      case "commands_changed": {
        onAgentCommandsChanged();
        break;
      }
      case "plugin": {
        if (event.name === AGENT_SETTINGS_CHANGED) {
          applyAgentSettingsEvent(event.payload);
        }
        break;
      }
      default:
        break;
    }
  },
  [
    "view_opened",
    "shown_views_changed",
    "machines_changed",
    "runtime_topic",
    "reconnected",
    "commands_changed",
    `plugin:${AGENT_SETTINGS_CHANGED}`,
  ],
);

// ---- 启动注册 ----

bootTask(async () => {
  await initHost(["machines_meta", "views", "commands"]);
  await registerTopics([TOPIC_PI_CONFIG_CHANGED]);
  await registerViews([
    {
      name: VIEW_NAME,
      export: VIEW_NAME,
      title: text("title"),
      icon: "icons/layout-dashboard.svg",
      draws_top_bar: false,
      instances: false,
    },
    {
      name: MODELS_VIEW_NAME,
      export: MODELS_VIEW_NAME,
      title: text("tab.models"),
      icon: "icons/sparkles.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "agents",
        keywords: MODELS_SETTINGS_KEYWORDS,
      },
    },
    {
      name: MEMORY_VIEW_NAME,
      export: MEMORY_VIEW_NAME,
      title: text("tab.memory"),
      icon: "icons/brain.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "agents",
        keywords: MEMORY_SETTINGS_KEYWORDS,
      },
    },
  ]);
  await registerCommands(
    [
      {
        name: OPEN_ON_MACHINE_NAME,
        title: text("command.open_on_machine"),
        requires_args: true,
        args: ["machine"],
      },
    ],
    text("title"),
  );
  await registerContextMenuItems(MACHINE_MENU, [
    {
      label: text("title"),
      icon: "icons/layout-dashboard.svg",
      command: OPEN_ON_MACHINE,
      args: null,
    },
  ]);
  onAgentCommandsChanged();
  // 事件驱动等待本机在线后刷新一次自动目录；不阻塞插件 ready。
  startRefreshAutoProvidersOnceOnline();
});

// ---- 视图外壳与导出组件 ----

function CapabilitiesShell(props: { page: PageController; initialArgs?: any }) {
  props.page.ensureMounted(props.initialArgs);
  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        width: "100%",
        height: "100%",
        flex: 1,
        minWidth: 0,
        minHeight: 0,
      }}
    >
      <PageToolbar page={props.page} />
      <View
        style={{
          flex: 1,
          minWidth: 0,
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
        }}
      >
        <If when={props.page.tab() === "models"}>
          <ModelsTab page={props.page} />
        </If>
        <If when={props.page.tab() === "skills"}>
          <PluginsTab page={props.page} kind="skill" />
        </If>
        <If when={props.page.tab() === "extensions"}>
          <PluginsTab page={props.page} kind="extension" />
        </If>
        <If when={props.page.tab() === "mcp"}>
          <McpTab page={props.page} />
        </If>
        <If when={props.page.tab() === "memory"}>
          <MemoryTab page={props.page} />
        </If>
      </View>
      <ConfirmDialog
        request={props.page.confirmDialog}
        onDismiss={props.page.dismissConfirm}
      />
    </View>
  );
}

export function main(props: { args?: any }) {
  return <CapabilitiesShell page={mainPage} initialArgs={props?.args} />;
}

export function models(props: { args?: any }) {
  return <CapabilitiesShell page={modelsPage} initialArgs={props?.args} />;
}

export function memory(props: { args?: any }) {
  return <CapabilitiesShell page={memoryPage} initialArgs={props?.args} />;
}
