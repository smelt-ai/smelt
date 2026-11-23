import { describe, expect, test } from "bun:test";

import {
  buildDiscoverQueries,
  buildMenuGroup,
  EXTERNAL_LINK_ICON,
  FILE_MANAGER_APP,
  FINDER_QUERY_ID,
  findDiscoveredApp,
  FOLDER_OPEN_ICON,
  IDE_DEFINITIONS,
  OPEN_WITH_COMMAND,
  PROJECT_MENU,
  sameDiscoveredApps,
  TERMINAL_ICON,
} from "../model.js";

const TEXTS = {
  open_with: { en: "Open With", "zh-CN": "用其他应用打开" },
  file_manager: { en: "File Manager", "zh-CN": "文件管理器" },
};

describe("smelt.ide model", () => {
  test("lists all 20 editors in exact order from detect.rs", () => {
    expect(IDE_DEFINITIONS.map((def) => def.label)).toEqual([
      "Zed",
      "Antigravity 2.0",
      "Antigravity IDE",
      "Cursor",
      "VS Code",
      "VS Code Insiders",
      "Windsurf",
      "IntelliJ IDEA",
      "Android Studio",
      "Xcode",
      "CLion",
      "GoLand",
      "WebStorm",
      "PyCharm",
      "RustRover",
      "Rider",
      "Sublime Text",
      "Nova",
      "Fleet",
      "Lapce",
    ]);
    expect(IDE_DEFINITIONS).toHaveLength(20);
    expect(PROJECT_MENU).toBe("smelt.session-list/project");
    expect(OPEN_WITH_COMMAND).toBe("smelt.ide/open-with");
  });

  test("Antigravity 2.0 and Antigravity IDE are separate definitions", () => {
    const two = IDE_DEFINITIONS.find((def) => def.label === "Antigravity 2.0");
    const ide = IDE_DEFINITIONS.find((def) => def.label === "Antigravity IDE");
    expect(two).toBeDefined();
    expect(ide).toBeDefined();
    expect(two.mac_apps).toContain("Antigravity");
    expect(two.mac_apps.some((name) => name.includes("IDE"))).toBe(false);
    expect(two.commands).toEqual(["antigravity"]);
    expect(ide.mac_apps).toContain("Antigravity IDE");
    expect(ide.commands).toEqual([]);
  });

  test("IDE definitions have stable unique labels and ids", () => {
    const seenIds = new Set();
    const seenLabels = new Set();
    for (const def of IDE_DEFINITIONS) {
      expect(def.id.trim().length).toBeGreaterThan(0);
      expect(def.label.trim().length).toBeGreaterThan(0);
      expect(def.mac_apps.length > 0 || def.commands.length > 0).toBe(true);
      expect(def.id).not.toBe(FILE_MANAGER_APP);
      expect(def.id).not.toBe(FINDER_QUERY_ID);
      expect(seenIds.has(def.id)).toBe(false);
      expect(seenLabels.has(def.label)).toBe(false);
      seenIds.add(def.id);
      seenLabels.add(def.label);
    }
  });

  test("buildDiscoverQueries enables icons and appends Finder only on darwin", () => {
    const mac = buildDiscoverQueries("darwin");
    expect(mac).toHaveLength(21);
    for (let i = 0; i < 20; i += 1) {
      expect(mac[i]).toEqual({
        id: IDE_DEFINITIONS[i].id,
        mac_apps: [...IDE_DEFINITIONS[i].mac_apps],
        commands: [...IDE_DEFINITIONS[i].commands],
        with_icon: true,
      });
    }
    expect(mac[20]).toEqual({
      id: FINDER_QUERY_ID,
      mac_apps: ["Finder"],
      commands: [],
      with_icon: true,
    });

    for (const otherOs of ["linux", "win32", "windows"]) {
      const queries = buildDiscoverQueries(otherOs);
      expect(queries).toHaveLength(20);
      expect(queries.every((q) => q.with_icon === false)).toBe(true);
      expect(queries.some((q) => q.id === FINDER_QUERY_ID)).toBe(false);
    }

    // Mutating returned arrays must not affect IDE_DEFINITIONS.
    mac[0].mac_apps.push("Mutated");
    expect(IDE_DEFINITIONS[0].mac_apps).toEqual(["Zed"]);
  });

  test("buildMenuGroup publishes File Manager immediately before scan completes", () => {
    const macGroup = buildMenuGroup([], "darwin", TEXTS);
    expect(macGroup).toEqual({
      label: TEXTS.open_with,
      image: null,
      icon: EXTERNAL_LINK_ICON,
      machine: null,
      submenu: [
        {
          label: "Finder",
          image: null,
          icon: FOLDER_OPEN_ICON,
          command: OPEN_WITH_COMMAND,
          args: { app: FILE_MANAGER_APP },
        },
      ],
    });

    const linuxGroup = buildMenuGroup([], "linux", TEXTS);
    expect(linuxGroup.submenu).toEqual([
      {
        label: TEXTS.file_manager,
        image: null,
        icon: FOLDER_OPEN_ICON,
        command: OPEN_WITH_COMMAND,
        args: { app: FILE_MANAGER_APP },
      },
    ]);
  });

  test("buildMenuGroup orders editors canonically, attaches icons with fallback, and puts file manager last", () => {
    const discovered = [
      {
        id: "VS Code",
        kind: "command",
        target: "code",
        icon: null,
      },
      {
        id: FINDER_QUERY_ID,
        kind: "mac_app",
        target: "/System/Library/CoreServices/Finder.app",
        icon: "data:image/png;base64,FINDER",
      },
      {
        id: "Zed",
        kind: "mac_app",
        target: "/Applications/Zed.app",
        icon: "data:image/png;base64,ZED",
      },
      // Duplicate and empty-string icon should be ignored / normalized to null.
      {
        id: "Zed",
        kind: "command",
        target: "zed",
        icon: null,
      },
      {
        id: "Cursor",
        kind: "mac_app",
        target: "/Applications/Cursor.app",
        icon: "",
      },
      {
        id: "Unknown Editor",
        kind: "command",
        target: "unknown",
        icon: "data:image/png;base64,NOPE",
      },
    ];

    const macGroup = buildMenuGroup(discovered, "darwin", TEXTS);
    expect(macGroup.machine).toBeNull();
    expect(macGroup.icon).toBe(EXTERNAL_LINK_ICON);
    expect(macGroup.submenu).toEqual([
      {
        label: "Zed",
        image: "data:image/png;base64,ZED",
        icon: TERMINAL_ICON,
        command: OPEN_WITH_COMMAND,
        args: { app: "Zed" },
      },
      {
        label: "Cursor",
        image: null,
        icon: TERMINAL_ICON,
        command: OPEN_WITH_COMMAND,
        args: { app: "Cursor" },
      },
      {
        label: "VS Code",
        image: null,
        icon: TERMINAL_ICON,
        command: OPEN_WITH_COMMAND,
        args: { app: "VS Code" },
      },
      {
        label: "Finder",
        image: "data:image/png;base64,FINDER",
        icon: FOLDER_OPEN_ICON,
        command: OPEN_WITH_COMMAND,
        args: { app: FILE_MANAGER_APP },
      },
    ]);

    // On non-darwin, Finder query icon is not used and localized file_manager label is emitted.
    const winGroup = buildMenuGroup(discovered, "win32", TEXTS);
    expect(winGroup.submenu[winGroup.submenu.length - 1]).toEqual({
      label: TEXTS.file_manager,
      image: null,
      icon: FOLDER_OPEN_ICON,
      command: OPEN_WITH_COMMAND,
      args: { app: FILE_MANAGER_APP },
    });
  });

  test("findDiscoveredApp locates editors and excludes file manager sentinels", () => {
    const discovered = [
      { id: "Zed", kind: "mac_app", target: "/Applications/Zed.app", icon: "data:image/png;base64,Z" },
      { id: FINDER_QUERY_ID, kind: "mac_app", target: "/System/Library/CoreServices/Finder.app", icon: "data:image/png;base64,F" },
    ];
    expect(findDiscoveredApp(discovered, "Zed")).toEqual(discovered[0]);
    expect(findDiscoveredApp(discovered, "Cursor")).toBeNull();
    expect(findDiscoveredApp(discovered, FINDER_QUERY_ID)).toBeNull();
    expect(findDiscoveredApp(discovered, FILE_MANAGER_APP)).toBeNull();
    expect(findDiscoveredApp(discovered, "")).toBeNull();
    expect(findDiscoveredApp(null, "Zed")).toBeNull();
  });

  test("sameDiscoveredApps detects equality and changes", () => {
    const a = [
      { id: "Zed", kind: "mac_app", target: "/Applications/Zed.app", icon: "data:image/png;base64,Z" },
      { id: "VS Code", kind: "command", target: "code", icon: null },
    ];
    const same = [
      { id: "Zed", kind: "mac_app", target: "/Applications/Zed.app", icon: "data:image/png;base64,Z" },
      { id: "VS Code", kind: "command", target: "code", icon: undefined },
    ];
    const diffTarget = [
      { id: "Zed", kind: "mac_app", target: "/Users/me/Applications/Zed.app", icon: "data:image/png;base64,Z" },
      { id: "VS Code", kind: "command", target: "code", icon: null },
    ];
    const diffIcon = [
      { id: "Zed", kind: "mac_app", target: "/Applications/Zed.app", icon: "data:image/png;base64,Z2" },
      { id: "VS Code", kind: "command", target: "code", icon: null },
    ];
    expect(sameDiscoveredApps(a, same)).toBe(true);
    expect(sameDiscoveredApps(a, diffTarget)).toBe(false);
    expect(sameDiscoveredApps(a, diffIcon)).toBe(false);
    expect(sameDiscoveredApps(a, a.slice(0, 1))).toBe(false);
  });
});
