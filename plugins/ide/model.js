// 「用其他应用打开」纯逻辑：支持的编辑器清单、探测请求构造、右键子菜单构造与已发现应用查找。
// 零外部依赖，方便独立单测。

export const OWNER = "smelt.ide";

// 约定：smelt.session-list 的 PROJECT_MENU，上下文是 `{ machine, path }`；权威定义在
// plugins/session-list/contract.js。
export const PROJECT_MENU = "smelt.session-list/project";

export const OPEN_WITH_NAME = "open-with";
export const OPEN_WITH_COMMAND = `${OWNER}/${OPEN_WITH_NAME}`;
export const OPEN_IN_FILE_MANAGER_COMMAND = "smelt.app/open-in-file-manager";

/** 文件管理器那一项的 `args.app`。编辑器用自己的名称，不会与它冲突。 */
export const FILE_MANAGER_APP = "file-manager";

/** macOS 上顺带探测 Finder 图标使用的查询 id，不作为编辑器项列出。 */
export const FINDER_QUERY_ID = "__file_manager__";

export const EXTERNAL_LINK_ICON = "icons/external-link.svg";
export const TERMINAL_ICON = "icons/terminal.svg";
export const FOLDER_OPEN_ICON = "icons/folder-open.svg";

const defineIde = (label, mac_apps, commands) =>
  Object.freeze({
    id: label,
    label,
    mac_apps: Object.freeze([...mac_apps]),
    commands: Object.freeze([...commands]),
  });

// 按常见程度排序；新增编辑器只需要补一条定义，不必改菜单。
export const IDE_DEFINITIONS = Object.freeze([
  defineIde("Zed", ["Zed"], ["zed"]),
  defineIde("Antigravity 2.0", ["Antigravity"], ["antigravity"]),
  defineIde("Antigravity IDE", ["Antigravity IDE"], []),
  defineIde("Cursor", ["Cursor"], ["cursor"]),
  defineIde("VS Code", ["Visual Studio Code"], ["code"]),
  defineIde("VS Code Insiders", ["Visual Studio Code - Insiders"], ["code-insiders"]),
  defineIde("Windsurf", ["Windsurf"], ["windsurf"]),
  defineIde("IntelliJ IDEA", ["IntelliJ IDEA", "IntelliJ IDEA CE"], ["idea"]),
  defineIde("Android Studio", ["Android Studio"], ["studio"]),
  defineIde("Xcode", ["Xcode"], ["xed"]),
  defineIde("CLion", ["CLion"], ["clion"]),
  defineIde("GoLand", ["GoLand"], ["goland"]),
  defineIde("WebStorm", ["WebStorm"], ["webstorm"]),
  defineIde("PyCharm", ["PyCharm", "PyCharm CE"], ["pycharm"]),
  defineIde("RustRover", ["RustRover"], ["rustrover"]),
  defineIde("Rider", ["Rider"], ["rider"]),
  defineIde("Sublime Text", ["Sublime Text"], ["subl"]),
  defineIde("Nova", ["Nova"], ["nova"]),
  defineIde("Fleet", ["Fleet"], ["fleet"]),
  defineIde("Lapce", ["Lapce"], ["lapce"]),
]);

/** 构造发给宿主 `discoverApps` 的查询清单；macOS 额外请求图标并附带 Finder。 */
export function buildDiscoverQueries(os) {
  const isMac = os === "darwin";
  const queries = IDE_DEFINITIONS.map((def) => ({
    id: def.id,
    mac_apps: [...def.mac_apps],
    commands: [...def.commands],
    with_icon: isMac,
  }));
  if (isMac) {
    queries.push({
      id: FINDER_QUERY_ID,
      mac_apps: ["Finder"],
      commands: [],
      with_icon: true,
    });
  }
  return queries;
}

/** 当前平台文件管理器在菜单中使用的名称。 */
export function fileManagerLabel(os, texts) {
  return os === "darwin" ? "Finder" : texts.file_manager;
}

function normalizeIcon(icon) {
  return typeof icon === "string" && icon.length > 0 ? icon : null;
}

function appItem(label, image, fallbackIcon, app) {
  return {
    label,
    image,
    icon: fallbackIcon,
    command: OPEN_WITH_COMMAND,
    args: { app },
  };
}

/**
 * 构造登记进 `smelt.session-list/project` 的单个「用其他应用打开」子菜单项：
 * 按 `IDE_DEFINITIONS` 稳定顺序排已发现的编辑器，文件管理器放最后，并仅限本机项目显示。
 */
export function buildMenuGroup(discovered, os, texts) {
  const byId = new Map();
  if (Array.isArray(discovered)) {
    for (const app of discovered) {
      if (app && typeof app.id === "string" && !byId.has(app.id)) {
        byId.set(app.id, app);
      }
    }
  }

  const apps = [];
  for (const def of IDE_DEFINITIONS) {
    const found = byId.get(def.id);
    if (!found) continue;
    apps.push(appItem(def.label, normalizeIcon(found.icon), TERMINAL_ICON, def.id));
  }

  const finder = os === "darwin" ? byId.get(FINDER_QUERY_ID) : null;
  apps.push(
    appItem(
      fileManagerLabel(os, texts),
      normalizeIcon(finder?.icon),
      FOLDER_OPEN_ICON,
      FILE_MANAGER_APP,
    ),
  );

  return {
    label: texts.open_with,
    image: null,
    icon: EXTERNAL_LINK_ICON,
    // 应用都装在这台电脑上，远程项目的目录不在这里（machine: null 表示仅本机）。
    machine: null,
    submenu: apps,
  };
}

/** 按命令参数里的应用名在最近一次探测结果中查找对应条目（排除内部 Finder 图标查询）。 */
export function findDiscoveredApp(discovered, appName) {
  if (!Array.isArray(discovered) || typeof appName !== "string") return null;
  if (!appName || appName === FINDER_QUERY_ID || appName === FILE_MANAGER_APP) return null;
  return discovered.find((app) => app && app.id === appName) ?? null;
}

/** 比较两次探测结果是否完全一致，用于跳过无变化时的菜单重登记。 */
export function sameDiscoveredApps(prev, next) {
  if (!Array.isArray(prev) || !Array.isArray(next) || prev.length !== next.length) {
    return false;
  }
  for (let i = 0; i < prev.length; i += 1) {
    const a = prev[i];
    const b = next[i];
    if (
      !a ||
      !b ||
      a.id !== b.id ||
      a.kind !== b.kind ||
      a.target !== b.target ||
      normalizeIcon(a.icon) !== normalizeIcon(b.icon)
    ) {
      return false;
    }
  }
  return true;
}
