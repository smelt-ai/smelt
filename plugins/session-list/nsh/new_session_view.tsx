// 新建会话菜单（NSH A2 版）：复用纯逻辑 `../new_session.js`，支持项目/工作树行「+」弹层与 ⌘N 命令面板对话框。

import { computed, Scroll, signal, Text, View } from "nsh";
import { DIALOG_SURFACE_CLASS, newSessionIcon, SmeltButton, SmeltDialog, SmeltIcon, SmeltMenu, SmeltPopover } from "smelt/sdk";

import { OPEN_PROJECT } from "../contract.js";
import { digit_key, initial_location, NewSession, pinned_by_digit } from "../new_session.js";
import { icon } from "../util.js";
import { list, t, theme, tick } from "./bridge.ts";
import { MenuDivider, MenuEntry, MenuSectionTitle, Tip } from "./menus.tsx";

const WIDTH = 280;
const PALETTE_WIDTH = 360;
const LIST_HEIGHT = 420;
const ROW_HEIGHT = 28;
const SEPARATOR_HEIGHT = 5;
const PADDING = 4;
// NSH 桌面覆盖滚动条宽 3pt + 0.75pt 描边、贴右缘 2pt；可滚时给列表右侧留出槽位，避免切过悬停高亮与星标。
const SCROLLBAR_GUTTER = 9;

const newSessionIconCache = new Map<string, string>();

function cachedNewSessionIcon(action: any): string {
  const key = `${action.kind ?? ""}\0${action.target ?? ""}\0${action.agentKind ?? ""}\0${action.provider ?? ""}\0${action.command ?? ""}`;
  const hit = newSessionIconCache.get(key);
  if (hit !== undefined) return hit;
  const resolved = newSessionIcon(action);
  newSessionIconCache.set(key, resolved);
  return resolved;
}

function slotsContentHeight(items: any[]): number {
  let total = PADDING * 2;
  for (const s of items) {
    total += s.kind === "separator" ? SEPARATOR_HEIGHT : ROW_HEIGHT;
  }
  return total;
}

function Separator() {
  return (
    <View
      style={{
        height: 1,
        marginTop: 2,
        marginBottom: 2,
        backgroundColor: theme().palette.hair,
        flexShrink: 0,
      }}
    />
  );
}

function LocationRow(props: { menu: () => any }) {
  const look = () => theme();
  const menu = props.menu;
  const locationText = () => menu().location ?? t("new.choose_location");

  const entries = () => {
    tick();
    const m = menu();
    const host = m.host;
    const allMachines = host.machines();
    const labelled = allMachines.length > 1;
    const items: any[] = [];
    for (const info of allMachines) {
      const machine = info.machine;
      if (labelled) items.push({ kind: "section", key: `m:${machine ?? "local"}`, label: info.name });
      const directory = host.workspace_directory(machine);
      if (!directory) {
        items.push({ kind: "disabled", key: `nc:${machine ?? "local"}`, label: t("new.not_connected") });
        continue;
      }
      if (directory.projects.length === 0) {
        items.push({ kind: "disabled", key: `np:${machine ?? "local"}`, label: t("list.no_projects") });
        continue;
      }
      const labels = host.project_labels(host.path_style(machine), directory.projects);
      const here = (machine ?? "") === (m.machine ?? "");
      directory.projects.forEach((root: string, ix: number) => {
        const label = labels[ix];
        items.push({
          kind: "project",
          key: `p:${machine ?? "local"}:${root}`,
          label,
          checked: here && m.root === root && m.workset == null,
          onSelect: () => m.set_location(machine, root, null),
        });
        for (const workset of directory.worksets.filter((w: any) => w.project_root === root)) {
          items.push({
            kind: "workset",
            key: `w:${machine ?? "local"}:${workset.id}`,
            label: `${label} / ${workset.task}`,
            checked: here && m.workset === workset.id,
            onSelect: () => m.set_location(machine, root, workset.id),
          });
        }
      });
    }
    return items;
  };

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        paddingHorizontal: 6 + PADDING,
        paddingTop: 4 + PADDING,
        paddingBottom: 4,
      }}
    >
      <SmeltMenu
        placement="bottom"
        align="start"
        popupStyle={{ minWidth: 220 }}
        trigger={
          <View
            role="button"
            class="flex-row items-center rounded-(--radius) hover:bg-accent"
            style={{ height: 24, paddingHorizontal: 8, gap: 6 }}
          >
            <SmeltIcon
              path={icon(menu().in_workset ? "git-branch" : "folder")}
              size={13}
              color={look().palette.muted}
            />
            <Text
              style={{
                fontSize: look().type.ui.size,
                color: look().palette.text,
                numberOfLines: 1,
              }}
            >
              {`${menu().machine_name} · ${locationText()}`}
            </Text>
            <SmeltIcon path={icon("chevron-down")} size={12} color={look().palette.muted} />
          </View>
        }
      >
        <Each of={entries()} key={(e: any) => e.key}>
          {(entry: () => any) =>
            entry().kind === "section" ? (
              <MenuSectionTitle text={entry().label} />
            ) : entry().kind === "disabled" ? (
              <MenuEntry label={entry().label} disabled={true} onSelect={() => {}} />
            ) : (
              <MenuEntry
                label={entry().label}
                iconPath={icon(entry().kind === "workset" ? "git-branch" : "folder")}
                checked={entry().checked}
                onSelect={entry().onSelect}
              />
            )
          }
        </Each>
        <MenuDivider />
        <MenuEntry
          label={t("menu.new_workset")}
          iconPath={icon("git-branch")}
          disabled={menu().root == null}
          onSelect={() => {
            const m = menu();
            if (m.root == null) return;
            m.list.open_repositories(m.machine, m.root, "create", (task: string) =>
              m.await_workset(m.root, task),
            );
          }}
        />
      </SmeltMenu>
    </View>
  );
}

function MenuBody(props: { menu: () => any; localTick?: () => number; maxHeight?: () => number }) {
  const look = () => theme();
  const menu = () => {
    tick();
    props.localTick?.();
    return props.menu();
  };
  const isPalette = computed(() => Boolean(menu()?.owner?.palette));
  const bodyMode = computed(() => {
    const m = menu();
    if (!m) return "none";
    if (m.root == null) return "no_projects";
    if (m.catalog.state === "loading") return "loading";
    if (m.catalog.state === "failed") return "failed";
    if (m.rows.length === 0) return "empty";
    return "ready";
  });
  const slots = computed(() => menu()?.slots ?? []);
  const listMaxHeight = computed(() => props.maxHeight?.() ?? LIST_HEIGHT);
  const scrollable = computed(() => slotsContentHeight(slots()) > listMaxHeight());
  const errorText = computed(() => menu()?.error ?? null);

  const handleKey = (e: any) => {
    const m = props.menu();
    if (!m) return;
    const mod = e?.modifiers;
    if (mod?.control || mod?.alt || mod?.shift || mod?.platform) return;
    const key = String(e?.key ?? "");
    if (key === "ArrowUp" || key === "up") {
      m.step(-1);
    } else if (key === "ArrowDown" || key === "down") {
      m.step(1);
    } else if (key === "Enter" || key === "enter") {
      if (m.selected != null) m.launch(m.selected);
    } else {
      const idx = pinned_by_digit(m.rows, key);
      if (idx != null) m.launch(idx);
    }
  };

  return (
    <View
      ref={(el: any) => {
        // 打开弹层 / ⌘N 面板时同步拿取键盘焦点，保证 ↑/↓/Enter/1-9 立即生效。
        el?.focus?.();
      }}
      focusable={true}
      style={{ display: "flex", flexDirection: "column", width: "100%", flexShrink: 1, minHeight: 0 }}
      onKeyDown={handleKey}
    >
      {isPalette() ? (
        <>
          <LocationRow menu={menu} />
          <Separator />
        </>
      ) : null}

      {(() => {
        const mode = bodyMode();
        if (mode === "none") return null;
        if (mode === "no_projects") {
          return (
            <View style={{ display: "flex", flexDirection: "column", alignItems: "flex-start", padding: PADDING }}>
              <View style={{ paddingHorizontal: 12, paddingVertical: 8 }}>
                <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                  {t("new.no_projects", { machine: menu()?.machine_name ?? "" })}
                </Text>
              </View>
              <View style={{ paddingHorizontal: 6, paddingBottom: 4 }}>
                <SmeltButton
                  small={true}
                  label={t("command.open_project")}
                  onPress={() => {
                    const m = props.menu();
                    if (!m) return;
                    m.dispose();
                    m.owner.close();
                    m.list.report_failure(t("action.open_project"), m.host.run(OPEN_PROJECT));
                  }}
                />
              </View>
            </View>
          );
        }
        if (mode === "loading") {
          return (
            <View style={{ paddingHorizontal: 12 + PADDING, paddingVertical: 8 + PADDING }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                {t("new.loading_tools", { machine: menu()?.machine_name ?? "" })}
              </Text>
            </View>
          );
        }
        if (mode === "failed") {
          return (
            <View style={{ paddingHorizontal: 12 + PADDING, paddingVertical: 8 + PADDING }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                {t("new.unreachable", {
                  machine: menu()?.machine_name ?? "",
                  error: menu()?.catalog?.error ?? "",
                })}
              </Text>
            </View>
          );
        }
        if (mode === "empty") {
          return (
            <View style={{ paddingHorizontal: 12 + PADDING, paddingVertical: 8 + PADDING }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                {t("new.no_options")}
              </Text>
            </View>
          );
        }
        return (
          <Scroll style={{ maxHeight: listMaxHeight(), width: "100%", flexShrink: 1, minHeight: 0 }}>
            <View
              style={{
                display: "flex",
                flexDirection: "column",
                width: "100%",
                paddingTop: PADDING,
                paddingBottom: PADDING,
                paddingLeft: PADDING,
                paddingRight: scrollable() ? SCROLLBAR_GUTTER : PADDING,
              }}
            >
              <Each
                of={slots()}
                key={(slot: any) =>
                  slot.kind === "row"
                    ? `r:${slot.index}`
                    : slot.kind === "header"
                      ? `h:${slot.section}`
                      : "sep"
                }
              >
                {(slot: () => any) => {
                  const s = slot();
                  if (s.kind === "separator") return <Separator />;
                  if (s.kind === "header") {
                    return (
                      <View
                        style={{
                          height: ROW_HEIGHT,
                          paddingHorizontal: 12,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          flexShrink: 0,
                        }}
                      >
                        <Text
                          style={{
                            fontSize: look().type.small_medium.size,
                            fontWeight: look().type.small_medium.weight,
                            color: look().palette.faint,
                          }}
                        >
                          {t(slot().section === 1 ? "kind.terminal" : "kind.conversation")}
                        </Text>
                      </View>
                    );
                  }
                  const index = s.index;
                  const entry = computed(() => menu()?.rows?.[index] ?? null);
                  const action = computed(() => entry()?.action ?? null);
                  const iconPath = computed(() => (action() ? cachedNewSessionIcon(action()) : ""));
                  const selected = computed(() => menu()?.selected === index);
                  const isCommon = computed(() => entry()?.section === 0);
                  const digit = computed(() => {
                    const row = entry();
                    return row ? digit_key(row, index) : null;
                  });
                  return (
                    <View
                      role="button"
                      onMouseEnter={() => props.menu()?.hover(index)}
                      onPress={() => props.menu()?.launch(index)}
                      style={{
                        height: ROW_HEIGHT,
                        paddingLeft: 12,
                        paddingRight: 4,
                        borderRadius: look().radius.sm,
                        backgroundColor: selected() ? look().palette.row_hover : "transparent",
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        gap: 8,
                        flexShrink: 0,
                      }}
                    >
                      <SmeltIcon
                        path={iconPath()}
                        size={16}
                        color={look().palette.muted}
                      />
                      <Text
                        class="flex-1 min-w-0"
                        style={{
                          fontSize: look().type.ui.size,
                          lineHeight: look().type.ui.line_height,
                          color: look().palette.text,
                          numberOfLines: 1,
                        }}
                      >
                        {action()?.label ?? ""}
                      </Text>
                      {isCommon() ? (
                        <Text
                          style={{
                            fontSize: look().type.text_xsmall.size,
                            color: look().palette.muted,
                            flexShrink: 0,
                          }}
                        >
                          {t(action()?.kind === "terminal" ? "kind.terminal" : "kind.conversation")}
                        </Text>
                      ) : null}
                      {digit() != null ? (
                        <View
                          style={{
                            paddingHorizontal: 5,
                            height: 18,
                            borderRadius: 4,
                            borderWidth: 1,
                            borderColor: look().palette.hair_strong,
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "center",
                            flexShrink: 0,
                          }}
                        >
                          <Text
                            style={{
                              fontSize: look().type.small.size,
                              color: look().palette.muted,
                            }}
                          >
                            {digit()}
                          </Text>
                        </View>
                      ) : null}
                      <View style={{ width: 22, height: 22, flexShrink: 0, alignItems: "center", justifyContent: "center" }}>
                        {selected() && action() ? (
                          <View
                            role="button"
                            accessibilityLabel={action()?.pinned ? t("new.unfavorite") : t("new.favorite")}
                            tooltip={
                              <Tip text={action()?.pinned ? t("new.unfavorite") : t("new.favorite")} />
                            }
                            class="items-center justify-center rounded-(--radius) hover:bg-accent"
                            style={{ width: 22, height: 22, cursor: "default" }}
                            onPress={(e: any) => {
                              e?.stopPropagation?.();
                              const key = action()?.key;
                              if (key != null) props.menu()?.toggle_pin(key);
                            }}
                          >
                            <SmeltIcon
                              path={icon(action()?.pinned ? "star-fill" : "star")}
                              size={13}
                              color={action()?.pinned ? look().palette.text : look().palette.muted}
                            />
                          </View>
                        ) : null}
                      </View>
                    </View>
                  );
                }}
              </Each>
            </View>
          </Scroll>
        );
      })()}

      {errorText() != null ? (
        <View style={{ paddingHorizontal: 12 + PADDING, paddingTop: 4, paddingBottom: 4 + PADDING }}>
          <Text style={{ fontSize: look().type.ui.size, color: look().palette.bad }}>
            {errorText()}
          </Text>
        </View>
      ) : null}
    </View>
  );
}

/** 项目行、工作树行上的「+」按钮：纯 NSH 按钮避免原生组件 hitbox 抢走父行 hover；点开弹出新建会话菜单。 */
export function NewSessionPopover(props: {
  machine: string | null;
  path: string;
  tooltip: string;
  hoverClass?: string;
  onOpenChange?: (open: boolean) => void;
}) {
  const open = signal(false);
  const localTick = signal(0);
  const availableHeight = signal<number | null>(null);
  let activeMenu: any = null;

  const listMaxHeight = computed(() => {
    const avail = availableHeight();
    if (avail == null) return LIST_HEIGHT;
    return Math.min(LIST_HEIGHT, Math.max(ROW_HEIGHT * 3, Math.floor(avail - 2)));
  });

  const closeMenu = () => {
    if (activeMenu) {
      activeMenu.dispose();
      activeMenu = null;
    }
    availableHeight.set(null);
    open.set(false);
    props.onOpenChange?.(false);
  };

  const openMenu = () => {
    if (activeMenu) activeMenu.dispose();
    availableHeight.set(null);
    const owner = {
      redraw: () => localTick.set(localTick() + 1),
      close: () => closeMenu(),
    };
    activeMenu = new NewSession(list, initial_location(list.host, props.machine, props.path), owner);
    open.set(true);
    props.onOpenChange?.(true);
    localTick.set(localTick() + 1);
  };

  return (
    <View
      class={open() ? "opacity-100" : (props.hoverClass ?? "opacity-0 group-hover/row:opacity-100")}
      style={{ flexShrink: 0 }}
    >
      <SmeltPopover
        open={open()}
        onOpenChange={({ open: next }: any) => {
          if (next) openMenu();
          else closeMenu();
        }}
        onPosition={({ available }: any) => {
          const h = available?.height;
          if (typeof h === "number" && Number.isFinite(h) && h > 0) {
            availableHeight.set(h);
          }
        }}
        placement="right"
        align="start"
        offset={8}
        margin={8}
        popupClass="rounded-xl border border-border bg-popover p-0 shadow-md overflow-hidden"
        popupStyle={{ width: WIDTH }}
        trigger={
          <SmeltButton
            quiet={true}
            small={true}
            selected={open()}
            icon={icon("plus")}
            tooltip={props.tooltip}
            accessibilityLabel={props.tooltip}
          />
        }
      >
        {open() && activeMenu ? (
          <MenuBody menu={() => activeMenu} localTick={localTick} maxHeight={listMaxHeight} />
        ) : null}
      </SmeltPopover>
    </View>
  );
}

/** ⌘N 打开的新建会话面板（挂在插件无头根上）。 */
export function PaletteDialog() {
  const paletteOpen = computed(() => {
    tick();
    return list.palette != null;
  });
  const menu = () => {
    tick();
    return list.palette;
  };
  return (
    <SmeltDialog
      open={paletteOpen()}
      accessibilityLabel={t("menu.new_session")}
      onOpenChange={({ open }) => {
        if (!open) list.palette?.owner?.close?.();
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS}`}
      style={{ width: PALETTE_WIDTH, padding: 0, overflow: "hidden" }}
    >
      {paletteOpen() ? <MenuBody menu={menu} /> : null}
    </SmeltDialog>
  );
}
