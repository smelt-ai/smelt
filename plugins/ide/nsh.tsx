// 用其他应用打开（来源 `smelt.ide`）：
// 无界面插件，只登记 `smelt.ide/open-with` 命令与会话列表项目行右键菜单中的「用其他应用打开」子菜单。
// 启动时立即登记文件管理器，后台探测本机已装编辑器并按需更新；主窗口回到前台时重新探测，零定时器/轮询。

import {
  bootTask,
  discoverApps,
  type DiscoveredApp,
  initHost,
  launchPath,
  localOs,
  onCommand,
  onEvent,
  registerCommands,
  registerContextMenuItems,
  run,
  t,
  text,
} from "smelt/sdk";

import {
  buildDiscoverQueries,
  buildMenuGroup,
  FILE_MANAGER_APP,
  findDiscoveredApp,
  OPEN_IN_FILE_MANAGER_COMMAND,
  OPEN_WITH_NAME,
  PROJECT_MENU,
  sameDiscoveredApps,
} from "./model.js";

const os = localOs();

let discovered: DiscoveredApp[] = [];
let scanning = false;
let booted = false;
let lastPublishedJson: string | null = null;

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

async function publish(): Promise<void> {
  const group = buildMenuGroup(discovered, os, {
    open_with: text("open_with"),
    file_manager: text("file_manager"),
  });
  const encoded = JSON.stringify(group);
  if (encoded === lastPublishedJson) return;
  lastPublishedJson = encoded;
  try {
    await registerContextMenuItems(PROJECT_MENU, [group]);
  } catch (error) {
    lastPublishedJson = null;
    console.error("[smelt.ide] registerContextMenuItems failed:", error);
  }
}

async function scan(): Promise<void> {
  if (scanning) return;
  scanning = true;
  try {
    const result = await discoverApps(buildDiscoverQueries(os));
    const next = Array.isArray(result) ? result : [];
    if (!sameDiscoveredApps(discovered, next)) {
      discovered = next;
      await publish();
    }
  } catch (error) {
    console.error("[smelt.ide] discoverApps failed:", error);
  } finally {
    scanning = false;
  }
}

onCommand(async ({ name, args }) => {
  if (name !== OPEN_WITH_NAME) {
    throw new Error(`Unknown command: ${name}`);
  }
  const app = typeof args?.app === "string" ? args.app : "";
  const path = typeof args?.path === "string" ? args.path : "";
  if (app === FILE_MANAGER_APP) {
    if (!path) {
      throw new Error(t("error.no_directory", { path }));
    }
    await run(OPEN_IN_FILE_MANAGER_COMMAND, { path });
    return null;
  }
  const found = findDiscoveredApp(discovered, app);
  if (!found) {
    throw new Error(t("error.app_missing", { app }));
  }
  if (!path) {
    throw new Error(t("error.no_directory", { path }));
  }
  try {
    await launchPath({
      kind: found.kind,
      target: found.target,
      path,
      require_dir: true,
    });
  } catch (error) {
    throw new Error(t("error.launch_failed", { app, error: errorMessage(error) }));
  }
  return null;
});

onEvent((event) => {
  if (event?.type === "window_activated" && booted) {
    void scan();
  }
}, ["window_activated"]);

bootTask(async () => {
  await initHost(["commands", "locale"]);
  await registerCommands(
    [
      {
        name: OPEN_WITH_NAME,
        title: text("open_with"),
        requires_args: true,
        args: ["app", "path"],
      },
    ],
    text("open_with"),
  );
  // 先把文件管理器登记上：它一定在，不用等后台探测完成。
  await publish();
  booted = true;
  void scan();
});
